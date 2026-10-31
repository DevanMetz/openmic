//! "Test my mic": record a few seconds, then play them back raw and as
//! Discord would hear them, on your own headphones or speakers. Cleaning
//! uses the current settings each time, so you can tune and listen again.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, TryRecvError};
use eframe::egui::{self, RichText};

use super::{AMBER, App, CYAN, GREEN, MUTED, RED, Status, short};
use crate::denoise::{Cleaner, ModelState, ProcessedRecording};
use crate::dsp::Params;
use crate::playback::{self, Player};
use crate::record::{Recorder, Source};
use crate::widgets;

/// How long a test recording runs.
pub(super) const TEST_LENGTH: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Version {
    Raw,
    Cleaned,
}

struct Cleaning {
    result: Receiver<ProcessedRecording>,
    raw: Arc<Vec<f32>>,
    params: Params,
    then_play: bool,
    report_status: bool,
}

struct Loading {
    result: Receiver<anyhow::Result<Option<Vec<f32>>>>,
    cancel: Arc<AtomicBool>,
}

impl Drop for Loading {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

#[derive(Default)]
pub(super) struct MicTest {
    recording: Option<(Recorder, Instant)>,
    /// Loading the finalized take off the UI thread before exposing previews.
    loading: Option<Loading>,
    /// The take as recorded (mono, DSP rate).
    raw: Option<Arc<Vec<f32>>>,
    /// The cleaned take, its settings, and the model state that produced it.
    cleaned: Option<(Arc<Vec<f32>>, Params, ModelState)>,
    /// Cleaning in the background, and whether to play it when done.
    cleaning: Option<Cleaning>,
    player: Option<(Player, Version)>,
    status: Option<Status>,
    /// Focus recording after cancellation; keyboard input also reveals it.
    record_focus: Option<bool>,
}

impl MicTest {
    pub(super) fn recording(&self) -> bool {
        self.recording.is_some() || self.loading.is_some()
    }

    pub(super) fn busy(&self) -> bool {
        self.recording() || self.cleaning.is_some() || self.player.is_some()
    }

    fn used_fallback(&self) -> bool {
        self.cleaned.as_ref().is_some_and(|(_, params, model)| {
            !params.bypass && *model == ModelState::DeepFilterFailed
        })
    }
}

impl App {
    /// The settings a test is cleaned with: yours, but never muted (a muted
    /// test would play silence).
    fn test_params(&self) -> Params {
        // Monitor and soundboard controls do not affect the voice chain;
        // keep them fixed so changing them does not invalidate its cache.
        Params {
            mute: false,
            monitor_on: false,
            monitor_clips: false,
            monitor_gain: 1.0,
            sound_gain: 1.0,
            ..self.params()
        }
    }

    /// Where the test plays: the headphone monitor if one is chosen,
    /// otherwise the Windows default output, but never VB-Cable.
    fn test_output(&self) -> String {
        let monitor = &self.settings.monitor_output;
        if !monitor.is_empty()
            && self.outputs.contains(monitor)
            && *monitor != self.settings.output
            && !playback::is_cable(monitor)
        {
            monitor.clone()
        } else {
            String::new()
        }
    }

    pub(super) fn start_mic_test(&mut self) {
        if self.mic_test.recording() {
            return;
        }
        self.stop_test();
        // Keep track of the worker until it finishes so a new take cannot
        // start a competing cleaner. Its source identifies stale results.
        match Recorder::start(Source::Microphone, &self.settings.microphone) {
            Ok(recorder) => {
                self.mic_test.recording = Some((recorder, Instant::now()));
                self.mic_test.status = Some(("Speak normally…".into(), AMBER));
            }
            Err(e) => self.mic_test.status = Some((short(&format!("{e:#}")), RED)),
        }
    }

    fn clean_test(&mut self, then_play: bool) {
        let Some(raw) = self.mic_test.raw.clone() else {
            return;
        };
        let params = self.test_params();
        if then_play {
            self.mic_test.status = Some(("Applying your settings…".into(), CYAN));
        }
        if let Some(cleaning) = &mut self.mic_test.cleaning {
            // Finish existing work first. Completion rechecks the take and,
            // when playing, the settings before using its result.
            cleaning.then_play |= then_play;
            cleaning.report_status = true;
            return;
        }
        let (done, job) = crossbeam_channel::bounded(1);
        let samples = Arc::clone(&raw);
        let spawned = std::thread::Builder::new()
            .name("openmic-mic-test".into())
            .spawn(move || {
                let _ = done.send(Cleaner::process_recording(&samples, &params));
            });
        match spawned {
            Ok(_) => {
                self.mic_test.cleaning = Some(Cleaning {
                    result: job,
                    raw,
                    params,
                    then_play,
                    report_status: true,
                })
            }
            Err(e) => self.mic_test.status = Some((short(&format!("{e:#}")), RED)),
        }
    }

    fn stop_test(&mut self) {
        self.mic_test.player = None;
        if let Some(cleaning) = &mut self.mic_test.cleaning {
            cleaning.then_play = false;
            // Completion must not replace a newer recording/playback status.
            cleaning.report_status = false;
        }
    }

    fn cancel_mic_test(&mut self) {
        if !self.mic_test.recording() {
            return;
        }
        self.stop_test();
        self.mic_test.recording = None;
        self.mic_test.loading = None;
        self.mic_test.status = Some(("Mic test cancelled".into(), MUTED));
    }

    fn play_test(&mut self, version: Version) {
        if self.mic_test.recording() {
            return;
        }
        self.stop_test();
        let samples = match version {
            Version::Raw => self.mic_test.raw.clone(),
            Version::Cleaned => match &self.mic_test.cleaned {
                Some((cleaned, params, _)) if *params == self.test_params() => {
                    Some(Arc::clone(cleaned))
                }
                // Settings changed since it was cleaned: clean again, then play.
                _ => {
                    self.clean_test(true);
                    return;
                }
            },
        };
        let Some(samples) = samples else { return };
        match Player::play(&self.test_output(), &self.settings.output, &samples) {
            Ok(player) => {
                self.mic_test.player = Some((player, version));
                let fallback = version == Version::Cleaned && self.mic_test.used_fallback();
                self.mic_test.status = Some((
                    match version {
                        Version::Raw => "Playing your raw microphone".into(),
                        Version::Cleaned if fallback => {
                            "Playing RNNoise fallback; DeepFilterNet unavailable".into()
                        }
                        Version::Cleaned => "Playing what Discord hears".into(),
                    },
                    if fallback { AMBER } else { GREEN },
                ));
            }
            Err(e) => self.mic_test.status = Some((short(&format!("{e:#}")), RED)),
        }
    }

    /// Advance the test: finish the recording, collect cleaning, notice the
    /// end of playback.
    pub(super) fn poll_mic_test(&mut self) {
        let test = &mut self.mic_test;
        if let Some((recorder, started)) = &mut test.recording {
            if !recorder.is_stopping() {
                if let Some(e) = recorder.error() {
                    recorder.stop_capture();
                    test.status = Some((short(&e), RED));
                } else if started.elapsed() >= TEST_LENGTH {
                    recorder.stop_capture();
                    test.status = Some(("Finishing mic test…".into(), CYAN));
                }
            }
            if recorder.is_stopping() && recorder.ready_to_finish() {
                let (recorder, _) = test.recording.take().expect("checked above");
                if let Some(e) = recorder.error() {
                    test.status = Some((short(&e), RED));
                } else {
                    let (done, job) = crossbeam_channel::bounded(1);
                    let cancel = Arc::new(AtomicBool::new(false));
                    let worker_cancel = Arc::clone(&cancel);
                    let spawned = std::thread::Builder::new()
                        .name("openmic-test-load".into())
                        .spawn(move || {
                            if worker_cancel.load(Ordering::Relaxed) {
                                return;
                            }
                            let take = recorder.finish().and_then(|take| {
                                take.map(|take| {
                                    if let Some(warning) = take.warning() {
                                        anyhow::bail!("Record the mic test again: {warning}");
                                    }
                                    take.samples_cancellable(&worker_cancel)
                                })
                                .transpose()
                            });
                            let _ = done.send(take);
                        });
                    match spawned {
                        Ok(_) => {
                            test.loading = Some(Loading {
                                result: job,
                                cancel,
                            })
                        }
                        Err(e) => {
                            test.status =
                                Some((short(&format!("Could not load the mic test: {e}")), RED))
                        }
                    }
                }
            }
        }
        let finished = test
            .loading
            .as_ref()
            .and_then(|loading| match loading.result.try_recv() {
                Err(TryRecvError::Empty) => None,
                result => Some(result.unwrap_or_else(|_| {
                    Err(anyhow::anyhow!("Could not load the mic test; record again"))
                })),
            });
        if let Some(result) = finished {
            test.loading = None;
            match result {
                Ok(Some(samples)) if !samples.is_empty() => {
                    test.raw = Some(Arc::new(samples));
                    test.cleaned = None;
                    test.status = Some(("Now compare: Raw, then Cleaned".into(), GREEN));
                    self.clean_test(false);
                }
                Ok(_) => test.status = Some(("Nothing was recorded".into(), RED)),
                Err(e) => test.status = Some((short(&format!("{e:#}")), RED)),
            }
        }
        let test = &mut self.mic_test;
        let finished =
            test.cleaning
                .as_ref()
                .and_then(|cleaning| match cleaning.result.try_recv() {
                    Err(TryRecvError::Empty) => None,
                    result => Some(result),
                });
        if let Some(result) = finished {
            let cleaning = test.cleaning.take().expect("checked above");
            if !test
                .raw
                .as_ref()
                .is_some_and(|raw| Arc::ptr_eq(raw, &cleaning.raw))
            {
                // A newer take is waiting. Never cache or play the old result,
                // and only start the next cleaner after this one has finished.
                if !test.recording() {
                    self.clean_test(cleaning.then_play);
                    if let Some(next) = &mut self.mic_test.cleaning {
                        next.report_status = cleaning.report_status;
                    }
                }
            } else {
                match result {
                    Ok((cleaned, model)) => {
                        test.cleaned = Some((Arc::new(cleaned), cleaning.params, model));
                        if !test.recording() {
                            if cleaning.then_play {
                                self.play_test(Version::Cleaned);
                            } else if cleaning.report_status && test.used_fallback() {
                                test.status = Some((
                                    "Cleaned uses RNNoise; DeepFilterNet unavailable".into(),
                                    AMBER,
                                ));
                            }
                        }
                    }
                    Err(_) if cleaning.report_status && !test.recording() => {
                        test.status = Some((
                            "Could not clean the mic test; click Cleaned to try again".into(),
                            RED,
                        ));
                    }
                    Err(_) => {}
                }
            }
        }
        let error = self
            .mic_test
            .player
            .as_ref()
            .and_then(|(player, _)| player.take_error());
        if let Some(error) = error {
            self.finish_test_playback(Err(error));
        } else if self
            .mic_test
            .player
            .as_ref()
            .is_some_and(|(player, _)| player.finished())
        {
            self.finish_test_playback(Ok(()));
        }
    }

