//! Passages enlevés au milieu d'un son (en plus de la découpe du début et de
//! la fin). Le fichier n'est jamais modifié : les passages sont sautés à la
//! lecture, avec un fondu très court de chaque côté pour ne pas claquer.

use rodio::Source;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Un passage enlevé, en millisecondes dans le fichier d'origine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cut {
    pub from_ms: u64,
    pub to_ms: u64,
}

/// Au plus 32 passages enlevés par son.
const MAX_CUTS: usize = 32;
/// Plus court que ça, le passage est oublié (clic involontaire).
const MIN_CUT_MS: u64 = 20;
/// Fondu de chaque côté d'un passage enlevé.
const FADE_MS: u64 = 5;

/// Passages rangés dans l'ordre, fusionnés quand ils se touchent, et gardés
/// dans le passage joué (`start`..`end`). Il reste toujours au moins 50 ms.
pub fn clean(mut cuts: Vec<Cut>, start: u64, end: u64) -> Vec<Cut> {
    for c in &mut cuts {
        if c.from_ms > c.to_ms {
            std::mem::swap(&mut c.from_ms, &mut c.to_ms);
        }
        c.from_ms = c.from_ms.max(start);
        c.to_ms = c.to_ms.min(end);
    }
    cuts.retain(|c| c.to_ms >= c.from_ms + MIN_CUT_MS);
    cuts.sort_by_key(|c| c.from_ms);
    let mut out: Vec<Cut> = Vec::with_capacity(cuts.len());
    for c in cuts {
        match out.last_mut() {
            Some(last) if c.from_ms <= last.to_ms => last.to_ms = last.to_ms.max(c.to_ms),
            _ => out.push(c),
        }
    }
    out.truncate(MAX_CUTS);
    // Tout enlever ne veut rien dire : on garde au moins 50 ms.
    while end.saturating_sub(start).saturating_sub(removed_ms(&out, start, Some(end))) < 50 && !out.is_empty() {
        out.pop();
    }
    out
}

/// Durée enlevée dans le passage `start`..`end` (fin du fichier si `None`).
pub fn removed_ms(cuts: &[Cut], start: u64, end: Option<u64>) -> u64 {
    let end = end.unwrap_or(u64::MAX);
    cuts.iter()
        .map(|c| c.to_ms.min(end).saturating_sub(c.from_ms.max(start)))
        .sum()
}

/// Empreinte des passages enlevés (0 quand il n'y en a pas), pour savoir si
/// le niveau mesuré correspond encore.
pub fn fingerprint(cuts: &[Cut]) -> u64 {
    if cuts.is_empty() {
        return 0;
    }
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for c in cuts {
        for v in [c.from_ms, c.to_ms] {
            h = (h ^ v).wrapping_mul(0x0100_0000_01b3);
        }
    }
    h | 1
}

/// Le passage joué d'un son en WAV, passages enlevés sautés (30 s au plus),
/// pour ce qui ne lit qu'un fichier tel quel (son joint à un extrait).
/// `None` : rien n'est enlevé dans ce passage, le fichier d'origine suffit.
pub fn kept_wav(sound: &crate::config::Sound) -> Option<Result<Vec<u8>, String>> {
    let total = sound.analysis.map_or(u64::MAX, |a| a.duration_ms);
    let end = sound.end_ms.unwrap_or(total).min(total);
    let removed = removed_ms(&sound.cuts, sound.start_ms, Some(end));
    if removed == 0 {
        return None;
    }
    Some(crate::decoder::AudioFile::open(&sound.path).map(|mut decoder| {
        decoder.seek_ms(sound.start_ms);
        let source = SkipCuts::new(decoder, sound.start_ms, &sound.cuts);
        let (channels, rate) = (source.channels().max(1), source.sample_rate().max(1));
        let kept = end.saturating_sub(sound.start_ms).saturating_sub(removed).min(30_000);
        let samples = (kept * rate as u64 / 1000) as usize * channels as usize;
        let data: Vec<f32> = source.take(samples).collect();
        crate::calibration::wav_bytes_of(&data, channels, rate)
    }))
}

/// Lit `inner` (déjà placé à `start_ms`) en sautant les passages enlevés.
pub struct SkipCuts<S> {
    inner: S,
    channels: u64,
    /// Passages en trames (début, fin), absolus dans le fichier.
    cuts: Vec<(u64, u64)>,
    next: usize,
    /// Trame en cours, absolue dans le fichier.
    frame: u64,
    channel: u64,
    fade: u64,
    gain: f32,
}

