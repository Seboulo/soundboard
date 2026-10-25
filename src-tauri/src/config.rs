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
    /// Épinglé en haut de la grille.
    #[serde(default)]
    pub favorite: bool,
    /// Pas coupé quand un autre son démarre.
    #[serde(default)]
    pub keep_playing: bool,
    /// Date d'ajout (secondes depuis 1970), pour le tri « ajoutés récemment ».
    /// 0 pour les sons ajoutés avant la 0.11.
    #[serde(default)]
    pub added_at: u64,
    /// Image du son (GIF animé joué pendant le son, ou image choisie).
    #[serde(default)]
    pub image: Option<PathBuf>,
    /// Version fixe de l'image, affichée quand le son ne joue pas.
    #[serde(default)]
    pub image_still: Option<PathBuf>,
    /// Statistiques : nombre de lectures, temps joué, dernière lecture
    /// (secondes depuis 1970, 0 si jamais joué).
    #[serde(default)]
    pub plays: u32,
    #[serde(default)]
    pub played_ms: u64,
    #[serde(default)]
    pub last_played: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Analysis {
    pub duration_ms: u64,
    pub peak: f32,
    pub rms: f32,
    /// Niveau moyen des passages audibles seulement (silences ignorés).
    /// 0 pour les sons analysés avant la 0.9.4 : recalculé au lancement.
    #[serde(default)]
    pub loudness: f32,
    /// Volume ressenti (EBU R128, en LUFS) : moyenne, pic sur 400 ms et pic
    /// sur 3 s. 0 pour les sons analysés avant la 0.12 : recalculé au lancement.
    #[serde(default)]
    pub lufs: f32,
    #[serde(default)]
    pub max_momentary: f32,
    #[serde(default)]
    pub max_short: f32,
}

impl Sound {
    /// Durée jouée, découpe comprise.
    pub fn play_duration_ms(&self) -> Option<u64> {
        let total = self.analysis?.duration_ms;
        let end = self.end_ms.unwrap_or(total).min(total);
        Some(end.saturating_sub(self.start_ms))
    }

    /// Gain qui amène le son à un volume ressenti commun sans saturer.
    ///
    /// Le volume est mesuré comme à la télé (EBU R128, en LUFS) et visé
    /// sous une voix dans Discord : `target` LUFS en moyenne (-28 par
    /// défaut), sans dépasser `target + 5` sur 3 s ni `target + 8` sur
    /// 400 ms. Ces deux plafonds calment les cris et explosions qui partent
    /// d'un coup après un début calme.
    pub fn normalize_gain(&self, target: f32) -> f32 {
        let target_lufs = target.clamp(-40.0, -14.0);
        let max_short = target_lufs + 5.0;
        let max_momentary = target_lufs + 8.0;
        // Marge sous 0 dBFS pour les pics entre deux échantillons.
        const PEAK_CEILING: f32 = 0.89;
        match self.analysis {
            Some(a) if a.peak > 0.0001 => {
                let gain = if a.lufs < 0.0 {
                    let db = (target_lufs - a.lufs)
                        .min(max_short - a.max_short)
                        .min(max_momentary - a.max_momentary);
                    10f32.powf(db / 20.0)
                } else {
                    let level = if a.loudness > 0.0001 { a.loudness } else { a.rms };
                    if level <= 0.0001 {
                        return 1.0;
                    }
                    // Ancienne mesure : RMS ≈ LUFS pour un son moyen.
                    10f32.powf((target_lufs + 0.7) / 20.0) / level
                };
                gain.min(PEAK_CEILING / a.peak).clamp(0.03, 2.0)
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
    /// Volume ressenti visé par l'égalisation, en LUFS (-31, -28 ou -25).
    pub loudness_target: f32,
    pub launch_at_startup: bool,
    /// Fermer la fenêtre garde l'app près de l'horloge (les raccourcis restent actifs).
    pub close_to_tray: bool,
    /// Enlève clavier, souris et bruits de fond du micro (pas des sons).
    pub noise_suppression: bool,
    /// Réduction de bruit par IA (DeepFilterNet) plutôt que RNNoise.
    pub strong_denoise: bool,
    /// Coupe le micro quand tu ne parles pas.
    pub voice_gate: bool,
    /// Sensibilité automatique du micro, comme Discord.
    pub gate_auto: bool,
    /// Probabilité de voix (0–1) en dessous de laquelle le micro est coupé.
    pub gate_threshold: f32,
    /// Volume de voix automatique : voix douce remontée, cris adoucis.
    pub voice_leveler: bool,
    /// Raccourci qui coupe / rallume ton micro (les sons passent toujours).
    pub mute_hotkey: Option<String>,
    /// Raccourci qui joue un son au hasard.
    pub random_hotkey: Option<String>,
    pub voice_effect: VoiceEffect,
    /// Clé personnelle de l'API Freesound (gratuite), pour la recherche en ligne.
    pub freesound_key: Option<String>,
    /// Clé GIPHY (gratuite) pour illustrer les sons avec des images de memes.
    pub giphy_key: Option<String>,
    /// Page web sur le réseau local pour jouer les sons depuis un téléphone.
    pub phone_enabled: bool,
    pub phone_port: u16,
    /// Code secret dans l'adresse de la page du téléphone.
    pub phone_token: String,
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
            loudness_target: -28.0,
            launch_at_startup: false,
            close_to_tray: true,
            noise_suppression: true,
            strong_denoise: true,
            voice_gate: true,
            gate_auto: true,
            gate_threshold: 0.6,
            voice_leveler: true,
            mute_hotkey: None,
            random_hotkey: None,
            voice_effect: VoiceEffect::None,
            freesound_key: None,
            giphy_key: None,
            phone_enabled: false,
            phone_port: 7878,
            phone_token: String::new(),
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
