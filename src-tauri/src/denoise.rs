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

use crate::config::VoiceEffect;
use crate::effects::Effect;
use df::tract::{DfParams, DfTract, RuntimeParams};
use ndarray::{ArrayView2, ArrayViewMut2};
use nnnoiseless::DenoiseState;
use serde::Serialize;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// RNNoise travaille en mono, 48 kHz, par trames de 10 ms.
pub const RATE: u32 = 48_000;
const FRAME: usize = DenoiseState::FRAME_SIZE;
/// Le micro reste ouvert 350 ms après la dernière trame de voix.
const HOLD_FRAMES: u32 = 35;
/// En dessous de -55 dBFS, le micro ne capte rien d'assez fort pour ouvrir
/// la porte, même si ça ressemble à de la voix.
const OPEN_FLOOR: f32 = 0.0018;
/// Réduction maximale de DeepFilterNet. Plus fort, il mange les fins de mots
/// et les syllabes douces (micro-coupures) ; 30 dB suffisent avec la porte.
const DF_ATTEN_LIMIT_DB: f32 = 30.0;
/// Détection automatique : la voix doit dépasser le bruit de fond de 10 dB
/// pour ouvrir, et de 5 dB pour garder ouvert.
const AUTO_OPEN_DB: f32 = 10.0;
const AUTO_KEEP_DB: f32 = 5.0;
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
    high_pass: HighPass,
    // Rééchantillonnage linéaire vers 48 kHz.
    step: f64,
    pos: f64,
    prev: f32,
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
    /// Bruit de fond estimé, en dBFS, et trames fortes d'affilée (mode automatique).
    floor_db: f32,
    onset: u32,
}

/// Passe-haut de Butterworth (2ᵉ ordre) à 80 Hz, pour 48 kHz.
struct HighPass {
    b: [f32; 3],
    a: [f32; 2],
    z: [f32; 2],
}

impl HighPass {
    fn new(cutoff: f32) -> Self {
        let k = (std::f32::consts::PI * cutoff / RATE as f32).tan();
        let q = std::f32::consts::FRAC_1_SQRT_2;
        let norm = 1.0 / (1.0 + k / q + k * k);
        Self {
            b: [norm, -2.0 * norm, norm],
            a: [2.0 * (k * k - 1.0) * norm, (1.0 - k / q + k * k) * norm],
            z: [0.0; 2],
        }
    }

    fn run(&mut self, x: f32) -> f32 {
        let y = self.b[0] * x + self.z[0];
        self.z[0] = self.b[1] * x - self.a[0] * y + self.z[1];
        self.z[1] = self.b[2] * x - self.a[1] * y;
        y
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
    let params = RuntimeParams::default_with_ch(1).with_atten_lim(DF_ATTEN_LIMIT_DB);
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
        // Retard ajouté pour que la porte voie ~20 ms en avance. RNNoise
        // retarde déjà d'une trame, DeepFilterNet de trois.
        let delay_frames = match (filters.gate, filters.denoise, deep.is_some()) {
            (false, _, _) | (true, true, true) => 0,
            (true, true, false) => 1,
            (true, false, _) => 2,
        };
        Self {
            filters,
            effect: Effect::new(effect),
            meter,
            channels: channels.max(1) as usize,
            rnnoise: DenoiseState::new(),
            deep,
            high_pass: HighPass::new(80.0),
            step: in_rate as f64 / RATE as f64,
            pos: 0.0,
            prev: 0.0,
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
            leveler: VoiceLeveler::new(),
            floor_db: f32::NAN,
            onset: 0,
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

            // Chaque échantillon d'entrée fait avancer de 1 ; on produit les
            // échantillons de sortie qui tombent entre le précédent et lui.
            while self.pos <= 1.0 {
                let v = self.prev + (mono - self.prev) * self.pos as f32;
                let v = if self.filters.denoise { self.high_pass.run(v) } else { v };
                self.frame_in.push(v * 32768.0);
                if self.frame_in.len() == FRAME {
                    self.flush_frame(out);
                }
                self.pos += self.step;
            }
            self.pos -= 1.0;
            self.prev = mono;
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
                *d = s / 32768.0;
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
            if self.filters.level {
                v = self.leveler.process(v);
            }
            let v = self.effect.process(v);
            out.push(v.clamp(-1.0, 1.0));
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
            let loud = above >= AUTO_OPEN_DB && voice >= 0.2 && input_rms >= OPEN_FLOOR;
            self.onset = if loud { self.onset + 1 } else { 0 };
            self.onset >= 2
        } else {
            above >= AUTO_KEEP_DB && voice >= 0.1
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

/// Volume de voix automatique.
///
/// Sans lui, une voix douce arrive trop bas dans Discord et un cri sature.
/// On mesure le niveau moyen de la parole (seulement quand tu parles), on
/// règle lentement le gain pour viser -20 dBFS (de -6 à +12 dB), puis un
/// compresseur doux (2:1 au-dessus de -14 dBFS) et un plafond à -3 dBFS
/// rattrapent les éclats de voix.
struct VoiceLeveler {
    speech_db: f32,
    gain_db: f32,
    /// Gain du début et de la fin de la trame en cours (linéaire).
    from: f32,
    to: f32,
    pos: f32,
    env: f32,
    reduce: f32,
}

const LEVEL_TARGET_DB: f32 = -20.0;
const LEVEL_MIN_DB: f32 = -6.0;
const LEVEL_MAX_DB: f32 = 12.0;
const VOICE_COMP_THRESHOLD: f32 = 0.2;
const VOICE_CEILING: f32 = 0.708;

impl VoiceLeveler {
    fn new() -> Self {
        Self {
            speech_db: LEVEL_TARGET_DB,
            gain_db: 0.0,
            from: 1.0,
            to: 1.0,
            pos: 0.0,
            env: 0.0,
            reduce: 1.0,
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
        let y = x * (self.from + (self.to - self.from) * self.pos);
        let level = y.abs();
        // Enveloppe : monte tout de suite, redescend en ~100 ms.
        self.env = if level > self.env { level } else { level + (self.env - level) * 0.9998 };
        let wanted = if self.env > VOICE_COMP_THRESHOLD {
            (self.env / VOICE_COMP_THRESHOLD).powf(-0.5)
        } else {
            1.0
        };
        // Attaque ~3 ms, relâche ~150 ms.
        let coef = if wanted < self.reduce { 0.993 } else { 0.99986 };
        self.reduce = wanted + (self.reduce - wanted) * coef;
        y * self.reduce.min(VOICE_CEILING / self.env.max(1e-6))
    }
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
        assert!(!p.auto_gate(0.9, 0.012));
        assert!(!p.auto_gate(0.9, 0.1));
        assert!(p.auto_gate(0.9, 0.1));
        // Puis la voix retombe dans le bruit : la porte se referme.
        assert!(!p.auto_gate(0.9, 0.006));
    }

    fn leveled(amp: f32) -> f32 {
        let mut l = VoiceLeveler::new();
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
        assert!(quiet > -28.0 && quiet < -22.0, "quiet {quiet}");
        assert!((normal + 20.0).abs() < 3.0, "normal {normal}");
        assert!(loud < -12.0, "loud {loud}");
        assert!(loud - quiet < 12.0, "écart {}", loud - quiet);
    }
}
