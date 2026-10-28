//! egui front end: app state and lifecycle. Each tab draws from its own
//! module; routing rules live in `routes`.

mod clips;
mod durations;
#[cfg(test)]
mod external_actions;
mod mic_tab;
mod mic_test;
mod overlay;
mod routes;
mod settings_tab;
mod startup;
#[cfg(test)]
mod status_controls;
#[cfg(test)]
mod snapshot;
mod sound_tab;
mod updates;

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use crossbeam_channel::{Receiver, Sender};
use eframe::egui::{self, CentralPanel, Color32, RichText, ScrollArea, Slider, ViewportCommand};

use crate::config::{self, Settings};
use crate::default_mic;
use crate::dictation::{self, Dictation};
use crate::dsp::{self, Params};
use crate::engine::{list_devices, Engine};
use crate::hotkeys::{self, Hotkeys};
use crate::record::{Recorder, Source, Take};
use crate::tray::Tray;
use crate::logfile;
use crate::update::Updater;
use crate::viz;
use crate::widgets;
use routes::{cable_input, choose_routes};

pub(crate) const CYAN: Color32 = Color32::from_rgb(0x38, 0xbd, 0xf8);
pub(crate) const GREEN: Color32 = Color32::from_rgb(0x22, 0xc5, 0x5e);
pub(crate) const AMBER: Color32 = Color32::from_rgb(0xfb, 0xbf, 0x24);
pub(crate) const VIOLET: Color32 = Color32::from_rgb(0xa7, 0x8b, 0xfa);
pub(crate) const RED: Color32 = Color32::from_rgb(0xf8, 0x71, 0x71);
pub(crate) const MUTED: Color32 = Color32::from_rgb(0x94, 0xa3, 0xb8);

/// A page of the main window, chosen in the sidebar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Page {
    Voice,
    Soundboard,
    Record,
    Dictation,
    Settings,
}

impl Page {
    const ALL: [Page; 5] = [Page::Voice, Page::Soundboard, Page::Record, Page::Dictation, Page::Settings];

    fn title(self) -> &'static str {
        match self {
            Page::Voice => "Voice",
            Page::Soundboard => "Soundboard",
            Page::Record => "Record",
            Page::Dictation => "Dictation",
            Page::Settings => "Settings",
        }
    }
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
#[cfg_attr(not(any(windows, test)), allow(dead_code))] // tray commands have no Linux source
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    Show,
    ToggleMute,
    ToggleBypass,
    ToggleRunning,
    /// Collapse the window to the strip, or expand it back.
    ToggleCollapse,
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
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Binding {
    Mute,
    Bypass,
    StopClips,
    Dictate,
    Pad(PathBuf),
}

struct CachedClip {
    samples: Arc<Vec<f32>>,
    peaks: Arc<Vec<f32>>,
    bytes: u64,
    modified: Option<SystemTime>,
}

struct DictationRecording {
    recorder: Recorder,
    /// Model selected and checked before this take started.
    model: dictation::SpeechModel,
    /// Stop the way this take started, even if the preference changes.
    hold_to_talk: bool,
}

pub struct App {
    settings: Settings,
    inputs: Vec<String>,
    outputs: Vec<String>,
    page: Page,
    engine: Option<Engine>,
    status: Status,
    sound_status: Status,
    /// Keys of the clips playing as of this frame.
    playing: Vec<u64>,
    /// Playback state and loaded durations for playing and paused clips.
    pad_progress: HashMap<u64, dsp::ClipProgress>,
    /// Clip lengths in seconds, read once per file in the background.
    pad_durations: durations::Durations,
    /// The soundboard's search text.
    pad_filter: String,
    /// A strip pad to reveal and focus when the board next draws.
    pad_to_reveal: Option<PathBuf>,
    /// A clip name being edited, separate from its saved display name.
    renaming_pad: Option<sound_tab::PadRename>,
    /// The trim menu's current clip.
    trim_clip: Option<PathBuf>,
    /// Load failures stay on their pads until retried or removed.
    clip_errors: HashMap<PathBuf, String>,
    /// Decoded clips, so a pad (or its hotkey) plays without a decode delay.
    clip_cache: HashMap<PathBuf, CachedClip>,
    /// Files being imported in the background, one decoder at a time.
    clip_import: Option<sound_tab::ClipImport>,
    /// Pending playback and trim previews, decoded one clip at a time.
    clip_loads: Option<clips::ClipLoads>,
    record_source: Source,
    record_output: String,
    recorder: Option<Recorder>,
    take: Option<Arc<Take>>,
    /// A take being saved without blocking microphone controls.
    recording_save: Option<sound_tab::RecordingSave>,
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
    /// Noise strength to bring back when Remove noise is switched on again.
    strength_before_off: f32,
    /// Stand-in audio history for pictures drawn without an engine (tests).
    #[cfg(test)]
    demo_frames: Vec<crate::engine::ScopeFrame>,
    #[cfg(test)]
    devices_for_test: Option<(Vec<String>, Vec<String>)>,
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
    /// Speech being recorded or finalized for dictation.
    dictating: Option<DictationRecording>,
    /// Most recent result; live listening/transcription indicators use their own state.
    dictation_status: Status,
    /// The words most recently recognized, including when typing was blocked.
    last_dictation: String,
    /// The window is hidden to the notification area.
    hidden: bool,
    /// Quit was chosen: let the window close instead of hiding it.
    quitting: bool,
    /// A quit request would discard an active recording or unsaved take.
    confirm_quit: bool,
    /// New releases: checked daily, installed when the user asks.
    updater: Updater,
    /// When this launch started, if it is a freshly installed update.
    updated_at: Option<Instant>,
    /// A manual check is waiting for its result.
    manual_update_check: bool,
    /// When the last manual check's result became available.
    update_checked_at: Option<Instant>,
    /// The "What's new" window is open.
    show_release_notes: bool,
    /// Launched by Windows startup: stay in the tray, even if collapsed,
    /// until the user opens OpenMic.
    strip_suppressed: bool,
    overlay_placement: overlay::Placement,
    /// The meter's width when it was pinned (kept constant while shown).
    overlay_spawn_width: f32,
    overlay_on_top: overlay::OnTop,
    /// Puts the main window back where it was last time.
    window_placement: overlay::Placement,
    /// The window was minimized last frame (to catch the moment it is).
    was_minimized: bool,
    mic_test: mic_test::MicTest,
    update_error: Option<String>,
}

impl App {
    /// The real app: tray icon, global hotkeys, and devices. `start_hidden`
    /// launches straight into the notification area (Windows startup).
    pub fn new(
        settings: Settings,
        ctx: &egui::Context,
        start_hidden: bool,
        instance: crate::instance::Instance,
        just_updated: bool,
    ) -> Self {
        widgets::configure_text_styles(ctx);
        let mut app = Self::stopped(settings);
        app.updated_at = just_updated.then(Instant::now);
        {
            // Development builds never replace themselves on their own, and
            // releases only ship an updater for Windows.
            let ctx = ctx.clone();
            app.updater = Updater::new(!cfg!(debug_assertions) && cfg!(windows), move || ctx.request_repaint());
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
        app.strip_suppressed = std::env::args().any(|arg| arg == config::MINIMIZED_ARG);
        app.set_startup_visibility(ctx, start_hidden);

        app.initialize_settings();
        app
    }

    /// Apply startup side effects only after preferences have loaded successfully.
    fn initialize_settings(&mut self) {
        if self.settings.load_error.is_some() { return; }
        // Reflect the actual registry state, like the Python app did.
        self.settings.start_with_windows = config::startup_enabled();
        if self.settings.start_with_windows {
            // Refresh the entry: the exe may have moved, and older versions
            // registered it without the start-hidden flag.
            let _ = config::set_startup(true);
        }
        let host = cpal_host();
        self.inputs = list_devices(&host, false);
        self.outputs = list_devices(&host, true);
        // Keep saved devices even if absent: a USB mic can appear after a
        // Windows-startup launch, and must not be silently swapped out. The
        // processed output goes back to VB-Cable on every launch.
        choose_routes(&mut self.settings, &self.inputs, &self.outputs, true, false);

        if self.settings.auto_start {
            self.start_engine();
        }
    }

    /// App state without a window, tray, hotkeys or devices (also for tests).
    fn stopped(settings: Settings) -> Self {
        Self {
            settings,
            inputs: Vec::new(),
            outputs: Vec::new(),
            page: Page::Voice,
            engine: None,
            status: ("Stopped".into(), MUTED),
            sound_status: ("Ready".into(), Color32::WHITE),
            playing: Vec::new(),
            pad_progress: HashMap::new(),
            pad_durations: durations::Durations::default(),
            pad_filter: String::new(),
            pad_to_reveal: None,
            renaming_pad: None,
            trim_clip: None,
            clip_errors: HashMap::new(),
            clip_cache: HashMap::new(),
            clip_import: None,
            clip_loads: None,
            record_source: Source::Microphone,
            record_output: String::new(),
            recorder: None,
            take: None,
            recording_save: None,
            record_name: "Mic recording".into(),
            record_status: ("Ready to record".into(), MUTED),
            record_hold: -60.0,
            warn_until: None,
            dirty_since: None,
            save_error: None,
            last_device_poll: Instant::now(),
            default_mic_sync: DefaultMicSync::default(),
            strength_before_off: 1.0,
            #[cfg(test)]
            demo_frames: Vec::new(),
            #[cfg(test)]
            devices_for_test: None,
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
            confirm_quit: false,
            updater: Updater::new(false, || {}),
            updated_at: None,
            manual_update_check: false,
            update_checked_at: None,
            show_release_notes: false,
            strip_suppressed: false,
            overlay_placement: overlay::Placement::default(),
            overlay_spawn_width: overlay::DEFAULT_WIDTH,
            overlay_on_top: overlay::OnTop::default(),
            window_placement: overlay::Placement::default(),
            was_minimized: false,
            mic_test: mic_test::MicTest::default(),
            update_error: None,
        }
    }

    fn params(&self) -> Params {
        let s = &self.settings;
        Params {
            model: s.model,
            strength: s.strength.clamp(0.0, 1.0),
            input_gain: dsp::db_to_gain(s.input_gain_db.clamp(*dsp::INPUT_GAIN_RANGE.start(), *dsp::INPUT_GAIN_RANGE.end())),
            output_gain: dsp::db_to_gain(s.output_gain_db.clamp(*dsp::OUTPUT_GAIN_RANGE.start(), *dsp::OUTPUT_GAIN_RANGE.end())),
            monitor_gain: s.monitor_volume.clamp(0.0, 1.0),
            sound_gain: s.sound_volume.clamp(0.0, 1.0),
            gate_enabled: s.gate,
            gate_threshold_db: s.gate_threshold_db.clamp(*dsp::GATE_THRESHOLD_RANGE.start(), *dsp::GATE_THRESHOLD_RANGE.end()),
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
                if self.save_error.is_none() {
                    logfile::error(format_args!("settings not saved: {e:#}"));
                }
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
            self.status = (
                if cfg!(windows) {
                    "Install VB-Cable or choose a processed output"
                } else {
                    "Create the virtual mic or choose a processed output"
                }.into(),
                AMBER,
            );
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
            logfile::info(format_args!("waiting for {name} to connect"));
            self.waiting_for_device = true;
            self.status = (short(&format!("Waiting for {name} to connect")), AMBER);
            return;
        }
        // Open the monitor whenever it's plugged in, so switching it on is
        // instant; it can't share the processed output's device. Linux opens
        // it only while monitoring: PulseAudio clients can hold fewer streams.
        let monitor_wanted = if cfg!(windows) {
            self.settings.monitor || *monitor != self.settings.output
        } else {
            self.settings.monitor
        };
        let monitor = (monitor_present && monitor_wanted).then_some(monitor.as_str());
        match Engine::start(&self.settings.microphone, &self.settings.output, monitor, self.params()) {
            Ok(engine) => {
                logfile::info(format_args!(
                    "processing started: mic '{}', output '{}', monitor {}",
                    self.settings.microphone,
                    self.settings.output,
                    monitor.map_or("off".into(), |m| format!("'{m}'")),
                ));
                self.engine = Some(engine);
                self.reconnecting = false;
                self.status = ("Starting…".into(), AMBER);
            }
            // A device that just reappeared can refuse to open for a moment.
            Err(e) if self.reconnecting => {
                logfile::warn(format_args!("reconnecting: {e:#}"));
                self.waiting_for_device = true;
                self.status = (short(&format!("Reconnecting · {e:#}")), AMBER);
            }
            Err(e) => {
                logfile::error(format_args!("processing did not start: {e:#}"));
                self.status = (short(&format!("{e:#}")), RED);
            }
        }
    }

    fn stop_engine(&mut self, announce: bool) {
        self.cancel_pending_playback(None);
        self.waiting_for_device = false;
        self.reconnecting = false;
        self.warn_until = None;
        if let Some(engine) = self.engine.take() {
            engine.stop();
            logfile::info("processing stopped");
        }
        self.hold = [-60.0; 2];
        self.playing.clear();
        self.pad_progress.clear();
        if announce {
            self.status = ("Stopped".into(), MUTED);
            self.sound_status = ("Ready".into(), Color32::WHITE);
        }
    }

    /// Live route change: stop old engine, start new, hand the clip playheads over.
    fn restart_engine(&mut self) {
        let clips = self.engine.as_ref().map(Engine::take_clips).unwrap_or_default();
        let pending = self.clip_loads.take();
        self.stop_engine(false);
        self.status = ("Switching route…".into(), AMBER);
        self.start_engine();
        if let Some(engine) = &self.engine {
            engine.resume_clips(clips);
        }
        self.clip_loads = pending;
        if self.engine.is_none() { self.cancel_pending_playback(None); }
        self.sync_clip_progress();
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
        #[cfg(test)]
        if let Some((inputs, outputs)) = &self.devices_for_test {
            self.update_devices(inputs.clone(), outputs.clone(), replace_missing);
            return;
        }
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
            logfile::error(format_args!(
                "{} could not be updated: {e:#}",
                if cfg!(windows) { "Windows startup" } else { "Login startup" }
            ));
            // Removing a legacy entry can fail after the main entry changed.
            self.settings.start_with_windows = config::startup_enabled();
            self.status = (short(&format!("{:#}", e)), RED);
        } else {
            self.status = (
                format!(
                    "{} {}",
                    if cfg!(windows) { "Windows startup" } else { "Login startup" },
                    if enabled { "enabled" } else { "disabled" }
                ),
                GREEN,
            );
        }
    }

    fn poll_engine(&mut self) {
        let err = self.engine.as_mut().and_then(Engine::take_error);
        if let Some(e) = err {
            logfile::error(format_args!("processing failed: {e}"));
            self.stop_engine(false);
            self.status = (short(&e), RED);
            return;
        }
        // A device went away (unplugged, driver reset): wait for it to
        // come back and pick up where we left off.
        let lost = self.engine.as_ref().and_then(Engine::take_lost);
        if let Some(lost) = lost {
            logfile::warn(format_args!("lost {lost}; reconnecting"));
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
            logfile::warn(&w);
            self.warn_until = Some((short(&w), Instant::now()));
        }
        if let Some((msg, at)) = &self.warn_until {
            if at.elapsed() < Duration::from_secs(2) {
                self.status = (msg.clone(), AMBER);
            } else {
                self.warn_until = None;
            }
        }
        self.sync_clip_progress();
    }

