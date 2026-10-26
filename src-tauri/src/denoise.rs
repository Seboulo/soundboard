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

use crate::config::{MicCalibration, VoiceEffect, VoiceEq};
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
/// En dessous de -55 dBFS, le micro ne capte rien d'assez fort pour ouvrir
/// la porte, même si ça ressemble à de la voix.
const OPEN_FLOOR: f32 = 0.0018;
/// Réduction maximale de DeepFilterNet. Plus fort, il mange les fins de mots
/// et les syllabes douces (micro-coupures) ; 30 dB suffisent avec la porte.
const DF_ATTEN_LIMIT_DB: f32 = 30.0;
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
/// Détection automatique : plus RNNoise est sûr que c'est de la voix, moins
/// elle a besoin de dépasser le bruit de fond. Une voix douce dans une pièce
/// bruyante ne dépasse souvent le bruit que de 6 à 9 dB : exiger 10 dB
/// coupait des phrases entières.
const AUTO_OPEN: [(f32, f32); 3] = [(0.9, 3.0), (0.5, 6.0), (0.2, 10.0)];
/// Pour rester ouvert : un peu au-dessus du bruit, ou de la voix probable.
const AUTO_KEEP_DB: f32 = 2.0;
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
    channels: usize,
    rnnoise: Box<DenoiseState<'static>>,
    deep: Option<Box<DeepFilter>>,
    high_pass: Biquad,
    // Conversion vers 48 kHz (sinus cardinal, voir `resample`).
    step: f64,
    pos: f64,
    window: SincWindow,
    // Downmix d'une trame entrelacée en cours.
    acc: f32,
    acc_n: usize,
    frame_in: Vec<f32>,
    frame_out: Vec<f32>,
    deep_in: Vec<f32>,
    deep_out: Vec<f32>,
    /// Trames en attente : la porte décide sur le son qui entre et
    /// s'applique au son qui sort un peu plus tard.
    delay: VecDeque<Vec<f32>>,
    delay_frames: usize,
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

