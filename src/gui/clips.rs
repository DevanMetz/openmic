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

#[derive(Default)]
pub(super) struct ClipLoads {
    /// The front request belongs to the running decoder unless it was cancelled.
    requests: VecDeque<ClipRequest>,
    job: Option<Receiver<anyhow::Result<CachedClip>>>,
    cancel: Arc<AtomicBool>,
}

impl ClipLoads {
    fn retain(&mut self, keep: impl Fn(&ClipRequest) -> bool) {
        if self.job.is_some() && !self.cancel.load(Ordering::Relaxed)
            && self.requests.front().is_some_and(|request| !keep(request))
        {
            self.cancel.store(true, Ordering::Relaxed);
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
        self.playing = progress.iter().filter(|clip| !clip.paused).map(|clip| clip.key).collect();
        self.pad_progress = progress.into_iter().map(|clip| (clip.key, clip)).collect();
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
        if self.trim_clip.as_deref() != Some(path) {
            self.close_trim_editor();
            self.trim_clip = Some(path.to_owned());
        }
        if let Some(clip) = self.cached_clip(path) {
            return Some((clip.samples.len() as f32 / dsp::SR as f32, Arc::clone(&clip.peaks)));
        }
        if !self.clip_errors.contains_key(path) {
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
        // Keep the cache to about ten minutes of audio (~110 MB).
        let cached: usize = self.clip_cache.values().map(|clip| clip.samples.len()).sum();
        if cached + clip.samples.len() > dsp::SR as usize * 600 {
            self.clip_cache.clear();
        }
        self.clip_cache.insert(path, clip);
    }

    pub(super) fn forget_clip(&mut self, path: &Path) {
        self.clip_cache.remove(path);
        self.clip_errors.remove(path);
        self.pad_durations.forget(path);
    }

    pub(super) fn clip_loading(&self, path: &Path) -> bool {
        self.clip_loads.as_ref().is_some_and(|pending| pending.requests.iter().any(|r| r.play && r.path == path))
    }

    pub(super) fn has_pending_playback(&self) -> bool {
        self.clip_loads.as_ref().is_some_and(|pending| pending.requests.iter().any(|r| r.play))
    }

    fn queue_clip(&mut self, path: &Path, play: bool) {
        self.clip_errors.remove(path);
        let pending = self.clip_loads.get_or_insert_with(ClipLoads::default);
        if let Some(request) = pending.requests.iter_mut().find(|r| r.path == path) {
            request.play |= play;
        } else {
            pending.requests.push_back(ClipRequest { path: path.to_owned(), play });
        }
    }

    /// Cancel unneeded work, but keep tracking its decoder until it finishes.
    fn prune_clip_loads(&mut self) {
        let Some(pending) = &mut self.clip_loads else { return };
        pending.retain(|request| request.play || self.trim_clip.as_ref() == Some(&request.path));
        if pending.requests.is_empty() && pending.job.is_none() { self.clip_loads = None; }
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
        if self.trim_clip.as_deref() == Some(path) { self.trim_clip = None; }
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
        self.queue_clip(&pad.path, true);
        self.poll_clip_loads(ctx);
    }

    fn play_decoded_clip(&mut self, pad: &Pad, samples: &Arc<Vec<f32>>) {
        let Some(engine) = &self.engine else { return };
        let clip = Clip::new(clip_key(&pad.path), Arc::clone(samples), pad.volume).with_trim(pad.start, pad.end);
        if clip.samples().is_empty() {
            self.sound_status = (format!("No audio in the trim for {}. Right-click the pad to adjust it.", pad.display_name()), AMBER);
            return;
        }
        engine.play_sound(clip, self.settings.overlap_clips);
        self.sound_status = (format!("Playing · {}", pad.display_name()), GREEN);
        self.sync_clip_progress();
    }

    fn clip_load_failed(&mut self, request: &ClipRequest, error: &anyhow::Error) {
        crate::logfile::error(format_args!("could not load {}: {error:#}", request.path.display()));
        let message = format!("{error:#}");
        if request.play { self.sound_status = (short(&message), RED); }
        self.clip_errors.insert(request.path.clone(), message);
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
            if pending.cancel.load(Ordering::Relaxed) {
                // Its request was removed. Do not consume the next queued request
                // or let a late success/failure replace the current playback state.
                pending.cancel = Arc::default();
            } else {
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
        draw_board_size(app, ctx, egui::vec2(700.0, 720.0), events)
    }

    fn draw_board_size(app: &mut App, ctx: &egui::Context, size: egui::Vec2, events: Vec<egui::Event>) -> egui::FullOutput {
        use eframe::App as _;
        let mut frame = eframe::Frame::_new_kittest();
        ctx.run_ui(egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
            time: Some(ctx.cumulative_frame_nr() as f64 * 0.2), events, ..Default::default()
        }, |ui| app.ui(ui, &mut frame))
    }

    fn text_rect(output: &egui::FullOutput, name: &str) -> egui::Rect {
        output.shapes.iter().find_map(|shape| match &shape.shape {
            egui::Shape::Text(text) if text.galley.text() == name => Some(text.visual_bounding_rect()),
            _ => None,
        }).unwrap_or_else(|| panic!("{name} not drawn"))
    }

    fn trim_waveform_rect(output: &egui::FullOutput) -> egui::Rect {
        let top = text_rect(output, "Trim").bottom();
        let bottom = text_rect(output, "Start").top();
        output.shapes.iter().find_map(|shape| match &shape.shape {
            egui::Shape::Rect(rect) if rect.rect.top() > top && rect.rect.bottom() < bottom && rect.rect.height() == 44.0 => Some(rect.rect),
            _ => None,
        }).expect("the trim waveform is drawn").shrink2(egui::vec2(4.0, 0.0))
    }

    fn click(pos: egui::Pos2, button: egui::PointerButton, pressed: bool) -> Vec<egui::Event> {
        vec![egui::Event::PointerMoved(pos), egui::Event::PointerButton { pos, button, pressed, modifiers: egui::Modifiers::NONE }]
    }

    #[test]
    fn keyboard_navigation_reveals_the_focused_pad_in_a_long_minimum_size_board() {
        let paths: Vec<_> = (0..33).map(|i| std::path::PathBuf::from(format!("keyboard-clip-{i}.wav"))).collect();
        let visible = |output: &egui::FullOutput| output.shapes.iter().any(|shape| matches!(&shape.shape,
            egui::Shape::Text(text) if text.galley.text() == "target" && shape.clip_rect.contains_rect(text.visual_bounding_rect())));
        for dark in [true, false] {
            for page_scrolled in [false, true] {
                for label in ["Play target", "Rename target", "Add target to the strip", "Remove target"] {
                    let mut app = app_with(&paths);
                    app.settings.sounds[32].name = Some("target".into());
                    let original = app.settings.sounds.clone();
                    let status = app.sound_status.clone();
                    let ctx = egui::Context::default();
                    ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                    ctx.enable_accesskit();
                    let size = egui::vec2(700.0, 420.0);
                    let output = draw_board_size(&mut app, &ctx, size, Vec::new());
                    let target = *output.platform_output.accesskit_update.as_ref().unwrap().nodes.iter()
                        .find(|(_, node)| node.label() == Some(label)).map(|(id, _)| id).unwrap();
                    draw_board_size(&mut app, &ctx, size, Vec::new());
                    if page_scrolled {
                        draw_board_size(&mut app, &ctx, size, vec![
                            egui::Event::PointerMoved(egui::pos2(300.0, 55.0)),
                            egui::Event::MouseWheel {
                                unit: egui::MouseWheelUnit::Point, delta: egui::vec2(0.0, -1000.0),
                                modifiers: egui::Modifiers::NONE, phase: egui::TouchPhase::Move,
                            },
                        ]);
                        for _ in 0..5 { draw_board_size(&mut app, &ctx, size, Vec::new()); }
                    }
                    let mut focused = false;
                    for _ in 0..180 {
                        let output = draw_board_size(&mut app, &ctx, size, vec![egui::Event::Key {
                            key: egui::Key::Tab, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE,
                        }]);
                        if output.platform_output.accesskit_update.unwrap().focus == target { focused = true; break; }
                    }
                    assert!(focused, "Tab reaches {label}");
                    for _ in 0..6 { draw_board_size(&mut app, &ctx, size, Vec::new()); }
                    let output = draw_board_size(&mut app, &ctx, size, Vec::new());
                    assert!(visible(&output), "the focused {label} is visible; dark={dark}, page_scrolled={page_scrolled}");
                    let tree = output.platform_output.accesskit_update.unwrap();
                    assert_eq!(tree.focus, target);
                    let bounds = tree.nodes.iter().find(|(id, _)| *id == target).unwrap().1.bounds().unwrap();
                    let control = egui::Rect::from_min_max(
                        egui::pos2(bounds.x0 as f32, bounds.y0 as f32), egui::pos2(bounds.x1 as f32, bounds.y1 as f32),
                    );
                    assert!(output.shapes.iter().any(|shape| matches!(&shape.shape,
                        egui::Shape::Rect(rect) if (rect.rect.height() - 108.0).abs() < 0.01
                            && rect.rect.contains(control.center()) && shape.clip_rect.contains_rect(control))),
                        "the whole focused control is visible");
                    assert_eq!(app.settings.sounds, original, "Tab never edits or removes a pad");
                    assert_eq!(app.sound_status, status);
                    assert!(app.clip_loads.is_none() && app.playing.is_empty() && app.renaming_pad.is_none());
                    for modifiers in [egui::Modifiers::SHIFT, egui::Modifiers::NONE] {
                        draw_board_size(&mut app, &ctx, size, vec![egui::Event::Key {
                            key: egui::Key::Tab, physical_key: None, pressed: true, repeat: false, modifiers,
                        }]);
                        // Shift+Tab transfers focus at the start of the next frame.
                        for _ in 0..6 { draw_board_size(&mut app, &ctx, size, Vec::new()); }
                    }
                    let output = draw_board_size(&mut app, &ctx, size, Vec::new());
                    assert!(visible(&output), "Shift+Tab and Tab return to the visible focused pad");
                    assert_eq!(output.platform_output.accesskit_update.unwrap().focus, target);
                    for delta in [1000.0, -1000.0] {
                        draw_board_size(&mut app, &ctx, size, vec![
                            egui::Event::PointerMoved(control.center()),
                            egui::Event::MouseWheel {
                                unit: egui::MouseWheelUnit::Point, delta: egui::vec2(0.0, delta),
                                modifiers: egui::Modifiers::NONE, phase: egui::TouchPhase::Move,
                            },
                        ]);
                        for _ in 0..6 { draw_board_size(&mut app, &ctx, size, Vec::new()); }
                        let output = draw_board_size(&mut app, &ctx, size, Vec::new());
                        assert_eq!(visible(&output), delta < 0.0, "focus permits scrolling away and back for {label}");
                        assert_eq!(output.platform_output.accesskit_update.unwrap().focus, target);
                    }
                    if page_scrolled {
                        draw_board_size(&mut app, &ctx, size, vec![
                            egui::Event::PointerMoved(egui::pos2(300.0, 55.0)),
                            egui::Event::MouseWheel {
                                unit: egui::MouseWheelUnit::Point, delta: egui::vec2(0.0, -1000.0),
                                modifiers: egui::Modifiers::NONE, phase: egui::TouchPhase::Move,
                            },
                        ]);
                        for _ in 0..6 { draw_board_size(&mut app, &ctx, size, Vec::new()); }
                    }
                    let pad = app.settings.sounds.remove(32);
                    app.settings.sounds.insert(0, pad);
                    for _ in 0..6 { draw_board_size(&mut app, &ctx, size, Vec::new()); }
                    let output = draw_board_size(&mut app, &ctx, size, Vec::new());
                    assert!(visible(&output), "moving {label} into view; dark={dark}, page_scrolled={page_scrolled}");
                    assert_eq!(output.platform_output.accesskit_update.unwrap().focus, target);
                    assert_eq!(app.sound_status, status);
                    assert!(app.clip_loads.is_none() && app.playing.is_empty() && app.renaming_pad.is_none());
                }
            }
        }
    }

    #[test]
    fn keyboard_navigation_reveals_playback_controls_below_a_long_board() {
        use egui::accesskit::Role;
        let dir = tempfile::tempdir().unwrap();
        let paths: Vec<_> = (0..33).map(|index| wav(dir.path(), &format!("playback-keyboard-{index}.wav"))).collect();
        let size = egui::vec2(700.0, 420.0);
        let key = |key, modifiers| vec![egui::Event::Key {
            key, physical_key: None, pressed: true, repeat: false, modifiers,
        }];
        for dark in [true, false] {
            for passes in [1, 2] {
                for control in ["stop", "slider", "number", "overlap"] {
                    let mut app = app_with(&paths);
                    app.settings.sound_volume = 0.61347;
                    app.settings.overlap_clips = false;
                    let ctx = egui::Context::default();
                    ctx.options_mut(|options| options.max_passes = passes.try_into().unwrap());
                    ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                    ctx.enable_accesskit();
                    app.cache_clip(paths[0].clone(), CachedClip::read(&paths[0], &AtomicBool::new(false)).unwrap());
                    app.play_clip(0, &ctx);
                    let original = serde_json::to_value(&app.settings).unwrap();
                    let playing = app.playing.clone();
                    let progress = app.pad_progress.clone();
                    let status = app.sound_status.clone();
                    for _ in 0..3 { draw_board_size(&mut app, &ctx, size, Vec::new()); }
                    let output = draw_board_size(&mut app, &ctx, size, Vec::new());
                    let tree = output.platform_output.accesskit_update.as_ref().unwrap();
                    let target = *tree.nodes.iter().find(|(_, node)| match control {
                        "stop" => node.role() == Role::Button && node.label() == Some("Stop clips"),
                        "slider" => node.role() == Role::Slider && node.numeric_value() == Some(f64::from(0.61347f32)),
                        "number" => node.role() == Role::SpinButton && node.numeric_value() == Some(f64::from(0.61347f32)),
                        _ => node.role() == Role::CheckBox && node.label() == Some("Let clips overlap"),
                    }).map(|(id, _)| id).unwrap();
                    let mut focused = false;
                    for _ in 0..200 {
                        let output = draw_board_size(&mut app, &ctx, size, key(egui::Key::Tab, egui::Modifiers::NONE));
                        if output.platform_output.accesskit_update.unwrap().focus == target { focused = true; break; }
                    }
                    assert!(focused, "Tab reaches the playback {control}; passes={passes}");
                    for _ in 0..8 { draw_board_size(&mut app, &ctx, size, Vec::new()); }
                    let output = draw_board_size(&mut app, &ctx, size, Vec::new());
                    let tree = output.platform_output.accesskit_update.as_ref().unwrap();
                    assert_eq!(tree.focus, target);
                    let bounds = tree.nodes.iter().find(|(id, _)| *id == target).unwrap().1.bounds().unwrap();
                    let rect = egui::Rect::from_min_max(egui::pos2(bounds.x0 as f32, bounds.y0 as f32),
                        egui::pos2(bounds.x1 as f32, bounds.y1 as f32));
                    assert!(output.shapes.iter().any(|shape| shape.clip_rect.contains_rect(rect)
                        && match &shape.shape {
                            egui::Shape::Rect(shape) => shape.rect.intersects(rect),
                            egui::Shape::Text(shape) => shape.visual_bounding_rect().intersects(rect),
                            _ => false,
                        }), "the focused playback {control} is fully visible; dark={dark}, passes={passes}, rect={rect:?}");
                    for modifiers in [egui::Modifiers::SHIFT, egui::Modifiers::NONE] {
                        draw_board_size(&mut app, &ctx, size, key(egui::Key::Tab, modifiers));
                        for _ in 0..8 { draw_board_size(&mut app, &ctx, size, Vec::new()); }
                    }
                    let output = draw_board_size(&mut app, &ctx, size, Vec::new());
                    let tree = output.platform_output.accesskit_update.as_ref().unwrap();
                    assert_eq!(tree.focus, target, "Shift+Tab and Tab return to the playback {control}");
                    let bounds = tree.nodes.iter().find(|(id, _)| *id == target).unwrap().1.bounds().unwrap();
                    assert!(bounds.y0 >= 44.0 && bounds.y1 <= f64::from(size.y), "returning focus reveals the control again");
                    let stop_right = tree.nodes.iter().find(|(_, node)| node.role() == Role::Button
                        && node.label() == Some("Stop clips")).unwrap().1.bounds().unwrap().x1 as f32;
                    let wheel_pos = output.shapes.iter().find_map(|shape| match &shape.shape {
                        egui::Shape::Text(text) if text.galley.text() == "PLAYBACK" =>
                            Some(egui::pos2(stop_right + 6.0, text.visual_bounding_rect().center().y)),
                        _ => None,
                    }).unwrap();
                    for (delta, visible) in [(1000.0, false), (-1000.0, true)] {
                        draw_board_size(&mut app, &ctx, size, vec![
                            egui::Event::PointerMoved(wheel_pos),
                            egui::Event::MouseWheel {
                                unit: egui::MouseWheelUnit::Point, delta: egui::vec2(0.0, delta),
                                modifiers: egui::Modifiers::NONE, phase: egui::TouchPhase::Move,
                            },
                        ]);
                        for _ in 0..8 { draw_board_size(&mut app, &ctx, size, Vec::new()); }
                        let output = draw_board_size(&mut app, &ctx, size, Vec::new());
                        let tree = output.platform_output.accesskit_update.as_ref().unwrap();
                        assert_eq!(tree.focus, target, "wheel scrolling keeps keyboard focus");
                        let bounds = tree.nodes.iter().find(|(id, _)| *id == target).unwrap().1.bounds().unwrap();
                        assert_eq!(bounds.y0 >= 44.0 && bounds.y1 <= f64::from(size.y), visible,
                            "the focused playback {control} permits scrolling away and back; bounds={bounds:?}, pointer={wheel_pos:?}");
                    }
                    assert_eq!(serde_json::to_value(&app.settings).unwrap(), original, "navigation preserves settings precision");
                    assert_eq!(app.playing, playing);
                    assert_eq!(app.pad_progress, progress);
                    assert_eq!(app.sound_status, status);
                    assert!(app.dirty_since.is_none() && app.clip_loads.is_none());
                    let action = match control { "slider" => egui::Key::ArrowRight, "number" => egui::Key::ArrowUp,
                        "overlap" => egui::Key::Space, _ => egui::Key::Enter };
                    draw_board_size(&mut app, &ctx, size, key(action, egui::Modifiers::NONE));
                    for _ in 0..3 { draw_board_size(&mut app, &ctx, size, Vec::new()); }
                    match control {
                        "stop" => assert!(app.playing.is_empty() && app.pad_progress.is_empty()
                            && app.engine.as_ref().unwrap().take_clips().is_empty()),
                        "overlap" => assert!(app.settings.overlap_clips),
                        _ => assert!(app.settings.sound_volume > 0.61347 && app.params().sound_gain == app.settings.sound_volume),
                    }
                    assert_eq!(app.settings.sounds, paths.iter().cloned().map(Pad::new).collect::<Vec<_>>());
                }
            }
        }
    }

    #[test]
    fn clicking_a_visible_pad_does_not_jump_the_page_before_the_mouse_is_released() {
        let paths: Vec<_> = (0..33).map(|i| std::path::PathBuf::from(format!("mouse-clip-{i}.wav"))).collect();
        let mut app = app_with(&paths);
        app.settings.sounds[3].name = Some("visible".into());
        let path = app.settings.sounds[3].path.clone();
        let ctx = egui::Context::default();
        let size = egui::vec2(700.0, 420.0);
        for _ in 0..2 { draw_board_size(&mut app, &ctx, size, Vec::new()); }
        draw_board_size(&mut app, &ctx, size, vec![
            egui::Event::PointerMoved(egui::pos2(300.0, 55.0)),
            egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point, delta: egui::vec2(0.0, -100.0),
                modifiers: egui::Modifiers::NONE, phase: egui::TouchPhase::Move,
            },
        ]);
        for _ in 0..6 { draw_board_size(&mut app, &ctx, size, Vec::new()); }
        let output = draw_board_size(&mut app, &ctx, size, Vec::new());
        let title = text_rect(&output, "visible");
        assert!(output.shapes.iter().any(|shape| matches!(&shape.shape,
            egui::Shape::Text(text) if text.galley.text() == "visible" && shape.clip_rect.contains_rect(text.visual_bounding_rect()))));
        draw_board_size(&mut app, &ctx, size, click(title.center(), egui::PointerButton::Primary, true));
        let output = draw_board_size(&mut app, &ctx, size, Vec::new());
        assert_eq!(text_rect(&output, "visible"), title, "mouse focus preserves the page position");
        draw_board_size(&mut app, &ctx, size, click(title.center(), egui::PointerButton::Primary, false));
        assert!(app.renaming_pad.is_some(), "releasing the mouse opens the rename field");
        draw_board_size(&mut app, &ctx, size, vec![
            egui::Event::Text("renamed".into()),
            egui::Event::Key { key: egui::Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE },
        ]);
        assert_eq!(app.settings.sounds.iter().find(|pad| pad.path == path).unwrap().display_name(), "renamed");
        assert!(app.playing.is_empty() && app.clip_loads.is_none());
    }

    fn pad_icon_position(app: &mut App, ctx: &egui::Context, name: &str) -> egui::Pos2 {
        draw_board(app, ctx, Vec::new());
        let title = text_rect(&draw_board(app, ctx, Vec::new()), name);
        egui::pos2(title.left() - 26.0, title.center().y)
    }

    #[test]
    #[ignore = "writes keyboard navigation screenshots; needs Python with Pillow"]
    fn snapshot_soundboard_keyboard_focus() {
        let paths: Vec<_> = (0..33).map(|i| std::path::PathBuf::from(format!("keyboard-clip-{i}.wav"))).collect();
        let size = egui::vec2(700.0, 420.0);
        for (dark, theme) in [(true, "dark"), (false, "light")] {
            for (control, label) in [("play", "Play target"), ("star", "Add target to the strip")] {
                let mut app = app_with(&paths);
                app.settings.sounds[32].name = Some("target".into());
                let original = app.settings.sounds.clone();
                let ctx = egui::Context::default();
                ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                ctx.enable_accesskit();
                let output = draw_board_size(&mut app, &ctx, size, Vec::new());
                let target = *output.platform_output.accesskit_update.unwrap().nodes.iter()
                    .find(|(_, node)| node.label() == Some(label)).map(|(id, _)| id).unwrap();
                let mut events = vec![Vec::new()];
                let mut focused = false;
                for _ in 0..180 {
                    let tab = vec![egui::Event::Key {
                        key: egui::Key::Tab, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE,
                    }];
                    let output = draw_board_size(&mut app, &ctx, size, tab.clone());
                    events.push(tab);
                    if output.platform_output.accesskit_update.unwrap().focus == target { focused = true; break; }
                }
                assert!(focused);
                let image = super::super::snapshot::snapshot_with_events(
                    &mut app, &format!("board-keyboard-{control}-{theme}"), size, 1.25, dark, events,
                );
                println!("SNAPSHOT {}", image.display());
                assert_eq!(app.settings.sounds, original);
                assert!(app.playing.is_empty() && app.clip_loads.is_none());
            }
        }
    }

    #[test]
    #[ignore = "writes rename focus screenshots; needs Python with Pillow"]
    fn snapshot_soundboard_rename_focus() {
        let paths: Vec<_> = (0..33).map(|index| PathBuf::from(format!("rename-clip-{index}.wav"))).collect();
        for (dark, theme) in [(true, "dark"), (false, "light")] {
            let mut app = app_with(&paths);
            app.settings.sounds[32].name = Some("target".into());
            let original = app.settings.sounds.clone();
            app.begin_pad_rename(&app.settings.sounds[32].clone());
            let image = super::super::snapshot::snapshot_with_events(&mut app, &format!("board-rename-focus-{theme}"),
                egui::vec2(700.0, 420.0), 1.25, dark, vec![Vec::new(); 8]);
            println!("SNAPSHOT {}", image.display());
            assert_eq!(app.settings.sounds, original);
            assert!(app.renaming_pad.is_some());
        }
    }

    #[test]
    #[ignore = "writes playback keyboard focus screenshots; needs Python with Pillow"]
    fn snapshot_soundboard_playback_focus() {
        use egui::accesskit::Role;
        let paths: Vec<_> = (0..33).map(|index| PathBuf::from(format!("playback-clip-{index}.wav"))).collect();
        let size = egui::vec2(700.0, 420.0);
        for (dark, theme) in [(true, "dark"), (false, "light")] {
            for control in ["stop", "slider", "number", "overlap"] {
                let mut app = app_with(&paths);
                app.settings.sound_volume = 0.61347;
                let key = clip_key(&paths[0]);
                app.playing.push(key);
                app.pad_progress.insert(key, crate::dsp::ClipProgress { key, fraction: 0.35, paused: false, duration: 2.0 });
                let original = serde_json::to_value(&app.settings).unwrap();
                let ctx = egui::Context::default();
                ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                ctx.enable_accesskit();
                let output = draw_board_size(&mut app, &ctx, size, Vec::new());
                let target = *output.platform_output.accesskit_update.unwrap().nodes.iter().find(|(_, node)| match control {
                    "stop" => node.role() == Role::Button && node.label() == Some("Stop clips"),
                    "slider" => node.role() == Role::Slider && node.numeric_value() == Some(f64::from(0.61347f32)),
                    "number" => node.role() == Role::SpinButton && node.numeric_value() == Some(f64::from(0.61347f32)),
                    _ => node.role() == Role::CheckBox && node.label() == Some("Let clips overlap"),
                }).map(|(id, _)| id).unwrap();
                let mut events = vec![Vec::new()];
                let mut focused = false;
                for _ in 0..200 {
                    let tab = vec![egui::Event::Key {
                        key: egui::Key::Tab, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE,
                    }];
                    let output = draw_board_size(&mut app, &ctx, size, tab.clone());
                    events.push(tab);
                    if output.platform_output.accesskit_update.unwrap().focus == target { focused = true; break; }
                }
                assert!(focused);
                events.extend(vec![Vec::new(); 8]);
                let image = super::super::snapshot::snapshot_with_events(&mut app, &format!("board-playback-{control}-{theme}"),
                    size, 1.25, dark, events);
                println!("SNAPSHOT {}", image.display());
                assert_eq!(serde_json::to_value(&app.settings).unwrap(), original);
            }
        }
    }

    fn open_trim_menu(app: &mut App, ctx: &egui::Context, name: &str) {
        let pos = pad_icon_position(app, ctx, name);
        for pressed in [true, false] { draw_board(app, ctx, click(pos, egui::PointerButton::Secondary, pressed)); }
    }

    fn open_keyboard_trim_menu(app: &mut App, ctx: &egui::Context, name: &str) {
        use egui::accesskit::{Action, ActionRequest, TreeId};
        let play = format!("Play {name}");
        let pause = format!("Pause {name}");
        let output = draw_board(app, ctx, Vec::new());
        let pad = *output.platform_output.accesskit_update.as_ref().unwrap().nodes.iter()
            .find(|(_, node)| node.label() == Some(play.as_str()) || node.label() == Some(pause.as_str()))
            .map(|(id, _)| id).unwrap();
        draw_board(app, ctx, vec![egui::Event::AccessKitActionRequest(ActionRequest {
            action: Action::Focus, target_tree: TreeId::ROOT, target_node: pad, data: None,
        })]);
        draw_board(app, ctx, vec![egui::Event::Key {
            key: egui::Key::F10, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::SHIFT,
        }]);
        for _ in 0..6 { draw_board(app, ctx, Vec::new()); }
        assert!(egui::Popup::is_any_open(ctx));
    }

    #[test]
    fn volume_text_entry_uses_percentages_and_updates_playing_clips() {
        use egui::accesskit::{Action, ActionData, ActionRequest, Role, TreeId};
        let dir = tempfile::tempdir().unwrap();
        let path = wav(dir.path(), "volume.wav");
        for dark in [true, false] {
            for pad_volume in [true, false] {
                let mut app = app_with(std::slice::from_ref(&path));
                app.settings.sounds[0].volume = 0.43561;
                app.settings.sound_volume = 0.61347;
                let original_pad = app.settings.sounds[0].clone();
                app.cache_clip(path.clone(), CachedClip::read(&path, &AtomicBool::new(false)).unwrap());
                let ctx = egui::Context::default();
                ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                ctx.enable_accesskit();
                app.play_clip(0, &ctx);
                if pad_volume { open_trim_menu(&mut app, &ctx, "volume"); }
                let output = draw_board(&mut app, &ctx, Vec::new());
                let initial = if pad_volume { "44%" } else { "61%" };
                let field = *output.platform_output.accesskit_update.as_ref().unwrap().nodes.iter()
                    .find(|(_, node)| node.role() == Role::SpinButton && node.value() == Some(initial))
                    .map(|(id, _)| id).expect("the displayed percentage is editable");
                draw_board(&mut app, &ctx, vec![egui::Event::AccessKitActionRequest(ActionRequest {
                    action: Action::Focus, target_tree: TreeId::ROOT, target_node: field, data: None,
                })]);
                for _ in 0..2 { draw_board(&mut app, &ctx, Vec::new()); }
                draw_board(&mut app, &ctx, vec![egui::Event::Key {
                    key: egui::Key::Tab, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE,
                }]);
                for _ in 0..3 { draw_board(&mut app, &ctx, Vec::new()); }
                assert_eq!(app.settings.sounds[0], original_pad, "visiting the number field preserves saved pad precision");
                assert_eq!(app.settings.sound_volume, 0.61347, "visiting the number field preserves saved master precision");
                assert!(app.dirty_since.is_none(), "moving focus never saves a rounded display value");
                draw_board(&mut app, &ctx, vec![egui::Event::AccessKitActionRequest(ActionRequest {
                    action: Action::Focus, target_tree: TreeId::ROOT, target_node: field, data: None,
                })]);
                draw_board(&mut app, &ctx, vec![egui::Event::Key {
                    key: egui::Key::Tab, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::SHIFT,
                }]);
                draw_board(&mut app, &ctx, vec![egui::Event::Key {
                    key: egui::Key::ArrowLeft, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE,
                }]);
                assert!(if pad_volume { app.settings.sounds[0].volume < original_pad.volume } else { app.settings.sound_volume < 0.61347 },
                    "returning to the slider lets Left reduce volume without committing rounded text");
                for (input, paste, expected) in [("50", false, 0.5), (" 37.5% ", true, 0.375), ("0%", false, 0.0), ("150", false, 1.0), ("-20%", false, 0.0)] {
                    draw_board(&mut app, &ctx, vec![egui::Event::AccessKitActionRequest(ActionRequest {
                        action: Action::Focus, target_tree: TreeId::ROOT, target_node: field, data: None,
                    })]);
                    draw_board(&mut app, &ctx, vec![egui::Event::Key {
                        key: egui::Key::A, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::COMMAND,
                    }]);
                    let mut events = vec![if paste { egui::Event::Paste(input.into()) } else { egui::Event::Text(input.into()) }];
                    if input == "50" || paste {
                        events.push(egui::Event::Key {
                            key: egui::Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE,
                        });
                    }
                    draw_board(&mut app, &ctx, events);
                    draw_board(&mut app, &ctx, Vec::new());
                    let actual = if pad_volume { app.settings.sounds[0].volume } else { app.settings.sound_volume };
                    assert_eq!(actual, expected, "{input:?} means a percentage in the {} control", if pad_volume { "pad" } else { "master" });
                    assert_eq!(app.params().sound_gain, if pad_volume { 0.61347 } else { expected });
                    let clips = app.engine.as_ref().unwrap().take_clips();
                    assert_eq!(clips.len(), 1, "editing volume keeps the loaded clip");
                    assert_eq!(clips[0].gain, if pad_volume { expected } else { 0.43561 }, "pad volume updates the playing clip's gain");
                    app.engine.as_ref().unwrap().resume_clips(clips);
                    assert!(app.dirty_since.is_some() && app.clip_loads.is_none());
                }
                for invalid in ["NaN", "inf", "%", "50%%", "not a volume"] {
                    app.dirty_since = None;
                    draw_board(&mut app, &ctx, vec![egui::Event::Key {
                        key: egui::Key::A, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::COMMAND,
                    }]);
                    draw_board(&mut app, &ctx, vec![egui::Event::Paste(invalid.into())]);
                    assert_eq!(app.settings.sounds[0].volume, if pad_volume { 0.0 } else { original_pad.volume });
                    assert_eq!(app.settings.sound_volume, if pad_volume { 0.61347 } else { 0.0 });
                    assert!(app.dirty_since.is_none(), "invalid percentage text never changes or saves audio settings");
                }
                draw_board(&mut app, &ctx, vec![egui::Event::Key {
                    key: egui::Key::Tab, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE,
                }]);
                draw_board(&mut app, &ctx, vec![egui::Event::AccessKitActionRequest(ActionRequest {
                    action: Action::SetValue, target_tree: TreeId::ROOT, target_node: field,
                    data: Some(ActionData::NumericValue(0.25)),
                })]);
                assert_eq!(if pad_volume { app.settings.sounds[0].volume } else { app.settings.sound_volume }, 0.25,
                    "assistive edits still use normalized values after focus moves away");
                draw_board(&mut app, &ctx, vec![egui::Event::AccessKitActionRequest(ActionRequest {
                    action: Action::Focus, target_tree: TreeId::ROOT, target_node: field, data: None,
                })]);
                draw_board(&mut app, &ctx, vec![egui::Event::Key {
                    key: egui::Key::ArrowDown, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE,
                }]);
                assert!(if pad_volume { app.settings.sounds[0].volume < 0.25 } else { app.settings.sound_volume < 0.25 },
                    "arrow keys still adjust volume immediately after the numeric field regains focus");
            }
        }
    }

    #[test]
    fn keyboard_pad_menu_edits_the_focused_file_and_restores_its_control() {
        let dir = tempfile::tempdir().unwrap();
        let paths: Vec<_> = (0..7).map(|i| wav(dir.path(), &format!("menu-clip-{i}.wav"))).collect();
        let key = |key, modifiers| vec![egui::Event::Key {
            key, physical_key: None, pressed: true, repeat: false, modifiers,
        }];
        let size = egui::vec2(700.0, 420.0);
        for dark in [true, false] {
            for pointer in [None, Some(egui::pos2(690.0, 410.0))] {
                for label in ["Play target", "Rename target", "Add target to the strip", "Remove target"] {
                    let mut app = app_with(&paths);
                    app.settings.sounds[1].name = Some("target".into());
                    app.settings.sounds[1].volume = 0.5;
                    app.settings.sounds[1].start = 0.1;
                    app.settings.sounds[1].end = Some(0.8);
                    app.cache_clip(paths[1].clone(), CachedClip::read(&paths[1], &AtomicBool::new(false)).unwrap());
                    let ctx = egui::Context::default();
                    ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                    ctx.enable_accesskit();
                    let output = draw_board_size(&mut app, &ctx, size, Vec::new());
                    let target = *output.platform_output.accesskit_update.unwrap().nodes.iter()
                        .find(|(_, node)| node.label() == Some(label)).map(|(id, _)| id).unwrap();
                    let mut focused = false;
                    for _ in 0..80 {
                        let output = draw_board_size(&mut app, &ctx, size, key(egui::Key::Tab, egui::Modifiers::NONE));
                        if output.platform_output.accesskit_update.unwrap().focus == target { focused = true; break; }
                    }
                    assert!(focused, "Tab reaches {label}");
                    app.play_clip(1, &ctx);
                    app.toggle_clip(1, &ctx);
                    let moved = app.settings.sounds.remove(1);
                    app.settings.sounds.push(moved);
                    app.remove_clip(0);
                    app.settings.sounds[0].name = Some("target".into());
                    let original = app.settings.sounds.clone();
                    let progress = app.engine.as_ref().unwrap().clip_progress();
                    let status = app.sound_status.clone();
                    let (_done, cancel) = delayed(&mut app, &paths[2..3]);
                    if let Some(pos) = pointer { draw_board_size(&mut app, &ctx, size, vec![egui::Event::PointerMoved(pos)]); }
                    for _ in 0..6 { draw_board_size(&mut app, &ctx, size, Vec::new()); }
                    draw_board_size(&mut app, &ctx, size, key(egui::Key::F10, egui::Modifiers::SHIFT));
                    for _ in 0..6 { draw_board_size(&mut app, &ctx, size, Vec::new()); }
                    assert!(egui::Popup::is_any_open(&ctx), "Shift+F10 opens {label}'s menu without relying on the pointer");
                    let output = draw_board_size(&mut app, &ctx, size, Vec::new());
                    let tree = output.platform_output.accesskit_update.as_ref().unwrap();
                    let focused = &tree.nodes.iter().find(|(id, _)| *id == tree.focus).unwrap().1;
                    assert_eq!(focused.role(), egui::accesskit::Role::Slider, "opening the menu focuses its volume slider");
                    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, size);
                    for label in ["Volume", "Hotkey", "Trim", "Remove from soundboard"] {
                        assert!(screen.contains_rect(text_rect(&output, label)), "{label} is visible at the minimum window size");
                    }
                    assert_eq!(app.trim_clip.as_deref(), Some(paths[1].as_path()));
                    assert_eq!(app.settings.sounds, original, "opening the menu never changes saved values");
                    assert_eq!(app.engine.as_ref().unwrap().clip_progress(), progress);
                    assert_eq!(app.sound_status, status);
                    assert!(!cancel.load(Ordering::Relaxed));
                    assert!(app.clip_loading(&paths[2]));
                    draw_board_size(&mut app, &ctx, size, key(egui::Key::ArrowRight, egui::Modifiers::NONE));
                    assert!(app.settings.sounds[5].volume > 0.5, "the volume slider accepts arrow keys");
                    assert_eq!(app.settings.sounds[..5], original[..5], "duplicate names do not redirect the edit");
                    let mut reached_star = false;
                    for _ in 0..12 {
                        let output = draw_board_size(&mut app, &ctx, size, key(egui::Key::Tab, egui::Modifiers::NONE));
                        let tree = output.platform_output.accesskit_update.unwrap();
                        if tree.nodes.iter().any(|(id, node)| *id == tree.focus && node.label() == Some("Show on the strip")) {
                            reached_star = true;
                            break;
                        }
                    }
                    assert!(reached_star, "Tab reaches the menu controls");
                    draw_board_size(&mut app, &ctx, size, key(egui::Key::Space, egui::Modifiers::NONE));
                    assert!(app.settings.sounds[5].starred && app.settings.sounds[..5].iter().all(|pad| !pad.starred));
                    draw_board_size(&mut app, &ctx, size, key(egui::Key::Escape, egui::Modifiers::NONE));
                    for _ in 0..6 { draw_board_size(&mut app, &ctx, size, Vec::new()); }
                    let output = draw_board_size(&mut app, &ctx, size, Vec::new());
                    assert!(!egui::Popup::is_any_open(&ctx));
                    assert_eq!(output.platform_output.accesskit_update.unwrap().focus, target, "Escape restores the same file and control");
                    assert!(app.trim_clip.is_none());
                    assert_eq!(app.engine.as_ref().unwrap().clip_progress(), progress);
                    assert!(!cancel.load(Ordering::Relaxed) && app.clip_loading(&paths[2]));
                }
            }
        }
    }

