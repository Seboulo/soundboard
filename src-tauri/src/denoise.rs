//! Nettoyage du micro avant de l'envoyer dans le câble virtuel.
//!
//! Discord ne sait pas faire la différence entre ta voix et les sons de la
//! soundboard (ils arrivent par le même micro virtuel) : sa suppression de
//! bruit coupe donc aussi les sons. On nettoie ici le micro seul, avant le
//! mélange, et Discord peut garder ses filtres désactivés.
//!
//! - Filtre passe-haut à 80 Hz : enlève les grondements (bureau, souffle).
//! - Réduction de bruit forte : DeepFilterNet 3, un réseau de neurones bien
//!   meilleur que RNNoise sur le clavier et les bruits soudains, sans abîmer
//!   la voix. Environ 30 ms de latence et quelques % d'un cœur de processeur.
//! - Réduction de bruit légère : RNNoise (via `nnnoiseless`), plus ancien et
//!   plus léger.
//! - Porte de voix : RNNoise donne aussi la probabilité qu'une trame soit de
//!   la voix. La porte s'ouvre au-dessus du seuil si le micro capte assez de
//!   son (une voix lointaine ou une télé n'ouvre pas), reste ouverte tant que
//!   la voix ne retombe pas sous la moitié du seuil, puis 350 ms de plus pour
//!   ne pas hacher la fin des mots. La décision est prise sur le son qui
//!   arrive, appliquée au son qui sort avec 20 à 30 ms de retard : le début
//!   des mots n'est plus mangé.

use crate::config::{FarVoices, MicCalibration, VoiceEffect, VoiceEq};
use crate::dynamics::LookaheadLimiter;
use crate::effects::Effect;
use crate::resample::{Kernel, SincWindow};
use df::tract::{DfParams, DfTract, RuntimeParams};
use ndarray::{ArrayView2, ArrayViewMut2};
use nnnoiseless::DenoiseState;
use serde::Serialize;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// RNNoise travaille en mono, 48 kHz, par trames de 10 ms.
pub const RATE: u32 = 48_000;
const FRAME: usize = DenoiseState::FRAME_SIZE;
/// Le micro reste ouvert 500 ms après la dernière trame de voix : les
/// respirations et les silences entre deux mots ne coupent pas la phrase.
const HOLD_FRAMES: u32 = 50;
/// Tant qu'on ne connaît pas ta voix : en dessous de -55 dBFS, le micro ne
/// capte rien d'assez fort pour ouvrir la porte, même si ça ressemble à de la
/// voix.
const OPEN_FLOOR_DB: f32 = -55.0;
/// Ensuite, le plancher suit ta voix : pour ouvrir la porte, il faut arriver
/// à moins de 12 dB de ton niveau habituel, et à moins de 18 dB pour la
/// garder ouverte. Mesuré avant : avec un plancher fixe à -55 dBFS, un micro
/// très faible (voix vers -57 dBFS) n'ouvrait jamais, et une télé ou
/// quelqu'un d'autre dans la pièce, 25 dB sous ta voix, ouvrait le micro un
/// tiers du temps où tu te taisais.
const NEAR_OPEN_DB: f32 = 12.0;
const NEAR_KEEP_DB: f32 = 18.0;
/// Réglage « strict » : 8 dB de moins partout (4 pour ouvrir, 10 pour rester
/// ouvert), pour une télé plus proche. Mesuré avec une voix 15 dB sous la
/// tienne : micro ouvert 56 % de tes silences en normal, 10 % en strict,
/// pour 1 % de tes propres syllabes en moins. Parler très bas n'ouvre plus.
const NEAR_STRICT_DB: f32 = 8.0;
/// Le niveau de ta voix est gardé pour le prochain lancement une fois
/// mesuré sur 3 s de parole, s'il est d'au moins -50 dBFS : plus bas, c'est
/// peut-être la télé qu'on a entendue, et il vaut mieux repartir de zéro.
const LEARNED_AFTER_FRAMES: u32 = 300;
pub const LEARNED_MIN_DB: f32 = -50.0;
/// Sans test de voix, une voix très faible peut être la tienne sur un micro
/// réglé trop bas, ou la télé avant que tu n'aies parlé : rien ne les
/// distingue. Le plancher ne descend alors sous -55 dBFS que pour rester
/// juste au-dessus de cette voix (ses syllabes fortes ouvrent, pas les
/// autres). Le test de voix lève le doute : c'est bien toi qui parlais.
const UNSURE_OPEN_DB: f32 = -2.0;
/// Le plancher qui suit ta voix reste entre ces deux valeurs (dBFS).
const NEAR_FLOOR_MIN_DB: f32 = -75.0;
const NEAR_FLOOR_MAX_DB: f32 = -30.0;
/// Une voix bien plus forte que prévu (20 dB : plus qu'un cri, et plus que
/// l'écart normal entre une voyelle forte et ta moyenne) : c'est toi, alors
/// qu'on avait pris la télé pour ta voix, ou ton micro vient d'être monté ;
/// on s'en rapproche en une demi-seconde.
const TALKER_JUMP_DB: f32 = 20.0;
/// Une voix qui reste trop loin fait quand même descendre le niveau attendu,
/// de 0,2 dB par seconde de parole : si tu baisses ton micro d'un coup ou
/// que tu t'éloignes beaucoup, la porte se rouvre toute seule au lieu de
/// rester fermée pour toujours.
const TALKER_FAR_STEP_DB: f32 = 0.002;
/// Réduction maximale de DeepFilterNet, en attendant de connaître le bruit
/// de la pièce. Plus fort, il mange les fins de mots et les syllabes douces.
const DF_ATTEN_LIMIT_DB: f32 = 30.0;
/// Ensuite, la réduction suit le bruit de fond : juste assez pour le
/// ramener vers -85 dBFS. Dans une pièce calme, moins de réduction = voix
/// plus naturelle ; dans une pièce bruyante, on va jusqu'à 35 dB.
const DF_ATTEN_MIN_DB: f32 = 20.0;
const DF_ATTEN_MAX_DB: f32 = 35.0;
const DF_RESIDUAL_DB: f32 = -85.0;
/// Post-filtre de DeepFilterNet (valeur conseillée par ses auteurs) : il
/// baisse encore le petit bruit qui reste entre les syllabes, sans dépasser
/// la réduction maximale.
const DF_POST_FILTER: f32 = 0.02;
/// DeepFilterNet rend le son avec trois trames (30 ms) de retard.
const DF_LAG_FRAMES: usize = 3;
/// Rapport voix/bruit estimé par DeepFilterNet (dB) au-dessus duquel la
/// porte reste ouverte : l'IA reconnaît la voix dans le bruit bien mieux que
/// le détecteur de RNNoise.
const DF_KEEP_LSNR_DB: f32 = 6.0;
/// Par défaut, DeepFilterNet saute une partie du calcul selon le rapport
/// voix/bruit de chaque trame de 10 ms : rien au-dessus de 30 dB, pas de
/// 2ᵉ étage au-dessus de 20 dB, silence total en dessous de -10 dB. Sur une
/// voix proche du micro, les voyelles et les consonnes bruitées (les R, les
/// CH) tombent de part et d'autre : le traitement change d'une trame à
/// l'autre, ce qui rend les R rugueux, et les syllabes douces sont coupées
/// net. On traite donc toutes les trames de la même façon, comme le modèle
/// a été entraîné ; la porte s'occupe des silences.
const DF_THRESHOLDS: (f32, f32, f32) = (-100.0, 100.0, 100.0);
/// DeepFilterNet rend un silence total, sans transition, dès qu'une trame
/// passe sous -70 dBFS. Sur un micro très propre (ou déjà filtré par
/// Windows ou le logiciel du casque), les fins de mots y tombent : trous de
/// 20 à 200 ms au milieu des phrases, mesurés sur un vrai enregistrement.
/// Un souffle imperceptible (-64 dBFS, que DeepFilterNet retire ensuite de
/// 30 dB) le garde toujours au travail.
const DF_DITHER: f32 = 0.0011;
/// Micro qui rend un silence numérique (filtré par Windows ou le casque) :
/// après une seconde de silence complet, DeepFilterNet n'est plus appelé,
/// ce qui libère le processeur tant que tu ne parles pas. Il a vu passer
/// cette seconde de silence et repart donc d'un état propre au premier son.
const DF_IDLE_RMS: f32 = 1e-5;
const DF_IDLE_FRAMES: u32 = 100;
/// DeepFilterNet reconnaît mal une voix qui arrive très bas (micro réglé
/// faible dans Windows) : mesuré sur un vrai micro à -41 dBFS, il écrasait le
/// premier mot après un silence de 23 dB (toute sa réduction), puis mettait
/// près d'une seconde à laisser passer la voix. Avec le même son 12 dB plus
/// fort, le premier mot passe intact. On monte donc le son avant l'IA pour
/// que la voix lui arrive vers -27 dBFS (jamais plus de 30 dB, jamais de
/// baisse), et on le redescend d'autant à la sortie : le reste de la chaîne
/// ne voit aucune différence de volume.
const DF_INPUT_TARGET_DB: f32 = -27.0;
const DF_PREGAIN_MAX_DB: f32 = 30.0;
/// Le gain d'avant l'IA bouge de 0,2 dB par trame au plus (20 dB/s). Sans
/// test de voix, la première demi-seconde de parole le règle bien plus vite
/// (3 dB par trame) : mesuré sur un micro faible dans le bruit, la première
/// phrase sortait 23 dB trop bas le temps que le gain monte.
const DF_PREGAIN_STEP_DB: f32 = 0.2;
const DF_PREGAIN_FAST_STEP_DB: f32 = 3.0;
const DF_PREGAIN_FAST_FRAMES: u32 = 50;
/// RNNoise n'a pas de limite : il peut retirer 60 dB et plus, ce qui donne
/// le son « sous l'eau » sur les fins de mots. On remet donc une part du son
/// d'origine, comme DeepFilterNet : au plus 30 dB de réduction (la porte
/// s'occupe des silences).
const RNN_DRY: f32 = 0.0316;
/// Détection automatique : plus RNNoise est sûr que c'est de la voix, moins
/// elle a besoin de dépasser le bruit de fond. Une voix douce dans une pièce
/// bruyante ne dépasse souvent le bruit que de 6 à 9 dB : exiger 10 dB
/// coupait des phrases entières.
const AUTO_OPEN: [(f32, f32); 3] = [(0.9, 3.0), (0.5, 6.0), (0.2, 10.0)];
/// Pour rester ouvert : un peu au-dessus du bruit, ou de la voix probable.
const AUTO_KEEP_DB: f32 = 2.0;
/// Pendant les pauses où la porte reste ouverte (respirations, fin de
/// phrase), le volume auto montait le bruit de fond autant que la voix. Après
/// 100 ms sans voix, on baisse doucement de 8 dB ; on remonte dès que la voix
/// revient (décidé sur le son qui entre, donc avant qu'elle ne sorte).
const PAUSE_DIP: f32 = 0.398;
const PAUSE_AFTER_FRAMES: u32 = 10;
const PAUSE_VOICE: f32 = 0.3;
/// Durée des fondus d'ouverture et de fermeture, en échantillons.
const ATTACK: f32 = 1.0 / (RATE as f32 * 0.005);
const RELEASE: f32 = 1.0 / (RATE as f32 * 0.08);