    /// Point the Windows default microphone at VB-Cable while processing into
    /// it, and put the user's own default back otherwise. Runs only when the
    /// relevant state changes, with throttled retries after a failure.
    fn sync_default_mic(&mut self) {
        if self.settings.load_error.is_some() { return; }
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
                    logfile::warn(format_args!(
                        "default mic not set to {}: {e:#}",
                        if cfg!(windows) { "VB-Cable" } else { "OpenMic Microphone" }
                    ));
                    self.warn_until = Some((short(&format!("{e:#}")), Instant::now()));
                    return;
                }
            }
            (Some(saved), false) => {
                if let Err(e) = default_mic::restore(&saved, &cable_mic) {
                    logfile::warn(format_args!("default mic not restored: {e:#}"));
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
        if self.quitting {
            return;
        }
        if self.settings.load_error.is_some() && !matches!(command, Command::Show | Command::Quit) {
            self.run_command(ctx, Command::Show);
            return;
        }
        match command {
            Command::Show => {
                // Showing the window expands a collapsed OpenMic.
                if self.settings.collapsed {
                    self.settings.collapsed = false;
                    self.touch();
                }
                self.strip_suppressed = false;
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
            Command::ToggleCollapse => {
                let collapse = !self.settings.collapsed;
                self.set_collapsed(ctx, collapse);
            }
            Command::StopClips => self.stop_clips(),
            Command::PlayPad(path) => {
                if let Some(index) = self.settings.sounds.iter().position(|p| p.path == path) {
                    self.play_clip(index, ctx);
                }
            }
            Command::Dictate => {
                if self.dictating.is_none() {
                    self.start_dictation();
                } else if self.dictating.as_ref().is_some_and(|recording| !recording.hold_to_talk) {
                    self.stop_dictation();
                }
            }
            Command::DictateReleased => {
                if self.dictating.as_ref().is_some_and(|recording| recording.hold_to_talk) {
                    self.stop_dictation();
                }
            }
            Command::Quit => self.request_quit(ctx),
        }
    }

    fn request_quit(&mut self, ctx: &egui::Context) {
        if self.quitting {
            return;
        }
        if self.recorder.is_some() || self.take.is_some() {
            self.confirm_quit = true;
            self.finish_pad_rename(true);
            self.capturing = None;
            self.page = Page::Record;
            self.run_command(ctx, Command::Show);
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
        } else {
            self.quit(ctx);
        }
    }

    fn quit(&mut self, ctx: &egui::Context) {
        self.confirm_quit = false;
        self.quitting = true;
        self.dictation.cancel_transcription();
        self.dictation.cancel_download();
        ctx.send_viewport_cmd(ViewportCommand::Close);
    }

    fn draw_quit_confirmation(&mut self, ctx: &egui::Context) {
        if !self.confirm_quit {
            return;
        }
        if self.recorder.is_none() && self.take.is_none() {
            self.quit(ctx);
            return;
        }
        let active = self.recorder.is_some();
        let finishing = self.recorder.as_ref().is_some_and(Recorder::is_stopping);
        let saving = self.recording_save.is_some();
        let dialog = egui::Modal::new(egui::Id::new("quit-recording")).show(ctx, |ui| {
            ui.set_width(340.0);
            ui.heading(if saving { "Saving before quitting…" } else { "Quit without saving?" });
            ui.label(if saving {
                "Your recording is being saved. OpenMic will close when it finishes."
            } else if finishing {
                "OpenMic is finishing your recording. Quitting will discard this take."
            } else if active {
                "Recording is still running. Quitting will discard this take."
            } else {
                "Your recording has not been saved. Quitting will discard it."
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button(if saving || finishing { "Keep OpenMic open" } else if active { "Keep recording" } else { "Back to recording" }).clicked() {
                    self.confirm_quit = false;
                }
                if saving {
                    ui.spinner();
                } else if ui.button(RichText::new("Discard and quit").color(widgets::text_color(ui, RED))).clicked() {
                    self.quit(ctx);
                }
            });
        });
        if dialog.should_close() {
            self.confirm_quit = false;
        }
    }

    /// Put the window where it was last time (once), then remember where
    /// it's moved to and how big it's made. Positions are screen pixels, so
    /// they mean the same on monitors with different scaling.
    fn remember_window(&mut self, ctx: &egui::Context) {
        let info = ctx.input(|i| i.viewport().clone());
        if self.hidden || info.minimized == Some(true) {
            return;
        }
        let saved = self.settings.window_position.filter(|&px| on_a_monitor(px));
        let (place, record) = self.window_placement.step(saved, info.outer_rect, info.native_pixels_per_point);
        if let Some(pos) = place {
            ctx.send_viewport_cmd(ViewportCommand::OuterPosition(pos));
        }
        let (Some(outer), Some(inner), Some(ppp)) = (info.outer_rect, info.inner_rect, info.native_pixels_per_point)
        else {
            return;
        };
        if record {
            let px = [(outer.min.x * ppp).round() as i32, (outer.min.y * ppp).round() as i32];
            let size = [inner.width().round(), inner.height().round()];
            if self.settings.window_position != Some(px) || self.settings.window_size != Some(size) {
                self.settings.window_position = Some(px);
                self.settings.window_size = Some(size);
                self.touch();
            }
        }
    }

    /// Swap in the downloaded version, start it, and quit this copy.
    fn install_update(&mut self, ctx: &egui::Context) {
        if let Some(why) = self.update_blocker() {
            self.update_error = Some(why.into());
            return;
        }
        match self.updater.install() {
            Ok(()) => {
                logfile::info("restarting into the downloaded update");
                self.run_command(ctx, Command::Quit);
            }
            Err(e) => {
                logfile::error(format_args!("update not installed: {e:#}"));
                self.update_error = Some(short(&format!("{e:#}")));
            }
        }
    }

    /// Work a restart would lose.
    fn update_blocker(&self) -> Option<&'static str> {
        if self.recording_save.is_some() {
            Some("Wait for your recording to finish saving")
        } else if self.recorder.as_ref().is_some_and(Recorder::is_stopping) {
            Some("Wait for your recording to finish")
        } else if self.recorder.is_some() || self.take.is_some() {
            Some("Save or discard your recording first")
        } else if self.dictating.is_some() || self.dictation.busy() {
            Some("Wait for speech to text to finish")
        } else if self.mic_test.busy() {
            Some("Wait for the mic test to finish")
        } else if self.dictation.download_progress().is_some() {
            Some("Wait for the speech model download to finish or cancel it")
        } else if self.clip_import.as_ref().is_some_and(sound_tab::ClipImport::has_pending_files) {
            Some("Wait for clip imports to finish or cancel them")
        } else {
            None
        }
    }

    /// The minimize button collapses to the strip. Only the moment the
    /// window becomes minimized counts: Windows can still report it
    /// minimized for a frame after the strip expands it again.
    fn minimize_to_strip(&mut self, ctx: &egui::Context) {
        let minimized = ctx.input(|i| i.viewport().minimized) == Some(true);
        let just_minimized = minimized && !self.was_minimized;
        self.was_minimized = minimized;
        if just_minimized && !self.hidden && self.settings.minimize_to_strip {
            self.set_collapsed(ctx, true);
        }
    }

    /// Hide or restore the full window; the strip can stay visible alongside it.
    fn set_collapsed(&mut self, ctx: &egui::Context, collapsed: bool) {
        if collapsed {
            self.settings.collapsed = true;
            self.strip_suppressed = false;
            self.hidden = true;
            ctx.send_viewport_cmd(ViewportCommand::Visible(false));
            self.touch();
        } else {
            self.run_command(ctx, Command::Show);
        }
    }

    /// Closing the window hides it to the tray, unless quitting.
    fn handle_close(&mut self, ctx: &egui::Context) {
        let close = ctx.input(|i| i.viewport().close_requested());
        if close && !self.quitting {
            if self.settings.load_error.is_none() && self.settings.close_to_tray && self.tray.is_some() && !self.confirm_quit {
                ctx.send_viewport_cmd(ViewportCommand::CancelClose);
                ctx.send_viewport_cmd(ViewportCommand::Visible(false));
                self.hidden = true;
            } else {
                self.request_quit(ctx);
            }
        }
    }

    /// The shortcuts that should be registered right now.
    fn hotkey_bindings(&self) -> Vec<(String, Command)> {
        // While recording a new shortcut, free every key so the press
        // reaches the window instead of triggering an old binding.
        if self.capturing.is_some() || self.settings.load_error.is_some() {
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

    fn sync_hotkeys(&mut self, ctx: &egui::Context) {
        // Capture temporarily frees every global shortcut. Restore them
        // when the full window can no longer receive the replacement key.
        if self.hidden || ctx.input(|i| !i.focused || i.viewport().minimized == Some(true)) {
            self.cancel_capture();
        }
        let bindings = self.hotkey_bindings();
        let Some(hotkeys) = &mut self.hotkeys else { return };
        let refused = hotkeys.set(bindings);
        if !refused.is_empty() {
            logfile::warn(format_args!(
                "shortcuts refused by {}: {}",
                if cfg!(windows) { "Windows" } else { "the system" },
                refused.join(", ")
            ));
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
        if self.capturing.as_ref() == Some(binding) {
            self.capturing = None;
        }
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
        let held_id = egui::Id::new(("captured-shortcut-keys", ctx.viewport_id()));
        let held = ctx.data(|data| data.get_temp::<Vec<(egui::Key, Option<egui::Key>)>>(held_id)).unwrap_or_default();
        let pressed = ctx.input(|i| {
            i.events.iter().enumerate().find_map(|(index, event)| match event {
                egui::Event::Key { key, physical_key, pressed: true, repeat, modifiers } => {
                    let physical = physical_key.unwrap_or(*key);
                    // egui counts repeats by logical key, even for different physical keys.
                    let fresh = !repeat || physical_key.is_some_and(|scan| held.iter().any(|(saved, saved_scan)|
                        saved == key && saved_scan.is_some_and(|saved_scan| saved_scan != scan)));
                    (!is_modifier(physical) && fresh).then_some((index, *key, *physical_key, physical, *modifiers))
                }
                _ => None,
            })
        });
        let cancel_button = !matches!(binding, Binding::Pad(_)) && pressed.is_some_and(|(_, logical, _, key, modifiers)|
            matches!(logical, egui::Key::Enter | egui::Key::Space) && logical == key && !modifiers.ctrl && !modifiers.alt)
            && ctx.data(|data| data.get_temp::<egui::Id>(egui::Id::new(("shortcut-row", &binding)).with("edit")))
                .is_some_and(|id| ctx.memory(|memory| memory.has_focus(id)));
        let combination = pressed.map(|(_, _, _, key, modifiers)| hotkeys::combo(key, modifiers));
        if let Some((index, logical, physical, _, _)) = pressed {
            let tail = ctx.input_mut(|input| {
                let next = input.events.iter().enumerate().skip(index + 1).find_map(|(next, event)| match event {
                    egui::Event::Key { key, physical_key, pressed: true, repeat, .. }
                        if !is_modifier(physical_key.unwrap_or(*key)) && (!repeat
                            || !shortcut_key_matches((logical, physical), (*key, *physical_key))) => Some(next),
                    _ => None,
                });
                next.map(|next| {
                    // Releases also clear activation guards on the replacement control.
                    let mut tail: Vec<_> = input.events[..next].iter().filter(|event|
                        matches!(event, egui::Event::Key { pressed: false, .. })).cloned().collect();
                    tail.extend(input.events.split_off(next));
                    tail
                })
            });
            if let Some(tail) = tail {
                ctx.plugin_or_default::<DeferredFocusInput>().lock().events.push_front(tail);
                ctx.request_repaint();
            }
        }
        let down = ctx.input(|input| {
            let mut down = Vec::new();
            for event in &input.events {
                if let egui::Event::Key { key, physical_key, pressed, .. } = event
                    && !is_modifier(physical_key.unwrap_or(*key))
                {
                    let key = (*key, *physical_key);
                    if *pressed { if !down.contains(&key) { down.push(key); } }
                    else { down.retain(|saved| !shortcut_key_matches(*saved, key)); }
                }
            }
            down
        });
        if !down.is_empty() {
            ctx.data_mut(|data| {
                let mut held = data.get_temp::<Vec<(egui::Key, Option<egui::Key>)>>(held_id).unwrap_or_default();
                for key in down { if !held.contains(&key) { held.push(key); } }
                data.insert_temp(held_id, held);
            });
        }
        // Capture owns navigation and text as well as the combination's first key.
        ctx.memory_mut(|memory| memory.move_focus(egui::FocusDirection::None));
        ctx.input_mut(|input| input.events.retain(|event| !shortcut_keyboard_event(event) || matches!(event,
            egui::Event::Key { key: egui::Key::Escape, .. }) || (cancel_button && matches!(event,
                egui::Event::Key { key: egui::Key::Enter | egui::Key::Space, .. }))));
        let Some((_, _, _, key, _)) = pressed else { return };
        if key == egui::Key::Escape {
            // egui clears button focus before delivering Escape. Keep the pad reachable.
            if matches!(binding, Binding::Pad(_)) && self.page == Page::Soundboard
                && let Some(owner) = ctx.data(|data| data.get_temp::<Option<egui::Id>>(egui::Id::new("keyboard-focus"))).flatten()
            {
                sound_tab::focus_soundboard_control(ctx, owner);
            }
            self.cancel_capture();
            return;
        }
        match combination.expect("a key was pressed") {
            Ok(combo) => {
                self.assign_hotkey(&binding, Some(combo));
            }
            // Keep listening for a usable combination.
            Err(why) => {
                self.hotkey_status = (why.into(), AMBER);
            }
        }
    }

    fn start_capture(&mut self, binding: Binding) {
        self.capturing = Some(binding);
        self.hotkey_status = ("Press the shortcut, or Esc to cancel".into(), CYAN);
    }

    fn cancel_capture(&mut self) {
        if self.capturing.take().is_some() {
            self.hotkey_status = ("Shortcut unchanged".into(), MUTED);
        }
    }

    // ---- speech to text ----------------------------------------------------

    /// Start recording speech to type (the shortcut went down).
    fn start_dictation(&mut self) {
        if self.dictating.is_some() {
            return;
        }
        if !self.settings.speech_model.is_downloaded() {
            self.dictation_status = ("Download a speech model on the Dictation page first".into(), AMBER);
            return;
        }
        // An empty choice falls back to the system default microphone.
        match Recorder::start(Source::Microphone, &self.settings.microphone) {
            Ok(recorder) => {
                self.dictating = Some(DictationRecording {
                    recorder, model: self.settings.speech_model, hold_to_talk: self.settings.dictation_hold,
                });
                self.dictation_status = (String::new(), MUTED);
            }
            Err(e) => {
                logfile::warn(format_args!("dictation did not start: {e:#}"));
                self.dictation_status = (short(&format!("{e:#}")), RED);
            }
        }
    }

    /// Stop recording and type what was said.
    fn stop_dictation(&mut self) {
        if let Some(recording) = &mut self.dictating {
            recording.recorder.stop_capture();
        }
        if let Some((model, take)) = self.finish_dictation() {
            self.dictation.transcribe(model, take);
        }
    }

    /// Finish and validate a take before handing it to the transcription worker.
    fn finish_dictation(&mut self) -> Option<(dictation::SpeechModel, Take)> {
        if !self.dictating.as_ref().is_some_and(|recording| recording.recorder.ready_to_finish()) {
            return None;
        }
        let recording = self.dictating.take()?;
        self.dictation_status = match recording.recorder.finish() {
            Ok(Some(take)) => {
                if let Some(warning) = take.warning() {
                    self.dictation_status = (short(&format!("Dictation stopped: {warning}")), RED);
                    return None;
                }
                self.dictation_status = (String::new(), MUTED);
                return Some((recording.model, take));
            }
            Ok(None) => ("Nothing was heard".into(), MUTED),
            Err(e) => (short(&format!("{e:#}")), RED),
        };
        None
    }

    /// End the current take without queueing transcription or typing.
    fn discard_dictation(&mut self) {
        if self.dictating.take().is_some() {
            self.dictation_status = ("Dictation recording discarded".into(), MUTED);
        }
    }

    fn poll_dictation(&mut self) {
        if let Some(e) = self.dictating.as_ref().and_then(|recording| recording.recorder.error()) {
            self.dictating = None;
            self.dictation_status = (short(&e), RED);
        }
        // A toggle left on by mistake shouldn't record all day.
        if self.dictating.as_ref().is_some_and(|recording| {
            recording.recorder.is_stopping() || recording.recorder.elapsed() >= MAX_DICTATION
        }) {
            self.stop_dictation();
        }
        while let Some(event) = self.dictation.poll() {
            self.dictation_status = match event {
                dictation::Event::Downloaded(model) => (format!("{} is ready", model.label()), GREEN),
                dictation::Event::DownloadCancelled => ("Model download cancelled".into(), MUTED),
                dictation::Event::TranscriptionCancelled => ("Transcription cancelled".into(), MUTED),
                dictation::Event::DownloadFailed(e) => {
                    logfile::warn(format_args!("speech model download failed: {e}"));
                    (short(&e), RED)
                }
                dictation::Event::Transcribed { text, typing_error } => {
                    self.last_dictation = text;
                    match typing_error {
                        Some(error) if cfg!(windows) => (format!("Copy the transcript below: {error}"), AMBER),
                        Some(_) => ("Transcript ready below".into(), MUTED),
                        None => ("Typed".into(), GREEN),
                    }
                }
                dictation::Event::Nothing => ("No speech heard".into(), MUTED),
                dictation::Event::Failed(e) => {
                    logfile::warn(format_args!("speech to text failed: {e}"));
                    (short(&e), RED)
                }
            };
        }
    }

    /// Everything that must keep running while the window is hidden.
    fn tick(&mut self, ctx: &egui::Context) {
        while let Ok(command) = self.commands.1.try_recv() {
            self.run_command(ctx, command);
        }
        self.handle_close(ctx);
        if self.quitting || self.settings.load_error.is_some() { return; }
        self.poll_engine();
        if self.hidden || self.page != Page::Soundboard { self.close_trim_editor(); }
        self.poll_clip_loads(ctx);
        self.pad_durations.poll(ctx);
        self.poll_recording_save(ctx);
        self.poll_recording();
        self.poll_dictation();
        self.poll_mic_test();
        self.poll_clip_import(ctx);
        self.poll_updates();
        self.sync_default_mic();
        if (cable_input(&self.outputs).is_none() || self.waiting_for_device)
            && self.last_device_poll.elapsed() >= DEVICE_POLL
        {
            self.last_device_poll = Instant::now();
            self.refresh_devices(false);
        }
        self.sync_hotkeys(ctx);
        let (muted, bypassed, running) = (self.settings.mute, self.settings.bypass, self.running());
        let listening = self.dictating.as_ref().is_some_and(|recording| !recording.recorder.is_stopping());
        if let Some(tray) = &mut self.tray {
            tray.sync(muted, bypassed, running, listening, self.settings.collapsed);
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
        logfile::info("quitting");
        self.dictation.cancel_transcription();
        self.dictation.cancel_download();
        self.finish_pad_rename(true);
        self.clip_import.take();
        self.recorder.take();
        self.dictating.take();
        self.close_trim_editor();
        self.stop_engine(false);
        // Quit gets one final restoration attempt even if Stop just failed.
        self.default_mic_sync = DefaultMicSync::default();
        self.sync_default_mic();
        crate::virtual_mic::shutdown();
        self.save_settings();
    }

    /// Runs before every frame, and on its own while the window is hidden.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.tick(ctx);
        if self.quitting { return; }
        if self.settings.load_error.is_some() {
            ctx.request_repaint_after(Duration::from_millis(250));
            return;
        }
        self.minimize_to_strip(ctx);
        self.remember_window(ctx);
        self.show_overlay(ctx);
        // The live scope, the recorder and the pinned meter animate at ~30
        // fps. Hidden, a slow tick still handles device loss and the default
        // mic; tray and hotkey commands wake it immediately.
        let animating = self.engine.is_some() || self.recorder.is_some();
        let meter_live = self.strip_visible() && self.engine.is_some();
        let frame_time = match (self.hidden, animating) {
            (true, _) if meter_live => 33,
            (true, _) => 250,
            (false, true) => 33,
            (false, false) => 100,
        };
        ctx.request_repaint_after(Duration::from_millis(frame_time));
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if self.quitting { return; }
        if self.settings.load_error.is_some() {
            self.draw_settings_recovery(ui);
            return;
        }
        ui.ctx().plugin_or_default::<DeferredFocusInput>();
        let openers = egui::Id::new(("shortcut-capture-openers", ui.ctx().viewport_id()));
        ui.data_mut(|data| data.remove::<Vec<egui::Id>>(openers));
        let routed_keyboard = if self.capturing.is_some() {
            // Shortcut capture reads the next physical key from the original batch.
            ui.data_mut(|data| data.remove::<(Option<egui::Id>, Vec<egui::Event>)>(egui::Id::new("keyboard-input")));
            false
        } else {
            apply_keyboard_input(ui)
        };
        consume_captured_shortcut(ui.ctx());
        widgets::consume_held_activation(ui.ctx());
        let ctx = ui.ctx().clone();
        self.import_dropped_sounds(&ctx);
        self.capture_hotkey(&ctx);
        self.update_running_status();

        egui::Panel::left("sidebar")
            .resizable(false)
            .exact_size(SIDEBAR_WIDTH)
            .show(ui, |ui| self.draw_sidebar(ui));

        CentralPanel::default().show(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.heading(self.page.title());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    self.draw_update_status(ui);
                });
            });
            ui.separator();
            // The window can be shorter than a page (a small laptop
            // screen): pages scroll, the sidebar stays put.
            let mut scroll = ScrollArea::vertical()
                .id_salt(self.page.title())
                .auto_shrink([false; 2]);
            if self.page == Page::Soundboard && self.pad_to_reveal.is_some() {
                scroll = scroll.vertical_scroll_offset(0.0);
            }
            scroll.show(ui, |ui| match self.page {
                Page::Voice => self.draw_mic_tab(ui),
                Page::Soundboard => self.draw_sound_tab(ui),
                Page::Record => self.draw_record_page(ui),
                Page::Dictation => self.draw_dictation_page(ui),
                Page::Settings => self.draw_settings_tab(ui),
            });
        });
        if self.page != Page::Soundboard { self.close_trim_editor(); }
        if self.draw_release_notes(&ctx) {
            self.restore_release_notes_focus(&ctx);
        }
        self.draw_quit_confirmation(&ctx);
        if routed_keyboard && ctx.output(|output| output.ime.is_some()) {
            // Replayed arrows belong to the active text editor's caret.
            ctx.memory_mut(|memory| memory.move_focus(egui::FocusDirection::None));
        }
        let focused = ctx.memory(|memory| memory.focused());
        let capture = egui::Id::new(("shortcut-capture-active", ctx.viewport_id()));
        ui.data_mut(|data| {
            data.insert_temp(egui::Id::new("keyboard-focus"), focused);
            data.insert_temp(capture, self.capturing.is_some());
        });
    }
}

