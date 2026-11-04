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
    /// Effets de l'éditeur (vitesse, hauteur, écho…), appliqués à la lecture.
    #[serde(default)]
    pub fx: crate::sound_fx::SoundFx,
    /// Niveau du passage réellement joué (découpe et effets compris),
    /// mesuré en arrière-plan. Ignoré s'il ne correspond plus à la découpe.
    #[serde(default)]
    pub segment: Option<SegmentLevel>,
    /// Son venu d'un pack partagé par lien (le lien), mis à jour avec lui.
    #[serde(default)]
    pub pack_link: Option<String>,
    /// Empreinte du fichier, pour le reconnaître dans le pack.
    #[serde(default)]
    pub pack_key: Option<String>,
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
    /// Mesuré sur le repli mono envoyé à Discord (depuis la 1.5). Les
    /// mesures plus anciennes sont refaites au lancement.
    #[serde(default)]
    pub mono_fold: bool,
}

/// Découpe et effets pour lesquels un niveau a été mesuré.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SegmentKey {
    pub start_ms: u64,
    pub end_ms: Option<u64>,
    pub fx: crate::sound_fx::SoundFx,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct SegmentLevel {
    pub key: SegmentKey,
    pub analysis: Analysis,
}

impl Sound {
    /// `None` quand le son est joué en entier et sans effet : la mesure du
    /// fichier suffit.
    pub fn segment_key(&self) -> Option<SegmentKey> {
        if self.start_ms == 0 && self.end_ms.is_none() && self.fx.is_neutral() {
            return None;
        }
        Some(SegmentKey { start_ms: self.start_ms, end_ms: self.end_ms, fx: self.fx })
    }

    /// Le passage découpé n'a pas encore de mesure à jour.
    pub fn needs_segment(&self) -> bool {
        self.segment_key().is_some_and(|k| self.segment.is_none_or(|s| s.key != k || !s.analysis.mono_fold))
    }

    /// Mesure à utiliser pour le volume : celle du passage joué si elle est
    /// à jour, sinon celle du fichier entier.
    fn level(&self) -> Option<Analysis> {
        match (self.segment_key(), &self.segment) {
            (Some(k), Some(seg)) if seg.key == k => Some(seg.analysis),
            _ => self.analysis,
        }
    }

