use std::{fs, io::Write, path::{Path, PathBuf}};

use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use crate::default_mic::SavedDefaults;
use crate::dictation::SpeechModel;
use crate::dsp::{Model, HIGHPASS_HZ, VOICE_THRESHOLD};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// A failed startup load protects the original file until a successful retry.
    /// This is runtime state, never part of the saved preferences.
    #[serde(skip)]
    pub load_error: Option<SettingsLoadError>,
    pub microphone: String,
    pub output: String,
    pub monitor_output: String,
    pub model: Model,
    pub strength: f32,
    pub input_gain_db: f32,
    pub output_gain_db: f32,
    pub gate: bool,
    pub gate_threshold_db: f32,
    pub highpass: bool,
    pub highpass_hz: f32,
    pub voice_gate: bool,
    pub voice_threshold: f32,
    pub bypass: bool,
    pub mute: bool,
    pub monitor: bool,
    pub monitor_volume: f32,
    pub sound_volume: f32,
    /// Soundboard pads, in board order.
    pub sounds: Vec<Pad>,
    /// Let pads play over each other instead of replacing the current clip.
    pub overlap_clips: bool,
    pub start_with_windows: bool,
    pub auto_start: bool,
    /// Closing the window hides it to the notification area; processing,
    /// hotkeys and the tray menu keep working.
    pub close_to_tray: bool,
    pub hotkeys: Hotkeys,
    /// Processing presets the user saved (built-ins come from [`builtin_presets`]).
    pub presets: Vec<Preset>,
    /// Make VB-Cable's "CABLE Output" the Windows default microphone while
    /// processing, so Discord left on "Default" hears the cleaned voice.
    pub default_mic: bool,
    /// Windows' default microphones before OpenMic changed them; restored
    /// when processing stops, including on the next launch after a crash.
    pub saved_default_mic: Option<SavedDefaults>,
    /// The Whisper model dictation uses.
    pub speech_model: SpeechModel,
    /// Dictate while the shortcut is held; otherwise it starts and stops.
    pub dictation_hold: bool,
    /// Look for a new OpenMic release on GitHub once a day.
    pub check_for_updates: bool,
    /// OpenMic is collapsed to the strip (the main window hidden); it
    /// reopens that way.
    pub collapsed: bool,
    /// The window's minimize button collapses to the strip.
    pub minimize_to_strip: bool,
    /// Keep the strip visible alongside the full window.
    pub always_show_strip: bool,
    /// Where the pinned meter was last dragged to, in screen pixels (the
    /// same on monitors with different scaling).
    pub overlay_position: Option<[i32; 2]>,
    /// The strip's width in points if resized by hand (`None`: fit what
    /// it shows). At its narrowest, just the microphone.
    pub strip_width: Option<f32>,
    /// First-run checklist progress.
    pub setup: Setup,
    /// Where the main window was, in screen pixels, and its size in points.
    pub window_position: Option<[i32; 2]>,
    pub window_size: Option<[f32; 2]>,
}