#[derive(Debug, Clone, Copy)]
pub struct MicFilters {
    pub denoise: bool,
    /// DeepFilterNet plutôt que RNNoise.
    pub strong: bool,
    pub gate: bool,
    /// Sensibilité automatique (comme Discord) : la porte suit le bruit de
    /// fond de la pièce au lieu d'un seuil fixe.
    pub gate_auto: bool,
    /// Probabilité de voix (0–1) en dessous de laquelle la porte se ferme.
    pub gate_threshold: f32,
    /// Volume de voix automatique (voir `VoiceLeveler`).
    pub level: bool,
    pub eq: VoiceEq,
    /// Mesures du test de voix : bruit de fond et volume connus d'avance.
    pub calibration: Option<MicCalibration>,
    /// Sévérité de la porte envers les voix lointaines.
    pub far_voices: FarVoices,
    /// Niveau de ta voix (dBFS) appris aux lancements précédents, s'il n'y a
    /// pas de test de voix.
    pub learned_db: Option<f32>,
}

/// Enregistrement du test de voix : le son du micro tel quel (48 kHz mono)
/// et le même passé dans tout le traitement.
pub struct Recording {
    pub raw: Vec<f32>,
    pub processed: Vec<f32>,
}

/// Dernières mesures du micro, lues par l'indicateur de niveau des réglages.
#[derive(Default)]
pub struct MicMeter {
    level: AtomicU32,
    voice: AtomicU32,
    open: AtomicBool,
    /// Niveau de ta voix appris depuis l'ouverture du micro (dBFS ; 0 =
    /// pas encore).
    learned: AtomicU32,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct MicLevel {
    /// Niveau du micro (0–1, échelle adaptée à l'œil).
    pub level: f32,
    /// Probabilité de voix donnée par RNNoise (0–1).
    pub voice: f32,
    /// Le micro passe-t-il (porte ouverte) ?
    pub open: bool,
}

impl MicMeter {
    /// Niveau habituel de ta voix, une fois mesuré sur assez de parole.
    pub fn learned_db(&self) -> Option<f32> {
        let db = f32::from_bits(self.learned.load(Ordering::Relaxed));
        (db < 0.0).then_some(db)
    }

    /// À l'ouverture d'un micro : rien n'est encore appris.
    pub fn forget(&self) {
        self.learned.store(0f32.to_bits(), Ordering::Relaxed);
    }

    pub fn read(&self) -> MicLevel {
        MicLevel {
            level: f32::from_bits(self.level.load(Ordering::Relaxed)),
            voice: f32::from_bits(self.voice.load(Ordering::Relaxed)),
            open: self.open.load(Ordering::Relaxed),
        }
    }
}

pub struct MicProcessor {
    filters: MicFilters,
    effect: Effect,
    meter: std::sync::Arc<MicMeter>,
    rnnoise: Box<DenoiseState<'static>>,
    deep: Option<Box<DeepFilter>>,
    high_pass: Biquad,
    // Conversion vers 48 kHz (sinus cardinal, voir `resample`).
    step: f64,
    pos: f64,
    window: SincWindow,
    /// Passage en mono des micros à plusieurs canaux.
    downmix: Downmix,
    frame_in: Vec<f32>,
    frame_out: Vec<f32>,
    /// Trame d'entrée précédente : RNNoise rend le son avec une trame de
    /// retard, c'est elle qu'on mélange à sa sortie.
    prev_in: Vec<f32>,
    /// Trames d'entrée récentes, au retard de DeepFilterNet : si l'IA rate
    /// une trame, on rend le son d'origine au bon moment (pas de saut).
    dry_hist: VecDeque<Vec<f32>>,
    /// Réduction actuelle de DeepFilterNet (dB) et dernier rapport voix/bruit.
    atten_db: f32,
    lsnr: f32,
    frames: u32,
    /// Trames de silence complet d'affilée (voir `DF_IDLE_FRAMES`).
    silent_frames: u32,
    deep_in: Vec<f32>,
    deep_out: Vec<f32>,
    /// Niveau habituel de ta voix à l'entrée (dBFS, inconnu = NaN) et nombre
    /// de trames de voix déjà mesurées : il règle le plancher de la porte
    /// (voir `NEAR_OPEN_DB`) et le gain d'avant l'IA. Puis ce gain (dB) et
    /// ceux des dernières trames, à retirer à la sortie (voir
    /// `DF_INPUT_TARGET_DB`).
    talker_db: f32,
    talker_frames: u32,
    /// Le niveau de ta voix vient du test de voix (voir `UNSURE_OPEN_DB`).
    talker_sure: bool,
    /// Trames de voix mesurées depuis l'ouverture du micro.
    heard_frames: u32,
    pregain_db: f32,
    pregains: VecDeque<f32>,
    /// Trames en attente : la porte décide sur le son qui entre et
    /// s'applique au son qui sort un peu plus tard.
    delay: VecDeque<Vec<f32>>,
    delay_frames: usize,
    /// Probabilités de voix des dernières trames : le son qui sort a
    /// quelques trames de retard (réduction de bruit + porte), le volume auto
    /// doit juger la trame qui sort, pas celle qui entre.
    voices: VecDeque<f32>,
    voice_lag: usize,
    /// Baisse pendant les pauses (voir `PAUSE_DIP`).
    pause_gain: f32,
    quiet_frames: u32,
    eq_buf: Vec<f32>,
    gain: f32,
    hold: u32,
    leveler: VoiceLeveler,
    limiter: LookaheadLimiter,
    eq: Vec<Biquad>,
    /// Test de voix en cours et sa longueur voulue (en échantillons).
    recording: Option<(Recording, usize)>,
    /// Bruit de fond estimé, en dBFS, et trames fortes d'affilée (mode automatique).
    floor_db: f32,
    onset: u32,
    dither: u32,
}

/// Passage en mono d'un micro à plusieurs canaux. La plupart envoient la
/// même chose partout, et la moyenne suffit. Mais deux cas la mettent en
/// échec :
///
/// - une carte son à deux entrées avec le micro branché sur une seule (très
///   courant avec un micro XLR) : la moyenne divise la voix par deux et y
///   ajoute le souffle de l'entrée vide ;
/// - un micro dont les deux canaux sont en opposition (câble ou adaptateur
///   mal fait) : la moyenne les annule, il ne reste presque plus de voix.
///
/// On écarte donc en douceur un canal presque muet (20 dB sous le plus fort)
/// ou opposé au plus fort, et on fait la moyenne des autres.
struct Downmix {
    channels: usize,
    frame: Vec<f32>,
    /// Puissance de chaque canal et produit avec le canal le plus fort,
    /// lissés sur ~0,2 s.
    power: Vec<f32>,
    cross: Vec<f32>,
    lead: usize,
    /// Part de chaque canal dans le mélange (0–1) : actuelle et voulue.
    weight: Vec<f32>,
    target: Vec<f32>,
    count: u32,
}

/// Lissage des mesures, par trame (~0,2 s à 48 kHz), et pas des décisions.
const DOWNMIX_SMOOTH: f32 = 1.0 / 8192.0;
const DOWNMIX_EVERY: u32 = 1024;
/// En dessous de -70 dBFS sur le canal le plus fort, on ne décide rien.
const DOWNMIX_MIN_POWER: f32 = 1e-7;

impl Downmix {
    fn new(channels: usize) -> Self {
        Self {
            channels,
            frame: Vec::with_capacity(channels),
            power: vec![0.0; channels],
            cross: vec![0.0; channels],
            lead: 0,
            weight: vec![1.0; channels],
            target: vec![1.0; channels],
            count: 0,
        }
    }

    /// Reçoit un échantillon entrelacé ; rend le mono quand la trame est
    /// complète.
    fn push(&mut self, s: f32) -> Option<f32> {
        if self.channels == 1 {
            return Some(s);
        }
        self.frame.push(s);
        if self.frame.len() < self.channels {
            return None;
        }
        let lead = self.frame[self.lead];
        let (mut sum, mut total) = (0.0, 0.0);
        for (c, &x) in self.frame.iter().enumerate() {
            self.power[c] += (x * x - self.power[c]) * DOWNMIX_SMOOTH;
            self.cross[c] += (x * lead - self.cross[c]) * DOWNMIX_SMOOTH;
            // ~10 ms pour écarter ou reprendre un canal : pas de clic.
            self.weight[c] += (self.target[c] - self.weight[c]) * 0.002;
            sum += x * self.weight[c];
            total += self.weight[c];
        }
        self.frame.clear();
        self.count += 1;
        if self.count >= DOWNMIX_EVERY {
            self.count = 0;
            self.decide();
        }
        Some(sum / total.max(1e-3))
    }

