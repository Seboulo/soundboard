//! Texte → voix : les voix de Windows lisent une phrase, hors ligne.

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct VoiceInfo {
    pub id: String,
    pub name: String,
    pub language: String,
}

/// Voix installées (les françaises en premier).
pub fn voices() -> Result<Vec<VoiceInfo>, String> {
    imp::voices().map(|mut v| {
        v.sort_by_key(|x| (!x.language.starts_with("fr"), x.name.clone()));
        v
    })
}

/// Lit `text` et renvoie un fichier WAV. `rate` : vitesse (0,5 à 2).
pub fn speak(text: &str, voice: Option<&str>, rate: f64) -> Result<Vec<u8>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("écris d'abord ce que la voix doit dire".into());
    }
    if text.chars().count() > 1000 {
        return Err("texte trop long (1000 caractères au plus)".into());
    }
    imp::speak(text, voice, rate.clamp(0.5, 2.0))
}

#[cfg(windows)]
mod imp {
    use super::VoiceInfo;
    use windows::core::HSTRING;
    use windows::Media::SpeechSynthesis::SpeechSynthesizer;
    use windows::Storage::Streams::DataReader;
    use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

    fn init() {
        // Déjà initialisé sur ce fil : sans importance.
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
    }

    pub fn voices() -> Result<Vec<VoiceInfo>, String> {
        init();
        let all = SpeechSynthesizer::AllVoices().map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for v in all {
            out.push(VoiceInfo {
                id: v.Id().map(|s| s.to_string()).unwrap_or_default(),
                name: v.DisplayName().map(|s| s.to_string()).unwrap_or_default(),
                language: v.Language().map(|s| s.to_string()).unwrap_or_default().to_lowercase(),
            });
        }
        Ok(out)
    }

    pub fn speak(text: &str, voice: Option<&str>, rate: f64) -> Result<Vec<u8>, String> {
        init();
        let err = |e: windows::core::Error| format!("la voix de Windows ne répond pas ({})", e.message());
        let synth = SpeechSynthesizer::new().map_err(err)?;
        if let Some(id) = voice.filter(|v| !v.is_empty()) {
            if let Some(v) = SpeechSynthesizer::AllVoices()
                .map_err(err)?
                .into_iter()
                .find(|v| v.Id().map(|s| s == id).unwrap_or(false))
            {
                synth.SetVoice(&v).map_err(err)?;
            }
        }
        if let Ok(options) = synth.Options() {
            let _ = options.SetSpeakingRate(rate);
        }
        let stream = synth.SynthesizeTextToStreamAsync(&HSTRING::from(text)).map_err(err)?.join().map_err(err)?;
        let size = stream.Size().map_err(err)? as u32;
        let input = stream.GetInputStreamAt(0).map_err(err)?;
        let reader = DataReader::CreateDataReader(&input).map_err(err)?;
        reader.LoadAsync(size).map_err(err)?.join().map_err(err)?;
        let mut data = vec![0u8; size as usize];
        reader.ReadBytes(&mut data).map_err(err)?;
        Ok(data)
    }
}

#[cfg(not(windows))]
mod imp {
    use super::VoiceInfo;

    pub fn voices() -> Result<Vec<VoiceInfo>, String> {
        Ok(Vec::new())
    }

    pub fn speak(_text: &str, _voice: Option<&str>, _rate: f64) -> Result<Vec<u8>, String> {
        Err("les voix ne sont disponibles que sous Windows".into())
    }
}
