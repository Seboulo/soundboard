//! Effets des sons (éditeur) : vitesse, hauteur, à l'envers, couleur du son,
//! écho et fondus. Non destructifs : le fichier d'origine n'est jamais
//! modifié, le son découpé est recalculé en mémoire à la lecture (gardé en
//! cache pour que le raccourci reste instantané).

use rodio::Source;
use serde::{Deserialize, Serialize};
use std::f32::consts::PI;
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tone {
    #[default]
    Normal,
    /// Talkie-walkie / vieille radio : bande étroite, un peu saturée.
    Radio,
    /// Étouffé, comme derrière un mur.
    Muffled,
    /// Basses renforcées.
    Bass,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SoundFx {
    /// Vitesse de lecture (change aussi la hauteur, comme un disque).
    pub speed: f32,
    /// Hauteur en demi-tons, sans changer la vitesse.
    pub pitch: f32,
    pub reverse: bool,
    pub tone: Tone,
    /// Quantité d'écho, 0 – 1.
    pub echo: f32,
    pub fade_in_ms: u32,
    pub fade_out_ms: u32,
}

impl Default for SoundFx {
    fn default() -> Self {
        Self { speed: 1.0, pitch: 0.0, reverse: false, tone: Tone::Normal, echo: 0.0, fade_in_ms: 0, fade_out_ms: 0 }
    }
}

const ECHO_DELAY_S: f32 = 0.28;
/// Au-delà, les effets ne sont pas appliqués (mémoire).
const MAX_SECONDS: u64 = 300;

impl SoundFx {
    /// Ramène chaque réglage dans ses bornes (valeurs venant de l'interface).
    pub fn sanitized(mut self) -> Self {
        let finite = |v: f32, d: f32| if v.is_finite() { v } else { d };
        self.speed = finite(self.speed, 1.0).clamp(0.5, 2.0);
        self.pitch = finite(self.pitch, 0.0).clamp(-12.0, 12.0).round();
        self.echo = finite(self.echo, 0.0).clamp(0.0, 1.0);
        self.fade_in_ms = self.fade_in_ms.min(5000);
        self.fade_out_ms = self.fade_out_ms.min(5000);
        self
    }

    pub fn is_neutral(&self) -> bool {
        *self == Self::default()
    }

    fn feedback(&self) -> f32 {
        0.2 + 0.35 * self.echo
    }

    /// Queue d'écho ajoutée après la fin du son, en ms.
    fn echo_tail_ms(&self) -> u64 {
        if self.echo <= 0.0 {
            return 0;
        }
        // Jusqu'à ce que les répétitions passent sous -40 dB.
        let repeats = (0.01f32.ln() / self.feedback().ln()).ceil();
        (repeats * ECHO_DELAY_S * 1000.0).min(3000.0) as u64
    }

    /// Durée entendue pour un passage de `input_ms` du fichier.
    pub fn output_ms(&self, input_ms: u64, looping: bool) -> u64 {
        let base = (input_ms as f64 / self.speed.max(0.1) as f64) as u64;
        if looping {
            base
        } else {
            base + self.echo_tail_ms()
        }
    }
}

/// Son déjà calculé, entrelacé.
#[derive(Clone)]
pub struct Rendered {
    pub data: Arc<[f32]>,
    pub channels: u16,
    pub rate: u32,
}

/// Décode le passage découpé puis lui applique les effets.
pub fn render(
    mut source: impl Source<Item = f32>,
    start_ms: u64,
    end_ms: Option<u64>,
    fx: &SoundFx,
    looping: bool,
) -> Rendered {
    let channels = source.channels().max(1);
    let rate = source.sample_rate().max(1);
    let max_ms = end_ms.map_or(MAX_SECONDS * 1000, |e| e.saturating_sub(start_ms).min(MAX_SECONDS * 1000));
    let max = (max_ms * rate as u64 / 1000) as usize * channels as usize;
    let mut data: Vec<f32> = Vec::with_capacity(max.min(rate as usize * channels as usize * 30));
    data.extend(source.by_ref().take(max));
    let ch = channels as usize;
    data.truncate(data.len() / ch * ch);

    if fx.reverse {
        reverse_frames(&mut data, ch);
    }
    // Hauteur sans changer la vitesse : on étire le son puis on le lit plus
    // vite (ou plus lentement) d'autant.
    let ratio = 2f32.powf(fx.pitch / 12.0);
    if (ratio - 1.0).abs() > 1e-3 {
        data = stretch(&data, ch, rate, ratio as f64);
    }
    let out_rate = ((rate as f64) * fx.speed as f64 * ratio as f64).round().clamp(4000.0, 384_000.0) as u32;
    apply_tone(&mut data, ch, out_rate as f32, fx.tone);
    if fx.echo > 0.0 {
        data = echo(data, ch, out_rate as f32, fx, looping);
    }
    fades(&mut data, ch, out_rate as f32, fx);
    Rendered { data: data.into(), channels, rate: out_rate }
}

fn reverse_frames(data: &mut [f32], ch: usize) {
    let frames = data.len() / ch;
    for i in 0..frames / 2 {
        let j = frames - 1 - i;
        for c in 0..ch {
            data.swap(i * ch + c, j * ch + c);
        }
    }
}

/// Étire le son d'un facteur `factor` sans changer sa hauteur (WSOLA : des
/// grains de 40 ms recollés là où ils se ressemblent le plus, pour éviter
/// l'effet de « phasing »).
fn stretch(x: &[f32], ch: usize, rate: u32, factor: f64) -> Vec<f32> {
    let frames = x.len() / ch;
    let n = ((rate as f64 * 0.04) as usize).max(64) & !1;
    if frames < n * 2 {
        return x.to_vec();
    }
    let hop_out = n / 2;
    let hop_in = hop_out as f64 / factor;
    let tol = (rate as f64 * 0.012) as isize;
    let out_frames = (frames as f64 * factor) as usize;
    let window: Vec<f32> = (0..n).map(|i| 0.5 - 0.5 * (2.0 * PI * i as f32 / n as f32).cos()).collect();
    let mono: Vec<f32> = x.chunks_exact(ch).map(|f| f.iter().sum::<f32>()).collect();
    let mut out = vec![0.0f32; (out_frames + n) * ch];
    let mut norm = vec![0.0f32; out_frames + n];
    let last = (frames - n) as isize;
    let mut prev: isize = 0;
    let mut k = 0usize;
    while k * hop_out < out_frames {
        let target = (k as f64 * hop_in) as isize;
        let pos = if k == 0 {
            0
        } else {
            // Ce qui suivrait naturellement le grain précédent.
            let natural = (prev + hop_out as isize).min(last);
            let mut best = target.clamp(0, last);
            let mut best_score = f32::MIN;
            let mut cand = (target - tol).max(0);
            while cand <= (target + tol).min(last) {
                let mut score = 0.0;
                let mut i = 0;
                while i < hop_out {
                    score += mono[(cand as usize) + i] * mono[natural as usize + i];
                    i += 4;
                }
                if score > best_score {
                    best_score = score;
                    best = cand;
                }
                cand += 1;
            }
            best
        };
        let base = k * hop_out;
        for i in 0..n {
            let w = window[i];
            let src = (pos as usize + i) * ch;
            let dst = (base + i) * ch;
            for c in 0..ch {
                out[dst + c] += x[src + c] * w;
            }
            norm[base + i] += w;
        }
        prev = pos;
        k += 1;
    }
    out.truncate(out_frames * ch);
    for (f, w) in out.chunks_exact_mut(ch).zip(&norm) {
        if *w > 1e-3 {
            f.iter_mut().for_each(|s| *s /= w);
        }
    }
    out
}

/// Filtre biquad (formules RBJ), un état par canal.
#[derive(Clone, Copy)]
struct Biquad {
    b: [f32; 3],
    a: [f32; 2],
    z: [f32; 2],
}

impl Biquad {
    fn new(kind: &str, f: f32, q: f32, gain_db: f32, rate: f32) -> Self {
        let w = 2.0 * PI * (f / rate).min(0.49);
        let (s, c) = w.sin_cos();
        let alpha = s / (2.0 * q);
        let (b, a) = match kind {
            "lp" => ([(1.0 - c) / 2.0, 1.0 - c, (1.0 - c) / 2.0], [1.0 + alpha, -2.0 * c, 1.0 - alpha]),
            "hp" => ([(1.0 + c) / 2.0, -(1.0 + c), (1.0 + c) / 2.0], [1.0 + alpha, -2.0 * c, 1.0 - alpha]),
            _ => {
                // Plateau grave.
                let a_ = 10f32.powf(gain_db / 40.0);
                let sa = 2.0 * a_.sqrt() * alpha;
                (
                    [
                        a_ * ((a_ + 1.0) - (a_ - 1.0) * c + sa),
                        2.0 * a_ * ((a_ - 1.0) - (a_ + 1.0) * c),
                        a_ * ((a_ + 1.0) - (a_ - 1.0) * c - sa),
                    ],
                    [(a_ + 1.0) + (a_ - 1.0) * c + sa, -2.0 * ((a_ - 1.0) + (a_ + 1.0) * c), (a_ + 1.0) + (a_ - 1.0) * c - sa],
                )
            }
        };
        Self { b: [b[0] / a[0], b[1] / a[0], b[2] / a[0]], a: [a[1] / a[0], a[2] / a[0]], z: [0.0; 2] }
    }

    fn process(&mut self, x: f32) -> f32 {
        let y = self.b[0] * x + self.z[0];
        self.z[0] = self.b[1] * x - self.a[0] * y + self.z[1];
        self.z[1] = self.b[2] * x - self.a[1] * y;
        y
    }
}

fn apply_tone(data: &mut [f32], ch: usize, rate: f32, tone: Tone) {
    let chain: Vec<Biquad> = match tone {
        Tone::Normal => return,
        Tone::Radio => vec![Biquad::new("hp", 450.0, 0.8, 0.0, rate), Biquad::new("lp", 2800.0, 0.9, 0.0, rate)],
        Tone::Muffled => vec![Biquad::new("lp", 450.0, 0.7, 0.0, rate), Biquad::new("lp", 450.0, 0.7, 0.0, rate)],
        Tone::Bass => vec![Biquad::new("shelf", 140.0, 0.7, 10.0, rate)],
    };
    let mut filters: Vec<Vec<Biquad>> = vec![chain; ch];
    let (pre, drive) = match tone {
        Tone::Radio => (1.0, Some(2.5f32)),
        Tone::Bass => (0.6, None),
        Tone::Muffled => (1.4, None),
        Tone::Normal => (1.0, None),
    };
    for frame in data.chunks_exact_mut(ch) {
        for (s, chain) in frame.iter_mut().zip(filters.iter_mut()) {
            let mut y = *s * pre;
            for f in chain.iter_mut() {
                y = f.process(y);
            }
            if let Some(d) = drive {
                y = (y * d).tanh() / d.tanh() * 0.9;
            }
            *s = y;
        }
    }
}

fn echo(mut data: Vec<f32>, ch: usize, rate: f32, fx: &SoundFx, looping: bool) -> Vec<f32> {
    if !looping {
        let tail = (fx.echo_tail_ms() as f32 * rate / 1000.0) as usize;
        data.resize(data.len() + tail * ch, 0.0);
    }
    let delay = ((ECHO_DELAY_S * rate) as usize).max(1) * ch;
    let fb = fx.feedback();
    let wet = 0.25 + 0.45 * fx.echo;
    let mut line = vec![0.0f32; delay];
    let mut at = 0;
    for s in data.iter_mut() {
        let d = line[at];
        let x = *s;
        line[at] = x + d * fb;
        *s = x + d * wet;
        at = (at + 1) % delay;
    }
    data
}

fn fades(data: &mut [f32], ch: usize, rate: f32, fx: &SoundFx) {
    let frames = data.len() / ch;
    let fin = ((fx.fade_in_ms as f32 * rate / 1000.0) as usize).min(frames);
    let fout = ((fx.fade_out_ms as f32 * rate / 1000.0) as usize).min(frames);
    for i in 0..fin {
        let g = (i as f32 / fin as f32).powi(2);
        data[i * ch..(i + 1) * ch].iter_mut().for_each(|s| *s *= g);
    }
    for i in 0..fout {
        let g = (i as f32 / fout as f32).powi(2);
        let f = frames - 1 - i;
        data[f * ch..(f + 1) * ch].iter_mut().for_each(|s| *s *= g);
    }
}

/// Lecture d'un son calculé (partagé entre le casque et le câble).
pub struct RenderedSource {
    sound: Rendered,
    pos: usize,
    looping: bool,
}

impl RenderedSource {
    pub fn new(sound: Rendered, looping: bool) -> Self {
        Self { sound, pos: 0, looping }
    }
}

impl Iterator for RenderedSource {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        if self.pos >= self.sound.data.len() {
            if !self.looping || self.sound.data.is_empty() {
                return None;
            }
            self.pos = 0;
        }
        let s = self.sound.data[self.pos];
        self.pos += 1;
        Some(s)
    }
}

