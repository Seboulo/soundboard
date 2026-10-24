//! Lecture complète d'un fichier pour connaître sa durée, son niveau et
//! dessiner sa forme d'onde dans l'éditeur.

use crate::config::Analysis;
use crate::decoder::AudioFile;
use rodio::Source;
use std::path::Path;

fn decode(path: &Path) -> Result<AudioFile, String> {
    AudioFile::open(path)
}

pub fn analyze(path: &Path) -> Result<Analysis, String> {
    let decoder = decode(path)?;
    let channels = decoder.channels().max(1) as u64;
    let rate = decoder.sample_rate().max(1) as u64;
    // Tranches de 50 ms : celles plus faibles que -40 dB comptent comme du
    // silence et ne baissent pas le niveau moyen.
    const GATE: f64 = 0.01 * 0.01;
    let block = (rate * channels / 20).max(1);
    let mut count: u64 = 0;
    let mut peak: f32 = 0.0;
    let mut sum_sq: f64 = 0.0;
    let (mut block_sq, mut block_n) = (0.0f64, 0u64);
    let (mut loud_sq, mut loud_n) = (0.0f64, 0u64);
    for s in decoder {
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
    }
    if block_n > 0 && block_sq / block_n as f64 > GATE {
        loud_sq += block_sq;
        loud_n += block_n;
    }
    if count == 0 {
        return Err("le fichier ne contient aucun son".into());
    }
    let rms = (sum_sq / count as f64).sqrt() as f32;
    Ok(Analysis {
        duration_ms: count * 1000 / (rate * channels),
        peak,
        rms,
        // Jamais 0, sinon le son serait réanalysé à chaque lancement.
        loudness: (if loud_n > 0 { (loud_sq / loud_n as f64).sqrt() as f32 } else { rms }).max(1e-6),
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
