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
}

impl Default for Settings {
    fn default() -> Self {
        let p = Processing::default();
        Self {
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
        }
    }
}

/// One soundboard pad. Older settings files stored a bare path per pad.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(from = "PadRepr")]
pub struct Pad {
    pub path: PathBuf,
    /// The pad's own volume (0..1), on top of the soundboard volume.
    pub volume: f32,
    /// Global shortcut that plays the pad, e.g. "Ctrl+Alt+1".
    pub hotkey: Option<String>,
}

impl Pad {
    pub fn new(path: PathBuf) -> Self {
        Self { path, volume: 1.0, hotkey: None }
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum PadRepr {
    Path(PathBuf),
    Full {
        path: PathBuf,
        #[serde(default = "full_volume")]
        volume: f32,
        #[serde(default)]
        hotkey: Option<String>,
    },
}

fn full_volume() -> f32 {
    1.0
}

impl From<PadRepr> for Pad {
    fn from(repr: PadRepr) -> Self {
        match repr {
            PadRepr::Path(path) => Pad::new(path),
            PadRepr::Full { path, volume, hotkey } => Pad { path, volume, hotkey },
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
    pub fn load() -> Self {
        let path = settings_path();
        let loaded = fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<Settings>(&text).ok());
        if let Some(settings) = loaded {
            return settings;
        }

        for legacy in legacy_settings_paths() {
            let Some(settings) = fs::read_to_string(&legacy)
                .ok()
                .and_then(|text| serde_json::from_str::<Settings>(&text).ok())
            else {
                continue;
            };
            let _ = settings.save();
            return settings;
        }
        Self::default()
    }

    pub fn save(&self) -> Result<()> {
        self.save_to(&settings_path())
    }

    fn save_to(&self, path: &Path) -> Result<()> {
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

pub fn settings_path() -> PathBuf {
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
    use winreg::{enums::HKEY_CURRENT_USER, RegKey};

    let (key, _) = RegKey::predef(HKEY_CURRENT_USER)
        .create_subkey(RUN_KEY)
        .context("open Windows startup registry key")?;
    let _ = key.delete_value(STARTUP_NAME);
    let _ = key.delete_value(LEGACY_STARTUP_NAME);
    if enabled {
        let exe = std::env::current_exe().context("locate OpenMic executable")?;
        key.set_value(STARTUP_NAME, &format!("\"{}\" {MINIMIZED_ARG}", exe.display()))
            .context("register OpenMic startup")?;
    }
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
    }

    #[test]
    fn bare_pad_paths_from_older_versions_still_load() {
        let settings: Settings = serde_json::from_str(
            r#"{"sounds": ["C:/clips/airhorn.mp3", {"path": "C:/clips/drum.wav", "volume": 0.5, "hotkey": "Ctrl+Alt+1"}]}"#,
        )
        .unwrap();
        assert_eq!(settings.sounds[0], Pad::new("C:/clips/airhorn.mp3".into()));
        assert_eq!(settings.sounds[1].volume, 0.5);
        assert_eq!(settings.sounds[1].hotkey.as_deref(), Some("Ctrl+Alt+1"));

        let saved = serde_json::to_string(&settings).unwrap();
        let reloaded: Settings = serde_json::from_str(&saved).unwrap();
        assert_eq!(reloaded.sounds, settings.sounds);
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
    fn saving_replaces_complete_settings_and_leaves_no_temporary_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/settings.json");
        let mut settings = Settings::default();
        settings.save_to(&path).unwrap();
        settings.mute = true;
        settings.save_to(&path).unwrap();
        let loaded: Settings = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert!(loaded.mute);
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
