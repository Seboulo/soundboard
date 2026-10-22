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
    /// Début et fin du son après découpe, en millisecondes.
    #[serde(default)]
    pub start_ms: u64,
    #[serde(default)]
    pub end_ms: Option<u64>,
    /// Durée et niveau, calculés à l'import (sert à égaliser les volumes).
    #[serde(default)]
    pub analysis: Option<Analysis>,
    /// Onglet dans lequel le son est rangé.
    #[serde(default)]
    pub category: Option<String>,
    /// Rejoue le son en boucle jusqu'à ce qu'on l'arrête.
    #[serde(default)]
    pub looping: bool,
    /// Emoji affiché sur le pad.
    #[serde(default)]
    pub emoji: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Analysis {
    pub duration_ms: u64,
    pub peak: f32,
    pub rms: f32,
}

impl Sound {
    /// Durée jouée, découpe comprise.
    pub fn play_duration_ms(&self) -> Option<u64> {
        let total = self.analysis?.duration_ms;
        let end = self.end_ms.unwrap_or(total).min(total);
        Some(end.saturating_sub(self.start_ms))
    }

    /// Gain qui amène le son à un niveau moyen commun sans saturer.
    pub fn normalize_gain(&self) -> f32 {
        const TARGET_RMS: f32 = 0.12;
        match self.analysis {
            Some(a) if a.rms > 0.0001 && a.peak > 0.0001 => {
                (TARGET_RMS / a.rms).min(0.98 / a.peak).clamp(0.1, 4.0)
            }
            _ => 1.0,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VoiceEffect {
    #[default]
    None,
    Robot,
    High,
    Low,
    Echo,
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
    /// Égalise le volume des sons entre eux.
    pub normalize: bool,
    pub launch_at_startup: bool,
    /// Fermer la fenêtre garde l'app près de l'horloge (les raccourcis restent actifs).
    pub close_to_tray: bool,
    /// Enlève clavier, souris et bruits de fond du micro (pas des sons).
    pub noise_suppression: bool,
    /// Coupe le micro quand tu ne parles pas.
    pub voice_gate: bool,
    /// Probabilité de voix (0–1) en dessous de laquelle le micro est coupé.
    pub gate_threshold: f32,
    /// Raccourci qui coupe / rallume ton micro (les sons passent toujours).
    pub mute_hotkey: Option<String>,
    /// Raccourci qui joue un son au hasard.
    pub random_hotkey: Option<String>,
    pub voice_effect: VoiceEffect,
    /// Clé personnelle de l'API Freesound (gratuite), pour la recherche en ligne.
    pub freesound_key: Option<String>,
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
            normalize: true,
            launch_at_startup: false,
            close_to_tray: true,
            noise_suppression: true,
            voice_gate: true,
            gate_threshold: 0.6,
            mute_hotkey: None,
            random_hotkey: None,
            voice_effect: VoiceEffect::None,
            freesound_key: None,
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