/// Width of the sidebar, points.
const SIDEBAR_WIDTH: f32 = 176.0;

impl App {
    /// Navigation above; below, what matters on every page: whether Discord
    /// hears you, your level, Mute and Start/Stop.
    fn draw_sidebar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("OpenMic").strong().size(17.0));
            self.draw_version(ui);
        });
        if cfg!(debug_assertions) {
            ui.label(RichText::new("dev build").small().color(widgets::text_color(ui, AMBER)))
                .on_hover_text("Built without --release: slower, and never updates itself");
        }
        ui.add_space(10.0);
        let navigation_height = (ui.available_height() - self.status_panel_height(ui) - ui.spacing().item_spacing.y).max(0.0);
        let navigation_layout = egui::vec2(ui.available_width(), navigation_height);
        let navigation_layout_id = egui::Id::new(("sidebar-navigation-layout", ui.ctx().viewport_id()));
        let navigation_reflowed = ui.data_mut(|data| {
            let previous = data.get_temp::<egui::Vec2>(navigation_layout_id);
            data.insert_temp(navigation_layout_id, navigation_layout);
            previous.is_some_and(|previous| previous != navigation_layout)
        });
        ScrollArea::vertical().id_salt("sidebar-navigation").max_height(navigation_height)
            .min_scrolled_height(0.0).auto_shrink([false, true]).show(ui, |ui| {
                for page in Page::ALL {
                    let label = match page {
                        Page::Record if self.recorder.as_ref().is_some_and(Recorder::is_stopping) => "Record  • finishing".to_owned(),
                        Page::Record if self.recorder.is_some() => "Record  ⏺ REC".to_owned(),
                        Page::Dictation if self.dictating.as_ref().is_some_and(|recording| recording.recorder.is_stopping()) => "Dictation  • finishing".to_owned(),
                        Page::Dictation if self.dictating.is_some() => "Dictation  • listening".to_owned(),
                        _ => page.title().to_owned(),
                    };
                    let selected = self.page == page;
                    let text = if selected { RichText::new(label).strong() } else { RichText::new(label) };
                    let response = ui.add_sized([ui.available_width(), 28.0], egui::Button::selectable(selected, text));
                    widgets::scroll_on_focus(&response);
                    if navigation_reflowed && response.has_focus()
                        && ui.input(|input| !input.pointer.any_down() && !input.pointer.any_released())
                    {
                        response.scroll_to_me(Some(egui::Align::Center));
                    }
                    if response.clicked() {
                        self.finish_pad_rename(true);
                        if !selected {
                            self.cancel_capture();
                        }
                        self.page = page;
                    }
                }
                // Keep the final button's focus outline inside fractional-height viewports.
                ui.add_space(1.0);
            });
        ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
            ui.add_space(8.0);
            self.draw_status_panel(ui);
        });
    }

    fn status_panel_height(&self, ui: &mut egui::Ui) -> f32 {
        // Measure only readonly content; the processing buttons run once in the real panel.
        let mut measure = ui.new_child(egui::UiBuilder::new().id_salt("status-size")
            .layout(egui::Layout::bottom_up(egui::Align::Min)).invisible().sizing_pass());
        measure.add_space(8.0);
        measure.allocate_exact_size(egui::vec2(measure.available_width(), 26.0), egui::Sense::hover());
        measure.add_space(4.0);
        measure.allocate_exact_size(egui::vec2(measure.available_width(), 6.0), egui::Sense::hover());
        measure.add_space(4.0);
        self.draw_status_details(&mut measure);
        measure.min_rect().height()
    }

    fn draw_status_panel(&mut self, ui: &mut egui::Ui) {
        // Laid out bottom-up: the last widget added is the top one.
        ui.horizontal(|ui| {
            let running = self.running();
            let width = (ui.available_width() - ui.spacing().item_spacing.x) / 2.0;
            let start = egui::Button::new(RichText::new(if running { "Stop" } else { "Start" }).color(Color32::BLACK))
                .fill(if running { RED } else { GREEN });
            let start = ui.add_sized([width, 26.0], start);
            self.restore_settings_retry_focus(&start, true);
            if start.clicked() {
                widgets::finish_keyboard_activation(ui.ctx());
                self.toggle_running();
                ui.ctx().request_repaint();
            }
            let muted = self.settings.mute;
            let mute = egui::Button::new(RichText::new(if muted { "Unmute" } else { "Mute" }).color(if muted { Color32::BLACK } else { ui.visuals().text_color() }))
                .fill(if muted { RED } else { ui.visuals().widgets.inactive.weak_bg_fill });
            if ui.add_sized([width, 26.0], mute).on_hover_text("Silence your voice; the soundboard still plays").clicked() {
                widgets::finish_keyboard_activation(ui.ctx());
                self.settings.mute = !muted;
                self.apply_live();
                ui.ctx().request_repaint();
            }
        });
        ui.add_space(4.0);
        // What Discord hears.
        let level = self.engine.as_ref().map(|e| e.stats().out_peak);
        let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 6.0), egui::Sense::hover());
        ui.painter().rect_filled(rect, 3.0, ui.visuals().extreme_bg_color);
        if let Some(peak) = level {
            let db = (20.0 * peak.max(1e-6).log10()).clamp(-60.0, 0.0);
            let fill = egui::Rect::from_min_max(rect.min, egui::pos2(rect.left() + rect.width() * (db + 60.0) / 60.0, rect.bottom()));
            let color = if db > -6.0 { RED } else if db > -18.0 { AMBER } else { GREEN };
            ui.painter().rect_filled(fill, 3.0, color);
        }
        ui.add_space(4.0);
        self.draw_status_details(ui);
    }

    fn draw_status_details(&self, ui: &mut egui::Ui) {
        if let Some(error) = &self.save_error {
            ui.label(RichText::new("Settings not saved · retrying…").small().color(widgets::text_color(ui, RED))).on_hover_text(error);
        }
        // Details beyond the state line ("Running · voice 42%", a warning).
        if self.status.0 != "Stopped" {
            ui.add(egui::Label::new(RichText::new(&self.status.0).small().color(widgets::text_color(ui, self.status.1))).wrap());
        }
        let (state, color) = match (self.engine.is_some(), self.settings.mute) {
            (true, true) => ("Muted", RED),
            (true, false) => ("Live: Discord hears you", GREEN),
            (false, _) if self.waiting_for_device => ("Waiting for a device", AMBER),
            (false, _) => ("Stopped", MUTED),
        };
        ui.label(RichText::new(format!("• {state}")).strong().color(widgets::text_color(ui, color)));
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

fn shortcut_keyboard_event(event: &egui::Event) -> bool {
    matches!(event, egui::Event::Key { pressed: true, .. } | egui::Event::Text(_)
        | egui::Event::Paste(_) | egui::Event::Copy | egui::Event::Cut
        | egui::Event::Ime(_))
}

fn shortcut_key_matches((logical, physical): (egui::Key, Option<egui::Key>), (key, scan): (egui::Key, Option<egui::Key>)) -> bool {
    physical.zip(scan).map_or(logical == key, |(physical, scan)| physical == scan)
}

fn shortcut_capture_opener(response: &egui::Response) {
    if response.enabled() {
        let id = egui::Id::new(("shortcut-capture-openers", response.ctx.viewport_id()));
        response.ctx.data_mut(|data| data.get_temp_mut_or_default::<Vec<egui::Id>>(id).push(response.id));
    }
}

/// A captured or rejected shortcut key stays out of the UI until it is released.
fn consume_captured_shortcut(ctx: &egui::Context) {
    let id = egui::Id::new(("captured-shortcut-keys", ctx.viewport_id()));
    let Some(mut held) = ctx.data(|data| data.get_temp::<Vec<(egui::Key, Option<egui::Key>)>>(id)) else { return };
    let mut text_from_capture = true;
    let mut navigation = false;
    let mut escape = false;
    if ctx.input(|input| input.focused) {
        ctx.input_mut(|input| {
            input.events.retain(|event| {
                if let egui::Event::Key { key, physical_key, pressed, .. } = event {
                    if let Some(index) = held.iter().position(|saved| shortcut_key_matches(*saved, (*key, *physical_key)))
                    {
                        if !pressed {
                            held.remove(index);
                            text_from_capture = !held.is_empty();
                            return true;
                        }
                        text_from_capture = true;
                        navigation |= matches!(key, egui::Key::Tab | egui::Key::ArrowUp | egui::Key::ArrowDown
                            | egui::Key::ArrowLeft | egui::Key::ArrowRight | egui::Key::Escape);
                        escape |= *key == egui::Key::Escape;
                        return false;
                    }
                    if *pressed && !is_modifier(*key) { text_from_capture = false; }
                }
                !(text_from_capture && !matches!(event, egui::Event::Key { .. }) && shortcut_keyboard_event(event))
            });
            // A different physical key can release the same logical key in egui.
            held.retain(|(logical, physical)| physical.is_some() || input.key_down(*logical));
        });
    } else { held.clear(); }
    if navigation {
        // egui sees navigation before App::ui. Recompute it from the remaining input.
        let direction = ctx.input(|input| input.events.iter().rev().find_map(keyboard_focus_direction)
            .unwrap_or(egui::FocusDirection::None));
        ctx.memory_mut(|memory| memory.move_focus(direction));
    }
    if escape && ctx.memory(|memory| memory.focused()).is_none()
        && let Some(owner) = ctx.data(|data| data.get_temp::<Option<egui::Id>>(egui::Id::new("keyboard-focus"))).flatten()
    {
        ctx.memory_mut(|memory| memory.request_focus(owner));
    }
    ctx.data_mut(|data| {
        if held.is_empty() { data.remove::<Vec<(egui::Key, Option<egui::Key>)>>(id); }
        else { data.insert_temp(id, held); }
    });
}

/// Whether a window placed at `px` (its top-left corner, screen pixels)
/// would show on a connected monitor; a monitor unplugged since then would
/// leave it off screen.
#[cfg(windows)]
fn on_a_monitor([x, y]: [i32; 2]) -> bool {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::Graphics::Gdi::{MONITOR_DEFAULTTONULL, MonitorFromPoint};
    // A point inside the main title bar or the strip's microphone.
    // SAFETY: a pure query on a point.
    !unsafe { MonitorFromPoint(POINT { x: x.saturating_add(16), y: y.saturating_add(16) }, MONITOR_DEFAULTTONULL) }.is_invalid()
}

#[cfg(target_os = "linux")]
fn on_a_monitor(px: [i32; 2]) -> bool {
    crate::monitors::on_a_monitor(px)
}

#[cfg(not(any(windows, target_os = "linux")))]
fn on_a_monitor(_: [i32; 2]) -> bool {
    true
}

fn open_url(ctx: &egui::Context, target: &str) {
    widgets::finish_keyboard_activation(ctx);
    #[cfg(test)]
    ctx.data_mut(|data| data.get_temp_mut_or_default::<Vec<String>>(egui::Id::new("opened-urls")).push(target.to_owned()));
    // Tests record the target instead of launching a browser or file manager.
    #[cfg(not(test))]
    {
        let program = open_program();
        if let Err(error) = spawn_open(program, target) {
            crate::logfile::warn(format_args!("could not open {target} with {program}: {error}"));
        }
    }
}

/// Explorer opens a URL in the browser and a folder in the file manager.
/// Linux uses `xdg-open` for both.
fn open_program() -> &'static str {
    if cfg!(windows) { "explorer" } else { "xdg-open" }
}

