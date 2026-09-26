//! egui front end: app state and lifecycle. Each tab draws from its own
//! module; routing rules live in `routes`.

mod mic_tab;
mod routes;
mod settings_tab;
mod sound_tab;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use eframe::egui::{self, CentralPanel, Color32, RichText, ScrollArea, Slider, ViewportCommand};

use crate::config::{self, Settings};
use crate::default_mic;
use crate::dictation::{self, Dictation};
use crate::dsp::{self, Clip, Params};
use crate::engine::{list_devices, Engine};
use crate::hotkeys::{self, Hotkeys};
use crate::record::{Recorder, Source, Take};
use crate::tray::Tray;
use crate::viz::{self, Focus};
use routes::{cable_input, choose_routes};

pub(crate) const CYAN: Color32 = Color32::from_rgb(0x38, 0xbd, 0xf8);
pub(crate) const GREEN: Color32 = Color32::from_rgb(0x22, 0xc5, 0x5e);
pub(crate) const AMBER: Color32 = Color32::from_rgb(0xfb, 0xbf, 0x24);
pub(crate) const VIOLET: Color32 = Color32::from_rgb(0xa7, 0x8b, 0xfa);
pub(crate) const RED: Color32 = Color32::from_rgb(0xf8, 0x71, 0x71);
pub(crate) const MUTED: Color32 = Color32::from_rgb(0x94, 0xa3, 0xb8);

#[derive(PartialEq)]
enum Tab {
    Mic,
    Soundboard,
    Settings,
}

type Status = (String, Color32);

type DefaultMicState = (bool, String, bool);

/// Successful reconciliations are cached; failed ones retry without doing
/// Windows COM work on every GUI frame. A changed request is always immediate.
#[derive(Default)]
struct DefaultMicSync {
    synced: Option<DefaultMicState>,
    attempted: Option<(DefaultMicState, Instant)>,
}

impl DefaultMicSync {
    fn begin(&mut self, key: &DefaultMicState, now: Instant) -> bool {
        if self.synced.as_ref() == Some(key)
            || self.attempted.as_ref().is_some_and(|(attempted, at)| {
                attempted == key && now.duration_since(*at) < DEVICE_POLL
            })
        {
            return false;
        }
        // A failed transition can have changed one Windows role already, so
        // the previously successful state must also be reconciled on return.
        self.synced = None;
        self.attempted = Some((key.clone(), now));
        true
    }

    fn complete(&mut self, key: DefaultMicState) {
        self.synced = Some(key);
        self.attempted = None;
    }
}

/// Something the tray menu or a global hotkey asked for. Delivered through
/// a channel so it works while the window is hidden.
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    Show,
    ToggleMute,
    ToggleBypass,
    ToggleRunning,
    StopClips,
    PlayPad(PathBuf),
    /// The speech-to-text shortcut went down...
    Dictate,
    /// ...and came back up.
    DictateReleased,
    Quit,
}

impl Command {
    /// What letting go of this command's shortcut does, if anything.
    pub fn released(&self) -> Option<Command> {
        (*self == Command::Dictate).then_some(Command::DictateReleased)
    }
}

/// A setting a global hotkey can be bound to.
#[derive(Clone, Debug, PartialEq)]
enum Binding {
    Mute,
    Bypass,
    StopClips,
    Dictate,
    Pad(PathBuf),
}

pub struct App {
    settings: Settings,
    inputs: Vec<String>,
    outputs: Vec<String>,
    tab: Tab,
    engine: Option<Engine>,
    status: Status,
    sound_status: Status,
    /// Keys of the clips playing as of this frame.
    playing: Vec<u64>,
    /// Decoded clips, so a pad (or its hotkey) plays without a decode delay.
    clip_cache: HashMap<PathBuf, Arc<Vec<f32>>>,
    record_source: Source,
    record_output: String,
    recorder: Option<Recorder>,
    take: Option<Take>,
    record_name: String,
    record_status: Status,
    /// Peak-hold position of the recording level meter, dBFS.
    record_hold: f32,
    warn_until: Option<(String, Instant)>,
    dirty_since: Option<Instant>,
    save_error: Option<String>,
    /// Re-list devices while VB-Cable is missing or a start request is
    /// waiting for a saved device to connect.
    last_device_poll: Instant,
    /// (running, processed output, setting) the Windows default microphone
    /// was last reconciled for.
    default_mic_sync: DefaultMicSync,
    /// The processing setting under the pointer this frame.
    focus: Option<Focus>,
    /// Peak-hold positions of the input/output meters, dBFS.
    hold: [f32; 2],
    /// Start was blocked by a device that isn't connected (e.g. a USB mic
    /// still coming up after a Windows-startup launch); start once it is.
    waiting_for_device: bool,
    /// A running engine lost a device; keep retrying until it comes back
    /// (or the user presses Stop).
    reconnecting: bool,
    /// Name typed into the preset menu's "Save current" field.
    preset_name: String,
    commands: (Sender<Command>, Receiver<Command>),
    tray: Option<Tray>,
    hotkeys: Option<Hotkeys>,
    /// Waiting for the user to press a shortcut for this binding.
    capturing: Option<Binding>,
    hotkey_status: Status,
    /// Speech-to-text model download and transcription.
    dictation: Dictation,
    /// Speech being recorded for dictation.
    dictating: Option<Recorder>,
    dictation_status: Status,
    /// The words most recently recognized, including when typing was blocked.
    last_dictation: String,
    /// The window is hidden to the notification area.
    hidden: bool,
    /// Quit was chosen: let the window close instead of hiding it.
    quitting: bool,
}

