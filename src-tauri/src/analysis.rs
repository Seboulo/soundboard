//! Lecture complète d'un fichier pour connaître sa durée, son niveau et
//! dessiner sa forme d'onde dans l'éditeur.

use crate::config::{Analysis, SegmentKey};
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
    let (channels, rate) = (decoder.channels(), decoder.sample_rate());
    analyze_samples(decoder, channels, rate)
}

/// Mesure le passage tel qu'il est joué : découpé, avec ses effets.
pub fn analyze_segment(path: &Path, key: &SegmentKey) -> Result<Analysis, String> {
    let mut decoder = decode(path)?;
    decoder.seek_ms(key.start_ms);
    let r = crate::sound_fx::render(decoder, key.start_ms, key.end_ms, &key.fx, false);
    analyze_samples(r.data.iter().copied(), r.channels, r.rate)
}

fn analyze_samples(samples: impl Iterator<Item = f32>, channels: u16, rate: u32) -> Result<Analysis, String> {
    let decoder = samples;
    let channels = channels.max(1) as usize;
    let rate = rate.max(1) as u64;
    // Tranches de 50 ms : celles plus faibles que -40 dB comptent comme du
    // silence et ne baissent pas le niveau moyen.
    const GATE: f64 = 0.01 * 0.01;
    let block = (rate * channels as u64 / 20).max(1);
    let mut count: u64 = 0;
    let mut peak: f32 = 0.0;
    let mut sum_sq: f64 = 0.0;
    let (mut block_sq, mut block_n) = (0.0f64, 0u64);
    let (mut loud_sq, mut loud_n) = (0.0f64, 0u64);
    // Le volume ressenti est mesuré tel que Discord le transmet : replié en
    // mono (somme, ou différence quand les canaux s'opposent, comme
    // `MonoSafe`), compté comme un mono joué sur deux haut-parleurs. Un même
    // son en mono ou en stéréo ressort ainsi au même volume, et un son
    // stéréo très large n'est plus trop doux chez les autres.
    // Pondération K, puis énergie par tranche de 100 ms.
    let mut filters = [k_weighting(rate as f64), k_weighting(rate as f64)];
    let (mut frame_sum, mut frame_diff) = (0.0f64, 0.0f64);
    let mut diff_sq = 0.0f64;
    let k_block = (rate / 10).max(1) as usize;
    let mut k_blocks: Vec<f64> = Vec::new();
    let (mut k_sq, mut k_n) = (0.0f64, 0usize);
    for (i, s) in decoder.enumerate() {
        let v = s;
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
        frame_sum += v as f64;
        if channels == 2 {
            frame_diff += if ch == 0 { v as f64 } else { -(v as f64) };
        }
        if ch == channels - 1 {
            let sum = frame_sum / channels as f64;
            let diff = frame_diff / channels as f64;
            (frame_sum, frame_diff) = (0.0, 0.0);
            let [shelf, high_pass] = &mut filters[0];
            let w = high_pass.run(shelf.run(sum));
            k_sq += w * w;
            let [shelf, high_pass] = &mut filters[1];
            let w = high_pass.run(shelf.run(diff));
            diff_sq += w * w;
            k_n += 1;
            if k_n == k_block {
                k_blocks.push(fold_power(k_sq, diff_sq) / k_n as f64);
                (k_sq, diff_sq, k_n) = (0.0, 0.0, 0);
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
        k_blocks.push(fold_power(k_sq, diff_sq) / k_n as f64);
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
        mono_fold: true,
    })
}

/// Énergie du repli mono (voir `MonoSafe`), comptée deux fois : un mono
/// joué sur deux haut-parleurs, comme le mesurait la stéréo jusqu'ici.
fn fold_power(sum: f64, diff: f64) -> f64 {
    2.0 * if diff > 4.0 * sum { diff } else { sum }
}

/// Pic de chaque tranche du son, entre 0 et 1.
pub fn waveform(path: &Path, points: usize) -> Result<Vec<f32>, String> {
    let samples: Vec<f32> = decode(path)?.collect();
    if samples.is_empty() || points == 0 {
        return Ok(vec![]);
    }
    let chunk = samples.len().div_ceil(points);
    Ok(samples
        .chunks(chunk)
        .map(|c| c.iter().fold(0.0f32, |m, s| m.max(s.abs())).min(1.0))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lufs(channels: u16, frames: impl Iterator<Item = Vec<f32>>) -> f32 {
        analyze_samples(frames.flatten(), channels, 48_000).unwrap().lufs
    }

    #[test]
    fn mono_and_stereo_copies_measure_the_same() {
        let tone = || (0..48_000 * 3).map(|i| (i as f32 * 0.05).sin() * 0.2);
        let mono = lufs(1, tone().map(|v| vec![v]));
        let stereo = lufs(2, tone().map(|v| vec![v, v]));
        assert!((mono - stereo).abs() < 0.1, "{mono} vs {stereo}");
        // Opposition de phase : mesuré sur la différence, comme le replie
        // `MonoSafe`, donc pas plus doux.
        let opposed = lufs(2, tone().map(|v| vec![v, -v]));
        assert!((opposed - stereo).abs() < 0.1, "{opposed} vs {stereo}");
    }
}