    fn decide(&mut self) {
        // Le canal de référence ne change que pour un canal deux fois plus fort.
        let loudest = (0..self.channels).fold(self.lead, |best, c| if self.power[c] > self.power[best] { c } else { best });
        if self.power[loudest] > self.power[self.lead] * 2.0 {
            self.lead = loudest;
            // Les produits étaient mesurés avec l'ancien canal de référence.
            self.cross.iter_mut().for_each(|c| *c = 0.0);
            self.target[loudest] = 1.0;
            return;
        }
        let lead_power = self.power[self.lead];
        if lead_power < DOWNMIX_MIN_POWER {
            return;
        }
        for c in 0..self.channels {
            if c == self.lead {
                self.target[c] = 1.0;
                continue;
            }
            let ratio = self.power[c] / lead_power;
            let correlation = self.cross[c] / (self.power[c] * lead_power).sqrt().max(1e-12);
            if ratio < 0.01 || correlation < -0.5 {
                self.target[c] = 0.0;
            } else if ratio > 0.04 && correlation > 0.0 {
                self.target[c] = 1.0;
            }
        }
    }
}

/// Filtre du 2ᵉ ordre, pour 48 kHz.
pub(crate) struct Biquad {
    b: [f32; 3],
    a: [f32; 2],
    z: [f32; 2],
}

impl Biquad {
    /// Coefficients bruts (formules du « RBJ Audio EQ Cookbook »).
    fn from_raw(b: [f32; 3], a: [f32; 3]) -> Self {
        Self {
            b: [b[0] / a[0], b[1] / a[0], b[2] / a[0]],
            a: [a[1] / a[0], a[2] / a[0]],
            z: [0.0; 2],
        }
    }

    /// Cloche : monte ou baisse une bande autour de `freq`.
    fn peaking(freq: f32, q: f32, gain_db: f32) -> Self {
        let a = 10f32.powf(gain_db / 40.0);
        let w = std::f32::consts::TAU * freq / RATE as f32;
        let alpha = w.sin() / (2.0 * q);
        let cos = w.cos();
        Self::from_raw([1.0 + alpha * a, -2.0 * cos, 1.0 - alpha * a], [1.0 + alpha / a, -2.0 * cos, 1.0 - alpha / a])
    }

    /// Plateau : monte ou baisse tout ce qui est sous (`high` = false) ou
    /// au-dessus de `freq`.
    fn shelf(freq: f32, gain_db: f32, high: bool) -> Self {
        let a = 10f32.powf(gain_db / 40.0);
        let w = std::f32::consts::TAU * freq / RATE as f32;
        let cos = w.cos();
        let k = 2.0 * a.sqrt() * w.sin() / std::f32::consts::SQRT_2;
        let (p, m) = (a + 1.0, a - 1.0);
        let s = if high { 1.0 } else { -1.0 };
        Self::from_raw(
            [a * (p + s * m * cos + k), -2.0 * s * a * (m + s * p * cos), a * (p + s * m * cos - k)],
            [p - s * m * cos + k, 2.0 * s * (m - s * p * cos), p - s * m * cos - k],
        )
    }

    /// Passe-haut de Butterworth.
    pub(crate) fn high_pass(cutoff: f32) -> Self {
        let k = (std::f32::consts::PI * cutoff / RATE as f32).tan();
        let q = std::f32::consts::FRAC_1_SQRT_2;
        let norm = 1.0 / (1.0 + k / q + k * k);
        Self {
            b: [norm, -2.0 * norm, norm],
            a: [2.0 * (k * k - 1.0) * norm, (1.0 - k / q + k * k) * norm],
            z: [0.0; 2],
        }
    }

    /// Passe-bande, gain 1 et sans déphasage au centre.
    pub(crate) fn band_pass(center: f32, q: f32) -> Self {
        let w = std::f32::consts::TAU * center / RATE as f32;
        let alpha = w.sin() / (2.0 * q);
        let norm = 1.0 / (1.0 + alpha);
        Self {
            b: [alpha * norm, 0.0, -alpha * norm],
            a: [-2.0 * w.cos() * norm, (1.0 - alpha) * norm],
            z: [0.0; 2],
        }
    }

    pub(crate) fn run(&mut self, x: f32) -> f32 {
        let y = self.b[0] * x + self.z[0];
        self.z[0] = self.b[1] * x - self.a[0] * y + self.z[1];
        self.z[1] = self.b[2] * x - self.a[1] * y;
        y
    }
}

/// Bandes de l'égaliseur sur mesure : un plateau grave, quatre cloches,
/// un plateau aigu (fréquences centrales en Hz).
pub const EQ_BANDS_HZ: [f32; 6] = [150.0, 300.0, 800.0, 2000.0, 4000.0, 8000.0];
/// Correction maximale d'une bande de l'égaliseur sur mesure, en dB.
pub const EQ_MAX_DB: f32 = 4.0;

/// Les réglages de chaque couleur de voix : des corrections douces (3 dB au
/// plus), comme un ingénieur du son sur une voix parlée.
fn equalizer(eq: VoiceEq, calibration: Option<MicCalibration>) -> Vec<Biquad> {
    match eq {
        VoiceEq::Custom => {
            // Plafonné ici aussi : un test de voix fait avec une ancienne
            // version pouvait demander jusqu'à 8 dB.
            let gains = calibration.map(|c| c.eq_db).unwrap_or_default().map(|g| g.clamp(-EQ_MAX_DB, EQ_MAX_DB));
            let last = EQ_BANDS_HZ.len() - 1;
            EQ_BANDS_HZ
                .iter()
                .zip(gains)
                .enumerate()
                .filter(|(_, (_, gain))| gain.abs() >= 0.1)
                .map(|(i, (&freq, gain))| match i {
                    0 => Biquad::shelf(freq, gain, false),
                    i if i == last => Biquad::shelf(freq, gain, true),
                    _ => Biquad::peaking(freq, 1.4, gain),
                })
                .collect()
        }
        VoiceEq::Natural => Vec::new(),
        VoiceEq::Warm => vec![
            Biquad::shelf(180.0, 3.0, false),
            Biquad::peaking(3500.0, 1.0, -1.5),
            Biquad::shelf(8000.0, -2.0, true),
        ],
        VoiceEq::Clear => vec![
            Biquad::peaking(300.0, 1.0, -2.5),
            Biquad::peaking(3500.0, 1.0, 3.0),
            Biquad::shelf(10_000.0, 2.0, true),
        ],
    }
}

/// DeepFilterNet garde des `Rc` internes et n'est donc pas `Send`, alors que
/// le filtre est créé ici puis déplacé dans le fil du micro.
struct DeepFilter(DfTract);

// SAFETY: le `DfTract` est créé neuf, n'est jamais cloné et tous ses `Rc`
// restent à l'intérieur : il change de fil d'un seul bloc et n'est ensuite
// utilisé que par le fil du micro.
unsafe impl Send for DeepFilter {}

fn load_deep_filter() -> Option<Box<DeepFilter>> {
    let (min, erb, df) = DF_THRESHOLDS;
    let params = RuntimeParams::default_with_ch(1)
        .with_atten_lim(DF_ATTEN_LIMIT_DB)
        .with_post_filter(DF_POST_FILTER)
        .with_thresholds(min, erb, df);
    match DfTract::new(DfParams::default(), &params) {
        Ok(df) if df.hop_size == FRAME && df.sr == RATE as usize => Some(Box::new(DeepFilter(df))),
        Ok(_) => None,
        Err(e) => {
            eprintln!("DeepFilterNet indisponible, RNNoise à la place : {e}");
            None
        }
    }
}

impl MicProcessor {
    pub fn new(
        channels: u16,
        in_rate: u32,
        filters: MicFilters,
        effect: VoiceEffect,
        meter: std::sync::Arc<MicMeter>,
    ) -> Self {
        let deep = (filters.denoise && filters.strong)
            .then(load_deep_filter)
            .flatten();
        // Retard ajouté pour que la porte voie ~30 ms en avance (la première
        // syllabe n'est pas mangée pendant qu'elle décide). RNNoise
        // retarde déjà d'une trame, DeepFilterNet de trois.
        let delay_frames = match (filters.gate, filters.denoise, deep.is_some()) {
            (false, _, _) => 0,
            (true, true, true) => 1,
            (true, true, false) => 2,
            (true, false, _) => 3,
        };
        let inner_lag = match (filters.denoise, deep.is_some()) {
            (false, _) => 0,
            (true, false) => 1,
            (true, true) => 3,
        };
        let voice_lag = inner_lag + delay_frames;
        // Niveau de voix connu d'avance (test de voix) : l'IA est bien
        // réglée dès le premier mot.
        let sure_db = filters.calibration.map(|c| c.speech_db);
        let talker_db = sure_db.or(filters.learned_db).unwrap_or(f32::NAN);
        let pregain_db = if talker_db.is_nan() { 0.0 } else { (DF_INPUT_TARGET_DB - talker_db).clamp(0.0, DF_PREGAIN_MAX_DB) };
        Self {
            filters,
            effect: Effect::new(effect),
            meter,
            rnnoise: DenoiseState::new(),
            deep,
            high_pass: Biquad::high_pass(80.0),
            step: in_rate as f64 / RATE as f64,
            pos: 0.0,
            window: SincWindow::new(Kernel::for_ratio(RATE as f64 / in_rate.max(1) as f64), 1),
            downmix: Downmix::new(channels.max(1) as usize),
            frame_in: Vec::with_capacity(FRAME),
            frame_out: vec![0.0; FRAME],
            prev_in: vec![0.0; FRAME],
            dry_hist: VecDeque::with_capacity(DF_LAG_FRAMES + 1),
            atten_db: DF_ATTEN_LIMIT_DB,
            lsnr: -15.0,
            frames: 0,
            silent_frames: 0,
            deep_in: vec![0.0; FRAME],
            deep_out: vec![0.0; FRAME],
            talker_db,
            talker_frames: if talker_db.is_nan() { 0 } else { 1000 },
            talker_sure: sure_db.is_some(),
            heard_frames: 0,
            pregain_db,
            pregains: VecDeque::with_capacity(DF_LAG_FRAMES + 1),
            delay: VecDeque::with_capacity(delay_frames + 1),
            delay_frames,
            voices: VecDeque::with_capacity(voice_lag + 1),
            voice_lag,
            pause_gain: 1.0,
            quiet_frames: 0,
            eq_buf: Vec::with_capacity(FRAME),
            gain: if filters.gate { 0.0 } else { 1.0 },
            hold: 0,
            leveler: VoiceLeveler::new(sure_db.or(filters.learned_db), sure_db.is_some()),
            limiter: voice_limiter(),
            eq: equalizer(filters.eq, filters.calibration),
            recording: None,
            floor_db: filters.calibration.map_or(f32::NAN, |c| c.noise_db.clamp(-90.0, -20.0)),
            onset: 0,
            dither: 0x2545_f491,
        }
    }