impl Default for Settings {
    fn default() -> Self {
        let p = Processing::default();
        Self {
            load_error: None,
            microphone: String::new(),
            output: String::new(),
            monitor_output: String::new(),
            model: p.model,
            strength: p.strength,
            input_gain_db: p.input_gain_db,
            output_gain_db: p.output_gain_db,
            gate: p.gate,
            gate_threshold_db: p.gate_threshold_db,
            highpass: p.highpass,
            highpass_hz: p.highpass_hz,
            voice_gate: p.voice_gate,
            voice_threshold: p.voice_threshold,
            bypass: false,
            mute: false,
            monitor: false,
            monitor_volume: 1.0,
            sound_volume: 0.8,
            sounds: Vec::new(),
            overlap_clips: false,
            start_with_windows: false,
            auto_start: true,
            close_to_tray: true,
            hotkeys: Hotkeys::default(),
            presets: Vec::new(),
            default_mic: true,
            saved_default_mic: None,
            speech_model: SpeechModel::default(),
            dictation_hold: true,
            check_for_updates: true,
            collapsed: false,
            minimize_to_strip: true,
            always_show_strip: true,
            overlay_position: None,
            strip_width: None,
            setup: Setup::default(),
            window_position: None,
            window_size: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct SettingsLoadError {
    pub path: PathBuf,
    pub message: String,
}

/// First-run checklist steps OpenMic can't see for itself, and whether the
/// checklist was put away.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Setup {
    /// The user set Discord's input device as the checklist said.
    pub discord: bool,
    /// A mic test was played back.
    pub tested: bool,
    pub dismissed: bool,
}

/// One soundboard pad. Older settings files stored a bare path per pad.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(from = "PadRepr")]
pub struct Pad {
    pub path: PathBuf,
    /// A display name; the audio file's stem is used when unset.
    pub name: Option<String>,
    /// The pad's own volume (0..1), on top of the soundboard volume.
    pub volume: f32,
    /// Global shortcut that plays the pad, e.g. "Ctrl+Alt+1".
    pub hotkey: Option<String>,
    /// Trim: where the pad starts playing, in seconds...
    pub start: f32,
    /// ...and where it stops (the clip's end when `None`).
    pub end: Option<f32>,
    /// Shown on the collapsed strip.
    pub starred: bool,
}

impl Pad {
    pub fn new(path: PathBuf) -> Self {
        Self { path, name: None, volume: 1.0, hotkey: None, start: 0.0, end: None, starred: false }
    }

    pub fn display_name(&self) -> String {
        self.name.as_deref().filter(|name| !name.trim().is_empty()).map_or_else(
            || self.path.file_stem().unwrap_or(self.path.as_os_str()).to_string_lossy().into_owned(),
            str::to_owned,
        )
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum PadRepr {
    Path(PathBuf),
    Full {
        path: PathBuf,
        #[serde(default)]
        name: Option<String>,
        #[serde(default = "full_volume")]
        volume: f32,
        #[serde(default)]
        hotkey: Option<String>,
        #[serde(default)]
        start: f32,
        #[serde(default)]
        end: Option<f32>,
        #[serde(default)]
        starred: bool,
    },
}

fn full_volume() -> f32 {
    1.0
}

impl From<PadRepr> for Pad {
    fn from(repr: PadRepr) -> Self {
        match repr {
            PadRepr::Path(path) => Pad::new(path),
            PadRepr::Full { path, name, volume, hotkey, start, end, starred } => {
                Pad { path, name, volume, hotkey, start, end, starred }
            }
        }
    }
}

/// Global shortcuts for the voice controls (pad shortcuts live on each [`Pad`]).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Hotkeys {
    pub mute: Option<String>,
    pub bypass: Option<String>,
    pub stop_clips: Option<String>,
    /// Speech to text into the focused app.
    pub dictate: Option<String>,
}

/// The settings a processing preset captures.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Processing {
    pub model: Model,
    pub strength: f32,
    pub input_gain_db: f32,
    pub output_gain_db: f32,
    pub gate: bool,
    pub gate_threshold_db: f32,
    pub highpass: bool,
    pub highpass_hz: f32,
    pub voice_gate: bool,
    pub voice_threshold: f32,
}

impl Default for Processing {
    fn default() -> Self {
        Self {
            model: Model::default(),
            strength: 1.0,
            input_gain_db: 0.0,
            output_gain_db: 0.0,
            gate: false,
            gate_threshold_db: -50.0,
            highpass: true,
            highpass_hz: HIGHPASS_HZ,
            voice_gate: true,
            voice_threshold: VOICE_THRESHOLD,
        }
    }
}

impl Processing {
    fn validate(&self) -> Result<()> {
        validate_finite(&[
            ("strength", self.strength),
            ("input_gain_db", self.input_gain_db),
            ("output_gain_db", self.output_gain_db),
            ("gate_threshold_db", self.gate_threshold_db),
            ("highpass_hz", self.highpass_hz),
            ("voice_threshold", self.voice_threshold),
        ])
    }
}

fn validate_finite(values: &[(&str, f32)]) -> Result<()> {
    for &(name, value) in values {
        anyhow::ensure!(value.is_finite(), "{name} must be a finite number");
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Preset {
    pub name: String,
    pub processing: Processing,
}

/// Starting points for common setups. Gains are left at 0 dB: they depend
/// on the microphone, not the room.
pub fn builtin_presets() -> Vec<Preset> {
    let base = Processing::default();
    vec![
        Preset { name: "Balanced".into(), processing: base.clone() },
        Preset {
            name: "Mechanical keyboard".into(),
            processing: Processing { voice_threshold: 0.75, highpass_hz: 100.0, ..base.clone() },
        },
        Preset {
            name: "Quiet room".into(),
            processing: Processing { strength: 0.7, voice_gate: false, ..base.clone() },
        },
        Preset {
            name: "Noisy room".into(),
            processing: Processing {
                voice_threshold: 0.7,
                highpass_hz: 120.0,
                gate: true,
                gate_threshold_db: -45.0,
                ..base.clone()
            },
        },
        Preset {
            name: "Low CPU".into(),
            processing: Processing { model: Model::Rnnoise, ..base },
        },
    ]
}

impl Settings {
    pub fn processing(&self) -> Processing {
        Processing {
            model: self.model,
            strength: self.strength,
            input_gain_db: self.input_gain_db,
            output_gain_db: self.output_gain_db,
            gate: self.gate,
            gate_threshold_db: self.gate_threshold_db,
            highpass: self.highpass,
            highpass_hz: self.highpass_hz,
            voice_gate: self.voice_gate,
            voice_threshold: self.voice_threshold,
        }
    }

    pub fn apply_processing(&mut self, p: &Processing) {
        self.model = p.model;
        self.strength = p.strength;
        self.input_gain_db = p.input_gain_db;
        self.output_gain_db = p.output_gain_db;
        self.gate = p.gate;
        self.gate_threshold_db = p.gate_threshold_db;
        self.highpass = p.highpass;
        self.highpass_hz = p.highpass_hz;
        self.voice_gate = p.voice_gate;
        self.voice_threshold = p.voice_threshold;
    }
}

impl Settings {
    /// JSON numbers can overflow f32 when read. Reject those values before
    /// use or save: serde_json would otherwise silently write them as null.
    fn validate(&self) -> Result<()> {
        self.processing().validate()?;
        validate_finite(&[("monitor_volume", self.monitor_volume), ("sound_volume", self.sound_volume)])?;
        for pad in &self.sounds {
            validate_finite(&[("volume", pad.volume), ("start", pad.start), ("end", pad.end.unwrap_or_default())])
                .with_context(|| format!("soundboard pad '{}'", pad.display_name()))?;
        }
        for preset in &self.presets {
            preset.processing.validate().with_context(|| format!("preset '{}'", preset.name))?;
        }
        if let Some(width) = self.strip_width { validate_finite(&[("strip_width", width)])?; }
        if let Some([width, height]) = self.window_size {
            validate_finite(&[("window width", width), ("window height", height)])?;
        }
        Ok(())
    }

    pub fn load() -> Self {
        Self::load_from(&settings_path(), &legacy_settings_paths())
    }

    pub fn retry_load(&self) -> Self {
        self.retry_from(&settings_path())
    }

    fn retry_from(&self, path: &Path) -> Self {
        // Retry the failed source, not unrelated older copies. Moving the bad
        // file aside must allow a fresh start instead of reviving stale settings.
        let sources: Vec<_> = self.load_error.iter().map(|error| error.path.clone()).collect();
        Self::load_from(path, &sources)
    }

    fn load_from(path: &Path, legacy: &[PathBuf]) -> Self {
        for source in std::iter::once(path).chain(legacy.iter().map(PathBuf::as_path)) {
            let result = match fs::read(source) {
                Ok(bytes) => serde_json::from_slice::<Self>(&bytes).context("Parse settings").and_then(|settings| {
                    settings.validate().context("Validate settings")?;
                    Ok(settings)
                }),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => Err(anyhow::Error::new(error).context("Read settings")),
            };
            match result {
                Ok(settings) => {
                    if source != path && let Err(error) = settings.save_to(path) {
                        crate::logfile::warn(format_args!("could not migrate settings to {}: {error:#}", path.display()));
                    }
                    return settings;
                }
                Err(error) => {
                    // Legacy sources can be a bare filename. Keep their full
                    // location so the recovery screen can open the right folder.
                    let source = std::path::absolute(source).unwrap_or_else(|_| source.to_owned());
                    crate::logfile::error(format_args!("settings not loaded from {}: {error:#}", source.display()));
                    // Do not replace an unreadable file with defaults or an older
                    // legacy copy. This guard also blocks shutdown/autosave writes.
                    return Self {
                        load_error: Some(SettingsLoadError { path: source, message: format!("{error:#}") }),
                        ..Self::default()
                    };
                }
            }
        }
        Self::default()
    }

    pub fn save(&self) -> Result<()> {
        self.save_to(&settings_path())
    }

    fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(error) = &self.load_error {
            anyhow::bail!("Settings were not loaded from {}; retry loading before saving", error.path.display());
        }
        self.validate().context("Validate settings")?;
        let text = serde_json::to_string_pretty(self).context("serialize settings")?;
        let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
        fs::create_dir_all(parent).context("create OpenMic settings directory")?;
        // Write beside the destination so replacement is atomic. A failed
        // write must leave the previous settings (and saved mic defaults) intact.
        let mut file = tempfile::NamedTempFile::new_in(parent).context("create settings temporary file")?;
        file.write_all(text.as_bytes()).context("write settings temporary file")?;
        file.as_file().sync_all().context("flush settings temporary file")?;
        file.persist(path)
            .map_err(|e| e.error)
            .with_context(|| format!("replace {}", path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
thread_local! {
    static SETTINGS_PATH_FOR_TEST: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn use_settings_path_for_test(path: &Path) -> impl Drop {
    struct Restore(Option<PathBuf>, std::marker::PhantomData<std::rc::Rc<()>>);
    impl Drop for Restore {
        fn drop(&mut self) { SETTINGS_PATH_FOR_TEST.with(|slot| *slot.borrow_mut() = self.0.take()); }
    }
    Restore(SETTINGS_PATH_FOR_TEST.with(|slot| slot.replace(Some(path.to_owned()))), std::marker::PhantomData)
}

pub fn settings_path() -> PathBuf {
    #[cfg(test)]
    if let Some(path) = SETTINGS_PATH_FOR_TEST.with(|slot| slot.borrow().clone()) { return path; }
    // Tests exercise code that saves settings; keep them off the real file.
    if cfg!(test) {
        return std::env::temp_dir()
            .join(format!("openmic-test-{}", std::process::id()))
            .join("settings.json");
    }
    ProjectDirs::from("app", "OpenMic", "OpenMic")
        .map(|dirs| dirs.config_dir().join("settings.json"))
        .unwrap_or_else(|| PathBuf::from("settings.json"))
}

fn legacy_settings_paths() -> Vec<PathBuf> {
    let mut paths = vec![PathBuf::from("settings.json")];
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        paths.push(dir.join("settings.json"));
        if let Some(root) = dir.parent().and_then(|p| p.parent()) {
            paths.push(root.join("settings.json"));
        }
    }
    paths
}

#[test]
fn isolated_settings_paths_restore_nested_scopes_and_do_not_cross_threads() {
    let original = settings_path();
    let folder = tempfile::tempdir().unwrap();
    let outer = folder.path().join("outer.json");
    let inner = folder.path().join("inner.json");
    {
        let _outer = use_settings_path_for_test(&outer);
        assert_eq!(settings_path(), outer);
        {
            let _inner = use_settings_path_for_test(&inner);
            assert_eq!(settings_path(), inner);
        }
        assert_eq!(settings_path(), outer);
        let expected = original.clone();
        std::thread::spawn(move || assert_eq!(settings_path(), expected)).join().unwrap();
        assert_eq!(settings_path(), outer);
    }
    assert_eq!(settings_path(), original);
}

/// Passed by the Windows startup entry: start hidden in the tray.
pub const MINIMIZED_ARG: &str = "--minimized";

#[cfg(windows)]
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
#[cfg(windows)]
const STARTUP_NAME: &str = "OpenMic";
#[cfg(windows)]
const LEGACY_STARTUP_NAME: &str = "Discord Denoiser";

#[cfg(windows)]
pub fn startup_enabled() -> bool {
    use winreg::{enums::HKEY_CURRENT_USER, RegKey};

    let Ok(key) = RegKey::predef(HKEY_CURRENT_USER).open_subkey(RUN_KEY) else {
        return false;
    };
    key.get_value::<String, _>(STARTUP_NAME).is_ok()
        || key.get_value::<String, _>(LEGACY_STARTUP_NAME).is_ok()
}

#[cfg(not(windows))]
pub fn startup_enabled() -> bool {
    false
}

#[cfg(windows)]
pub fn set_startup(enabled: bool) -> Result<()> {
    use winreg::{enums::{HKEY_CURRENT_USER, KEY_SET_VALUE}, RegKey};

    let (key, _) = RegKey::predef(HKEY_CURRENT_USER)
        .create_subkey_with_flags(RUN_KEY, KEY_SET_VALUE)
        .context("open Windows startup registry key")?;
    let remove = |name: &str| -> Result<()> {
        match key.delete_value(name) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error).with_context(|| format!("remove {name} from Windows startup")),
        }
    };
    if enabled {
        let exe = std::env::current_exe().context("locate OpenMic executable")?;
        // Replace in place so a failed registration keeps the old entry.
        key.set_value(STARTUP_NAME, &format!("\"{}\" {MINIMIZED_ARG}", exe.display()))
            .context("register OpenMic startup")?;
    } else {
        remove(STARTUP_NAME)?;
    }
    remove(LEGACY_STARTUP_NAME)?;
    Ok(())
}

#[cfg(not(windows))]
pub fn set_startup(_enabled: bool) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_safe_and_enabled() {
        let settings = Settings::default();
        assert_eq!(settings.strength, 1.0);
        assert_eq!(settings.sound_volume, 0.8);
        assert!(settings.auto_start);
        assert!(!settings.monitor);
        assert_eq!(settings.model, Model::DeepFilter);
        assert!(settings.highpass && settings.voice_gate);
    }

    #[test]
    fn older_settings_files_gain_the_new_voice_defaults() {
        let settings: Settings = serde_json::from_str(r#"{"strength": 0.8}"#).unwrap();
        assert_eq!(settings.strength, 0.8);
        assert_eq!(settings.model, Model::DeepFilter);
        assert!(settings.highpass && settings.voice_gate);
        assert_eq!(settings.voice_threshold, VOICE_THRESHOLD);
        assert!(settings.always_show_strip, "older settings keep the strip visible by default");
    }

    #[test]
    fn bare_pad_paths_from_older_versions_still_load() {
        let mut settings: Settings = serde_json::from_str(
            r#"{"sounds": ["C:/clips/airhorn.mp3", {"path": "C:/clips/drum.wav", "volume": 0.5, "hotkey": "Ctrl+Alt+1"}]}"#,
        )
        .unwrap();
        assert_eq!(settings.sounds[0], Pad::new("C:/clips/airhorn.mp3".into()));
        assert_eq!(settings.sounds[1].volume, 0.5);
        assert_eq!(settings.sounds[1].hotkey.as_deref(), Some("Ctrl+Alt+1"));
        assert_eq!(settings.sounds[1].display_name(), "drum");
        settings.sounds[1].name = Some("Drum roll".into());

        let saved = serde_json::to_string(&settings).unwrap();
        let reloaded: Settings = serde_json::from_str(&saved).unwrap();
        assert_eq!(reloaded.sounds, settings.sounds);
        assert_eq!(reloaded.sounds[1].display_name(), "Drum roll");
        assert_eq!(reloaded.sounds[1].path, PathBuf::from("C:/clips/drum.wav"));
    }

    #[test]
    fn presets_round_trip_the_processing_settings() {
        let mut settings = Settings::default();
        assert_eq!(settings.processing(), Processing::default());
        let keyboard = &builtin_presets()[1];
        settings.apply_processing(&keyboard.processing);
        assert_eq!(settings.processing(), keyboard.processing);
        assert_eq!(settings.voice_threshold, 0.75);
        assert!(settings.close_to_tray, "unrelated settings are untouched");
    }

    #[test]
    fn missing_settings_use_defaults_and_valid_legacy_settings_still_migrate() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("current/settings.json");
        let legacy = dir.path().join("legacy.json");
        let fresh = Settings::load_from(&path, std::slice::from_ref(&legacy));
        assert!(fresh.load_error.is_none() && fresh.auto_start && fresh.sounds.is_empty());
        assert!(!path.exists(), "a missing file is not a failed load");

        let mut previous = Settings { mute: true, microphone: "USB mic".into(), ..Default::default() };
        previous.sounds.push(Pad::new("applause.wav".into()));
        previous.hotkeys.mute = Some("Ctrl+Alt+M".into());
        previous.saved_default_mic = Some(SavedDefaults { console: "before-console".into(), communications: "before-chat".into() });
        previous.save_to(&legacy).unwrap();
        let original = fs::read(&legacy).unwrap();
        let loaded = Settings::load_from(&path, std::slice::from_ref(&legacy));
        assert!(loaded.load_error.is_none());
        assert_eq!(serde_json::to_value(&loaded).unwrap(), serde_json::to_value(&previous).unwrap());
        assert_eq!(fs::read(&legacy).unwrap(), original);
        let migrated: Settings = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(serde_json::to_value(migrated).unwrap(), serde_json::to_value(&previous).unwrap());

        Settings::default().save_to(&legacy).unwrap();
        assert!(Settings::load_from(&path, &[legacy]).mute, "an existing current file takes precedence over old copies");
    }

    #[test]
    fn malformed_settings_block_saving_and_never_fall_back_to_an_old_copy() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let legacy = dir.path().join("legacy.json");
        Settings::default().save_to(&legacy).unwrap();
        for bytes in [b"{\"sounds\":[".as_slice(), b"\xff\xfe\x00", b"{\"model\":\"unknown-model\"}"] {
            fs::write(&path, bytes).unwrap();
            let mut loaded = Settings::load_from(&path, std::slice::from_ref(&legacy));
            let error = loaded.load_error.as_ref().unwrap();
            assert_eq!(error.path, path);
            assert!(error.message.starts_with("Parse settings:"));
            loaded.mute = true;
            assert!(loaded.save_to(&path).is_err(), "later changes cannot overwrite an unreadable source");
            assert_eq!(fs::read(&path).unwrap(), bytes);
        }

        let failed = Settings::load_from(&path, &[legacy]);
        fs::write(&path, br#"{"mute":true,"sounds":["clap.wav"],"highpass_hz":120.0}"#).unwrap();
        assert!(failed.save_to(&path).is_err(), "even a repaired file is protected until it has actually been reloaded");
        let mut restored = failed.retry_from(&path);
        assert!(restored.load_error.is_none() && restored.mute);
        assert_eq!(restored.sounds, [Pad::new("clap.wav".into())]);
        assert_eq!(restored.highpass_hz, 120.0);
        restored.mute = false;
        restored.save_to(&path).unwrap();
        assert!(!fs::read_to_string(&path).unwrap().contains("load_error"));
        assert!(!Settings::load_from(&path, &[]).mute);
    }

    #[test]
    fn failed_legacy_loads_stay_intact_and_retries_do_not_revive_unrelated_settings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("current/settings.json");
        let legacy = dir.path().join("legacy.json");
        let older = dir.path().join("older.json");
        Settings { mute: true, ..Default::default() }.save_to(&older).unwrap();
        let bytes = b"{ unfinished settings";
        fs::write(&legacy, bytes).unwrap();
        let failed = Settings::load_from(&path, &[legacy.clone(), older]);
        assert_eq!(failed.load_error.as_ref().unwrap().path, legacy);
        assert!(failed.save_to(&path).is_err());
        assert_eq!(fs::read(&legacy).unwrap(), bytes);
        assert!(!path.exists());

        Settings { sound_volume: 0.3, ..Default::default() }.save_to(&legacy).unwrap();
        let repaired = failed.retry_from(&path);
        assert!(repaired.load_error.is_none());
        assert_eq!(repaired.sound_volume, 0.3);
        assert!(path.exists(), "a repaired legacy file can finish migrating");

        fs::write(&path, bytes).unwrap();
        let failed = Settings::load_from(&path, std::slice::from_ref(&legacy));
        let backup = dir.path().join("settings-backup.json");
        fs::rename(&path, &backup).unwrap();
        let fresh = failed.retry_from(&path);
        assert!(fresh.load_error.is_none());
        assert_eq!(fresh.sound_volume, Settings::default().sound_volume, "moving the failed file aside allows a fresh start");
        assert_eq!(fs::read(backup).unwrap(), bytes);
    }

    #[test]
    fn overflowing_settings_numbers_are_protected_until_repaired() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        for json in [
            r#"{"input_gain_db":1e100}"#,
            r#"{"monitor_volume":-1e100}"#,
            r#"{"presets":[{"name":"Broken","processing":{"voice_threshold":1e100}}]}"#,
            r#"{"sounds":[{"path":"clip.wav","volume":1e100}]}"#,
            r#"{"sounds":[{"path":"clip.wav","start":1e100}]}"#,
            r#"{"sounds":[{"path":"clip.wav","end":1e100}]}"#,
            r#"{"strip_width":1e100}"#,
            r#"{"window_size":[700,1e100]}"#,
        ] {
            fs::write(&path, json).unwrap();
            let loaded = Settings::load_from(&path, &[]);
            assert!(loaded.load_error.as_ref().unwrap().message.contains("finite number"), "{json}");
            assert!(loaded.save_to(&path).is_err());
            assert_eq!(fs::read_to_string(&path).unwrap(), json, "preserve the source instead of saving null");
            fs::write(&path, br#"{"input_gain_db":6.0}"#).unwrap();
            let repaired = loaded.retry_from(&path);
            assert!(repaired.load_error.is_none());
            assert_eq!(repaired.input_gain_db, 6.0);
            repaired.save_to(&path).unwrap();
        }
    }

    #[test]
    fn nonfinite_runtime_values_never_replace_valid_settings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        Settings { mute: true, ..Default::default() }.save_to(&path).unwrap();
        let before = fs::read(&path).unwrap();
        for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            for field in ["voice", "monitor", "soundboard", "preset", "pad trim", "strip", "window"] {
                let mut settings = Settings::default();
                match field {
                    "voice" => settings.gate_threshold_db = value,
                    "monitor" => settings.monitor_volume = value,
                    "soundboard" => settings.sound_volume = value,
                    "preset" => settings.presets.push(Preset {
                        name: "Broken".into(), processing: Processing { strength: value, ..Default::default() },
                    }),
                    "pad trim" => settings.sounds.push(Pad { end: Some(value), ..Pad::new("clip.wav".into()) }),
                    "strip" => settings.strip_width = Some(value),
                    _ => settings.window_size = Some([value, 420.0]),
                }
                let error = settings.save_to(&path).unwrap_err();
                assert!(format!("{error:#}").contains("finite number"), "{field}: {error:#}");
                assert_eq!(fs::read(&path).unwrap(), before);
                assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1, "validation happens before creating a temporary file");
            }
        }
    }