    fn finish_test_playback(&mut self, result: Result<(), String>) {
        self.mic_test.player = None;
        match result {
            Ok(()) => {
                self.mic_test.status = Some(if self.mic_test.used_fallback() {
                    (
                        "Cleaned uses RNNoise; DeepFilterNet unavailable".into(),
                        AMBER,
                    )
                } else {
                    ("Tune the settings and listen again".into(), MUTED)
                });
                if !self.settings.setup.tested {
                    self.settings.setup.tested = true;
                    self.touch();
                }
            }
            Err(error) => {
                crate::logfile::error(format_args!("mic test playback stopped: {error}"));
                self.mic_test.status = Some((short(&format!("Playback stopped: {error}")), RED));
            }
        }
    }

    /// The test's controls wrap to fit the current window width.
    pub(super) fn draw_mic_test(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            if let Some((recorder, started)) = &self.mic_test.recording {
                if recorder.is_stopping() {
                    ui.spinner();
                } else {
                    let left = TEST_LENGTH.saturating_sub(started.elapsed()).as_secs_f32();
                    ui.add(egui::ProgressBar::new(1.0 - left / TEST_LENGTH.as_secs_f32()).desired_width(120.0).text(format!("{left:.0} s")));
                }
            } else if self.mic_test.loading.is_some() {
                ui.spinner();
            } else {
                let record = ui.button(if self.mic_test.raw.is_some() { "Record again" } else { "Test my mic" })
                    .on_hover_text("Record 5 seconds, then hear it raw and cleaned on your headphones or speakers");
                let reveal = self.mic_test.record_focus.take();
                if reveal.is_some() { record.request_focus(); }
                if reveal != Some(false) { widgets::scroll_on_focus(&record); }
                if record.clicked() { self.start_mic_test(); }
            }
            if self.mic_test.recording() {
                let cancel = ui.small_button("Cancel test").on_hover_text("Discard this mic test; keep the previous preview");
                widgets::scroll_on_focus(&cancel);
                if cancel.clicked() {
                    self.cancel_mic_test();
                    self.mic_test.record_focus = Some(ui.input(|input| !input.pointer.any_down() && !input.pointer.any_released()));
                    ui.ctx().request_repaint();
                }
            }
            if self.mic_test.raw.is_some() && !self.mic_test.recording() {
                for (version, label) in [(Version::Raw, "⏵ Raw"), (Version::Cleaned, "⏵ Cleaned")] {
                    let playing = self.mic_test.player.as_ref().is_some_and(|(_, v)| *v == version)
                        || (version == Version::Cleaned && self.mic_test.cleaning.as_ref().is_some_and(|cleaning| cleaning.then_play));
                    let preview = ui.selectable_label(playing, label)
                        .on_hover_text(if playing { "Stop preview" } else { "Play preview" });
                    widgets::scroll_on_focus(&preview);
                    if preview.clicked() {
                        if playing {
                            self.stop_test();
                            self.mic_test.status = Some(("Playback stopped".into(), MUTED));
                        } else {
                            self.play_test(version);
                        }
                    }
                }
                if let Some((player, _)) = &self.mic_test.player {
                    ui.add(egui::ProgressBar::new(player.progress()).desired_width(60.0));
                } else if self.mic_test.cleaning.is_some() {
                    ui.spinner();
                }
            }
            if let Some((text, color)) = &self.mic_test.status {
                ui.label(RichText::new(text).small().color(widgets::text_color(ui, *color)));
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Settings;

    fn pending_cleanup(
        app: &mut App,
        then_play: bool,
    ) -> crossbeam_channel::Sender<ProcessedRecording> {
        let params = app.test_params();
        let raw = Arc::clone(
            app.mic_test
                .raw
                .get_or_insert_with(|| Arc::new(vec![0.1; 480])),
        );
        let (done, job) = crossbeam_channel::bounded(1);
        app.mic_test.cleaning = Some(Cleaning {
            result: job,
            raw,
            params,
            then_play,
            report_status: true,
        });
        done
    }

    fn draw_mic_frame(
        app: &mut App,
        ctx: &egui::Context,
        size: egui::Vec2,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        use eframe::App as _;
        let mut frame = eframe::Frame::_new_kittest();
        ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                time: Some(ctx.cumulative_frame_nr() as f64 * 0.2),
                events,
                ..Default::default()
            },
            |ui| app.ui(ui, &mut frame),
        )
    }

    fn mic_preview_app() -> App {
        let mut app = App::stopped(Settings::default());
        app.settings.setup.dismissed = true;
        app.settings.microphone = "Test microphone".into();
        app.settings.output = "CABLE Input (VB-Audio Virtual Cable)".into();
        app.inputs = vec![app.settings.microphone.clone()];
        app.outputs = vec![app.settings.output.clone(), "Headphones".into()];
        app.mic_test.raw = Some(Arc::new(vec![0.1; 480]));
        app.mic_test.cleaned = Some((
            Arc::new(vec![0.05; 480]),
            app.test_params(),
            ModelState::DeepFilter,
        ));
        app
    }

