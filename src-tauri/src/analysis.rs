//! Lecture complète d'un fichier pour connaître sa durée, son niveau et
//! dessiner sa forme d'onde dans l'éditeur.

use crate::config::Analysis;
use crate::decoder::AudioFile;
use rodio::Source;
use std::path::Path;

fn decode(path: &Path) -> Result<AudioFile, String> {
    AudioFile::open(path)
}

/// Filtre de pondération K (ITU-R BS.1770) : coupe le grave et remonte
/// l'aigu comme le fait l'oreille. Coefficients calculés pour toute
/// fréquence d'échantillonnage, comme dans libebur128.
#[derive(Clone, Copy)]
struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
    z: [f64; 2],
}

impl Biquad {
    fn run(&mut self, x: f64) -> f64 {
        let y = self.b[0] * x + self.z[0];
        self.z[0] = self.b[1] * x - self.a[0] * y + self.z[1];
        self.z[1] = self.b[2] * x - self.a[1] * y;
        y
    }
}

fn k_weighting(rate: f64) -> [Biquad; 2] {
    use std::f64::consts::PI;
    let (f0, g, q) = (1681.974450955533, 3.999843853973347, 0.7071752369554196);
    let k = (PI * f0 / rate).tan();
    let vh = 10f64.powf(g / 20.0);
    let vb = vh.powf(0.4996667741545416);
    let a0 = 1.0 + k / q + k * k;
    let shelf = Biquad {
        b: [(vh + vb * k / q + k * k) / a0, 2.0 * (k * k - vh) / a0, (vh - vb * k / q + k * k) / a0],
        a: [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
        z: [0.0; 2],
    };
    let (f0, q) = (38.13547087602444, 0.5003270373238773);
    let k = (PI * f0 / rate).tan();
    let a0 = 1.0 + k / q + k * k;
    let high_pass = Biquad {
        b: [1.0, -2.0, 1.0],
        a: [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
        z: [0.0; 2],
    };
    [shelf, high_pass]
}

fn lufs(mean_square: f64) -> f32 {
    (-0.691 + 10.0 * mean_square.max(1e-12).log10()) as f32
}

/// Plus forte moyenne sur `width` tranches de 100 ms consécutives (ou sur
/// tout le son s'il est plus court).
fn max_window(blocks: &[f64], width: usize) -> f64 {
    let width = width.min(blocks.len()).max(1);
    let mut sum: f64 = blocks[..width].iter().sum();
    let mut best = sum;
    for i in width..blocks.len() {
        sum += blocks[i] - blocks[i - width];
        best = best.max(sum);
    }
    best / width as f64
}

/// Volume ressenti selon la norme EBU R128 : intégré (moyenne avec portes
/// à -70 LUFS puis -10 LU), momentané max (400 ms) et court terme max (3 s).
fn loudness(blocks: &[f64]) -> (f32, f32, f32) {
    if blocks.is_empty() {
        return (-70.0, -70.0, -70.0);
    }
    let momentary: Vec<f64> = if blocks.len() < 4 {
        vec![blocks.iter().sum::<f64>() / blocks.len() as f64]
    } else {
        blocks.windows(4).map(|w| w.iter().sum::<f64>() / 4.0).collect()
    };
    let gated = |threshold: f64| {
        let kept: Vec<f64> = momentary.iter().copied().filter(|&m| m > threshold).collect();
        (!kept.is_empty()).then(|| kept.iter().sum::<f64>() / kept.len() as f64)
    };
    let abs_gate = 10f64.powf((-70.0 + 0.691) / 10.0);
    let integrated = gated(abs_gate)
        .and_then(|mean| gated(mean * 0.1))
        .map_or(-70.0, lufs);
    (integrated, lufs(max_window(blocks, 4)), lufs(max_window(blocks, 30)))
}

pub fn analyze(path: &Path) -> Result<Analysis, String> {
    let decoder = decode(path)?;
    let channels = decoder.channels().max(1) as usize;
    let rate = decoder.sample_rate().max(1) as u64;
    // Tranches de 50 ms : celles plus faibles que -40 dB comptent comme du
    // silence et ne baissent pas le niveau moyen.
    const GATE: f64 = 0.01 * 0.01;
    let block = (rate * channels as u64 / 20).max(1);
    let mut count: u64 = 0;
    let mut peak: f32 = 0.0;
    let mut sum_sq: f64 = 0.0;
    let (mut block_sq, mut block_n) = (0.0f64, 0u64);
    let (mut loud_sq, mut loud_n) = (0.0f64, 0u64);
    // Pondération K par canal, puis énergie par tranche de 100 ms.
    let mut filters = vec![k_weighting(rate as f64); channels];
    let k_block = (rate / 10).max(1) as usize;
    let mut k_blocks: Vec<f64> = Vec::new();
    let (mut k_sq, mut k_n) = (0.0f64, 0usize);
    for (i, s) in decoder.enumerate() {
        let v = s as f32 / 32768.0;
        peak = peak.max(v.abs());
        let sq = (v * v) as f64;
        sum_sq += sq;
        count += 1;
        block_sq += sq;
        block_n += 1;
        if block_n == block {
            if block_sq / block_n as f64 > GATE {
                loud_sq += block_sq;
                loud_n += block_n;
            }
            (block_sq, block_n) = (0.0, 0);
        }
        let ch = i % channels;
        let [shelf, high_pass] = &mut filters[ch];
        let w = high_pass.run(shelf.run(v as f64));
        k_sq += w * w;
        if ch == channels - 1 {
            k_n += 1;
            if k_n == k_block {
                k_blocks.push(k_sq / k_n as f64);
                (k_sq, k_n) = (0.0, 0);
            }
        }
    }
    if block_n > 0 && block_sq / block_n as f64 > GATE {
        loud_sq += block_sq;
        loud_n += block_n;
    }
    // Dernière tranche incomplète : comptée seulement si elle n'est pas
    // minuscule, sinon elle fausserait la moyenne des sons très courts.
    if k_n > k_block / 4 || (k_blocks.is_empty() && k_n > 0) {
        k_blocks.push(k_sq / k_n as f64);
    }
    if count == 0 {
        return Err("le fichier ne contient aucun son".into());
    }
    let rms = (sum_sq / count as f64).sqrt() as f32;
    let (integrated, max_momentary, max_short) = loudness(&k_blocks);
    Ok(Analysis {
        duration_ms: count * 1000 / (rate * channels as u64),
        peak,
        rms,
        // Jamais 0, sinon le son serait réanalysé à chaque lancement.
        loudness: (if loud_n > 0 { (loud_sq / loud_n as f64).sqrt() as f32 } else { rms }).max(1e-6),
        // Toujours négatif : 0 veut dire « pas encore mesuré ».
        lufs: integrated.min(-0.01),
        max_momentary: max_momentary.min(-0.01),
        max_short: max_short.min(-0.01),
    })
}

/// Pic de chaque tranche du son, entre 0 et 1.
pub fn waveform(path: &Path, points: usize) -> Result<Vec<f32>, String> {
    let samples: Vec<i16> = decode(path)?.collect();
    if samples.is_empty() || points == 0 {
        return Ok(vec![]);
    }
    let chunk = samples.len().div_ceil(points);
    Ok(samples
        .chunks(chunk)
        .map(|c| c.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0) as f32 / 32768.0)
        .collect())
}
