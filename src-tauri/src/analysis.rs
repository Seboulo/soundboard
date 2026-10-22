//! Lecture complète d'un fichier pour connaître sa durée, son niveau et
//! dessiner sa forme d'onde dans l'éditeur.

use crate::config::Analysis;
use rodio::{Decoder, Source};
use std::fs::File;
use std::io::BufReader;
use std::path::Path;

fn decode(path: &Path) -> Result<Decoder<BufReader<File>>, String> {
    let file = File::open(path).map_err(|e| format!("fichier illisible : {e}"))?;
    Decoder::new(BufReader::new(file)).map_err(|e| format!("format audio non reconnu : {e}"))
}

pub fn analyze(path: &Path) -> Result<Analysis, String> {
    let decoder = decode(path)?;
    let channels = decoder.channels().max(1) as u64;
    let rate = decoder.sample_rate().max(1) as u64;
    let mut count: u64 = 0;
    let mut peak: f32 = 0.0;
    let mut sum_sq: f64 = 0.0;
    for s in decoder {
        let v = s as f32 / 32768.0;
        peak = peak.max(v.abs());
        sum_sq += (v * v) as f64;
        count += 1;
    }
    if count == 0 {
        return Err("le fichier ne contient aucun son".into());
    }
    Ok(Analysis {
        duration_ms: count * 1000 / (rate * channels),
        peak,
        rms: (sum_sq / count as f64).sqrt() as f32,
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
