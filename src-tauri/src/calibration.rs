//! Test de voix : 5 s de silence puis 5 s de parole, enregistrées brutes et
//! traitées. On en tire le bruit de la pièce, le niveau de la voix, ses
//! crêtes et sa couleur, pour régler le micro et conseiller l'égaliseur.

use crate::config::{MicCalibration, VoiceEq};
use crate::denoise::{Biquad, EQ_BANDS_HZ, RATE};
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
    /// Corrections de l'égaliseur sur mesure, en dB (bandes de `EQ_BANDS_HZ`).
    pub eq_db: [f32; 6],
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
    let mut filters: Vec<Biquad> = EQ_BANDS_HZ.iter().map(|&f| Biquad::band_pass(f, 1.4)).collect();
    let mut frame_db = Vec::with_capacity(frames);
    // Énergie de chaque trame : totale, puis par bande de l'égaliseur.
    let mut energy = Vec::with_capacity(frames);
    for frame in raw.chunks_exact(FRAME) {
        let mut e = [0.0f64; 7];
        for &x in frame {
            let x = high_pass.run(x);
            e[0] += (x * x) as f64;
            for (b, f) in e[1..].iter_mut().zip(&mut filters) {
                *b += f.run(x).powi(2) as f64;
            }
        }
        frame_db.push(power_db(e[0] / FRAME as f64));
        energy.push(e);
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
    let sum = |k: usize| speech.iter().map(|&i| energy[i][k]).sum::<f64>();
    let speech_db = power_db(sum(0) / (speech.len() * FRAME) as f64);
    let band_db: Vec<f32> = (1..7).map(|k| power_db(sum(k))).collect();
    let peak = raw[(phase + 50) * FRAME..].iter().fold(0.0f32, |m, v| m.max(v.abs()));
    let peak_db = 20.0 * peak.max(1e-6).log10();
    let eq_db = match_eq(&band_db);
    let eq = if eq_db.iter().any(|g| g.abs() >= 2.0) { VoiceEq::Custom } else { VoiceEq::Natural };

    let mut advice = Vec::new();
    if noise_db < -100.0 {
        advice.push("Quand tu te tais, ton micro envoie un silence parfait : un autre logiciel filtre déjà ta voix (Windows « Effets audio » ou « Clarté vocale », logiciel du casque, NVIDIA Broadcast…). Coupe-le : deux antibruits l'un sur l'autre abîment la voix.".into());
    }
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
    if eq == VoiceEq::Custom {
        advice.push(format!("Égaliseur sur mesure pour ton micro : {}.", describe_eq(&eq_db)));
    }
    if advice.is_empty() {
        advice.push("Tout est bon : ton micro est bien réglé.".into());
    }

    let calibration = MicCalibration { noise_db, speech_db, eq_db };
    let report = VoiceReport { noise_db, speech_db, peak_db, eq_db, eq, advice, folder: String::new() };
    Ok((calibration, report))
}

/// Spectre moyen d'une voix parlée équilibrée (Byrne et al., 1994), en dB
/// par rapport à la bande de 300 Hz, pour chaque bande de `EQ_BANDS_HZ`.
const REFERENCE_DB: [f32; 6] = [-2.0, 0.0, -5.5, -11.0, -13.5, -17.0];

/// Corrections qui rapprochent la voix mesurée de la référence, comme un
/// égaliseur d'appariement en studio : on ne corrige que les trois quarts de
/// l'écart (chaque voix a sa couleur), jamais plus de 8 dB (6 dB pour les
/// aigus extrêmes), et on ne remonte jamais le grave (ronflement).
fn match_eq(band_db: &[f32]) -> [f32; 6] {
    let deviation: Vec<f32> = band_db.iter().zip(REFERENCE_DB).map(|(m, r)| m - r).collect();
    // Le niveau global ne compte pas : on se cale sur les médiums.
    let center = (deviation[1] + deviation[2] + deviation[3]) / 3.0;
    let mut eq = [0.0f32; 6];
    for (i, g) in eq.iter_mut().enumerate() {
        let max = if i == 5 { 6.0 } else { 8.0 };
        let min = -8.0;
        let wanted = -(deviation[i] - center) * 0.75;
        let wanted = if i == 0 { wanted.min(0.0) } else { wanted };
        *g = (wanted.clamp(min, max) * 2.0).round() / 2.0;
    }
    eq
}

fn describe_eq(eq_db: &[f32; 6]) -> String {
    const NAMES: [&str; 6] = ["graves", "bas-médiums", "médiums", "présence", "clarté", "aigus"];
    let parts: Vec<String> = NAMES
        .iter()
        .zip(eq_db)
        .filter(|(_, g)| g.abs() >= 1.0)
        .map(|(n, g)| format!("{n} {g:+.0} dB"))
        .collect();
    parts.join(", ")
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
    fn dull_microphone_gets_brighter_eq() {
        // Bandes mesurées d'un micro très sourd (son vrai test de voix).
        let measured = [2.0, 0.0, -13.0, -21.0, -33.0, -40.0];
        let eq = match_eq(&measured);
        assert!(eq[0] <= 0.0, "jamais de grave en plus");
        assert!(eq[4] >= 6.0 && eq[5] >= 5.0, "{eq:?}");
        // Une voix déjà équilibrée ne bouge pas.
        assert!(match_eq(&REFERENCE_DB).iter().all(|g| g.abs() < 0.5));
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