fn spawn_open(program: &str, target: &str) -> std::io::Result<()> {
    std::process::Command::new(program)
        .arg(target)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
}

/// Input that can change a numeric field's text, including undo and redo.
fn number_text_edit(ui: &egui::Ui) -> bool {
    ui.input(|input| !input.pointer.any_pressed() && input.events.iter().any(|event| matches!(event,
        egui::Event::Text(_) | egui::Event::Paste(_) | egui::Event::Cut
            | egui::Event::Ime(egui::ImeEvent::Commit(_))
            | egui::Event::Key { key: egui::Key::Backspace | egui::Key::Delete | egui::Key::Z | egui::Key::Y, pressed: true, .. }
            | egui::Event::AccessKitActionRequest(egui::accesskit::ActionRequest {
                action: egui::accesskit::Action::SetValue, data: Some(egui::accesskit::ActionData::Value(_)), ..
            }))))
}

fn keyboard_edit(event: &egui::Event) -> bool {
    matches!(event, egui::Event::Text(_) | egui::Event::Paste(_) | egui::Event::Copy | egui::Event::Cut
        | egui::Event::Ime(_)
        | egui::Event::Key { pressed: true, key: egui::Key::ArrowLeft | egui::Key::ArrowRight
            | egui::Key::ArrowUp | egui::Key::ArrowDown | egui::Key::Home | egui::Key::End
            | egui::Key::Backspace | egui::Key::Delete | egui::Key::A | egui::Key::Z | egui::Key::Y, .. })
}

fn keyboard_focus_direction(event: &egui::Event) -> Option<egui::FocusDirection> {
    use egui::{Event, FocusDirection, Key};
    match event {
        Event::Key { key: Key::Tab, pressed: true, modifiers, .. } if modifiers.shift_only() => Some(FocusDirection::Previous),
        Event::Key { key, pressed: true, modifiers, .. } if modifiers.is_none() => match key {
            Key::Tab => Some(FocusDirection::Next),
            Key::ArrowLeft => Some(FocusDirection::Left),
            Key::ArrowRight => Some(FocusDirection::Right),
            Key::ArrowUp => Some(FocusDirection::Up),
            Key::ArrowDown => Some(FocusDirection::Down),
            _ => None,
        },
        _ => None,
    }
}

/// Process earlier edits before pointer or assistive focus changes and commands.
#[derive(Default)]
struct DeferredFocusInput {
    events: VecDeque<Vec<egui::Event>>,
}

impl egui::plugin::Plugin for DeferredFocusInput {
    fn debug_name(&self) -> &'static str { "Deferred focus input" }

    fn input_hook(&mut self, ctx: &egui::Context, input: &mut egui::RawInput) {
        if input.viewport_id != egui::ViewportId::ROOT { return; }
        let key = egui::Id::new("keyboard-input");
        let capturing = ctx.data(|data| data.get_temp::<bool>(
            egui::Id::new(("shortcut-capture-active", input.viewport_id)))).unwrap_or(false);
        let mut held = ctx.data(|data| data.get_temp::<Vec<(egui::Key, Option<egui::Key>)>>(
            egui::Id::new(("captured-shortcut-keys", input.viewport_id)))).unwrap_or_default();
        if !capturing && input.events.iter().any(|event| {
            if let egui::Event::Key { key, physical_key, pressed, .. } = event {
                let scan = (*key, *physical_key);
                if !pressed { held.retain(|saved| !shortcut_key_matches(*saved, scan)); }
                else if *key == egui::Key::Escape { return !held.iter().any(|saved| shortcut_key_matches(*saved, scan)); }
            }
            false
        })
        {
            // Escape cancels an edit that has not been shown yet.
            ctx.data_mut(|data| data.remove::<(Option<egui::Id>, Vec<egui::Event>)>(key));
            for events in &mut self.events {
                events.retain(|event| !keyboard_edit(event) && !matches!(event, egui::Event::Key { pressed: true, .. }));
            }
            self.events.retain(|events| !events.is_empty());
        }
        let pending = ctx.data(|data| data.get_temp::<(Option<egui::Id>, Vec<egui::Event>)>(key).is_some());
        let assistive_focus = |event: &egui::Event| matches!(event,
            egui::Event::AccessKitActionRequest(egui::accesskit::ActionRequest {
                action: egui::accesskit::Action::Focus, target_tree: egui::accesskit::TreeId::ROOT, data: None, ..
            }));
        let changes_focus = input.events.iter().any(|event| assistive_focus(event) || matches!(event,
            egui::Event::PointerButton { pressed: true, .. }));
        if (pending && changes_focus) || !self.events.is_empty() {
            // Preserve input frames: egui does not recognize a drag when its
            // press and release arrive together in the same pass.
            if !input.events.is_empty() {
                self.events.push_back(std::mem::take(&mut input.events));
            }
            if !pending {
                input.events = self.events.pop_front().unwrap_or_default();
            }
        }
        if !pending && let Some(focus) = input.events.iter().position(assistive_focus) {
            // egui claims assistive focus as its target is drawn. Commands must
            // wait until then, and earlier input must use the previous focus.
            let tail = input.events.split_off(if focus == 0 { 1 } else { focus });
            if !tail.is_empty() { self.events.push_front(tail); }
        }
        if capturing {
            let mut candidate = false;
            let next = input.events.iter().enumerate().find_map(|(index, event)| {
                if let egui::Event::Key { key, physical_key, pressed, repeat, .. } = event
                    && !is_modifier(physical_key.unwrap_or(*key))
                {
                    let scan = (*key, *physical_key);
                    if !pressed { held.retain(|saved| !shortcut_key_matches(*saved, scan)); }
                    else if !repeat && !held.iter().any(|saved| shortcut_key_matches(*saved, scan)) {
                        if candidate { return Some(index); }
                        candidate = true;
                        held.push(scan);
                    }
                }
                None
            });
            if let Some(next) = next {
                // Reject or accept this key before later navigation or Escape runs.
                let tail = input.events.split_off(next);
                self.events.push_front(tail);
            }
        }
        let openers = ctx.data(|data| data.get_temp::<Vec<egui::Id>>(
            egui::Id::new(("shortcut-capture-openers", input.viewport_id)))).unwrap_or_default();
        let openers: Vec<_> = openers.into_iter().filter_map(|id| ctx.read_response(id)).filter(egui::Response::enabled).collect();
        let focused = ctx.memory(|memory| memory.focused());
        let (mut enter, mut space) = ctx.input(|input| (input.key_down(egui::Key::Enter), input.key_down(egui::Key::Space)));
        let activation = input.events.iter().position(|event| match event {
            egui::Event::Key { key: key @ (egui::Key::Enter | egui::Key::Space), pressed, .. } => {
                let down = if *key == egui::Key::Enter { &mut enter } else { &mut space };
                let fresh = *pressed && !*down;
                *down = *pressed;
                fresh && openers.iter().any(|response| focused == Some(response.id))
            }
            egui::Event::AccessKitActionRequest(egui::accesskit::ActionRequest {
                action: egui::accesskit::Action::Click, target_tree: egui::accesskit::TreeId::ROOT, target_node, ..
            }) => openers.iter().any(|response| target_node.0 == response.id.value()),
            egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: false, .. } =>
                openers.iter().any(|response| response.is_pointer_button_down_on() && response.interact_rect.contains(*pos)
                    && ctx.layer_id_at(*pos) == Some(response.layer_id)),
            _ => false,
        });
        if let Some(activation) = activation {
            // Draw Set before later keys can change focus or activate its replacement.
            let tail = input.events.split_off(activation + 1);
            if !tail.is_empty() { self.events.push_front(tail); }
        }
    }

    fn on_end_pass(&mut self, ui: &mut egui::Ui) {
        if ui.ctx().viewport_id() == egui::ViewportId::ROOT && !self.events.is_empty() {
            ui.ctx().request_discard("Process input in order before changing focus");
            ui.ctx().request_repaint();
        }
    }
}

fn defer_keyboard_input(ui: &egui::Ui, owner: Option<egui::Id>, mut events: Vec<egui::Event>) {
    if events.is_empty() { return; }
    ui.data_mut(|data| {
        let key = egui::Id::new("keyboard-input");
        if let Some((_, remaining)) = data.remove_temp::<(Option<egui::Id>, Vec<egui::Event>)>(key) {
            events.extend(remaining);
        }
        data.insert_temp(key, (owner, events));
    });
    ui.ctx().request_discard("Apply keyboard input in order after focus settles");
    ui.ctx().request_repaint();
}

/// Process input before Tab, navigate, then deliver the remaining command in order.
fn apply_keyboard_input(ui: &egui::Ui) -> bool {
    use egui::FocusDirection;
    let pending = ui.data_mut(|data| data.remove_temp::<(Option<egui::Id>, Vec<egui::Event>)>(egui::Id::new("keyboard-input")));
    let mut routed = false;
    if let Some((owner, mut events)) = pending
        && owner.is_none_or(|id| ui.memory(|memory| memory.has_focus(id)))
    {
        let focused = ui.memory(|memory| memory.focused());
        let rendered_focus = ui.data(|data| data.get_temp::<Option<egui::Id>>(egui::Id::new("keyboard-focus"))).flatten();
        if owner.is_none() && focused != rendered_focus
            && ui.input(|input| !input.pointer.any_pressed() && !input.key_pressed(egui::Key::Escape))
        {
            // Backward Tab applies focus at the start of the next pass. Render
            // the target first so native selection and Enter's blur can work.
            ui.input_mut(|input| events.append(&mut input.events));
            defer_keyboard_input(ui, None, events);
            ui.memory_mut(|memory| memory.move_focus(FocusDirection::None));
            return true;
        }
        ui.input_mut(|input| {
            events.append(&mut input.events);
            input.events = events;
        });
        routed = true;
        ui.ctx().request_repaint();
    }
    let split = ui.input(|input| {
        if input.pointer.any_pressed() || input.key_pressed(egui::Key::Escape) { return None; }
        let tabs = input.events.iter().enumerate().filter_map(|(index, event)| {
            let direction = keyboard_focus_direction(event)?;
            matches!(direction, FocusDirection::Next | FocusDirection::Previous).then_some(index)
        }).collect::<Vec<_>>();
        let command = input.events.iter().any(|event| keyboard_edit(event) || matches!(event,
            egui::Event::Key { key, pressed: true, .. } if *key != egui::Key::Tab));
        if routed || tabs.len() > 1 || command { tabs.first().copied() } else { None }
    });
    if let Some(tab) = split {
        let tail = ui.input_mut(|input| input.events.split_off(if tab == 0 { 1 } else { tab }));
        defer_keyboard_input(ui, None, tail);
        routed = true;
    }
    if routed {
        let direction = ui.input(|input| input.events.iter().rev().find_map(keyboard_focus_direction).unwrap_or(FocusDirection::None));
        ui.memory_mut(|memory| memory.move_focus(direction));
    }
    routed
}

