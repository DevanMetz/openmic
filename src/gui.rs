//! egui front end: routing, processing controls, meters, soundboard.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{
    self, CentralPanel, Color32, ComboBox, ProgressBar, RichText, ScrollArea, Slider,
};

use crate::config::{self, Settings};
use crate::decode;
use crate::default_mic;
use crate::denoise::ModelState;
use crate::dsp::{self, Model, Params};
use crate::engine::{list_devices, Engine};
use crate::viz::{self, Focus};

pub(crate) const CYAN: Color32 = Color32::from_rgb(0x38, 0xbd, 0xf8);
pub(crate) const GREEN: Color32 = Color32::from_rgb(0x22, 0xc5, 0x5e);
pub(crate) const AMBER: Color32 = Color32::from_rgb(0xfb, 0xbf, 0x24);
pub(crate) const RED: Color32 = Color32::from_rgb(0xf8, 0x71, 0x71);
const MUTED: Color32 = Color32::from_rgb(0x94, 0xa3, 0xb8);

#[derive(PartialEq)]
enum Tab {
    Mic,
    Soundboard,
}

type Status = (String, Color32);

pub struct App {
    settings: Settings,
    inputs: Vec<String>,
    outputs: Vec<String>,
    tab: Tab,
    engine: Option<Engine>,
    status: Status,
    sound_status: Status,
    selected: usize,
    playing_name: Option<String>,
    warn_until: Option<(String, Instant)>,
    dirty_since: Option<Instant>,
    /// While VB-Cable is missing, devices are re-listed periodically so an
    /// install is picked up without pressing Refresh.
    last_device_poll: Instant,
    /// (running, processed output, setting) the Windows default microphone
    /// was last reconciled for.
    default_mic_synced: Option<(bool, String, bool)>,
    /// The processing setting under the pointer this frame.
    focus: Option<Focus>,
}

impl App {
    pub fn new(settings: Settings) -> Self {
        let mut app = Self {
            settings,
            inputs: Vec::new(),
            outputs: Vec::new(),
            tab: Tab::Mic,
            engine: None,
            status: ("Stopped".into(), MUTED),
            sound_status: ("Ready".into(), Color32::WHITE),
            selected: 0,
            playing_name: None,
            warn_until: None,
            dirty_since: None,
            last_device_poll: Instant::now(),
            default_mic_synced: None,
            focus: None,
        };
        // Reflect the actual registry state, like the Python app did.
        app.settings.start_with_windows = config::startup_enabled();

        let host = cpal_host();
        app.inputs = list_devices(&host, false);
        app.outputs = list_devices(&host, true);
        // Keep saved devices even if absent: a USB mic can appear after a
        // Windows-startup launch, and must not be silently swapped out. The
        // processed output goes back to VB-Cable on every launch.
        choose_routes(&mut app.settings, &app.inputs, &app.outputs, true, false);

        if app.settings.auto_start {
            app.start_engine();
        }
        app
    }

    fn params(&self) -> Params {
        let s = &self.settings;
        Params {
            model: s.model,
            strength: s.strength.clamp(0.0, 1.0),
            input_gain: dsp::db_to_gain(s.input_gain_db),
            output_gain: dsp::db_to_gain(s.output_gain_db),
            monitor_gain: s.monitor_volume.clamp(0.0, 1.0),
            sound_gain: s.sound_volume.clamp(0.0, 1.0),
            gate_enabled: s.gate,
            gate_threshold_db: s.gate_threshold_db,
            highpass: s.highpass,
            highpass_hz: s.highpass_hz.clamp(*dsp::HIGHPASS_RANGE.start(), *dsp::HIGHPASS_RANGE.end()),
            voice_gate: s.voice_gate,
            voice_threshold: s.voice_threshold.clamp(0.0, 1.0),
            bypass: s.bypass,
            mute: s.mute,
            monitor_on: s.monitor,
        }
    }

    fn apply_live(&mut self) {
        if let Some(engine) = &self.engine {
            engine.set_params(self.params());
        }
        self.dirty_since.get_or_insert(Instant::now());
    }

    fn touch(&mut self) {
        self.dirty_since.get_or_insert(Instant::now());
    }