    #[test]
    fn batched_tab_menu_opens_the_new_pad_and_restores_its_control() {
        use egui::accesskit::{Action, ActionRequest, Role, TreeId};
        let dir = tempfile::tempdir().unwrap();
        let paths = [wav(dir.path(), "first.wav"), wav(dir.path(), "second.wav")];
        let key = |key, modifiers| egui::Event::Key {
            key, physical_key: None, pressed: true, repeat: false, modifiers,
        };
        for dark in [true, false] {
            for passes in [1, 2] {
                for backwards in [false, true] {
                    let mut app = app_with(&paths);
                    for (index, path) in paths.iter().enumerate() {
                        app.settings.sounds[index].name = Some(if index == 0 { "first" } else { "second" }.into());
                        app.cache_clip(path.clone(), CachedClip::read(path, &AtomicBool::new(false)).unwrap());
                    }
                    let original = app.settings.sounds.clone();
                    let ctx = egui::Context::default();
                    ctx.options_mut(|options| options.max_passes = passes.try_into().unwrap());
                    ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                    ctx.enable_accesskit();
                    for _ in 0..3 { draw_board(&mut app, &ctx, Vec::new()); }
                    let output = draw_board(&mut app, &ctx, Vec::new());
                    let tree = output.platform_output.accesskit_update.as_ref().unwrap();
                    let [first, second] = ["Remove first", "Play second"].map(|label| *tree.nodes.iter()
                        .find(|(_, node)| node.label() == Some(label)).map(|(id, _)| id).unwrap());
                    let (old, new, index) = if backwards { (second, first, 0) } else { (first, second, 1) };
                    draw_board(&mut app, &ctx, vec![egui::Event::AccessKitActionRequest(ActionRequest {
                        action: Action::Focus, target_tree: TreeId::ROOT, target_node: old, data: None,
                    })]);
                    for _ in 0..3 { draw_board(&mut app, &ctx, Vec::new()); }
                    draw_board(&mut app, &ctx, vec![
                        key(egui::Key::Tab, if backwards { egui::Modifiers::SHIFT } else { egui::Modifiers::NONE }),
                        key(egui::Key::F10, egui::Modifiers::SHIFT),
                    ]);
                    for _ in 0..8 { draw_board(&mut app, &ctx, Vec::new()); }
                    assert!(egui::Popup::is_any_open(&ctx), "Shift+F10 after Tab opens a menu");
                    assert_eq!(app.trim_clip.as_deref(), Some(paths[index].as_path()),
                        "the menu belongs to the newly focused pad; backwards={backwards}, passes={passes}");
                    let output = draw_board(&mut app, &ctx, Vec::new());
                    let tree = output.platform_output.accesskit_update.as_ref().unwrap();
                    assert!(tree.nodes.iter().any(|(id, node)| *id == tree.focus && node.role() == Role::Slider));
                    assert_eq!(app.settings.sounds, original);
                    assert!(app.dirty_since.is_none());
                    assert!(app.playing.is_empty() && app.clip_loads.is_none() && app.renaming_pad.is_none());
                    draw_board(&mut app, &ctx, vec![key(egui::Key::Escape, egui::Modifiers::NONE)]);
                    for _ in 0..6 { draw_board(&mut app, &ctx, Vec::new()); }
                    let output = draw_board(&mut app, &ctx, Vec::new());
                    assert!(!egui::Popup::is_any_open(&ctx) && app.trim_clip.is_none());
                    assert_eq!(output.platform_output.accesskit_update.as_ref().unwrap().focus, new,
                        "Escape returns to the control reached by Tab");
                }
            }
        }
    }