    #[test]
    fn relative_settings_failures_keep_the_full_location_for_recovery_and_retry() {
        let current_dir = std::env::current_dir().unwrap();
        let mut file = tempfile::NamedTempFile::new_in(&current_dir).unwrap();
        file.write_all(b"{ unfinished settings").unwrap();
        file.flush().unwrap();
        let relative = PathBuf::from(file.path().file_name().unwrap());
        let destination = tempfile::tempdir().unwrap();
        let path = destination.path().join("settings.json");

        for (primary, legacy) in [
            (relative.as_path(), Vec::new()),
            (path.as_path(), vec![relative.clone()]),
        ] {
            let failed = Settings::load_from(primary, &legacy);
            let error = failed.load_error.as_ref().unwrap();
            assert!(error.path.is_absolute());
            assert_eq!(error.path, file.path());
            assert_eq!(error.path.parent(), Some(current_dir.as_path()), "Open settings folder must not receive an empty path");
            assert!(failed.save_to(primary).is_err());
        }

        let failed = Settings::load_from(&path, &[relative]);
        fs::write(file.path(), br#"{"mute":true,"sounds":["clap.wav"]}"#).unwrap();
        let recovered = failed.retry_from(&path);
        assert!(recovered.load_error.is_none() && recovered.mute);
        assert_eq!(recovered.sounds, [Pad::new("clap.wav".into())]);
        assert!(path.exists(), "a repaired relative legacy source can still migrate");
    }

    #[cfg(windows)]
    #[test]
    fn a_locked_settings_file_is_protected_until_a_successful_retry() {
        use std::os::windows::fs::OpenOptionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let legacy = dir.path().join("legacy.json");
        Settings { mute: true, ..Default::default() }.save_to(&path).unwrap();
        Settings::default().save_to(&legacy).unwrap();
        let before = fs::read(&path).unwrap();
        let held = fs::OpenOptions::new().read(true).share_mode(0).open(&path).unwrap();
        let failed = Settings::load_from(&path, &[legacy]);
        assert!(failed.load_error.as_ref().unwrap().message.starts_with("Read settings:"));
        assert!(failed.save_to(&path).is_err());
        drop(held);
        assert!(failed.save_to(&path).is_err(), "releasing a lock does not authorize defaults to replace the original");
        assert_eq!(fs::read(&path).unwrap(), before);
        let loaded = failed.retry_from(&path);
        assert!(loaded.load_error.is_none() && loaded.mute);
        assert_eq!(fs::read(&path).unwrap(), before);
    }

    #[test]
    fn saving_replaces_complete_settings_and_leaves_no_temporary_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/settings.json");
        let mut settings = Settings::default();
        settings.save_to(&path).unwrap();
        settings.mute = true;
        settings.always_show_strip = false;
        settings.save_to(&path).unwrap();
        let loaded: Settings = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert!(loaded.mute);
        assert!(!loaded.always_show_strip, "opting out survives a restart");
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
    }

    #[cfg(windows)]
    #[test]
    fn failed_replacement_preserves_previous_settings() {
        use std::os::windows::fs::OpenOptionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let mut settings = Settings::default();
        settings.save_to(&path).unwrap();
        let before = fs::read(&path).unwrap();
        // Another process permits reading but refuses replacement.
        let held = fs::OpenOptions::new().read(true).share_mode(1).open(&path).unwrap();
        settings.mute = true;
        assert!(settings.save_to(&path).is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
        drop(held);
        settings.save_to(&path).unwrap();
    }
}