    fn start_engine(&mut self) {
        if self.settings.output.is_empty() {
            self.status = ("Install VB-Cable or choose a processed output".into(), AMBER);
            return;
        }
        match Engine::start(
            &self.settings.microphone,
            &self.settings.output,
            &self.settings.monitor_output,
            self.params(),
        ) {
            Ok(engine) => {
                self.engine = Some(engine);
                self.status = ("Starting…".into(), AMBER);
            }
            Err(e) => self.status = (short(&format!("{:#}", e)), RED),
        }
    }

    fn stop_engine(&mut self, announce: bool) {
        if let Some(engine) = self.engine.take() {
            engine.stop();
        }
        self.playing_name = None;
        if announce {
            self.status = ("Stopped".into(), MUTED);
            self.sound_status = ("Ready".into(), Color32::WHITE);
        }
    }

    /// Live route change: stop old engine, start new, hand the clip playhead over.
    fn restart_engine(&mut self) {
        let remaining = self.engine.as_ref().and_then(Engine::take_remaining_clip);
        self.stop_engine(false);
        self.status = ("Switching route…".into(), AMBER);
        self.start_engine();
        if let (Some(engine), Some(samples)) = (&self.engine, remaining) {
            engine.play_sound(Arc::new(samples));
        }
    }

    /// Re-list devices. `replace_missing` (the Refresh button) swaps out
    /// choices whose device is gone; background polls only fill empty ones.
    fn refresh_devices(&mut self, replace_missing: bool) {
        let host = cpal_host();
        let before = (
            self.settings.microphone.clone(),
            self.settings.output.clone(),
            self.settings.monitor_output.clone(),
        );
        let had_cable = cable_input(&self.outputs).is_some();
        self.inputs = list_devices(&host, false);
        self.outputs = list_devices(&host, true);
        let cable_appeared = !had_cable && cable_input(&self.outputs).is_some();
        choose_routes(
            &mut self.settings,
            &self.inputs,
            &self.outputs,
            cable_appeared,
            replace_missing,
        );
        let after = (
            self.settings.microphone.clone(),
            self.settings.output.clone(),
            self.settings.monitor_output.clone(),
        );
        if before == after {
            return;
        }
        self.touch();
        if self.engine.is_some() {
            self.restart_engine();
        } else if cable_appeared && self.settings.auto_start {
            // Nothing could run before VB-Cable existed; start now it does.
            self.start_engine();
        }
    }

    fn play_selected(&mut self) {
        let Some(path) = self.settings.sounds.get(self.selected).cloned() else {
            self.sound_status = ("Select a clip first".into(), AMBER);
            return;
        };
        let Some(engine) = &self.engine else {
            self.sound_status = ("Start OpenMic before playing a clip".into(), AMBER);
            return;
        };
        match decode::load_clip(&path) {
            Ok(samples) => {
                engine.play_sound(Arc::new(samples));
                let name = file_name(&path);
                self.sound_status = (format!("Playing · {name}"), GREEN);
                self.playing_name = Some(name);
            }
            Err(e) => self.sound_status = (short(&format!("{:#}", e)), RED),
        }
    }

    fn add_sounds(&mut self) {
        let Some(paths) = rfd::FileDialog::new()
            .add_filter(
                "Audio files",
                &["wav", "flac", "ogg", "mp3", "aiff", "aif"],
            )
            .add_filter("All files", &["*"])
            .pick_files()
        else {
            return;
        };
        let mut invalid = Vec::new();
        for path in paths {
            if self.settings.sounds.contains(&path) {
                continue;
            }
            match decode::load_clip(&path) {
                Ok(_) => self.settings.sounds.push(path),
                Err(_) => invalid.push(file_name(&path)),
            }
        }
        if !invalid.is_empty() {
            self.sound_status = (format!("Could not read: {}", invalid.join(", ")), RED);
        }
        self.touch();
    }

    fn remove_selected(&mut self) {
        if self.selected < self.settings.sounds.len() {
            self.settings.sounds.remove(self.selected);
            self.selected = self.selected.min(self.settings.sounds.len().saturating_sub(1));
            self.touch();
        }
    }

