//! Cached clip audio and cancellable background loads for soundboard playback.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};

use anyhow::Context;
use crossbeam_channel::{Receiver, TryRecvError};
use eframe::egui::{self, Color32};

use super::{App, CachedClip, AMBER, CYAN, GREEN, MUTED, RED, clip_key, short, trimmed};
use crate::config::Pad;
use crate::dsp::{self, Clip};
use crate::engine::Engine;

#[derive(Default)]
pub(super) struct ClipPlayback {
    /// The front path belongs to the running decoder, if any.
    paths: VecDeque<PathBuf>,
    job: Option<Receiver<anyhow::Result<CachedClip>>>,
    cancel: Arc<AtomicBool>,
}

impl Drop for ClipPlayback {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl CachedClip {
    pub(super) fn read(path: &Path, cancel: &AtomicBool) -> anyhow::Result<Self> {
        let metadata = std::fs::metadata(path).with_context(|| format!("read {}", path.display()))?;
        let samples = crate::decode::load_clip_cancellable(path, cancel)?;
        Ok(Self { samples: Arc::new(samples), bytes: metadata.len(), modified: metadata.modified().ok() })
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

    fn cached_clip_samples(&mut self, path: &Path) -> Option<Arc<Vec<f32>>> {
        if let Some(cached) = self.clip_cache.get(path)
            && let Ok(metadata) = std::fs::metadata(path)
            && cached.bytes == metadata.len()
            && cached.modified == metadata.modified().ok()
        {
            return Some(Arc::clone(&cached.samples));
        }
        self.forget_clip(path);
        None
    }

    /// The trim editor needs the whole clip; playback uses the queue below.
    pub(super) fn clip_samples(&mut self, path: &Path) -> anyhow::Result<Arc<Vec<f32>>> {
        if let Some(samples) = self.cached_clip_samples(path) {
            return Ok(samples);
        }
        let clip = CachedClip::read(path, &AtomicBool::new(false))?;
        let samples = Arc::clone(&clip.samples);
        self.cache_clip(path.to_owned(), clip);
        Ok(samples)
    }

    pub(super) fn cache_clip(&mut self, path: PathBuf, clip: CachedClip) {
        self.forget_clip(&path);
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
        if self.trim_peaks.as_ref().is_some_and(|(cached_path, _)| cached_path == path) {
            self.trim_peaks = None;
        }
    }

    pub(super) fn clip_loading(&self, path: &Path) -> bool {
        self.clip_playback.as_ref().is_some_and(|pending| pending.paths.iter().any(|p| p == path))
    }

    pub(super) fn cancel_clip_loading(&mut self, path: &Path) {
        let Some(pending) = &mut self.clip_playback else { return };
        if pending.paths.front().is_some_and(|p| p == path) {
            pending.cancel.store(true, Ordering::Relaxed);
            pending.job = None;
            pending.cancel = Arc::default();
        }
        pending.paths.retain(|p| p != path);
        if pending.paths.is_empty() {
            self.clip_playback = None;
        }
    }

    /// Card and strip clicks cancel a pending load, or pause/resume playback.
    pub(super) fn toggle_clip(&mut self, index: usize, ctx: &egui::Context) {
        let Some(pad) = self.settings.sounds.get(index).cloned() else { return };
        if self.clip_loading(&pad.path) {
            self.cancel_clip_loading(&pad.path);
            self.sound_status = (format!("Cancelled · {}", pad.display_name()), MUTED);
            return;
        }
        let paused = self.engine.as_ref().and_then(|engine| {
            engine.toggle_sound_pause(clip_key(&pad.path), self.settings.overlap_clips)
        });
        if let Some(paused) = paused {
            if !paused && !self.settings.overlap_clips {
                self.clip_playback = None;
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
            if let Some(pending) = &mut self.clip_playback
                && pending.paths.front() == Some(&pad.path)
            {
                pending.paths.truncate(1); // reuse this decoder, cancel later requests
            } else {
                self.clip_playback = None;
            }
        }
        if self.clip_loading(&pad.path) {
            return; // repeated hotkeys reuse the same pending load
        }
        if let Some(samples) = self.cached_clip_samples(&pad.path) {
            self.play_decoded_clip(&pad, &samples);
            return;
        }
        self.clip_playback.get_or_insert_with(ClipPlayback::default).paths.push_back(pad.path);
        self.poll_clip_playback(ctx);
    }

    fn play_decoded_clip(&mut self, pad: &Pad, samples: &Arc<Vec<f32>>) {
        let Some(engine) = &self.engine else { return };
        engine.play_sound(Clip::new(clip_key(&pad.path), trimmed(samples, pad.start, pad.end), pad.volume), self.settings.overlap_clips);
        self.sound_status = (format!("Playing · {}", pad.display_name()), GREEN);
        self.sync_clip_progress();
    }

    pub(super) fn poll_clip_playback(&mut self, ctx: &egui::Context) {
        let Some(mut pending) = self.clip_playback.take() else { return };
        if self.engine.is_none() {
            return;
        }
        let result = pending.job.as_ref().and_then(|job| match job.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err(anyhow::anyhow!("Clip decoder stopped unexpectedly; try again"))),
        });
        if let Some(result) = result {
            pending.job = None;
            let path = pending.paths.pop_front().expect("a decoder has a clip path");
            match result {
                Ok(clip) => {
                    let samples = Arc::clone(&clip.samples);
                    self.cache_clip(path.clone(), clip);
                    if let Some(pad) = self.settings.sounds.iter().find(|pad| pad.path == path).cloned() {
                        self.play_decoded_clip(&pad, &samples);
                    }
                }
                Err(error) => {
                    crate::logfile::error(format_args!("could not play {}: {error:#}", path.display()));
                    self.sound_status = (short(&format!("{error:#}")), RED);
                }
            }
        }
        while pending.job.is_none() && let Some(path) = pending.paths.front().cloned() {
            let Some(pad) = self.settings.sounds.iter().find(|pad| pad.path == path) else {
                pending.paths.pop_front();
                continue;
            };
            self.sound_status = (format!("Loading · {}", pad.display_name()), CYAN);
            let (done, job) = crossbeam_channel::bounded(1);
            let (cancel, wake) = (Arc::clone(&pending.cancel), ctx.clone());
            let spawned = std::thread::Builder::new().name("openmic-clip-playback".into()).spawn(move || {
                let _ = done.send(CachedClip::read(&path, &cancel));
                wake.request_repaint();
            });
            match spawned {
                Ok(_) => pending.job = Some(job),
                Err(error) => {
                    pending.paths.pop_front();
                    self.sound_status = (short(&format!("Could not load clip: {error}")), RED);
                }
            }
        }
        if pending.job.is_some() {
            self.clip_playback = Some(pending);
        }
    }

