//! Table de mixage d'une sortie (casque ou câble).
//!
//! rodio additionne chaque son de son côté et envoie la somme telle quelle :
//! deux sons forts, ou un son et ta voix en même temps, dépassent 0 dBFS et
//! Windows écrête (le son grésille dans Discord). Ici tous les sons d'une
//! sortie passent par un seul bus, qui les additionne par blocs de 5 ms puis
//! baisse doucement le total juste avant qu'il ne sature. Le bloc suivant
//! est déjà connu quand on joue celui-ci : le limiteur voit venir les pics
//! et n'a jamais besoin de couper net.

use rodio::Source;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Plafond du total : -0,5 dBFS.
const CEILING: f32 = 0.944;
/// Le limiteur relâche en ~150 ms.
const RELEASE_SECONDS: f32 = 0.15;

type BoxedSource = Box<dyn Source<Item = i16> + Send>;

/// Commande d'un son en cours sur le bus.
#[derive(Clone)]
pub struct Voice {
    volume: Arc<AtomicU32>,
    stop: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
}

impl Voice {
    pub fn set_volume(&self, v: f32) {
        self.volume.store(v.max(0.0).to_bits(), Ordering::Relaxed);
    }
    /// Arrête le son tout de suite (au bloc suivant).
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
    /// Le son est fini (ou arrêté) et a quitté le bus.
    pub fn is_done(&self) -> bool {
        self.done.load(Ordering::Relaxed)
    }
}

struct Track {
    source: BoxedSource,
    channels: usize,
    voice: Voice,
    /// Volume appliqué au bloc précédent (les changements glissent sur un bloc).
    gain: f32,
}

/// Côté moteur : ajoute des sons au bus.
#[derive(Clone)]
pub struct Bus {
    tracks: Arc<Mutex<Vec<Track>>>,
    rate: u32,
}

impl Bus {
    /// Crée le bus et la source à donner à rodio.
    pub fn new(channels: u16, rate: u32) -> (Self, BusSource) {
        let channels = channels.max(1);
        let rate = rate.max(1);
        let tracks = Arc::new(Mutex::new(Vec::new()));
        let block = (rate as usize / 200).max(32);
        let source = BusSource {
            tracks: tracks.clone(),
            channels: channels as usize,
            rate,
            block,
            mix: vec![0.0; block * channels as usize],
            ahead: vec![0.0; block * channels as usize],
            ahead_target: 1.0,
            out: Vec::with_capacity(block * channels as usize),
            out_pos: 0,
            gain: 1.0,
            release_step: block as f32 / (rate as f32 * RELEASE_SECONDS),
            frame: Vec::with_capacity(8),
            primed: false,
        };
        (Self { tracks, rate }, source)
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    /// Joue `source` (déjà à la fréquence du bus) à ce volume.
    pub fn add(&self, source: BoxedSource, volume: f32) -> Voice {
        let voice = Voice {
            volume: Arc::new(AtomicU32::new(volume.max(0.0).to_bits())),
            stop: Arc::new(AtomicBool::new(false)),
            done: Arc::new(AtomicBool::new(false)),
        };
        let track = Track {
            channels: source.channels().max(1) as usize,
            source,
            voice: voice.clone(),
            gain: volume.max(0.0),
        };
        if let Ok(mut tracks) = self.tracks.lock() {
            tracks.push(track);
        }
        voice
    }
}

/// Côté rodio : source infinie qui sort le mélange limité.
pub struct BusSource {
    tracks: Arc<Mutex<Vec<Track>>>,
    channels: usize,
    rate: u32,
    /// Taille d'un bloc, en trames.
    block: usize,
    mix: Vec<f32>,
    /// Bloc mélangé d'avance (celui qui sera joué au prochain tour).
    ahead: Vec<f32>,
    ahead_target: f32,
    out: Vec<f32>,
    out_pos: usize,
    gain: f32,
    release_step: f32,
    frame: Vec<f32>,
    primed: bool,
}

impl BusSource {
    /// Mélange le bloc suivant dans `mix` et renvoie le gain qu'il lui faut.
    fn mix_block(&mut self) -> f32 {
        self.mix.iter_mut().for_each(|v| *v = 0.0);
        let ch = self.channels;
        if let Ok(mut tracks) = self.tracks.lock() {
            for track in tracks.iter_mut() {
                if track.voice.stop.load(Ordering::Relaxed) {
                    continue;
                }
                let target = f32::from_bits(track.voice.volume.load(Ordering::Relaxed));
                let start = track.gain;
                let step = (target - start) / self.block as f32;
                for f in 0..self.block {
                    self.frame.clear();
                    self.frame.extend((&mut track.source).take(track.channels).map(|s| s as f32 / 32768.0));
                    if self.frame.len() < track.channels {
                        track.voice.done.store(true, Ordering::Relaxed);
                        break;
                    }
                    let g = start + step * f as f32;
                    let dst = &mut self.mix[f * ch..(f + 1) * ch];
                    if track.channels == ch {
                        for (d, s) in dst.iter_mut().zip(&self.frame) {
                            *d += s * g;
                        }
                    } else if track.channels == 1 {
                        dst.iter_mut().for_each(|d| *d += self.frame[0] * g);
                    } else if ch == 1 {
                        dst[0] += self.frame.iter().sum::<f32>() / track.channels as f32 * g;
                    } else {
                        for (i, d) in dst.iter_mut().enumerate() {
                            *d += self.frame[i % track.channels] * g;
                        }
                    }
                }
                track.gain = target;
            }
            tracks.retain(|t| {
                let stopped = t.voice.stop.load(Ordering::Relaxed);
                if stopped {
                    t.voice.done.store(true, Ordering::Relaxed);
                }
                !stopped && !t.voice.done.load(Ordering::Relaxed)
            });
        }
        let peak = self.mix.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        if peak > CEILING {
            CEILING / peak
        } else {
            1.0
        }
    }