    fn toggle_startup(&mut self, enabled: bool) {
        if let Err(e) = config::set_startup(enabled) {
            self.settings.start_with_windows = !enabled;
            self.status = (short(&format!("{:#}", e)), RED);
        } else {
            self.status = (
                format!(
                    "Windows startup {}",
                    if enabled { "enabled" } else { "disabled" }
                ),
                GREEN,
            );
        }
    }

    fn poll_engine(&mut self) {
        let err = self.engine.as_ref().and_then(Engine::take_error);
        if let Some(e) = err {
            self.stop_engine(false);
            self.status = (short(&e), RED);
            return;
        }
        // Stream glitches are transient: surface them briefly, keep running.
        if let Some(w) = self.engine.as_ref().and_then(Engine::take_warning) {
            self.warn_until = Some((short(&w), Instant::now()));
        }
        if let Some((msg, at)) = &self.warn_until {
            if at.elapsed() < Duration::from_secs(2) {
                self.status = (msg.clone(), AMBER);
            } else {
                self.warn_until = None;
            }
        }
        let playing = self.engine.as_ref().map(Engine::sound_playing);
        if self.playing_name.is_some() && playing == Some(false) {
            self.playing_name = None;
            self.sound_status = ("Ready".into(), Color32::WHITE);
        }
    }
    /// Discord hears OpenMic through VB-Cable; walk the user to a working route.
    fn draw_cable_hint(&mut self, ui: &mut egui::Ui) {
        match cable_input(&self.outputs).cloned() {
            None => {
                ui.label(
                    RichText::new("VB-Cable isn't installed: Discord needs it to hear OpenMic.")
                        .color(AMBER),
                );
                ui.horizontal(|ui| {
                    if ui.button("Get VB-Cable").clicked() {
                        open_url(VB_CABLE_URL);
                    }
                    ui.weak("Run its setup as administrator; OpenMic switches to it automatically.");
                });
            }
            Some(cable) if self.settings.output != cable => {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Discord can't hear this output.").color(AMBER));
                    if ui.button("Use VB-Cable").clicked() {
                        self.settings.output = cable;
                        self.touch();
                        if self.engine.is_some() {
                            self.restart_engine();
                        }
                    }
                });
            }
            Some(_) => {
                let changed = ui
                    .checkbox(
                        &mut self.settings.default_mic,
                        "Make CABLE Output my Windows default mic while running",
                    )
                    .on_hover_text(
                        "Apps set to the default microphone (Discord's default) hear your \
                         cleaned voice. Your previous default comes back when OpenMic stops.",
                    )
                    .changed();
                if changed {
                    self.touch();
                }
                if self.settings.default_mic {
                    ui.weak("In Discord: leave Voice & Video > Input Device on Default");
                } else {
                    ui.weak("In Discord: Voice & Video > Input Device > CABLE Output");
                }
            }
        }
    }

    /// Point the Windows default microphone at VB-Cable while processing into
    /// it, and put the user's own default back otherwise. Runs only when the
    /// relevant state changes.
    fn sync_default_mic(&mut self) {
        let running = self.engine.is_some();
        let key = (running, self.settings.output.clone(), self.settings.default_mic);
        if self.default_mic_synced.as_ref() == Some(&key) {
            return;
        }
        self.default_mic_synced = Some(key);

        let Ok(Some(cable_mic)) = default_mic::cable_capture_id() else {
            return;
        };
        let want = running
            && self.settings.default_mic
            && cable_input(&self.outputs) == Some(&self.settings.output);
        match (self.settings.saved_default_mic.clone(), want) {
            (None, true) => {
                let Ok(before) = default_mic::current() else { return };
                if before.console == cable_mic && before.communications == cable_mic {
                    return; // already the default; nothing to restore later
                }
                // Persist first so a crash can't lose the user's default.
                self.settings.saved_default_mic = Some(before);
                let _ = self.settings.save();
                if let Err(e) = default_mic::set(&cable_mic) {
                    self.settings.saved_default_mic = None;
                    let _ = self.settings.save();
                    self.warn_until = Some((short(&format!("{e:#}")), Instant::now()));
                }
            }
            (Some(saved), false) => {
                if default_mic::restore(&saved, &cable_mic).is_ok() {
                    self.settings.saved_default_mic = None;
                    let _ = self.settings.save();
                }
            }
            _ => {}
        }
    }

    fn draw_mic_tab(&mut self, ui: &mut egui::Ui) {
        ui.group(|ui| {
            ui.strong(RichText::new("ROUTING").color(CYAN));
            macro_rules! route_row {
                ($label:expr, $id:expr, $field:ident, $pool:expr) => {{
                    let changed = ui
                        .horizontal(|ui| {
                            ui.label($label);
                            let field = &mut self.settings.$field;
                            combo(ui, $id, field, &$pool).changed()
                        })
                        .inner;
                    if changed {
                        self.touch();
                        if self.engine.is_some() {
                            self.restart_engine();
                        }
                    }
                }};
            }
            route_row!("Microphone", 0, microphone, self.inputs);
            route_row!("Processed output", 1, output, self.outputs);
            route_row!("Monitor output", 2, monitor_output, self.outputs);
            ui.horizontal(|ui| {
                if ui.button("Refresh").clicked() {
                    self.refresh_devices(true);
                }
                ui.weak(RichText::new("Changes apply live").color(GREEN));
            });
            self.draw_cable_hint(ui);
        });

        ui.add_space(8.0);
        ui.group(|ui| {
            ui.strong(RichText::new("PROCESSING").color(CYAN));
            let mut changed = false;
            // Which setting the pointer is on, so the scope can highlight it.
            let mut focus = None;
            let mut watch = |r: egui::Response, f: Focus| {
                if r.hovered() || r.dragged() {
                    focus = Some(f);
                }
                r.changed()
            };
            ui.horizontal(|ui| {
                let label = ui.label("Model");
                watch(label, Focus::Model);
                let combo = ComboBox::from_id_salt("model")
                    .selected_text(model_name(self.settings.model))
                    .show_ui(ui, |ui| {
                        for model in [Model::DeepFilter, Model::Rnnoise] {
                            changed |= ui
                                .selectable_value(&mut self.settings.model, model, model_name(model))
                                .changed();
                        }
                    });
                watch(combo.response, Focus::Model);
            });
            ui.horizontal(|ui| {
                changed |= watch(
                    ui.checkbox(&mut self.settings.voice_gate, "Voice gate")
                        .on_hover_text("Silence everything that isn't speech, however loud"),
                    Focus::VoiceGate,
                );
                changed |= watch(
                    ui.checkbox(&mut self.settings.highpass, "Rumble filter")
                        .on_hover_text("Cut low rumble: desk bumps, hum, handling noise"),
                    Focus::Rumble,
                );
                changed |= watch(ui.checkbox(&mut self.settings.gate, "Level gate"), Focus::LevelGate);
            });
            ui.horizontal(|ui| {
                changed |= ui
                    .checkbox(&mut self.settings.bypass, "Bypass reduction")
                    .changed();
                changed |= ui.checkbox(&mut self.settings.mute, "Mute microphone").changed();
                if ui.button("Reset").clicked() {
                    self.settings.model = Model::default();
                    self.settings.highpass = true;
                    self.settings.highpass_hz = dsp::HIGHPASS_HZ;
                    self.settings.voice_gate = true;
                    self.settings.voice_threshold = dsp::VOICE_THRESHOLD;
                    self.settings.strength = 1.0;
                    self.settings.input_gain_db = 0.0;
                    self.settings.output_gain_db = 0.0;
                    self.settings.gate = false;
                    self.settings.gate_threshold_db = -50.0;
                    self.settings.bypass = false;
                    self.settings.mute = false;
                    self.settings.monitor_volume = 1.0;
                    changed = true;
                }
            });
            if changed {
                self.apply_live();
            }
            self.focus = focus;
        });

        ui.add_space(8.0);
        ui.group(|ui| {
            ui.strong(RichText::new("MONITOR & LEVELS").color(CYAN));
            ui.horizontal(|ui| {
                let mut changed =
                    ui.checkbox(&mut self.settings.monitor, "Headphone monitor").changed();
                changed |= ui
                    .add(
                        Slider::new(&mut self.settings.monitor_volume, 0.0..=1.0)
                            .custom_formatter(|v, _| format!("{:.0}%", v * 100.0)),
                    )
                    .changed();
                if changed {
                    self.apply_live();
                }
            });
            let snapshot = self.engine.as_ref().map(Engine::stats);
            meter(
                ui,
                "Input",
                snapshot.map(|s| s.in_peak),
                CYAN,
            );
            meter(
                ui,
                "Output",
                snapshot.map(|s| s.out_peak),
                GREEN,
            );
            if let Some(stats) = snapshot {
                let warning_fresh = self
                    .warn_until
                    .as_ref()
                    .is_some_and(|(_, at)| at.elapsed() < Duration::from_secs(2));
                if !warning_fresh {
                    let voice = format!("voice {:.0}%", stats.prob * 100.0);
                    self.status = match stats.model {
                        ModelState::DeepFilterLoading => {
                            (format!("Loading DeepFilterNet… · {voice}"), AMBER)
                        }
                        ModelState::DeepFilterFailed => {
                            (format!("DeepFilterNet unavailable, using RNNoise · {voice}"), AMBER)
                        }
                        ModelState::DeepFilter | ModelState::Rnnoise => {
                            (format!("Running · {voice}"), GREEN)
                        }
                    };
                }
            }
        });

        ui.add_space(8.0);
        ui.group(|ui| {
            ui.strong(RichText::new("LIVE SCOPE · drag to adjust").color(CYAN));
            let frames = self.engine.as_ref().map(Engine::scope).unwrap_or_default();
            if viz::scope(ui, &frames, &mut self.settings, self.focus) {
                self.apply_live();
            }
        });
    }

    fn draw_sound_tab(&mut self, ui: &mut egui::Ui) {
        ui.label("Play clips directly into your processed microphone output.");
        let sounds = self.settings.sounds.clone();
        ScrollArea::vertical()
            .id_salt("sounds")
            .max_height(ui.available_height() - 140.0)
            .show(ui, |ui| {
                for (i, path) in sounds.iter().enumerate() {
                    let name = file_name(path);
                    let resp =
                        ui.selectable_label(i == self.selected, RichText::new(name.clone()));
                    if resp.clicked() {
                        self.selected = i;
                    }
                    if resp.double_clicked() {
                        self.selected = i;
                        self.play_selected();
                    }
                }
            });
        ui.horizontal(|ui| {
            if ui.button("Add clips").clicked() {
                self.add_sounds();
            }
            if ui.button(RichText::new("Play").color(Color32::BLACK)).clicked() {
                self.play_selected();
            }
            if ui.button("Stop").clicked() {
                if let Some(engine) = &self.engine {
                    engine.stop_sound();
                }
                self.playing_name = None;
                self.sound_status = ("Ready".into(), Color32::WHITE);
            }
            if ui.button(RichText::new("Remove").color(Color32::from_rgb(0xfe, 0xca, 0xca))).clicked() {
                self.remove_selected();
            }
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label("Sound volume");
            if ui
                .add(
                    Slider::new(&mut self.settings.sound_volume, 0.0..=1.0)
                        .custom_formatter(|v, _| format!("{:.0}%", v * 100.0)),
                )
                .changed()
            {
                self.apply_live();
            }
        });
        ui.add_space(4.0);
        ui.weak(RichText::new(format!(
            "{}   (WAV, FLAC, OGG, MP3, AIFF supported)",
            self.sound_status.0
        )).color(self.sound_status.1));
    }
}

