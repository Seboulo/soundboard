//! Effets sur la voix (micro seulement), appliqués en mono 48 kHz.

use crate::config::VoiceEffect;
use std::f32::consts::PI;

const RATE: f32 = crate::denoise::RATE as f32;
/// Tampon circulaire partagé par les effets à retard (puissance de 2).
const BUF: usize = 1 << 15;

pub struct Effect {
    kind: VoiceEffect,
    buf: Vec<f32>,
    write: usize,
    /// Phase de l'oscillateur (robot) ou de la fenêtre (changement de hauteur).
    phase: f32,
}

impl Effect {
    pub fn new(kind: VoiceEffect) -> Self {
        Self {
            kind,
            buf: if kind == VoiceEffect::None { vec![] } else { vec![0.0; BUF] },
            write: 0,
            phase: 0.0,
        }
    }

    pub fn process(&mut self, x: f32) -> f32 {
        match self.kind {
            VoiceEffect::None => x,
            VoiceEffect::Robot => self.robot(x),
            VoiceEffect::High => self.pitch(x, 1.45),
            VoiceEffect::Low => self.pitch(x, 0.72),
            VoiceEffect::Echo => self.echo(x),
        }
    }

    /// Modulation en anneau : la voix devient métallique.
    fn robot(&mut self, x: f32) -> f32 {
        self.phase = (self.phase + 55.0 / RATE) % 1.0;
        x * (2.0 * PI * self.phase).sin() * 1.4
    }

    /// Écho : 250 ms de retard, un peu de réinjection.
    fn echo(&mut self, x: f32) -> f32 {
        const DELAY: usize = (RATE * 0.25) as usize;
        let delayed = self.buf[(self.write + BUF - DELAY) % BUF];
        let y = x + delayed * 0.45;
        self.buf[self.write] = y;
        self.write = (self.write + 1) % BUF;
        y * 0.8
    }

    /// Changement de hauteur par deux têtes de lecture qui glissent dans un
    /// retard et se relaient avec un fondu (sans changer la vitesse).
    fn pitch(&mut self, x: f32, ratio: f32) -> f32 {
        const WINDOW: f32 = RATE * 0.05;
        self.buf[self.write] = x;
        let mut y = 0.0;
        for offset in [0.0, 0.5] {
            let p = (self.phase + offset) % 1.0;
            let delay = p * WINDOW + 2.0;
            let pos = self.write as f32 - delay;
            let pos = if pos < 0.0 { pos + BUF as f32 } else { pos };
            let i = pos as usize;
            let frac = pos - i as f32;
            let a = self.buf[i % BUF];
            let b = self.buf[(i + 1) % BUF];
            let w = (PI * p).sin().powi(2);
            y += (a + (b - a) * frac) * w;
        }
        self.write = (self.write + 1) % BUF;
        self.phase = (self.phase + (1.0 - ratio) / WINDOW).rem_euclid(1.0);
        y
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compte les passages par zéro montants : une mesure grossière de la hauteur.
    fn crossings(s: &[f32]) -> usize {
        s.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count()
    }

    fn sine(freq: f32, n: usize) -> Vec<f32> {
        (0..n).map(|i| (2.0 * PI * freq * i as f32 / RATE).sin() * 0.5).collect()
    }

    #[test]
    fn pitch_shift_changes_frequency() {
        let input = sine(200.0, 48_000);
        for (kind, expected) in [(VoiceEffect::High, 1.45), (VoiceEffect::Low, 0.72)] {
            let mut fx = Effect::new(kind);
            let out: Vec<f32> = input.iter().map(|&x| fx.process(x)).collect();
            let ratio = crossings(&out[4800..]) as f32 / crossings(&input[4800..]) as f32;
            assert!((ratio - expected).abs() < 0.08, "{kind:?}: {ratio}");
        }
    }

    #[test]
    fn effects_stay_bounded() {
        let input = sine(300.0, 48_000);
        for kind in [VoiceEffect::Robot, VoiceEffect::Echo, VoiceEffect::High, VoiceEffect::Low] {
            let mut fx = Effect::new(kind);
            let peak = input.iter().map(|&x| fx.process(x).abs()).fold(0.0, f32::max);
            assert!(peak < 1.2, "{kind:?}: {peak}");
        }
    }
}
