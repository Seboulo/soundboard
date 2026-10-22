//! Nettoyage du micro avant de l'envoyer dans le câble virtuel.
//!
//! Discord ne sait pas faire la différence entre ta voix et les sons de la
//! soundboard (ils arrivent par le même micro virtuel) : sa suppression de
//! bruit coupe donc aussi les sons. On nettoie ici le micro seul, avant le
//! mélange, et Discord peut garder ses filtres désactivés.
//!
//! - Réduction de bruit : RNNoise (via `nnnoiseless`), qui enlève clavier,
//!   souris, ventilateur…
//! - Porte de voix : RNNoise donne aussi la probabilité qu'une trame soit de
//!   la voix ; en dessous du seuil, le micro est coupé (après un court délai
//!   pour ne pas hacher la fin des mots).

use crate::config::VoiceEffect;
use crate::effects::Effect;
use nnnoiseless::DenoiseState;
use serde::Serialize;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// RNNoise travaille en mono, 48 kHz, par trames de 10 ms.
pub const RATE: u32 = 48_000;
const FRAME: usize = DenoiseState::FRAME_SIZE;
/// Le micro reste ouvert 300 ms après la dernière trame de voix.
const HOLD_FRAMES: u32 = 30;
/// Durée des fondus d'ouverture et de fermeture, en échantillons.
const ATTACK: f32 = 1.0 / (RATE as f32 * 0.005);
const RELEASE: f32 = 1.0 / (RATE as f32 * 0.08);

#[derive(Debug, Clone, Copy)]
pub struct MicFilters {
    pub denoise: bool,
    pub gate: bool,
    /// Probabilité de voix (0–1) en dessous de laquelle la porte se ferme.
    pub gate_threshold: f32,
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
    // Rééchantillonnage linéaire vers 48 kHz.
    step: f64,
    pos: f64,
    prev: f32,
    // Downmix d'une trame entrelacée en cours.
    acc: f32,
    acc_n: usize,
    frame_in: Vec<f32>,
    frame_out: Vec<f32>,
    gain: f32,
    hold: u32,
}

impl MicProcessor {
    pub fn new(
        channels: u16,
        in_rate: u32,
        filters: MicFilters,
        effect: VoiceEffect,
        meter: std::sync::Arc<MicMeter>,
    ) -> Self {
        Self {
            filters,
            effect: Effect::new(effect),
            meter,
            channels: channels.max(1) as usize,
            rnnoise: DenoiseState::new(),
            step: in_rate as f64 / RATE as f64,
            pos: 0.0,
            prev: 0.0,
            acc: 0.0,
            acc_n: 0,
            frame_in: Vec::with_capacity(FRAME),
            frame_out: vec![0.0; FRAME],
            gain: if filters.gate { 0.0 } else { 1.0 },
            hold: 0,
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
        let source = if self.filters.denoise {
            &self.frame_out
        } else {
            &self.frame_in
        };

        let target = if !self.filters.gate {
            1.0
        } else {
            if voice >= self.filters.gate_threshold {
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

        let rms = (source.iter().map(|s| (s / 32768.0).powi(2)).sum::<f32>() / FRAME as f32).sqrt();
        // Échelle en dB ramenée à 0–1 (−60 dB → 0, 0 dB → 1).
        let level = ((20.0 * rms.max(1e-6).log10() + 60.0) / 60.0).clamp(0.0, 1.0);
        self.meter.level.store(level.to_bits(), Ordering::Relaxed);
        self.meter.voice.store(voice.to_bits(), Ordering::Relaxed);
        self.meter.open.store(target > 0.0, Ordering::Relaxed);

        for &s in source.iter() {
            if self.gain < target {
                self.gain = (self.gain + ATTACK).min(target);
            } else if self.gain > target {
                self.gain = (self.gain - RELEASE).max(target);
            }
            let v = self.effect.process(s / 32768.0 * self.gain);
            out.push(v.clamp(-1.0, 1.0));
        }
        self.frame_in.clear();
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
        gate: false,
        gate_threshold: 0.5,
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
    fn gate_closes_on_noise() {
        // Bruit blanc faible : pas de la voix, la porte doit rester fermée.
        let mut seed = 1u32;
        let noise: Vec<f32> = (0..48_000)
            .map(|_| {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                ((seed >> 16) as f32 / 32768.0 - 1.0) * 0.05
            })
            .collect();
        let gate = MicFilters {
            denoise: true,
            gate: true,
            gate_threshold: 0.5,
        };
        let out = run(gate, 48_000, 1, &noise);
        let tail = &out[out.len() / 2..];
        let peak = tail.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(peak < 1e-3, "peak {peak}");
    }
}