impl eframe::App for App {
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.stop_engine(false);
        self.sync_default_mic();
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        // The live scope animates at ~30 fps while processing.
        let frame_time = if self.engine.is_some() { 33 } else { 100 };
        ctx.request_repaint_after(Duration::from_millis(frame_time));
        self.poll_engine();
        self.sync_default_mic();
        if cable_input(&self.outputs).is_none()
            && self.last_device_poll.elapsed() >= Duration::from_secs(3)
        {
            self.last_device_poll = Instant::now();
            self.refresh_devices(false);
        }

        if let Some(t) = self.dirty_since {
            if t.elapsed() >= Duration::from_millis(300) {
                let _ = self.settings.save();
                self.dirty_since = None;
            }
        }

        CentralPanel::default().show(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.heading("OpenMic");
                ui.weak(dsp_version());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.weak("Clean voice. Instant sounds. Fully local.");
                });
            });
            ui.add_space(6.0);

            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.tab, Tab::Mic, "Microphone");
                ui.selectable_value(&mut self.tab, Tab::Soundboard, "Soundboard");
            });
            ui.separator();
            match self.tab {
                Tab::Mic => self.draw_mic_tab(ui),
                Tab::Soundboard => self.draw_sound_tab(ui),
            }

            // Footer follows the content; the fixed window is sized to fit.
            ui.add_space(10.0);
            ui.separator();
            ui.horizontal(|ui| {
                let running = self.engine.is_some();
                let btn = egui::Button::new(if running {
                    RichText::new("Stop OpenMic").color(Color32::BLACK)
                } else {
                    RichText::new("Start OpenMic").color(Color32::BLACK)
                })
                .fill(if running { RED } else { GREEN });
                if ui.add(btn).clicked() {
                    let _ = self.settings.save();
                    if running {
                        self.stop_engine(true);
                    } else {
                        self.start_engine();
                    }
                }
                ui.label(RichText::new(&self.status.0).color(self.status.1));
            });
            ui.horizontal(|ui| {
                if ui
                    .checkbox(&mut self.settings.start_with_windows, "Start with Windows")
                    .changed()
                {
                    self.toggle_startup(self.settings.start_with_windows);
                }
                if ui
                    .checkbox(
                        &mut self.settings.auto_start,
                        "Start processing automatically",
                    )
                    .changed()
                {
                    self.touch();
                }
            });
        });
    }
}

