//! Shared, cancellable background loads for soundboard playback and trimming.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};

use anyhow::Context;
use crossbeam_channel::{Receiver, TryRecvError};
use eframe::egui::{self, Color32};

use super::{App, CachedClip, AMBER, CYAN, GREEN, MUTED, RED, clip_key, short};
use crate::config::Pad;
use crate::dsp::{self, Clip};
use crate::engine::Engine;

struct ClipRequest {
    path: PathBuf,
    play: bool,
}

pub(super) struct TrimClip {
    path: PathBuf,
    pub(super) error: Option<String>,
}

#[derive(Default)]
pub(super) struct ClipLoads {
    /// The front request belongs to the running decoder, if any.
    requests: VecDeque<ClipRequest>,
    job: Option<Receiver<anyhow::Result<CachedClip>>>,
    cancel: Arc<AtomicBool>,
}

impl ClipLoads {
    fn retain(&mut self, keep: impl Fn(&ClipRequest) -> bool) {
        if self.requests.front().is_some_and(|request| !keep(request)) {
            self.cancel.store(true, Ordering::Relaxed);
            self.job = None;
            self.cancel = Arc::default();
        }
        self.requests.retain(keep);
    }
}

impl Drop for ClipLoads {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl CachedClip {
    pub(super) fn read(path: &Path, cancel: &AtomicBool) -> anyhow::Result<Self> {
        let metadata = std::fs::metadata(path).with_context(|| format!("read {}", path.display()))?;
        let samples = crate::decode::load_clip_cancellable(path, cancel)?;
        // Scanning long recordings for a waveform must also stay off the UI thread.
        let mut peaks = Vec::with_capacity(240);
        for chunk in samples.chunks(samples.len().div_ceil(240).max(1)) {
            anyhow::ensure!(!cancel.load(Ordering::Relaxed), "Clip load cancelled");
            peaks.push(chunk.iter().fold(0.0f32, |peak, sample| peak.max(sample.abs())));
        }
        Ok(Self { samples: Arc::new(samples), peaks: Arc::new(peaks), bytes: metadata.len(), modified: metadata.modified().ok() })
    }

    /// A queued decode must still describe the file when the UI accepts it.
    pub(super) fn validate_source(&self, path: &Path) -> anyhow::Result<()> {
        let metadata = std::fs::metadata(path).with_context(|| format!("read {}", path.display()))?;
        anyhow::ensure!(
            self.bytes == metadata.len() && self.modified == metadata.modified().ok(),
            "Clip changed while loading; try again",
        );
        Ok(())
    }
}

impl App {
    pub(super) fn sync_clip_progress(&mut self) {
        let progress = self.engine.as_ref().map(Engine::clip_progress).unwrap_or_default();
        self.playing = progress.iter().filter(|(_, _, paused)| !paused).map(|(key, _, _)| *key).collect();
        self.pad_progress = progress.into_iter().map(|(key, at, _)| (key, at)).collect();
        if self.pad_progress.is_empty()
            && (self.sound_status.0.starts_with("Playing") || self.sound_status.0.starts_with("Paused"))
        {
            self.sound_status = ("Ready".into(), Color32::WHITE);
        }
    }

    pub(super) fn cached_clip(&mut self, path: &Path) -> Option<&CachedClip> {
        let cached = self.clip_cache.get(path)?;
        if cached.validate_source(path).is_err() { self.forget_clip(path); }
        self.clip_cache.get(path)
    }

    pub(super) fn trim_waveform(&mut self, path: &Path, ctx: &egui::Context) -> Option<(f32, Arc<Vec<f32>>)> {
        if self.trim_clip.as_ref().is_none_or(|clip| clip.path != path) {
            self.close_trim_editor();
            self.trim_clip = Some(TrimClip { path: path.to_owned(), error: None });
        }
        if let Some(clip) = self.cached_clip(path) {
            return Some((clip.samples.len() as f32 / dsp::SR as f32, Arc::clone(&clip.peaks)));
        }
        if self.trim_clip.as_ref().is_some_and(|clip| clip.error.is_none()) {
            self.queue_clip(path, false);
            self.poll_clip_loads(ctx);
        }
        None
    }

    pub(super) fn close_trim_editor(&mut self) {
        self.trim_clip = None;
        self.prune_clip_loads();
    }