/// egui 0.36 selects all text after processing input when a number gains focus.
fn number_field(ui: &mut egui::Ui, widget: egui::DragValue<'_>) -> egui::Response {
    let id = ui.next_auto_id();
    let layer = ui.layer_id();
    let editing_input = ui.input(|input| !input.pointer.any_pressed() && input.events.iter().any(keyboard_edit));
    let requested_elsewhere = ui.input(|input| input.events.iter().rev().find_map(|event| match event {
        egui::Event::AccessKitActionRequest(egui::accesskit::ActionRequest {
            action: egui::accesskit::Action::Focus, target_tree: egui::accesskit::TreeId::ROOT, target_node, ..
        }) => Some(*target_node != id.accesskit_id()),
        _ => None,
    }).unwrap_or(false));
    if editing_input && requested_elsewhere {
        // AccessKit focus is claimed when its target is drawn. Until then,
        // the previous field must not receive the target's keyboard input.
        let events = ui.input_mut(|input| std::mem::take(&mut input.events));
        ui.input_mut(|input| input.events = events.iter().filter(|event|
            matches!(event, egui::Event::AccessKitActionRequest(_))).cloned().collect());
        let response = ui.add(widget);
        ui.input_mut(|input| input.events = events);
        return response;
    }
    let focused = editing_input && ui.is_enabled() && ui.memory_mut(|memory| {
        memory.interested_in_focus(id, layer);
        memory.has_focus(id)
    });
    if focused { ui.memory_mut(|memory| memory.move_focus(egui::FocusDirection::None)); }
    let gained = focused && ui.ctx().read_response(id).is_none_or(|response| response.gained_focus());
    if !gained { return ui.add(widget); }
    let events = ui.input_mut(|input| std::mem::take(&mut input.events));
    let pending = events.iter().filter(|event| !matches!(event,
        egui::Event::Key { key: egui::Key::Tab, .. }
            | egui::Event::AccessKitActionRequest(egui::accesskit::ActionRequest {
                action: egui::accesskit::Action::Focus, .. }))).cloned().collect::<Vec<_>>();
    // The next pass handles input with the selection in place, preserving
    // native caret movement, selection, undo, and event order.
    defer_keyboard_input(ui, Some(id), pending);
    let response = ui.add(widget);
    ui.input_mut(|input| input.events = events);
    response
}