/// Les réglages de chaque couleur de voix : des corrections douces (3 dB au
/// plus), comme un ingénieur du son sur une voix parlée.
fn equalizer(eq: VoiceEq, calibration: Option<MicCalibration>) -> Vec<Biquad> {
    match eq {
        VoiceEq::Custom => {
            let gains = calibration.map(|c| c.eq_db).unwrap_or_default();
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
        Self {
            filters,
            effect: Effect::new(effect),
            meter,
            channels: channels.max(1) as usize,
            rnnoise: DenoiseState::new(),
            deep,
            high_pass: Biquad::high_pass(80.0),
            step: in_rate as f64 / RATE as f64,
            pos: 0.0,
            window: SincWindow::new(Kernel::for_ratio(RATE as f64 / in_rate.max(1) as f64), 1),
            acc: 0.0,
            acc_n: 0,
            frame_in: Vec::with_capacity(FRAME),
            frame_out: vec![0.0; FRAME],
            deep_in: vec![0.0; FRAME],
            deep_out: vec![0.0; FRAME],
            delay: VecDeque::with_capacity(delay_frames + 1),
            delay_frames,
            gain: if filters.gate { 0.0 } else { 1.0 },
            hold: 0,
            leveler: VoiceLeveler::new(filters.calibration.map(|c| c.speech_db)),
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
            self.acc += s;
            self.acc_n += 1;
            if self.acc_n < self.channels {
                continue;
            }
            let mono = self.acc / self.channels as f32;
            self.acc = 0.0;
            self.acc_n = 0;

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
        let v = if self.filters.denoise { self.high_pass.run(v) } else { v };
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

        let mut source = if !self.filters.denoise {
            self.frame_in.clone()
        } else if let Some(df) = self.deep.as_mut() {
            for (d, s) in self.deep_in.iter_mut().zip(&self.frame_in) {
                self.dither = self.dither.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let white = (self.dither >> 8) as f32 / (1 << 23) as f32 - 1.0;
                *d = s / 32768.0 + white * DF_DITHER;
            }
            let noisy = ArrayView2::from_shape((1, FRAME), &self.deep_in).expect("trame");
            let enhanced = ArrayViewMut2::from_shape((1, FRAME), &mut self.deep_out).expect("trame");
            match df.0.process(noisy, enhanced) {
                Ok(_) => self.deep_out.iter().map(|s| s * 32768.0).collect(),
                Err(_) => self.frame_out.clone(),
            }
        } else {
            self.frame_out.clone()
        };

        let target = if !self.filters.gate {
            1.0
        } else {
            let open = if self.filters.gate_auto {
                self.auto_gate(voice, input_rms)
            } else {
                let threshold = self.filters.gate_threshold;
                let opening = self.hold == 0;
                (opening && voice >= threshold && input_rms >= OPEN_FLOOR)
                    || (!opening && voice >= threshold * 0.5)
            };
            if open {
                self.hold = HOLD_FRAMES;
            } else {
                self.hold = self.hold.saturating_sub(1);
            }
            if self.hold > 0 {
                1.0
            } else {
                0.0
            }
        };

        if self.delay_frames > 0 {
            self.delay.push_back(source);
            if self.delay.len() <= self.delay_frames {
                source = vec![0.0; FRAME];
            } else {
                source = self.delay.pop_front().unwrap_or_else(|| vec![0.0; FRAME]);
            }
        }

        let rms = (source.iter().map(|s| (s / 32768.0).powi(2)).sum::<f32>() / FRAME as f32).sqrt();
        // Échelle en dB ramenée à 0–1 (−60 dB → 0, 0 dB → 1).
        let level = ((20.0 * rms.max(1e-6).log10() + 60.0) / 60.0).clamp(0.0, 1.0);
        self.meter.level.store(level.to_bits(), Ordering::Relaxed);
        self.meter.voice.store(voice.to_bits(), Ordering::Relaxed);
        self.meter.open.store(target > 0.0, Ordering::Relaxed);

        if self.filters.level {
            self.leveler.begin_frame(rms, voice >= 0.5 && target > 0.0);
        }
        for &s in source.iter() {
            if self.gain < target {
                self.gain = (self.gain + ATTACK).min(target);
            } else if self.gain > target {
                self.gain = (self.gain - RELEASE).max(target);
            }
            let mut v = s / 32768.0 * self.gain;
            for band in &mut self.eq {
                v = band.run(v);
            }
            if self.filters.level {
                v = self.leveler.process(v);
            }
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
    fn auto_gate(&mut self, voice: f32, input_rms: f32) -> bool {
        let db = 20.0 * input_rms.max(1e-6).log10();
        if self.floor_db.is_nan() {
            self.floor_db = db;
        }
        let above = db - self.floor_db;
        let open = if self.hold == 0 {
            // Deux trames de suite (20 ms) : un clic de clavier n'ouvre pas.
            let loud = input_rms >= OPEN_FLOOR && AUTO_OPEN.iter().any(|&(v, db)| voice >= v && above >= db);
            self.onset = if loud { self.onset + 1 } else { 0 };
            self.onset >= 2
        } else {
            (above >= AUTO_KEEP_DB && voice >= 0.1) || voice >= 0.5
        };
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
        open
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
///    -20 dBFS (de -6 à +18 dB). Une voix douce n'arrive plus trop bas, un
///    cri ne sature plus.
/// 2. Dé-esseur : quand l'aigu (au-dessus de 5 kHz) prend le dessus, comme
///    sur les S, CH et Z, on ne baisse que cet aigu, de 6 dB au plus. Le
///    reste de la voix ne bouge pas.
/// 3. Compresseur : il mesure l'énergie moyenne (RMS sur 10 ms) et non
///    chaque crête, avec un genou doux autour de -18 dBFS et un rapport
///    2:1. Il resserre la voix sans écraser les voyelles ni faire ressortir
///    les consonnes. Attaque 10 ms, relâche 150 ms, réglés en dB.
///
/// Le plafond de sécurité est à part (`Limiter`) et toujours actif.
struct VoiceLeveler {
    speech_db: f32,
    gain_db: f32,
    /// Gain du début et de la fin de la trame en cours (linéaire).
    from: f32,
    to: f32,
    pos: f32,
    band: Biquad,
    high_power: f32,
    full_power: f32,
    ess_db: f32,
    power: f32,
    comp_db: f32,
}

const LEVEL_TARGET_DB: f32 = -20.0;
const LEVEL_MIN_DB: f32 = -6.0;
const LEVEL_MAX_DB: f32 = 18.0;
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
    /// `speech_db` : niveau de ta voix s'il est déjà connu (test de voix),
    /// pour que le premier mot sorte déjà au bon volume.
    fn new(speech_db: Option<f32>) -> Self {
        let speech_db = speech_db.unwrap_or(LEVEL_TARGET_DB);
        let gain_db = (LEVEL_TARGET_DB - speech_db).clamp(LEVEL_MIN_DB, LEVEL_MAX_DB);
        let gain = 10f32.powf(gain_db / 20.0);
        Self {
            speech_db,
            gain_db,
            from: gain,
            to: gain,
            pos: 0.0,
            band: Biquad::band_pass(ESS_CENTER_HZ, 0.7),
            high_power: 0.0,
            full_power: 0.0,
            ess_db: 0.0,
            power: 0.0,
            comp_db: 0.0,
        }
    }

    /// À appeler avant chaque trame de 10 ms, avec son niveau (RMS).
    fn begin_frame(&mut self, rms: f32, speaking: bool) {
        let db = 20.0 * rms.max(1e-6).log10();
        if speaking && db > -55.0 {
            // ~0,5 s : suit ta façon de parler, pas chaque syllabe.
            self.speech_db += (db - self.speech_db) * 0.02;
        }
        let wanted = (LEVEL_TARGET_DB - self.speech_db).clamp(LEVEL_MIN_DB, LEVEL_MAX_DB);
        self.gain_db += (wanted - self.gain_db) * 0.03;
        self.from = self.to;
        self.to = 10f32.powf(self.gain_db / 20.0);
        self.pos = 0.0;
    }

    fn process(&mut self, x: f32) -> f32 {
        self.pos = (self.pos + 1.0 / FRAME as f32).min(1.0);
        let x = x * (self.from + (self.to - self.from) * self.pos);

        // Dé-esseur en égaliseur dynamique : on retire une part de la bande
        // aiguë. Sans S, rien n'est retiré et le son passe intact.
        let high = self.band.run(x);
        let c = smoothing(0.002);
        self.high_power = self.high_power * c + high * high * (1.0 - c);
        self.full_power = self.full_power * c + x * x * (1.0 - c);
        let high_db = to_db(self.high_power);
        let excess = high_db - to_db(self.full_power) - ESS_THRESHOLD_DB;
        let wanted = if high_db > ESS_FLOOR_DB { excess.clamp(0.0, ESS_MAX_DB) } else { 0.0 };
        let c = if wanted > self.ess_db { smoothing(0.001) } else { smoothing(0.06) };
        self.ess_db = wanted + (self.ess_db - wanted) * c;
        let y = x - high * (1.0 - 10f32.powf(-self.ess_db / 20.0));

        // Compresseur RMS à genou doux.
        let c = smoothing(0.01);
        self.power = self.power * c + y * y * (1.0 - c);
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
        y * 10f32.powf(self.comp_db / 20.0)
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
    };

    #[test]
    fn resamples_to_48k_mono() {
        // 1 s de stéréo 44,1 kHz → ~1 s de mono 48 kHz (à une trame près).
        let input = vec![0.1; 44_100 * 2];
        let out = run(OFF, 44_100, 2, &input);
        assert!((out.len() as i64 - 48_000).abs() <= FRAME as i64, "{}", out.len());
        // Sans filtre, le signal passe tel quel (hors tout début interpolé depuis 0).
        assert!(out[FRAME..].iter().all(|v| (v - 0.1).abs() < 1e-4));
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
        assert!(db < -90.0, "parasites {db:.1} dB");
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
        assert!(!p.auto_gate(0.3, 0.012));
        assert!(!p.auto_gate(0.9, 0.1));
        assert!(p.auto_gate(0.9, 0.1));
        // Puis la voix retombe dans le bruit : la porte se referme.
        assert!(!p.auto_gate(0.9, 0.006));
    }

    fn leveled(amp: f32) -> f32 {
        let mut l = VoiceLeveler::new(None);
        let mut out = Vec::new();
        for f in 0..500 {
            let frame: Vec<f32> = (0..FRAME).map(|i| ((f * FRAME + i) as f32 * 0.03).sin() * amp).collect();
            l.begin_frame(rms(&frame), true);
            out.extend(frame.iter().map(|&x| l.process(x)));
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

    fn tone(freq: f32, amp: f32, n: usize) -> Vec<f32> {
        (0..n).map(|i| (i as f32 * freq / RATE as f32 * std::f32::consts::TAU).sin() * amp).collect()
    }

    #[test]
    fn de_esser_tames_sibilants_only() {
        // Même niveau : un 7 kHz (un S) est baissé, un 1 kHz (une voyelle) non.
        let through = |freq: f32| {
            let mut l = VoiceLeveler::new(None);
            let input = tone(freq, 0.05, 48_000);
            let out: Vec<f32> = input.iter().map(|&x| l.process(x)).collect();
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
        let calibration = MicCalibration { noise_db: -70.0, speech_db: -30.0, eq_db: [-6.0, 0.0, 0.0, 3.0, 8.0, 6.0] };
        let mut eq = equalizer(VoiceEq::Custom, Some(calibration));
        assert!(gain_db(&mut eq, 50.0) < -5.0);
        assert!((gain_db(&mut eq, 4000.0) - 8.0).abs() < 2.0, "{}", gain_db(&mut eq, 4000.0));
        assert!(gain_db(&mut eq, 14_000.0) > 5.0);
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