    pub(super) fn cache_clip(&mut self, path: PathBuf, clip: CachedClip) {
        self.forget_clip(&path);
        if let Some(trim) = &mut self.trim_clip && trim.path == path {
            trim.error = None;
        }
        // Keep the cache to about ten minutes of audio (~110 MB).
        let cached: usize = self.clip_cache.values().map(|clip| clip.samples.len()).sum();
        if cached + clip.samples.len() > dsp::SR as usize * 600 {
            self.clip_cache.clear();
        }
        self.clip_cache.insert(path, clip);
    }

    pub(super) fn forget_clip(&mut self, path: &Path) {
        self.clip_cache.remove(path);
        self.pad_durations.remove(path);
    }

    pub(super) fn clip_loading(&self, path: &Path) -> bool {
        self.clip_loads.as_ref().is_some_and(|pending| pending.requests.iter().any(|r| r.play && r.path == path))
    }

    pub(super) fn has_pending_playback(&self) -> bool {
        self.clip_loads.as_ref().is_some_and(|pending| pending.requests.iter().any(|r| r.play))
    }

    fn queue_clip(&mut self, path: &Path, play: bool) {
        let pending = self.clip_loads.get_or_insert_with(ClipLoads::default);
        if let Some(request) = pending.requests.iter_mut().find(|r| r.path == path) {
            request.play |= play;
        } else {
            pending.requests.push_back(ClipRequest { path: path.to_owned(), play });
        }
    }

    /// Keep a decoder only while playback or the open trim menu needs it.
    fn prune_clip_loads(&mut self) {
        let Some(pending) = &mut self.clip_loads else { return };
        pending.retain(|request| request.play || self.trim_clip.as_ref().is_some_and(|trim| trim.path == request.path));
        if pending.requests.is_empty() { self.clip_loads = None; }
    }

    pub(super) fn cancel_pending_playback(&mut self, except: Option<&Path>) {
        if let Some(pending) = &mut self.clip_loads {
            for request in &mut pending.requests {
                if Some(request.path.as_path()) != except { request.play = false; }
            }
        }
        self.prune_clip_loads();
    }

    pub(super) fn cancel_clip_loading(&mut self, path: &Path) {
        if self.trim_clip.as_ref().is_some_and(|trim| trim.path == path) { self.trim_clip = None; }
        if let Some(pending) = &mut self.clip_loads { pending.retain(|r| r.path != path); }
        self.prune_clip_loads();
    }

    /// Card and strip clicks cancel a pending load, or pause/resume playback.
    pub(super) fn toggle_clip(&mut self, index: usize, ctx: &egui::Context) {
        let Some(pad) = self.settings.sounds.get(index).cloned() else { return };
        if self.clip_loading(&pad.path) {
            if let Some(pending) = &mut self.clip_loads {
                for request in &mut pending.requests {
                    if request.path == pad.path { request.play = false; }
                }
            }
            self.prune_clip_loads();
            self.sound_status = (format!("Cancelled · {}", pad.display_name()), MUTED);
            return;
        }
        let paused = self.engine.as_ref().and_then(|engine| {
            engine.toggle_sound_pause(clip_key(&pad.path), self.settings.overlap_clips)
        });
        if let Some(paused) = paused {
            if !paused && !self.settings.overlap_clips {
                self.cancel_pending_playback(None);
            }
            let (label, color) = if paused { ("Paused", AMBER) } else { ("Playing", GREEN) };
            self.sound_status = (format!("{label} · {}", pad.display_name()), color);
            self.sync_clip_progress();
        } else {
            self.play_clip(index, ctx);
        }
    }

    pub(super) fn play_clip(&mut self, index: usize, ctx: &egui::Context) {
        let Some(pad) = self.settings.sounds.get(index).cloned() else { return };
        if self.engine.is_none() {
            self.sound_status = ("Start OpenMic before playing a clip".into(), AMBER);
            return;
        }
        if !self.settings.overlap_clips {
            self.cancel_pending_playback(Some(&pad.path));
        }
        if self.clip_loading(&pad.path) {
            return; // repeated hotkeys reuse the same pending load
        }
        if let Some(samples) = self.cached_clip(&pad.path).map(|clip| Arc::clone(&clip.samples)) {
            self.play_decoded_clip(&pad, &samples);
            return;
        }
        if let Some(trim) = &mut self.trim_clip && trim.path == pad.path { trim.error = None; }
        self.queue_clip(&pad.path, true);
        self.poll_clip_loads(ctx);
    }