    #[test]
    fn rename_keeps_input_batched_with_activation() {
        use egui::accesskit::{Action, ActionRequest, TreeId};
        let dir = tempfile::tempdir().unwrap();
        let paths = [wav(dir.path(), "keep.wav"), wav(dir.path(), "rename.wav")];
        for dark in [true, false] {
            for passes in [1, 2] {
                for activation in ["Space", "Enter", "assistive click", "pointer"] {
                    for (finish, typed, cancel_pending) in [
                        (egui::Key::Enter, false, false), (egui::Key::Enter, true, false),
                        (egui::Key::Tab, false, false), (egui::Key::Tab, true, false),
                        (egui::Key::Escape, false, false), (egui::Key::Escape, true, false),
                        (egui::Key::Enter, true, true),
                    ] {
                        if cancel_pending && passes != 1 { continue; }
                        let mut app = app_with(&paths);
                        app.settings.sounds[0].name = Some("Original".into());
                        app.settings.sounds[1].name = Some("target".into());
                        app.settings.sounds[1].starred = true;
                        app.cache_clip(paths[1].clone(), CachedClip::read(&paths[1], &AtomicBool::new(false)).unwrap());
                        let ctx = egui::Context::default();
                        ctx.options_mut(|options| options.max_passes = passes.try_into().unwrap());
                        ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                        ctx.enable_accesskit();
                        for _ in 0..3 { draw_board(&mut app, &ctx, Vec::new()); }
                        let output = draw_board(&mut app, &ctx, Vec::new());
                        let (target, node) = output.platform_output.accesskit_update.as_ref().unwrap().nodes.iter()
                            .find(|(_, node)| node.label() == Some("Rename target")).unwrap();
                        let target = *target;
                        let bounds = node.bounds().unwrap();
                        let pos = egui::pos2((bounds.x0 + bounds.x1) as f32 * 0.5, (bounds.y0 + bounds.y1) as f32 * 0.5);
                        app.settings.sounds[1].name = Some("Original".into());
                        app.play_clip(1, &ctx);
                        app.toggle_clip(1, &ctx);
                        let original = app.settings.sounds.clone();
                        let progress = app.engine.as_ref().unwrap().clip_progress();
                        let status = app.sound_status.clone();
                        app.dirty_since = None;
                        draw_board(&mut app, &ctx, vec![egui::Event::AccessKitActionRequest(ActionRequest {
                            action: Action::Focus, target_tree: TreeId::ROOT, target_node: target, data: None,
                        })]);
                        for _ in 0..3 { draw_board(&mut app, &ctx, Vec::new()); }
                        let key = |key, pressed| egui::Event::Key {
                            key, physical_key: None, pressed, repeat: false, modifiers: egui::Modifiers::NONE,
                        };
                        let mut events = match activation {
                            "Space" => vec![key(egui::Key::Space, true), egui::Event::Text(" ".into()), key(egui::Key::Space, false)],
                            "Enter" => vec![key(egui::Key::Enter, true), key(egui::Key::Enter, false)],
                            "assistive click" => vec![egui::Event::AccessKitActionRequest(ActionRequest {
                                action: Action::Click, target_tree: TreeId::ROOT, target_node: target, data: None,
                            })],
                            "pointer" => {
                                draw_board(&mut app, &ctx, click(pos, egui::PointerButton::Primary, true));
                                click(pos, egui::PointerButton::Primary, false)
                            }
                            _ => unreachable!(),
                        };
                        if typed { events.push(egui::Event::Text("Chime".into())); }
                        events.extend([key(finish, true), key(finish, false)]);
                        draw_board(&mut app, &ctx, events);
                        if cancel_pending {
                            assert!(app.renaming_pad.is_some());
                            assert_eq!(app.settings.sounds, original, "the unseen typing has not saved yet");
                            draw_board(&mut app, &ctx, vec![key(egui::Key::Escape, true), key(egui::Key::Escape, false)]);
                        }
                        for _ in 0..8 { draw_board(&mut app, &ctx, Vec::new()); }
                        let mut expected = original;
                        let saved = typed && finish != egui::Key::Escape && !cancel_pending;
                        if saved { expected[1].name = Some("Chime".into()); }
                        assert_eq!(app.settings.sounds, expected,
                            "rename keeps input following {activation}, finished by {finish:?}; passes={passes}, typed={typed}, cancel_pending={cancel_pending}");
                        assert!(app.renaming_pad.is_none(), "the batched completion closes the editor");
                        assert_eq!(app.engine.as_ref().unwrap().clip_progress(), progress);
                        assert_eq!(app.sound_status, status);
                        assert_eq!(app.dirty_since.is_some(), saved);
                        assert!(paths.iter().all(|path| path.is_file()));
                    }
                }
            }
        }
    }

