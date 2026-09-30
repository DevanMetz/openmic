//! "Test my mic": record a few seconds, then play them back raw and as
//! Discord would hear them, on your own headphones or speakers. Cleaning
//! uses the current settings each time, so you can tune and listen again.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, TryRecvError};
use eframe::egui::{self, RichText};

use super::{short, App, Status, AMBER, CYAN, GREEN, MUTED, RED};
use crate::denoise::{Cleaner, ModelState, ProcessedRecording};
use crate::dsp::Params;
use crate::playback::{self, Player};
use crate::record::{Recorder, Source};

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

#[derive(Default)]
pub(super) struct MicTest {
    recording: Option<(Recorder, Instant)>,
    /// The take as recorded (mono, DSP rate).
    raw: Option<Arc<Vec<f32>>>,
    /// The cleaned take, its settings, and the model state that produced it.
    cleaned: Option<(Arc<Vec<f32>>, Params, ModelState)>,
    /// Cleaning in the background, and whether to play it when done.
    cleaning: Option<Cleaning>,
    player: Option<(Player, Version)>,
    status: Option<Status>,
}

impl MicTest {
    pub(super) fn recording(&self) -> bool {
        self.recording.is_some()
    }

    pub(super) fn busy(&self) -> bool {
        self.recording.is_some() || self.cleaning.is_some() || self.player.is_some()
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
        Params { mute: false, monitor_on: false, monitor_gain: 1.0, sound_gain: 1.0, ..self.params() }
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
        let Some(raw) = self.mic_test.raw.clone() else { return };
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
        let spawned = std::thread::Builder::new().name("openmic-mic-test".into()).spawn(move || {
            let _ = done.send(Cleaner::process_recording(&samples, &params));
        });
        match spawned {
            Ok(_) => self.mic_test.cleaning = Some(Cleaning { result: job, raw, params, then_play, report_status: true }),
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

    fn play_test(&mut self, version: Version) {
        self.stop_test();
        let samples = match version {
            Version::Raw => self.mic_test.raw.clone(),
            Version::Cleaned => match &self.mic_test.cleaned {
                Some((cleaned, params, _)) if *params == self.test_params() => Some(Arc::clone(cleaned)),
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
                        Version::Cleaned if fallback => "Playing RNNoise fallback; DeepFilterNet unavailable".into(),
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
        if let Some((recorder, started)) = &test.recording {
            if let Some(e) = recorder.error() {
                test.recording = None;
                test.status = Some((short(&e), RED));
            } else if started.elapsed() >= TEST_LENGTH {
                let (recorder, _) = test.recording.take().expect("checked above");
                let take = recorder.finish().and_then(|take| take.map(|take| {
                    if let Some(warning) = take.warning() {
                        anyhow::bail!("Record the mic test again: {warning}");
                    }
                    take.samples()
                }).transpose());
                match take {
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
        }
        let test = &mut self.mic_test;
        let finished = test.cleaning.as_ref().and_then(|cleaning| match cleaning.result.try_recv() {
            Err(TryRecvError::Empty) => None,
            result => Some(result),
        });
        if let Some(result) = finished {
            let cleaning = test.cleaning.take().expect("checked above");
            if !test.raw.as_ref().is_some_and(|raw| Arc::ptr_eq(raw, &cleaning.raw)) {
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
                                test.status = Some(("Cleaned uses RNNoise; DeepFilterNet unavailable".into(), AMBER));
                            }
                        }
                    }
                    Err(_) if cleaning.report_status && !test.recording() => {
                        test.status = Some(("Could not clean the mic test; click Cleaned to try again".into(), RED));
                    }
                    Err(_) => {}
                }
            }
        }
        let error = self.mic_test.player.as_ref().and_then(|(player, _)| player.take_error());
        if let Some(error) = error {
            self.finish_test_playback(Err(error));
        } else if self.mic_test.player.as_ref().is_some_and(|(player, _)| player.finished()) {
            self.finish_test_playback(Ok(()));
        }
    }

    fn finish_test_playback(&mut self, result: Result<(), String>) {
        self.mic_test.player = None;
        match result {
            Ok(()) => {
                self.mic_test.status = Some(if self.mic_test.used_fallback() {
                    ("Cleaned uses RNNoise; DeepFilterNet unavailable".into(), AMBER)
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

    /// The test's controls: one row.
    pub(super) fn draw_mic_test(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if let Some((_, started)) = &self.mic_test.recording {
                let left = TEST_LENGTH.saturating_sub(started.elapsed()).as_secs_f32();
                ui.add(egui::ProgressBar::new(1.0 - left / TEST_LENGTH.as_secs_f32()).desired_width(120.0).text(format!("{left:.0} s")));
            } else if ui
                .button(if self.mic_test.raw.is_some() { "Record again" } else { "Test my mic" })
                .on_hover_text("Record 5 seconds, then hear it raw and cleaned on your headphones or speakers")
                .clicked()
            {
                self.start_mic_test();
            }
            if self.mic_test.raw.is_some() && !self.mic_test.recording() {
                for (version, label) in [(Version::Raw, "⏵ Raw"), (Version::Cleaned, "⏵ Cleaned")] {
                    let playing = self.mic_test.player.as_ref().is_some_and(|(_, v)| *v == version)
                        || (version == Version::Cleaned && self.mic_test.cleaning.as_ref().is_some_and(|cleaning| cleaning.then_play));
                    if ui.selectable_label(playing, label)
                        .on_hover_text(if playing { "Stop preview" } else { "Play preview" })
                        .clicked()
                    {
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
                ui.label(RichText::new(text).small().color(*color));
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Settings;

    fn pending_cleanup(app: &mut App, then_play: bool) -> crossbeam_channel::Sender<ProcessedRecording> {
        let params = app.test_params();
        let raw = Arc::clone(app.mic_test.raw.get_or_insert_with(|| Arc::new(vec![0.1; 480])));
        let (done, job) = crossbeam_channel::bounded(1);
        app.mic_test.cleaning = Some(Cleaning { result: job, raw, params, then_play, report_status: true });
        done
    }

    #[test]
    fn the_test_never_plays_into_vb_cable_or_discords_output() {
        let mut app = App::stopped(Settings {
            output: "CABLE Input (VB-Audio Virtual Cable)".into(),
            monitor_output: "CABLE Input (VB-Audio Virtual Cable)".into(),
            ..Settings::default()
        });
        app.outputs = vec!["CABLE Input (VB-Audio Virtual Cable)".into(), "Headphones".into()];
        assert_eq!(app.test_output(), "", "falls back to the Windows default");
        app.settings.monitor_output = "Headphones".into();
        assert_eq!(app.test_output(), "Headphones");
        app.settings.monitor_output = "Unplugged headset".into();
        assert_eq!(app.test_output(), "");
    }

    #[test]
    fn a_muted_mic_still_tests_with_your_other_settings() {
        let mut app = App::stopped(Settings { mute: true, strength: 0.4, ..Settings::default() });
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
        assert_eq!(app.test_params(), before, "output controls must not rerun the denoiser");

        app.settings.strength = 0.5;
        assert_ne!(app.test_params(), before, "voice changes still need a new preview");
    }

    #[test]
    fn a_recording_is_cleaned_to_the_same_length() {
        let p = Params { model: crate::dsp::Model::Rnnoise, ..Params::default() };
        let tone: Vec<f32> = (0..48_000).map(|i| (i as f32 * 0.05).sin() * 0.2).collect();
        let (cleaned, model) = Cleaner::process_recording(&tone, &p);
        assert_eq!(cleaned.len(), tone.len());
        assert_eq!(model, ModelState::Rnnoise);
        let (muted, _) = Cleaner::process_recording(&tone, &Params { mute: true, ..p });
        assert!(muted.iter().all(|s| *s == 0.0), "mute would silence a test: test_params clears it");
    }

    #[test]
    fn incomplete_mic_tests_keep_the_previous_preview_and_request_another_recording() {
        for had_preview in [false, true] {
            let mut app = App::stopped(Settings::default());
            let raw = Arc::new(vec![0.1; 480]);
            let cleaned = Arc::new(vec![0.05; 480]);
            if had_preview {
                app.mic_test.raw = Some(Arc::clone(&raw));
                app.mic_test.cleaned = Some((Arc::clone(&cleaned), app.test_params(), ModelState::DeepFilter));
            }
            let recorder = Recorder::from_samples_for_test(&[0.25; 96]);
            recorder.report_skipped_for_test();
            app.mic_test.recording = Some((recorder, Instant::now() - TEST_LENGTH));
            app.poll_mic_test();

            assert!(!app.mic_test.busy(), "an incomplete take must not start cleaning or playback");
            assert!(!app.settings.setup.tested && app.dirty_since.is_none());
            let (message, color) = app.mic_test.status.as_ref().unwrap();
            assert!(message.starts_with("Record the mic test again:"));
            assert!(message.contains("audio was skipped"));
            assert_eq!(*color, RED);
            if had_preview {
                assert!(Arc::ptr_eq(app.mic_test.raw.as_ref().unwrap(), &raw));
                assert!(Arc::ptr_eq(&app.mic_test.cleaned.as_ref().unwrap().0, &cleaned));
            } else {
                assert!(app.mic_test.raw.is_none() && app.mic_test.cleaned.is_none());
            }
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

        assert!(!app.mic_test.busy(), "a failed worker must not block previews or updates forever");
        assert!(Arc::ptr_eq(app.mic_test.raw.as_ref().unwrap(), &raw));
        let (message, color) = app.mic_test.status.as_ref().unwrap();
        assert!(message.contains("try again"), "{message}");
        assert_eq!(*color, RED);
    }

    #[test]
    fn a_fallback_preview_keeps_its_audio_and_explains_the_model_used() {
        for bypass in [false, true] {
            let mut app = App::stopped(Settings { bypass, ..Settings::default() });
            let raw = Arc::new(vec![0.1; 480]);
            app.mic_test.raw = Some(Arc::clone(&raw));
            let params = app.test_params();
            let done = pending_cleanup(&mut app, false);
            done.send((vec![0.05; 480], ModelState::DeepFilterFailed)).unwrap();

            app.poll_mic_test();
            assert!(!app.mic_test.busy(), "collecting a preview must not start playback");
            assert!(Arc::ptr_eq(app.mic_test.raw.as_ref().unwrap(), &raw));
            let (samples, cleaned_params, model) = app.mic_test.cleaned.as_ref().unwrap();
            assert_eq!(samples.as_slice(), &[0.05; 480]);
            assert_eq!(*cleaned_params, params);
            assert_eq!(*model, ModelState::DeepFilterFailed);
            assert_eq!(app.mic_test.used_fallback(), !bypass,
                "bypass audio does not depend on the denoising model");

            if !bypass {
                let (message, color) = app.mic_test.status.as_ref().unwrap();
                assert!(message.contains("RNNoise") && message.contains("DeepFilterNet unavailable"));
                assert_eq!(*color, AMBER);
            }
            app.finish_test_playback(Ok(()));
            assert_eq!(app.mic_test.status.as_ref().unwrap().1, if bypass { MUTED } else { AMBER },
                "successful playback must not erase the fallback information");
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

        assert!(done.send((vec![0.2; 480], ModelState::DeepFilter)).is_ok(), "clicks reuse pending work instead of spawning competing cleaners");
        let cleaning = app.mic_test.cleaning.as_ref().unwrap();
        assert_eq!(cleaning.params, params, "the result keeps the settings it was actually cleaned with");
        assert!(cleaning.then_play, "request playback once the result is ready");
    }

    #[test]
    fn an_old_cleaner_cannot_interrupt_a_new_recording() {
        for recording_fails in [false, true] {
            for failed in [false, true] {
                let mut app = App::stopped(Settings::default());
                let done = pending_cleanup(&mut app, true);
                app.stop_test();
                let recorder = Recorder::from_samples_for_test(&[0.25; 480]);
                if recording_fails { recorder.report_error_for_test("Microphone disconnected"); }
                app.mic_test.recording = Some((recorder, Instant::now()));
                app.mic_test.status = Some(("Speak normally…".into(), AMBER));
                app.poll_mic_test();
                if failed {
                    drop(done);
                } else {
                    done.send((vec![0.05; 480], ModelState::DeepFilterFailed)).unwrap();
                }

                app.poll_mic_test();

                assert_eq!(app.mic_test.recording(), !recording_fails);
                assert!(app.mic_test.cleaning.is_none() && app.mic_test.player.is_none());
                assert_eq!(app.mic_test.status, Some(if recording_fails {
                    ("Microphone disconnected".into(), RED)
                } else {
                    ("Speak normally…".into(), AMBER)
                }));
                assert_eq!(app.mic_test.cleaned.is_none(), failed, "a useful old preview is kept if recording fails");
                assert!(!app.settings.setup.tested, "old work must not start playback");
            }
        }
    }

    #[test]
    fn a_new_take_waits_for_the_previous_cleaner_and_discards_its_result() {
        for failed in [false, true] {
            let mut app = App::stopped(Settings { model: crate::dsp::Model::Rnnoise, ..Settings::default() });
            let done = pending_cleanup(&mut app, true);
            let old_raw = Arc::clone(app.mic_test.raw.as_ref().unwrap());
            app.stop_test();
            app.mic_test.recording = Some((Recorder::from_samples_for_test(&[0.25; 480]), Instant::now() - TEST_LENGTH));

            app.poll_mic_test();

            let raw = Arc::clone(app.mic_test.raw.as_ref().unwrap());
            assert!(!Arc::ptr_eq(&raw, &old_raw), "the new take was recorded");
            assert!(Arc::ptr_eq(&app.mic_test.cleaning.as_ref().unwrap().raw, &old_raw),
                "the old worker remains tracked until it finishes");
            if failed {
                drop(done);
            } else {
                done.send((vec![0.9; 480], ModelState::DeepFilterFailed)).unwrap();
            }

            app.poll_mic_test();

            assert!(app.mic_test.cleaned.is_none(), "an old result must not be cached for the new take");
            assert_eq!(app.mic_test.status, Some(("Now compare: Raw, then Cleaned".into(), GREEN)));
            let cleaning = app.mic_test.cleaning.as_ref().unwrap();
            assert!(Arc::ptr_eq(&cleaning.raw, &raw), "the latest take is cleaned next");
            assert!(!cleaning.then_play && app.mic_test.player.is_none(), "recording again cancels queued playback");
            let (samples, model) = cleaning.result.recv_timeout(Duration::from_secs(5)).unwrap();
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
        app.mic_test.recording = Some((Recorder::from_samples_for_test(&[0.25; 480]), Instant::now()));
        app.mic_test.status = Some(("Speak normally…".into(), AMBER));
        done.send((vec![0.05; 480], ModelState::DeepFilterFailed)).unwrap();

        app.poll_mic_test();

        assert!(app.mic_test.recording());
        assert!(app.mic_test.cleaning.is_none(), "wait for the new recording instead of cleaning an intermediate take");
        assert!(app.mic_test.cleaned.is_none() && app.mic_test.player.is_none());
        assert!(Arc::ptr_eq(app.mic_test.raw.as_ref().unwrap(), &waiting), "keep the previous take until recording succeeds");
        assert_eq!(app.mic_test.status, Some(("Speak normally…".into(), AMBER)));
    }

    #[test]
    fn stopping_a_preview_cancels_queued_playback_but_keeps_the_cleaned_take() {
        let mut app = App::stopped(Settings::default());
        let params = app.test_params();
        let done = pending_cleanup(&mut app, true);

        app.stop_test();
        done.send((vec![0.25; 480], ModelState::DeepFilter)).unwrap();
        app.poll_mic_test();

        assert!(!app.mic_test.busy());
        assert!(app.mic_test.status.is_none(), "a cancelled preview never opens an audio device");
        let (samples, cleaned_params, _) = app.mic_test.cleaned.as_ref().unwrap();
        assert_eq!(samples.as_slice(), &[0.25; 480]);
        assert_eq!(*cleaned_params, params);
        assert!(!app.settings.setup.tested, "cancelling is not a successful listen");
    }

    #[test]
    fn the_cleaned_button_can_cancel_playback_while_cleanup_is_running() {
        let mut app = App::stopped(Settings::default());
        app.mic_test.raw = Some(Arc::new(vec![0.1; 480]));
        let done = pending_cleanup(&mut app, false);
        let ctx = egui::Context::default();
        let draw = |app: &mut App, events| {
            ctx.run_ui(egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(760.0, 100.0))),
                events,
                ..Default::default()
            }, |ui| app.draw_mic_test(ui))
        };
        draw(&mut app, Vec::new());
        let pos = draw(&mut app, Vec::new()).shapes.iter().find_map(|shape| match &shape.shape {
            egui::Shape::Text(text) if text.galley.text() == "⏵ Cleaned" => Some(text.visual_bounding_rect().center()),
            _ => None,
        }).expect("Cleaned control is visible");

        for expected in [true, false] {
            for pressed in [true, false] {
                draw(&mut app, vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE },
                ]);
            }
            assert_eq!(app.mic_test.cleaning.as_ref().unwrap().then_play, expected, "clicking again cancels pending playback");
        }
        done.send((vec![0.25; 480], ModelState::DeepFilter)).unwrap();
        app.poll_mic_test();
        assert!(!app.mic_test.busy());
        assert_eq!(app.mic_test.status, Some(("Playback stopped".into(), MUTED)));
        assert_eq!(app.mic_test.cleaned.as_ref().unwrap().0.as_slice(), &[0.25; 480]);
        assert!(!app.settings.setup.tested, "cancelled playback must never open a device or complete setup");
    }

    #[test]
    #[ignore = "writes preview-control screenshots; needs Python with Pillow"]
    fn snapshot_pending_mic_preview() {
        let mut app = App::stopped(Settings::default());
        app.mic_test.raw = Some(Arc::new(vec![0.1; 480]));
        let _done = pending_cleanup(&mut app, true);
        app.mic_test.status = Some(("Applying your settings…".into(), CYAN));
        let path = super::super::snapshot::snapshot(&mut app, "mic-preview-pending", egui::vec2(760.0, 1100.0), 1.0, true);
        println!("SNAPSHOT {}", path.display());
    }

    #[test]
    fn failed_playback_preserves_the_preview_for_retry_without_completing_setup() {
        let mut app = App::stopped(Settings::default());
        let raw = Arc::new(vec![0.1; 480]);
        let cleaned = Arc::new(vec![0.05; 480]);
        app.mic_test.raw = Some(Arc::clone(&raw));
        app.mic_test.cleaned = Some((Arc::clone(&cleaned), app.test_params(), ModelState::DeepFilter));

        app.finish_test_playback(Err("Headphones disconnected".into()));

        assert!(!app.mic_test.busy());
        assert!(!app.settings.setup.tested && app.dirty_since.is_none());
        assert_eq!(app.mic_test.status, Some(("Playback stopped: Headphones disconnected".into(), RED)));
        assert!(Arc::ptr_eq(app.mic_test.raw.as_ref().unwrap(), &raw));
        assert!(Arc::ptr_eq(&app.mic_test.cleaned.as_ref().unwrap().0, &cleaned));

        app.finish_test_playback(Ok(()));
        assert!(app.settings.setup.tested && app.dirty_since.is_some(), "a successful retry completes the checklist");
        assert_eq!(app.mic_test.status.as_ref().unwrap().1, MUTED);
    }
}