impl App {
    /// The real app: tray icon, global hotkeys, and devices. `start_hidden`
    /// launches straight into the notification area (Windows startup).
    pub fn new(
        settings: Settings,
        ctx: &egui::Context,
        start_hidden: bool,
        instance: crate::instance::Instance,
    ) -> Self {
        let mut app = Self::stopped(settings);
        // Reflect the actual registry state, like the Python app did.
        app.settings.start_with_windows = config::startup_enabled();
        if app.settings.start_with_windows {
            // Refresh the entry: the exe may have moved, and older versions
            // registered it without the start-hidden flag.
            let _ = config::set_startup(true);
        }

        app.tray = Tray::new(ctx, app.commands.0.clone()).ok();
        {
            let (commands, ctx) = (app.commands.0.clone(), ctx.clone());
            instance.on_show(move || {
                let _ = commands.send(Command::Show);
                ctx.request_repaint();
            });
        }
        {
            let ctx = ctx.clone();
            app.dictation = Dictation::new(move || ctx.request_repaint());
        }
        match Hotkeys::new(ctx, app.commands.0.clone()) {
            Ok(hotkeys) => app.hotkeys = Some(hotkeys),
            Err(e) => app.hotkey_status = (short(&format!("{e:#}")), RED),
        }
        app.hidden = start_hidden;
        if start_hidden && app.tray.is_none() {
            // Nowhere to restore it from: show the window after all.
            app.hidden = false;
            ctx.send_viewport_cmd(ViewportCommand::Visible(true));
        }

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

    /// App state without a window, tray, hotkeys or devices (also for tests).
    fn stopped(settings: Settings) -> Self {
        Self {
            settings,
            inputs: Vec::new(),
            outputs: Vec::new(),
            tab: Tab::Mic,
            engine: None,
            status: ("Stopped".into(), MUTED),
            sound_status: ("Ready".into(), Color32::WHITE),
            playing: Vec::new(),
            clip_cache: HashMap::new(),
            record_source: Source::Microphone,
            record_output: String::new(),
            recorder: None,
            take: None,
            record_name: "Mic recording".into(),
            record_status: ("Ready to record".into(), MUTED),
            record_hold: -60.0,
            warn_until: None,
            dirty_since: None,
            save_error: None,
            last_device_poll: Instant::now(),
            default_mic_sync: DefaultMicSync::default(),
            focus: None,
            hold: [-60.0; 2],
            waiting_for_device: false,
            reconnecting: false,
            preset_name: String::new(),
            commands: crossbeam_channel::unbounded(),
            tray: None,
            hotkeys: None,
            capturing: None,
            hotkey_status: (String::new(), MUTED),
            dictation: Dictation::new(|| {}),
            dictating: None,
            dictation_status: (String::new(), MUTED),
            last_dictation: String::new(),
            hidden: false,
            quitting: false,
        }
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

    /// Failed saves stay pending and visible until a later retry succeeds.
    fn save_settings(&mut self) -> bool {
        match self.settings.save() {
            Ok(()) => {
                self.dirty_since = None;
                self.save_error = None;
                true
            }
            Err(e) => {
                self.save_error = Some(format!("Settings could not be saved: {e:#}"));
                self.dirty_since = Some(Instant::now());
                false
            }
        }
    }

    /// Processing on, or waiting for a device so it can be.
    fn running(&self) -> bool {
        self.engine.is_some() || self.waiting_for_device
    }

    fn start_engine(&mut self) {
        self.waiting_for_device = false;
        if self.settings.output.is_empty() {
            self.waiting_for_device = true;
            self.status = ("Install VB-Cable or choose a processed output".into(), AMBER);
            return;
        }
        let monitor = &self.settings.monitor_output;
        let monitor_present = !monitor.is_empty() && self.outputs.contains(monitor);
        let missing = [
            (&self.settings.microphone, &self.inputs, "a microphone", true),
            (&self.settings.output, &self.outputs, "a processed output", true),
            // Only wait for the monitor when it is switched on.
            (monitor, &self.outputs, "a monitor output", self.settings.monitor),
        ]
        .into_iter()
        .find(|(name, pool, _, needed)| *needed && !pool.contains(name))
        .map(|(name, _, fallback, _)| {
            if name.is_empty() { fallback } else { name.as_str() }
        });
        if let Some(name) = missing {
            self.waiting_for_device = true;
            self.status = (short(&format!("Waiting for {name} to connect")), AMBER);
            return;
        }
        // Open the monitor whenever it's plugged in, so switching it on is
        // instant; it can't share the processed output's device.
        let monitor = (monitor_present && (self.settings.monitor || *monitor != self.settings.output))
            .then_some(monitor.as_str());
        match Engine::start(&self.settings.microphone, &self.settings.output, monitor, self.params()) {
            Ok(engine) => {
                self.engine = Some(engine);
                self.reconnecting = false;
                self.status = ("Starting…".into(), AMBER);
            }
            // A device that just reappeared can refuse to open for a moment.
            Err(e) if self.reconnecting => {
                self.waiting_for_device = true;
                self.status = (short(&format!("Reconnecting · {e:#}")), AMBER);
            }
            Err(e) => self.status = (short(&format!("{e:#}")), RED),
        }
    }

    fn stop_engine(&mut self, announce: bool) {
        self.waiting_for_device = false;
        self.reconnecting = false;
        if let Some(engine) = self.engine.take() {
            engine.stop();
        }
        self.hold = [-60.0; 2];
        self.playing.clear();
        if announce {
            self.status = ("Stopped".into(), MUTED);
            self.sound_status = ("Ready".into(), Color32::WHITE);
        }
    }

    /// Live route change: stop old engine, start new, hand the clip playheads over.
    fn restart_engine(&mut self) {
        let clips = self.engine.as_ref().map(Engine::take_clips).unwrap_or_default();
        self.stop_engine(false);
        self.status = ("Switching route…".into(), AMBER);
        self.start_engine();
        if let Some(engine) = &self.engine {
            engine.resume_clips(clips);
        }
    }

    fn toggle_running(&mut self) {
        self.save_settings();
        if self.running() {
            self.stop_engine(true);
        } else {
            // A stopped app may have an old device list.
            self.refresh_devices(false);
            self.start_engine();
        }
    }

    /// Re-list devices. `replace_missing` (the Refresh button) swaps out
    /// choices whose device is gone; background polls only fill empty ones.
    fn refresh_devices(&mut self, replace_missing: bool) {
        let host = cpal_host();
        self.update_devices(list_devices(&host, false), list_devices(&host, true), replace_missing);
    }

    fn update_devices(&mut self, inputs: Vec<String>, outputs: Vec<String>, replace_missing: bool) {
        let before = (
            self.settings.microphone.clone(),
            self.settings.output.clone(),
            self.settings.monitor_output.clone(),
        );
        let had_cable = cable_input(&self.outputs).is_some();
        self.inputs = inputs;
        self.outputs = outputs;
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
        if before != after {
            self.touch();
        }
        if self.engine.is_some() && before != after {
            self.restart_engine();
        } else if self.waiting_for_device && self.engine.is_none() {
            // Retry even when the saved names did not change. Stop cancels
            // this request, regardless of the automatic-start preference.
            self.start_engine();
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
        // A device went away (unplugged, driver reset): wait for it to
        // come back and pick up where we left off.
        let lost = self.engine.as_ref().and_then(Engine::take_lost);
        if let Some(lost) = lost {
            self.stop_engine(false);
            self.waiting_for_device = true;
            self.reconnecting = true;
            self.status = (short(&format!("Lost {lost} · reconnecting…")), AMBER);
            // Retry within a second rather than a full poll interval.
            self.last_device_poll = Instant::now()
                .checked_sub(DEVICE_POLL - Duration::from_secs(1))
                .unwrap_or_else(Instant::now);
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
        self.playing = self.engine.as_ref().map(Engine::playing).unwrap_or_default();
        if self.playing.is_empty() && self.sound_status.0.starts_with("Playing") {
            self.sound_status = ("Ready".into(), Color32::WHITE);
        }
    }

    /// Point the Windows default microphone at VB-Cable while processing into
    /// it, and put the user's own default back otherwise. Runs only when the
    /// relevant state changes, with throttled retries after a failure.
    fn sync_default_mic(&mut self) {
        // A brief reconnect keeps VB-Cable as the default rather than
        // flipping Discord's input back and forth.
        let running = self.engine.is_some() || self.reconnecting;
        let key = (running, self.settings.output.clone(), self.settings.default_mic);
        if !self.default_mic_sync.begin(&key, Instant::now()) {
            return;
        }

        let want = running
            && self.settings.default_mic
            && cable_input(&self.outputs) == Some(&self.settings.output);
        if !want && self.settings.saved_default_mic.is_none() {
            self.default_mic_sync.complete(key);
            return;
        }
        let Ok(Some(cable_mic)) = default_mic::cable_capture_id() else {
            return;
        };
        match (self.settings.saved_default_mic.clone(), want) {
            (saved, true) => {
                if saved.is_none() {
                    let Ok(before) = default_mic::current() else { return };
                    if before.console == cable_mic && before.communications == cable_mic {
                        self.default_mic_sync.complete(key);
                        return; // already the default; nothing to restore later
                    }
                    // Persist first so a crash can't lose the user's default.
                    self.settings.saved_default_mic = Some(before);
                }
                if !self.save_settings() {
                    return;
                }
                if let Err(e) = default_mic::set(&cable_mic) {
                    // One role may have changed before the other failed.
                    // Keep the snapshot both for Stop and the next retry.
                    self.warn_until = Some((short(&format!("{e:#}")), Instant::now()));
                    return;
                }
            }
            (Some(saved), false) => {
                if let Err(e) = default_mic::restore(&saved, &cable_mic) {
                    self.warn_until = Some((short(&format!("{e:#}")), Instant::now()));
                    return;
                }
                self.settings.saved_default_mic = None;
                if !self.save_settings() {
                    return;
                }
            }
            _ => {}
        }
        self.default_mic_sync.complete(key);
    }

    // ---- tray, hotkeys and window ------------------------------------------

    fn run_command(&mut self, ctx: &egui::Context, command: Command) {
        match command {
            Command::Show => {
                self.hidden = false;
                ctx.send_viewport_cmd(ViewportCommand::Visible(true));
                ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
                ctx.send_viewport_cmd(ViewportCommand::Focus);
            }
            Command::ToggleMute => {
                self.settings.mute = !self.settings.mute;
                self.apply_live();
            }
            Command::ToggleBypass => {
                self.settings.bypass = !self.settings.bypass;
                self.apply_live();
            }
            Command::ToggleRunning => self.toggle_running(),
            Command::StopClips => self.stop_clips(),
            Command::PlayPad(path) => {
                if let Some(index) = self.settings.sounds.iter().position(|p| p.path == path) {
                    self.play_clip(index);
                }
            }
            Command::Dictate => {
                if self.dictating.is_none() {
                    self.start_dictation();
                } else if !self.settings.dictation_hold {
                    self.stop_dictation();
                }
            }
            Command::DictateReleased => {
                if self.settings.dictation_hold {
                    self.stop_dictation();
                }
            }
            Command::Quit => {
                self.quitting = true;
                ctx.send_viewport_cmd(ViewportCommand::Close);
            }
        }
    }

    /// Closing the window hides it to the tray, unless quitting.
    fn handle_close(&mut self, ctx: &egui::Context) {
        let close = ctx.input(|i| i.viewport().close_requested());
        if close && !self.quitting && self.settings.close_to_tray && self.tray.is_some() {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(ViewportCommand::Visible(false));
            self.hidden = true;
        }
    }

    /// The shortcuts that should be registered right now.
    fn hotkey_bindings(&self) -> Vec<(String, Command)> {
        // While recording a new shortcut, free every key so the press
        // reaches the window instead of triggering an old binding.
        if self.capturing.is_some() {
            return Vec::new();
        }
        let h = &self.settings.hotkeys;
        [
            (&h.mute, Command::ToggleMute),
            (&h.bypass, Command::ToggleBypass),
            (&h.stop_clips, Command::StopClips),
            (&h.dictate, Command::Dictate),
        ]
        .into_iter()
        .filter_map(|(key, command)| Some((key.clone()?, command)))
        .chain(
            self.settings
                .sounds
                .iter()
                .filter_map(|pad| Some((pad.hotkey.clone()?, Command::PlayPad(pad.path.clone())))),
        )
        .collect()
    }

    fn sync_hotkeys(&mut self) {
        let bindings = self.hotkey_bindings();
        let Some(hotkeys) = &mut self.hotkeys else { return };
        let refused = hotkeys.set(bindings);
        if !refused.is_empty() {
            self.hotkey_status = (
                short(&format!("{} is taken by another app", refused.join(", "))),
                AMBER,
            );
        }
    }

    fn binding_slot(&mut self, binding: &Binding) -> Option<&mut Option<String>> {
        let h = &mut self.settings.hotkeys;
        match binding {
            Binding::Mute => Some(&mut h.mute),
            Binding::Bypass => Some(&mut h.bypass),
            Binding::StopClips => Some(&mut h.stop_clips),
            Binding::Dictate => Some(&mut h.dictate),
            Binding::Pad(path) => self
                .settings
                .sounds
                .iter_mut()
                .find(|p| p.path == *path)
                .map(|p| &mut p.hotkey),
        }
    }

    /// Bind `combo` to `binding`, taking it from any other binding.
    fn assign_hotkey(&mut self, binding: &Binding, combo: Option<String>) {
        if let Some(combo) = &combo {
            let h = &mut self.settings.hotkeys;
            let slots = [&mut h.mute, &mut h.bypass, &mut h.stop_clips, &mut h.dictate]
                .into_iter()
                .chain(self.settings.sounds.iter_mut().map(|p| &mut p.hotkey));
            for slot in slots {
                if slot.as_deref().is_some_and(|s| hotkeys::same(s, combo)) {
                    *slot = None;
                }
            }
        }
        if let Some(slot) = self.binding_slot(binding) {
            *slot = combo.clone();
        }
        self.hotkey_status = match combo {
            Some(combo) => (format!("{combo} set"), GREEN),
            None => ("Shortcut cleared".into(), MUTED),
        };
        self.touch();
    }

    /// Record the next key press as a shortcut for the binding being set.
    fn capture_hotkey(&mut self, ctx: &egui::Context) {
        let Some(binding) = self.capturing.clone() else { return };
        let pressed = ctx.input(|i| {
            i.events.iter().find_map(|event| match event {
                egui::Event::Key { key, physical_key, pressed: true, repeat: false, modifiers } => {
                    let key = physical_key.unwrap_or(*key);
                    (!is_modifier(key)).then_some((key, *modifiers))
                }
                _ => None,
            })
        });
        let Some((key, modifiers)) = pressed else { return };
        if key == egui::Key::Escape {
            self.capturing = None;
            self.hotkey_status = ("Shortcut unchanged".into(), MUTED);
            return;
        }
        match hotkeys::combo(key, modifiers) {
            Ok(combo) => {
                self.capturing = None;
                self.assign_hotkey(&binding, Some(combo));
            }
            // Keep listening for a usable combination.
            Err(why) => self.hotkey_status = (why.into(), AMBER),
        }
    }

    fn start_capture(&mut self, binding: Binding) {
        self.capturing = Some(binding);
        self.hotkey_status = ("Press the shortcut, or Esc to cancel".into(), CYAN);
    }

    // ---- speech to text ----------------------------------------------------

    /// Start recording speech to type (the shortcut went down).
    fn start_dictation(&mut self) {
        if self.dictating.is_some() {
            return;
        }
        if !self.settings.speech_model.is_downloaded() {
            self.dictation_status = ("Download a speech model under Settings first".into(), AMBER);
            return;
        }
        // An empty choice falls back to the Windows default microphone.
        match Recorder::start(Source::Microphone, &self.settings.microphone) {
            Ok(recorder) => {
                self.dictating = Some(recorder);
                self.dictation_status = ("Listening…".into(), AMBER);
            }
            Err(e) => self.dictation_status = (short(&format!("{e:#}")), RED),
        }
    }

    /// Stop recording and type what was said.
    fn stop_dictation(&mut self) {
        let Some(recorder) = self.dictating.take() else { return };
        self.dictation_status = match recorder.finish() {
            Ok(Some(take)) => {
                if let Some(warning) = take.warning() {
                    self.dictation_status = (short(&format!("Dictation stopped: {warning}")), RED);
                    return;
                }
                self.dictation.transcribe(self.settings.speech_model, take);
                ("Transcribing…".into(), CYAN)
            }
            Ok(None) => ("Nothing was heard".into(), MUTED),
            Err(e) => (short(&format!("{e:#}")), RED),
        };
    }

    fn poll_dictation(&mut self) {
        if let Some(e) = self.dictating.as_ref().and_then(Recorder::take_error) {
            self.dictating = None;
            self.dictation_status = (short(&e), RED);
        }
        // A toggle left on by mistake shouldn't record all day.
        if self.dictating.as_ref().is_some_and(|r| r.elapsed() >= MAX_DICTATION) {
            self.stop_dictation();
        }
        while let Some(event) = self.dictation.poll() {
            self.dictation_status = match event {
                dictation::Event::Downloaded(model) => (format!("{} is ready", model.label()), GREEN),
                dictation::Event::DownloadFailed(e) => (short(&e), RED),
                dictation::Event::Transcribed { text, typing_error } => {
                    self.last_dictation = text;
                    match typing_error {
                        Some(error) => (short(&format!("Copy the transcript below: {error}")), AMBER),
                        None => ("Typed".into(), GREEN),
                    }
                }
                dictation::Event::Nothing => ("No speech heard".into(), MUTED),
                dictation::Event::Failed(e) => (short(&e), RED),
            };
        }
    }

    // ---- soundboard ------------------------------------------------------

    /// Decoded samples for a clip, from the cache when possible.
    fn clip_samples(&mut self, path: &Path) -> anyhow::Result<Arc<Vec<f32>>> {
        if let Some(samples) = self.clip_cache.get(path) {
            return Ok(Arc::clone(samples));
        }
        let samples = Arc::new(crate::decode::load_clip(path)?);
        // Keep the cache to about ten minutes of audio (~110 MB).
        let cached: usize = self.clip_cache.values().map(|s| s.len()).sum();
        if cached + samples.len() > dsp::SR as usize * 600 {
            self.clip_cache.clear();
        }
        self.clip_cache.insert(path.to_owned(), Arc::clone(&samples));
        Ok(samples)
    }

    fn play_clip(&mut self, index: usize) {
        let Some(pad) = self.settings.sounds.get(index).cloned() else {
            return;
        };
        if self.engine.is_none() {
            self.sound_status = ("Start OpenMic before playing a clip".into(), AMBER);
            return;
        }
        match self.clip_samples(&pad.path) {
            Ok(samples) => {
                let key = clip_key(&pad.path);
                if let Some(engine) = &self.engine {
                    engine.play_sound(Clip::new(key, samples, pad.volume), self.settings.overlap_clips);
                    self.playing = engine.playing();
                }
                self.sound_status = (format!("Playing · {}", file_name(&pad.path)), GREEN);
            }
            Err(e) => self.sound_status = (short(&format!("{:#}", e)), RED),
        }
    }

    fn stop_clips(&mut self) {
        if let Some(engine) = &self.engine {
            engine.stop_sounds();
        }
        self.playing.clear();
        self.sound_status = ("Ready".into(), Color32::WHITE);
    }

    /// Everything that must keep running while the window is hidden.
    fn tick(&mut self, ctx: &egui::Context) {
        while let Ok(command) = self.commands.1.try_recv() {
            self.run_command(ctx, command);
        }
        self.handle_close(ctx);
        self.poll_engine();
        self.poll_recording();
        self.poll_dictation();
        self.sync_default_mic();
        if (cable_input(&self.outputs).is_none() || self.waiting_for_device)
            && self.last_device_poll.elapsed() >= DEVICE_POLL
        {
            self.last_device_poll = Instant::now();
            self.refresh_devices(false);
        }
        self.sync_hotkeys();
        let (muted, bypassed, running) = (self.settings.mute, self.settings.bypass, self.running());
        let listening = self.dictating.is_some();
        if let Some(tray) = &mut self.tray {
            tray.sync(muted, bypassed, running, listening);
        }

        if let Some(t) = self.dirty_since
            && t.elapsed() >= if self.save_error.is_some() { DEVICE_POLL } else { Duration::from_millis(300) }
        {
            self.save_settings();
        }
    }
}

/// How often device lists are re-read while something is missing.
const DEVICE_POLL: Duration = Duration::from_secs(3);

/// Dictation stops listening on its own after this long.
const MAX_DICTATION: Duration = Duration::from_secs(300);

impl eframe::App for App {
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.recorder.take();
        self.dictating.take();
        self.stop_engine(false);
        // Quit gets one final restoration attempt even if Stop just failed.
        self.default_mic_sync = DefaultMicSync::default();
        self.sync_default_mic();
        self.save_settings();
    }

    /// Runs before every frame, and on its own while the window is hidden.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.tick(ctx);
        // The live scope and the recorder animate at ~30 fps. Hidden, a
        // slow tick still handles device loss and the default mic; tray and
        // hotkey commands wake it immediately.
        let animating = self.engine.is_some() || self.recorder.is_some();
        let frame_time = match (self.hidden, animating) {
            (true, _) => 250,
            (false, true) => 33,
            (false, false) => 100,
        };
        ctx.request_repaint_after(Duration::from_millis(frame_time));
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.capture_hotkey(&ctx);

        CentralPanel::default().show(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.heading("OpenMic");
                ui.weak(format!("v{}", env!("CARGO_PKG_VERSION")));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.weak("Clean voice. Instant sounds. Fully local.");
                });
            });
            ui.add_space(6.0);

            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.tab, Tab::Mic, "Microphone");
                ui.selectable_value(
                    &mut self.tab,
                    Tab::Soundboard,
                    if self.recorder.is_some() {
                        "Soundboard · REC"
                    } else {
                        "Soundboard"
                    },
                );
                ui.selectable_value(&mut self.tab, Tab::Settings, "Settings");
            });
            ui.separator();
            match self.tab {
                Tab::Mic => self.draw_mic_tab(ui),
                Tab::Soundboard => {
                    let height = (ui.available_height() - 60.0).max(320.0);
                    ScrollArea::vertical()
                        .id_salt("sound_tab")
                        .max_height(height)
                        .show(ui, |ui| self.draw_sound_tab(ui));
                }
                Tab::Settings => self.draw_settings_tab(ui),
            }

            // Footer follows the content; the fixed window is sized to fit.
            ui.add_space(10.0);
            ui.separator();
            ui.horizontal(|ui| {
                // Waiting for a device counts as on: Stop cancels the wait.
                let running = self.running();
                let btn = egui::Button::new(if running {
                    RichText::new("Stop OpenMic").color(Color32::BLACK)
                } else {
                    RichText::new("Start OpenMic").color(Color32::BLACK)
                })
                .fill(if running { RED } else { GREEN });
                if ui.add(btn).clicked() {
                    self.toggle_running();
                }
                if let Some(error) = &self.save_error {
                    ui.label(RichText::new("Settings not saved · retrying…").color(RED))
                        .on_hover_text(error);
                } else {
                    ui.label(RichText::new(&self.status.0).color(self.status.1));
                }
            });
        });
    }
}