impl Source for RenderedSource {
    fn current_frame_len(&self) -> Option<usize> {
        None
    }
    fn channels(&self) -> u16 {
        self.sound.channels
    }
    fn sample_rate(&self) -> u32 {
        self.sound.rate
    }
    fn total_duration(&self) -> Option<Duration> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rodio::buffer::SamplesBuffer;

    fn sine(freq: f32, rate: u32, secs: f32) -> Vec<f32> {
        (0..(rate as f32 * secs) as usize).map(|i| (2.0 * PI * freq * i as f32 / rate as f32).sin() * 0.5).collect()
    }

    /// Fréquence dominante par passages à zéro.
    fn freq(x: &[f32], rate: f32) -> f32 {
        let crossings = x.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count();
        crossings as f32 * rate / x.len() as f32
    }

    #[test]
    fn neutral_by_default_and_clamped() {
        assert!(SoundFx::default().is_neutral());
        let fx = SoundFx { speed: 9.0, pitch: f32::NAN, echo: -1.0, ..Default::default() }.sanitized();
        assert_eq!((fx.speed, fx.pitch, fx.echo), (2.0, 0.0, 0.0));
    }

    #[test]
    fn pitch_keeps_duration_and_shifts_frequency() {
        let rate = 48_000;
        let src = SamplesBuffer::new(1, rate, sine(440.0, rate, 2.0));
        let fx = SoundFx { pitch: 12.0, ..Default::default() };
        let r = render(src, 0, None, &fx, false);
        let secs = r.data.len() as f32 / r.rate as f32;
        assert!((secs - 2.0).abs() < 0.05, "durée {secs}");
        // Lu à r.rate, la fréquence entendue double.
        let f = freq(&r.data[r.data.len() / 4..r.data.len() * 3 / 4], r.rate as f32);
        assert!((f - 880.0).abs() < 25.0, "fréquence {f}");
    }

    #[test]
    fn speed_shortens_and_reverse_flips() {
        let rate = 44_100;
        let ramp: Vec<f32> = (0..rate).map(|i| i as f32 / rate as f32).collect();
        let fx = SoundFx { speed: 2.0, reverse: true, ..Default::default() };
        let r = render(SamplesBuffer::new(1, rate, ramp), 0, None, &fx, false);
        assert_eq!(r.rate, rate * 2);
        assert!(r.data[0] > 0.99 && *r.data.last().unwrap() < 0.01);
        assert_eq!(fx.output_ms(1000, false), 500);
    }

    #[test]
    fn echo_adds_tail_and_fades_reach_zero() {
        let rate = 48_000;
        let fx = SoundFx { echo: 0.5, fade_in_ms: 100, tone: Tone::Radio, ..Default::default() };
        let r = render(SamplesBuffer::new(2, rate, sine(300.0, rate, 1.0)), 0, None, &fx, false);
        assert!(r.data.len() > rate as usize * 2);
        assert!(r.data[0].abs() < 1e-6);
        assert!(r.data.iter().all(|s| s.is_finite()));
    }
}