// ---- small helpers ---------------------------------------------------------

fn cpal_host() -> cpal::Host {
    cpal::default_host()
}

fn dsp_version() -> String {
    format!("v{}", env!("CARGO_PKG_VERSION"))
}

fn short(msg: &str) -> String {
    msg.chars().take(64).collect()
}

fn file_name(path: &PathBuf) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

fn combo(ui: &mut egui::Ui, id: usize, value: &mut String, pool: &[String]) -> egui::Response {
    ComboBox::from_id_salt(id)
        .width(360.0)
        .selected_text(value.as_str())
        .show_ui(ui, |ui| {
            for name in pool {
                ui.selectable_value(value, name.clone(), name);
            }
        })
        .response
}

fn meter(ui: &mut egui::Ui, label: &str, peak: Option<f32>, color: Color32) {
    ui.horizontal(|ui| {
        ui.label(label);
        let pct = dsp::meter_percent(peak.unwrap_or(0.0));
        ui.add(
            ProgressBar::new(pct / 100.0)
                .desired_width(ui.available_width())
                .fill(color),
        );
    });
}

fn model_name(model: Model) -> &'static str {
    match model {
        Model::DeepFilter => "DeepFilterNet 3 (best)",
        Model::Rnnoise => "RNNoise (light)",
    }
}

