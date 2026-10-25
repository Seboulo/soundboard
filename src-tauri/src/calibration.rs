//! Test de voix : 5 s de silence puis 5 s de parole, enregistrées brutes et
//! traitées. On en tire le bruit de la pièce, le niveau de la voix, ses
//! crêtes et sa couleur, pour régler le micro et conseiller l'égaliseur.

use crate::config::{MicCalibration, VoiceEq};
use crate::denoise::{Biquad, RATE};
use serde::Serialize;
use std::fs;
use std::path::Path;

/// Durée de chaque partie du test, en secondes.
pub const PHASE_SECONDS: usize = 5;
const FRAME: usize = RATE as usize / 100;

#[derive(Debug, Serialize)]
pub struct VoiceReport {
    pub noise_db: f32,
    pub speech_db: f32,
    pub peak_db: f32,
    /// Aigus de présence (3 kHz) par rapport au bas de la voix (250 Hz), en dB.
    pub tilt_db: f32,
    pub eq: VoiceEq,
    /// Conseils, du plus important au moins important.
    pub advice: Vec<String>,
    /// Dossier des deux enregistrements.
    pub folder: String,
}

fn power_db(p: f64) -> f32 {
    (10.0 * p.max(1e-12).log10()) as f32
}

/// Analyse le son brut du micro (48 kHz mono) : silence, puis parole.
pub fn analyze(raw: &[f32]) -> Result<(MicCalibration, VoiceReport), String> {
    let frames = raw.len() / FRAME;
    let phase = PHASE_SECONDS * 100;
    if frames < phase * 2 {
        return Err("enregistrement trop court".into());
    }
    // Comme le traitement : le grondement sous 80 Hz ne compte pas.
    let mut high_pass = Biquad::high_pass(80.0);
    let mut low_band = Biquad::band_pass(250.0, 0.7);
    let mut presence = Biquad::band_pass(3000.0, 0.7);
    let mut frame_db = Vec::with_capacity(frames);
    let mut bands = Vec::with_capacity(frames);
    for frame in raw.chunks_exact(FRAME) {
        let (mut all, mut low, mut high) = (0.0f64, 0.0f64, 0.0f64);
        for &x in frame {
            let x = high_pass.run(x);
            all += (x * x) as f64;
            low += low_band.run(x).powi(2) as f64;
            high += presence.run(x).powi(2) as f64;
        }
        frame_db.push(power_db(all / FRAME as f64));
        bands.push((all, low, high));
    }

    // Silence : la médiane, pour qu'un clic ou un souffle ne compte pas.
    // On laisse une demi-seconde de marge à chaque changement de partie.
    let mut quiet: Vec<f32> = frame_db[50..phase - 50].to_vec();
    quiet.sort_by(f32::total_cmp);
    let noise_db = quiet[quiet.len() / 2];

    let speech: Vec<usize> = (phase + 50..phase * 2)
        .filter(|&i| frame_db[i] > noise_db + 10.0 && frame_db[i] > -60.0)
        .collect();
    if speech.len() < 100 {
        return Err("je ne t'ai presque pas entendu parler : recommence en parlant normalement pendant les 5 dernières secondes".into());
    }
    let sum = |f: fn(&(f64, f64, f64)) -> f64| speech.iter().map(|&i| f(&bands[i])).sum::<f64>();
    let speech_db = power_db(sum(|b| b.0) / (speech.len() * FRAME) as f64);
    let tilt_db = power_db(sum(|b| b.2)) - power_db(sum(|b| b.1));
    let peak = raw[(phase + 50) * FRAME..].iter().fold(0.0f32, |m, v| m.max(v.abs()));
    let peak_db = 20.0 * peak.max(1e-6).log10();

    // Une voix moyenne a ~11 dB de moins vers 3 kHz que vers 250 Hz.
    let eq = if tilt_db < -17.0 {
        VoiceEq::Clear
    } else if tilt_db > -5.0 {
        VoiceEq::Warm
    } else {
        VoiceEq::Natural
    };

    let mut advice = Vec::new();
    if peak_db > -1.0 {
        advice.push("Ta voix sature le micro : baisse son volume dans Windows (Paramètres > Son > ton micro), sinon elle grésille.".into());
    }
    if speech_db < -42.0 {
        advice.push("Ta voix arrive faible : rapproche le micro ou monte son volume dans Windows.".into());
    }
    if speech_db - noise_db < 20.0 {
        advice.push("Ta voix dépasse peu le bruit de ta pièce : rapproche-toi du micro et garde la réduction de bruit forte.".into());
    } else if noise_db > -50.0 {
        advice.push("Ta pièce est un peu bruyante : garde la réduction de bruit forte.".into());
    }
    match eq {
        VoiceEq::Clear => advice.push("Ton micro sonne un peu sourd : l'égaliseur passe sur « Claire ».".into()),
        VoiceEq::Warm => advice.push("Ton micro sonne un peu dur dans les aigus : l'égaliseur passe sur « Chaude ».".into()),
        VoiceEq::Natural => {}
    }
    if advice.is_empty() {
        advice.push("Tout est bon : ton micro est bien réglé.".into());
    }

    let calibration = MicCalibration { noise_db, speech_db };
    let report = VoiceReport { noise_db, speech_db, peak_db, tilt_db, eq, advice, folder: String::new() };
    Ok((calibration, report))
}