    /// Reçoit des échantillons entrelacés (−1…1) au format du micro et ajoute
    /// à `out` du mono 48 kHz nettoyé.
    pub fn process(&mut self, input: impl Iterator<Item = f32>, out: &mut Vec<f32>) {
        for s in input {
            let Some(mono) = self.downmix.push(s) else { continue };

            if self.step == 1.0 {
                self.push_sample(mono, out);
                continue;
            }
            // Chaque échantillon d'entrée fait avancer de 1 ; on produit les
            // échantillons de sortie qui tombent entre le centre de la
            // fenêtre et le suivant.
            self.window.push(&[mono]);
            let mut v = [0.0];
            while self.pos < 1.0 {
                self.window.read(self.pos, &mut v);
                self.push_sample(v[0], out);
                self.pos += self.step;
            }
            self.pos -= 1.0;
        }
    }

    /// Commence à enregistrer `samples` échantillons (brut et traité).
    pub fn start_recording(&mut self, samples: usize) {
        let empty = || Vec::with_capacity(samples);
        self.recording = Some((Recording { raw: empty(), processed: empty() }, samples));
    }

    /// L'enregistrement, une fois complet.
    pub fn take_recording(&mut self) -> Option<Recording> {
        let full = self
            .recording
            .as_ref()
            .is_some_and(|(r, n)| r.raw.len() >= *n && r.processed.len() >= *n);
        full.then(|| self.recording.take().map(|(r, _)| r)).flatten()
    }

    fn push_sample(&mut self, v: f32, out: &mut Vec<f32>) {
        if let Some((r, n)) = self.recording.as_mut() {
            if r.raw.len() < *n {
                r.raw.push(v);
            }
        }
        // Toujours actif : sans lui, le volume auto amplifie les grondements.
        let v = self.high_pass.run(v);
        self.frame_in.push(v * 32768.0);
        if self.frame_in.len() == FRAME {
            self.flush_frame(out);
        }
    }

    fn flush_frame(&mut self, out: &mut Vec<f32>) {
        let voice = self
            .rnnoise
            .process_frame(&mut self.frame_out, &self.frame_in);
        let input_rms =
            (self.frame_in.iter().map(|s| (s / 32768.0).powi(2)).sum::<f32>() / FRAME as f32).sqrt();

        let in_db = 20.0 * input_rms.max(1e-6).log10();
        let fast_start = self.talker_frames < DF_PREGAIN_FAST_FRAMES;
        if self.track_talker(voice, in_db) {
            // Le volume automatique s'était réglé sur une voix lointaine :
            // il repart de la tienne avant qu'elle ne sorte trop fort.
            self.leveler.relearn(in_db);
        }
        // Assez fort pour être toi (et non une voix au fond de la pièce) :
        // de quoi ouvrir la porte, ou seulement la garder ouverte.
        let strict = if self.filters.far_voices == FarVoices::Strict { NEAR_STRICT_DB } else { 0.0 };
        let near_open = in_db >= self.near_floor_db(NEAR_OPEN_DB - strict);
        // Réglage désactivé : toute voix garde la porte ouverte, comme avant.
        let near = self.filters.far_voices == FarVoices::Off || in_db >= self.near_floor_db(NEAR_KEEP_DB - strict);

        // Gain d'avant l'IA : la voix lui arrive toujours à un niveau normal.
        let (mut pre, mut post) = (1.0, 1.0);
        if self.deep.is_some() && self.filters.denoise {
            if !self.talker_db.is_nan() {
                let wanted = (DF_INPUT_TARGET_DB - self.talker_db).clamp(0.0, DF_PREGAIN_MAX_DB);
                let step = if fast_start { DF_PREGAIN_FAST_STEP_DB } else { DF_PREGAIN_STEP_DB };
                self.pregain_db += (wanted - self.pregain_db).clamp(-step, step);
            }
            pre = 10f32.powf(self.pregain_db / 20.0);
            // L'IA rend le son trois trames plus tard : on retire le gain
            // qui avait été mis à ce moment-là.
            self.pregains.push_back(pre);
            if self.pregains.len() > DF_LAG_FRAMES {
                post = self.pregains.pop_front().unwrap_or(1.0);
            }
        }

        let mut source = if !self.filters.denoise {
            self.frame_in.clone()
        } else if let Some(df) = self.deep.as_mut() {
            for (d, s) in self.deep_in.iter_mut().zip(&self.frame_in) {
                self.dither = self.dither.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let white = (self.dither >> 8) as f32 / (1 << 23) as f32 - 1.0;
                *d = s / 32768.0 * pre + white * DF_DITHER;
            }
            let noisy = ArrayView2::from_shape((1, FRAME), &self.deep_in).expect("trame");
            let enhanced = ArrayViewMut2::from_shape((1, FRAME), &mut self.deep_out).expect("trame");
            self.dry_hist.push_back(self.frame_in.clone());
            let dry = if self.dry_hist.len() > DF_LAG_FRAMES { self.dry_hist.pop_front() } else { None };
            self.silent_frames = if input_rms < DF_IDLE_RMS { self.silent_frames.saturating_add(1) } else { 0 };
            if self.silent_frames > DF_IDLE_FRAMES {
                // Rien à nettoyer : l'IA rendrait le même silence.
                self.lsnr = -15.0;
                vec![0.0; FRAME]
            } else {
                match df.0.process(noisy, enhanced) {
                    Ok(lsnr) => {
                        self.lsnr = lsnr;
                        self.deep_out.iter().map(|s| s * 32768.0 / post).collect()
                    }
                    Err(_) => {
                        self.lsnr = -15.0;
                        dry.unwrap_or_else(|| vec![0.0; FRAME])
                    }
                }
            }
        } else {
            self.frame_out
                .iter()
                .zip(&self.prev_in)
                .map(|(wet, dry)| wet * (1.0 - RNN_DRY) + dry * RNN_DRY)
                .collect()
        };
        self.prev_in.copy_from_slice(&self.frame_in);
        if !(self.filters.gate && self.filters.gate_auto) {
            // La porte automatique suit déjà le bruit de fond ; sinon on le
            // suit ici, pour régler la réduction.
            self.track_floor(voice, in_db);
        }
        self.frames = self.frames.wrapping_add(1);
        if self.frames.is_multiple_of(50) && !self.floor_db.is_nan() {
            if let Some(df) = self.deep.as_mut() {
                // Le souffle anti-trous (-64 dBFS) compte comme du bruit.
                let wanted = (self.floor_db.max(-64.0) - DF_RESIDUAL_DB)
                    .clamp(DF_ATTEN_MIN_DB, DF_ATTEN_MAX_DB)
                    .round();
                if (wanted - self.atten_db).abs() >= 1.0 {
                    self.atten_db = wanted;
                    df.0.set_atten_lim(wanted);
                }
            }
        }
        let ai_voice = near && self.deep.is_some() && self.filters.denoise && self.lsnr >= DF_KEEP_LSNR_DB;

        let gate_open = if !self.filters.gate {
            true
        } else {
            let open = if self.filters.gate_auto {
                self.auto_gate(voice, input_rms, near_open, near) || (self.hold > 0 && ai_voice)
            } else {
                let threshold = self.filters.gate_threshold;
                let opening = self.hold == 0;
                (opening && voice >= threshold && near_open)
                    || (!opening && ((near && voice >= threshold * 0.5) || ai_voice))
            };
            if open {
                self.hold = HOLD_FRAMES;
            } else {
                self.hold = self.hold.saturating_sub(1);
            }
            self.hold > 0
        };
        // Fermée, la porte coupe tout : une porte qui ne ferait que baisser
        // laisserait passer le clavier ou la télé entre deux phrases.
        let target = if gate_open { 1.0 } else { 0.0 };

        if self.delay_frames > 0 {
            self.delay.push_back(source);
            if self.delay.len() <= self.delay_frames {
                source = vec![0.0; FRAME];
            } else {
                source = self.delay.pop_front().unwrap_or_else(|| vec![0.0; FRAME]);
            }
        }

        // Probabilité de voix de la trame qui sort maintenant. Une voix trop
        // lointaine ne compte pas : elle ne règle pas le volume automatique.
        let voice_near = if near { voice } else { 0.0 };
        self.voices.push_back(voice_near);
        while self.voices.len() > self.voice_lag + 1 {
            self.voices.pop_front();
        }
        let voice_out = if self.voices.len() > self.voice_lag { self.voices[0] } else { 0.0 };

        let rms = (source.iter().map(|s| (s / 32768.0).powi(2)).sum::<f32>() / FRAME as f32).sqrt();
        // Échelle en dB ramenée à 0–1 (−60 dB → 0, 0 dB → 1).
        let level = ((20.0 * rms.max(1e-6).log10() + 60.0) / 60.0).clamp(0.0, 1.0);
        self.meter.level.store(level.to_bits(), Ordering::Relaxed);
        self.meter.voice.store(voice.to_bits(), Ordering::Relaxed);
        self.meter.open.store(gate_open, Ordering::Relaxed);

        // Pauses : la voix qui entre (en avance) et celle qui sort sont
        // toutes deux absentes depuis 100 ms, et l'IA n'entend pas de voix
        // non plus (une voix douce dans le bruit ne doit pas baisser).
        let pause = self.filters.level && self.filters.gate;
        if pause && voice_near.max(voice_out) < PAUSE_VOICE && !ai_voice {
            self.quiet_frames = self.quiet_frames.saturating_add(1);
        } else {
            self.quiet_frames = 0;
        }
        let pause_target = if pause && self.quiet_frames >= PAUSE_AFTER_FRAMES { PAUSE_DIP } else { 1.0 };
        let (down, up) = (smoothing(0.15), smoothing(0.01));

        // Porte, baisse des pauses puis égaliseur.
        self.eq_buf.clear();
        let mut dry = [0.0f32; FRAME];
        for (&s, d) in source.iter().zip(dry.iter_mut()) {
            if self.gain < target {
                self.gain = (self.gain + ATTACK).min(target);
            } else if self.gain > target {
                self.gain = (self.gain - RELEASE).max(target);
            }
            let c = if pause_target < self.pause_gain { down } else { up };
            self.pause_gain = pause_target + (self.pause_gain - pause_target) * c;
            let mut v = s / 32768.0 * self.gain * self.pause_gain;
            *d = v;
            for band in &mut self.eq {
                v = band.run(v);
            }
            self.eq_buf.push(v);
        }

        if self.filters.level {
            // Le volume auto mesure la voix telle qu'elle sort de
            // l'égaliseur, seulement quand c'est vraiment de la voix et que
            // la porte est grande ouverte.
            let eq_rms = (self.eq_buf.iter().map(|v| v * v).sum::<f32>() / FRAME as f32).sqrt();
            self.leveler.begin_frame(eq_rms, voice_out >= 0.5 && self.gain >= 0.99);
        }
        for (i, &v) in self.eq_buf.iter().enumerate() {
            let v = if self.filters.level { self.leveler.process(v, dry[i]) } else { v };
            let mut frame = [self.effect.process(v)];
            self.limiter.process(&mut frame);
            out.push(frame[0].clamp(-1.0, 1.0));
        }
        if let Some((r, n)) = self.recording.as_mut() {
            let missing = n.saturating_sub(r.processed.len());
            r.processed.extend(out[out.len() - FRAME..].iter().take(missing));
        }
        self.frame_in.clear();
    }

