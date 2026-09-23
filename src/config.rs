use std::{fs, path::PathBuf};

use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use crate::dsp::{Model, VOICE_THRESHOLD};

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
    pub voice_gate: bool,
    pub voice_threshold: f32,
    pub bypass: bool,
    pub mute: bool,
    pub monitor: bool,
    pub monitor_volume: f32,
    pub sound_volume: f32,
    pub sounds: Vec<PathBuf>,
    pub start_with_windows: bool,
    pub auto_start: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            microphone: String::new(),
            output: String::new(),
            monitor_output: String::new(),
            model: Model::default(),
            strength: 1.0,
            input_gain_db: 0.0,
            output_gain_db: 0.0,
            gate: false,
            gate_threshold_db: -50.0,
            highpass: true,
            voice_gate: true,
            voice_threshold: VOICE_THRESHOLD,
            bypass: false,
            mute: false,
            monitor: false,
            monitor_volume: 1.0,
            sound_volume: 0.8,
            sounds: Vec::new(),
            start_with_windows: false,
            auto_start: true,
        }
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
        let path = settings_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).context("create OpenMic settings directory")?;
        }
        let text = serde_json::to_string_pretty(self).context("serialize settings")?;
        fs::write(&path, text).with_context(|| format!("write {}", path.display()))
    }
}

pub fn settings_path() -> PathBuf {
    ProjectDirs::from("app", "OpenMic", "OpenMic")
        .map(|dirs| dirs.config_dir().join("settings.json"))
        .unwrap_or_else(|| PathBuf::from("settings.json"))
}

fn legacy_settings_paths() -> Vec<PathBuf> {
    let mut paths = vec![PathBuf::from("settings.json")];
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            paths.push(dir.join("settings.json"));
            if let Some(root) = dir.parent().and_then(|p| p.parent()) {
                paths.push(root.join("settings.json"));
            }
        }
    }
    paths
}

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
        key.set_value(STARTUP_NAME, &format!("\"{}\"", exe.display()))
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
}