const VB_CABLE_URL: &str = "https://vb-audio.com/Cable/";

/// VB-Audio Virtual Cable's playback side (Discord records "CABLE Output").
fn cable_input(outputs: &[String]) -> Option<&String> {
    outputs.iter().find(|n| n.contains("CABLE Input"))
}

/// Keep valid device choices and fill missing ones: the processed output
/// prefers VB-Cable and the monitor avoids it. When VB-Cable has just been
/// installed or the app launches (`prefer_cable`), switch the processed
/// output over to it.
/// Without VB-Cable the processed output stays unset: defaulting to the
/// first device would play the mic out of the speakers (feedback).
/// Choices whose device is absent are only replaced if `replace_missing`.
fn choose_routes(
    s: &mut Settings,
    inputs: &[String],
    outputs: &[String],
    prefer_cable: bool,
    replace_missing: bool,
) {
    let needs = |current: &String, pool: &[String]| {
        current.is_empty() || (replace_missing && !pool.contains(current))
    };
    if needs(&s.microphone, inputs) {
        s.microphone = inputs.first().cloned().unwrap_or_default();
    }
    let cable = cable_input(outputs);
    if (prefer_cable && cable.is_some()) || needs(&s.output, outputs) {
        s.output = cable.cloned().unwrap_or_default();
    }
    if needs(&s.monitor_output, outputs) {
        s.monitor_output = outputs
            .iter()
            .find(|n| Some(*n) != cable)
            .or(outputs.first())
            .cloned()
            .unwrap_or_default();
    }
}