    /// Comme Discord : on suit le bruit de fond de la pièce (il descend vite
    /// sur les silences et remonte lentement) et on ouvre quand le son le
    /// dépasse nettement et ressemble à de la voix.
    /// `near_open`, `near` : le son est assez fort pour être ta voix (pour
    /// ouvrir, pour rester ouvert), voir `NEAR_OPEN_DB`.
    fn auto_gate(&mut self, voice: f32, input_rms: f32, near_open: bool, near: bool) -> bool {
        let db = 20.0 * input_rms.max(1e-6).log10();
        if self.floor_db.is_nan() {
            self.floor_db = db;
        }
        let above = db - self.floor_db;
        let open = if self.hold == 0 {
            // Deux trames de suite (20 ms) : un clic de clavier n'ouvre pas.
            let loud = near_open && AUTO_OPEN.iter().any(|&(v, db)| voice >= v && above >= db);
            self.onset = if loud { self.onset + 1 } else { 0 };
            self.onset >= 2
        } else {
            near && ((above >= AUTO_KEEP_DB && voice >= 0.1) || voice >= 0.5)
        };
        self.track_floor(voice, db);
        open
    }

    /// Niveau habituel de ta voix, mesuré sur les trames de voix qui dépassent
    /// le bruit de la pièce. Les premières comptent beaucoup (moyenne simple),
    /// puis la mesure devient lente (~3 s). Rend vrai quand une voix bien
    /// plus forte que prévu arrive (voir `TALKER_JUMP_DB`).
    fn track_talker(&mut self, voice: f32, db: f32) -> bool {
        let over_noise = self.floor_db.is_nan() || db >= self.floor_db + 6.0;
        if voice < 0.5 || db < NEAR_FLOOR_MIN_DB || !over_noise {
            return false;
        }
        if self.talker_db.is_nan() {
            self.talker_db = db;
        }
        let gap = db - self.talker_db;
        if gap < -NEAR_KEEP_DB {
            self.talker_db -= TALKER_FAR_STEP_DB;
            return false;
        }
        self.talker_frames = self.talker_frames.saturating_add(1);
        self.heard_frames = self.heard_frames.saturating_add(1);
        let keep = self.heard_frames >= LEARNED_AFTER_FRAMES && self.talker_db >= LEARNED_MIN_DB;
        if keep && self.heard_frames.is_multiple_of(100) {
            self.meter.learned.store(self.talker_db.min(-1.0).to_bits(), Ordering::Relaxed);
        }
        let slow = (1.0 / (self.talker_frames as f32 + 4.0)).max(0.003);
        let jump = gap > TALKER_JUMP_DB;
        self.talker_db += gap * if jump { slow.max(0.05) } else { slow };
        // Au tout début, c'est juste ta première syllabe qui monte : rien à
        // signaler tant qu'on n'a pas une demi-seconde de voix mesurée.
        jump && self.talker_frames > DF_PREGAIN_FAST_FRAMES
    }

    /// Niveau (dBFS) à partir duquel un son est assez fort pour être ta
    /// voix, à `margin` dB sous ton niveau habituel.
    fn near_floor_db(&self, margin: f32) -> f32 {
        if self.talker_db.is_nan() {
            return OPEN_FLOOR_DB;
        }
        let mut floor = self.talker_db - margin;
        if self.filters.far_voices == FarVoices::Off {
            // Jamais plus haut que l'ancien plancher fixe ; plus bas
            // seulement pour un micro très faible.
            floor = floor.min(OPEN_FLOOR_DB);
        }
        if !self.talker_sure {
            let unsure = (self.talker_db - UNSURE_OPEN_DB).min(OPEN_FLOOR_DB) - (margin - NEAR_OPEN_DB);
            floor = floor.max(unsure);
        }
        floor.clamp(NEAR_FLOOR_MIN_DB, NEAR_FLOOR_MAX_DB)
    }

    /// Bruit de fond de la pièce : descend vite sur les silences, remonte
    /// lentement, bouge à peine pendant que tu parles.
    fn track_floor(&mut self, voice: f32, db: f32) {
        if self.floor_db.is_nan() {
            self.floor_db = db;
        }
        let rate = if db < self.floor_db {
            0.1
        } else if voice >= 0.5 {
            // Pendant que tu parles, le bruit de fond bouge à peine.
            0.0005
        } else {
            // ~3 s pour s'adapter à un bruit qui monte (ventilateur, pluie).
            0.003
        };
        self.floor_db = (self.floor_db + (db - self.floor_db) * rate).clamp(-90.0, -20.0);
    }
}

/// Une constante de temps (en secondes) en coefficient de lissage par
/// échantillon, pour 48 kHz.
fn smoothing(seconds: f32) -> f32 {
    (-1.0 / (seconds * RATE as f32)).exp()
}

fn to_db(power: f32) -> f32 {
    10.0 * power.max(1e-12).log10()
}

/// Traitement « studio » de la voix, dans l'ordre d'une console de radio :
///
/// 1. Volume automatique : on mesure le niveau moyen de la parole
///    (seulement quand tu parles) et on règle lentement le gain pour viser
///    -20 dBFS (de -6 à +24 dB). Une voix douce n'arrive plus trop bas, un
///    cri ne sature plus.
/// 2. Compresseur : il mesure l'énergie moyenne (RMS sur 10 ms) et non
///    chaque crête, avec un genou doux autour de -18 dBFS et un rapport
///    2:1. Il resserre la voix sans écraser les voyelles ni faire ressortir
///    les consonnes. Attaque 10 ms, relâche 150 ms, réglés en dB.
/// 3. Dé-esseur : quand l'aigu (au-dessus de 5 kHz) prend le dessus, comme
///    sur les S, CH et Z, on ne baisse que cet aigu, de 6 dB au plus. Le
///    reste de la voix ne bouge pas.
///
/// Le plafond de sécurité est à part (`Limiter`) et toujours actif.
struct VoiceLeveler {
    speech_db: f32,
    /// Trames de voix déjà mesurées (les premières comptent davantage).
    measured: u32,
    /// Niveau (dBFS) sous lequel une trame ne compte pas : -55 sans test de
    /// voix (une voix si faible peut être la télé, qu'il ne faut pas
    /// monter), -70 quand le test a montré que ton micro est faible.
    min_db: f32,
    gain_db: f32,
    /// Gain du début et de la fin de la trame en cours (linéaire).
    from: f32,
    to: f32,
    pos: f32,
    band: Biquad,
    /// Même filtre, sur le son d'avant l'égaliseur : c'est lui qui repère
    /// les S, pour qu'un égaliseur qui remonte les aigus ne déclenche pas
    /// le dé-esseur sur toute la voix.
    side_band: Biquad,
    high_power: f32,
    full_power: f32,
    ess_db: f32,
    power: f32,
    comp_db: f32,
}

const LEVEL_TARGET_DB: f32 = -20.0;
const LEVEL_MIN_DB: f32 = -6.0;
/// 24 dB au plus : avec 18, un micro faible (voix vers -45 dBFS) sortait
/// encore 8 dB sous les autres.
const LEVEL_MAX_DB: f32 = 24.0;
/// Vitesse de la mesure de la voix par le volume automatique, par trame de
/// 10 ms (~3 s).
const LEVEL_TRACK: f32 = 0.003;
/// Le dé-esseur agit quand l'aigu (bande large autour de 7 kHz) dépasse
/// -6 dB de l'énergie totale (une voyelle est vers -25 dB, un S vers -3 dB)
/// et qu'il est audible.
const ESS_CENTER_HZ: f32 = 7000.0;
const ESS_THRESHOLD_DB: f32 = -6.0;
const ESS_FLOOR_DB: f32 = -50.0;
const ESS_MAX_DB: f32 = 6.0;
const COMP_THRESHOLD_DB: f32 = -18.0;
const COMP_KNEE_DB: f32 = 6.0;
const COMP_RATIO: f32 = 2.0;

impl VoiceLeveler {
    /// `speech_db` : niveau de ta voix s'il est déjà connu, pour que le
    /// premier mot sorte déjà au bon volume ; `sure` : il vient du test de
    /// voix, et non de ce qui a été appris aux lancements précédents.
    fn new(speech_db: Option<f32>, sure: bool) -> Self {
        // Test de voix : pas de démarrage rapide. Niveau appris : on s'en
        // écarte en une seconde s'il n'est plus bon (micro changé).
        let measured = match (speech_db, sure) {
            (None, _) => 0,
            (Some(_), false) => 80,
            (Some(_), true) => 1000,
        };
        let min_db = if speech_db.is_some() && sure { -70.0 } else { -55.0 };
        let speech_db = speech_db.unwrap_or(LEVEL_TARGET_DB);
        let gain_db = (LEVEL_TARGET_DB - speech_db).clamp(LEVEL_MIN_DB, LEVEL_MAX_DB);
        let gain = 10f32.powf(gain_db / 20.0);
        Self {
            speech_db,
            measured,
            min_db,
            gain_db,
            from: gain,
            to: gain,
            pos: 0.0,
            band: Biquad::band_pass(ESS_CENTER_HZ, 0.7),
            side_band: Biquad::band_pass(ESS_CENTER_HZ, 0.7),
            high_power: 0.0,
            full_power: 0.0,
            ess_db: 0.0,
            power: 0.0,
            comp_db: 0.0,
        }
    }