    #[test]
    fn mic_test_controls_fit_and_scroll_into_view_at_zoom() {
        use egui::accesskit::{Action, ActionRequest, Role, TreeId};
        let size = egui::vec2(700.0, 420.0) / 1.5;
        for dark in [true, false] {
            for passes in [1, 2] {
                for assistive in [false, true] {
                    for stage in [
                        "idle",
                        "ready",
                        "cleanup",
                        "recording",
                        "finishing",
                        "loading",
                    ] {
                        let mut app = App::stopped(Settings::default());
                        app.settings.setup.dismissed = true;
                        app.settings.microphone = "Test microphone".into();
                        app.settings.output = "CABLE Input (VB-Audio Virtual Cable)".into();
                        app.inputs = vec![app.settings.microphone.clone()];
                        app.outputs = vec![app.settings.output.clone(), "Headphones".into()];
                        let raw = Arc::new(vec![0.1; 480]);
                        let cleaned = Arc::new(vec![0.05; 480]);
                        if stage != "idle" {
                            app.mic_test.raw = Some(Arc::clone(&raw));
                            app.mic_test.cleaned = Some((
                                Arc::clone(&cleaned),
                                app.test_params(),
                                ModelState::DeepFilter,
                            ));
                        }
                        let (_done, job) = crossbeam_channel::bounded(1);
                        let (_loaded, load) = crossbeam_channel::bounded(1);
                        match stage {
                            "cleanup" => {
                                app.mic_test.cleaning = Some(Cleaning {
                                    result: job,
                                    raw: Arc::clone(&raw),
                                    params: app.test_params(),
                                    then_play: false,
                                    report_status: true,
                                })
                            }
                            "recording" | "finishing" => {
                                let mut recorder = Recorder::from_samples_for_test(&[0.25; 480]);
                                if stage == "finishing" {
                                    recorder.stop_capture();
                                }
                                app.mic_test.recording = Some((recorder, Instant::now()));
                            }
                            "loading" => {
                                app.mic_test.loading = Some(Loading {
                                    result: load,
                                    cancel: Arc::new(AtomicBool::new(false)),
                                })
                            }
                            _ => {}
                        }
                        app.mic_test.status = Some((
                            "Compare Raw and Cleaned, then change your settings and listen again."
                                .into(),
                            MUTED,
                        ));
                        let original = serde_json::to_value(&app.settings).unwrap();
                        let ctx = egui::Context::default();
                        ctx.options_mut(|options| {
                            options.max_passes = passes.try_into().unwrap();
                            options.zoom_factor = 1.5;
                        });
                        ctx.set_visuals(if dark {
                            egui::Visuals::dark()
                        } else {
                            egui::Visuals::light()
                        });
                        widgets::configure_text_styles(&ctx);
                        ctx.enable_accesskit();
                        for _ in 0..3 {
                            draw_mic_frame(&mut app, &ctx, size, Vec::new());
                        }
                        let labels: &[&str] = match stage {
                            "idle" => &["Bypass", "Test my mic"],
                            "ready" | "cleanup" => {
                                &["Bypass", "Record again", "⏵ Raw", "⏵ Cleaned"]
                            }
                            _ => &["Bypass", "Cancel test"],
                        };
                        for &label in labels {
                            let output = draw_mic_frame(&mut app, &ctx, size, Vec::new());
                            let target = *output
                                .platform_output
                                .accesskit_update
                                .unwrap()
                                .nodes
                                .iter()
                                .find(|(_, node)| {
                                    matches!(node.role(), Role::Button | Role::CheckBox)
                                        && node.label() == Some(label)
                                })
                                .map(|(id, _)| id)
                                .unwrap();
                            let key = |modifiers| egui::Event::Key {
                                key: egui::Key::Tab,
                                physical_key: None,
                                pressed: true,
                                repeat: false,
                                modifiers,
                            };
                            if assistive {
                                draw_mic_frame(
                                    &mut app,
                                    &ctx,
                                    size,
                                    vec![egui::Event::AccessKitActionRequest(ActionRequest {
                                        action: Action::Focus,
                                        target_tree: TreeId::ROOT,
                                        target_node: target,
                                        data: None,
                                    })],
                                );
                            } else {
                                let mut reached = false;
                                for _ in 0..80 {
                                    let output = draw_mic_frame(
                                        &mut app,
                                        &ctx,
                                        size,
                                        vec![key(egui::Modifiers::NONE)],
                                    );
                                    if output.platform_output.accesskit_update.unwrap().focus
                                        == target
                                    {
                                        reached = true;
                                        break;
                                    }
                                }
                                assert!(reached, "Tab reaches {label} during {stage}");
                            }
                            for modifiers in [
                                None,
                                Some(egui::Modifiers::SHIFT),
                                Some(egui::Modifiers::NONE),
                            ] {
                                if let Some(modifiers) = modifiers {
                                    draw_mic_frame(&mut app, &ctx, size, vec![key(modifiers)]);
                                }
                                for _ in 0..8 {
                                    draw_mic_frame(&mut app, &ctx, size, Vec::new());
                                }
                            }
                            let output = draw_mic_frame(&mut app, &ctx, size, Vec::new());
                            let tree = output.platform_output.accesskit_update.as_ref().unwrap();
                            assert_eq!(tree.focus, target);
                            let bounds = tree
                                .nodes
                                .iter()
                                .find(|(id, _)| *id == target)
                                .unwrap()
                                .1
                                .bounds()
                                .unwrap();
                            let rect = egui::Rect::from_min_max(
                                egui::pos2(bounds.x0 as f32, bounds.y0 as f32),
                                egui::pos2(bounds.x1 as f32, bounds.y1 as f32),
                            );
                            assert!(output.shapes.iter().any(|shape| shape.clip_rect.contains_rect(rect)
                                && matches!(&shape.shape, egui::Shape::Rect(shape) if shape.rect.intersects(rect))),
                                "{label} is visible during {stage}; passes={passes}, assistive={assistive}: {rect:?}");
                            assert_eq!(serde_json::to_value(&app.settings).unwrap(), original);
                            assert!(app.dirty_since.is_none() && app.mic_test.player.is_none());
                            if stage != "idle" {
                                assert!(Arc::ptr_eq(app.mic_test.raw.as_ref().unwrap(), &raw));
                                assert!(Arc::ptr_eq(
                                    &app.mic_test.cleaned.as_ref().unwrap().0,
                                    &cleaned
                                ));
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn cancelling_a_mic_test_returns_focus_to_record_without_playing_the_preview() {
        use egui::accesskit::{Action, ActionRequest, Role, TreeId};
        let size = egui::vec2(700.0, 420.0) / 1.5;
        for dark in [true, false] {
            for passes in [1, 2] {
                for activation in ["keyboard", "assistive", "pointer"] {
                    let mut app = App::stopped(Settings::default());
                    app.settings.setup.dismissed = true;
                    app.settings.output = "CABLE Input (VB-Audio Virtual Cable)".into();
                    app.outputs = vec![app.settings.output.clone()];
                    let raw = Arc::new(vec![0.1; 480]);
                    app.mic_test.raw = Some(Arc::clone(&raw));
                    let (loaded, result) = crossbeam_channel::bounded(1);
                    let cancel = Arc::new(AtomicBool::new(false));
                    app.mic_test.loading = Some(Loading {
                        result,
                        cancel: Arc::clone(&cancel),
                    });
                    let original = serde_json::to_value(&app.settings).unwrap();
                    let ctx = egui::Context::default();
                    ctx.options_mut(|options| {
                        options.max_passes = passes.try_into().unwrap();
                        options.zoom_factor = 1.5;
                    });
                    ctx.set_visuals(if dark {
                        egui::Visuals::dark()
                    } else {
                        egui::Visuals::light()
                    });
                    widgets::configure_text_styles(&ctx);
                    ctx.enable_accesskit();
                    for _ in 0..3 {
                        draw_mic_frame(&mut app, &ctx, size, Vec::new());
                    }
                    let output = draw_mic_frame(&mut app, &ctx, size, Vec::new());
                    let target = *output
                        .platform_output
                        .accesskit_update
                        .unwrap()
                        .nodes
                        .iter()
                        .find(|(_, node)| {
                            node.role() == Role::Button && node.label() == Some("Cancel test")
                        })
                        .map(|(id, _)| id)
                        .unwrap();
                    let action = |action| {
                        egui::Event::AccessKitActionRequest(ActionRequest {
                            action,
                            target_tree: TreeId::ROOT,
                            target_node: target,
                            data: None,
                        })
                    };
                    draw_mic_frame(&mut app, &ctx, size, vec![action(Action::Focus)]);
                    for _ in 0..8 {
                        draw_mic_frame(&mut app, &ctx, size, Vec::new());
                    }
                    let routing_position = |output: &egui::FullOutput| {
                        output
                            .platform_output
                            .accesskit_update
                            .as_ref()
                            .unwrap()
                            .nodes
                            .iter()
                            .find(|(_, node)| {
                                node.role() == Role::Label && node.value() == Some("ROUTING")
                            })
                            .unwrap()
                            .1
                            .bounds()
                            .unwrap()
                            .y0
                    };
                    let before =
                        routing_position(&draw_mic_frame(&mut app, &ctx, size, Vec::new()));
                    if activation == "pointer" {
                        let output = draw_mic_frame(&mut app, &ctx, size, Vec::new());
                        let bounds = output
                            .platform_output
                            .accesskit_update
                            .unwrap()
                            .nodes
                            .iter()
                            .find(|(id, _)| *id == target)
                            .unwrap()
                            .1
                            .bounds()
                            .unwrap();
                        let pos = egui::pos2(
                            (bounds.x0 + bounds.x1) as f32 * 0.5,
                            (bounds.y0 + bounds.y1) as f32 * 0.5,
                        );
                        for pressed in [true, false] {
                            draw_mic_frame(
                                &mut app,
                                &ctx,
                                size,
                                vec![
                                    egui::Event::PointerMoved(pos),
                                    egui::Event::PointerButton {
                                        pos,
                                        button: egui::PointerButton::Primary,
                                        pressed,
                                        modifiers: egui::Modifiers::NONE,
                                    },
                                ],
                            );
                        }
                    } else {
                        let event = if activation == "assistive" {
                            action(Action::Click)
                        } else {
                            egui::Event::Key {
                                key: egui::Key::Enter,
                                physical_key: None,
                                pressed: true,
                                repeat: false,
                                modifiers: egui::Modifiers::NONE,
                            }
                        };
                        draw_mic_frame(&mut app, &ctx, size, vec![event]);
                    }
                    for _ in 0..8 {
                        draw_mic_frame(&mut app, &ctx, size, Vec::new());
                    }
                    let output = draw_mic_frame(&mut app, &ctx, size, Vec::new());
                    if activation == "pointer" {
                        assert_eq!(
                            routing_position(&output),
                            before,
                            "mouse cancellation keeps the page's scroll position"
                        );
                    }
                    let tree = output.platform_output.accesskit_update.unwrap();
                    let record = *tree
                        .nodes
                        .iter()
                        .find(|(_, node)| {
                            node.role() == Role::Button && node.label() == Some("Record again")
                        })
                        .map(|(id, _)| id)
                        .unwrap();
                    assert_eq!(
                        tree.focus, record,
                        "{activation} cancellation returns focus to recording; passes={passes}"
                    );
                    assert!(cancel.load(Ordering::Relaxed));
                    assert!(loaded.send(Ok(Some(vec![0.9; 480]))).is_err());
                    assert!(Arc::ptr_eq(app.mic_test.raw.as_ref().unwrap(), &raw));
                    assert!(!app.mic_test.busy() && app.mic_test.player.is_none());
                    assert_eq!(
                        app.mic_test.status,
                        Some(("Mic test cancelled".into(), MUTED))
                    );
                    assert_eq!(serde_json::to_value(&app.settings).unwrap(), original);
                    assert!(app.dirty_since.is_none());
                }
            }
        }
    }

    #[test]
    fn mic_preview_focus_survives_cleanup_and_page_changes() {
        use egui::accesskit::{Action, ActionRequest, Role, TreeId};
        let size = egui::vec2(700.0, 420.0) / 1.5;
        for dark in [true, false] {
            for passes in [1, 2] {
                for scene in ["cleanup result", "checklist completion", "preset added"] {
                    for label in ["Bypass", "Record again", "⏵ Raw", "⏵ Cleaned"] {
                        let mut app = mic_preview_app();
                        app.settings.setup.dismissed = false;
                        let raw = Arc::clone(app.mic_test.raw.as_ref().unwrap());
                        let done = pending_cleanup(&mut app, false);
                        let ctx = egui::Context::default();
                        ctx.options_mut(|options| {
                            options.max_passes = passes.try_into().unwrap();
                            options.zoom_factor = 1.5;
                        });
                        ctx.set_visuals(if dark {
                            egui::Visuals::dark()
                        } else {
                            egui::Visuals::light()
                        });
                        widgets::configure_text_styles(&ctx);
                        ctx.enable_accesskit();
                        for _ in 0..3 {
                            draw_mic_frame(&mut app, &ctx, size, Vec::new());
                        }
                        let output = draw_mic_frame(&mut app, &ctx, size, Vec::new());
                        let target = *output
                            .platform_output
                            .accesskit_update
                            .unwrap()
                            .nodes
                            .iter()
                            .find(|(_, node)| {
                                matches!(node.role(), Role::Button | Role::CheckBox)
                                    && node.label() == Some(label)
                            })
                            .map(|(id, _)| id)
                            .unwrap();
                        draw_mic_frame(
                            &mut app,
                            &ctx,
                            size,
                            vec![egui::Event::AccessKitActionRequest(ActionRequest {
                                action: Action::Focus,
                                target_tree: TreeId::ROOT,
                                target_node: target,
                                data: None,
                            })],
                        );
                        for _ in 0..8 {
                            draw_mic_frame(&mut app, &ctx, size, Vec::new());
                        }
                        match scene {
                            "cleanup result" => {
                                done.send((vec![0.05; 480], ModelState::DeepFilterFailed))
                                    .unwrap();
                                app.poll_mic_test();
                            }
                            "checklist completion" => {
                                app.settings.setup.discord = true;
                                app.settings.setup.tested = true;
                            }
                            "preset added" => app.settings.presets.push(crate::config::Preset {
                                name: "Desk".into(),
                                processing: app.settings.processing(),
                            }),
                            _ => unreachable!(),
                        }
                        let original = serde_json::to_value(&app.settings).unwrap();
                        for _ in 0..8 {
                            draw_mic_frame(&mut app, &ctx, size, Vec::new());
                        }
                        let output = draw_mic_frame(&mut app, &ctx, size, Vec::new());
                        let tree = output.platform_output.accesskit_update.as_ref().unwrap();
                        assert_eq!(
                            tree.focus, target,
                            "{label} keeps focus after {scene}; passes={passes}"
                        );
                        assert!(
                            tree.nodes
                                .iter()
                                .any(|(id, node)| *id == target && node.label() == Some(label))
                        );
                        assert!(Arc::ptr_eq(app.mic_test.raw.as_ref().unwrap(), &raw));
                        assert_eq!(serde_json::to_value(&app.settings).unwrap(), original);
                        assert!(app.dirty_since.is_none() && app.mic_test.player.is_none());
                    }
                }
            }
        }
    }

    #[test]
    #[ignore = "writes mic-test focus screenshots; needs Python with Pillow"]
    fn snapshot_mic_test_keyboard_focus() {
        use egui::accesskit::{Action, ActionRequest, Role, TreeId};
        for (zoom, scale) in [(1.0, "standard"), (1.5, "zoomed")] {
            let size = egui::vec2(700.0, 420.0) / zoom;
            for (dark, theme) in [(true, "dark"), (false, "light")] {
                for (stage, label) in [
                    ("idle", "Test my mic"),
                    ("raw", "⏵ Raw"),
                    ("pending", "⏵ Cleaned"),
                    ("no-cable", "Record again"),
                    ("custom-output", "Record again"),
                    ("recording", "Cancel test"),
                    ("finishing", "Cancel test"),
                    ("loading", "Cancel test"),
                    ("cancelled", "Cancel test"),
                ] {
                    let mut app = mic_preview_app();
                    let (_done, result) = crossbeam_channel::bounded(1);
                    let (_loaded, load) = crossbeam_channel::bounded(1);
                    app.mic_test.status = Some(("Now compare: Raw, then Cleaned".into(), GREEN));
                    match stage {
                        "idle" => {
                            app.mic_test.raw = None;
                            app.mic_test.cleaned = None;
                            app.mic_test.status = None;
                        }
                        "no-cable" => app.outputs = vec!["Headphones".into()],
                        "custom-output" => app.settings.output = "Headphones".into(),
                        "pending" => {
                            app.mic_test.cleaning = Some(Cleaning {
                                result,
                                raw: Arc::clone(app.mic_test.raw.as_ref().unwrap()),
                                params: app.test_params(),
                                then_play: true,
                                report_status: true,
                            });
                            app.mic_test.status = Some(("Applying your settings…".into(), CYAN));
                        }
                        "recording" | "finishing" => {
                            let mut recorder = Recorder::from_samples_for_test(&[0.25; 480]);
                            if stage == "finishing" {
                                recorder.stop_capture();
                            }
                            app.mic_test.recording = Some((recorder, Instant::now()));
                            app.mic_test.status = Some(if stage == "finishing" {
                                ("Finishing mic test…".into(), CYAN)
                            } else {
                                ("Speak normally…".into(), AMBER)
                            });
                        }
                        "loading" | "cancelled" => {
                            app.mic_test.loading = Some(Loading {
                                result: load,
                                cancel: Arc::new(AtomicBool::new(false)),
                            });
                            app.mic_test.status = Some(("Finishing mic test…".into(), CYAN));
                        }
                        _ => {}
                    }
                    let original = serde_json::to_value(&app.settings).unwrap();
                    let ctx = egui::Context::default();
                    ctx.options_mut(|options| options.zoom_factor = zoom);
                    ctx.set_visuals(if dark {
                        egui::Visuals::dark()
                    } else {
                        egui::Visuals::light()
                    });
                    widgets::configure_text_styles(&ctx);
                    ctx.enable_accesskit();
                    for _ in 0..3 {
                        draw_mic_frame(&mut app, &ctx, size, Vec::new());
                    }
                    let output = draw_mic_frame(&mut app, &ctx, size, Vec::new());
                    let target = *output
                        .platform_output
                        .accesskit_update
                        .unwrap()
                        .nodes
                        .iter()
                        .find(|(_, node)| {
                            node.role() == Role::Button && node.label() == Some(label)
                        })
                        .map(|(id, _)| id)
                        .unwrap();
                    let action = |action| {
                        egui::Event::AccessKitActionRequest(ActionRequest {
                            action,
                            target_tree: TreeId::ROOT,
                            target_node: target,
                            data: None,
                        })
                    };
                    let mut events = vec![Vec::new(); 3];
                    events.push(vec![action(Action::Focus)]);
                    events.extend(vec![Vec::new(); 8]);
                    if stage == "cancelled" {
                        events.push(vec![action(Action::Click)]);
                    }
                    let image = super::super::snapshot::snapshot_with_events(
                        &mut app,
                        &format!("mic-test-keyboard-{stage}-{scale}-{theme}"),
                        size,
                        zoom,
                        dark,
                        events,
                    );
                    println!("SNAPSHOT {}", image.display());
                    assert_eq!(serde_json::to_value(&app.settings).unwrap(), original);
                    assert!(app.dirty_since.is_none() && app.mic_test.player.is_none());
                }
            }
        }
    }

    #[test]
    fn voice_controls_keep_focus_when_mic_results_and_page_sections_change() {
        use eframe::App as _;
        use egui::accesskit::{Action, ActionRequest, Role, TreeId};
        let controls = [
            (Role::Slider, "Your mic"),
            (Role::Slider, "Remove rumble"),
            (Role::Slider, "Remove noise"),
            (Role::Slider, "Only my voice"),
            (Role::Slider, "Quiet cut-off"),
            (Role::Slider, "To Discord"),
            (Role::Slider, "Hear yourself"),
            (Role::CheckBox, "Remove rumble"),
            (Role::CheckBox, "Remove noise"),
            (Role::CheckBox, "Only my voice"),
            (Role::CheckBox, "Quiet cut-off"),
            (Role::CheckBox, "Hear yourself"),
            (Role::Link, "DeepFilterNet"),
        ];
        for dark in [true, false] {
            for scene in ["cleanup result", "checklist completion", "bypass message"] {
                for (role, label) in controls {
                    let mut app = App::stopped(Settings {
                        microphone: "Test microphone".into(),
                        output: "CABLE Input (VB-Audio Virtual Cable)".into(),
                        input_gain_db: 3.37,
                        highpass_hz: 137.25,
                        strength: 0.74321,
                        voice_threshold: 0.61347,
                        gate_threshold_db: -43.27,
                        output_gain_db: -2.33,
                        monitor_volume: 0.43561,
                        ..Settings::default()
                    });
                    app.settings.setup.discord = true;
                    app.settings.setup.tested = true;
                    app.inputs = vec![app.settings.microphone.clone()];
                    assert!(
                        app.show_checklist(),
                        "only the cable setup step is unfinished"
                    );
                    let done =
                        (scene == "cleanup result").then(|| pending_cleanup(&mut app, false));
                    let original = app.settings.processing();
                    let original_monitor = app.settings.monitor_volume;
                    let ctx = egui::Context::default();
                    ctx.set_visuals(if dark {
                        egui::Visuals::dark()
                    } else {
                        egui::Visuals::light()
                    });
                    ctx.enable_accesskit();
                    let mut frame = eframe::Frame::_new_kittest();
                    let mut draw = |app: &mut App, events| {
                        ctx.run_ui(
                            egui::RawInput {
                                screen_rect: Some(egui::Rect::from_min_size(
                                    egui::Pos2::ZERO,
                                    egui::vec2(760.0, 880.0),
                                )),
                                time: Some(ctx.cumulative_frame_nr() as f64 * 0.2),
                                events,
                                ..Default::default()
                            },
                            |ui| app.ui(ui, &mut frame),
                        )
                    };
                    for _ in 0..3 {
                        draw(&mut app, Vec::new());
                    }
                    let output = draw(&mut app, Vec::new());
                    let target_node = *output
                        .platform_output
                        .accesskit_update
                        .as_ref()
                        .unwrap()
                        .nodes
                        .iter()
                        .find(|(_, node)| node.role() == role && node.label() == Some(label))
                        .map(|(id, _)| id)
                        .unwrap_or_else(|| {
                            panic!(
                                "{role:?} {label} is named before {scene}; matching nodes: {:?}",
                                output
                                    .platform_output
                                    .accesskit_update
                                    .as_ref()
                                    .unwrap()
                                    .nodes
                                    .iter()
                                    .filter(|(_, node)| node.label() == Some(label)
                                        || node.value() == Some(label))
                                    .map(|(_, node)| (node.role(), node.label(), node.value()))
                                    .collect::<Vec<_>>()
                            )
                        });
                    draw(
                        &mut app,
                        vec![egui::Event::AccessKitActionRequest(ActionRequest {
                            action: Action::Focus,
                            target_tree: TreeId::ROOT,
                            target_node,
                            data: None,
                        })],
                    );
                    for _ in 0..3 {
                        draw(&mut app, Vec::new());
                    }
                    match scene {
                        "cleanup result" => {
                            done.unwrap()
                                .send((vec![0.05; 480], ModelState::DeepFilterFailed))
                                .unwrap();
                            app.poll_mic_test();
                            assert!(
                                app.mic_test.cleaning.is_none() && app.mic_test.cleaned.is_some()
                            );
                            assert!(app.mic_test.status.is_some());
                        }
                        "checklist completion" => {
                            app.outputs.push(app.settings.output.clone());
                            assert!(!app.show_checklist());
                        }
                        "bypass message" => app.settings.bypass = true,
                        _ => unreachable!(),
                    }
                    for _ in 0..3 {
                        draw(&mut app, Vec::new());
                    }
                    let output = draw(&mut app, Vec::new());
                    let tree = output.platform_output.accesskit_update.as_ref().unwrap();
                    assert_eq!(
                        tree.focus, target_node,
                        "{role:?} {label} keeps focus after {scene}"
                    );
                    assert!(
                        tree.nodes.iter().any(|(id, node)| *id == target_node
                            && node.role() == role
                            && node.label() == Some(label)),
                        "the same control remains available to assistive input"
                    );
                    assert_eq!(
                        app.settings.processing(),
                        original,
                        "layout changes preserve saved precision"
                    );
                    assert_eq!(app.settings.monitor_volume, original_monitor);
                    assert!(app.dirty_since.is_none());
                    let mut expected = original;
                    let mut monitor = original_monitor;
                    let mut monitor_on = app.settings.monitor;
                    let key = if role == Role::Slider {
                        match label {
                            "Your mic" => expected.input_gain_db = 4.0,
                            "Remove rumble" => expected.highpass_hz = 138.0,
                            "Remove noise" => expected.strength = 0.75,
                            "Only my voice" => expected.voice_threshold = 0.62,
                            "Quiet cut-off" => expected.gate_threshold_db = -42.0,
                            "To Discord" => expected.output_gain_db = -2.0,
                            "Hear yourself" => monitor = 0.45,
                            _ => unreachable!(),
                        }
                        egui::Key::ArrowRight
                    } else if role == Role::CheckBox {
                        match label {
                            "Remove rumble" => expected.highpass = !expected.highpass,
                            "Remove noise" => expected.strength = 0.0,
                            "Only my voice" => expected.voice_gate = !expected.voice_gate,
                            "Quiet cut-off" => expected.gate = !expected.gate,
                            "Hear yourself" => monitor_on = !monitor_on,
                            _ => unreachable!(),
                        }
                        egui::Key::Space
                    } else {
                        expected.model = crate::dsp::Model::Rnnoise;
                        egui::Key::Enter
                    };
                    draw(
                        &mut app,
                        vec![egui::Event::Key {
                            key,
                            physical_key: None,
                            pressed: true,
                            repeat: false,
                            modifiers: egui::Modifiers::NONE,
                        }],
                    );
                    for _ in 0..3 {
                        draw(&mut app, Vec::new());
                    }
                    assert_eq!(
                        app.settings.processing(),
                        expected,
                        "the next key edits only {role:?} {label} after {scene}"
                    );
                    assert_eq!(app.settings.monitor_volume, monitor);
                    assert_eq!(app.settings.monitor, monitor_on);
                    assert!(app.dirty_since.is_some());
                }
            }
        }
    }

    fn finish_capture(app: &mut App) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while app.mic_test.recording() {
            app.poll_mic_test();
            assert!(
                Instant::now() < deadline,
                "the mic test did not finish loading"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn the_test_never_plays_into_vb_cable_or_discords_output() {
        let mut app = App::stopped(Settings {
            output: "CABLE Input (VB-Audio Virtual Cable)".into(),
            monitor_output: "CABLE Input (VB-Audio Virtual Cable)".into(),
            ..Settings::default()
        });
        app.outputs = vec![
            "CABLE Input (VB-Audio Virtual Cable)".into(),
            "Headphones".into(),
        ];
        assert_eq!(app.test_output(), "", "falls back to the Windows default");
        app.settings.monitor_output = "Headphones".into();
        assert_eq!(app.test_output(), "Headphones");
        app.settings.monitor_output = "Unplugged headset".into();
        assert_eq!(app.test_output(), "");
    }

    #[test]
    fn a_muted_mic_still_tests_with_your_other_settings() {
        let mut app = App::stopped(Settings {
            mute: true,
            strength: 0.4,
            ..Settings::default()
        });
        app.settings.input_gain_db = 6.0;
        let p = app.test_params();
        assert!(!p.mute);
        assert_eq!(p.strength, 0.4);
        assert_eq!(p.input_gain, crate::dsp::db_to_gain(6.0));
    }

    #[test]
    fn preview_cache_ignores_controls_that_do_not_change_the_cleaned_audio() {
        let mut app = App::stopped(Settings::default());
        let before = app.test_params();
        app.settings.monitor = true;
        app.settings.monitor_volume = 0.25;
        app.settings.sound_volume = 0.1;
        app.settings.mute = true;
        assert_eq!(
            app.test_params(),
            before,
            "output controls must not rerun the denoiser"
        );

        app.settings.strength = 0.5;
        assert_ne!(
            app.test_params(),
            before,
            "voice changes still need a new preview"
        );
    }

    #[test]
    fn a_recording_is_cleaned_to_the_same_length() {
        let p = Params {
            model: crate::dsp::Model::Rnnoise,
            ..Params::default()
        };
        let tone: Vec<f32> = (0..48_000).map(|i| (i as f32 * 0.05).sin() * 0.2).collect();
        let (cleaned, model) = Cleaner::process_recording(&tone, &p);
        assert_eq!(cleaned.len(), tone.len());
        assert_eq!(model, ModelState::Rnnoise);
        let (muted, _) = Cleaner::process_recording(&tone, &Params { mute: true, ..p });
        assert!(
            muted.iter().all(|s| *s == 0.0),
            "mute would silence a test: test_params clears it"
        );
    }

    #[test]
    fn incomplete_mic_tests_keep_the_previous_preview_and_request_another_recording() {
        for (had_preview, count) in [(false, 0), (false, 96), (true, 0), (true, 96)] {
            let mut app = App::stopped(Settings::default());
            let raw = Arc::new(vec![0.1; 480]);
            let cleaned = Arc::new(vec![0.05; 480]);
            if had_preview {
                app.mic_test.raw = Some(Arc::clone(&raw));
                app.mic_test.cleaned = Some((
                    Arc::clone(&cleaned),
                    app.test_params(),
                    ModelState::DeepFilter,
                ));
            }
            let recorder = Recorder::from_samples_for_test(&vec![0.25; count]);
            recorder.report_skipped_for_test();
            app.mic_test.recording = Some((recorder, Instant::now() - TEST_LENGTH));
            finish_capture(&mut app);

            assert!(
                !app.mic_test.busy(),
                "an incomplete take must not start cleaning or playback"
            );
            assert!(!app.settings.setup.tested && app.dirty_since.is_none());
            let (message, color) = app.mic_test.status.as_ref().unwrap();
            assert!(message.starts_with(if count == 0 {
                "Recording stopped:"
            } else {
                "Record the mic test again:"
            }));
            assert!(message.contains("audio was skipped"));
            assert_eq!(*color, RED);
            if had_preview {
                assert!(Arc::ptr_eq(app.mic_test.raw.as_ref().unwrap(), &raw));
                assert!(Arc::ptr_eq(
                    &app.mic_test.cleaned.as_ref().unwrap().0,
                    &cleaned
                ));
            } else {
                assert!(app.mic_test.raw.is_none() && app.mic_test.cleaned.is_none());
            }
        }
    }

    #[test]
    fn slow_mic_test_writes_keep_controls_responsive_and_preserve_old_previews_on_failure() {
        for capture_fails in [false, true] {
            let mut app = App::stopped(Settings {
                model: crate::dsp::Model::Rnnoise,
                ..Settings::default()
            });
            let raw = Arc::new(vec![0.1; 480]);
            let cleaned = Arc::new(vec![0.05; 480]);
            app.mic_test.raw = Some(Arc::clone(&raw));
            app.mic_test.cleaned =
                Some((Arc::clone(&cleaned), app.test_params(), ModelState::Rnnoise));
            let (recorder, finish) = Recorder::delayed_writer_for_test(&[0.25; 480]);
            if capture_fails {
                recorder.report_error_for_test("Microphone disconnected");
            }
            app.mic_test.recording = Some((recorder, Instant::now() - TEST_LENGTH));

            for _ in 0..3 {
                app.poll_mic_test();
            }
            let recorder = &app.mic_test.recording.as_ref().unwrap().0;
            assert!(recorder.is_stopping() && !recorder.ready_to_finish());
            assert!(app.mic_test.recording() && app.mic_test.busy());
            assert!(app.mic_test.loading.is_none());
            assert_eq!(
                app.update_blocker(),
                Some("Wait for the mic test to finish")
            );
            app.start_mic_test();
            app.play_test(Version::Raw);
            app.play_test(Version::Cleaned);
            assert!(app.mic_test.player.is_none() && app.mic_test.cleaning.is_none());
            assert!(Arc::ptr_eq(app.mic_test.raw.as_ref().unwrap(), &raw));
            assert_eq!(
                app.mic_test.status,
                Some(if capture_fails {
                    ("Microphone disconnected".into(), RED)
                } else {
                    ("Finishing mic test…".into(), CYAN)
                })
            );
            let ctx = egui::Context::default();
            let output = ctx.run_ui(egui::RawInput::default(), |ui| app.draw_mic_test(ui));
            let texts: Vec<_> = output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) => Some(text.galley.text()),
                    _ => None,
                })
                .collect();
            assert!(
                !texts
                    .iter()
                    .any(|text| ["0 s", "Record again", "⏵ Raw", "⏵ Cleaned"].contains(text))
            );
            assert!(
                texts
                    .iter()
                    .any(|text| *text == app.mic_test.status.as_ref().unwrap().0)
            );
            app.run_command(&ctx, super::super::Command::ToggleMute);
            assert!(app.settings.mute);

            finish.send(()).unwrap();
            finish_capture(&mut app);
            assert!(!app.mic_test.recording() && app.mic_test.player.is_none());
            assert!(!app.settings.setup.tested);
            if capture_fails {
                assert!(Arc::ptr_eq(app.mic_test.raw.as_ref().unwrap(), &raw));
                assert!(Arc::ptr_eq(
                    &app.mic_test.cleaned.as_ref().unwrap().0,
                    &cleaned
                ));
                assert_eq!(
                    app.mic_test.status,
                    Some(("Microphone disconnected".into(), RED))
                );
                assert!(!app.mic_test.busy());
            } else {
                let samples = app.mic_test.raw.as_ref().unwrap();
                assert!(!Arc::ptr_eq(samples, &raw));
                assert_eq!(samples.len(), 240);
                assert!(samples.iter().all(|sample| (*sample - 0.25).abs() < 0.001));
                assert_eq!(
                    app.mic_test.status,
                    Some(("Now compare: Raw, then Cleaned".into(), GREEN))
                );
            }
        }
    }

    #[test]
    fn loading_a_mic_test_suppresses_old_playback_and_recovers_from_empty_or_failed_results() {
        for outcome in 0..4 {
            let mut app = App::stopped(Settings {
                model: crate::dsp::Model::Rnnoise,
                ..Settings::default()
            });
            let clean = pending_cleanup(&mut app, true);
            let raw = Arc::clone(app.mic_test.raw.as_ref().unwrap());
            app.stop_test();
            let (done, job) = crossbeam_channel::bounded(1);
            app.mic_test.loading = Some(Loading {
                result: job,
                cancel: Arc::new(AtomicBool::new(false)),
            });
            app.mic_test.status = Some(("Finishing mic test…".into(), CYAN));
            clean.send((vec![0.05; 480], ModelState::Rnnoise)).unwrap();

            app.poll_mic_test();
            app.start_mic_test();
            app.play_test(Version::Raw);
            assert!(app.mic_test.recording.is_none() && app.mic_test.loading.is_some());
            assert!(app.mic_test.cleaning.is_none() && app.mic_test.player.is_none());
            assert_eq!(
                app.mic_test.status,
                Some(("Finishing mic test…".into(), CYAN))
            );
            let old_cleaned = Arc::clone(&app.mic_test.cleaned.as_ref().unwrap().0);
            match outcome {
                0 => done.send(Ok(Some(vec![0.25; 240]))).unwrap(),
                1 => done.send(Ok(None)).unwrap(),
                2 => done
                    .send(Err(anyhow::anyhow!("Unable to read test audio")))
                    .unwrap(),
                _ => drop(done),
            }
            app.poll_mic_test();

            assert!(!app.mic_test.recording() && app.mic_test.player.is_none());
            assert!(!app.settings.setup.tested && app.dirty_since.is_none());
            if outcome == 0 {
                assert_eq!(app.mic_test.raw.as_ref().unwrap().as_slice(), &[0.25; 240]);
                assert_eq!(
                    app.mic_test.status,
                    Some(("Now compare: Raw, then Cleaned".into(), GREEN))
                );
                if let Some((samples, _, _)) = &app.mic_test.cleaned {
                    assert_eq!(
                        samples.len(),
                        240,
                        "never cache the previous preview for the new take"
                    );
                }
            } else {
                assert!(Arc::ptr_eq(app.mic_test.raw.as_ref().unwrap(), &raw));
                assert!(Arc::ptr_eq(
                    &app.mic_test.cleaned.as_ref().unwrap().0,
                    &old_cleaned
                ));
                assert!(!app.mic_test.busy() && app.update_blocker().is_none());
                let (text, color) = app.mic_test.status.as_ref().unwrap();
                assert_eq!(*color, RED);
                assert!(text.contains(match outcome {
                    1 => "Nothing was recorded",
                    2 => "Unable to read test audio",
                    _ => "record again",
                }));
            }
        }
    }

    #[test]
    fn cancel_test_button_releases_capture_and_loading_without_losing_the_previous_preview() {
        for stage in ["recording", "finishing", "loading"] {
            for had_preview in [false, true] {
                let mut app = App::stopped(Settings::default());
                let raw = Arc::new(vec![0.1; 480]);
                let cleaned = Arc::new(vec![0.05; 480]);
                if had_preview {
                    app.mic_test.raw = Some(Arc::clone(&raw));
                    app.mic_test.cleaned = Some((
                        Arc::clone(&cleaned),
                        app.test_params(),
                        ModelState::DeepFilter,
                    ));
                }
                let (loaded, job) = crossbeam_channel::bounded(1);
                let cancel = Arc::new(AtomicBool::new(false));
                let finish = if stage == "loading" {
                    app.mic_test.loading = Some(Loading {
                        result: job,
                        cancel: Arc::clone(&cancel),
                    });
                    None
                } else {
                    let (mut recorder, finish) = Recorder::delayed_writer_for_test(&[0.25; 480]);
                    if stage == "finishing" {
                        recorder.stop_capture();
                    }
                    app.mic_test.recording = Some((recorder, Instant::now()));
                    Some(finish)
                };
                app.mic_test.status = Some(if stage == "recording" {
                    ("Speak normally…".into(), AMBER)
                } else {
                    ("Finishing mic test…".into(), CYAN)
                });
                let ctx = egui::Context::default();
                ctx.enable_accesskit();
                let draw = |app: &mut App, events| {
                    ctx.run_ui(
                        egui::RawInput {
                            screen_rect: Some(egui::Rect::from_min_size(
                                egui::Pos2::ZERO,
                                egui::vec2(700.0, 100.0),
                            )),
                            events,
                            ..Default::default()
                        },
                        |ui| app.draw_mic_test(ui),
                    )
                };
                let first = draw(&mut app, Vec::new());
                assert!(
                    first
                        .platform_output
                        .accesskit_update
                        .unwrap()
                        .nodes
                        .iter()
                        .any(|(_, node)| node.label() == Some("Cancel test"))
                );
                let pos = draw(&mut app, Vec::new())
                    .shapes
                    .iter()
                    .find_map(|shape| match &shape.shape {
                        egui::Shape::Text(text) if text.galley.text() == "Cancel test" => {
                            Some(text.visual_bounding_rect().center())
                        }
                        _ => None,
                    })
                    .expect("cancel is visible throughout capture and loading");
                for pressed in [true, false] {
                    draw(
                        &mut app,
                        vec![
                            egui::Event::PointerMoved(pos),
                            egui::Event::PointerButton {
                                pos,
                                button: egui::PointerButton::Primary,
                                pressed,
                                modifiers: egui::Modifiers::NONE,
                            },
                        ],
                    );
                }

                assert!(!app.mic_test.recording() && !app.mic_test.busy());
                assert!(app.mic_test.player.is_none() && app.mic_test.cleaning.is_none());
                assert!(app.update_blocker().is_none());
                assert_eq!(
                    app.mic_test.status,
                    Some(("Mic test cancelled".into(), MUTED))
                );
                assert!(!app.settings.setup.tested && app.dirty_since.is_none());
                if let Some(finish) = finish {
                    finish
                        .send(())
                        .expect("cancellation must not wait for the disk writer");
                } else {
                    assert!(
                        cancel.load(Ordering::Relaxed),
                        "cancel the decoder as well as its result"
                    );
                    assert!(
                        loaded.send(Ok(Some(vec![0.9; 480]))).is_err(),
                        "a cancelled load cannot publish late audio"
                    );
                }
                app.poll_mic_test();
                if had_preview {
                    assert!(Arc::ptr_eq(app.mic_test.raw.as_ref().unwrap(), &raw));
                    assert!(Arc::ptr_eq(
                        &app.mic_test.cleaned.as_ref().unwrap().0,
                        &cleaned
                    ));
                } else {
                    assert!(app.mic_test.raw.is_none() && app.mic_test.cleaned.is_none());
                }
                let output = draw(&mut app, Vec::new());
                let texts: Vec<_> = output
                    .shapes
                    .iter()
                    .filter_map(|shape| match &shape.shape {
                        egui::Shape::Text(text) => Some(text.galley.text()),
                        _ => None,
                    })
                    .collect();
                assert!(texts.contains(&if had_preview {
                    "Record again"
                } else {
                    "Test my mic"
                }));
                assert!(!texts.contains(&"Cancel test"));
                assert_eq!(texts.contains(&"⏵ Raw"), had_preview);
                assert_eq!(texts.contains(&"⏵ Cleaned"), had_preview);
            }
        }
    }

    #[test]
    fn cancelling_a_mic_load_ignores_queued_audio_and_suppresses_the_previous_cleaner() {
        for cleanup_fails in [false, true] {
            let mut app = App::stopped(Settings::default());
            let clean = pending_cleanup(&mut app, true);
            let raw = Arc::clone(app.mic_test.raw.as_ref().unwrap());
            let (loaded, job) = crossbeam_channel::bounded(1);
            let cancel = Arc::new(AtomicBool::new(false));
            app.mic_test.loading = Some(Loading {
                result: job,
                cancel: Arc::clone(&cancel),
            });
            loaded.send(Ok(Some(vec![0.9; 480]))).unwrap();

            app.cancel_mic_test();
            assert!(cancel.load(Ordering::Relaxed));
            let cleaning = app.mic_test.cleaning.as_ref().unwrap();
            assert!(!cleaning.then_play && !cleaning.report_status);
            if cleanup_fails {
                drop(clean);
            } else {
                clean
                    .send((vec![0.05; 480], ModelState::DeepFilterFailed))
                    .unwrap();
            }
            app.poll_mic_test();
            assert!(!app.mic_test.busy() && app.mic_test.player.is_none());
            assert!(Arc::ptr_eq(app.mic_test.raw.as_ref().unwrap(), &raw));
            assert_eq!(
                app.mic_test.status,
                Some(("Mic test cancelled".into(), MUTED))
            );
            assert!(!app.settings.setup.tested && app.dirty_since.is_none());

            // A new test still loads and cleans normally after cancellation.
            app.settings.model = crate::dsp::Model::Rnnoise;
            app.mic_test.recording = Some((
                Recorder::from_samples_for_test(&[0.25; 480]),
                Instant::now() - TEST_LENGTH,
            ));
            finish_capture(&mut app);
            assert!(!Arc::ptr_eq(app.mic_test.raw.as_ref().unwrap(), &raw));
            assert_eq!(app.mic_test.raw.as_ref().unwrap().len(), 240);
            assert_eq!(
                app.mic_test.status,
                Some(("Now compare: Raw, then Cleaned".into(), GREEN))
            );
            assert!(app.mic_test.player.is_none() && !app.settings.setup.tested);
        }
    }

    #[test]
    #[ignore = "writes mic test finalization screenshots; needs Python with Pillow"]
    fn snapshot_mic_test_finalization() {
        for (dark, theme) in [(true, "dark"), (false, "light")] {
            let (recorder, finish) = Recorder::delayed_writer_for_test(&[0.25; 480]);
            let mut app = App::stopped(Settings::default());
            app.mic_test.raw = Some(Arc::new(vec![0.1; 480]));
            app.mic_test.cleaned = Some((
                Arc::new(vec![0.05; 480]),
                app.test_params(),
                ModelState::DeepFilter,
            ));
            app.mic_test.recording = Some((recorder, Instant::now()));
            app.mic_test.status = Some(("Speak normally…".into(), AMBER));
            super::super::snapshot::snapshot(
                &mut app,
                &format!("mic-test-recording-{theme}"),
                egui::vec2(700.0, 420.0),
                1.0,
                dark,
            );
            app.mic_test.recording.as_mut().unwrap().1 = Instant::now() - TEST_LENGTH;
            app.poll_mic_test();
            super::super::snapshot::snapshot(
                &mut app,
                &format!("mic-test-finishing-{theme}"),
                egui::vec2(700.0, 420.0),
                1.0,
                dark,
            );
            app.cancel_mic_test();
            super::super::snapshot::snapshot(
                &mut app,
                &format!("mic-test-cancelled-{theme}"),
                egui::vec2(700.0, 420.0),
                1.0,
                dark,
            );
            finish.send(()).unwrap();
        }
    }

    #[test]
    fn a_failed_cleaner_releases_the_mic_test_and_keeps_the_recording() {
        let mut app = App::stopped(Settings::default());
        let raw = Arc::new(vec![0.1; 480]);
        app.mic_test.raw = Some(Arc::clone(&raw));
        let done = pending_cleanup(&mut app, true);
        drop(done); // a worker that exits without delivering its result

        app.poll_mic_test();

        assert!(
            !app.mic_test.busy(),
            "a failed worker must not block previews or updates forever"
        );
        assert!(Arc::ptr_eq(app.mic_test.raw.as_ref().unwrap(), &raw));
        let (message, color) = app.mic_test.status.as_ref().unwrap();
        assert!(message.contains("try again"), "{message}");
        assert_eq!(*color, RED);
    }

    #[test]
    fn a_fallback_preview_keeps_its_audio_and_explains_the_model_used() {
        for bypass in [false, true] {
            let mut app = App::stopped(Settings {
                bypass,
                ..Settings::default()
            });
            let raw = Arc::new(vec![0.1; 480]);
            app.mic_test.raw = Some(Arc::clone(&raw));
            let params = app.test_params();
            let done = pending_cleanup(&mut app, false);
            done.send((vec![0.05; 480], ModelState::DeepFilterFailed))
                .unwrap();

            app.poll_mic_test();
            assert!(
                !app.mic_test.busy(),
                "collecting a preview must not start playback"
            );
            assert!(Arc::ptr_eq(app.mic_test.raw.as_ref().unwrap(), &raw));
            let (samples, cleaned_params, model) = app.mic_test.cleaned.as_ref().unwrap();
            assert_eq!(samples.as_slice(), &[0.05; 480]);
            assert_eq!(*cleaned_params, params);
            assert_eq!(*model, ModelState::DeepFilterFailed);
            assert_eq!(
                app.mic_test.used_fallback(),
                !bypass,
                "bypass audio does not depend on the denoising model"
            );

            if !bypass {
                let (message, color) = app.mic_test.status.as_ref().unwrap();
                assert!(
                    message.contains("RNNoise") && message.contains("DeepFilterNet unavailable")
                );
                assert_eq!(*color, AMBER);
            }
            app.finish_test_playback(Ok(()));
            assert_eq!(
                app.mic_test.status.as_ref().unwrap().1,
                if bypass { MUTED } else { AMBER },
                "successful playback must not erase the fallback information"
            );
        }
    }

    #[test]
    fn cleaned_playback_reuses_the_running_cleaner() {
        let mut app = App::stopped(Settings::default());
        app.mic_test.raw = Some(Arc::new(vec![0.1; 480]));
        let params = app.test_params();
        let done = pending_cleanup(&mut app, false);

        app.clean_test(true);
        app.settings.strength = 0.5;
        app.clean_test(true);

        assert!(
            done.send((vec![0.2; 480], ModelState::DeepFilter)).is_ok(),
            "clicks reuse pending work instead of spawning competing cleaners"
        );
        let cleaning = app.mic_test.cleaning.as_ref().unwrap();
        assert_eq!(
            cleaning.params, params,
            "the result keeps the settings it was actually cleaned with"
        );
        assert!(
            cleaning.then_play,
            "request playback once the result is ready"
        );
    }

    #[test]
    fn an_old_cleaner_cannot_interrupt_a_new_recording() {
        for recording_fails in [false, true] {
            for failed in [false, true] {
                let mut app = App::stopped(Settings::default());
                let done = pending_cleanup(&mut app, true);
                app.stop_test();
                let recorder = Recorder::from_samples_for_test(&[0.25; 480]);
                if recording_fails {
                    recorder.report_error_for_test("Microphone disconnected");
                }
                app.mic_test.recording = Some((recorder, Instant::now()));
                app.mic_test.status = Some(("Speak normally…".into(), AMBER));
                app.poll_mic_test();
                if failed {
                    drop(done);
                } else {
                    done.send((vec![0.05; 480], ModelState::DeepFilterFailed))
                        .unwrap();
                }

                app.poll_mic_test();

                assert_eq!(app.mic_test.recording(), !recording_fails);
                assert!(app.mic_test.cleaning.is_none() && app.mic_test.player.is_none());
                assert_eq!(
                    app.mic_test.status,
                    Some(if recording_fails {
                        ("Microphone disconnected".into(), RED)
                    } else {
                        ("Speak normally…".into(), AMBER)
                    })
                );
                assert_eq!(
                    app.mic_test.cleaned.is_none(),
                    failed,
                    "a useful old preview is kept if recording fails"
                );
                assert!(
                    !app.settings.setup.tested,
                    "old work must not start playback"
                );
            }
        }
    }

    #[test]
    fn a_new_take_waits_for_the_previous_cleaner_and_discards_its_result() {
        for failed in [false, true] {
            let mut app = App::stopped(Settings {
                model: crate::dsp::Model::Rnnoise,
                ..Settings::default()
            });
            let done = pending_cleanup(&mut app, true);
            let old_raw = Arc::clone(app.mic_test.raw.as_ref().unwrap());
            app.stop_test();
            app.mic_test.recording = Some((
                Recorder::from_samples_for_test(&[0.25; 480]),
                Instant::now() - TEST_LENGTH,
            ));

            finish_capture(&mut app);

            let raw = Arc::clone(app.mic_test.raw.as_ref().unwrap());
            assert!(!Arc::ptr_eq(&raw, &old_raw), "the new take was recorded");
            assert!(
                Arc::ptr_eq(&app.mic_test.cleaning.as_ref().unwrap().raw, &old_raw),
                "the old worker remains tracked until it finishes"
            );
            if failed {
                drop(done);
            } else {
                done.send((vec![0.9; 480], ModelState::DeepFilterFailed))
                    .unwrap();
            }

            app.poll_mic_test();

            assert!(
                app.mic_test.cleaned.is_none(),
                "an old result must not be cached for the new take"
            );
            assert_eq!(
                app.mic_test.status,
                Some(("Now compare: Raw, then Cleaned".into(), GREEN))
            );
            let cleaning = app.mic_test.cleaning.as_ref().unwrap();
            assert!(
                Arc::ptr_eq(&cleaning.raw, &raw),
                "the latest take is cleaned next"
            );
            assert!(
                !cleaning.then_play && app.mic_test.player.is_none(),
                "recording again cancels queued playback"
            );
            let (samples, model) = cleaning
                .result
                .recv_timeout(Duration::from_secs(5))
                .unwrap();
            assert_eq!(samples.len(), raw.len());
            assert_eq!(model, ModelState::Rnnoise);
        }
    }

    #[test]
    fn recording_again_skips_a_take_still_waiting_for_the_old_cleaner() {
        let mut app = App::stopped(Settings::default());
        let done = pending_cleanup(&mut app, true);
        let waiting = Arc::new(vec![0.2; 480]);
        app.mic_test.raw = Some(Arc::clone(&waiting));
        app.clean_test(false);
        app.stop_test();
        app.mic_test.recording = Some((
            Recorder::from_samples_for_test(&[0.25; 480]),
            Instant::now(),
        ));
        app.mic_test.status = Some(("Speak normally…".into(), AMBER));
        done.send((vec![0.05; 480], ModelState::DeepFilterFailed))
            .unwrap();

        app.poll_mic_test();

        assert!(app.mic_test.recording());
        assert!(
            app.mic_test.cleaning.is_none(),
            "wait for the new recording instead of cleaning an intermediate take"
        );
        assert!(app.mic_test.cleaned.is_none() && app.mic_test.player.is_none());
        assert!(
            Arc::ptr_eq(app.mic_test.raw.as_ref().unwrap(), &waiting),
            "keep the previous take until recording succeeds"
        );
        assert_eq!(app.mic_test.status, Some(("Speak normally…".into(), AMBER)));
    }

    #[test]
    fn stopping_a_preview_cancels_queued_playback_but_keeps_the_cleaned_take() {
        let mut app = App::stopped(Settings::default());
        let params = app.test_params();
        let done = pending_cleanup(&mut app, true);

        app.stop_test();
        done.send((vec![0.25; 480], ModelState::DeepFilter))
            .unwrap();
        app.poll_mic_test();

        assert!(!app.mic_test.busy());
        assert!(
            app.mic_test.status.is_none(),
            "a cancelled preview never opens an audio device"
        );
        let (samples, cleaned_params, _) = app.mic_test.cleaned.as_ref().unwrap();
        assert_eq!(samples.as_slice(), &[0.25; 480]);
        assert_eq!(*cleaned_params, params);
        assert!(
            !app.settings.setup.tested,
            "cancelling is not a successful listen"
        );
    }

    #[test]
    fn the_cleaned_button_can_cancel_playback_while_cleanup_is_running() {
        let mut app = App::stopped(Settings::default());
        app.mic_test.raw = Some(Arc::new(vec![0.1; 480]));
        let done = pending_cleanup(&mut app, false);
        let ctx = egui::Context::default();
        let draw = |app: &mut App, events| {
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(760.0, 100.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ui| app.draw_mic_test(ui),
            )
        };
        draw(&mut app, Vec::new());
        let pos = draw(&mut app, Vec::new())
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.text() == "⏵ Cleaned" => {
                    Some(text.visual_bounding_rect().center())
                }
                _ => None,
            })
            .expect("Cleaned control is visible");

        for expected in [true, false] {
            for pressed in [true, false] {
                draw(
                    &mut app,
                    vec![
                        egui::Event::PointerMoved(pos),
                        egui::Event::PointerButton {
                            pos,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                );
            }
            assert_eq!(
                app.mic_test.cleaning.as_ref().unwrap().then_play,
                expected,
                "clicking again cancels pending playback"
            );
        }
        done.send((vec![0.25; 480], ModelState::DeepFilter))
            .unwrap();
        app.poll_mic_test();
        assert!(!app.mic_test.busy());
        assert_eq!(
            app.mic_test.status,
            Some(("Playback stopped".into(), MUTED))
        );
        assert_eq!(
            app.mic_test.cleaned.as_ref().unwrap().0.as_slice(),
            &[0.25; 480]
        );
        assert!(
            !app.settings.setup.tested,
            "cancelled playback must never open a device or complete setup"
        );
    }

    #[test]
    #[ignore = "writes preview-control screenshots; needs Python with Pillow"]
    fn snapshot_pending_mic_preview() {
        let mut app = App::stopped(Settings::default());
        app.mic_test.raw = Some(Arc::new(vec![0.1; 480]));
        let _done = pending_cleanup(&mut app, true);
        app.mic_test.status = Some(("Applying your settings…".into(), CYAN));
        let path = super::super::snapshot::snapshot(
            &mut app,
            "mic-preview-pending",
            egui::vec2(760.0, 1100.0),
            1.0,
            true,
        );
        println!("SNAPSHOT {}", path.display());
    }

    #[test]
    fn failed_playback_preserves_the_preview_for_retry_without_completing_setup() {
        let mut app = App::stopped(Settings::default());
        let raw = Arc::new(vec![0.1; 480]);
        let cleaned = Arc::new(vec![0.05; 480]);
        app.mic_test.raw = Some(Arc::clone(&raw));
        app.mic_test.cleaned = Some((
            Arc::clone(&cleaned),
            app.test_params(),
            ModelState::DeepFilter,
        ));

        app.finish_test_playback(Err("Headphones disconnected".into()));

        assert!(!app.mic_test.busy());
        assert!(!app.settings.setup.tested && app.dirty_since.is_none());
        assert_eq!(
            app.mic_test.status,
            Some(("Playback stopped: Headphones disconnected".into(), RED))
        );
        assert!(Arc::ptr_eq(app.mic_test.raw.as_ref().unwrap(), &raw));
        assert!(Arc::ptr_eq(
            &app.mic_test.cleaned.as_ref().unwrap().0,
            &cleaned
        ));

        app.finish_test_playback(Ok(()));
        assert!(
            app.settings.setup.tested && app.dirty_since.is_some(),
            "a successful retry completes the checklist"
        );
        assert_eq!(app.mic_test.status.as_ref().unwrap().1, MUTED);
    }
}