/// A 0..100% volume slider that also takes the mouse wheel (1% per notch).
fn volume_slider(ui: &mut egui::Ui, value: &mut f32) -> egui::Response {
    let mut response = ui.horizontal(|ui| {
        let slider = ui.add(Slider::new(value, 0.0..=1.0).show_value(false));
        let original = *value;
        let focus_before = ui.memory(|memory| memory.focused());
        let text_edit = number_text_edit(ui);
        let submitted = ui.input(|input| input.key_pressed(egui::Key::Enter) && !input.key_pressed(egui::Key::Tab));
        let number = number_field(ui, egui::DragValue::new(value).range(0.0..=1.0).speed(0.01)
            .custom_formatter(|v, _| format!("{:.0}%", v * 100.0))
            .custom_parser(|text| {
                // Live text edits already update the value. Re-parsing the
                // rounded display on blur could override a slider adjustment.
                if !text_edit { return None; }
                let text = text.trim();
                text.strip_suffix('%').unwrap_or(text).trim().parse::<f64>().ok()
                    .filter(|value| value.is_finite()).map(|value| value / 100.0)
            }));
        let focused = number.has_focus();
        let explicit_edit = number.dragged() || ui.input(|input| {
            use egui::accesskit::Action;
            focused || (submitted && focus_before == Some(number.id))
                || [Action::SetValue, Action::Increment, Action::Decrement].into_iter()
                    .any(|action| input.has_accesskit_action_request(number.id, action))
        });
        if number.lost_focus() && text_edit && !explicit_edit { *value = original; }
        if number.gained_focus() || focused || number.lost_focus() { number.union(slider) } else { slider.union(number) }
    }).inner;
    let notches = viz::wheel_notches(ui, response.id, response.hovered() && !response.dragged());
    if viz::step_by(value, notches, 0.01, 0.0..=1.0) { response.mark_changed(); }
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Pad;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn links_and_folders_use_the_platform_opener() {
        if cfg!(windows) {
            assert_eq!(open_program(), "explorer");
        } else {
            assert_eq!(open_program(), "xdg-open");
        }
    }

    #[test]
    fn a_missing_opener_is_reported() {
        let error = spawn_open("openmic-missing-opener", "https://example.com").unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    }

    #[test]
    fn loaded_decibel_controls_stay_within_the_supported_audio_range() {
        for (db, input_db, output_db, gate_db) in [
            (1e30, 24.0, 12.0, -20.0),
            (-1e30, -12.0, -24.0, -80.0),
        ] {
            let settings: Settings = serde_json::from_value(serde_json::json!({
                "input_gain_db": db, "output_gain_db": db, "gate_threshold_db": db,
            })).unwrap();
            let app = App::stopped(settings);
            let params = app.params();
            assert!(params.input_gain.is_finite() && params.output_gain.is_finite());
            assert_eq!(params.input_gain, dsp::db_to_gain(input_db));
            assert_eq!(params.output_gain, dsp::db_to_gain(output_db));
            assert_eq!(params.gate_threshold_db, gate_db);
        }
        let app = App::stopped(Settings {
            input_gain_db: 6.0, output_gain_db: -6.0, gate_threshold_db: -50.0, ..Settings::default()
        });
        let params = app.params();
        assert_eq!(params.input_gain, dsp::db_to_gain(6.0));
        assert_eq!(params.output_gain, dsp::db_to_gain(-6.0));
        assert_eq!(params.gate_threshold_db, -50.0);
    }

    #[test]
    fn a_capture_error_distinguishes_recovered_audio_from_an_empty_recording() {
        for count in [0, 480] {
            let mut app = App::stopped(Settings::default());
            let recorder = Recorder::from_samples_for_test(&vec![0.25; count]);
            recorder.report_error_for_test("microphone disconnected");
            app.recorder = Some(recorder);
            app.poll_recording();
            assert!(app.recorder.is_none());
            assert!(app.record_status.0.contains("microphone disconnected"));
            if count == 0 {
                assert!(app.take.is_none());
                assert_eq!(app.record_status.1, RED);
            } else {
                let take = app.take.as_ref().unwrap();
                assert_eq!(take.warning(), Some("microphone disconnected"));
                assert_eq!(take.samples().unwrap().len(), count / 2);
                assert!(app.record_status.0.starts_with("Partial take ready"));
                assert_eq!(app.record_status.1, AMBER);
                assert!(app.update_blocker().is_some(), "the recovered audio still needs to be saved");
            }
        }
    }

    #[test]
    fn dictation_does_not_type_a_take_that_skipped_audio() {
        let mut app = App::stopped(Settings::default());
        let recorder = Recorder::from_samples_for_test(&[0.25; 96]);
        recorder.report_skipped_for_test();
        app.dictating = Some(DictationRecording { recorder, model: app.settings.speech_model, hold_to_talk: true });
        app.stop_dictation();
        assert!(app.dictating.is_none());
        assert!(!app.dictation.busy());
        assert!(app.dictation_status.0.contains("audio was skipped"));
        assert_eq!(app.dictation_status.1, RED);
    }

    #[test]
    fn empty_failed_dictation_reports_the_failure_when_the_shortcut_stops_it() {
        for skipped in [false, true] {
            let mut app = App::stopped(Settings::default());
            app.last_dictation = "Previous phrase".into();
            let recorder = Recorder::from_samples_for_test(&[]);
            recorder.report_error_for_test("Mic disconnected");
            if skipped { recorder.report_skipped_for_test(); }
            app.dictating = Some(DictationRecording {
                recorder, model: app.settings.speech_model, hold_to_talk: true,
            });

            // Hotkey commands run before capture polling, so finishing itself
            // must preserve failures even when no complete frames were written.
            app.run_command(&egui::Context::default(), Command::DictateReleased);
            assert!(app.dictating.is_none() && !app.dictation.busy());
            assert_eq!(app.dictation_status.1, RED);
            assert!(app.dictation_status.0.contains("Mic disconnected"));
            assert_eq!(app.dictation_status.0.contains("audio was skipped"), skipped);
            assert_eq!(app.last_dictation, "Previous phrase");
        }
    }

    #[test]
    fn dictation_stop_waits_for_the_writer_without_blocking_or_submitting_twice() {
        let (dictation, transcribe) = Dictation::delayed_for_test();
        let (recorder, finish) = Recorder::delayed_writer_for_test(&[0.25; 96]);
        let mut app = App::stopped(Settings::default());
        app.dictation = dictation;
        app.page = Page::Dictation;
        app.last_dictation = "Previous phrase".into();
        app.dictating = Some(DictationRecording {
            recorder, model: dictation::SpeechModel::TinyEn, hold_to_talk: true,
        });
        app.settings.speech_model = dictation::SpeechModel::Small;

        let ctx = egui::Context::default();
        app.run_command(&ctx, Command::DictateReleased);
        for _ in 0..3 {
            app.poll_dictation();
            app.run_command(&ctx, Command::DictateReleased);
            app.run_command(&ctx, Command::Dictate);
        }
        let recording = app.dictating.as_ref().unwrap();
        assert!(recording.recorder.is_stopping() && !recording.recorder.ready_to_finish());
        assert_eq!(recording.model, dictation::SpeechModel::TinyEn);
        assert!(!app.dictation.busy(), "the WAV must finish before transcription starts");
        assert_eq!(app.update_blocker(), Some("Wait for speech to text to finish"));
        let (texts, _) = render(&mut app, egui::vec2(700.0, 420.0));
        assert!(visible(&texts, "Finishing recording…").is_some());
        assert!(visible(&texts, "Listening…").is_none());
        assert!(visible(&texts, "Discard recording").is_some());
        assert!(visible(&texts, "Previous phrase").is_some());
        app.run_command(&ctx, Command::ToggleMute);
        assert!(app.settings.mute, "mic controls remain responsive while finalizing");

        finish.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while app.dictating.is_some() {
            app.poll_dictation();
            assert!(Instant::now() < deadline, "dictation did not finish writing");
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(app.dictation.busy(), "the finished take is handed to the existing worker");
        app.run_command(&ctx, Command::DictateReleased);
        transcribe.send(()).unwrap();
        while app.dictation.busy() || app.dictation_status.0 != "No speech heard" {
            app.poll_dictation();
            assert!(Instant::now() < deadline, "the short fixture did not finish transcription");
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(app.last_dictation, "Previous phrase");
        assert_eq!(app.settings.speech_model, dictation::SpeechModel::Small);
    }

    #[test]
    fn discarded_or_failed_dictation_releases_capture_without_waiting_or_typing() {
        for capture_error in [None, Some("Microphone disconnected")] {
            let (recorder, finish) = Recorder::delayed_writer_for_test(&[0.25; 96]);
            if let Some(error) = capture_error { recorder.report_error_for_test(error); }
            let mut app = App::stopped(Settings::default());
            app.page = Page::Dictation;
            app.last_dictation = "Previous phrase".into();
            app.dictating = Some(DictationRecording {
                recorder, model: app.settings.speech_model, hold_to_talk: false,
            });
            if capture_error.is_some() {
                app.poll_dictation();
            } else {
                app.stop_dictation();
                app.discard_dictation();
            }
            let expected = app.dictation_status.clone();
            for _ in 0..3 {
                app.poll_dictation();
                app.run_command(&egui::Context::default(), Command::DictateReleased);
            }
            assert!(app.dictating.is_none() && !app.dictation.busy());
            assert!(app.update_blocker().is_none(), "discard immediately releases the recording slot");
            let (texts, _) = render(&mut app, egui::vec2(700.0, 420.0));
            assert!(visible(&texts, "Finishing recording…").is_none());
            assert!(visible(&texts, "Listening…").is_none());
            assert!(visible(&texts, "Discard recording").is_none());
            assert!(visible(&texts, &expected.0).is_some());
            assert_eq!(expected, (capture_error.unwrap_or("Dictation recording discarded").into(), if capture_error.is_some() { RED } else { MUTED }));

            finish.send(()).expect("discard must not wait for the writer to drain");
            app.poll_dictation();
            assert!(!app.dictation.busy(), "discarded takes never reach transcription");
            assert_eq!(app.dictation_status, expected);
            assert_eq!(app.last_dictation, "Previous phrase");
            assert!(app.update_blocker().is_none());
        }
    }

    #[test]
    fn active_dictation_keeps_its_original_stop_behavior_when_the_mode_changes() {
        let ctx = egui::Context::default();
        for hold_to_talk in [true, false] {
            let mut app = App::stopped(Settings { dictation_hold: hold_to_talk, ..Settings::default() });
            app.dictating = Some(DictationRecording {
                recorder: Recorder::from_samples_for_test(&[]), model: app.settings.speech_model, hold_to_talk,
            });
            app.settings.dictation_hold = !hold_to_talk;
            let (ignored, stops) = if hold_to_talk {
                (Command::Dictate, Command::DictateReleased)
            } else {
                (Command::DictateReleased, Command::Dictate)
            };
            app.run_command(&ctx, ignored);
            assert!(app.dictating.is_some(), "the new preference must not change the current take");
            app.run_command(&ctx, stops);
            assert!(app.dictating.is_none(), "the original stop gesture must still work");
            assert!(!app.dictation.busy(), "the empty fixture never loads a speech model");
            assert_eq!(app.dictation_status.0, "Nothing was heard");
            assert_eq!(app.settings.dictation_hold, !hold_to_talk, "keep the preference for the next take");
        }
    }

    #[test]
    fn discarding_dictation_stops_capture_without_typing_or_losing_the_previous_transcript() {
        let mut app = App::stopped(Settings::default());
        app.last_dictation = "Previous transcript".into();
        for hold_to_talk in [true, false] {
            app.dictating = Some(DictationRecording {
                recorder: Recorder::from_samples_for_test(&[0.25; 96]), model: app.settings.speech_model, hold_to_talk,
            });
            app.discard_dictation();
            assert!(app.dictating.is_none());
            assert!(!app.dictation.busy(), "discarded audio must never reach the transcription worker");
            assert_eq!(app.last_dictation, "Previous transcript");
            assert_eq!(app.dictation_status.0, "Dictation recording discarded");

            app.run_command(&egui::Context::default(), Command::DictateReleased);
            assert!(app.dictating.is_none());
            assert!(!app.dictation.busy(), "a late key release must not transcribe the discarded take");
        }
    }

    #[test]
    fn finished_dictation_keeps_its_original_model_after_the_preference_changes() {
        let mut app = App::stopped(Settings::default());
        app.dictating = Some(DictationRecording {
            recorder: Recorder::from_samples_for_test(&[0.25; 96]),
            model: dictation::SpeechModel::TinyEn,
            hold_to_talk: false,
        });
        app.settings.speech_model = dictation::SpeechModel::Small;

        let (model, take) = app.finish_dictation().expect("take prepared for transcription");
        assert_eq!(model, dictation::SpeechModel::TinyEn);
        assert_eq!(take.samples().unwrap().len(), 48);
        assert!(app.dictating.is_none());
        assert_eq!(app.settings.speech_model, dictation::SpeechModel::Small,
            "keep the new preference for the next recording");
    }

    #[test]
    fn quit_and_window_close_preserve_active_and_unsaved_recordings() {
        for active in [true, false] {
            for window_close in [true, false] {
                let ctx = egui::Context::default();
                let mut app = App::stopped(Settings { close_to_tray: false, collapsed: true, ..Settings::default() });
                let recorder = Recorder::from_samples_for_test(&[0.25; 480]);
                if active {
                    app.recorder = Some(recorder);
                } else {
                    app.take = recorder.finish().unwrap().map(Arc::new);
                }
                app.hidden = true;
                let mut input = egui::RawInput::default();
                if window_close {
                    input.viewports.entry(egui::ViewportId::ROOT).or_default().events.push(egui::ViewportEvent::Close);
                }
                let output = ctx.run_ui(input, |ui| {
                    if window_close {
                        app.handle_close(ui.ctx());
                    } else {
                        app.run_command(ui.ctx(), Command::Quit);
                    }
                });
                let commands = &output.viewport_output[&egui::ViewportId::ROOT].commands;
                assert!(commands.contains(&ViewportCommand::CancelClose));
                assert!(!commands.contains(&ViewportCommand::Close));
                assert!(app.confirm_quit && !app.quitting);
                assert!(!app.hidden && !app.settings.collapsed, "a hidden quit request must reveal the choice");
                assert_eq!(app.page, Page::Record);
                assert_eq!(app.recorder.is_some(), active, "asking to quit must not stop capture");
                let take = if active { Arc::new(app.recorder.take().unwrap().finish().unwrap().unwrap()) } else { app.take.take().unwrap() };
                assert_eq!(take.samples().unwrap().len(), 240, "no audio was discarded");
            }
        }
    }

    #[test]
    fn quit_confirmation_can_return_to_the_take_or_explicitly_discard_it() {
        use eframe::App as _;
        for (active, choice) in [
            (false, "Back to recording"), (true, "Keep recording"),
            (false, "Discard and quit"), (true, "Discard and quit"),
            (false, "Escape"), (true, "Escape"),
        ] {
            let mut app = App::stopped(Settings::default());
            let recorder = Recorder::from_samples_for_test(&[0.25; 480]);
            if active {
                app.recorder = Some(recorder);
            } else {
                app.take = recorder.finish().unwrap().map(Arc::new);
            }
            let ctx = egui::Context::default();
            app.run_command(&ctx, Command::Quit);
            let mut frame = eframe::Frame::_new_kittest();
            let mut draw = |app: &mut App, events| {
                ctx.run_ui(egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(700.0, 420.0))),
                    events,
                    ..Default::default()
                }, |ui| app.ui(ui, &mut frame))
            };
            draw(&mut app, Vec::new());
            let output = draw(&mut app, Vec::new());
            let output = if choice == "Escape" {
                draw(&mut app, vec![egui::Event::Key {
                    key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE,
                }])
            } else {
                let pos = output.shapes.iter().find_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) if text.galley.text() == choice => Some(text.visual_bounding_rect().center()),
                    _ => None,
                }).unwrap_or_else(|| panic!("{choice} must be visible"));
                let mut click = |pressed| draw(&mut app, vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE },
                ]);
                click(true);
                click(false)
            };
            let discard = choice == "Discard and quit";
            assert!(!app.confirm_quit);
            assert_eq!(app.quitting, discard);
            assert_eq!(output.viewport_output[&egui::ViewportId::ROOT].commands.contains(&ViewportCommand::Close), discard);
            assert_eq!(app.recorder.is_some(), active, "capture continues until actual exit");
            let take = if active { Arc::new(app.recorder.take().unwrap().finish().unwrap().unwrap()) } else { app.take.take().unwrap() };
            assert_eq!(take.samples().unwrap().len(), 240, "the take remains intact until actual exit");
        }

        let mut app = App::stopped(Settings::default());
        app.run_command(&egui::Context::default(), Command::Quit);
        assert!(app.quitting && !app.confirm_quit, "nothing to lose: quit immediately");
    }

    #[test]
    fn quitting_cancels_transcription_and_ignores_commands_during_shutdown() {
        for confirm_first in [false, true] {
            let (dictation, release) = Dictation::delayed_for_test();
            let mut app = App::stopped(Settings::default());
            app.dictation = dictation;
            app.dictating = Some(DictationRecording {
                recorder: Recorder::from_samples_for_test(&[0.25; 96]),
                model: app.settings.speech_model, hold_to_talk: true,
            });
            let take = Recorder::from_samples_for_test(&[0.25; 96]).finish().unwrap().unwrap();
            app.dictation.transcribe(app.settings.speech_model, take);
            let ctx = egui::Context::default();
            if confirm_first {
                app.recorder = Some(Recorder::from_samples_for_test(&[0.25; 96]));
                app.run_command(&ctx, Command::Quit);
                assert!(app.confirm_quit && !app.quitting);
                assert!(!app.dictation.transcription_cancelling(), "asking to quit must let the user keep working");
                app.quit(&ctx); // the user chooses Discard and quit
            } else {
                app.run_command(&ctx, Command::Quit);
            }
            assert!(app.quitting && app.dictation.transcription_cancelling());
            let settings = serde_json::to_value(&app.settings).unwrap();
            let dirty = app.dirty_since;
            let hidden = app.hidden;
            for command in [
                Command::DictateReleased, Command::Dictate, Command::ToggleMute,
                Command::ToggleBypass, Command::ToggleRunning, Command::ToggleCollapse, Command::Show,
            ] {
                app.commands.0.send(command).unwrap();
            }
            if let Some(recorder) = &app.recorder { recorder.report_error_for_test("capture ended during shutdown"); }
            app.tick(&ctx);
            assert!(app.dictating.is_some(), "a late shortcut release must not submit the unfinished take");
            assert_eq!(app.recorder.is_some(), confirm_first, "normal polling stops once quitting starts");
            assert_eq!(serde_json::to_value(&app.settings).unwrap(), settings);
            assert_eq!(app.dirty_since, dirty);
            assert_eq!(app.hidden, hidden);
            release.send(()).unwrap();
            let deadline = Instant::now() + Duration::from_secs(3);
            while app.dictation.busy() || app.dictation_status.0 != "Transcription cancelled" {
                app.poll_dictation();
                assert!(Instant::now() < deadline, "quitting failed to cancel speech work");
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }

    #[test]
    fn collapsing_to_the_strip_keeps_pending_dictation_running() {
        let (dictation, release) = Dictation::delayed_for_test();
        let mut app = App::stopped(Settings::default());
        app.dictation = dictation;
        let take = Recorder::from_samples_for_test(&[0.25; 96]).finish().unwrap().unwrap();
        app.dictation.transcribe(app.settings.speech_model, take);
        let ctx = egui::Context::default();
        app.set_collapsed(&ctx, true);
        assert!(app.hidden && !app.quitting);
        assert!(app.dictation.busy() && !app.dictation.transcription_cancelling());
        app.run_command(&ctx, Command::Show);
        assert!(!app.hidden && !app.dictation.transcription_cancelling());
        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while app.dictation.busy() || app.dictation_status.0 != "No speech heard" {
            app.poll_dictation();
            assert!(Instant::now() < deadline, "hidden dictation did not finish");
            std::thread::sleep(Duration::from_millis(1));
        }
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
    fn stopping_processing_does_not_replay_a_previous_engines_warning() {
        for announce in [true, false] {
            let mut app = App::stopped(Settings::default());
            app.engine = Some(Engine::for_test());
            app.warn_until = Some(("output buffer underrun".into(), Instant::now()));
            app.stop_engine(announce);
            assert!(app.warn_until.is_none());
            if !announce {
                // Fatal errors supply their own result after stopping streams.
                app.status = ("Processing failed".into(), RED);
            }
            let stopped_status = app.status.clone();
            app.poll_engine();
            assert_eq!(app.status, stopped_status);
            assert!(!app.running());
        }
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
    fn clearing_a_shortcut_while_capturing_restores_the_other_global_bindings() {
        let mut app = App::stopped(Settings::default());
        app.assign_hotkey(&Binding::Mute, Some("Ctrl+Alt+M".into()));
        app.assign_hotkey(&Binding::Dictate, Some("Ctrl+Alt+D".into()));
        app.start_capture(Binding::Dictate);
        assert!(app.hotkey_bindings().is_empty());

        app.assign_hotkey(&Binding::Dictate, None);
        assert!(app.capturing.is_none(), "Clear must end shortcut capture");
        assert!(app.settings.hotkeys.dictate.is_none());
        assert_eq!(app.hotkey_bindings(), vec![("Ctrl+Alt+M".into(), Command::ToggleMute)]);
    }

    #[test]
    fn shortcut_capture_does_not_replay_keys_as_text_navigation() {
        use eframe::App as _;
        use egui::accesskit::{Action, ActionRequest, TreeId};
        for passes in [1, 2] {
            let mut app = App::stopped(Settings::default());
            app.page = Page::Settings;
            app.settings.hotkeys.mute = Some("Ctrl+Alt+M".into());
            let mut expected = serde_json::to_value(&app.settings).unwrap();
            app.start_capture(Binding::Mute);
            let ctx = egui::Context::default();
            ctx.options_mut(|options| options.max_passes = passes.try_into().unwrap());
            ctx.enable_accesskit();
            let mut frame = eframe::Frame::_new_kittest();
            let mut draw = |app: &mut App, events| ctx.run_ui(egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(760.0, 880.0))),
                time: Some(ctx.cumulative_frame_nr() as f64 * 0.2), events, ..Default::default()
            }, |ui| app.ui(ui, &mut frame));
            for _ in 0..3 { draw(&mut app, Vec::new()); }
            let output = draw(&mut app, Vec::new());
            let owner = *output.platform_output.accesskit_update.unwrap().nodes.iter()
                .find(|(_, node)| node.label() == Some("Start processing automatically")).map(|(id, _)| id).unwrap();
            draw(&mut app, vec![egui::Event::AccessKitActionRequest(ActionRequest {
                action: Action::Focus, target_tree: TreeId::ROOT, target_node: owner, data: None,
            })]);
            for _ in 0..8 { draw(&mut app, Vec::new()); }
            let key = |key, pressed, modifiers| egui::Event::Key {
                key, physical_key: Some(key), pressed, repeat: false, modifiers,
            };
            let ctrl = egui::Modifiers { ctrl: true, command: true, ..egui::Modifiers::NONE };
            // Edits deferred before capture started must never become shortcuts.
            ctx.data_mut(|data| data.insert_temp(egui::Id::new("keyboard-input"),
                (None::<egui::Id>, vec![key(egui::Key::M, true, ctrl), egui::Event::Text("m".into())])));
            draw(&mut app, vec![key(egui::Key::Tab, true, egui::Modifiers::NONE)]);
            for _ in 0..3 { draw(&mut app, Vec::new()); }
            assert_eq!(serde_json::to_value(&app.settings).unwrap(), expected);
            assert!(app.capturing.is_some(), "a rejected key keeps capture open");
            assert!(app.dirty_since.is_none());
            // A fresh correction is captured; neither the rejected Tab nor its repeats navigate.
            draw(&mut app, vec![key(egui::Key::Tab, true, egui::Modifiers::NONE),
                key(egui::Key::M, true, ctrl), egui::Event::Text("m".into())]);
            for _ in 0..3 { draw(&mut app, Vec::new()); }
            let output = draw(&mut app, Vec::new());
            expected["hotkeys"]["mute"] = serde_json::json!("Ctrl+M");
            assert_eq!(serde_json::to_value(&app.settings).unwrap(), expected);
            assert_eq!(output.platform_output.accesskit_update.unwrap().focus, owner);
            assert!(app.capturing.is_none() && app.dirty_since.is_some());
            assert_eq!(app.page, Page::Settings);
            assert!(app.pad_filter.is_empty() && app.engine.is_none() && app.dictating.is_none());
            draw(&mut app, vec![key(egui::Key::Tab, false, egui::Modifiers::NONE), key(egui::Key::M, false, ctrl)]);
            app.start_capture(Binding::Mute);
            draw(&mut app, vec![key(egui::Key::F13, true, egui::Modifiers::NONE)]);
            expected["hotkeys"]["mute"] = serde_json::json!("F13");
            assert_eq!(serde_json::to_value(&app.settings).unwrap(), expected);
            assert!(app.capturing.is_none() && app.dirty_since.is_some());
        }
    }

    #[test]
    fn unavailable_global_hotkeys_still_allow_clearing_saved_bindings() {
        use eframe::App as _;
        for (page, binding) in [
            (Page::Settings, Binding::Mute),
            (Page::Settings, Binding::Bypass),
            (Page::Settings, Binding::StopClips),
            (Page::Settings, Binding::Dictate),
            (Page::Dictation, Binding::Dictate),
        ] {
            let mut app = App::stopped(Settings::default());
            app.page = page;
            *app.binding_slot(&binding).unwrap() = Some("Ctrl+Alt+M".into());
            let ctx = egui::Context::default();
            let mut frame = eframe::Frame::_new_kittest();
            let mut draw = |app: &mut App, events| ctx.run_ui(egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(760.0, 880.0))),
                events, ..Default::default()
            }, |ui| app.ui(ui, &mut frame));
            draw(&mut app, Vec::new());
            for label in ["Set…", "Clear"] {
                let output = draw(&mut app, Vec::new());
                let pos = output.shapes.iter().find_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) if text.galley.text() == label => Some(text.visual_bounding_rect().center()),
                    _ => None,
                }).expect("shortcut control is visible");
                for pressed in [true, false] {
                    draw(&mut app, vec![egui::Event::PointerMoved(pos), egui::Event::PointerButton {
                        pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE,
                    }]);
                }
                assert!(app.capturing.is_none(), "unavailable hotkeys must not start capture");
                assert_eq!(app.binding_slot(&binding).unwrap().is_none(), label == "Clear", "{page:?}: {binding:?}");
                assert_eq!(app.dirty_since.is_some(), label == "Clear", "only clearing changes settings");
            }
            assert_eq!(app.hotkey_status, ("Shortcut cleared".into(), MUTED));
        }
    }

    #[test]
    fn leaving_the_full_window_restores_shortcuts_without_changing_the_binding() {
        for (focused, hidden, minimized) in [
            (true, false, false),
            (false, false, false),
            (true, true, false),
            (true, false, true),
        ] {
            let mut app = App::stopped(Settings::default());
            app.assign_hotkey(&Binding::Dictate, Some("Ctrl+Alt+D".into()));
            let saved = app.hotkey_bindings();
            app.start_capture(Binding::Dictate);
            app.hidden = hidden;

            let ctx = egui::Context::default();
            let mut input = egui::RawInput { focused, ..Default::default() };
            input.viewports.get_mut(&egui::ViewportId::ROOT).unwrap().minimized = Some(minimized);
            let _ = ctx.run_ui(input, |ui| app.sync_hotkeys(ui.ctx()));

            let active = focused && !hidden && !minimized;
            assert_eq!(app.capturing.is_some(), active);
            assert_eq!(app.settings.hotkeys.dictate.as_deref(), Some("Ctrl+Alt+D"));
            if active {
                assert!(app.hotkey_bindings().is_empty());
            } else {
                assert_eq!(app.hotkey_bindings(), saved);
            }
        }
    }

    #[test]
    fn changing_pages_cancels_shortcut_capture() {
        use eframe::App as _;
        for destination in [Page::Settings, Page::Voice] {
            let mut app = App::stopped(Settings::default());
            app.page = Page::Settings;
            app.assign_hotkey(&Binding::Dictate, Some("Ctrl+Alt+D".into()));
            app.start_capture(Binding::Dictate);
            let ctx = egui::Context::default();
            let mut frame = eframe::Frame::_new_kittest();
            let mut draw = |app: &mut App, events| ctx.run_ui(egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(760.0, 880.0))),
                events,
                ..Default::default()
            }, |ui| app.ui(ui, &mut frame));
            let output = draw(&mut app, vec![]);
            let pos = output.shapes.iter().find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.text() == destination.title()
                    && text.visual_bounding_rect().right() < SIDEBAR_WIDTH => Some(text.visual_bounding_rect().center()),
                _ => None,
            }).expect("sidebar navigation");
            let button = |pressed| egui::Event::PointerButton {
                pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE,
            };
            for events in [vec![egui::Event::PointerMoved(pos)], vec![button(true)], vec![button(false)]] {
                draw(&mut app, events);
            }
            assert_eq!(app.page, destination);
            assert_eq!(app.capturing.is_some(), destination == Page::Settings,
                "only leaving the current page cancels capture");
            assert_eq!(app.settings.hotkeys.dictate.as_deref(), Some("Ctrl+Alt+D"));
            if destination != Page::Settings {
                assert_eq!(app.hotkey_bindings(), vec![("Ctrl+Alt+D".into(), Command::Dictate)]);
            }
        }
    }

    /// Draw the whole window headlessly (as eframe does) and return every
    /// piece of text with where it landed, plus the screen reader tree.
    fn render(app: &mut App, size: egui::Vec2) -> (Vec<(String, egui::Rect)>, egui::FullOutput) {
        use eframe::App as _;
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let mut frame = eframe::Frame::_new_kittest();
        let input = || egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
            ..Default::default()
        };
        // A few frames so layouts that measure themselves settle.
        let mut output = ctx.run_ui(input(), |ui| app.ui(ui, &mut frame));
        for _ in 0..3 {
            output = ctx.run_ui(input(), |ui| app.ui(ui, &mut frame));
        }
        fn collect(shape: &egui::Shape, clip: egui::Rect, out: &mut Vec<(String, egui::Rect)>) {
            match shape {
                egui::Shape::Text(text) => {
                    let rect = text.visual_bounding_rect().intersect(clip);
                    if rect.is_positive() {
                        out.push((text.galley.text().to_owned(), rect));
                    }
                }
                egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| collect(s, clip, out)),
                _ => {}
            }
        }
        let mut texts = Vec::new();
        for clipped in &output.shapes {
            collect(&clipped.shape, clipped.clip_rect, &mut texts);
        }
        (texts, output)
    }

    fn visible<'a>(texts: &'a [(String, egui::Rect)], needle: &str) -> Option<&'a egui::Rect> {
        texts.iter().find(|(text, _)| text.contains(needle)).map(|(_, rect)| rect)
    }

    #[test]
    fn completed_dictation_does_not_hide_an_active_recording() {
        let mut app = App::stopped(Settings::default());
        app.page = Page::Dictation;
        app.dictating = Some(DictationRecording {
            recorder: Recorder::from_samples_for_test(&[0.25; 96]),
            model: app.settings.speech_model,
            hold_to_talk: false,
        });
        app.dictation_status = ("Typed".into(), GREEN);
        app.last_dictation = "Previous phrase".into();

        let (texts, _) = render(&mut app, egui::vec2(700.0, 420.0));
        let listening = visible(&texts, "Listening…").expect("the microphone is still recording");
        let result = visible(&texts, "Typed").expect("the earlier phrase's result stays visible");
        assert!(listening.bottom() < result.top());
        assert!(visible(&texts, "Discard recording").is_some());
        assert!(visible(&texts, "Previous phrase").is_some());

        app.discard_dictation();
        let (texts, _) = render(&mut app, egui::vec2(700.0, 420.0));
        assert!(visible(&texts, "Listening…").is_none());
        assert!(visible(&texts, "Discard recording").is_none());
        assert!(visible(&texts, "Dictation recording discarded").is_some());
    }

    #[test]
    fn the_cancel_transcription_button_keeps_active_capture_and_previous_words() {
        use eframe::App as _;
        let (dictation, release) = Dictation::delayed_for_test();
        let mut app = App::stopped(Settings::default());
        app.dictation = dictation;
        app.page = Page::Dictation;
        app.last_dictation = "Previous transcript".into();
        app.dictating = Some(DictationRecording {
            recorder: Recorder::from_samples_for_test(&[0.25; 96]),
            model: app.settings.speech_model, hold_to_talk: false,
        });
        let take = Recorder::from_samples_for_test(&[0.25; 96]).finish().unwrap().unwrap();
        app.dictation.transcribe(app.settings.speech_model, take);
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let mut frame = eframe::Frame::_new_kittest();
        let mut draw = |app: &mut App, events| ctx.run_ui(egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(760.0, 650.0))),
            events, ..Default::default()
        }, |ui| app.ui(ui, &mut frame));
        draw(&mut app, Vec::new());
        let output = draw(&mut app, Vec::new());
        let pos = output.shapes.iter().find_map(|shape| match &shape.shape {
            egui::Shape::Text(text) if text.galley.text() == "Cancel transcription" => Some(text.visual_bounding_rect().center()),
            _ => None,
        }).expect("cancellation must be visible while transcribing");
        let tree = output.platform_output.accesskit_update.unwrap();
        assert!(tree.nodes.iter().any(|(_, node)| node.label() == Some("Cancel transcription")));
        for pressed in [true, false] {
            draw(&mut app, vec![egui::Event::PointerMoved(pos), egui::Event::PointerButton {
                pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE,
            }]);
        }
        assert!(app.dictation.transcription_cancelling());
        assert!(app.dictating.is_some(), "cancelling completed takes must not discard a new recording");
        assert_eq!(app.last_dictation, "Previous transcript");
        let (texts, _) = render(&mut app, egui::vec2(760.0, 650.0));
        assert!(visible(&texts, "Cancelling transcription…").is_some());
        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while app.dictation.busy() || app.dictation_status.0 != "Transcription cancelled" {
            app.poll_dictation();
            assert!(Instant::now() < deadline, "cancellation did not finish");
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(app.last_dictation, "Previous transcript");
        assert!(app.dictating.is_some());
        let (texts, _) = render(&mut app, egui::vec2(760.0, 650.0));
        assert!(visible(&texts, "Cancel transcription").is_none());
        assert!(visible(&texts, "Transcription cancelled").is_some());
        assert!(visible(&texts, "Listening…").is_some());
    }

    #[test]
    #[ignore = "writes transcription cancellation screenshots; needs Python with Pillow"]
    fn snapshot_transcription_cancellation() {
        for (dark, theme) in [(true, "dark"), (false, "light")] {
            let (dictation, _release) = Dictation::delayed_for_test();
            let mut app = App::stopped(Settings::default());
            app.dictation = dictation;
            app.page = Page::Dictation;
            app.last_dictation = "Previous transcript remains available to copy.".into();
            let take = Recorder::from_samples_for_test(&[0.25; 96]).finish().unwrap().unwrap();
            app.dictation.transcribe(app.settings.speech_model, take);
            for cancelling in [false, true] {
                if cancelling { app.dictation.cancel_transcription(); }
                snapshot::snapshot(&mut app, &format!("dictation-{}-{theme}", if cancelling { "cancelling" } else { "transcribing" }), egui::vec2(700.0, 420.0), 1.0, dark);
            }
        }
    }

    #[test]
    #[ignore = "writes dictation finalization screenshots; needs Python with Pillow"]
    fn snapshot_dictation_finalization() {
        for (dark, theme) in [(true, "dark"), (false, "light")] {
            let (recorder, finish) = Recorder::delayed_writer_for_test(&[0.25; 96]);
            let mut app = App::stopped(Settings::default());
            app.page = Page::Dictation;
            app.last_dictation = "Previous transcript remains available to copy.".into();
            app.dictating = Some(DictationRecording {
                recorder, model: app.settings.speech_model, hold_to_talk: true,
            });
            app.stop_dictation();
            for discarded in [false, true] {
                if discarded { app.discard_dictation(); }
                snapshot::snapshot(&mut app, &format!("dictation-{}-{theme}", if discarded { "discarded" } else { "finishing" }), egui::vec2(700.0, 420.0), 1.0, dark);
            }
            finish.send(()).unwrap();
        }
    }

    /// A 1080p laptop at 150% has about 690 points of height: every page
    /// must keep the sidebar's navigation and status panel on screen, at
    /// that size and at the natural one.
    #[test]
    fn every_page_keeps_the_sidebar_and_status_on_screen() {
        for size in [egui::vec2(700.0, 420.0), egui::vec2(760.0, 880.0)] {
            for page in Page::ALL {
                let mut app = App::stopped(Settings::default());
                app.page = page;
                let (texts, _) = render(&mut app, size);
                let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, size);
                for needle in ["OpenMic", "Voice", "Settings", "• Stopped", "Start", "Mute"] {
                    let rect = visible(&texts, needle).unwrap_or_else(|| panic!("{needle} missing on {page:?} at {size:?}"));
                    assert!(screen.contains_rect(*rect), "{needle} off screen on {page:?} at {size:?}: {rect:?}");
                    assert!(rect.right() < SIDEBAR_WIDTH + 4.0 || needle == "Voice", "{needle} is in the sidebar: {rect:?}");
                }
                let start = visible(&texts, "Start").unwrap();
                assert!(start.bottom() > size.y - 40.0, "the status panel sits at the bottom: {start:?}");
                let title = texts.iter().filter(|(t, _)| t == page.title()).map(|(_, r)| r).find(|r| r.left() > SIDEBAR_WIDTH);
                assert!(title.is_some(), "{page:?} shows its title beside the sidebar");
            }
        }
    }

    #[test]
    fn the_window_reopens_where_it_was_and_remembers_moves() {
        use eframe::App as _;
        let mut app = App::stopped(Settings { window_position: Some([3900, 150]), ..Settings::default() });
        let ctx = egui::Context::default();
        let mut frame = eframe::Frame::_new_kittest();
        // eframe reports the window at 150% scaling, where Windows first put it.
        let mut frame_at = |app: &mut App, min: egui::Pos2| {
            let info = egui::ViewportInfo {
                native_pixels_per_point: Some(1.5),
                outer_rect: Some(egui::Rect::from_min_size(min, egui::vec2(760.0, 880.0))),
                inner_rect: Some(egui::Rect::from_min_size(min + egui::vec2(8.0, 30.0), egui::vec2(744.0, 842.0))),
                ..Default::default()
            };
            let mut input = egui::RawInput::default();
            input.viewports.insert(egui::ViewportId::ROOT, info);
            ctx.run_ui(input, |ui| app.logic(ui.ctx(), &mut frame)).viewport_output
        };
        let first = frame_at(&mut app, egui::pos2(100.0, 100.0));
        let moved: Vec<_> = first.values().flat_map(|v| v.commands.clone()).collect();
        if on_a_monitor([3900, 150]) {
            assert!(moved.contains(&ViewportCommand::OuterPosition(egui::pos2(2600.0, 100.0))), "{moved:?}");
        } else {
            let placed = moved.iter().any(|c| matches!(c, ViewportCommand::OuterPosition(_)));
            assert!(!placed, "a position on a monitor that's gone is ignored: {moved:?}");
        }
        for _ in 0..6 {
            frame_at(&mut app, egui::pos2(400.0, 300.0));
        }
        assert_eq!(app.settings.window_position, Some([600, 450]), "saved in screen pixels");
        assert_eq!(app.settings.window_size, Some([744.0, 842.0]), "and the size in points");
    }

    #[test]
    fn a_saved_position_off_every_monitor_is_ignored() {
        use eframe::App as _;
        // Windows always has a monitor list. Linux only does on X11; Wayland
        // and a missing server keep the saved position, which this test skips.
        #[cfg(windows)]
        assert!(!on_a_monitor([-100_000, -100_000]), "Windows reports connected monitors");
        if on_a_monitor([-100_000, -100_000]) {
            return;
        }
        let mut app = App::stopped(Settings { window_position: Some([-100_000, -100_000]), ..Settings::default() });
        let ctx = egui::Context::default();
        let mut frame = eframe::Frame::_new_kittest();
        let info = egui::ViewportInfo {
            native_pixels_per_point: Some(1.0),
            outer_rect: Some(egui::Rect::from_min_size(egui::pos2(50.0, 50.0), egui::vec2(760.0, 880.0))),
            inner_rect: Some(egui::Rect::from_min_size(egui::pos2(58.0, 80.0), egui::vec2(744.0, 842.0))),
            ..Default::default()
        };
        let mut input = egui::RawInput::default();
        input.viewports.insert(egui::ViewportId::ROOT, info);
        let output = ctx.run_ui(input, |ui| app.logic(ui.ctx(), &mut frame));
        let placed = output
            .viewport_output
            .values()
            .flat_map(|v| &v.commands)
            .any(|c| matches!(c, ViewportCommand::OuterPosition(_)));
        assert!(!placed, "an off-screen saved position is not applied");
    }

    /// egui's built-in fonts lack many symbols (check marks, arrows, most
    /// emoji), which then draw as empty boxes. Every non-ASCII character in
    /// the UI code must be one the fonts can draw.
    #[test]
    fn every_symbol_in_the_ui_has_a_glyph() {
        let sources = [
            ("gui/clips.rs", include_str!("clips.rs")),
            ("gui/mod.rs", include_str!("mod.rs")),
            ("gui/mic_tab.rs", include_str!("mic_tab.rs")),
            ("gui/mic_test.rs", include_str!("mic_test.rs")),
            ("gui/overlay.rs", include_str!("overlay.rs")),
            ("gui/settings_tab.rs", include_str!("settings_tab.rs")),
            ("gui/sound_tab.rs", include_str!("sound_tab.rs")),
            ("gui/startup.rs", include_str!("startup.rs")),
            ("gui/updates.rs", include_str!("updates.rs")),
            ("gui/routes.rs", include_str!("routes.rs")),
            ("widgets.rs", include_str!("../widgets.rs")),
            ("viz.rs", include_str!("../viz.rs")),
            ("tray.rs", include_str!("../tray.rs")),
            // Labels the UI shows for speech models.
            ("dictation.rs", &include_str!("../dictation.rs")[..include_str!("../dictation.rs").find("// ---- download").unwrap()]),
        ];
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |_| {});
        let mut missing = Vec::new();
        for (file, source) in sources {
            for (n, line) in source.lines().enumerate() {
                let code = line.trim_start();
                if code.starts_with("//") {
                    continue;
                }
                for ch in code.chars().filter(|c| !c.is_ascii()) {
                    let drawn = ctx.fonts_mut(|f| f.has_glyph(&egui::FontId::proportional(14.0), ch));
                    if !drawn {
                        missing.push(format!("{file}:{} {ch} (U+{:04X})", n + 1, ch as u32));
                    }
                }
            }
        }
        assert!(missing.is_empty(), "symbols the fonts can't draw:\n{}", missing.join("\n"));
    }

    #[test]
    fn minimizing_collapses_to_the_strip_once() {
        use eframe::App as _;
        let mut app = App::stopped(Settings::default());
        let ctx = egui::Context::default();
        let mut frame = eframe::Frame::_new_kittest();
        let mut frame_with = |app: &mut App, minimized: bool| {
            let info = egui::ViewportInfo { minimized: Some(minimized), ..Default::default() };
            let mut input = egui::RawInput::default();
            input.viewports.insert(egui::ViewportId::ROOT, info);
            let output = ctx.run_ui(input, |ui| app.logic(ui.ctx(), &mut frame));
            output.viewport_output.into_values().flat_map(|v| v.commands).collect::<Vec<_>>()
        };
        frame_with(&mut app, false);
        let commands = frame_with(&mut app, true);
        assert!(app.settings.collapsed && app.hidden, "minimize collapsed it");
        assert!(commands.contains(&ViewportCommand::Visible(false)));

        // Expanding restores the window; a stale "minimized" doesn't re-collapse.
        app.commands.0.send(Command::Show).unwrap();
        let commands = frame_with(&mut app, true);
        assert!(commands.contains(&ViewportCommand::Minimized(false)), "{commands:?}");
        assert!(!app.settings.collapsed && !app.hidden);
        frame_with(&mut app, true);
        assert!(!app.settings.collapsed, "no flip-flop while Windows catches up");
        frame_with(&mut app, false);
        frame_with(&mut app, true);
        assert!(app.settings.collapsed, "minimizing again collapses again");

        // Switched off: minimize is just minimize.
        app.commands.0.send(Command::Show).unwrap();
        frame_with(&mut app, false);
        app.settings.minimize_to_strip = false;
        frame_with(&mut app, true);
        assert!(!app.settings.collapsed);
    }

    /// Every setting is a row (name, a line of explanation, its picture and
    /// its value), with presets as chips above; nothing needs a mode switch.
    #[test]
    fn the_voice_page_shows_every_setting_as_a_row() {
        for size in [egui::vec2(700.0, 1400.0), egui::vec2(760.0, 1400.0)] {
            let mut app = App::stopped(Settings::default());
            let (texts, _) = render(&mut app, size);
            let rows = [
                ("Your mic", "How loud you are going in"),
                ("Remove rumble", "Cuts low thumps and hum"),
                ("Remove noise", "Keeps your voice, drops the room"),
                ("Only my voice", "Silent when you're not talking"),
                ("Quiet cut-off", "Mutes anything below a level"),
                ("To Discord", "How loud Discord hears you"),
                ("Hear yourself", "What Discord hears, in your headphones"),
            ];
            let mut last_top = 0.0;
            for (title, explain) in rows {
                let t = *visible(&texts, title).unwrap_or_else(|| panic!("{title} missing at {size:?}"));
                let e = *visible(&texts, explain).unwrap_or_else(|| panic!("{title}'s explanation missing"));
                assert!(t.top() > last_top, "{title} is below the row before it");
                assert!(e.top() > t.top() && e.top() - t.bottom() < 12.0, "{title}'s explanation sits under it");
                assert!(e.right() <= size.x, "{title} fits at {size:?}");
                last_top = t.top();
            }
            for chip in ["Balanced", "Mechanical keyboard", "Quiet room", "Noisy room", "Low CPU", "Bypass", "Test my mic", "DeepFilterNet"] {
                assert!(visible(&texts, chip).is_some(), "{chip} missing at {size:?}");
            }
            for gone in ["Simple", "Advanced", "LIVE SCOPE", "PROCESSING", "Noise reduction"] {
                assert!(visible(&texts, gone).is_none(), "{gone} should be gone");
            }
        }
    }

    #[test]
    fn preset_chips_apply_a_preset_in_one_click() {
        use eframe::App as _;
        let mut app = App::stopped(Settings::default());
        let ctx = egui::Context::default();
        let mut frame = eframe::Frame::_new_kittest();
        let size = egui::vec2(760.0, 1400.0);
        let mut run = |app: &mut App, events: Vec<egui::Event>| {
            let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)), events, ..Default::default() };
            ctx.run_ui(input, |ui| app.ui(ui, &mut frame))
        };
        let output = run(&mut app, vec![]);
        let chip = output
            .shapes
            .iter()
            .find_map(|c| match &c.shape {
                egui::Shape::Text(t) if t.galley.text() == "Noisy room" => Some(t.visual_bounding_rect().center()),
                _ => None,
            })
            .expect("Noisy room chip");
        let button = |pressed| egui::Event::PointerButton { pos: chip, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        for events in [vec![egui::Event::PointerMoved(chip)], vec![button(true)], vec![button(false)], vec![]] {
            run(&mut app, events);
        }
        let noisy = crate::config::builtin_presets().into_iter().find(|p| p.name == "Noisy room").unwrap();
        assert_eq!(app.settings.processing(), noisy.processing);
        assert!(app.settings.gate, "Noisy room turns the quiet cut-off on");
    }

    #[test]
    fn the_checklist_ticks_itself_off_and_can_be_hidden() {
        let mut app = App::stopped(Settings::default());
        let (texts, _) = render(&mut app, egui::vec2(760.0, 1400.0));
        assert!(visible(&texts, "GET SET UP").is_some());
        assert!(visible(&texts, "0 of 4").is_some(), "nothing is set up yet");

        app.outputs = vec!["CABLE Input (VB-Audio Virtual Cable)".into()];
        app.inputs = vec!["Yeti".into()];
        app.settings.microphone = "Yeti".into();
        let (texts, _) = render(&mut app, egui::vec2(760.0, 1400.0));
        assert!(visible(&texts, "2 of 4").is_some(), "VB-Cable and the mic are seen");
        assert!(visible(&texts, "Input Device to Default").is_some());

        app.settings.setup.discord = true;
        app.settings.setup.tested = true;
        assert!(!app.show_checklist(), "all done: it goes away");
        let (texts, _) = render(&mut app, egui::vec2(760.0, 1400.0));
        assert!(visible(&texts, "GET SET UP").is_none());

        app.settings.setup.tested = false;
        app.settings.setup.dismissed = true;
        assert!(!app.show_checklist(), "hidden by the user");
    }

    #[test]
    fn routing_is_on_the_voice_page_above_the_settings() {
        let mut app = App::stopped(Settings { microphone: "Yeti".into(), output: "CABLE Input".into(), ..Settings::default() });
        let (texts, _) = render(&mut app, egui::vec2(760.0, 1400.0));
        let routing = *visible(&texts, "ROUTING").expect("routing on the Voice page");
        let rows = *visible(&texts, "YOUR VOICE, STEP BY STEP").unwrap();
        assert!(routing.top() < rows.top(), "devices first, then what happens to your voice");
        for label in ["Microphone", "Processed output", "Monitor output"] {
            assert!(visible(&texts, label).is_some(), "{label} chooser missing");
        }
        assert!(visible(&texts, "Hear yourself").is_some(), "the monitor is a Voice control");

        app.page = Page::Settings;
        let (texts, _) = render(&mut app, egui::vec2(760.0, 1400.0));
        assert!(visible(&texts, "ROUTING").is_none() && visible(&texts, "HOTKEYS").is_some());
        assert!(visible(&texts, "Always show the strip").is_some(), "strip visibility is a Settings option");
        assert!(visible(&texts, "SPEECH TO TEXT").is_none(), "dictation has its own page");
        app.page = Page::Dictation;
        let (texts, _) = render(&mut app, egui::vec2(760.0, 1400.0));
        assert!(visible(&texts, "SPEECH TO TEXT").is_some() && visible(&texts, "Shortcut").is_some());
        app.page = Page::Record;
        let (texts, _) = render(&mut app, egui::vec2(760.0, 1400.0));
        assert!(visible(&texts, "RECORD A CLIP").is_some());
        app.page = Page::Soundboard;
        let (texts, _) = render(&mut app, egui::vec2(760.0, 1400.0));
        assert!(visible(&texts, "RECORD A CLIP").is_none(), "recording has its own page");
    }

    #[test]
    fn update_states_show_in_the_header() {
        use crate::update::{Release, Status};
        let cases = [
            (Status::Available(Release::sample("v9.0.0")), "v9.0.0 available"),
            (Status::Downloading(Release::sample("v9.0.0")), "/ 54 MB"),
            (Status::Ready(Release::sample("v9.0.0")), "Restart to update"),
            (Status::Checking, "Checking for updates"),
        ];
        for (status, needle) in cases {
            let mut app = App::stopped(Settings::default());
            app.updater.set_status_for_test(status);
            let (texts, _) = render(&mut app, egui::vec2(700.0, 420.0));
            let rect = visible(&texts, needle).unwrap_or_else(|| panic!("{needle} not shown"));
            assert!(rect.top() < 60.0, "{needle} belongs in the title row: {rect:?}");
            assert!(visible(&texts, "Clean voice").is_none(), "the tagline gives way to {needle}");
        }
    }

    #[test]
    fn manual_update_results_get_their_display_time_after_checking_finishes() {
        use crate::update::Status;
        for (status, message) in [
            (Status::UpToDate, "is up to date"),
            (Status::Failed("Connection timed out".into()), "Couldn't check for updates"),
        ] {
            let mut app = App::stopped(Settings::default());
            app.manual_update_check = true;
            app.updater.set_status_for_test(Status::Checking);
            app.poll_updates();
            assert!(app.manual_update_check && app.update_checked_at.is_none(),
                "the result timer must not run during a slow check");

            let completed = Instant::now();
            app.updater.set_status_for_test(status.clone());
            app.poll_updates();
            assert!(!app.manual_update_check);
            let shown_at = app.update_checked_at.unwrap();
            assert!(shown_at >= completed);
            let (texts, _) = render(&mut app, egui::vec2(700.0, 420.0));
            assert!(visible(&texts, message).is_some(), "{message} must be shown when the result arrives");
            app.poll_updates();
            assert_eq!(app.update_checked_at, Some(shown_at), "polling must not extend the result forever");

            let expired = Instant::now() - Duration::from_secs(60);
            app.update_checked_at = Some(expired);
            let (texts, _) = render(&mut app, egui::vec2(700.0, 420.0));
            assert!(visible(&texts, message).is_none());

            app.updater.set_status_for_test(Status::Checking);
            app.poll_updates();
            app.updater.set_status_for_test(status);
            app.poll_updates();
            assert_eq!(app.update_checked_at, Some(expired), "an automatic check must not restart the manual timer");
            let (texts, _) = render(&mut app, egui::vec2(700.0, 420.0));
            assert!(visible(&texts, message).is_none(), "automatic check results stay quiet");
        }
    }

    #[test]
    fn failed_update_download_keeps_a_visible_retry_after_the_check_timer_expires() {
        use crate::update::{Release, Status};
        for checked_at in [None, Some(Instant::now() - Duration::from_secs(60))] {
            let mut app = App::stopped(Settings::default());
            app.page = Page::Soundboard;
            app.update_checked_at = checked_at;
            app.updater.set_status_for_test(Status::DownloadFailed {
                release: Release::sample("v9.0.0"), error: "Connection interrupted".into(),
            });
            let (texts, _) = render(&mut app, egui::vec2(700.0, 420.0));
            let error = visible(&texts, "Update download failed").expect("the failure must stay visible");
            let retry = visible(&texts, "Retry update").expect("the failed release can be retried directly");
            let title = &texts.iter().find(|(text, rect)| text == "Soundboard" && rect.left() >= SIDEBAR_WIDTH).unwrap().1;
            assert!(error.top() < 60.0 && retry.top() < 60.0);
            assert!(title.right() < error.left() && error.right() < retry.left(), "header controls overlap");
            assert!(visible(&texts, "Couldn't check for updates").is_none());
            assert!(visible(&texts, "Clean voice").is_none());
        }
    }

    #[test]
    #[ignore]
    fn snapshot_failed_update_download() {
        for (dark, theme) in [(true, "dark"), (false, "light")] {
            let mut app = App::stopped(Settings::default());
            app.page = Page::Soundboard;
            app.updater.set_status_for_test(crate::update::Status::DownloadFailed {
                release: crate::update::Release::sample("v9.0.0"), error: "Connection interrupted".into(),
            });
            snapshot::snapshot(&mut app, &format!("update-download-failed-{theme}"), egui::vec2(700.0, 420.0), 1.0, dark);
        }
    }

    #[test]
    fn release_notes_open_inside_the_app() {
        use crate::update::{Release, Status};
        for status in [
            Status::Available(Release::sample("v9.0.0")),
            Status::DownloadFailed { release: Release::sample("v9.0.0"), error: "Connection interrupted".into() },
        ] {
            let mut app = App::stopped(Settings::default());
            app.updater.set_status_for_test(status);
            app.show_release_notes = true;
            let (texts, _) = render(&mut app, egui::vec2(760.0, 880.0));
            assert!(visible(&texts, "What's new in v9.0.0").is_some());
            assert!(visible(&texts, "Fixes").is_some());
            assert!(visible(&texts, "Faster updates, see the docs").is_some(), "inline Markdown is plain text");
        }
    }

    #[test]
    fn screen_readers_can_name_the_controls() {
        let mut app = App::stopped(Settings::default());
        let (_, output) = render(&mut app, egui::vec2(760.0, 880.0));
        let tree = output.platform_output.accesskit_update.expect("accessibility tree");
        let labels: Vec<String> = tree.nodes.iter().filter_map(|(_, node)| node.label().map(str::to_owned)).collect();
        for control in ["Start", "Mute", "Voice", "Soundboard", "Record", "Dictation", "Settings"] {
            assert!(labels.iter().any(|l| l.contains(control)), "{control} has no accessible name: {labels:?}");
        }
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