    /// Une voix bien plus forte que celle mesurée jusque-là arrive (une
    /// trame à `db` dBFS, donc un peu au-dessus de sa moyenne) : on repart
    /// d'elle, comme au lancement.
    fn relearn(&mut self, db: f32) {
        self.speech_db = self.speech_db.max(db - 4.0);
        self.measured = 0;
    }

    /// À appeler avant chaque trame de 10 ms, avec son niveau (RMS).
    fn begin_frame(&mut self, rms: f32, speaking: bool) {
        let db = 20.0 * rms.max(1e-6).log10();
        if speaking && db > self.min_db {
            // ~3 s : suit ta façon de parler, pas chaque mot. Plus rapide,
            // un rire ou un cri faisait baisser la suite de la phrase (le
            // compresseur et le limiteur s'occupent des éclats eux-mêmes).
            // Sans test de voix, la première seconde de parole règle le
            // volume (moyenne simple) : pas besoin d'attendre 3 s au lancement.
            self.measured = self.measured.saturating_add(1);
            let rate = (1.0 / (self.measured as f32 + 20.0)).max(LEVEL_TRACK);
            self.speech_db += (db - self.speech_db) * rate;
        }
        let wanted = (LEVEL_TARGET_DB - self.speech_db).clamp(LEVEL_MIN_DB, LEVEL_MAX_DB);
        // Le gain bouge en ~0,3 s ; bien plus vite s'il est très au-dessus
        // de ce qu'il faut (réglé sur une voix lointaine, puis tu parles).
        let rate = if wanted < self.gain_db - 6.0 { 0.2 } else { 0.03 };
        self.gain_db += (wanted - self.gain_db) * rate;
        self.from = self.to;
        self.to = 10f32.powf(self.gain_db / 20.0);
        self.pos = 0.0;
    }