    #[test]
    fn rename_focus_stays_visible_when_search_and_order_change() {
        use egui::accesskit::{Action, ActionRequest, TreeId};
        let dir = tempfile::tempdir().unwrap();
        let paths: Vec<_> = (0..33).map(|index| wav(dir.path(), &format!("rename-{index}.wav"))).collect();
        let size = egui::vec2(700.0, 420.0);
        let visible = |output: &egui::FullOutput| output.shapes.iter().any(|shape| matches!(&shape.shape,
            egui::Shape::Text(text) if text.galley.text() == "Draft" && shape.clip_rect.contains_rect(text.visual_bounding_rect())));
        for dark in [true, false] {
            for passes in [1, 2] {
                for clear_search in [false, true] {
                    let index = if clear_search { 32 } else { 0 };
                    let path = &paths[index];
                    let mut app = app_with(&paths);
                    app.settings.sounds[index].name = Some("target".into());
                    if clear_search { app.pad_filter = "target".into(); }
                    app.cache_clip(path.clone(), CachedClip::read(path, &AtomicBool::new(false)).unwrap());
                    let ctx = egui::Context::default();
                    ctx.options_mut(|options| options.max_passes = passes.try_into().unwrap());
                    ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                    ctx.enable_accesskit();
                    for _ in 0..3 { draw_board_size(&mut app, &ctx, size, Vec::new()); }
                    let output = draw_board_size(&mut app, &ctx, size, Vec::new());
                    let target = *output.platform_output.accesskit_update.as_ref().unwrap().nodes.iter()
                        .find(|(_, node)| node.label() == Some("Rename target")).map(|(id, _)| id).unwrap();
                    app.play_clip(index, &ctx);
                    app.toggle_clip(index, &ctx);
                    let progress = app.engine.as_ref().unwrap().clip_progress();
                    let status = app.sound_status.clone();
                    draw_board_size(&mut app, &ctx, size, [Action::Focus, Action::Click].map(|action|
                        egui::Event::AccessKitActionRequest(ActionRequest {
                            action, target_tree: TreeId::ROOT, target_node: target, data: None,
                        })).into());
                    for _ in 0..4 { draw_board_size(&mut app, &ctx, size, Vec::new()); }
                    draw_board_size(&mut app, &ctx, size, vec![egui::Event::Text("Draft".into())]);
                    for _ in 0..6 { draw_board_size(&mut app, &ctx, size, Vec::new()); }
                    let output = draw_board_size(&mut app, &ctx, size, Vec::new());
                    let editor = output.platform_output.accesskit_update.as_ref().unwrap().focus;
                    assert!(visible(&output), "the editor starts visible");
                    if clear_search {
                        app.pad_filter.clear();
                    } else {
                        let pad = app.settings.sounds.remove(index);
                        app.settings.sounds.push(pad);
                    }
                    let original = app.settings.sounds.clone();
                    for _ in 0..8 { draw_board_size(&mut app, &ctx, size, Vec::new()); }
                    let output = draw_board_size(&mut app, &ctx, size, Vec::new());
                    assert_eq!(output.platform_output.accesskit_update.as_ref().unwrap().focus, editor);
                    assert!(visible(&output), "the focused editor stays visible; clear_search={clear_search}, passes={passes}");
                    assert_eq!(app.settings.sounds, original, "moving the editor never commits its draft");
                    assert!(app.renaming_pad.is_some());
                    let bounds = output.platform_output.accesskit_update.as_ref().unwrap().nodes.iter()
                        .find(|(id, _)| *id == editor).unwrap().1.bounds().unwrap();
                    let pos = egui::pos2((bounds.x0 + bounds.x1) as f32 * 0.5, (bounds.y0 + bounds.y1) as f32 * 0.5);
                    for delta in [1000.0, -1000.0] {
                        draw_board_size(&mut app, &ctx, size, vec![
                            egui::Event::PointerMoved(pos),
                            egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: egui::vec2(0.0, delta),
                                modifiers: egui::Modifiers::NONE, phase: egui::TouchPhase::Move },
                        ]);
                        for _ in 0..6 { draw_board_size(&mut app, &ctx, size, Vec::new()); }
                        let output = draw_board_size(&mut app, &ctx, size, Vec::new());
                        assert_eq!(visible(&output), delta < 0.0, "wheel scrolling can move away from the focused editor and back");
                        assert_eq!(output.platform_output.accesskit_update.as_ref().unwrap().focus, editor);
                    }
                    draw_board_size(&mut app, &ctx, size, vec![
                        egui::Event::Key { key: egui::Key::A, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::COMMAND },
                        egui::Event::Text("Final".into()),
                        egui::Event::Key { key: egui::Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE },
                    ]);
                    for _ in 0..4 { draw_board_size(&mut app, &ctx, size, Vec::new()); }
                    assert_eq!(app.settings.sounds.iter().find(|pad| &pad.path == path).unwrap().name.as_deref(), Some("Final"),
                        "typing continues on the same file; clear_search={clear_search}, passes={passes}");
                    let mut expected = original;
                    expected.iter_mut().find(|pad| &pad.path == path).unwrap().name = Some("Final".into());
                    assert_eq!(app.settings.sounds, expected, "typing continues on the same file");
                    assert!(app.renaming_pad.is_none());
                    assert_eq!(app.engine.as_ref().unwrap().clip_progress(), progress);
                    assert_eq!(app.sound_status, status);
                }
            }
        }
    }

    #[test]
    fn keyboard_menu_removal_does_not_delete_a_neighbor_with_the_same_name() {
        let dir = tempfile::tempdir().unwrap();
        let paths = [wav(dir.path(), "keep.wav"), wav(dir.path(), "remove.wav")];
        let key = |key, modifiers| vec![egui::Event::Key {
            key, physical_key: None, pressed: true, repeat: false, modifiers,
        }];
        let size = egui::vec2(700.0, 420.0);
        for dark in [true, false] {
            let mut app = app_with(&paths);
            app.settings.sounds[1].name = Some("target".into());
            app.cache_clip(paths[1].clone(), CachedClip::read(&paths[1], &AtomicBool::new(false)).unwrap());
            let ctx = egui::Context::default();
            ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
            ctx.enable_accesskit();
            let output = draw_board_size(&mut app, &ctx, size, Vec::new());
            let target = *output.platform_output.accesskit_update.unwrap().nodes.iter()
                .find(|(_, node)| node.label() == Some("Play target")).map(|(id, _)| id).unwrap();
            let mut focused = false;
            for _ in 0..40 {
                let output = draw_board_size(&mut app, &ctx, size, key(egui::Key::Tab, egui::Modifiers::NONE));
                if output.platform_output.accesskit_update.unwrap().focus == target { focused = true; break; }
            }
            assert!(focused);
            for pad in &mut app.settings.sounds { pad.name = Some("same".into()); }
            for _ in 0..6 { draw_board_size(&mut app, &ctx, size, Vec::new()); }
            draw_board_size(&mut app, &ctx, size, key(egui::Key::F10, egui::Modifiers::SHIFT));
            for _ in 0..6 { draw_board_size(&mut app, &ctx, size, Vec::new()); }
            let mut reached_remove = false;
            for _ in 0..20 {
                let output = draw_board_size(&mut app, &ctx, size, key(egui::Key::Tab, egui::Modifiers::NONE));
                let tree = output.platform_output.accesskit_update.unwrap();
                if tree.nodes.iter().any(|(id, node)| *id == tree.focus && node.label() == Some("Remove from soundboard")) {
                    reached_remove = true;
                    break;
                }
            }
            assert!(reached_remove, "Tab reaches Remove within the open menu");
            draw_board_size(&mut app, &ctx, size, key(egui::Key::Enter, egui::Modifiers::NONE));
            assert_eq!(app.settings.sounds.iter().map(|pad| &pad.path).collect::<Vec<_>>(), [&paths[0]]);
            assert!(!egui::Popup::is_any_open(&ctx));
            draw_board_size(&mut app, &ctx, size, key(egui::Key::Enter, egui::Modifiers::NONE));
            assert_eq!(app.settings.sounds.len(), 1, "the removed menu does not give its neighbor destructive focus");
            assert!(paths.iter().all(|path| path.is_file()));
            assert!(app.playing.is_empty() && app.clip_loads.is_none() && app.renaming_pad.is_none());
        }
    }

    #[test]
    fn focused_menu_actions_survive_waveform_results_and_shortcut_changes() {
        let dir = tempfile::tempdir().unwrap();
        let paths = [wav(dir.path(), "keep.wav"), wav(dir.path(), "target.wav")];
        let key = |key| vec![egui::Event::Key {
            key, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE,
        }];
        for dark in [true, false] {
            for change in ["waveform ready", "waveform failed", "shortcut cleared"] {
                let mut app = app_with(&paths);
                let ctx = egui::Context::default();
                ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                ctx.enable_accesskit();
                let pending = if change == "shortcut cleared" {
                    app.settings.sounds[1].hotkey = Some("Ctrl+Alt+Shift+PrintScreen".into());
                    app.cache_clip(paths[1].clone(), CachedClip::read(&paths[1], &AtomicBool::new(false)).unwrap());
                    None
                } else {
                    Some(delayed(&mut app, &paths[1..2]))
                };
                open_trim_menu(&mut app, &ctx, "target");
                if pending.is_some() {
                    app.clip_loads.as_mut().unwrap().requests.front_mut().unwrap().play = false;
                }
                for _ in 0..6 { draw_board(&mut app, &ctx, Vec::new()); }
                let label = if pending.is_some() { "Remove from soundboard" } else { "Show on the strip" };
                let mut target = None;
                for _ in 0..30 {
                    let output = draw_board(&mut app, &ctx, key(egui::Key::Tab));
                    let tree = output.platform_output.accesskit_update.unwrap();
                    if tree.nodes.iter().any(|(id, node)| *id == tree.focus && node.label() == Some(label)) {
                        target = Some(tree.focus);
                        break;
                    }
                }
                let target = target.expect("Tab reaches the menu action before its content changes");
                if let Some((done, _)) = pending {
                    let result = if change == "waveform ready" {
                        CachedClip::read(&paths[1], &AtomicBool::new(false))
                    } else {
                        Err(anyhow::anyhow!("Waveform failed"))
                    };
                    done.send(result).unwrap();
                    app.poll_clip_loads(&ctx);
                } else {
                    app.settings.sounds[1].hotkey = None;
                }
                let output = draw_board(&mut app, &ctx, Vec::new());
                assert_eq!(output.platform_output.accesskit_update.unwrap().focus, target,
                    "{label} keeps focus after {change}; dark={dark}");
                draw_board(&mut app, &ctx, key(egui::Key::Enter));
                if change == "shortcut cleared" {
                    assert!(!app.settings.sounds[0].starred && app.settings.sounds[1].starred);
                } else {
                    assert_eq!(app.settings.sounds.iter().map(|pad| &pad.path).collect::<Vec<_>>(), [&paths[0]]);
                }
                assert!(app.playing.is_empty() && app.clip_loads.is_none());
                assert!(paths.iter().all(|path| path.is_file()));
            }
        }
    }

    #[test]
    fn long_pad_menu_content_keeps_its_controls_inside_the_minimum_window() {
        let dir = tempfile::tempdir().unwrap();
        let path = wav(dir.path(), "long-menu.wav");
        let name = format!("{}final cue", "A very long recording title with several words — ".repeat(32));
        let shortcut = "Ctrl+Alt+Shift+PrintScreen";
        let size = egui::vec2(700.0, 420.0);
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, size);
        for dark in [true, false] {
            for keyboard in [false, true] {
                let mut app = app_with(std::slice::from_ref(&path));
                app.settings.sounds[0].name = Some(name.clone());
                app.settings.sounds[0].hotkey = Some(shortcut.into());
                app.settings.sounds[0].start = 0.12345;
                app.settings.sounds[0].end = Some(0.812345);
                app.cache_clip(path.clone(), CachedClip::read(&path, &AtomicBool::new(false)).unwrap());
                let original = app.settings.sounds.clone();
                let ctx = egui::Context::default();
                ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                ctx.enable_accesskit();
                let output = draw_board_size(&mut app, &ctx, size, Vec::new());
                let label = format!("Play {name}");
                let (target, bounds) = output.platform_output.accesskit_update.as_ref().unwrap().nodes.iter()
                    .find(|(_, node)| node.label() == Some(label.as_str())).map(|(id, node)| (*id, node.bounds().unwrap())).unwrap();
                if keyboard {
                    let mut focused = false;
                    for _ in 0..30 {
                        let output = draw_board_size(&mut app, &ctx, size, vec![egui::Event::Key {
                            key: egui::Key::Tab, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE,
                        }]);
                        if output.platform_output.accesskit_update.unwrap().focus == target { focused = true; break; }
                    }
                    assert!(focused);
                    draw_board_size(&mut app, &ctx, size, vec![egui::Event::Key {
                        key: egui::Key::F10, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::SHIFT,
                    }]);
                } else {
                    let pos = egui::pos2(bounds.x0 as f32 + 20.0, (bounds.y0 + bounds.y1) as f32 * 0.5);
                    for pressed in [true, false] { draw_board_size(&mut app, &ctx, size, click(pos, egui::PointerButton::Secondary, pressed)); }
                }
                for _ in 0..6 { draw_board_size(&mut app, &ctx, size, Vec::new()); }
                let output = draw_board_size(&mut app, &ctx, size, Vec::new());
                assert!(egui::Popup::is_any_open(&ctx));
                let tree = output.platform_output.accesskit_update.as_ref().unwrap();
                for label in [name.as_str(), shortcut] {
                    let bounds = tree.nodes.iter().find(|(_, node)| node.value() == Some(label)).unwrap().1.bounds().unwrap();
                    let rect = egui::Rect::from_min_max(
                        egui::pos2(bounds.x0 as f32, bounds.y0 as f32), egui::pos2(bounds.x1 as f32, bounds.y1 as f32),
                    );
                    assert!(screen.contains_rect(rect), "{label} stays inside the window; keyboard={keyboard}, dark={dark}");
                }
                for label in ["Volume", "Hotkey", "Clear", "Trim", "Start", "End", "Reset", "Remove from soundboard"] {
                    let rect = text_rect(&output, label);
                    assert!(screen.contains_rect(rect), "{label} is visible; keyboard={keyboard}, dark={dark}");
                    assert!(output.shapes.iter().any(|shape| matches!(&shape.shape,
                        egui::Shape::Text(text) if text.galley.text() == label && text.galley.rows.len() == 1
                            && shape.clip_rect.contains_rect(rect))), "{label} is readable on one line");
                }
                let bounds = tree.nodes.iter().find(|(_, node)| node.value() == Some(name.as_str())).unwrap().1.bounds().unwrap();
                let pos = egui::pos2((bounds.x0 + bounds.x1) as f32 * 0.5, (bounds.y0 + bounds.y1) as f32 * 0.5);
                draw_board_size(&mut app, &ctx, size, vec![egui::Event::PointerMoved(pos)]);
                let mut full_name = false;
                for _ in 0..6 {
                    let output = draw_board_size(&mut app, &ctx, size, Vec::new());
                    full_name |= output.shapes.iter().any(|shape| matches!(&shape.shape,
                        egui::Shape::Text(text) if text.galley.text() == name && !text.galley.elided
                            && shape.clip_rect.contains_rect(text.visual_bounding_rect())));
                }
                assert!(full_name, "hovering the heading exposes the full name");
                assert_eq!(app.settings.sounds, original, "drawing the menu preserves full names, shortcuts and exact trim points");
                assert_eq!(app.trim_clip.as_deref(), Some(path.as_path()));
                assert!(app.playing.is_empty() && app.clip_loads.is_none());
            }
        }
    }

    #[test]
    #[ignore = "writes keyboard menu screenshots; needs Python with Pillow"]
    fn snapshot_soundboard_keyboard_menu() {
        let dir = tempfile::tempdir().unwrap();
        let path = wav(dir.path(), "target.wav");
        let size = egui::vec2(700.0, 420.0);
        for (dark, theme) in [(true, "dark"), (false, "light")] {
            for (caption, name, shortcut) in [
                ("menu", "target", None),
                ("long-menu", "Quarterly meeting — opening music and final countdown for stage announcements", Some("Ctrl+Alt+Shift+PrintScreen")),
            ] {
                let paths: Vec<_> = (0..32).map(|i| PathBuf::from(format!("clip-{i}.wav"))).chain([path.clone()]).collect();
                let mut app = app_with(&paths);
                app.settings.sounds[32].name = Some(name.into());
                app.settings.sounds[32].hotkey = shortcut.map(str::to_owned);
                app.settings.sounds[32].volume = 0.5;
                app.settings.sounds[32].start = 0.15;
                app.settings.sounds[32].end = Some(0.75);
                app.cache_clip(path.clone(), CachedClip::read(&path, &AtomicBool::new(false)).unwrap());
                let original = app.settings.sounds.clone();
                let ctx = egui::Context::default();
                ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                ctx.enable_accesskit();
                let output = draw_board_size(&mut app, &ctx, size, Vec::new());
                let label = format!("Play {name}");
                let target = *output.platform_output.accesskit_update.unwrap().nodes.iter()
                    .find(|(_, node)| node.label() == Some(label.as_str())).map(|(id, _)| id).unwrap();
                let mut events = vec![Vec::new()];
                let mut focused = false;
                for _ in 0..180 {
                    let tab = vec![egui::Event::Key {
                        key: egui::Key::Tab, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE,
                    }];
                    let output = draw_board_size(&mut app, &ctx, size, tab.clone());
                    events.push(tab);
                    if output.platform_output.accesskit_update.unwrap().focus == target { focused = true; break; }
                }
                assert!(focused);
                events.extend(std::iter::repeat_n(Vec::new(), 6));
                events.push(vec![egui::Event::Key {
                    key: egui::Key::F10, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::SHIFT,
                }]);
                let image = super::super::snapshot::snapshot_with_events(
                    &mut app, &format!("board-keyboard-{caption}-{theme}"), size, 1.25, dark, events,
                );
                println!("SNAPSHOT {}", image.display());
                assert_eq!(app.settings.sounds, original);
                assert_eq!(app.trim_clip.as_deref(), Some(path.as_path()));
                assert!(app.playing.is_empty() && app.clip_loads.is_none());
            }
        }
    }

    fn trim_number_position(output: &egui::FullOutput, label: &str) -> egui::Pos2 {
        let label_rect = text_rect(output, label);
        output.shapes.iter().find_map(|shape| match &shape.shape {
            egui::Shape::Text(text) if text.galley.text().parse::<f32>().is_ok()
                && (text.visual_bounding_rect().center().y - label_rect.center().y).abs() < 6.0
                && text.visual_bounding_rect().left() > label_rect.right() => Some(text.visual_bounding_rect().center()),
            _ => None,
        }).expect("the trim point has a number field")
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
    fn opening_and_closing_trim_menus_preserves_saved_points_and_does_not_save() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trim.wav");
        for (count, start, end) in [
            (96_000, 2.0, None),
            (96_000, 2.0, Some(3.0)),
            (96_000, 0.0, Some(2.0)),
            (96_000, 0.2, Some(0.21)),
            (96_000, 0.003, Some(0.998)),
            (1_920, 0.003, Some(0.019)),
        ] {
            crate::record::Recorder::from_samples_for_test(&vec![0.25; count]).finish().unwrap().unwrap().save_wav(&path).unwrap();
            let mut app = app_with(std::slice::from_ref(&path));
            app.settings.sounds[0].start = start;
            app.settings.sounds[0].end = end;
            let pad = app.settings.sounds[0].clone();
            app.cache_clip(path.clone(), CachedClip::read(&path, &AtomicBool::new(false)).unwrap());
            let ctx = egui::Context::default();
            open_trim_menu(&mut app, &ctx, "trim");
            for _ in 0..3 { draw_board(&mut app, &ctx, Vec::new()); }
            assert!(app.trim_clip.is_some());
            assert_eq!(app.settings.sounds[0], pad, "opening the menu preserves saved points even outside the current file");
            assert!(app.dirty_since.is_none(), "looking at a trim never schedules a settings save");
            for label in ["Start", "End"] {
                let output = draw_board(&mut app, &ctx, Vec::new());
                let pos = trim_number_position(&output, label);
                for pressed in [true, false] { draw_board(&mut app, &ctx, click(pos, egui::PointerButton::Primary, pressed)); }
                draw_board(&mut app, &ctx, Vec::new());
                let output = draw_board(&mut app, &ctx, Vec::new());
                let pos = text_rect(&output, "Trim").center();
                for pressed in [true, false] { draw_board(&mut app, &ctx, click(pos, egui::PointerButton::Primary, pressed)); }
                draw_board(&mut app, &ctx, Vec::new());
                assert_eq!(app.settings.sounds[0], pad, "focusing a field and leaving it is not an edit");
                assert!(app.dirty_since.is_none());
            }
            draw_board(&mut app, &ctx, vec![egui::Event::Key {
                key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE,
            }]);
            draw_board(&mut app, &ctx, Vec::new());
            assert!(app.trim_clip.is_none() && app.clip_loads.is_none());
            assert_eq!(app.settings.sounds[0], pad);
            assert!(app.dirty_since.is_none() && app.playing.is_empty());
        }
    }

    #[test]
    fn waveform_clicks_preserve_points_and_drags_keep_the_handle_chosen_at_press() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trim.wav");
        for (count, start, end, moves) in [
            (96_000, 0.1, Some(0.9), vec![(0.1, 0.7, 0.6, 0.6, Some(0.9)), (0.9, 0.2, 0.7, 0.6, Some(0.7))]),
            (1_920, 0.0, None, vec![(0.0, 0.9, 0.8, 0.0, None), (1.0, 0.1, 0.2, 0.0, None)]),
        ] {
            crate::record::Recorder::from_samples_for_test(&vec![0.25; count]).finish().unwrap().unwrap().save_wav(&path).unwrap();
            let mut app = app_with(std::slice::from_ref(&path));
            app.settings.sounds[0].start = start;
            app.settings.sounds[0].end = end;
            app.cache_clip(path.clone(), CachedClip::read(&path, &AtomicBool::new(false)).unwrap());
            let ctx = egui::Context::default();
            open_trim_menu(&mut app, &ctx, "trim");
            let output = draw_board(&mut app, &ctx, Vec::new());
            let wave = trim_waveform_rect(&output);
            let point = |fraction| egui::pos2(wave.left() + fraction * wave.width(), wave.center().y);
            let pad = app.settings.sounds[0].clone();
            for pressed in [true, false] { draw_board(&mut app, &ctx, click(point(0.4), egui::PointerButton::Primary, pressed)); }
            assert_eq!(app.settings.sounds[0], pad, "a waveform click does not move a handle");
            assert!(app.dirty_since.is_none());
            for (from, to, released, expected_start, expected_end) in moves {
                draw_board(&mut app, &ctx, click(point(from), egui::PointerButton::Primary, true));
                draw_board(&mut app, &ctx, vec![egui::Event::PointerMoved(point(to))]);
                draw_board(&mut app, &ctx, click(point(released), egui::PointerButton::Primary, false));
                let pad = &app.settings.sounds[0];
                assert!((pad.start - expected_start).abs() < 1e-4, "the same handle follows the pointer through release");
                match (pad.end, expected_end) {
                    (Some(actual), Some(expected)) => assert!((actual - expected).abs() < 1e-4),
                    (None, None) => {}
                    _ => panic!("unexpected end point: {:?}", pad.end),
                }
                assert!(pad.start >= 0.0 && pad.end.unwrap_or(count as f32 / 96_000.0) <= count as f32 / 96_000.0);
            }
            assert_eq!(app.dirty_since.is_some(), count == 96_000);
            assert!(app.playing.is_empty() && app.clip_loads.is_none());
        }
    }

    #[test]
    fn trim_menu_tab_navigation_reaches_only_named_usable_controls() {
        use egui::accesskit::Role;
        let dir = tempfile::tempdir().unwrap();
        let path = wav(dir.path(), "trim.wav");
        let key = |key, modifiers| vec![egui::Event::Key {
            key, physical_key: None, pressed: true, repeat: false, modifiers,
        }];
        let size = egui::vec2(700.0, 420.0);
        for dark in [true, false] {
            let mut app = app_with(std::slice::from_ref(&path));
            app.settings.sounds[0].start = 0.12345;
            app.settings.sounds[0].end = Some(0.812345);
            app.cache_clip(path.clone(), CachedClip::read(&path, &AtomicBool::new(false)).unwrap());
            let original = app.settings.sounds.clone();
            let ctx = egui::Context::default();
            ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
            ctx.enable_accesskit();
            let output = draw_board_size(&mut app, &ctx, size, Vec::new());
            let pad = *output.platform_output.accesskit_update.unwrap().nodes.iter()
                .find(|(_, node)| node.label() == Some("Play trim")).map(|(id, _)| id).unwrap();
            let mut focused = false;
            for _ in 0..30 {
                let output = draw_board_size(&mut app, &ctx, size, key(egui::Key::Tab, egui::Modifiers::NONE));
                if output.platform_output.accesskit_update.unwrap().focus == pad { focused = true; break; }
            }
            assert!(focused);
            draw_board_size(&mut app, &ctx, size, key(egui::Key::F10, egui::Modifiers::SHIFT));
            for _ in 0..6 { draw_board_size(&mut app, &ctx, size, Vec::new()); }
            let mut visited = Vec::new();
            let mut stops = Vec::new();
            let mut output = draw_board_size(&mut app, &ctx, size, Vec::new());
            for _ in 0..20 {
                let tree = output.platform_output.accesskit_update.as_ref().unwrap();
                let node = &tree.nodes.iter().find(|(id, _)| *id == tree.focus).expect("each Tab stop has an accessible control").1;
                assert!(matches!(node.role(), Role::Slider | Role::SpinButton | Role::CheckBox | Role::Button),
                    "the trim menu never focuses an unnamed pointer-only surface: {:?}", node.role());
                let name = node.label().map(str::to_owned).or_else(|| node.labelled_by().iter().find_map(|label|
                    tree.nodes.iter().find(|(id, node)| id == label && node.role() == Role::Label)
                        .and_then(|(_, node)| node.value()).map(str::to_owned))).expect("each focused control identifies itself");
                let last = name == "Remove from soundboard";
                visited.push(name);
                stops.push(tree.focus);
                if last { break; }
                output = draw_board_size(&mut app, &ctx, size, key(egui::Key::Tab, egui::Modifiers::NONE));
            }
            assert_eq!(visited, ["Volume", "Volume", "Show on the strip", "Start", "End", "Reset", "Remove from soundboard"]);
            for expected in stops.into_iter().rev().skip(1) {
                draw_board_size(&mut app, &ctx, size, key(egui::Key::Tab, egui::Modifiers::SHIFT));
                for _ in 0..3 { output = draw_board_size(&mut app, &ctx, size, Vec::new()); }
                assert_eq!(output.platform_output.accesskit_update.as_ref().unwrap().focus, expected,
                    "Shift+Tab follows the same usable controls in reverse");
            }
            for _ in 0..3 { draw_board_size(&mut app, &ctx, size, Vec::new()); }
            assert_eq!(app.settings.sounds, original, "keyboard navigation preserves both endpoints exactly");
            assert!(app.dirty_since.is_none() && app.playing.is_empty() && app.clip_loads.is_none());
        }
    }

    #[test]
    fn editing_after_tab_never_rounds_the_number_field_left_behind() {
        use egui::accesskit::{Action, ActionRequest, Role, TreeId};
        let dir = tempfile::tempdir().unwrap();
        let path = wav(dir.path(), "precision.wav");
        let key = |modifiers| egui::Event::Key {
            key: egui::Key::Tab, physical_key: None, pressed: true, repeat: false, modifiers,
        };
        for dark in [true, false] {
            for paste in [false, true] {
                for previous in ["start", "end", "pad volume", "master volume"] {
                    let mut app = app_with(std::slice::from_ref(&path));
                    app.settings.sounds[0].volume = 0.43561;
                    app.settings.sounds[0].start = 0.12345;
                    app.settings.sounds[0].end = Some(0.812345);
                    app.settings.sound_volume = 0.61347;
                    app.cache_clip(path.clone(), CachedClip::read(&path, &AtomicBool::new(false)).unwrap());
                    let original = app.settings.sounds[0].clone();
                    let ctx = egui::Context::default();
                    ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                    ctx.enable_accesskit();
                    app.play_clip(0, &ctx);
                    if previous != "master volume" {
                        open_keyboard_trim_menu(&mut app, &ctx, "precision");
                    }
                    let output = draw_board(&mut app, &ctx, Vec::new());
                    let value = match previous {
                        "start" => original.start,
                        "end" => original.end.unwrap(),
                        "pad volume" => original.volume,
                        _ => app.settings.sound_volume,
                    };
                    let field = *output.platform_output.accesskit_update.as_ref().unwrap().nodes.iter()
                        .find(|(_, node)| node.role() == Role::SpinButton && node.numeric_value() == Some(value as f64))
                        .map(|(id, _)| id).unwrap_or_else(|| panic!("{previous} field missing; numeric controls: {:?}",
                            output.platform_output.accesskit_update.as_ref().unwrap().nodes.iter()
                                .filter(|(_, node)| matches!(node.role(), Role::SpinButton | Role::Slider))
                                .map(|(_, node)| (node.role(), node.numeric_value(), node.value())).collect::<Vec<_>>()));
                    draw_board(&mut app, &ctx, vec![egui::Event::AccessKitActionRequest(ActionRequest {
                        action: Action::Focus, target_tree: TreeId::ROOT, target_node: field, data: None,
                    })]);
                    for _ in 0..2 { draw_board(&mut app, &ctx, Vec::new()); }
                    let text = if previous == "end" { "0.25" } else { "0.7" };
                    let text = if paste { egui::Event::Paste(text.into()) } else { egui::Event::Text(text.into()) };
                    let mut events = vec![egui::Event::Key {
                        key: egui::Key::A, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::COMMAND,
                    }, text];
                    if matches!(previous, "start" | "end") {
                        events.push(egui::Event::Key {
                            key: egui::Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE,
                        });
                    }
                    if previous == "end" {
                        // Shift+Tab gives the preceding field focus on the next frame.
                        draw_board(&mut app, &ctx, vec![key(egui::Modifiers::SHIFT)]);
                        draw_board(&mut app, &ctx, events);
                    } else {
                        events.insert(0, key(egui::Modifiers::NONE));
                        draw_board(&mut app, &ctx, events);
                    }
                    for _ in 0..3 { draw_board(&mut app, &ctx, Vec::new()); }
                    let mut expected = original;
                    match previous {
                        "start" => expected.end = Some(0.7),
                        "end" => expected.start = 0.25,
                        _ => {}
                    }
                    assert_eq!(app.settings.sounds[0], expected,
                        "typing after leaving {previous} changes only the newly focused field");
                    assert_eq!(app.settings.sound_volume, 0.61347);
                    assert_eq!(app.dirty_since.is_some(), matches!(previous, "start" | "end"));
                    let clips = app.engine.as_ref().unwrap().take_clips();
                    assert_eq!(clips.len(), 1);
                    assert_eq!(clips[0].gain, 0.43561, "navigation never rounds the playing clip's volume");
                    assert!(app.clip_loads.is_none());
                }
            }
        }
    }

    #[test]
    fn first_number_entry_preserves_typing_caret_movement_and_selection() {
        use egui::accesskit::{Action, ActionRequest, Role, TreeId};
        let dir = tempfile::tempdir().unwrap();
        let path = wav(dir.path(), "entry.wav");
        for dark in [true, false] {
            for (paste, passes, tab_batch) in [(false, 1, false), (true, 1, false), (false, 2, false), (true, 2, false),
                (false, 1, true), (true, 1, true), (false, 2, true), (true, 2, true)] {
                for edit in ["append", "insert", "replace", "batched insert", "batched replace", "commit"] {
                    for (field, backwards) in [("start", false), ("start", true), ("end", false), ("end", true),
                        ("pad volume", false), ("pad volume", true), ("master volume", false), ("master volume", true)] {
                        let mut app = app_with(std::slice::from_ref(&path));
                        app.settings.sounds[0].volume = 0.43561;
                        app.settings.sounds[0].start = 0.12345;
                        app.settings.sounds[0].end = Some(0.812345);
                        app.settings.sound_volume = 0.61347;
                        app.cache_clip(path.clone(), CachedClip::read(&path, &AtomicBool::new(false)).unwrap());
                        let original = app.settings.sounds[0].clone();
                        let ctx = egui::Context::default();
                        ctx.options_mut(|options| options.max_passes = passes.try_into().unwrap());
                        ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                        ctx.enable_accesskit();
                        app.play_clip(0, &ctx);
                        if field != "master volume" {
                            open_keyboard_trim_menu(&mut app, &ctx, "entry");
                        }
                        let output = draw_board(&mut app, &ctx, Vec::new());
                        let current_value = match field {
                            "start" => original.start,
                            "end" => original.end.unwrap(),
                            "pad volume" => original.volume,
                            _ => app.settings.sound_volume,
                        };
                        let number = *output.platform_output.accesskit_update.as_ref().unwrap().nodes.iter()
                            .find(|(_, node)| node.role() == Role::SpinButton && node.numeric_value() == Some(current_value as f64))
                            .map(|(id, _)| id).unwrap();
                        let previous = *output.platform_output.accesskit_update.as_ref().unwrap().nodes.iter()
                            .find(|(_, node)| match (field, backwards) {
                                ("start", false) | ("pad volume", true) => node.label() == Some("Show on the strip"),
                                ("start", true) => node.role() == Role::SpinButton && node.numeric_value() == Some(original.end.unwrap() as f64),
                                ("end", false) => node.role() == Role::SpinButton && node.numeric_value() == Some(original.start as f64),
                                ("end", true) => node.label() == Some("Reset"),
                                ("pad volume", false) => node.role() == Role::Slider && node.numeric_value() == Some(original.volume as f64),
                                ("master volume", true) => node.label() == Some("Let clips overlap"),
                                _ => node.role() == Role::Slider && node.numeric_value() == Some(app.settings.sound_volume as f64),
                            }).map(|(id, _)| id).unwrap();
                        draw_board(&mut app, &ctx, vec![egui::Event::AccessKitActionRequest(ActionRequest {
                            action: Action::Focus, target_tree: TreeId::ROOT, target_node: previous, data: None,
                        })]);
                        for _ in 0..2 { draw_board(&mut app, &ctx, Vec::new()); }
                        let (first, following, expected_value) = match (field, edit) {
                            ("start", "append") => ("0.", "25", 0.25),
                            ("end", "append") => ("0.", "75", 0.75),
                            (_, "append") => ("5", "0", 0.5),
                            ("start", "commit") => ("0.25", "", 0.25),
                            ("end", "commit") => ("0.75", "", 0.75),
                            (_, "commit") => ("50", "", 0.5),
                            ("start", "insert" | "batched insert") => ("0.25", "7", 0.275),
                            ("end", "insert" | "batched insert") => ("0.75", "6", 0.765),
                            (_, "insert" | "batched insert") => ("5%", "0", 0.5),
                            ("start", _) => ("0.25", "7", 0.27),
                            ("end", _) => ("0.75", "6", 0.76),
                            _ => ("50", "5", 0.55),
                        };
                        let text = |value: &str| if paste { egui::Event::Paste(value.into()) } else { egui::Event::Text(value.into()) };
                        let mut first_events = vec![text(first)];
                        if !matches!(edit, "append" | "commit") {
                            first_events.push(egui::Event::Key {
                                key: egui::Key::ArrowLeft, physical_key: None, pressed: true, repeat: false,
                                modifiers: if edit.ends_with("replace") { egui::Modifiers::SHIFT } else { egui::Modifiers::NONE },
                            });
                        }
                        if edit.starts_with("batched") { first_events.push(text(following)); }
                        if edit == "commit" {
                            first_events.push(egui::Event::Key {
                                key: egui::Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE,
                            });
                        }
                        if tab_batch {
                            first_events.insert(0, egui::Event::Key {
                                key: egui::Key::Tab, physical_key: None, pressed: true, repeat: false,
                                modifiers: if backwards { egui::Modifiers::SHIFT } else { egui::Modifiers::NONE },
                            });
                            draw_board(&mut app, &ctx, first_events);
                        } else if backwards {
                            draw_board(&mut app, &ctx, vec![egui::Event::Key {
                                key: egui::Key::Tab, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::SHIFT,
                            }]);
                            draw_board(&mut app, &ctx, first_events);
                        } else {
                            first_events.insert(0, if edit.ends_with("insert") {
                                // Native focus navigation uses the last cardinal key
                                // in a batch. Target the field explicitly when the
                                // first input also contains an unmodified arrow.
                                egui::Event::AccessKitActionRequest(ActionRequest {
                                    action: Action::Focus, target_tree: TreeId::ROOT, target_node: number, data: None,
                                })
                            } else {
                                egui::Event::Key {
                                    key: egui::Key::Tab, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE,
                                }
                            });
                            draw_board(&mut app, &ctx, first_events);
                        }
                        let mut following_events = Vec::new();
                        if !edit.starts_with("batched") && edit != "commit" { following_events.push(text(following)); }
                        following_events.push(egui::Event::Key {
                            key: egui::Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE,
                        });
                        draw_board(&mut app, &ctx, following_events);
                        for _ in 0..3 { draw_board(&mut app, &ctx, Vec::new()); }
                        let mut expected = original;
                        match field {
                            "start" => expected.start = expected_value,
                            "end" => expected.end = Some(expected_value),
                            "pad volume" => expected.volume = expected_value,
                            _ => {}
                        }
                        assert_eq!(app.settings.sounds[0], expected, "first entry into {field} preserves {edit}; backwards={backwards}, passes={passes}, tab_batch={tab_batch}");
                        assert_eq!(app.settings.sound_volume, if field == "master volume" { expected_value } else { 0.61347 });
                        let clips = app.engine.as_ref().unwrap().take_clips();
                        assert_eq!(clips.len(), 1);
                        assert_eq!(clips[0].gain, if field == "pad volume" { expected_value } else { 0.43561 });
                        assert!(app.dirty_since.is_some() && app.clip_loads.is_none());
                    }
                }
            }
        }
    }

    #[test]
    fn batched_trim_edits_follow_tabs_without_losing_or_reassigning_text() {
        use egui::accesskit::{Action, ActionRequest, Role, TreeId};
        let dir = tempfile::tempdir().unwrap();
        let path = wav(dir.path(), "ordered.wav");
        let key = |key, modifiers| egui::Event::Key {
            key, physical_key: None, pressed: true, repeat: false, modifiers,
        };
        for dark in [true, false] {
            for passes in [1, 2] {
                for paste in [true, false] {
                    for (backwards, from_control) in [(false, false), (true, false), (false, true), (true, true)] {
                        let mut app = app_with(std::slice::from_ref(&path));
                        app.settings.sounds[0].volume = 0.43561;
                        app.settings.sounds[0].start = 0.12345;
                        app.settings.sounds[0].end = Some(0.812345);
                        app.settings.sound_volume = 0.61347;
                        app.cache_clip(path.clone(), CachedClip::read(&path, &AtomicBool::new(false)).unwrap());
                        let original = app.settings.sounds[0].clone();
                        let ctx = egui::Context::default();
                        ctx.options_mut(|options| options.max_passes = passes.try_into().unwrap());
                        ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                        ctx.enable_accesskit();
                        app.play_clip(0, &ctx);
                        open_keyboard_trim_menu(&mut app, &ctx, "ordered");
                        let output = draw_board(&mut app, &ctx, Vec::new());
                        let previous = *output.platform_output.accesskit_update.as_ref().unwrap().nodes.iter()
                            .find(|(_, node)| if from_control {
                                node.label() == Some(if backwards { "Reset" } else { "Show on the strip" })
                            } else {
                                node.role() == Role::SpinButton && node.numeric_value() == Some(
                                    if backwards { original.end.unwrap() } else { original.start } as f64)
                            }).map(|(id, _)| id).unwrap();
                        draw_board(&mut app, &ctx, vec![egui::Event::AccessKitActionRequest(ActionRequest {
                            action: Action::Focus, target_tree: TreeId::ROOT, target_node: previous, data: None,
                        })]);
                        for _ in 0..2 { draw_board(&mut app, &ctx, Vec::new()); }
                        let text = |value: &str| if paste { egui::Event::Paste(value.into()) } else { egui::Event::Text(value.into()) };
                        let modifiers = if backwards { egui::Modifiers::SHIFT } else { egui::Modifiers::NONE };
                        let mut events = Vec::new();
                        if from_control { events.push(key(egui::Key::Tab, modifiers)); }
                        events.extend([
                            text(if backwards { "0.7" } else { "0.25" }),
                            key(egui::Key::Tab, modifiers),
                            text(if backwards { "0.25" } else { "0.75" }),
                            key(egui::Key::ArrowLeft, egui::Modifiers::NONE),
                            text(if backwards { "7" } else { "6" }),
                            key(egui::Key::Enter, egui::Modifiers::NONE),
                        ]);
                        draw_board(&mut app, &ctx, events);
                        for _ in 0..6 { draw_board(&mut app, &ctx, Vec::new()); }
                        let mut expected = original;
                        expected.start = if backwards { 0.275 } else { 0.25 };
                        expected.end = Some(if backwards { 0.7 } else { 0.765 });
                        assert_eq!(app.settings.sounds[0], expected,
                            "edits on both sides of Tab stay with their own field; backwards={backwards}, from_control={from_control}, passes={passes}");
                        assert_eq!(app.settings.sound_volume, 0.61347);
                        let clips = app.engine.as_ref().unwrap().take_clips();
                        assert_eq!(clips.len(), 1);
                        assert_eq!(clips[0].gain, 0.43561);
                        assert!(app.dirty_since.is_some() && app.clip_loads.is_none());
                    }
                }
            }
        }
    }

    #[test]
    fn queued_volume_entry_commits_before_a_focus_change_or_cancels_on_escape() {
        use egui::accesskit::{Action, ActionRequest, Role, TreeId};
        let dir = tempfile::tempdir().unwrap();
        let path = wav(dir.path(), "handoff.wav");
        for dark in [true, false] {
            for (escape, backwards, assistive) in [
                (true, false, false), (false, false, false), (true, true, false), (false, true, false),
                (false, false, true), (false, true, true),
            ] {
                let mut app = app_with(std::slice::from_ref(&path));
                app.settings.sounds[0].volume = 0.43561;
                app.settings.sounds[0].start = 0.12345;
                app.settings.sounds[0].end = Some(0.812345);
                app.settings.sound_volume = 0.61347;
                app.cache_clip(path.clone(), CachedClip::read(&path, &AtomicBool::new(false)).unwrap());
                let original = app.settings.sounds[0].clone();
                let ctx = egui::Context::default();
                ctx.options_mut(|options| options.max_passes = 1.try_into().unwrap());
                ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                ctx.enable_accesskit();
                app.play_clip(0, &ctx);
                open_keyboard_trim_menu(&mut app, &ctx, "handoff");
                let output = draw_board(&mut app, &ctx, Vec::new());
                let previous = *output.platform_output.accesskit_update.as_ref().unwrap().nodes.iter()
                    .find(|(_, node)| if backwards { node.label() == Some("Show on the strip") }
                        else { node.role() == Role::Slider && node.numeric_value() == Some(original.volume as f64) })
                    .map(|(id, _)| id).unwrap();
                draw_board(&mut app, &ctx, vec![egui::Event::AccessKitActionRequest(ActionRequest {
                    action: Action::Focus, target_tree: TreeId::ROOT, target_node: previous, data: None,
                })]);
                for _ in 0..2 { draw_board(&mut app, &ctx, Vec::new()); }
                let output = draw_board(&mut app, &ctx, vec![
                    egui::Event::Key {
                        key: egui::Key::Tab, physical_key: None, pressed: true, repeat: false,
                        modifiers: if backwards { egui::Modifiers::SHIFT } else { egui::Modifiers::NONE },
                    },
                    egui::Event::Text("50".into()),
                ]);
                assert_eq!(app.settings.sounds[0], original, "the first pass has only changed focus");
                let mut expected = original.clone();
                if escape {
                    let output = draw_board(&mut app, &ctx, vec![egui::Event::Key {
                        key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE,
                    }]);
                    assert!(!egui::Popup::is_any_open(&ctx));
                    let tree = output.platform_output.accesskit_update.as_ref().unwrap();
                    let pad = *tree.nodes.iter().find(|(_, node)| node.label() == Some("Pause handoff")).map(|(id, _)| id).unwrap();
                    assert_eq!(tree.focus, pad, "Escape returns focus to the menu's original pad");
                    draw_board(&mut app, &ctx, vec![egui::Event::Text("90".into())]);
                } else {
                    let (end_id, end_node) = output.platform_output.accesskit_update.as_ref().unwrap().nodes.iter()
                        .find(|(_, node)| node.role() == Role::SpinButton && node.numeric_value() == Some(original.end.unwrap() as f64))
                        .unwrap();
                    let end = end_node.bounds().unwrap();
                    let pos = egui::pos2(((end.x0 + end.x1) / 2.0) as f32, ((end.y0 + end.y1) / 2.0) as f32);
                    if assistive {
                        draw_board(&mut app, &ctx, vec![egui::Event::AccessKitActionRequest(ActionRequest {
                            action: Action::Focus, target_tree: TreeId::ROOT, target_node: *end_id, data: None,
                        })]);
                    } else {
                        for pressed in [true, false] { draw_board(&mut app, &ctx, click(pos, egui::PointerButton::Primary, pressed)); }
                    }
                    for _ in 0..3 { draw_board(&mut app, &ctx, Vec::new()); }
                    assert!(egui::Popup::is_any_open(&ctx));
                    let output = draw_board(&mut app, &ctx, Vec::new());
                    assert_eq!(output.platform_output.accesskit_update.as_ref().unwrap().focus, *end_id,
                        "the deferred focus change still reaches End");
                    expected.volume = 0.5;
                    assert_eq!(app.settings.sounds[0], expected, "Volume commits before End receives new input");
                    draw_board(&mut app, &ctx, vec![
                        egui::Event::Text("0.75".into()),
                        egui::Event::Key {
                            key: egui::Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE,
                        },
                    ]);
                    expected.end = Some(0.75);
                }
                for _ in 0..4 { draw_board(&mut app, &ctx, Vec::new()); }
                assert_eq!(app.settings.sounds[0], expected, "queued text stays with Volume; escape={escape}, backwards={backwards}");
                assert_eq!(app.settings.sound_volume, 0.61347);
                let clips = app.engine.as_ref().unwrap().take_clips();
                assert_eq!(clips.len(), 1);
                assert_eq!(clips[0].gain, expected.volume);
                assert_eq!(app.dirty_since.is_some(), !escape);
                assert!(app.clip_loads.is_none());
            }
        }
    }

    #[test]
    fn waveform_drags_survive_a_pending_keyboard_volume_edit() {
        use egui::accesskit::{Action, ActionRequest, Role, TreeId};
        let dir = tempfile::tempdir().unwrap();
        let path = wav(dir.path(), "drag.wav");
        for dark in [true, false] {
            for backwards in [true, false] {
                for start_handle in [true, false] {
                    let mut app = app_with(std::slice::from_ref(&path));
                    app.settings.sounds[0].volume = 0.43561;
                    app.settings.sounds[0].start = 0.1;
                    app.settings.sounds[0].end = Some(0.9);
                    app.settings.sound_volume = 0.61347;
                    app.cache_clip(path.clone(), CachedClip::read(&path, &AtomicBool::new(false)).unwrap());
                    let original = app.settings.sounds[0].clone();
                    let ctx = egui::Context::default();
                    ctx.options_mut(|options| options.max_passes = 1.try_into().unwrap());
                    ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                    ctx.enable_accesskit();
                    app.play_clip(0, &ctx);
                    open_keyboard_trim_menu(&mut app, &ctx, "drag");
                    let output = draw_board(&mut app, &ctx, Vec::new());
                    let wave = trim_waveform_rect(&output);
                    let point = |fraction| egui::pos2(wave.left() + fraction * wave.width(), wave.center().y);
                    let previous = *output.platform_output.accesskit_update.as_ref().unwrap().nodes.iter()
                        .find(|(_, node)| if backwards { node.label() == Some("Show on the strip") }
                            else { node.role() == Role::Slider && node.numeric_value() == Some(original.volume as f64) })
                        .map(|(id, _)| id).unwrap();
                    draw_board(&mut app, &ctx, vec![egui::Event::AccessKitActionRequest(ActionRequest {
                        action: Action::Focus, target_tree: TreeId::ROOT, target_node: previous, data: None,
                    })]);
                    for _ in 0..2 { draw_board(&mut app, &ctx, Vec::new()); }
                    draw_board(&mut app, &ctx, vec![
                        egui::Event::Key {
                            key: egui::Key::Tab, physical_key: None, pressed: true, repeat: false,
                            modifiers: if backwards { egui::Modifiers::SHIFT } else { egui::Modifiers::NONE },
                        },
                        egui::Event::Text("50".into()),
                    ]);
                    assert_eq!(app.settings.sounds[0], original, "the volume edit is still pending");
                    let (from, to) = if start_handle { (0.1, 0.7) } else { (0.9, 0.2) };
                    let released = if start_handle { 0.65 } else { 0.3 };
                    draw_board(&mut app, &ctx, click(point(from), egui::PointerButton::Primary, true));
                    draw_board(&mut app, &ctx, vec![egui::Event::PointerMoved(point(to))]);
                    draw_board(&mut app, &ctx, click(point(released), egui::PointerButton::Primary, false));
                    for _ in 0..6 { draw_board(&mut app, &ctx, Vec::new()); }
                    let actual = &app.settings.sounds[0];
                    let mut expected = original;
                    expected.volume = 0.5;
                    if start_handle {
                        assert!((actual.start - released).abs() < 1e-4, "the queued drag moves Start through release; backwards={backwards}");
                        expected.start = actual.start;
                    } else {
                        assert!((actual.end.unwrap() - released).abs() < 1e-4, "the queued drag moves End through release; backwards={backwards}");
                        expected.end = actual.end;
                    }
                    assert_eq!(*actual, expected, "the other saved fields stay unchanged");
                    assert_eq!(app.settings.sound_volume, 0.61347);
                    let clips = app.engine.as_ref().unwrap().take_clips();
                    assert_eq!(clips.len(), 1);
                    assert_eq!(clips[0].gain, 0.5);
                    assert!(app.dirty_since.is_some() && app.clip_loads.is_none());
                }
            }
        }
    }

    #[test]
    fn trim_accessibility_labels_survive_focus_typing_and_assistive_edits() {
        use egui::accesskit::{Action, ActionData, ActionRequest, NodeId, Role, TreeId};
        fn assert_label(output: &egui::FullOutput, id: NodeId, name: &str) {
            let tree = output.platform_output.accesskit_update.as_ref().unwrap();
            let field = &tree.nodes.iter().find(|(node_id, _)| *node_id == id).unwrap().1;
            let label = *tree.nodes.iter().find(|(_, node)| node.role() == Role::Label && node.value() == Some(name)).map(|(id, _)| id).unwrap();
            assert_eq!(field.role(), Role::SpinButton);
            assert!(field.labelled_by().contains(&label), "the number field identifies itself as {name}");
        }
        let action = |target_node, action, data| egui::Event::AccessKitActionRequest(ActionRequest {
            action, target_tree: TreeId::ROOT, target_node, data,
        });
        let dir = tempfile::tempdir().unwrap();
        let path = wav(dir.path(), "trim.wav");
        for dark in [true, false] {
            let mut app = app_with(std::slice::from_ref(&path));
            app.settings.sounds[0].start = 0.12345;
            app.settings.sounds[0].end = Some(0.812345);
            app.cache_clip(path.clone(), CachedClip::read(&path, &AtomicBool::new(false)).unwrap());
            let original = app.settings.sounds[0].clone();
            let ctx = egui::Context::default();
            ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
            ctx.enable_accesskit();
            open_trim_menu(&mut app, &ctx, "trim");
            let output = draw_board(&mut app, &ctx, Vec::new());
            let tree = output.platform_output.accesskit_update.as_ref().unwrap();
            let [start, end] = [original.start, original.end.unwrap()].map(|value| {
                *tree.nodes.iter().find(|(_, node)| node.role() == Role::SpinButton && node.numeric_value() == Some(value as f64))
                    .map(|(id, _)| id).expect("the trim endpoint exposes its exact numeric value")
            });
            for (id, label) in [(start, "Start"), (end, "End")] { assert_label(&output, id, label); }
            assert_eq!(app.settings.sounds[0], original, "reading accessibility metadata preserves saved precision");
            assert!(app.dirty_since.is_none());
            let output = draw_board(&mut app, &ctx, vec![action(start, Action::Focus, None)]);
            assert_eq!(output.platform_output.accesskit_update.as_ref().unwrap().focus, start);
            for (id, label) in [(start, "Start"), (end, "End")] { assert_label(&output, id, label); }
            draw_board(&mut app, &ctx, vec![
                action(end, Action::Focus, None),
                action(end, Action::SetValue, Some(ActionData::NumericValue(0.7))),
            ]);
            draw_board(&mut app, &ctx, Vec::new());
            assert_eq!(app.settings.sounds[0].start, original.start, "an edit to End never rounds the untouched Start on blur");
            assert_eq!(app.settings.sounds[0].end, Some(0.7));
            draw_board(&mut app, &ctx, vec![action(start, Action::Focus, None)]);
            draw_board(&mut app, &ctx, vec![egui::Event::Text("0.25".into())]);
            let output = draw_board(&mut app, &ctx, Vec::new());
            assert_label(&output, start, "Start");
            assert_eq!(app.settings.sounds[0].start, 0.25);
            assert_eq!(app.settings.sounds[0].end, Some(0.7), "typing in Start leaves End unchanged");
            draw_board(&mut app, &ctx, vec![
                action(end, Action::Focus, None),
                action(end, Action::SetValue, Some(ActionData::NumericValue(0.75))),
            ]);
            let output = draw_board(&mut app, &ctx, Vec::new());
            assert_eq!(output.platform_output.accesskit_update.as_ref().unwrap().focus, end);
            for (id, label) in [(start, "Start"), (end, "End")] { assert_label(&output, id, label); }
            assert_eq!((app.settings.sounds[0].start, app.settings.sounds[0].end), (0.25, Some(0.75)));
            draw_board(&mut app, &ctx, vec![action(end, Action::Increment, None)]);
            assert!((app.settings.sounds[0].end.unwrap() - 0.76).abs() < 1e-6);
            assert_eq!(app.settings.sounds[0].start, 0.25);
            // Leave edit mode before exercising a direct range-value request.
            let output = draw_board(&mut app, &ctx, Vec::new());
            let tree = output.platform_output.accesskit_update.unwrap();
            let strip = *tree.nodes.iter().find(|(_, node)| node.label() == Some("Show on the strip")).map(|(id, _)| id).unwrap();
            let output = draw_board(&mut app, &ctx, vec![action(strip, Action::Focus, None)]);
            assert_eq!(output.platform_output.accesskit_update.as_ref().unwrap().focus, strip, "focus moves out of the number field");
            for (id, label) in [(start, "Start"), (end, "End")] { assert_label(&output, id, label); }
            draw_board(&mut app, &ctx, vec![action(end, Action::SetValue, Some(ActionData::NumericValue(-1.0)))]);
            assert_eq!((app.settings.sounds[0].start, app.settings.sounds[0].end), (0.25, Some(0.3)), "assistive edits respect the minimum trim");
            draw_board(&mut app, &ctx, vec![action(end, Action::SetValue, Some(ActionData::NumericValue(10.0)))]);
            assert_eq!((app.settings.sounds[0].start, app.settings.sounds[0].end), (0.25, None), "the endpoint stays inside the source");
            draw_board(&mut app, &ctx, vec![action(start, Action::SetValue, Some(ActionData::NumericValue(10.0)))]);
            assert_eq!((app.settings.sounds[0].start, app.settings.sounds[0].end), (0.95, None));
            let output = draw_board(&mut app, &ctx, Vec::new());
            for (id, label) in [(start, "Start"), (end, "End")] { assert_label(&output, id, label); }
            assert!(app.dirty_since.is_some() && app.playing.is_empty() && app.clip_loads.is_none());
        }
    }

    #[test]
    fn editing_trim_numbers_preserves_the_other_saved_point_and_bounds_keyboard_edits() {
        let dir = tempfile::tempdir().unwrap();
        let path = wav(dir.path(), "trim.wav");
        for (label, start, end, value, expected_start, expected_end) in [
            ("Start", 0.1, Some(2.0), "0.25", 0.25, Some(2.0)),
            ("End", 0.003, Some(0.8), "0.75", 0.003, Some(0.75)),
        ] {
            let mut app = app_with(std::slice::from_ref(&path));
            app.settings.sounds[0].start = start;
            app.settings.sounds[0].end = end;
            app.cache_clip(path.clone(), CachedClip::read(&path, &AtomicBool::new(false)).unwrap());
            let ctx = egui::Context::default();
            open_trim_menu(&mut app, &ctx, "trim");
            let output = draw_board(&mut app, &ctx, Vec::new());
            let pos = trim_number_position(&output, label);
            for pressed in [true, false] { draw_board(&mut app, &ctx, click(pos, egui::PointerButton::Primary, pressed)); }
            draw_board(&mut app, &ctx, Vec::new());
            draw_board(&mut app, &ctx, vec![
                egui::Event::Text(value.into()),
                egui::Event::Key { key: egui::Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE },
            ]);
            draw_board(&mut app, &ctx, Vec::new());
            assert_eq!(app.settings.sounds[0].start, expected_start);
            assert_eq!(app.settings.sounds[0].end, expected_end);
            assert!(app.dirty_since.is_some() && app.playing.is_empty() && app.clip_loads.is_none());
            if app.trim_clip.is_none() { open_trim_menu(&mut app, &ctx, "trim"); }
            let output = draw_board(&mut app, &ctx, Vec::new());
            let pos = trim_number_position(&output, label);
            for pressed in [true, false] { draw_board(&mut app, &ctx, click(pos, egui::PointerButton::Primary, pressed)); }
            draw_board(&mut app, &ctx, Vec::new());
            draw_board(&mut app, &ctx, (0..100).map(|_| egui::Event::Key {
                key: egui::Key::ArrowUp, physical_key: None, pressed: true, repeat: true, modifiers: egui::Modifiers::NONE,
            }).collect());
            assert_eq!(app.settings.sounds[0].start, if label == "Start" { 0.95 } else { expected_start });
            assert_eq!(app.settings.sounds[0].end, if label == "End" { None } else { expected_end });
        }
    }

    #[test]
    fn reset_recovers_saved_trims_beyond_the_current_source() {
        let dir = tempfile::tempdir().unwrap();
        let path = wav(dir.path(), "trim.wav");
        for start in [0.0, 2.0] {
            let mut app = app_with(std::slice::from_ref(&path));
            app.settings.sounds[0].start = start;
            app.settings.sounds[0].end = Some(3.0);
            app.cache_clip(path.clone(), CachedClip::read(&path, &AtomicBool::new(false)).unwrap());
            let ctx = egui::Context::default();
            open_trim_menu(&mut app, &ctx, "trim");
            let output = draw_board(&mut app, &ctx, Vec::new());
            text_rect(&output, "Saved trim extends past this clip. Adjust it or Reset.");
            let pos = text_rect(&output, "Reset").center();
            for pressed in [true, false] { draw_board(&mut app, &ctx, click(pos, egui::PointerButton::Primary, pressed)); }
            assert_eq!((app.settings.sounds[0].start, app.settings.sounds[0].end), (0.0, None));
            assert!(app.dirty_since.is_some() && app.clip_loads.is_none());
            app.play_clip(0, &ctx);
            let clips = app.engine.as_ref().unwrap().take_clips();
            assert_eq!(clips.len(), 1);
            assert_eq!(clips[0].samples().len(), 48_000, "Reset makes the full current source playable");
        }
    }

    #[test]
    #[ignore = "writes saved-trim screenshots; needs Python with Pillow"]
    fn snapshot_trim_recovery() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trim.wav");
        for (state, count, start, end) in [
            ("shorter", 96_000, 2.0, Some(3.0)),
            ("tiny", 1_920, 0.003, Some(0.019)),
            ("sub-millisecond", 2, 0.0, None),
        ] {
            crate::record::Recorder::from_samples_for_test(&vec![0.25; count]).finish().unwrap().unwrap().save_wav(&path).unwrap();
            for dark in [true, false] {
                let mut app = app_with(std::slice::from_ref(&path));
                app.settings.sounds[0].start = start;
                app.settings.sounds[0].end = end;
                app.cache_clip(path.clone(), CachedClip::read(&path, &AtomicBool::new(false)).unwrap());
                let ctx = egui::Context::default();
                let pos = pad_icon_position(&mut app, &ctx, "trim");
                let events = vec![Vec::new(), click(pos, egui::PointerButton::Secondary, true), click(pos, egui::PointerButton::Secondary, false)];
                let theme = if dark { "dark" } else { "light" };
                let image = super::super::snapshot::snapshot_with_events(
                    &mut app, &format!("trim-{state}-{theme}"), egui::vec2(700.0, 420.0), 1.0, dark, events,
                );
                println!("SNAPSHOT {}", image.display());
                assert_eq!((app.settings.sounds[0].start, app.settings.sounds[0].end), (start, end));
            }
        }
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
            if !play_first { app.trim_clip = Some(path.clone()); }
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
            assert!(late.send(Err(anyhow::anyhow!("late preview result"))).is_ok());
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
        assert!(app.clip_errors[&paths[0]].contains("stopped unexpectedly"));
        for _ in 0..3 { assert!(app.trim_waveform(&paths[0], &ctx).is_none()); }
        assert!(app.clip_loads.is_none(), "a failed waveform must not retry on every frame");
        app.close_trim_editor();
        assert!(app.trim_waveform(&paths[0], &ctx).is_none());
        assert!(app.clip_loads.is_none(), "reopening the menu keeps the error and retry control");
        wav(dir.path(), "missing.wav");
        app.clip_errors.remove(&paths[0]);
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
        let (done, cancel) = delayed(&mut app, std::slice::from_ref(&path));
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
        assert!(app.trim_clip.is_none() && !app.has_pending_playback() && cancel.load(Ordering::Relaxed));
        assert!(app.clip_loads.is_some(), "keep tracking the cancelled decoder until it exits");
        drop(done);
        finish(&mut app, &ctx);
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
        assert!(app.cached_clip(&path).is_some() && !app.clip_errors.contains_key(&path));
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
    fn playing_and_paused_pads_keep_the_loaded_duration_after_trims_and_file_edits() {
        let dir = tempfile::tempdir().unwrap();
        let path = wav(dir.path(), "horn.wav");
        let ctx = egui::Context::default();
        for paused in [false, true] {
            wav(dir.path(), "horn.wav");
            let mut app = app_with(std::slice::from_ref(&path));
            app.settings.sounds[0].start = 0.25;
            app.settings.sounds[0].end = Some(0.75);
            app.play_clip(0, &ctx);
            finish(&mut app, &ctx);
            if paused { app.toggle_clip(0, &ctx); }
            let mut loaded = app.engine.as_ref().unwrap().take_clips();
            loaded[0].pos = 6_000;
            app.engine.as_ref().unwrap().resume_clips(loaded);
            app.sync_clip_progress();

            app.settings.sounds[0].start = 0.1;
            app.settings.sounds[0].end = Some(0.3);
            crate::record::Recorder::from_samples_for_test(&[0.1; 76_800]).finish().unwrap().unwrap().save_wav(&path).unwrap();
            assert!(app.trim_waveform(&path, &ctx).is_none());
            finish(&mut app, &ctx);
            assert_eq!(app.trim_waveform(&path, &ctx).unwrap().0, 0.8);
            draw_board(&mut app, &ctx, Vec::new());
            let output = draw_board(&mut app, &ctx, Vec::new());
            let label = if paused { "WAV · 0.5 s · Paused" } else { "WAV · 0.5 s · Playing" };
            text_rect(&output, label);

            let loaded = app.engine.as_ref().unwrap().take_clips();
            assert_eq!(loaded[0].samples().len(), 24_000, "editing the source keeps the loaded selection alive");
            assert_eq!(loaded[0].pos, 6_000);
            assert_eq!(loaded[0].paused, paused);
            assert!((loaded[0].samples()[0] - 0.25).abs() < 0.001);
            app.engine = Some(Engine::for_test());
            app.engine.as_ref().unwrap().resume_clips(loaded);
            app.sync_clip_progress();
            text_rect(&draw_board(&mut app, &ctx, Vec::new()), label);

            app.stop_clips();
            text_rect(&draw_board(&mut app, &ctx, Vec::new()), "WAV · 0.2 s");
            app.play_clip(0, &ctx);
            assert!(app.clip_loads.is_none(), "restarting uses the refreshed cache");
            text_rect(&draw_board(&mut app, &ctx, Vec::new()), "WAV · 0.2 s · Playing");
            let restarted = app.engine.as_ref().unwrap().take_clips();
            assert_eq!(restarted[0].pos, 0);
            assert!(!restarted[0].paused);
            assert!((restarted[0].samples()[0] - 0.1).abs() < 0.001);
        }
    }

    #[test]
    fn revealing_a_strip_pad_scrolls_to_its_path_without_changing_audio_or_pending_loads() {
        let dir = tempfile::tempdir().unwrap();
        let first = wav(dir.path(), "clip-0.wav");
        let paths: Vec<_> = (0..33).map(|i| dir.path().join(if i == 32 { "wanted.wav".into() } else { format!("clip-{i}.wav") })).collect();
        for dark in [true, false] {
            let mut app = app_with(&paths);
            app.settings.sounds[0].name = Some("wanted".into());
            app.settings.sounds[32].starred = true;
            app.cache_clip(first.clone(), CachedClip::read(&first, &AtomicBool::new(false)).unwrap());
            let ctx = egui::Context::default();
            ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
            ctx.enable_accesskit();
            app.play_clip(0, &ctx);
            let mut loaded = app.engine.as_ref().unwrap().take_clips();
            loaded[0].pos = 12_000;
            loaded[0].paused = true;
            app.engine.as_ref().unwrap().resume_clips(loaded);
            app.sync_clip_progress();
            let before = app.engine.as_ref().unwrap().clip_progress();
            let (done, cancel) = delayed(&mut app, &paths[1..2]);
            app.clip_errors.insert(paths[32].clone(), "Reconnect the audio drive before retrying".into());
            for _ in 0..2 { draw_board_size(&mut app, &ctx, egui::vec2(700.0, 420.0), Vec::new()); }
            draw_board_size(&mut app, &ctx, egui::vec2(700.0, 420.0), vec![
                egui::Event::PointerMoved(egui::pos2(300.0, 55.0)),
                egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: egui::vec2(0.0, -1000.0), modifiers: egui::Modifiers::NONE, phase: egui::TouchPhase::Move },
            ]);
            for _ in 0..3 { draw_board_size(&mut app, &ctx, egui::vec2(700.0, 420.0), Vec::new()); }
            let scrolled = draw_board_size(&mut app, &ctx, egui::vec2(700.0, 420.0), Vec::new());
            assert!(!scrolled.shapes.iter().any(|shape| matches!(&shape.shape,
                egui::Shape::Text(text) if text.galley.text() == "Click a name to rename; drag to reorder."
                    && shape.clip_rect.contains(text.visual_bounding_rect().center()))), "the board was already scrolled away before revealing the pad");
            app.begin_pad_rename(&app.settings.sounds[0].clone());
            app.capturing = Some(super::super::Binding::Pad(paths[0].clone()));
            app.trim_clip = Some(paths[2].clone());
            app.pad_filter = "does not match the target".into();
            app.page = super::super::Page::Settings;
            app.hidden = true;
            app.settings.collapsed = true;
            let status = app.sound_status.clone();
            let output = ctx.run_ui(egui::RawInput::default(), |ui| app.reveal_pad(32, ui.ctx()));
            let commands = &output.viewport_output[&egui::ViewportId::ROOT].commands;
            for command in [egui::ViewportCommand::Visible(true), egui::ViewportCommand::Minimized(false), egui::ViewportCommand::Focus] {
                assert!(commands.contains(&command));
            }
            assert!(app.page == super::super::Page::Soundboard && !app.hidden && !app.settings.collapsed);
            assert!(app.pad_filter.is_empty() && app.renaming_pad.is_none() && app.capturing.is_none() && app.trim_clip.is_none());
            let mut target = None;
            let mut output = draw_board_size(&mut app, &ctx, egui::vec2(700.0, 420.0), Vec::new());
            for _ in 0..5 {
                let update = output.platform_output.accesskit_update.as_ref().unwrap();
                if let Some((id, _)) = update.nodes.iter().find(|(_, node)| node.label() == Some("Retry wanted")) { target = Some(*id); }
                output = draw_board_size(&mut app, &ctx, egui::vec2(700.0, 420.0), Vec::new());
            }
            assert_eq!(Some(output.platform_output.accesskit_update.unwrap().focus), target);
            let visible = output.shapes.iter().find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.text() == "wanted" && shape.clip_rect.contains(text.visual_bounding_rect().center()) => Some(text.visual_bounding_rect()),
                _ => None,
            }).expect("the requested pad is visible, even with another pad of the same name above it");
            assert!(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(700.0, 420.0)).contains_rect(visible));
            assert!(app.pad_to_reveal.is_none());
            assert_eq!(app.engine.as_ref().unwrap().clip_progress(), before);
            assert_eq!(app.sound_status, status);
            assert!(!cancel.load(Ordering::Relaxed) && app.has_pending_playback());
            assert_eq!(app.clip_loads.as_ref().unwrap().requests[0].path, paths[1]);
            assert!(app.clip_errors.contains_key(&paths[32]));
            drop(done);
        }
    }

    #[test]
    fn revealing_removed_pads_or_revealing_while_quitting_leaves_the_view_alone() {
        let mut app = app_with(&[PathBuf::from("horn.wav")]);
        let ctx = egui::Context::default();
        app.page = super::super::Page::Settings;
        app.hidden = true;
        app.pad_filter = "current search".into();
        app.quitting = true;
        app.reveal_pad(0, &ctx);
        app.quitting = false;
        app.reveal_pad(99, &ctx);
        assert!(app.page == super::super::Page::Settings && app.hidden);
        assert_eq!(app.pad_filter, "current search");
        assert!(app.pad_to_reveal.is_none());
    }

    #[test]
    fn strip_resume_keeps_loaded_audio_after_a_source_failure_and_stop_returns_to_retry() {
        use super::super::overlay::{self, Action, Meter};
        let dir = tempfile::tempdir().unwrap();
        for dark in [true, false] {
            let path = wav(dir.path(), "horn.wav");
            let mut app = app_with(std::slice::from_ref(&path));
            app.settings.sounds[0].starred = true;
            app.cache_clip(path.clone(), CachedClip::read(&path, &AtomicBool::new(false)).unwrap());
            let ctx = egui::Context::default();
            ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
            ctx.enable_accesskit();
            app.play_clip(0, &ctx);
            let mut loaded = app.engine.as_ref().unwrap().take_clips();
            loaded[0].pos = 12_000;
            app.engine.as_ref().unwrap().resume_clips(loaded);
            app.toggle_clip(0, &ctx);
            let before = app.engine.as_ref().unwrap().clip_progress();
            assert_eq!(before.len(), 1);
            assert!(before[0].paused && before[0].fraction == 0.25);

            std::fs::remove_file(&path).unwrap();
            assert!(app.trim_waveform(&path, &ctx).is_none());
            finish(&mut app, &ctx);
            let error = app.clip_errors[&path].clone();
            assert_eq!(app.engine.as_ref().unwrap().clip_progress(), before, "a failed preview leaves the paused clip loaded");
            let render = |app: &App, events| {
                let mut actions = Vec::new();
                let output = ctx.run_ui(egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(320.0, 140.0))),
                    time: Some(ctx.cumulative_frame_nr() as f64 * 0.2), events, ..Default::default()
                }, |ui| {
                    let rect = egui::Rect::from_min_size(ui.max_rect().min, egui::vec2(ui.available_width(), overlay::HEIGHT));
                    ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
                        actions = overlay::draw(ui, Meter::Live(-20.0), &app.strip_contents());
                    });
                });
                (output, actions)
            };
            let (output, _) = render(&app, Vec::new());
            assert!(output.platform_output.accesskit_update.as_ref().unwrap().nodes.iter()
                .any(|(_, node)| node.label() == Some("Resume horn")), "a paused loaded clip offers Resume even after a source failure");
            let pos = text_rect(&output, "horn").center();
            assert!(output.shapes.iter().any(|shape| matches!(&shape.shape,
                egui::Shape::Rect(rect) if rect.stroke.color == AMBER && rect.rect.contains(pos))));
            assert!(!output.shapes.iter().any(|shape| matches!(&shape.shape,
                egui::Shape::Text(text) if text.galley.text() == "! horn")));
            render(&app, vec![egui::Event::PointerMoved(pos)]);
            let mut tooltip = false;
            for _ in 0..6 {
                let (output, _) = render(&app, Vec::new());
                tooltip |= output.shapes.iter().any(|shape| matches!(&shape.shape,
                    egui::Shape::Text(text) if text.galley.text().contains(&error) && text.galley.text().contains("Click to resume")));
            }
            assert!(tooltip, "the source error remains available without mislabeling the resume action");
            let mut actions = Vec::new();
            for pressed in [true, false] { actions.extend(render(&app, click(pos, egui::PointerButton::Primary, pressed)).1); }
            assert_eq!(actions, [Action::PlayPad(0)]);
            app.toggle_clip(0, &ctx);
            let after = app.engine.as_ref().unwrap().clip_progress();
            assert_eq!(after[0].fraction, before[0].fraction);
            assert_eq!(after[0].duration, before[0].duration);
            assert!(!after[0].paused && app.clip_loads.is_none(), "resume uses the loaded audio without reopening the missing source");
            let (output, _) = render(&app, Vec::new());
            assert!(output.platform_output.accesskit_update.unwrap().nodes.iter()
                .any(|(_, node)| node.label() == Some("Pause horn")));

            app.stop_clips();
            let (output, _) = render(&app, Vec::new());
            assert!(output.platform_output.accesskit_update.as_ref().unwrap().nodes.iter()
                .any(|(_, node)| node.label() == Some("Retry horn")));
            text_rect(&output, "! horn");
            assert_eq!(app.clip_errors[&path], error);
            assert!(app.pad_progress.is_empty() && app.playing.is_empty());
        }
    }

    #[test]
    fn an_empty_trim_explains_the_silent_pad_and_can_be_corrected() {
        let path = PathBuf::from("shortened.wav");
        let samples = Arc::new(vec![0.25; dsp::SR as usize]);
        for (start, end) in [(2.0, None), (0.5, Some(0.5))] {
            let mut app = app_with(std::slice::from_ref(&path));
            app.settings.overlap_clips = false;
            app.engine.as_ref().unwrap().play_sound(Clip::new(7, Arc::clone(&samples), 1.0), false);
            app.sync_clip_progress();
            let before = app.engine.as_ref().unwrap().clip_progress();
            let pad = Pad { start, end, ..app.settings.sounds[0].clone() };
            app.play_decoded_clip(&pad, &samples);
            app.sync_clip_progress();

            assert_eq!(app.engine.as_ref().unwrap().clip_progress(), before, "an empty pad must not interrupt a playing clip");
            assert!(app.sound_status.0.contains("No audio in the trim for shortened"));
            assert!(app.sound_status.0.contains("Right-click the pad"));
            assert_eq!(app.sound_status.1, AMBER);

            let corrected = Pad { start: 0.25, end: Some(0.75), ..pad };
            app.play_decoded_clip(&corrected, &samples);
            assert_eq!(app.sound_status, ("Playing · shortened".into(), GREEN));
            let clips = app.engine.as_ref().unwrap().take_clips();
            assert_eq!(clips.len(), 1);
            assert_eq!(clips[0].key, clip_key(&path));
            assert_eq!(clips[0].samples().len(), 24_000);
        }
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
            assert!(Arc::ptr_eq(&cancel, &app.clip_loads.as_ref().unwrap().cancel), "the next decoder waits for cancellation to finish");
            assert_eq!(app.playing.is_empty(), !cached, "cached audio can still start immediately");
            assert!(late.send(CachedClip::read(&paths[0], &AtomicBool::new(false))).is_ok());
            finish(&mut app, &ctx);
            assert_eq!(app.playing, [clip_key(&paths[1])]);
            assert!(!app.clip_cache.contains_key(&paths[0]), "ignore the cancelled decoder's late success");
        }
    }

    #[test]
    fn repeated_cancellation_keeps_one_decoder_and_preserves_the_latest_request() {
        let dir = tempfile::tempdir().unwrap();
        let paths = [wav(dir.path(), "one.wav"), wav(dir.path(), "two.wav")];
        let ctx = egui::Context::default();
        let mut app = app_with(&paths);
        let (done, cancel) = delayed(&mut app, &paths[..1]);
        for _ in 0..3 {
            app.play_clip(1, &ctx);
            app.toggle_clip(1, &ctx);
            app.play_clip(0, &ctx);
            let pending = app.clip_loads.as_ref().unwrap();
            assert!(Arc::ptr_eq(&cancel, &pending.cancel) && cancel.load(Ordering::Relaxed));
            assert_eq!(pending.requests.len(), 1);
            assert_eq!(pending.requests[0].path, paths[0]);
            assert!(app.playing.is_empty());
        }
        drop(done); // A cancelled worker can also exit without sending a result.
        finish(&mut app, &ctx);
        assert_eq!(app.playing, [clip_key(&paths[0])]);
        assert!(!app.clip_cache.contains_key(&paths[1]));
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
        let error = anyhow::anyhow!("Reconnect the audio drive and try again").context(format!("Could not read {}", paths[0].display()));
        let message = format!("{error:#}");
        failed.send(Err(error)).unwrap();
        app.poll_clip_loads(&ctx);
        assert_eq!(app.clip_errors[&paths[0]], message);
        assert!(app.sound_status.0.starts_with("Loading"), "the next load may replace the shared status immediately");
        finish(&mut app, &ctx);
        let clips = app.engine.as_ref().unwrap().take_clips();
        assert_eq!(clips.iter().map(|clip| clip.key).collect::<Vec<_>>(), [clip_key(&paths[1]), clip_key(&paths[2])]);
        assert_eq!(app.clip_errors.len(), 1);
        assert_eq!(app.clip_errors[&paths[0]], message, "the failure stays with the file when pads move and other clips play");
        app.stop_clips();
        assert_eq!(app.clip_errors[&paths[0]], message, "stopping playback preserves errors for review");
        app.remove_clip(2);
        assert!(app.clip_errors.is_empty(), "removing the failed pad clears its transient error");
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
            assert!(!app.has_pending_playback());
            let status = app.sound_status.clone();
            assert!(late.send(Err(anyhow::anyhow!("late result"))).is_ok());
            app.engine = Some(Engine::for_test());
            app.poll_clip_loads(&ctx);
            assert!(app.clip_loads.is_none() && app.playing.is_empty());
            assert_eq!(app.sound_status, status, "a cancelled failure must not replace the current status");
            assert!(path.exists(), "cancelling or removing a pad keeps its file");
            assert!(app.clip_errors.is_empty(), "a cancelled failure must not mark a pad as failed");
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
        let error = app.clip_errors[&path].clone();
        app.stop_engine(true);
        app.play_clip(0, &ctx);
        assert_eq!(app.clip_errors[&path], error, "the Start requirement must not erase the failure before a retry");
        app.engine = Some(Engine::for_test());
        wav(dir.path(), "reconnected.wav");
        app.play_clip(0, &ctx);
        assert!(!app.clip_errors.contains_key(&path), "a retry clears the previous failure while it loads");
        finish(&mut app, &ctx);
        assert_eq!(app.playing, [clip_key(&path)]);
        assert!(app.clip_errors.is_empty());
    }

    #[test]
    fn the_failed_pad_shows_the_full_error_and_clicking_it_retries() {
        let dir = tempfile::tempdir().unwrap();
        let paths = [dir.path().join("horn.wav"), wav(dir.path(), "applause.wav")];
        let mut app = app_with(&paths);
        app.settings.overlap_clips = true;
        app.settings.sounds[0].starred = true;
        let (failed, _) = delayed(&mut app, &paths[..1]);
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        app.play_clip(1, &ctx);
        let cause = "Reconnect the audio drive before retrying";
        let error = anyhow::anyhow!(cause).context(format!("Could not open {}", paths[0].display()));
        let message = format!("{error:#}");
        assert!(message.chars().count() > 64, "the error's cause extends beyond the shared status truncation");
        failed.send(Err(error)).unwrap();
        finish(&mut app, &ctx);
        assert_eq!(app.sound_status.0, "Playing · applause");
        assert_eq!(app.strip_contents().pads[0].error.as_deref(), Some(message.as_str()));
        let first = draw_board(&mut app, &ctx, Vec::new());
        assert!(first.platform_output.accesskit_update.unwrap().nodes.iter()
            .any(|(_, node)| node.label() == Some("Retry horn")));
        let pos = pad_icon_position(&mut app, &ctx, "horn");
        text_rect(&draw_board(&mut app, &ctx, Vec::new()), "Failed");
        draw_board(&mut app, &ctx, vec![egui::Event::PointerMoved(pos)]);
        let mut tooltip = false;
        for _ in 0..6 {
            let output = draw_board(&mut app, &ctx, Vec::new());
            tooltip |= output.shapes.iter().any(|shape| matches!(&shape.shape,
                egui::Shape::Text(text) if text.galley.text().contains(&message)));
        }
        assert!(tooltip, "hovering the pad exposes the full error including its cause");

        wav(dir.path(), "horn.wav");
        for pressed in [true, false] { draw_board(&mut app, &ctx, click(pos, egui::PointerButton::Primary, pressed)); }
        finish(&mut app, &ctx);
        assert!(app.clip_errors.is_empty() && app.playing.contains(&clip_key(&paths[0])));
        assert!(app.strip_contents().pads[0].error.is_none());
        let output = draw_board(&mut app, &ctx, vec![egui::Event::PointerMoved(egui::Pos2::ZERO)]);
        assert!(!output.shapes.iter().any(|shape| matches!(&shape.shape,
            egui::Shape::Text(text) if text.galley.text() == "Failed")));
    }

    #[test]
    fn changed_or_removed_files_cannot_play_or_publish_a_stale_waveform() {
        for removed in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let path = wav(dir.path(), "edited.wav");
            let mut app = app_with(std::slice::from_ref(&path));
            app.trim_clip = Some(path.clone());
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
            assert!(app.clip_errors.contains_key(&path));
            assert_eq!(app.sound_status.1, RED);

            if removed { replacement(); }
            app.play_clip(0, &ctx);
            finish(&mut app, &ctx);
            let clips = app.engine.as_ref().unwrap().take_clips();
            assert_eq!(clips.len(), 1);
            assert_eq!(clips[0].samples().len(), 120, "retry reads the replacement file");
            assert!(clips[0].samples().iter().all(|sample| (*sample - 0.5).abs() < 0.001));
            assert!(!app.clip_errors.contains_key(&path));
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
                assert!(done.send(Err(anyhow::anyhow!("late cancelled result"))).is_ok());
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
        let (done, cancel) = delayed(&mut app, std::slice::from_ref(&path));
        assert!(app.strip_contents().pads[0].loading, "the strip shows a pending pad");
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
        assert!(!app.has_pending_playback() && cancel.load(Ordering::Relaxed));
        assert!(!app.strip_contents().pads[0].loading);
        drop(done);
        finish(&mut app, &ctx);
    }

    #[test]
    #[ignore = "writes strip-to-board navigation screenshots; needs Python with Pillow"]
    fn snapshot_revealed_strip_pad() {
        let dir = tempfile::tempdir().unwrap();
        let paths: Vec<_> = (0..33).map(|i| dir.path().join(if i == 32 { "wanted.wav".into() } else { format!("clip-{i}.wav") })).collect();
        for (dark, theme) in [(true, "dark"), (false, "light")] {
            let mut app = app_with(&paths);
            app.settings.sounds[0].name = Some("wanted".into());
            app.settings.sounds[32].starred = true;
            app.clip_errors.insert(paths[32].clone(), "Reconnect the audio drive before retrying".into());
            app.pad_filter = "does not match the target".into();
            app.hidden = true;
            app.settings.collapsed = true;
            app.reveal_pad(32, &egui::Context::default());
            let image = super::super::snapshot::snapshot(&mut app, &format!("strip-revealed-pad-{theme}"), egui::vec2(700.0, 420.0), 1.25, dark);
            println!("SNAPSHOT {}", image.display());
        }
    }

    #[test]
    #[ignore = "writes loaded-playback duration screenshots; needs Python with Pillow"]
    fn snapshot_pad_playback_durations() {
        let dir = tempfile::tempdir().unwrap();
        let paths = [wav(dir.path(), "horn.wav"), wav(dir.path(), "rim.wav"), wav(dir.path(), "next.wav")];
        for (dark, theme) in [(true, "dark"), (false, "light")] {
            let mut app = app_with(&paths);
            app.settings.overlap_clips = true;
            for pad in &mut app.settings.sounds { pad.start = 0.25; pad.end = Some(0.75); }
            for path in &paths { app.cache_clip(path.clone(), CachedClip::read(path, &AtomicBool::new(false)).unwrap()); }
            let ctx = egui::Context::default();
            for index in 0..2 { app.play_clip(index, &ctx); finish(&mut app, &ctx); }
            app.toggle_clip(1, &ctx);
            let mut playing = app.engine.as_ref().unwrap().take_clips();
            for (index, clip) in playing.iter_mut().enumerate() { clip.pos = (index + 1) * 6_000; }
            app.engine.as_ref().unwrap().resume_clips(playing);
            for pad in &mut app.settings.sounds { pad.start = 0.1; pad.end = Some(0.3); }
            let image = super::super::snapshot::snapshot(&mut app, &format!("pad-loaded-durations-{theme}"), egui::vec2(700.0, 420.0), 1.25, dark);
            println!("SNAPSHOT {}", image.display());
        }
        let paths: Vec<_> = [("tap.wav", 1_920), ("tick.wav", 2), ("empty.wav", 1_920)].into_iter().map(|(name, count)| {
            let path = dir.path().join(name);
            crate::record::Recorder::from_samples_for_test(&vec![0.25; count]).finish().unwrap().unwrap().save_wav(&path).unwrap();
            path
        }).collect();
        for (dark, theme) in [(true, "dark"), (false, "light")] {
            let mut app = app_with(&paths);
            app.settings.overlap_clips = true;
            app.settings.sounds[0].start = 0.003;
            app.settings.sounds[0].end = Some(0.019);
            app.settings.sounds[2].start = 0.1;
            for path in &paths { app.cache_clip(path.clone(), CachedClip::read(path, &AtomicBool::new(false)).unwrap()); }
            let ctx = egui::Context::default();
            for index in 0..2 { app.play_clip(index, &ctx); }
            app.toggle_clip(1, &ctx);
            let mut playing = app.engine.as_ref().unwrap().take_clips();
            for clip in &mut playing { clip.pos = clip.samples().len() / 2; }
            app.engine.as_ref().unwrap().resume_clips(playing);
            let image = super::super::snapshot::snapshot(&mut app, &format!("pad-short-durations-{theme}"), egui::vec2(700.0, 420.0), 1.25, dark);
            println!("SNAPSHOT {}", image.display());
        }
    }

    #[test]
    #[ignore = "writes soundboard-failure screenshots; needs Python with Pillow"]
    fn snapshot_clip_load_failure() {
        let dir = tempfile::tempdir().unwrap();
        let paths = [dir.path().join("horn.wav"), wav(dir.path(), "applause.wav"), wav(dir.path(), "rim.wav")];
        for (dark, theme) in [(true, "dark"), (false, "light")] {
            let mut app = app_with(&paths);
            app.settings.overlap_clips = true;
            app.settings.sounds[0].starred = true;
            let (failed, _) = delayed(&mut app, &paths[..1]);
            failed.send(Err(anyhow::anyhow!("Reconnect the audio drive before retrying")
                .context(format!("Could not open {}", paths[0].display())))).unwrap();
            let ctx = egui::Context::default();
            app.play_clip(1, &ctx);
            finish(&mut app, &ctx);
            let (_loading, _) = delayed(&mut app, &paths[2..]);
            app.sound_status = ("Loading · rim".into(), CYAN);
            let image = super::super::snapshot::snapshot(&mut app, &format!("soundboard-failed-{theme}"), egui::vec2(700.0, 420.0), 1.25, dark);
            println!("SNAPSHOT {}", image.display());
            let pos = pad_icon_position(&mut app, &ctx, "horn");
            let image = super::super::snapshot::snapshot_with_events(
                &mut app, &format!("soundboard-error-details-{theme}"), egui::vec2(700.0, 420.0), 1.25, dark,
                vec![vec![egui::Event::PointerMoved(pos)]],
            );
            println!("SNAPSHOT {}", image.display());
        }
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