    pub(super) fn stop_clips(&mut self) {
        self.clip_playback = None;
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
        app.clip_playback = Some(ClipPlayback { paths: paths.to_vec().into(), job: Some(job), cancel: Arc::clone(&cancel) });
        (done, cancel)
    }

    fn finish(app: &mut App, ctx: &egui::Context) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while app.clip_playback.is_some() {
            app.poll_clip_playback(ctx);
            assert!(std::time::Instant::now() < deadline, "clip playback load did not finish");
            std::thread::sleep(std::time::Duration::from_millis(1));
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
        let cancel = Arc::clone(&app.clip_playback.as_ref().unwrap().cancel);
        app.run_command(&ctx, super::super::Command::PlayPad(path.clone()));
        assert!(Arc::ptr_eq(&cancel, &app.clip_playback.as_ref().unwrap().cancel), "repeated hotkeys reuse the decoder");
        app.settings.sounds[0].volume = 0.3;
        app.settings.sounds[0].start = 0.25;
        app.settings.sounds[0].end = Some(0.75);
        app.settings.sounds[0].name = Some("Clapping".into());
        finish(&mut app, &ctx);
        assert_eq!(app.sound_status.0, "Playing · Clapping");
        let clips = app.engine.as_ref().unwrap().take_clips();
        assert_eq!(clips.len(), 1);
        assert_eq!(clips[0].samples.len(), 24_000);
        assert_eq!(clips[0].gain, 0.3);
        assert_eq!(clips[0].key, clip_key(&path));
        app.play_clip(0, &ctx);
        assert!(app.clip_playback.is_none(), "cached clips start without a worker");
        assert_eq!(app.playing, [clip_key(&path)]);
    }

    #[test]
    fn a_new_single_clip_request_cancels_older_pending_playback() {
        let dir = tempfile::tempdir().unwrap();
        let paths = [wav(dir.path(), "old.wav"), wav(dir.path(), "new.wav")];
        let ctx = egui::Context::default();
        for cached in [false, true] {
            let mut app = app_with(&paths);
            if cached { app.clip_samples(&paths[1]).unwrap(); }
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
        assert_eq!(app.clip_playback.as_ref().unwrap().paths.len(), 3);
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
            app.poll_clip_playback(&ctx);
            assert!(app.clip_playback.is_none() && app.playing.is_empty());
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
        assert!(app.clip_playback.is_none() && cancel.load(Ordering::Relaxed));
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
