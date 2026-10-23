//! Lecture des fichiers audio avec symphonia, en source rodio.
//!
//! On n'utilise pas `rodio::Decoder` : il ne donne pas la taille du fichier à
//! symphonia, qui en a besoin pour les M4A/MP4 (ceux de YouTube notamment),
//! et rodio plante alors au lieu de renvoyer une erreur.

use rodio::Source;
use std::fs::File;
use std::path::Path;
use std::time::Duration;
use symphonia::core::audio::{SampleBuffer, SignalSpec};
use symphonia::core::codecs::{Decoder, DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error;
use symphonia::core::formats::{FormatOptions, FormatReader};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

/// Nombre de paquets illisibles tolérés d'affilée avant d'abandonner.
const MAX_DECODE_ERRORS: usize = 20;

pub struct AudioFile {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn Decoder>,
    track_id: u32,
    buffer: Vec<i16>,
    pos: usize,
    spec: SignalSpec,
    total_duration: Option<Duration>,
}

impl AudioFile {
    pub fn open(path: &Path) -> Result<Self, String> {
        let file = File::open(path).map_err(|e| format!("fichier illisible : {e}"))?;
        // `File` donne sa taille à symphonia, contrairement à rodio.
        let mss = MediaSourceStream::new(Box::new(file), Default::default());
        let mut hint = Hint::new();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(ext);
        }
        let format_opts = FormatOptions {
            enable_gapless: true,
            ..Default::default()
        };
        let probed = symphonia::default::get_probe()
            .format(&hint, mss, &format_opts, &MetadataOptions::default())
            .map_err(|e| format!("format audio non reconnu : {e}"))?;
        let format = probed.format;
        let track = format
            .tracks()
            .iter()
            .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
            .ok_or("aucune piste audio lisible dans ce fichier")?;
        let track_id = track.id;
        let total_duration = track
            .codec_params
            .time_base
            .zip(track.codec_params.n_frames)
            .map(|(base, frames)| {
                let t = base.calc_time(frames);
                Duration::from_secs(t.seconds) + Duration::from_secs_f64(t.frac)
            });
        let decoder = symphonia::default::get_codecs()
            .make(&track.codec_params, &DecoderOptions::default())
            .map_err(|e| format!("codec audio non pris en charge : {e}"))?;

        let mut file = Self {
            format,
            decoder,
            track_id,
            buffer: Vec::new(),
            pos: 0,
            spec: SignalSpec::new(44_100, Default::default()),
            total_duration,
        };
        // On décode le premier bloc pour connaître le format de sortie.
        if !file.refill() {
            return Err("fichier audio vide ou illisible".into());
        }
        Ok(file)
    }

    /// Décode le paquet suivant de la piste. Faux à la fin du fichier.
    fn refill(&mut self) -> bool {
        let mut errors = 0;
        loop {
            let packet = match self.format.next_packet() {
                Ok(p) => p,
                Err(_) => return false,
            };
            if packet.track_id() != self.track_id {
                continue;
            }
            match self.decoder.decode(&packet) {
                Ok(decoded) => {
                    if decoded.frames() == 0 {
                        continue;
                    }
                    let spec = *decoded.spec();
                    let mut buf = SampleBuffer::<i16>::new(decoded.capacity() as u64, spec);
                    buf.copy_interleaved_ref(decoded);
                    self.buffer.clear();
                    self.buffer.extend_from_slice(buf.samples());
                    self.pos = 0;
                    self.spec = spec;
                    return true;
                }
                Err(Error::DecodeError(_)) => {
                    errors += 1;
                    if errors > MAX_DECODE_ERRORS {
                        return false;
                    }
                }
                Err(_) => return false,
            }
        }
    }
}

impl Iterator for AudioFile {
    type Item = i16;

    fn next(&mut self) -> Option<i16> {
        if self.pos >= self.buffer.len() && !self.refill() {
            return None;
        }
        let sample = self.buffer[self.pos];
        self.pos += 1;
        Some(sample)
    }
}

impl Source for AudioFile {
    fn current_frame_len(&self) -> Option<usize> {
        // Le format peut changer d'un bloc à l'autre : rodio relit alors
        // canaux et fréquence à la fin du bloc en cours.
        Some(self.buffer.len().saturating_sub(self.pos).max(1))
    }

    fn channels(&self) -> u16 {
        self.spec.channels.count().max(1) as u16
    }

    fn sample_rate(&self) -> u32 {
        self.spec.rate
    }

    fn total_duration(&self) -> Option<Duration> {
        self.total_duration
    }
}
