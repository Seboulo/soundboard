use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sound {
    pub id: String,
    pub name: String,
    /// Chemin absolu du fichier copié dans le dossier de sons de l'app.
    pub path: PathBuf,
    /// Raccourci global, ex. "Ctrl+Shift+KeyA" ou "Numpad1".
    pub hotkey: Option<String>,
    /// Volume propre au son, 0.0 – 2.0.
    pub volume: f32,
    pub color: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Sortie où tu t'entends (casque). None = périphérique par défaut.
    pub monitor_device: Option<String>,
    /// Sortie vers Discord (le câble virtuel, ex. "CABLE Input").
    pub virtual_device: Option<String>,
    /// Ton vrai micro, renvoyé dans le câble virtuel avec les sons.
    pub mic_device: Option<String>,
    pub mic_enabled: bool,
    pub mic_volume: f32,
    /// Volume des sons envoyés à Discord.
    pub master_volume: f32,
    /// Volume des sons dans ton casque.
    pub monitor_volume: f32,
    /// Un nouveau son coupe ceux en cours.
    pub stop_previous: bool,
    pub stop_hotkey: Option<String>,
    pub setup_done: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            monitor_device: None,
            virtual_device: None,
            mic_device: None,
            mic_enabled: true,
            mic_volume: 1.0,
            master_volume: 0.8,
            monitor_volume: 0.6,
            stop_previous: false,
            stop_hotkey: Some("Pause".into()),
            setup_done: false,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub sounds: Vec<Sound>,
    pub settings: Settings,
}

impl Config {
    pub fn load(path: &Path) -> Self {
        fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        // Écriture atomique pour ne pas corrompre la config en cas de crash.
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, json).map_err(|e| e.to_string())?;
        fs::rename(&tmp, path).map_err(|e| e.to_string())
    }
}
