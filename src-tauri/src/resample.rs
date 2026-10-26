//! Conversion de fréquence de qualité studio : interpolation par sinus
//! cardinal fenêtré (Kaiser, 64 points), comme les convertisseurs des
//! logiciels audio pro. Bande passante plate jusqu'à ~18 kHz, repliements
//! et images à -90 dB environ, là où l'interpolation cubique laissait des
//! parasites vers -50 dB et assourdissait l'extrême aigu.

use std::sync::{Arc, Mutex, OnceLock};

/// Points du filtre : la conversion « voit » 32 échantillons de part et
/// d'autre (moins d'une milliseconde de retard).
pub const TAPS: usize = 64;
const HALF: usize = TAPS / 2;
/// Positions fractionnaires précalculées (interpolées entre elles).
const PHASES: usize = 512;
/// Fenêtre de Kaiser pour ~90 dB de réjection.
const BETA: f64 = 9.0;

pub struct Kernel {
    /// (PHASES + 1) lignes de TAPS coefficients.
    table: Vec<f32>,
}

/// Fonction de Bessel modifiée d'ordre 0 (pour la fenêtre de Kaiser).
fn bessel_i0(x: f64) -> f64 {
    let (mut sum, mut term, mut k) = (1.0, 1.0, 1.0);
    while term > sum * 1e-12 {
        term *= (x / (2.0 * k)).powi(2);
        sum += term;
        k += 1.0;
    }
    sum
}

impl Kernel {
    /// Filtre pour passer d'une fréquence à une autre (`ratio` = sortie /
    /// entrée). Partagé entre toutes les lectures au même rapport.
    pub fn for_ratio(ratio: f64) -> Arc<Self> {
        type Cache = Mutex<Vec<(u64, Arc<Kernel>)>>;
        static CACHE: OnceLock<Cache> = OnceLock::new();
        let cache = CACHE.get_or_init(Default::default);
        let key = ratio.to_bits();
        let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, k)) = cache.iter().find(|(r, _)| *r == key) {
            return k.clone();
        }
        let kernel = Arc::new(Self::new(ratio));
        cache.push((key, kernel.clone()));
        kernel
    }

    fn new(ratio: f64) -> Self {
        // Coupure à 91 % de la plus petite des deux moitiés de fréquence :
        // 20 kHz pour du 44,1 kHz, la bande de transition finit avant
        // Nyquist (pas de repliement).
        let cutoff = 0.5 * ratio.min(1.0) * 0.91;
        let norm = bessel_i0(BETA);
        let mut table = Vec::with_capacity((PHASES + 1) * TAPS);
        for p in 0..=PHASES {
            let frac = p as f64 / PHASES as f64;
            let row: Vec<f64> = (0..TAPS)
                .map(|k| {
                    let d = (k as f64 + 1.0 - HALF as f64) - frac;
                    let x = d / HALF as f64;
                    if x.abs() > 1.0 {
                        return 0.0;
                    }
                    let arg = 2.0 * cutoff * d * std::f64::consts::PI;
                    let sinc = if arg.abs() < 1e-12 { 1.0 } else { arg.sin() / arg };
                    2.0 * cutoff * sinc * bessel_i0(BETA * (1.0 - x * x).sqrt()) / norm
                })
                .collect();
            // Gain exactement 1 sur un son continu, à toutes les positions.
            let sum: f64 = row.iter().sum();
            table.extend(row.iter().map(|w| (w / sum) as f32));
        }
        Self { table }
    }
}

/// Les derniers échantillons (entrelacés) d'un son, prêts à être lus à une
/// position fractionnaire quelconque entre l'échantillon central et le
/// suivant.
pub struct SincWindow {
    kernel: Arc<Kernel>,
    channels: usize,
    data: Vec<f32>,
    /// Première trame de la fenêtre dans `data`.
    start: usize,
    weights: [f32; TAPS],
}

impl SincWindow {
    pub fn new(kernel: Arc<Kernel>, channels: usize) -> Self {
        let channels = channels.max(1);
        Self { kernel, channels, data: vec![0.0; TAPS * channels], start: 0, weights: [0.0; TAPS] }
    }