// ---- small helpers ---------------------------------------------------------

fn cpal_host() -> cpal::Host {
    cpal::default_host()
}

fn short(msg: &str) -> String {
    msg.chars().take(64).collect()
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

fn clock_time(duration: Duration) -> String {
    let seconds = duration.as_secs();
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

/// Identifies a pad's clip in the mixer.
fn clip_key(path: &Path) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut hasher);
    hasher.finish()
}

fn is_modifier(key: egui::Key) -> bool {
    use egui::Key::*;
    matches!(
        key,
        ShiftLeft | ShiftRight | ControlLeft | ControlRight | AltLeft | AltRight | SuperLeft | SuperRight
    )
}

fn open_url(url: &str) {
    // Explorer hands URLs to the default browser.
    let _ = std::process::Command::new("explorer").arg(url).spawn();
}

/// A 0..100% volume slider that also takes the mouse wheel (1% per notch).
fn volume_slider(ui: &mut egui::Ui, value: &mut f32) -> bool {
    let response = ui.add(
        Slider::new(value, 0.0..=1.0).custom_formatter(|v, _| format!("{:.0}%", v * 100.0)),
    );
    let notches = viz::wheel_notches(ui, response.id, response.hovered() && !response.dragged());
    response.changed() | viz::step_by(value, notches, 0.01, 0.0..=1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Pad;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn failed_default_mic_sync_retries_without_blocking_state_changes() {
        let mut sync = DefaultMicSync::default();
        let now = Instant::now();
        let stopped = (false, "CABLE Input".into(), true);
        let running = (true, "CABLE Input".into(), true);
        assert!(sync.begin(&stopped, now));
        sync.complete(stopped.clone());
        assert!(!sync.begin(&stopped, now + DEVICE_POLL));

        assert!(sync.begin(&running, now));
        assert!(!sync.begin(&running, now + Duration::from_millis(250)));
        assert!(sync.begin(&running, now + DEVICE_POLL));
        // A partial start failure may have changed a role. Stop must restore
        // it immediately even though the stopped state once succeeded.
        assert!(sync.begin(&stopped, now + DEVICE_POLL));
    }

    #[test]
    fn exiting_can_force_a_final_default_mic_restore() {
        let stopped = (false, "CABLE Input".into(), true);
        let now = Instant::now();
        let mut sync = DefaultMicSync::default();
        assert!(sync.begin(&stopped, now));
        assert!(!sync.begin(&stopped, now));
        sync = DefaultMicSync::default();
        assert!(sync.begin(&stopped, now));
    }

    #[test]
    fn stop_cancels_waiting_even_when_vb_cable_appears() {
        let mut app = App::stopped(Settings::default());
        app.start_engine();
        assert!(app.waiting_for_device, "a start request survives a missing output");

        app.stop_engine(true);
        app.update_devices(Vec::new(), names(&["CABLE Input", "Headphones"]), false);
        assert!(app.settings.auto_start);
        assert!(!app.waiting_for_device, "Stop cancels the pending start request");
        assert!(app.engine.is_none());
        assert_eq!(app.status.0, "Stopped");
    }

    #[test]
    fn device_poll_retries_unchanged_saved_routes() {
        let mut app = App::stopped(Settings {
            microphone: "USB mic".into(),
            output: "CABLE Input".into(),
            monitor_output: "Headphones".into(),
            monitor: true,
            ..Default::default()
        });
        app.outputs = names(&["CABLE Input"]);
        app.start_engine();
        assert_eq!(app.status.0, "Waiting for USB mic to connect");

        app.update_devices(names(&["USB mic"]), names(&["CABLE Input"]), false);
        assert!(app.waiting_for_device);
        assert_eq!(app.settings.microphone, "USB mic");
        assert_eq!(app.settings.monitor_output, "Headphones");
        assert_eq!(app.status.0, "Waiting for Headphones to connect");
    }

    #[test]
    fn a_missing_monitor_only_blocks_start_while_monitoring() {
        let mut app = App::stopped(Settings {
            microphone: "USB mic".into(),
            output: "CABLE Input".into(),
            monitor_output: "Headphones".into(),
            ..Default::default()
        });
        app.inputs = names(&["USB mic"]);
        app.outputs = names(&["CABLE Input"]);
        app.start_engine();
        assert!(
            !app.status.0.starts_with("Waiting"),
            "monitor is off, so unplugged headphones don't matter: {}",
            app.status.0
        );
    }

    #[test]
    fn stop_ends_a_reconnect() {
        let mut app = App::stopped(Settings::default());
        app.reconnecting = true;
        app.waiting_for_device = true;
        app.stop_engine(true);
        assert!(!app.reconnecting && !app.running());
    }

    #[test]
    fn a_shortcut_moves_to_its_newest_binding() {
        let mut app = App::stopped(Settings {
            sounds: vec![Pad::new("a.wav".into()), Pad::new("b.wav".into())],
            ..Default::default()
        });
        app.assign_hotkey(&Binding::Pad("a.wav".into()), Some("Ctrl+Alt+1".into()));
        app.assign_hotkey(&Binding::Mute, Some("Ctrl+Alt+M".into()));
        app.assign_hotkey(&Binding::Pad("b.wav".into()), Some("control+alt+Digit1".into()));
        assert_eq!(app.settings.sounds[0].hotkey, None, "taken over by pad b");
        assert_eq!(app.settings.sounds[1].hotkey.as_deref(), Some("control+alt+Digit1"));

        let bindings = app.hotkey_bindings();
        assert_eq!(bindings.len(), 2);
        assert!(bindings.contains(&("Ctrl+Alt+M".into(), Command::ToggleMute)));
        assert!(bindings.contains(&("control+alt+Digit1".into(), Command::PlayPad("b.wav".into()))));

        app.start_capture(Binding::StopClips);
        assert!(app.hotkey_bindings().is_empty(), "keys are freed while recording one");
    }

    #[test]
    fn tray_and_hotkey_commands_drive_the_app() {
        let ctx = egui::Context::default();
        let mut app = App::stopped(Settings::default());
        app.commands.0.send(Command::ToggleMute).unwrap();
        app.commands.0.send(Command::ToggleBypass).unwrap();
        app.tick(&ctx);
        assert!(app.settings.mute && app.settings.bypass);
        assert!(app.dirty_since.is_some(), "toggles are saved");

        app.waiting_for_device = true;
        app.commands.0.send(Command::ToggleRunning).unwrap();
        app.tick(&ctx);
        assert!(!app.running(), "Stop from the tray cancels a pending start");
    }
}
