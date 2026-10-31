//! Einstellungen als JSON im AppData-Ordner.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TargetConfig {
    /// WASAPI-Endpoint-ID
    pub device_id: String,
    /// Anzeigename (für nicht angeschlossene Geräte)
    pub name: String,
    #[serde(default = "default_volume")]
    pub volume: f32,
    #[serde(default)]
    pub muted: bool,
    #[serde(default)]
    pub delay_ms: u32,
}

fn default_volume() -> f32 {
    1.0
}

impl TargetConfig {
    pub fn new(device_id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            device_id: device_id.into(),
            name: name.into(),
            volume: 1.0,
            muted: false,
            delay_ms: 0,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Config {
    /// `None` = Windows-Standardausgabe
    #[serde(default)]
    pub source_id: Option<String>,
    #[serde(default)]
    pub source_name: Option<String>,
    #[serde(default)]
    pub targets: Vec<TargetConfig>,
}

impl Config {
    pub fn default_path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("shared-audio")
            .join("config.json")
    }

    /// Lädt die Konfiguration; fehlende Datei → Standardwerte.
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(s) => {
                serde_json::from_str(&s).with_context(|| format!("{} ist ungültig", path.display()))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("{} nicht lesbar", path.display())),
        }
    }

    /// Atomar speichern (temporäre Datei + Umbenennen).
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("shared-audio-test-{}-{name}", std::process::id()))
    }

    #[test]
    fn roundtrip() {
        let dir = tmp("rt");
        let path = dir.join("config.json");
        let mut c = Config {
            source_id: Some("src".into()),
            ..Default::default()
        };
        let mut t = TargetConfig::new("dev1", "Kopfhörer A");
        t.delay_ms = 120;
        t.muted = true;
        c.targets.push(t);
        c.save(&path).unwrap();
        assert_eq!(Config::load(&path).unwrap(), c);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn missing_file_gives_default() {
        assert_eq!(
            Config::load(&tmp("none").join("x.json")).unwrap(),
            Config::default()
        );
    }

    #[test]
    fn partial_json_uses_defaults() {
        let c: Config =
            serde_json::from_str(r#"{"targets":[{"device_id":"a","name":"A"}]}"#).unwrap();
        assert_eq!(c.targets[0].volume, 1.0);
        assert_eq!(c.source_id, None);
    }

    #[test]
    fn invalid_json_is_an_error() {
        let dir = tmp("bad");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("c.json");
        std::fs::write(&p, "{ kaputt").unwrap();
        assert!(Config::load(&p).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }
}