    /// `x` : le son à traiter ; `side` : le même avant l'égaliseur, qui
    /// sert seulement à repérer les S.
    fn process(&mut self, x: f32, side: f32) -> f32 {
        self.pos = (self.pos + 1.0 / FRAME as f32).min(1.0);
        let gain = self.from + (self.to - self.from) * self.pos;
        let x = x * gain;
        let side = side * gain;

        // Compresseur RMS à genou doux.
        let c = smoothing(0.01);
        self.power = self.power * c + x * x * (1.0 - c);
        let over = to_db(self.power) - COMP_THRESHOLD_DB;
        let slope = 1.0 / COMP_RATIO - 1.0;
        let wanted = if over <= -COMP_KNEE_DB / 2.0 {
            0.0
        } else if over >= COMP_KNEE_DB / 2.0 {
            over * slope
        } else {
            slope * (over + COMP_KNEE_DB / 2.0).powi(2) / (2.0 * COMP_KNEE_DB)
        };
        let c = if wanted < self.comp_db { smoothing(0.01) } else { smoothing(0.15) };
        self.comp_db = wanted + (self.comp_db - wanted) * c;
        let comp = 10f32.powf(self.comp_db / 20.0);
        let x = x * comp;
        let side = side * comp;

        // Dé-esseur en égaliseur dynamique, après le compresseur comme en
        // studio : la compression fait ressortir les S, on les calme ensuite.
        // On retire une part de la bande aiguë ; sans S, rien n'est retiré.
        let high = self.band.run(x);
        let side_high = self.side_band.run(side);
        let c = smoothing(0.002);
        self.high_power = self.high_power * c + side_high * side_high * (1.0 - c);
        self.full_power = self.full_power * c + side * side * (1.0 - c);
        let high_db = to_db(self.high_power);
        let excess = high_db - to_db(self.full_power) - ESS_THRESHOLD_DB;
        let wanted = if high_db > ESS_FLOOR_DB { excess.clamp(0.0, ESS_MAX_DB) } else { 0.0 };
        let c = if wanted > self.ess_db { smoothing(0.001) } else { smoothing(0.06) };
        self.ess_db = wanted + (self.ess_db - wanted) * c;
        x - high * (1.0 - 10f32.powf(-self.ess_db / 20.0))
    }
}

/// Plafond de la voix : -3 dBFS, 2 ms d'anticipation (voir `dynamics`).
const LIMIT_CEILING: f32 = 0.708;
const LIMIT_LOOKAHEAD: usize = 96;

fn voice_limiter() -> LookaheadLimiter {
    LookaheadLimiter::new(1, LIMIT_LOOKAHEAD, LIMIT_CEILING, smoothing(0.08))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(filters: MicFilters, rate: u32, channels: u16, input: &[f32]) -> Vec<f32> {
        let mut p = MicProcessor::new(channels, rate, filters, VoiceEffect::None, Default::default());
        let mut out = Vec::new();
        p.process(input.iter().copied(), &mut out);
        out
    }

    const OFF: MicFilters = MicFilters {
        denoise: false,
        strong: false,
        gate: false,
        gate_auto: false,
        gate_threshold: 0.5,
        level: false,
        eq: VoiceEq::Natural,
        calibration: None,
        far_voices: FarVoices::Normal,
        learned_db: None,
    };

    #[test]
    fn resamples_to_48k_mono() {
        // 1 s de stéréo 44,1 kHz → ~1 s de mono 48 kHz (à une trame près).
        let input = vec![0.1; 44_100 * 2];
        let out = run(OFF, 44_100, 2, &input);
        assert!((out.len() as i64 - 48_000).abs() <= FRAME as i64, "{}", out.len());
        // Même sans filtre, l'anti-grondement (80 Hz) reste là : un continu
        // disparaît en quelques dizaines de ms.
        assert!(out[out.len() / 2..].iter().all(|v| v.abs() < 1e-3));
    }

    #[test]
    fn resampling_keeps_a_clean_tone() {
        // Un 3 kHz capté à 44,1 kHz ressort propre à 48 kHz : le linéaire y
        // laissait des parasites vers -35 dB, la cubique vers -60 dB.
        let f = 3000.0f64;
        let w = f / 48_000.0 * std::f64::consts::TAU;
        let input: Vec<f32> = (0..44_100).map(|i| (i as f64 * f / 44_100.0 * std::f64::consts::TAU).sin() as f32 * 0.5).collect();
        let out = &run(OFF, 44_100, 1, &input)[4800..40_000];
        // Sinusoïde la plus proche (moindres carrés), puis ce qui reste.
        let (mut ss, mut cc, mut sc, mut vs, mut vc) = (0.0, 0.0, 0.0, 0.0, 0.0);
        for (i, &v) in out.iter().enumerate() {
            let (s, c) = (w * i as f64).sin_cos();
            ss += s * s;
            cc += c * c;
            sc += s * c;
            vs += v as f64 * s;
            vc += v as f64 * c;
        }
        let det = ss * cc - sc * sc;
        let (a, b) = ((vs * cc - vc * sc) / det, (vc * ss - vs * sc) / det);
        let err: f64 = out
            .iter()
            .enumerate()
            .map(|(i, &v)| (v as f64 - a * (w * i as f64).sin() - b * (w * i as f64).cos()).powi(2))
            .sum::<f64>()
            / out.len() as f64;
        let db = 10.0 * (err / ((a * a + b * b) / 2.0)).log10();
        // L'anti-grondement (toujours actif, en f32) laisse un fond vers
        // -89 dB : inaudible.
        assert!(db < -85.0, "parasites {db:.1} dB");
    }

    #[test]
    fn stereo_microphone_with_one_dead_or_inverted_channel_keeps_the_voice() {
        let voice = tone(300.0, 0.1, 48_000);
        let level = |right: &dyn Fn(f32) -> f32| {
            let input: Vec<f32> = voice.iter().flat_map(|&v| [v, right(v)]).collect();
            let out = run(OFF, 48_000, 2, &input);
            20.0 * (rms(&out[24_000..]) / rms(&voice[24_000..])).log10()
        };
        // Les deux canaux pareils : rien ne change.
        assert!(level(&|v| v).abs() < 0.1);
        // Micro sur une seule entrée : avant, -6 dB.
        let dead = level(&|_| 0.0);
        assert!(dead.abs() < 0.3, "canal muet : {dead} dB");
        // Canaux en opposition : avant, silence complet.
        let inverted = level(&|v| -v);
        assert!(inverted.abs() < 0.3, "canaux opposés : {inverted} dB");
        // Deux capsules qui captent la même voix à des niveaux proches : moyenne.
        let pair = level(&|v| v * 0.5);
        assert!((pair + 2.5).abs() < 0.3, "paire : {pair} dB");
    }

    fn noise(n: usize, amp: f32) -> Vec<f32> {
        let mut seed = 7u32;
        (0..n)
            .map(|_| {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                ((seed >> 16) as f32 / 32768.0 - 1.0) * amp
            })
            .collect()
    }

    fn rms(v: &[f32]) -> f32 {
        (v.iter().map(|s| s * s).sum::<f32>() / v.len() as f32).sqrt()
    }

    #[test]
    fn strong_denoise_removes_steady_noise() {
        let input = noise(48_000 * 2, 0.05);
        let filters = MicFilters { denoise: true, strong: true, ..OFF };
        let out = run(filters, 48_000, 1, &input);
        let tail = &out[out.len() / 2..];
        // Au moins 20 dB de moins sur un souffle constant.
        assert!(rms(tail) < rms(&input) * 0.1, "{} vs {}", rms(tail), rms(&input));
    }

    #[test]
    fn light_denoise_is_capped_at_30_db() {
        // Un souffle faible, que RNNoise seul retire presque entièrement.
        let input = noise(48_000 * 2, 0.0005);
        let filters = MicFilters { denoise: true, strong: false, ..OFF };
        let out = run(filters, 48_000, 1, &input);
        let db = 20.0 * (rms(&out[out.len() / 2..]) / rms(&input)).log10();
        assert!(db > -30.0, "{db}");
    }

    #[test]
    fn gate_ignores_faint_input() {
        // Sous le plancher de -55 dBFS, la porte reste fermée quoi qu'il arrive.
        let input = noise(48_000, 0.001);
        let filters = MicFilters { gate: true, gate_threshold: 0.0, ..OFF };
        let out = run(filters, 48_000, 1, &input);
        assert!(out.iter().all(|v| v.abs() < 1e-6));
    }

    #[test]
    fn gate_closes_on_noise() {
        // Souffle constant : la détection automatique l'apprend comme bruit de
        // fond et la porte reste fermée.
        let mut seed = 1u32;
        let noise: Vec<f32> = (0..48_000)
            .map(|_| {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                ((seed >> 16) as f32 / 32768.0 - 1.0) * 0.05
            })
            .collect();
        let gate = MicFilters {
            denoise: true,
            strong: true,
            gate: true,
            gate_auto: true,
            gate_threshold: 0.5,
            level: false,
            eq: VoiceEq::Natural,
            calibration: None,
            far_voices: FarVoices::Normal,
            learned_db: None,
        };
        let out = run(gate, 48_000, 1, &noise);
        let tail = &out[out.len() / 2..];
        let peak = tail.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(peak < 1e-3, "peak {peak}");
    }

    #[test]
    fn auto_gate_ignores_room_noise_and_opens_on_loud_input() {
        // Souffle constant pendant 2 s : la porte apprend le bruit et reste
        // fermée ; il faut nettement plus fort pour l'ouvrir.
        let filters = MicFilters { gate: true, gate_auto: true, ..OFF };
        let mut p = MicProcessor::new(1, 48_000, filters, VoiceEffect::None, Default::default());
        let mut out = Vec::new();
        p.process(noise(48_000 * 2, 0.01).into_iter(), &mut out);
        let tail = &out[out.len() / 2..];
        assert!(tail.iter().all(|v| v.abs() < 1e-3));
        assert!((p.floor_db + 45.0).abs() < 4.0, "floor {}", p.floor_db);
        // Un peu plus fort mais pas franchement de la voix : reste fermé.
        assert!(!p.auto_gate(0.3, 0.012, true, true));
        assert!(!p.auto_gate(0.9, 0.1, true, true));
        assert!(p.auto_gate(0.9, 0.1, true, true));
        // Puis la voix retombe dans le bruit : la porte se referme.
        assert!(!p.auto_gate(0.9, 0.006, true, true));
    }

    #[test]
    fn gate_floor_follows_the_talker() {
        let filters = MicFilters { gate: true, gate_auto: true, ..OFF };
        let new = || MicProcessor::new(1, 48_000, filters, VoiceEffect::None, Default::default());
        // Rien entendu encore : plancher fixe.
        let mut p = new();
        assert_eq!(p.near_floor_db(NEAR_OPEN_DB), OPEN_FLOOR_DB);
        // Voix très faible (-65 dBFS) sans test de voix : toi ou la télé ?
        // Le plancher descend juste assez pour ses syllabes fortes.
        (0..100).for_each(|_| assert!(!p.track_talker(0.9, -65.0)));
        assert!((p.near_floor_db(NEAR_OPEN_DB) + 63.0).abs() < 0.5, "{}", p.near_floor_db(NEAR_OPEN_DB));
        assert!((p.near_floor_db(NEAR_KEEP_DB) + 69.0).abs() < 0.5);
        // Avec le test de voix, on sait que c'est toi : avant, un micro à
        // -57 dBFS n'ouvrait jamais.
        let weak = MicCalibration { noise_db: -80.0, speech_db: -57.0, eq_db: [0.0; 6] };
        let sure = MicProcessor::new(1, 48_000, MicFilters { calibration: Some(weak), ..filters }, VoiceEffect::None, Default::default());
        assert_eq!(sure.near_floor_db(NEAR_OPEN_DB), -69.0);
        // Voix normale à -30 dBFS : une télé à -55 n'ouvre plus, et une
        // minute de télé ne déplace presque pas le plancher.
        let mut p = new();
        (0..300).for_each(|_| { p.track_talker(0.9, -30.0); });
        assert!(p.near_floor_db(NEAR_OPEN_DB) > -45.0);
        (0..3000).for_each(|_| { p.track_talker(0.9, -55.0); });
        let floor = p.near_floor_db(NEAR_OPEN_DB);
        assert!(floor > -50.0, "{floor}");
        // Mais si c'est toi qui parles plus bas (-12 dB), on te suit en
        // quelques secondes.
        (0..600).for_each(|_| { p.track_talker(0.9, -42.0); });
        assert!(p.near_floor_db(NEAR_OPEN_DB) < -50.0);
        // Et une voix bien plus forte est apprise en une demi-seconde.
        assert!(p.track_talker(0.9, -20.0));
        // Sauf au tout début, où ce n'est que la première syllabe qui monte.
        let mut fresh = new();
        assert!(!fresh.track_talker(0.9, -50.0) && !fresh.track_talker(0.9, -25.0));
        (0..50).for_each(|_| { p.track_talker(0.9, -20.0); });
        assert!(p.near_floor_db(NEAR_OPEN_DB) > -55.0, "{}", p.near_floor_db(NEAR_OPEN_DB));
        (0..600).for_each(|_| { p.track_talker(0.9, -20.0); });
        assert!(p.near_floor_db(NEAR_OPEN_DB) > -40.0, "{}", p.near_floor_db(NEAR_OPEN_DB));
        // Le bruit de la pièce pris pour de la voix ne compte pas.
        let mut p = new();
        p.floor_db = -50.0;
        (0..100).for_each(|_| { p.track_talker(0.9, -47.0); });
        assert!(p.talker_db.is_nan());
        // Test de voix fait : plancher connu dès le premier mot.
        let calibration = MicCalibration { noise_db: -70.0, speech_db: -40.0, eq_db: [0.0; 6] };
        let p = MicProcessor::new(1, 48_000, MicFilters { calibration: Some(calibration), ..filters }, VoiceEffect::None, Default::default());
        assert_eq!(p.near_floor_db(NEAR_OPEN_DB), -52.0);
    }

    #[test]
    fn far_voices_setting_and_learned_level() {
        let filters = MicFilters { gate: true, gate_auto: true, ..OFF };
        let with = |f: MicFilters| MicProcessor::new(1, 48_000, f, VoiceEffect::None, Default::default());
        let calibration = MicCalibration { noise_db: -70.0, speech_db: -30.0, eq_db: [0.0; 6] };
        let sure = MicFilters { calibration: Some(calibration), ..filters };
        // Désactivé : l'ancien plancher.
        assert_eq!(with(sure).near_floor_db(NEAR_OPEN_DB), -42.0);
        assert_eq!(with(MicFilters { far_voices: FarVoices::Off, ..sure }).near_floor_db(NEAR_OPEN_DB), OPEN_FLOOR_DB);
        // Désactivé, un micro très faible ouvre quand même.
        let weak = MicCalibration { speech_db: -57.0, ..calibration };
        let off = MicFilters { far_voices: FarVoices::Off, calibration: Some(weak), ..filters };
        assert_eq!(with(off).near_floor_db(NEAR_OPEN_DB), -69.0);
        // Niveau appris au lancement précédent : tout est réglé d'avance.
        let p = with(MicFilters { learned_db: Some(-30.0), denoise: true, strong: true, ..filters });
        assert_eq!(p.near_floor_db(NEAR_OPEN_DB), -42.0);
        assert!((p.leveler.gain_db - 10.0).abs() < 0.01);
        // Le test de voix passe avant.
        let p = with(MicFilters { learned_db: Some(-50.0), ..sure });
        assert_eq!(p.near_floor_db(NEAR_OPEN_DB), -42.0);
        // Le niveau est donné à garder après 3 s de voix, pas avant.
        let meter = std::sync::Arc::new(MicMeter::default());
        let mut p = MicProcessor::new(1, 48_000, filters, VoiceEffect::None, meter.clone());
        (0..250).for_each(|_| { p.track_talker(0.9, -33.0); });
        assert_eq!(meter.learned_db(), None);
        (0..150).for_each(|_| { p.track_talker(0.9, -33.0); });
        assert!((meter.learned_db().unwrap() + 33.0).abs() < 0.5);
        meter.forget();
        assert_eq!(meter.learned_db(), None);
    }

    fn leveled(amp: f32) -> f32 {
        let mut l = VoiceLeveler::new(None, false);
        let mut out = Vec::new();
        // 15 s : le volume automatique prend quelques secondes pour monter.
        for f in 0..1500 {
            let frame: Vec<f32> = (0..FRAME).map(|i| ((f * FRAME + i) as f32 * 0.03).sin() * amp).collect();
            l.begin_frame(rms(&frame), true);
            out.extend(frame.iter().map(|&x| l.process(x, x)));
        }
        rms(&out[out.len() / 2..])
    }

    #[test]
    fn voice_leveler_evens_out_quiet_and_loud_voices() {
        let db = |v: f32| 20.0 * v.log10();
        let quiet = db(leveled(0.02)); // voix à -37 dBFS
        let normal = db(leveled(0.14)); // -20 dBFS
        let loud = db(leveled(0.7)); // -6 dBFS
        assert!(quiet > -24.0 && quiet < -17.0, "quiet {quiet}");
        assert!((normal + 20.0).abs() < 3.0, "normal {normal}");
        assert!(loud < -12.0, "loud {loud}");
        assert!(loud - quiet < 12.0, "écart {}", loud - quiet);
    }

    #[test]
    fn voice_leveler_restarts_from_a_much_louder_voice() {
        // Réglé sur une voix lointaine (-50 dBFS) : gain au maximum. Puis ta
        // voix arrive à -25 dBFS : en 100 ms le gain est déjà redescendu.
        let mut l = VoiceLeveler::new(None, false);
        (0..300).for_each(|_| l.begin_frame(0.0032, true));
        assert!(l.gain_db > 20.0, "{}", l.gain_db);
        l.relearn(-25.0);
        (0..10).for_each(|_| l.begin_frame(0.056, true));
        assert!(l.gain_db < 14.0, "{}", l.gain_db);
    }

    fn tone(freq: f32, amp: f32, n: usize) -> Vec<f32> {
        (0..n).map(|i| (i as f32 * freq / RATE as f32 * std::f32::consts::TAU).sin() * amp).collect()
    }

    #[test]
    fn voice_leveler_does_not_pump_after_a_shout() {
        // Voix normale, 1 s de cri (+14 dB), puis voix normale : une seconde
        // après le cri, le gain a peu bougé (avant : environ -5 dB).
        let mut l = VoiceLeveler::new(None, false);
        let mut gain_after = 0.0;
        for f in 0..700 {
            let amp = if (400..500).contains(&f) { 0.7 } else { 0.14 };
            l.begin_frame(amp / 2f32.sqrt(), true);
            if f == 600 {
                gain_after = l.gain_db;
            }
        }
        assert!(gain_after > -3.5, "gain après le cri : {gain_after} dB");
    }

    #[test]
    fn de_esser_tames_sibilants_only() {
        // Même niveau : un 7 kHz (un S) est baissé, un 1 kHz (une voyelle) non.
        let through = |freq: f32| {
            let mut l = VoiceLeveler::new(None, false);
            let input = tone(freq, 0.05, 48_000);
            let out: Vec<f32> = input.iter().map(|&x| l.process(x, x)).collect();
            20.0 * (rms(&out[24_000..]) / rms(&input[24_000..])).log10()
        };
        let vowel = through(1000.0);
        let sibilant = through(7000.0);
        assert!(vowel.abs() < 0.5, "voyelle {vowel}");
        assert!(sibilant < vowel - 6.0, "S {sibilant} vs {vowel}");
    }

    fn gain_db(bands: &mut [Biquad], freq: f32) -> f32 {
        let input = tone(freq, 0.1, 48_000);
        let out: Vec<f32> = input.iter().map(|&x| bands.iter_mut().fold(x, |v, b| b.run(v))).collect();
        20.0 * (rms(&out[24_000..]) / rms(&input[24_000..])).log10()
    }

    #[test]
    fn equalizer_presets_are_gentle() {
        let mut clear = equalizer(VoiceEq::Clear, None);
        assert!((gain_db(&mut clear, 300.0) + 2.5).abs() < 0.7);
        assert!((gain_db(&mut clear, 3500.0) - 3.0).abs() < 0.7);
        assert!((gain_db(&mut clear, 1000.0)).abs() < 1.5);
        let mut warm = equalizer(VoiceEq::Warm, None);
        assert!((gain_db(&mut warm, 60.0) - 3.0).abs() < 0.5);
        assert!((gain_db(&mut warm, 14_000.0) + 2.0).abs() < 1.0);
        assert!((gain_db(&mut warm, 1000.0)).abs() < 1.0);
    }

    #[test]
    fn custom_equalizer_follows_calibration() {
        // Un ancien test de voix (jusqu'à 8 dB) est ramené à 4 dB au plus.
        let calibration = MicCalibration { noise_db: -70.0, speech_db: -30.0, eq_db: [-6.0, 0.0, 0.0, 3.0, 8.0, 3.0] };
        let mut eq = equalizer(VoiceEq::Custom, Some(calibration));
        assert!((gain_db(&mut eq, 50.0) + 4.0).abs() < 0.5);
        assert!((gain_db(&mut eq, 4000.0) - 4.0).abs() < 1.5, "{}", gain_db(&mut eq, 4000.0));
        assert!(gain_db(&mut eq, 14_000.0) > 2.5);
        // Sans test de voix, rien ne change.
        assert!(equalizer(VoiceEq::Custom, None).is_empty());
    }

    #[test]
    fn deep_filter_never_cuts_quiet_word_endings() {
        // Une fin de mot qui descend à -75 dBFS sur un micro sans bruit de
        // fond : avant, DeepFilterNet rendait un trou de silence total.
        let filters = MicFilters { denoise: true, strong: true, ..OFF };
        let mut input = tone(300.0, 0.03, 24_000);
        input.extend(tone(300.0, 0.0003, 9600));
        input.extend(tone(300.0, 0.03, 24_000));
        let out = run(filters, 48_000, 1, &input);
        let mut run_len = 0;
        let mut longest = 0;
        for v in &out[4800..] {
            run_len = if v.abs() < 1e-9 { run_len + 1 } else { 0 };
            longest = longest.max(run_len);
        }
        assert!(longest < 48, "trou de {longest} échantillons");
    }

    #[test]
    fn deep_filter_rests_on_digital_silence_and_wakes_up() {
        let filters = MicFilters { denoise: true, strong: true, ..OFF };
        let mut p = MicProcessor::new(1, 48_000, filters, VoiceEffect::None, Default::default());
        let mut out = Vec::new();
        p.process(std::iter::repeat_n(0.0, 48_000 * 3), &mut out);
        assert!(p.silent_frames > DF_IDLE_FRAMES, "l'IA tourne encore sur le silence");
        out.clear();
        p.process(tone(300.0, 0.03, 24_000).into_iter(), &mut out);
        assert_eq!(p.silent_frames, 0);
        let rms = |o: &[f32]| (o[9600..].iter().map(|v| v * v).sum::<f32>() / (o.len() - 9600) as f32).sqrt();
        // Même son sur un micro qui n'a jamais été au repos.
        let fresh = run(filters, 48_000, 1, &tone(300.0, 0.03, 24_000));
        assert!(rms(&out) > rms(&fresh) * 0.5, "la voix revient mal après le silence : {} / {}", rms(&out), rms(&fresh));
    }

    #[test]
    fn gain_before_deep_filter_is_undone_after_it() {
        // Micro faible connu par le test de voix : +15 dB avant l'IA.
        let calibration = MicCalibration { noise_db: -70.0, speech_db: -42.0, eq_db: [0.0; 6] };
        let boosted = MicFilters { denoise: true, strong: true, calibration: Some(calibration), ..OFF };
        let mut input = tone(300.0, 0.01, 48_000);
        input.iter_mut().zip(noise(48_000, 0.001)).for_each(|(s, n)| *s += n);
        let mut p = MicProcessor::new(1, 48_000, boosted, VoiceEffect::None, Default::default());
        assert!((p.pregain_db - 15.0).abs() < 0.01);
        let mut out = Vec::new();
        p.process(input.iter().copied(), &mut out);
        // Jamais plus fort qu'à l'entrée : le gain est bien retiré à la
        // sortie. (L'effet sur le premier mot ne se mesure que sur une vraie
        // voix : -23 dB sans ce gain, -2 dB avec, sur un micro à -41 dBFS.)
        let db = 20.0 * (rms(&out[24_000..]) / rms(&input[24_000..])).log10();
        assert!(db < 0.5, "sortie {db} dB au-dessus de l'entrée");
    }

    #[test]
    fn deep_filter_reduction_follows_room_noise() {
        let filters = MicFilters { denoise: true, strong: true, ..OFF };
        let atten = |amp: f32| {
            let mut p = MicProcessor::new(1, 48_000, filters, VoiceEffect::None, Default::default());
            let mut out = Vec::new();
            p.process(noise(48_000 * 3, amp).into_iter(), &mut out);
            p.atten_db
        };
        // Micro muet (déjà filtré) : le souffle anti-trous suffit, 21 dB.
        assert_eq!(atten(0.0), 21.0);
        // Pièce bruyante (-45 dBFS) : réduction maximale.
        assert_eq!(atten(0.01), DF_ATTEN_MAX_DB);
    }

    #[test]
    fn records_raw_and_processed() {
        let filters = MicFilters { level: true, ..OFF };
        let mut p = MicProcessor::new(1, 48_000, filters, VoiceEffect::None, Default::default());
        p.start_recording(4800);
        let mut out = Vec::new();
        p.process(noise(2400, 0.1).into_iter(), &mut out);
        assert!(p.take_recording().is_none());
        p.process(noise(9600, 0.1).into_iter(), &mut out);
        let r = p.take_recording().expect("enregistrement");
        assert_eq!((r.raw.len(), r.processed.len()), (4800, 4800));
        assert!(p.take_recording().is_none());
    }

    #[test]
    fn limiter_never_exceeds_ceiling_and_leaves_quiet_sound_alone() {
        let mut l = voice_limiter();
        let mut input = tone(300.0, 0.3, 9600);
        input.extend(tone(300.0, 1.4, 4800));
        input.extend(tone(300.0, 0.3, 48_000));
        let out: Vec<f32> = input
            .iter()
            .map(|&x| {
                let mut f = [x];
                l.process(&mut f);
                f[0]
            })
            .collect();
        let peak = out.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(peak <= LIMIT_CEILING + 1e-4, "peak {peak}");
        // Le calme d'avant ressort intact (à 2 ms de retard près).
        for i in 0..9000 {
            assert!((out[i + LIMIT_LOOKAHEAD] - input[i]).abs() < 1e-6);
        }
        // Et le gain revient après l'éclat.
        let tail = &out[out.len() - 4800..];
        assert!((rms(tail) - 0.3 / 2f32.sqrt()).abs() < 0.01, "{}", rms(tail));
    }
}