impl<S: Source<Item = f32>> SkipCuts<S> {
    pub fn new(inner: S, start_ms: u64, cuts: &[Cut]) -> Self {
        let rate = inner.sample_rate().max(1) as u64;
        let at = |ms: u64| ms * rate / 1000;
        let start = at(start_ms);
        let cuts: Vec<(u64, u64)> = cuts.iter().map(|c| (at(c.from_ms), at(c.to_ms))).filter(|c| c.1 > start).collect();
        Self {
            channels: inner.channels().max(1) as u64,
            inner,
            cuts,
            next: 0,
            frame: start,
            channel: 0,
            fade: at(FADE_MS).max(1),
            gain: 1.0,
        }
    }
}

impl<S: Source<Item = f32>> Iterator for SkipCuts<S> {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        if self.channel == 0 {
            // Début d'une trame : on saute le passage enlevé qui commence ici.
            while let Some(&(from, to)) = self.cuts.get(self.next) {
                if self.frame < from {
                    break;
                }
                if self.frame < to {
                    for _ in 0..(to - self.frame) * self.channels {
                        self.inner.next()?;
                    }
                    self.frame = to;
                }
                self.next += 1;
            }
            // Fondu juste avant et juste après chaque passage enlevé.
            let mut g = 1.0f32;
            if let Some(&(from, _)) = self.cuts.get(self.next) {
                let d = from - self.frame;
                if d < self.fade {
                    g = g.min(d as f32 / self.fade as f32);
                }
            }
            if self.next > 0 {
                let d = self.frame - self.cuts[self.next - 1].1;
                if d < self.fade {
                    g = g.min(d as f32 / self.fade as f32);
                }
            }
            self.gain = g;
        }
        let s = self.inner.next()?;
        self.channel += 1;
        if self.channel == self.channels {
            self.channel = 0;
            self.frame += 1;
        }
        Some(s * self.gain)
    }
}

impl<S: Source<Item = f32>> Source for SkipCuts<S> {
    fn current_frame_len(&self) -> Option<usize> {
        None
    }
    fn channels(&self) -> u16 {
        self.channels as u16
    }
    fn sample_rate(&self) -> u32 {
        self.inner.sample_rate()
    }
    fn total_duration(&self) -> Option<Duration> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cut(a: u64, b: u64) -> Cut {
        Cut { from_ms: a, to_ms: b }
    }

    #[test]
    fn clean_merges_and_clamps() {
        let c = clean(vec![cut(500, 300), cut(400, 700), cut(50, 60), cut(900, 2000)], 100, 1000);
        assert_eq!(c, vec![cut(300, 700), cut(900, 1000)]);
        assert_eq!(removed_ms(&c, 100, Some(1000)), 500);
        // Tout enlever : refusé.
        assert!(clean(vec![cut(0, 1000)], 0, 1000).is_empty());
        assert_eq!(fingerprint(&[]), 0);
        assert_ne!(fingerprint(&c), 0);
    }

    #[test]
    fn kept_wav_leaves_out_the_removed_passage() {
        // 2 s stéréo à 8 kHz, un passage d'une seconde enlevé au milieu.
        let dir = std::env::temp_dir().join(format!("sb-cuts-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("son.wav");
        let data: Vec<f32> = (0..32_000).map(|i| ((i / 2) as f32 * 0.05).sin() * 0.5).collect();
        std::fs::write(&path, crate::calibration::wav_bytes_of(&data, 2, 8000)).unwrap();
        let analysis = crate::analysis::analyze(&path).unwrap();
        assert!(analysis.duration_ms.abs_diff(2000) < 5);
        let mut sound = crate::config::Sound::new("a".into(), "a".into(), path, analysis);
        assert!(kept_wav(&sound).is_none());
        sound.cuts = vec![cut(500, 1500)];
        let wav = kept_wav(&sound).unwrap().unwrap();
        // En-tête de 44 octets, puis 1 s × 8000 trames × 2 voies × 2 octets.
        assert_eq!(wav.len(), 44 + 32_000);
        assert_eq!(u16::from_le_bytes([wav[22], wav[23]]), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn skips_the_removed_passage() {
        // 1 kHz mono, valeur = numéro de l'échantillon.
        let data: Vec<f32> = (0..1000).map(|i| i as f32).collect();
        let src = rodio::buffer::SamplesBuffer::new(1, 1000, data);
        let out: Vec<f32> = SkipCuts::new(src, 0, &[cut(100, 600)]).collect();
        assert_eq!(out.len(), 500);
        assert_eq!(out[50], 50.0);
        // Juste après le saut on lit l'échantillon 600 (fondu en cours), puis 610.
        assert_eq!(out[110], 610.0);
    }
}