    /// Durée jouée, découpe comprise.
    pub fn play_duration_ms(&self) -> Option<u64> {
        let total = self.analysis?.duration_ms;
        let end = self.end_ms.unwrap_or(total).min(total);
        Some(self.fx.output_ms(end.saturating_sub(self.start_ms), self.looping))
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
        match self.level() {
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

/// Couleur de la voix (égaliseur du micro).
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VoiceEq {
    /// Aucune correction.
    #[default]
    Natural,
    /// Plus de corps, aigus adoucis : pour un micro trop brillant.
    Warm,
    /// Moins de « boue », plus de présence : pour un micro sourd.
    Clear,
    /// Calculé par le test de voix pour rapprocher ton micro d'une voix
    /// équilibrée (voir `MicCalibration::eq_db`).
    Custom,
}

/// Mesures du test de voix, en dBFS : de quoi régler le micro dès le
/// premier mot au lieu de l'apprendre pendant les premières secondes.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MicCalibration {
    /// Bruit de fond de la pièce.
    pub noise_db: f32,
    /// Niveau moyen de ta voix.
    pub speech_db: f32,
    /// Corrections de l'égaliseur sur mesure, en dB, pour chaque bande de
    /// `denoise::EQ_BANDS_HZ`.
    #[serde(default)]
    pub eq_db: [f32; 6],
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
    pub voice_eq: VoiceEq,
    pub mic_calibration: Option<MicCalibration>,
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
    /// Mode Surprise : sons qui peuvent sortir (vide = tous les sons).
    pub surprise_sounds: Vec<String>,
    /// Mode Surprise : attente entre deux sons, en minutes (tirée au hasard entre les deux).
    pub surprise_min_minutes: u32,
    pub surprise_max_minutes: u32,
    /// Garder les 30 dernières secondes pour « Rejouer ».
    pub replay_enabled: bool,
    /// Raccourci qui fait un son des 30 dernières secondes.
    pub replay_hotkey: Option<String>,
    /// Baisser les sons quand un pote parle dans Discord.
    pub duck_enabled: bool,
    /// Volume des sons pendant qu'un pote parle (0,1–0,8).
    pub duck_level: f32,
    /// Code du salon entre potes (vide : pas de salon).
    pub room_code: String,
    /// Ton nom dans le salon.
    pub room_name: String,
    /// Identifiant de cette app dans les salons (fixe).
    pub room_id: String,
    /// Les potes du salon peuvent jouer des sons chez toi.
    pub room_accept: bool,
    /// Extraits des potes sur mon écran : "off", "ask" (demander) ou "auto".
    pub clips_mode: String,
    /// Packs de potes suivis par lien.
    pub subscriptions: Vec<Subscription>,
    /// Onglets que tu partages par lien.
    pub published: Vec<Published>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Subscription {
    pub url: String,
    /// Onglet où arrivent les sons.
    pub name: String,
    /// Empreinte du dernier pack lu (rien à faire s'il n'a pas changé).
    pub last_hash: String,
    /// Dernière vérification (secondes depuis 1970).
    pub last_check: u64,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Published {
    pub category: String,
    /// Fichier .zip tenu à jour, dans un dossier synchronisé.
    pub path: PathBuf,
    /// Empreinte du contenu écrit la dernière fois.
    pub signature: String,
    pub error: Option<String>,
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
            voice_eq: VoiceEq::Natural,
            mic_calibration: None,
            mute_hotkey: None,
            random_hotkey: None,
            voice_effect: VoiceEffect::None,
            freesound_key: None,
            giphy_key: None,
            phone_enabled: false,
            phone_port: 7878,
            phone_token: String::new(),
            surprise_sounds: Vec::new(),
            surprise_min_minutes: 10,
            surprise_max_minutes: 60,
            replay_enabled: true,
            replay_hotkey: None,
            duck_enabled: false,
            duck_level: 0.35,
            room_code: String::new(),
            room_name: String::new(),
            room_id: String::new(),
            room_accept: true,
            clips_mode: "ask".into(),
            subscriptions: Vec::new(),
            published: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub sounds: Vec<Sound>,
    pub settings: Settings,
    /// Ordre des onglets choisi à la main ; garde aussi les onglets encore vides.
    pub categories: Vec<String>,
}

impl Config {
    /// Lit la config. Si le fichier est abîmé, il est mis de côté
    /// (`config.corrupt.json`) et la sauvegarde `config.json.bak` est
    /// reprise : la soundboard n'est jamais remplacée par une vide.
    /// Renvoie aussi un message à afficher quand une réparation a eu lieu.
    pub fn load(path: &Path) -> (Self, Option<String>) {
        let backup = backup_path(path);
        let read = |p: &Path| -> Option<Result<Self, ()>> {
            let text = fs::read_to_string(p).ok()?;
            Some(serde_json::from_str(&text).map_err(|_| ()))
        };
        match read(path) {
            Some(Ok(config)) => {
                // Bonne config : on la garde comme sauvegarde de secours,
                // une fois par lancement.
                let _ = fs::copy(path, &backup);
                (config, None)
            }
            broken => {
                if broken.is_some() {
                    let _ = fs::rename(path, path.with_extension("corrupt.json"));
                }
                match read(&backup) {
                    Some(Ok(config)) => {
                        let _ = config.save(path);
                        let msg = if broken.is_some() {
                            "le fichier des réglages était abîmé, la dernière sauvegarde a été restaurée."
                        } else {
                            "le fichier des réglages avait disparu, la dernière sauvegarde a été restaurée."
                        };
                        (config, Some(msg.into()))
                    }
                    _ if broken.is_some() => (
                        Self::default(),
                        Some("le fichier des réglages était abîmé et aucune sauvegarde n'existe. Il a été mis de côté (config.corrupt.json).".into()),
                    ),
                    _ => (Self::default(), None),
                }
            }
        }
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

fn backup_path(path: &Path) -> PathBuf {
    path.with_extension("json.bak")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sb-config-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn one_sound() -> Config {
        let mut c: Config = serde_json::from_str(
            r##"{"sounds":[{"id":"a","name":"Pouet","path":"x.mp3","hotkey":null,"volume":1.0,"color":"#fff"}]}"##,
        )
        .unwrap();
        c.settings.master_volume = 0.5;
        c
    }

    #[test]
    fn corrupt_config_restores_backup() {
        let dir = temp_dir("corrupt");
        let path = dir.join("config.json");
        one_sound().save(&path).unwrap();
        // Premier lancement : la sauvegarde est faite.
        let (c, note) = Config::load(&path);
        assert_eq!(c.sounds.len(), 1);
        assert!(note.is_none());
        // Fichier abîmé : on retrouve les sons et l'original est mis de côté.
        fs::write(&path, "{ pas du json").unwrap();
        let (c, note) = Config::load(&path);
        assert_eq!(c.sounds.len(), 1);
        assert!(note.is_some());
        assert!(dir.join("config.corrupt.json").exists());
        assert_eq!(Config::load(&path).0.sounds.len(), 1);
    }

    #[test]
    fn corrupt_config_without_backup_is_kept_aside() {
        let dir = temp_dir("nobackup");
        let path = dir.join("config.json");
        fs::write(&path, "{ pas du json").unwrap();
        let (c, note) = Config::load(&path);
        assert!(c.sounds.is_empty());
        assert!(note.is_some());
        assert!(dir.join("config.corrupt.json").exists());
    }

    #[test]
    fn first_launch_is_silent() {
        let dir = temp_dir("first");
        let (c, note) = Config::load(&dir.join("config.json"));
        assert!(c.sounds.is_empty());
        assert!(note.is_none());
    }
}
