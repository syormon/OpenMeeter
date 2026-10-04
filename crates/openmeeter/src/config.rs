use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::model::Mixer;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub mixer: Mixer,
    pub settings: AppSettings,
}

/// App behaviour from the Menu (not part of presets, which only hold the mixer).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    pub auto_restart: AutoRestart,
    /// Tray icon; closing the window hides to the tray instead of quitting.
    pub tray: bool,
    /// Show the window at launch (off = start hidden in the tray, or minimized).
    pub show_on_startup: bool,
    pub always_on_top: bool,
    /// Mixer controls ignore input, so nothing gets bumped by accident.
    pub lock_ui: bool,
    /// Preset loaded at every launch, replacing the last session's mixer.
    pub startup_preset: Option<PathBuf>,
    /// Audio buffered per source, in milliseconds.
    pub buffer_ms: u32,
    /// Macro buttons and their global hotkeys.
    pub macros: Vec<crate::macros::Macro>,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            auto_restart: AutoRestart::A1,
            tray: true,
            show_on_startup: true,
            always_on_top: false,
            lock_ui: false,
            startup_preset: None,
            buffer_ms: 20,
            macros: Vec::new(),
        }
    }
}

/// Restart the engine automatically when a failed device comes back.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum AutoRestart {
    Off,
    /// Only when the A1 device failed (Voicemeeter's default).
    #[default]
    A1,
    AllDevices,
}

/// A preset holds only the mixer, so loading one never changes app behaviour.
#[derive(Serialize, Deserialize)]
struct Preset {
    mixer: Mixer,
}

pub fn save_preset(mixer: &Mixer, path: &Path) -> anyhow::Result<()> {
    let preset = Preset { mixer: mixer.clone() };
    std::fs::write(path, serde_json::to_string_pretty(&preset)?).with_context(|| format!("writing {}", path.display()))
}

/// Accepts preset files and full config files alike.
pub fn load_preset(path: &Path) -> anyhow::Result<Mixer> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let preset: Preset = serde_json::from_str(&text).with_context(|| format!("{} isn't an OpenMeeter settings file", path.display()))?;
    Ok(preset.mixer)
}

/// Default config location. The mock backend gets its own file so UI testing
/// never touches the real mixer settings.
pub fn default_path(mock: bool) -> anyhow::Result<PathBuf> {
    let dirs = directories::ProjectDirs::from("", "", "openmeeter").context("no home directory")?;
    let file = if mock { "config.mock.json" } else { "config.json" };
    Ok(dirs.config_dir().join(file))
}

/// Load the config, falling back to defaults if it's missing or unreadable.
pub fn load(path: &Path) -> Config {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
            log::warn!("ignoring invalid config {}: {e}", path.display());
            Config::default()
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Config::default(),
        Err(e) => {
            log::warn!("could not read {}: {e}", path.display());
            Config::default()
        }
    }
}

pub fn save(config: &Config, path: &Path) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // Write-then-rename so a crash mid-write can't leave a truncated config.
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(config)?)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_through_json() {
        let mut config = Config::default();
        config.mixer.strips[0].gain_db = -6.5;
        let text = serde_json::to_string(&config).unwrap();
        assert_eq!(serde_json::from_str::<Config>(&text).unwrap(), config);
    }

    #[test]
    fn presets_hold_only_the_mixer() {
        let dir = std::env::temp_dir().join(format!("openmeeter-preset-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("preset.json");
        let mut mixer = Mixer::default();
        mixer.strips[0].gain_db = -12.0;
        save_preset(&mixer, &path).unwrap();
        assert_eq!(load_preset(&path).unwrap(), mixer);
        assert!(!std::fs::read_to_string(&path).unwrap().contains("settings"));

        // A full config file loads as a preset too.
        let config = Config { mixer: mixer.clone(), ..Config::default() };
        std::fs::write(&path, serde_json::to_string(&config).unwrap()).unwrap();
        assert_eq!(load_preset(&path).unwrap(), mixer);
        assert!(load_preset(&dir.join("missing.json")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_fields_use_defaults() {
        assert_eq!(serde_json::from_str::<Config>("{}").unwrap(), Config::default());
    }
}