    fn play_decoded_clip(&mut self, pad: &Pad, samples: &Arc<Vec<f32>>) {
        let Some(engine) = &self.engine else { return };
        let clip = Clip::new(clip_key(&pad.path), Arc::clone(samples), pad.volume).with_trim(pad.start, pad.end);
        engine.play_sound(clip, self.settings.overlap_clips);
        self.sound_status = (format!("Playing · {}", pad.display_name()), GREEN);
        self.sync_clip_progress();
    }

    fn clip_load_failed(&mut self, request: &ClipRequest, error: &anyhow::Error) {
        crate::logfile::error(format_args!("could not load {}: {error:#}", request.path.display()));
        let message = format!("{error:#}");
        if let Some(trim) = &mut self.trim_clip && trim.path == request.path {
            trim.error = Some(message.clone());
        }
        if request.play { self.sound_status = (short(&message), RED); }
    }

    pub(super) fn poll_clip_loads(&mut self, ctx: &egui::Context) {
        if self.engine.is_none() {
            self.cancel_pending_playback(None);
        }
        let Some(mut pending) = self.clip_loads.take() else { return };
        let result = pending.job.as_ref().and_then(|job| match job.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err(anyhow::anyhow!("Clip decoder stopped unexpectedly; try again"))),
        });
        if let Some(result) = result {
            pending.job = None;
            let request = pending.requests.pop_front().expect("a decoder has a clip request");
            let result = result.and_then(|clip| {
                clip.validate_source(&request.path)?;
                Ok(clip)
            });
            match result {
                Ok(clip) => {
                    let samples = Arc::clone(&clip.samples);
                    self.cache_clip(request.path.clone(), clip);
                    if request.play && let Some(pad) = self.settings.sounds.iter().find(|pad| pad.path == request.path).cloned() {
                        self.play_decoded_clip(&pad, &samples);
                    }
                }
                Err(error) => self.clip_load_failed(&request, &error),
            }
        }
        while pending.job.is_none() && let Some(request) = pending.requests.front() {
            let Some(pad) = self.settings.sounds.iter().find(|pad| pad.path == request.path) else {
                pending.requests.pop_front();
                continue;
            };
            if request.play { self.sound_status = (format!("Loading · {}", pad.display_name()), CYAN); }
            let (done, job) = crossbeam_channel::bounded(1);
            let (cancel, wake) = (Arc::clone(&pending.cancel), ctx.clone());
            let path = request.path.clone();
            let spawned = std::thread::Builder::new().name("openmic-clip-loader".into()).spawn(move || {
                let _ = done.send(CachedClip::read(&path, &cancel));
                wake.request_repaint();
            });
            match spawned {
                Ok(_) => pending.job = Some(job),
                Err(error) => {
                    let request = pending.requests.pop_front().unwrap();
                    self.clip_load_failed(&request, &anyhow::Error::new(error).context("Could not load clip"));
                }
            }
        }
        if pending.job.is_some() {
            self.clip_loads = Some(pending);
        }
    }

    pub(super) fn stop_clips(&mut self) {
        self.cancel_pending_playback(None);
        if let Some(engine) = &self.engine {
            engine.stop_sounds();
        }
        self.playing.clear();
        self.pad_progress.clear();
        self.sound_status = ("Ready".into(), Color32::WHITE);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Settings;

    fn wav(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        let take = crate::record::Recorder::from_samples_for_test(&vec![0.25; 96_000]).finish().unwrap().unwrap();
        take.save_wav(&path).unwrap();
        path
    }

    fn app_with(paths: &[PathBuf]) -> App {
        let mut app = App::stopped(Settings { sounds: paths.iter().cloned().map(Pad::new).collect(), ..Settings::default() });
        app.engine = Some(Engine::for_test());
        app.page = super::super::Page::Soundboard;
        app
    }

    fn delayed(app: &mut App, paths: &[PathBuf]) -> (crossbeam_channel::Sender<anyhow::Result<CachedClip>>, Arc<AtomicBool>) {
        let (done, job) = crossbeam_channel::bounded(1);
        let cancel = Arc::default();
        app.clip_loads = Some(ClipLoads {
            requests: paths.iter().cloned().map(|path| ClipRequest { path, play: true }).collect(),
            job: Some(job), cancel: Arc::clone(&cancel),
        });
        (done, cancel)
    }

    fn finish(app: &mut App, ctx: &egui::Context) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while app.clip_loads.is_some() {
            app.poll_clip_loads(ctx);
            assert!(std::time::Instant::now() < deadline, "clip playback load did not finish");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    fn draw_board(app: &mut App, ctx: &egui::Context, events: Vec<egui::Event>) -> egui::FullOutput {
        use eframe::App as _;
        let mut frame = eframe::Frame::_new_kittest();
        ctx.run_ui(egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(700.0, 720.0))),
            time: Some(ctx.cumulative_frame_nr() as f64 * 0.2), events, ..Default::default()
        }, |ui| app.ui(ui, &mut frame))
    }

    fn text_rect(output: &egui::FullOutput, name: &str) -> egui::Rect {
        output.shapes.iter().find_map(|shape| match &shape.shape {
            egui::Shape::Text(text) if text.galley.text() == name => Some(text.visual_bounding_rect()),
            _ => None,
        }).unwrap_or_else(|| panic!("{name} not drawn"))
    }

    fn click(pos: egui::Pos2, button: egui::PointerButton, pressed: bool) -> Vec<egui::Event> {
        vec![egui::Event::PointerMoved(pos), egui::Event::PointerButton { pos, button, pressed, modifiers: egui::Modifiers::NONE }]
    }

    fn pad_icon_position(app: &mut App, ctx: &egui::Context, name: &str) -> egui::Pos2 {
        draw_board(app, ctx, Vec::new());
        let title = text_rect(&draw_board(app, ctx, Vec::new()), name);
        egui::pos2(title.left() - 26.0, title.center().y)
    }

    fn open_trim_menu(app: &mut App, ctx: &egui::Context, name: &str) {
        let pos = pad_icon_position(app, ctx, name);
        for pressed in [true, false] { draw_board(app, ctx, click(pos, egui::PointerButton::Secondary, pressed)); }
    }

    #[test]
    fn waveform_loads_while_stopped_without_changing_playback_or_trim_points() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("waveform.wav");
        let samples: Vec<f32> = [-0.5, 0.0, 0.25].into_iter().flat_map(|level| vec![level; 32_000]).collect();
        crate::record::Recorder::from_samples_for_test(&samples).finish().unwrap().unwrap().save_wav(&path).unwrap();
        let mut app = app_with(std::slice::from_ref(&path));
        app.stop_engine(true);
        app.settings.sounds[0].start = 0.1;
        app.settings.sounds[0].end = Some(0.8);
        let pad = app.settings.sounds[0].clone();
        let status = app.sound_status.clone();
        let ctx = egui::Context::default();
        assert!(app.trim_waveform(&path, &ctx).is_none());
        assert!(!app.has_pending_playback() && !app.clip_loading(&path));
        finish(&mut app, &ctx);
        let (length, peaks) = app.trim_waveform(&path, &ctx).unwrap();
        assert_eq!(length, 1.0);
        assert_eq!(peaks.len(), 240);
        for (&peak, expected) in peaks.iter().zip([0.5, 0.0, 0.25].into_iter().flat_map(|level| vec![level; 80])) {
            assert!((peak - expected).abs() < 0.001, "waveform preserves negative peaks, silence and quieter audio");
        }
        assert!(Arc::ptr_eq(&peaks, &app.trim_waveform(&path, &ctx).unwrap().1));
        assert!(app.engine.is_none() && app.playing.is_empty() && app.clip_loads.is_none());
        assert_eq!(app.settings.sounds[0], pad);
        assert_eq!(app.sound_status, status);
    }

    #[test]
    fn trimming_and_playback_share_a_decoder_in_either_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = wav(dir.path(), "shared.wav");
        let ctx = egui::Context::default();
        for play_first in [true, false] {
            let mut app = app_with(std::slice::from_ref(&path));
            let (done, cancel) = delayed(&mut app, std::slice::from_ref(&path));
            app.clip_loads.as_mut().unwrap().requests[0].play = play_first;
            if !play_first { app.trim_clip = Some(TrimClip { path: path.clone(), error: None }); }
            app.trim_waveform(&path, &ctx);
            if !play_first { app.play_clip(0, &ctx); }
            assert_eq!(app.clip_loads.as_ref().unwrap().requests.len(), 1);
            assert!(Arc::ptr_eq(&cancel, &app.clip_loads.as_ref().unwrap().cancel));
            app.close_trim_editor();
            assert!(!cancel.load(Ordering::Relaxed), "closing the menu preserves requested playback");
            app.settings.sounds[0].start = 0.25;
            done.send(CachedClip::read(&path, &cancel)).unwrap();
            finish(&mut app, &ctx);
            let playing = app.engine.as_ref().unwrap().take_clips();
            assert_eq!(playing.len(), 1);
            assert_eq!(playing[0].samples().len(), 36_000);
            assert!(app.trim_waveform(&path, &ctx).is_some(), "the same decode also cached the waveform");
        }
    }

    #[test]
    fn cancelling_playback_keeps_an_open_waveform_but_never_plays_its_late_result() {
        let dir = tempfile::tempdir().unwrap();
        let path = wav(dir.path(), "shared.wav");
        let ctx = egui::Context::default();
        for action in ["toggle", "stop clips", "stop engine"] {
            let mut app = app_with(std::slice::from_ref(&path));
            let (done, cancel) = delayed(&mut app, std::slice::from_ref(&path));
            app.trim_waveform(&path, &ctx);
            match action {
                "toggle" => app.toggle_clip(0, &ctx),
                "stop clips" => app.stop_clips(),
                _ => app.stop_engine(true),
            }
            assert!(!app.has_pending_playback() && !cancel.load(Ordering::Relaxed));
            app.engine = Some(Engine::for_test());
            done.send(CachedClip::read(&path, &cancel)).unwrap();
            finish(&mut app, &ctx);
            assert!(app.playing.is_empty() && app.engine.as_ref().unwrap().take_clips().is_empty());
            assert!(app.trim_waveform(&path, &ctx).is_some());
        }
    }

    #[test]
    fn switching_closing_or_removing_a_trimmed_pad_cancels_unused_work() {
        let dir = tempfile::tempdir().unwrap();
        let paths = [wav(dir.path(), "one.wav"), wav(dir.path(), "two.wav")];
        let ctx = egui::Context::default();
        for action in ["switch", "close", "remove"] {
            let mut app = app_with(&paths);
            let (late, cancel) = delayed(&mut app, &paths[..1]);
            app.trim_waveform(&paths[0], &ctx);
            app.cancel_pending_playback(None);
            match action {
                "switch" => { app.trim_waveform(&paths[1], &ctx); }
                "close" => app.close_trim_editor(),
                _ => app.remove_clip(0),
            }
            assert!(cancel.load(Ordering::Relaxed));
            assert!(late.send(Err(anyhow::anyhow!("late preview result"))).is_err());
            finish(&mut app, &ctx);
            assert!(app.cached_clip(&paths[0]).is_none());
            assert_eq!(app.cached_clip(&paths[1]).is_some(), action == "switch");
            assert!(app.playing.is_empty() && paths.iter().all(|path| path.exists()));
        }
    }

    #[test]
    fn a_failed_waveform_waits_for_retry_and_leaves_other_clip_requests_running() {
        let dir = tempfile::tempdir().unwrap();
        let paths = [dir.path().join("missing.wav"), wav(dir.path(), "play.wav")];
        let mut app = app_with(&paths);
        let ctx = egui::Context::default();
        let (failed, _) = delayed(&mut app, &paths[..1]);
        app.trim_waveform(&paths[0], &ctx);
        app.cancel_pending_playback(None);
        app.play_clip(1, &ctx);
        drop(failed);
        finish(&mut app, &ctx);
        assert_eq!(app.playing, [clip_key(&paths[1])]);
        assert!(app.trim_clip.as_ref().unwrap().error.as_ref().unwrap().contains("stopped unexpectedly"));
        for _ in 0..3 { assert!(app.trim_waveform(&paths[0], &ctx).is_none()); }
        assert!(app.clip_loads.is_none(), "a failed waveform must not retry on every frame");
        wav(dir.path(), "missing.wav");
        app.trim_clip.as_mut().unwrap().error = None;
        app.trim_waveform(&paths[0], &ctx);
        finish(&mut app, &ctx);
        assert!(app.trim_waveform(&paths[0], &ctx).is_some());
        assert_eq!(app.playing, [clip_key(&paths[1])], "retrying a waveform leaves playback alone");
    }

    #[test]
    fn the_trim_menu_shows_progress_and_closing_it_cancels_unused_decoding() {
        let dir = tempfile::tempdir().unwrap();
        let path = wav(dir.path(), "clap.wav");
        let mut app = app_with(std::slice::from_ref(&path));
        app.engine = None;
        let (_done, cancel) = delayed(&mut app, std::slice::from_ref(&path));
        let ctx = egui::Context::default();
        open_trim_menu(&mut app, &ctx, "clap");
        text_rect(&draw_board(&mut app, &ctx, Vec::new()), "Loading waveform…");
        assert!(!app.has_pending_playback() && app.trim_clip.is_some());
        app.run_command(&ctx, super::super::Command::ToggleMute);
        assert!(app.settings.mute);
        draw_board(&mut app, &ctx, vec![egui::Event::Key {
            key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE,
        }]);
        draw_board(&mut app, &ctx, Vec::new());
        assert!(app.trim_clip.is_none() && app.clip_loads.is_none() && cancel.load(Ordering::Relaxed));
    }

    #[test]
    fn the_trim_menu_retry_button_recovers_a_failed_waveform_without_playing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clap.wav");
        let mut app = app_with(std::slice::from_ref(&path));
        app.engine = None;
        let (done, _) = delayed(&mut app, std::slice::from_ref(&path));
        drop(done);
        let ctx = egui::Context::default();
        open_trim_menu(&mut app, &ctx, "clap");
        let retry = text_rect(&draw_board(&mut app, &ctx, Vec::new()), "Retry waveform").center();
        assert!(app.clip_loads.is_none());
        wav(dir.path(), "clap.wav");
        for pressed in [true, false] { draw_board(&mut app, &ctx, click(retry, egui::PointerButton::Primary, pressed)); }
        draw_board(&mut app, &ctx, Vec::new());
        finish(&mut app, &ctx);
        let output = draw_board(&mut app, &ctx, Vec::new());
        assert!(app.trim_clip.is_some(), "retry keeps the menu open");
        assert!(app.cached_clip(&path).is_some() && app.trim_clip.as_ref().unwrap().error.is_none());
        text_rect(&output, "Plays 1.0 s of 1.0 s");
        assert!(app.engine.is_none() && app.playing.is_empty());
    }

    #[test]
    #[ignore = "writes trim-menu screenshots; needs Python with Pillow"]
    fn snapshot_trim_loading() {
        let dir = tempfile::tempdir().unwrap();
        let path = wav(dir.path(), "clap.wav");
        for state in ["loading", "failed", "ready"] {
            let mut app = app_with(std::slice::from_ref(&path));
            app.engine = None;
            let (done, cancel) = delayed(&mut app, std::slice::from_ref(&path));
            match state {
                "failed" => done.send(Err(anyhow::anyhow!("Audio file unavailable; reconnect its drive and retry"))).unwrap(),
                "ready" => done.send(CachedClip::read(&path, &cancel)).unwrap(),
                _ => {}
            }
            let ctx = egui::Context::default();
            let pos = pad_icon_position(&mut app, &ctx, "clap");
            let events = vec![Vec::new(), click(pos, egui::PointerButton::Secondary, true), click(pos, egui::PointerButton::Secondary, false)];
            let image = super::super::snapshot::snapshot_with_events(
                &mut app, &format!("trim-{state}"), egui::vec2(700.0, 720.0), 1.25, true, events,
            );
            println!("SNAPSHOT {}", image.display());
        }
    }

    #[test]
    fn uncached_playback_uses_current_pad_settings_and_warms_the_cache() {
        let dir = tempfile::tempdir().unwrap();
        let path = wav(dir.path(), "applause.wav");
        let mut app = app_with(std::slice::from_ref(&path));
        let ctx = egui::Context::default();
        app.play_clip(0, &ctx);
        assert!(app.clip_loading(&path) && app.playing.is_empty());
        let cancel = Arc::clone(&app.clip_loads.as_ref().unwrap().cancel);
        app.run_command(&ctx, super::super::Command::PlayPad(path.clone()));
        assert!(Arc::ptr_eq(&cancel, &app.clip_loads.as_ref().unwrap().cancel), "repeated hotkeys reuse the decoder");
        app.settings.sounds[0].volume = 0.3;
        app.settings.sounds[0].start = 0.25;
        app.settings.sounds[0].end = Some(0.75);
        app.settings.sounds[0].name = Some("Clapping".into());
        finish(&mut app, &ctx);
        assert_eq!(app.sound_status.0, "Playing · Clapping");
        let clips = app.engine.as_ref().unwrap().take_clips();
        assert_eq!(clips.len(), 1);
        assert_eq!(clips[0].samples().len(), 24_000);
        assert_eq!(clips[0].samples().as_ptr(), app.cached_clip(&path).unwrap().samples[12_000..].as_ptr(), "trimmed playback shares the decoded cache");
        assert_eq!(clips[0].gain, 0.3);
        assert_eq!(clips[0].key, clip_key(&path));
        app.play_clip(0, &ctx);
        assert!(app.clip_loads.is_none(), "cached clips start without a worker");
        assert_eq!(app.playing, [clip_key(&path)]);
    }

    #[test]
    fn a_new_single_clip_request_cancels_older_pending_playback() {
        let dir = tempfile::tempdir().unwrap();
        let paths = [wav(dir.path(), "old.wav"), wav(dir.path(), "new.wav")];
        let ctx = egui::Context::default();
        for cached in [false, true] {
            let mut app = app_with(&paths);
            if cached { app.cache_clip(paths[1].clone(), CachedClip::read(&paths[1], &AtomicBool::new(false)).unwrap()); }
            let (late, cancel) = delayed(&mut app, &paths[..1]);
            app.play_clip(1, &ctx);
            assert!(cancel.load(Ordering::Relaxed));
            assert!(late.send(Err(anyhow::anyhow!("late old result"))).is_err());
            finish(&mut app, &ctx);
            assert_eq!(app.playing, [clip_key(&paths[1])]);
        }
    }

    #[test]
    fn overlapping_loads_continue_after_failure_and_keep_paths_when_pads_move() {
        let dir = tempfile::tempdir().unwrap();
        let paths = [dir.path().join("failed.wav"), wav(dir.path(), "one.wav"), wav(dir.path(), "two.wav")];
        let mut app = app_with(&paths);
        app.settings.overlap_clips = true;
        let (failed, _) = delayed(&mut app, &paths[..1]);
        let ctx = egui::Context::default();
        app.play_clip(1, &ctx);
        app.play_clip(2, &ctx);
        assert_eq!(app.clip_loads.as_ref().unwrap().requests.len(), 3);
        app.run_command(&ctx, super::super::Command::ToggleMute);
        assert!(app.settings.mute, "mic controls work while a decoder is pending");
        app.settings.sounds.swap(0, 2);
        drop(failed);
        finish(&mut app, &ctx);
        let clips = app.engine.as_ref().unwrap().take_clips();
        assert_eq!(clips.iter().map(|clip| clip.key).collect::<Vec<_>>(), [clip_key(&paths[1]), clip_key(&paths[2])]);
    }

    #[test]
    fn cancelling_loading_stopping_or_removing_a_pad_prevents_late_audio() {
        let dir = tempfile::tempdir().unwrap();
        let path = wav(dir.path(), "cancel.wav");
        let ctx = egui::Context::default();
        for action in ["toggle", "stop clips", "stop engine", "remove"] {
            let mut app = app_with(std::slice::from_ref(&path));
            let (late, cancel) = delayed(&mut app, std::slice::from_ref(&path));
            match action {
                "toggle" => app.toggle_clip(0, &ctx),
                "stop clips" => app.run_command(&ctx, super::super::Command::StopClips),
                "stop engine" => app.stop_engine(true),
                _ => app.remove_clip(0),
            }
            assert!(cancel.load(Ordering::Relaxed), "{action} cancels the worker");
            assert!(late.send(Err(anyhow::anyhow!("late result"))).is_err());
            app.engine = Some(Engine::for_test());
            app.poll_clip_loads(&ctx);
            assert!(app.clip_loads.is_none() && app.playing.is_empty());
            assert!(path.exists(), "cancelling or removing a pad keeps its file");
        }
    }

    #[test]
    fn a_failed_playback_load_can_be_retried_when_the_file_returns() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reconnected.wav");
        let mut app = app_with(std::slice::from_ref(&path));
        let ctx = egui::Context::default();
        app.play_clip(0, &ctx);
        finish(&mut app, &ctx);
        assert!(app.playing.is_empty());
        assert_eq!(app.sound_status.1, RED);
        wav(dir.path(), "reconnected.wav");
        app.play_clip(0, &ctx);
        finish(&mut app, &ctx);
        assert_eq!(app.playing, [clip_key(&path)]);
    }

    #[test]
    fn changed_or_removed_files_cannot_play_or_publish_a_stale_waveform() {
        for removed in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let path = wav(dir.path(), "edited.wav");
            let mut app = app_with(std::slice::from_ref(&path));
            app.trim_clip = Some(TrimClip { path: path.clone(), error: None });
            let stale = CachedClip::read(&path, &AtomicBool::new(false)).unwrap();
            let (done, _) = delayed(&mut app, std::slice::from_ref(&path));
            let replacement = || {
                crate::record::Recorder::from_samples_for_test(&[0.5; 240])
                    .finish().unwrap().unwrap().save_wav(&path).unwrap();
            };
            if removed { std::fs::remove_file(&path).unwrap(); } else { replacement(); }
            assert!(done.send(Ok(stale)).is_ok());
            let ctx = egui::Context::default();
            app.poll_clip_loads(&ctx);
            assert!(app.clip_loads.is_none());
            assert!(app.engine.as_ref().unwrap().take_clips().is_empty(), "do not play an earlier file version");
            assert!(!app.clip_cache.contains_key(&path), "do not publish a stale waveform");
            assert!(app.trim_clip.as_ref().unwrap().error.is_some());
            assert_eq!(app.sound_status.1, RED);

            if removed { replacement(); }
            app.play_clip(0, &ctx);
            finish(&mut app, &ctx);
            let clips = app.engine.as_ref().unwrap().take_clips();
            assert_eq!(clips.len(), 1);
            assert_eq!(clips[0].samples().len(), 120, "retry reads the replacement file");
            assert!(clips[0].samples().iter().all(|sample| (*sample - 0.5).abs() < 0.001));
            assert!(app.trim_clip.as_ref().unwrap().error.is_none());
        }
    }

    #[test]
    fn cancelling_one_overlapping_load_keeps_the_other_requests() {
        let dir = tempfile::tempdir().unwrap();
        let paths = [wav(dir.path(), "one.wav"), wav(dir.path(), "two.wav"), wav(dir.path(), "three.wav")];
        let ctx = egui::Context::default();
        for cancelled in [0, 1] {
            let mut app = app_with(&paths);
            app.settings.overlap_clips = true;
            let (done, cancel) = delayed(&mut app, &paths);
            app.toggle_clip(cancelled, &ctx);
            assert_eq!(cancel.load(Ordering::Relaxed), cancelled == 0, "only cancelling the running decoder sets its flag");
            if cancelled == 0 {
                assert!(done.send(Err(anyhow::anyhow!("late cancelled result"))).is_err());
            } else {
                done.send(CachedClip::read(&paths[0], &cancel)).unwrap();
            }
            finish(&mut app, &ctx);
            let clips = app.engine.as_ref().unwrap().take_clips();
            let expected: Vec<_> = paths.iter().enumerate().filter(|(i, _)| *i != cancelled).map(|(_, p)| clip_key(p)).collect();
            assert_eq!(clips.iter().map(|clip| clip.key).collect::<Vec<_>>(), expected);
        }
    }

    #[test]
    fn the_board_and_strip_show_loading_and_stop_clips_cancels_it() {
        use eframe::App as _;
        let dir = tempfile::tempdir().unwrap();
        let path = wav(dir.path(), "pending.wav");
        let mut app = app_with(std::slice::from_ref(&path));
        app.settings.sounds[0].starred = true;
        let (_done, cancel) = delayed(&mut app, std::slice::from_ref(&path));
        assert!(app.strip_contents().pads[0].3, "the strip shows a pending pad");
        let ctx = egui::Context::default();
        let mut frame = eframe::Frame::_new_kittest();
        let mut draw = |app: &mut App, events| ctx.run_ui(egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(700.0, 720.0))),
            events, ..Default::default()
        }, |ui| app.ui(ui, &mut frame));
        draw(&mut app, Vec::new());
        let output = draw(&mut app, Vec::new());
        let label = |name| output.shapes.iter().find_map(|shape| match &shape.shape {
            egui::Shape::Text(text) if text.galley.text() == name => Some(text.visual_bounding_rect().center()),
            _ => None,
        });
        assert!(label("Loading…").is_some());
        let pos = label("Stop clips").unwrap();
        for pressed in [true, false] {
            draw(&mut app, vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE },
            ]);
        }
        assert!(app.clip_loads.is_none() && cancel.load(Ordering::Relaxed));
    }

    #[test]
    #[ignore = "writes pending-playback screenshots; needs Python with Pillow"]
    fn snapshot_loading_clips() {
        let dir = tempfile::tempdir().unwrap();
        let paths = [wav(dir.path(), "applause.wav"), wav(dir.path(), "airhorn.wav")];
        let mut app = app_with(&paths);
        app.settings.overlap_clips = true;
        let (_done, _) = delayed(&mut app, &paths);
        app.sound_status = ("Loading · applause".into(), CYAN);
        for (height, name) in [(720.0, "soundboard-loading"), (420.0, "soundboard-loading-short")] {
            let path = super::super::snapshot::snapshot(&mut app, name, egui::vec2(700.0, height), 1.25, true);
            println!("SNAPSHOT {}", path.display());
        }
    }
}