    /// Ajoute une trame (une valeur par canal) et fait glisser la fenêtre.
    pub fn push(&mut self, frame: &[f32]) {
        if self.start >= 4096 {
            self.data.drain(..self.start * self.channels);
            self.start = 0;
        }
        self.data.extend(frame.iter().copied().take(self.channels));
        self.start += 1;
    }

    /// Valeur de la trame la plus récente (canal `c`).
    pub fn newest(&self, c: usize) -> f32 {
        self.data[self.data.len() - self.channels + c]
    }

    /// Lit le son à `frac` (0 à 1) entre l'échantillon central et le suivant.
    pub fn read(&mut self, frac: f64, out: &mut [f32]) {
        let f = frac.clamp(0.0, 1.0) * PHASES as f64;
        let p = (f as usize).min(PHASES - 1);
        let a = (f - p as f64) as f32;
        let t = &self.kernel.table;
        let (r0, r1) = (&t[p * TAPS..(p + 1) * TAPS], &t[(p + 1) * TAPS..(p + 2) * TAPS]);
        for ((w, &x), &y) in self.weights.iter_mut().zip(r0).zip(r1) {
            *w = x + (y - x) * a;
        }
        let ch = self.channels;
        let window = &self.data[self.start * ch..(self.start + TAPS) * ch];
        for (c, o) in out.iter_mut().enumerate().take(ch) {
            *o = self.weights.iter().zip(window[c..].iter().step_by(ch)).map(|(w, x)| w * x).sum();
        }
    }
}

/// Retard du filtre, en échantillons d'entrée : il faut pousser autant de
/// trames avant que la première lue soit le début du son.
pub const DELAY: usize = HALF - 1;

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::TAU;

    /// Convertit un son mono d'une fréquence à l'autre.
    fn convert(input: &[f32], from: u32, to: u32) -> Vec<f32> {
        let step = from as f64 / to as f64;
        let mut w = SincWindow::new(Kernel::for_ratio(1.0 / step), 1);
        let mut out = Vec::new();
        let mut pos = 0.0;
        let mut v = [0.0];
        for &x in input {
            w.push(&[x]);
            while pos < 1.0 {
                w.read(pos, &mut v);
                out.push(v[0]);
                pos += step;
            }
            pos -= 1.0;
        }
        out
    }

    /// Parasites (tout ce qui n'est pas la sinusoïde attendue), en dB.
    fn residual_db(out: &[f32], freq: f64, rate: f64) -> f64 {
        let n = out.len();
        let w = TAU * freq / rate;
        // Moindres carrés exacts sur sin et cos (pas besoin de périodes entières).
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
            / n as f64;
        10.0 * (err / ((a * a + b * b) / 2.0)).log10()
    }

    fn tone(freq: f64, rate: u32, secs: f64) -> Vec<f32> {
        (0..(rate as f64 * secs) as usize).map(|i| ((TAU * freq * i as f64 / rate as f64).sin() * 0.5) as f32).collect()
    }

    #[test]
    fn upsampling_is_clean_up_to_high_treble() {
        for freq in [1000.0, 8000.0, 16_000.0] {
            let out = convert(&tone(freq, 44_100, 1.0), 44_100, 48_000);
            let db = residual_db(&out[4800..43_000], freq, 48_000.0);
            assert!(db < -80.0, "{freq} Hz : {db:.1} dB");
        }
    }

    #[test]
    fn downsampling_does_not_fold_ultrasound_back() {
        // Un 23 kHz (inaudible) en 48 kHz ne doit pas revenir en 21 kHz
        // audible dans du 44,1 kHz.
        let input = tone(23_000.0, 48_000, 1.0);
        let out = convert(&input, 48_000, 44_100);
        let rms = |v: &[f32]| (v.iter().map(|x| x * x).sum::<f32>() / v.len() as f32).sqrt();
        let db = 20.0 * (rms(&out[4410..40_000]) / rms(&input)).log10();
        assert!(db < -70.0, "repliement {db:.1} dB");
    }

    #[test]
    fn keeps_the_level_of_steady_sound() {
        let out = convert(&vec![0.25; 9600], 48_000, 44_100);
        assert!(out[200..8000].iter().all(|v| (v - 0.25).abs() < 1e-4));
    }
}