    fn refill(&mut self) {
        if !self.primed {
            self.ahead_target = self.mix_block();
            std::mem::swap(&mut self.ahead, &mut self.mix);
            self.primed = true;
        }
        // On joue le bloc d'avance ; le suivant est mélangé pour savoir
        // jusqu'où baisser d'ici la fin de celui-ci.
        let current_target = self.ahead_target;
        let next_target = self.mix_block();
        std::mem::swap(&mut self.ahead, &mut self.mix);
        self.ahead_target = next_target;

        let wanted = current_target.min(next_target);
        let start = self.gain.min(current_target);
        let end = if wanted < start { wanted } else { (start + self.release_step).min(wanted) };
        let ch = self.channels;
        self.out.clear();
        for f in 0..self.block {
            let g = start + (end - start) * (f + 1) as f32 / self.block as f32;
            for c in 0..ch {
                self.out.push((self.mix[f * ch + c] * g).clamp(-1.0, 1.0));
            }
        }
        self.gain = end;
        self.out_pos = 0;
    }
}

impl Iterator for BusSource {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        if self.out_pos >= self.out.len() {
            self.refill();
        }
        let v = self.out[self.out_pos];
        self.out_pos += 1;
        Some(v)
    }
}

impl Source for BusSource {
    fn current_frame_len(&self) -> Option<usize> {
        None
    }
    fn channels(&self) -> u16 {
        self.channels as u16
    }
    fn sample_rate(&self) -> u32 {
        self.rate
    }
    fn total_duration(&self) -> Option<Duration> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rodio::buffer::SamplesBuffer;

    fn tone(amp: f32, frames: usize, channels: u16) -> BoxedSource {
        let data: Vec<i16> = (0..frames)
            .flat_map(|i| {
                let v = ((i as f32 * 0.05).sin() * amp * 32767.0) as i16;
                std::iter::repeat_n(v, channels as usize)
            })
            .collect();
        Box::new(SamplesBuffer::new(channels, 48_000, data))
    }

    #[test]
    fn single_quiet_sound_passes_untouched() {
        let (bus, mut out) = Bus::new(2, 48_000);
        let voice = bus.add(tone(0.5, 4800, 1), 1.0);
        let got: Vec<f32> = (&mut out).take(9600).collect();
        let peak = got.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!((peak - 0.5).abs() < 0.01, "peak {peak}");
        // Mono copié sur les deux canaux.
        assert!(got.chunks(2).all(|f| f[0] == f[1]));
        (&mut out).take(2000).count();
        assert!(voice.is_done());
    }

    #[test]
    fn loud_sum_is_limited_without_clipping() {
        let (bus, mut out) = Bus::new(2, 48_000);
        for _ in 0..3 {
            bus.add(tone(0.8, 48_000, 2), 1.0);
        }
        let got: Vec<f32> = (&mut out).take(96_000).collect();
        let peak = got.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(peak <= CEILING + 1e-4, "peak {peak}");
        assert!(peak > 0.8, "trop baissé : {peak}");
    }

    #[test]
    fn stop_and_volume_changes() {
        let (bus, mut out) = Bus::new(1, 48_000);
        let voice = bus.add(tone(0.5, 48_000, 1), 1.0);
        (&mut out).take(4800).count();
        voice.set_volume(0.0);
        (&mut out).take(960).count();
        assert!((&mut out).take(4800).all(|v| v == 0.0));
        voice.stop();
        (&mut out).take(960).count();
        assert!(voice.is_done());
    }
}
