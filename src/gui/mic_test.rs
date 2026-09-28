//! "Test my mic": record a few seconds, then play them back raw and as
//! Discord would hear them, on your own headphones or speakers. Cleaning
//! uses the current settings each time, so you can tune and listen again.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;
use eframe::egui::{self, RichText};

use super::{short, App, Status, AMBER, CYAN, GREEN, MUTED, RED};
use crate::denoise::Cleaner;
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

#[derive(Default)]
pub(super) struct MicTest {
    recording: Option<(Recorder, Instant)>,
    /// The take as recorded (mono, DSP rate).
    raw: Option<Arc<Vec<f32>>>,
    /// The take cleaned, and the settings it was cleaned with.
    cleaned: Option<(Arc<Vec<f32>>, Params)>,
    /// Cleaning in the background, and whether to play it when done.
    cleaning: Option<(Receiver<Vec<f32>>, Params, bool)>,
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
}

impl App {
    /// The settings a test is cleaned with: yours, but never muted (a muted
    /// test would play silence).
    fn test_params(&self) -> Params {
        Params { mute: false, ..self.params() }
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
        self.mic_test.player = None;
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
        let (done, job) = crossbeam_channel::bounded(1);
        let spawned = std::thread::Builder::new().name("openmic-mic-test".into()).spawn(move || {
            let _ = done.send(Cleaner::process_recording(&raw, &params));
        });
        match spawned {
            Ok(_) => self.mic_test.cleaning = Some((job, params, then_play)),
            Err(e) => self.mic_test.status = Some((short(&format!("{e:#}")), RED)),
        }
    }

    fn play_test(&mut self, version: Version) {
        let samples = match version {
            Version::Raw => self.mic_test.raw.clone(),
            Version::Cleaned => match &self.mic_test.cleaned {
                Some((cleaned, params)) if *params == self.test_params() => Some(Arc::clone(cleaned)),
                // Settings changed since it was cleaned: clean again, then play.
                _ => {
                    self.mic_test.player = None;
                    self.clean_test(true);
                    self.mic_test.status = Some(("Applying your settings…".into(), CYAN));
                    return;
                }
            },
        };
        let Some(samples) = samples else { return };
        self.mic_test.player = None; // stop what's playing first
        match Player::play(&self.test_output(), &samples) {
            Ok(player) => {
                self.mic_test.player = Some((player, version));
                self.mic_test.status = Some((
                    match version {
                        Version::Raw => "Playing your raw microphone".into(),
                        Version::Cleaned => "Playing what Discord hears".into(),
                    },
                    GREEN,
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
            if let Some(e) = recorder.take_error() {
                test.recording = None;
                test.status = Some((short(&e), RED));
            } else if started.elapsed() >= TEST_LENGTH {
                let (recorder, _) = test.recording.take().expect("checked above");
                let take = recorder.finish().and_then(|take| take.map(|t| t.samples()).transpose());
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
        if let Some((job, params, then_play)) = &test.cleaning
            && let Ok(cleaned) = job.try_recv()
        {
            let (params, then_play) = (*params, *then_play);
            test.cleaned = Some((Arc::new(cleaned), params));
            test.cleaning = None;
            if then_play {
                self.play_test(Version::Cleaned);
            }
        }
        if self.mic_test.player.as_ref().is_some_and(|(player, _)| player.finished()) {
            self.mic_test.player = None;
            self.mic_test.status = Some(("Tune the settings and listen again".into(), MUTED));
            if !self.settings.setup.tested {
                self.settings.setup.tested = true;
                self.touch();
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
                    let playing = self.mic_test.player.as_ref().is_some_and(|(_, v)| *v == version);
                    if ui.selectable_label(playing, label).clicked() {
                        if playing {
                            self.mic_test.player = None;
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
    fn a_recording_is_cleaned_to_the_same_length() {
        let p = Params { model: crate::dsp::Model::Rnnoise, ..Params::default() };
        let tone: Vec<f32> = (0..48_000).map(|i| (i as f32 * 0.05).sin() * 0.2).collect();
        let cleaned = Cleaner::process_recording(&tone, &p);
        assert_eq!(cleaned.len(), tone.len());
        let muted = Cleaner::process_recording(&tone, &Params { mute: true, ..p });
        assert!(muted.iter().all(|s| *s == 0.0), "mute would silence a test: test_params clears it");
    }
}