fn open_url(url: &str) {
    // Explorer hands URLs to the default browser.
    let _ = std::process::Command::new("explorer").arg(url).spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn fresh_install_routes_voice_to_vb_cable_and_monitor_elsewhere() {
        let mut s = Settings::default();
        let inputs = names(&["Mic (USB)"]);
        let outputs = names(&["Speakers (Realtek)", "CABLE Input (VB-Audio Virtual Cable)"]);
        choose_routes(&mut s, &inputs, &outputs, false, false);
        assert_eq!(s.microphone, "Mic (USB)");
        assert_eq!(s.output, "CABLE Input (VB-Audio Virtual Cable)");
        assert_eq!(s.monitor_output, "Speakers (Realtek)");
    }

    #[test]
    fn never_defaults_the_voice_to_speakers() {
        let mut s = Settings::default();
        let outputs = names(&["Speakers (Realtek)"]);
        choose_routes(&mut s, &names(&["Mic (USB)"]), &outputs, false, false);
        assert_eq!(s.output, "", "would feed the mic back out of the speakers");
        assert_eq!(s.monitor_output, "Speakers (Realtek)");
    }

    #[test]
    fn launch_always_returns_the_voice_to_vb_cable() {
        let mut s = Settings {
            microphone: "Mic (USB)".into(),
            output: "Speakers (Realtek)".into(),
            ..Default::default()
        };
        let outputs = names(&["Speakers (Realtek)", "CABLE Input (VB-Audio Virtual Cable)"]);
        choose_routes(&mut s, &names(&["Mic (USB)"]), &outputs, true, false);
        assert_eq!(s.output, "CABLE Input (VB-Audio Virtual Cable)");
    }

    #[test]
    fn keeps_valid_choices_until_vb_cable_is_installed() {
        let mut s = Settings {
            microphone: "Mic (USB)".into(),
            output: "Speakers (Realtek)".into(),
            monitor_output: "Headphones".into(),
            ..Default::default()
        };
        let inputs = names(&["Mic (USB)"]);
        let outputs = names(&["Speakers (Realtek)", "Headphones"]);
        choose_routes(&mut s, &inputs, &outputs, false, false);
        assert_eq!(s.output, "Speakers (Realtek)");

        let outputs = names(&[
            "Speakers (Realtek)",
            "Headphones",
            "CABLE Input (VB-Audio Virtual Cable)",
        ]);
        choose_routes(&mut s, &inputs, &outputs, false, true);
        assert_eq!(s.output, "Speakers (Realtek)", "a choice made this session survives a refresh");
        choose_routes(&mut s, &inputs, &outputs, true, false);
        assert_eq!(s.output, "CABLE Input (VB-Audio Virtual Cable)");
        assert_eq!(s.monitor_output, "Headphones");
    }

    #[test]
    fn late_usb_mic_is_not_swapped_out_except_by_refresh() {
        let mut s = Settings {
            microphone: "Mic (USB)".into(),
            output: "CABLE Input (VB-Audio Virtual Cable)".into(),
            ..Default::default()
        };
        let outputs = names(&["CABLE Input (VB-Audio Virtual Cable)", "Speakers"]);
        let without_usb = names(&["Webcam Mic"]);
        choose_routes(&mut s, &without_usb, &outputs, false, false);
        assert_eq!(s.microphone, "Mic (USB)", "startup/poll keeps the saved mic");
        choose_routes(&mut s, &without_usb, &outputs, false, true);
        assert_eq!(s.microphone, "Webcam Mic", "Refresh replaces a device that is gone");
    }
}