/// Écrit du mono 48 kHz en WAV 16 bits.
pub fn write_wav(path: &Path, samples: &[f32]) -> Result<(), String> {
    let data_len = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + samples.len() * 2);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&RATE.to_le_bytes());
    out.extend_from_slice(&(RATE * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for &s in samples {
        out.extend_from_slice(&((s.clamp(-1.0, 1.0) * 32767.0).round() as i16).to_le_bytes());
    }
    fs::write(path, out).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noise(n: usize, amp: f32, seed: &mut u32) -> Vec<f32> {
        (0..n)
            .map(|_| {
                *seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                ((*seed >> 16) as f32 / 32768.0 - 1.0) * amp
            })
            .collect()
    }

    #[test]
    fn measures_room_and_voice() {
        let mut seed = 3;
        let n = PHASE_SECONDS * RATE as usize;
        let mut raw = noise(n, 0.003, &mut seed);
        // « Voix » : un son riche en harmoniques, haché comme des syllabes.
        let voice: Vec<f32> = (0..n)
            .map(|i| {
                let t = i as f32 / RATE as f32;
                let on = if (t * 4.0).fract() < 0.7 { 1.0 } else { 0.0 };
                let tone: f32 = (1..20).map(|h| (t * 150.0 * h as f32 * std::f32::consts::TAU).sin() / h as f32).sum();
                tone * 0.05 * on
            })
            .collect();
        let room = noise(n, 0.003, &mut seed);
        raw.extend(voice.iter().zip(&room).map(|(v, r)| v + r));
        let (cal, report) = analyze(&raw).unwrap();
        assert!((cal.noise_db + 55.0).abs() < 3.0, "bruit {}", cal.noise_db);
        assert!(cal.speech_db > -30.0 && cal.speech_db < -20.0, "voix {}", cal.speech_db);
        assert!(report.peak_db < -6.0);
        assert!(!report.advice.is_empty());
    }

    #[test]
    fn rejects_a_silent_test() {
        let mut seed = 5;
        let raw = noise(PHASE_SECONDS * 2 * RATE as usize, 0.003, &mut seed);
        assert!(analyze(&raw).is_err());
    }

    #[test]
    fn wav_header_is_valid() {
        let dir = std::env::temp_dir().join("soundboard-wav-test");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.wav");
        write_wav(&path, &[0.0, 0.5, -0.5]).unwrap();
        let bytes = fs::read(&path).unwrap();
        assert_eq!(bytes.len(), 44 + 6);
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(bytes[24..28].try_into().unwrap()), 48_000);
    }
}
