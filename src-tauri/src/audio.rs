//! Moteur audio.
//!
//! Chaque son est joué deux fois en parallèle : dans le casque (pour que tu
//! l'entendes) et dans le câble virtuel (que Discord utilise comme micro).
//! Le vrai micro est aussi capté et renvoyé dans le câble virtuel, pour que
//! ta voix et les sons arrivent ensemble dans Discord.
//!
//! Les flux audio ne sont pas `Send` sur toutes les plateformes, donc tout
//! vit dans un thread dédié piloté par des messages.

use crate::config::Settings;
use rodio::cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use rodio::cpal::{self, SampleFormat};
use rodio::{Decoder, OutputStream, OutputStreamHandle, Sink, Source};
use serde::Serialize;
use std::collections::VecDeque;
use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// Morceau du nom du périphérique de sortie de VB-Audio Virtual Cable.
pub const VB_CABLE_OUTPUT: &str = "CABLE Input";

type Reply = Sender<Result<(), String>>;

enum Command {
    Configure(Settings, Reply),
    Play {
        path: PathBuf,
        volume: f32,
        reply: Option<Reply>,
    },
    StopAll,
}

#[derive(Clone)]
pub struct AudioHandle {
    tx: Sender<Command>,
}

impl AudioHandle {
    pub fn spawn() -> Self {
        let (tx, rx) = mpsc::channel();
        thread::Builder::new()
            .name("audio".into())
            .spawn(move || run(rx))
            .expect("impossible de démarrer le thread audio");
        Self { tx }
    }

    pub fn configure(&self, settings: Settings) -> Result<(), String> {
        let (reply, rx) = mpsc::channel();
        self.send(Command::Configure(settings, reply))?;
        wait(rx)
    }

    pub fn play(&self, path: PathBuf, volume: f32) -> Result<(), String> {
        let (reply, rx) = mpsc::channel();
        self.send(Command::Play {
            path,
            volume,
            reply: Some(reply),
        })?;
        wait(rx)
    }

    /// Variante sans attente, utilisée par les raccourcis globaux.
    pub fn play_detached(&self, path: PathBuf, volume: f32) {
        let _ = self.send(Command::Play {
            path,
            volume,
            reply: None,
        });
    }

    pub fn stop_all(&self) {
        let _ = self.send(Command::StopAll);
    }

    fn send(&self, cmd: Command) -> Result<(), String> {
        self.tx
            .send(cmd)
            .map_err(|_| "le moteur audio s'est arrêté".to_string())
    }
}

fn wait(rx: Receiver<Result<(), String>>) -> Result<(), String> {
    rx.recv_timeout(Duration::from_secs(10))
        .unwrap_or_else(|_| Err("le moteur audio ne répond pas".into()))
}

#[derive(Debug, Serialize)]
pub struct DeviceList {
    pub outputs: Vec<String>,
    pub inputs: Vec<String>,
    pub default_output: Option<String>,
    pub default_input: Option<String>,
    /// Nom de la sortie VB-Cable si elle est installée.
    pub vb_cable: Option<String>,
}

pub fn list_devices() -> DeviceList {
    let host = cpal::default_host();
    let names = |it: Option<Vec<cpal::Device>>| -> Vec<String> {
        it.unwrap_or_default()
            .iter()
            .filter_map(|d| d.name().ok())
            .collect()
    };
    let outputs = names(host.output_devices().ok().map(|d| d.collect()));
    let inputs = names(host.input_devices().ok().map(|d| d.collect()));
    let vb_cable = outputs
        .iter()
        .find(|n| n.contains(VB_CABLE_OUTPUT))
        .cloned();
    DeviceList {
        default_output: host.default_output_device().and_then(|d| d.name().ok()),
        default_input: host.default_input_device().and_then(|d| d.name().ok()),
        outputs,
        inputs,
        vb_cable,
    }
}

fn find_output(name: Option<&str>) -> Option<cpal::Device> {
    let host = cpal::default_host();
    match name {
        None => host.default_output_device(),
        Some(name) => host
            .output_devices()
            .ok()?
            .find(|d| d.name().map(|n| n == name).unwrap_or(false)),
    }
}

fn find_input(name: Option<&str>) -> Option<cpal::Device> {
    let host = cpal::default_host();
    match name {
        None => host.default_input_device(),
        Some(name) => host
            .input_devices()
            .ok()?
            .find(|d| d.name().map(|n| n == name).unwrap_or(false)),
    }
}

struct Output {
    _stream: OutputStream,
    handle: OutputStreamHandle,
}

impl Output {
    fn open(device: &cpal::Device) -> Result<Self, String> {
        let (stream, handle) = OutputStream::try_from_device(device).map_err(|e| e.to_string())?;
        Ok(Self {
            _stream: stream,
            handle,
        })
    }
}

#[derive(Default)]
struct Engine {
    settings: Settings,
    monitor: Option<Output>,
    virt: Option<Output>,
    mic_stream: Option<cpal::Stream>,
    mic_sink: Option<Sink>,
    playing: Vec<Sink>,
}

fn run(rx: Receiver<Command>) {
    let mut engine = Engine::default();
    while let Ok(cmd) = rx.recv() {
        engine.playing.retain(|s| !s.empty());
        match cmd {
            Command::Configure(settings, reply) => {
                let _ = reply.send(engine.configure(settings));
            }
            Command::Play {
                path,
                volume,
                reply,
            } => {
                let res = engine.play(&path, volume);
                if let Some(reply) = reply {
                    let _ = reply.send(res);
                } else if let Err(e) = res {
                    eprintln!("lecture impossible ({}): {e}", path.display());
                }
            }
            Command::StopAll => engine.stop_all(),
        }
    }
}

impl Engine {
    fn configure(&mut self, settings: Settings) -> Result<(), String> {
        self.stop_all();
        self.mic_sink = None;
        self.mic_stream = None;
        self.monitor = None;
        self.virt = None;
        self.settings = settings;

        let mut errors = Vec::new();

        match find_output(self.settings.monitor_device.as_deref()) {
            Some(dev) => match Output::open(&dev) {
                Ok(o) => self.monitor = Some(o),
                Err(e) => errors.push(format!("casque : {e}")),
            },
            None => errors.push("casque introuvable".into()),
        }

        // Sans câble virtuel choisi, on ne joue que dans le casque : jouer deux
        // fois sur la sortie par défaut doublerait le son.
        if let Some(name) = self.settings.virtual_device.clone() {
            match find_output(Some(&name)) {
                Some(dev) => match Output::open(&dev) {
                    Ok(o) => self.virt = Some(o),
                    Err(e) => errors.push(format!("câble virtuel : {e}")),
                },
                None => errors.push(format!("câble virtuel « {name} » introuvable")),
            }
        }

        if self.settings.mic_enabled && self.virt.is_some() {
            if let Err(e) = self.start_mic() {
                errors.push(format!("micro : {e}"));
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join(" ; "))
        }
    }

    fn start_mic(&mut self) -> Result<(), String> {
        let virt = self.virt.as_ref().ok_or("pas de câble virtuel")?;
        let device = find_input(self.settings.mic_device.as_deref()).ok_or("micro introuvable")?;
        if device.name().is_ok_and(|n| n.contains("CABLE Output")) {
            // Le câble renverrait dans lui-même : larsen garanti.
            return Err("le micro choisi est le câble virtuel, choisis ton vrai micro".into());
        }
        let supported = device.default_input_config().map_err(|e| e.to_string())?;
        let channels = supported.channels();
        let sample_rate = supported.sample_rate().0;
        let format = supported.sample_format();
        let config: cpal::StreamConfig = supported.into();

        // ~100 ms de marge : au-delà on jette le plus ancien pour garder une
        // latence basse si les horloges du micro et du câble dérivent.
        let max_len = (sample_rate as usize * channels as usize) / 10;
        let buffer = Arc::new(Mutex::new(VecDeque::<f32>::with_capacity(max_len * 2)));

        let err_fn = |e| eprintln!("erreur micro : {e}");
        let stream = match format {
            SampleFormat::F32 => {
                let buf = buffer.clone();
                device.build_input_stream(
                    &config,
                    move |data: &[f32], _: &_| push(&buf, data.iter().copied(), max_len),
                    err_fn,
                    None,
                )
            }
            SampleFormat::I16 => {
                let buf = buffer.clone();
                device.build_input_stream(
                    &config,
                    move |data: &[i16], _: &_| {
                        push(&buf, data.iter().map(|s| *s as f32 / 32768.0), max_len)
                    },
                    err_fn,
                    None,
                )
            }
            SampleFormat::U16 => {
                let buf = buffer.clone();
                device.build_input_stream(
                    &config,
                    move |data: &[u16], _: &_| {
                        push(
                            &buf,
                            data.iter().map(|s| (*s as f32 - 32768.0) / 32768.0),
                            max_len,
                        )
                    },
                    err_fn,
                    None,
                )
            }
            other => return Err(format!("format de micro non géré : {other:?}")),
        }
        .map_err(|e| e.to_string())?;
        stream.play().map_err(|e| e.to_string())?;

        let sink = Sink::try_new(&virt.handle).map_err(|e| e.to_string())?;
        sink.set_volume(self.settings.mic_volume);
        sink.append(MicSource {
            buffer,
            local: VecDeque::new(),
            channels,
            sample_rate,
        });

        self.mic_stream = Some(stream);
        self.mic_sink = Some(sink);
        Ok(())
    }

    fn play(&mut self, path: &PathBuf, volume: f32) -> Result<(), String> {
        if self.settings.stop_previous {
            self.stop_all();
        }
        let targets = [
            (&self.monitor, self.settings.monitor_volume),
            (&self.virt, self.settings.master_volume),
        ];
        let mut played = false;
        for (output, out_volume) in targets {
            let Some(output) = output else { continue };
            let file = File::open(path).map_err(|e| format!("fichier illisible : {e}"))?;
            let source = Decoder::new(BufReader::new(file))
                .map_err(|e| format!("format audio non reconnu : {e}"))?;
            let sink = Sink::try_new(&output.handle).map_err(|e| e.to_string())?;
            sink.set_volume(volume * out_volume);
            sink.append(source);
            self.playing.push(sink);
            played = true;
        }
        if played {
            Ok(())
        } else {
            Err("aucune sortie audio configurée".into())
        }
    }

    fn stop_all(&mut self) {
        for sink in self.playing.drain(..) {
            sink.stop();
        }
    }
}

fn push(buf: &Mutex<VecDeque<f32>>, samples: impl Iterator<Item = f32>, max_len: usize) {
    let Ok(mut buf) = buf.lock() else { return };
    buf.extend(samples);
    if buf.len() > max_len {
        let excess = buf.len() - max_len;
        buf.drain(..excess);
    }
}

/// Source rodio infinie qui lit les échantillons du micro. rodio s'occupe
/// de convertir la fréquence et le nombre de canaux vers ceux du câble.
struct MicSource {
    buffer: Arc<Mutex<VecDeque<f32>>>,
    local: VecDeque<f32>,
    channels: u16,
    sample_rate: u32,
}

impl Iterator for MicSource {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        if self.local.is_empty() {
            // On récupère par paquets pour ne pas verrouiller à chaque échantillon.
            if let Ok(mut buf) = self.buffer.lock() {
                let n = buf.len() - buf.len() % self.channels as usize;
                self.local.extend(buf.drain(..n));
            }
            if self.local.is_empty() {
                // Rien de capté : un court silence, aligné sur les canaux.
                self.local
                    .extend(std::iter::repeat_n(0.0, 64 * self.channels as usize));
            }
        }
        self.local.pop_front()
    }
}

impl Source for MicSource {
    fn current_frame_len(&self) -> Option<usize> {
        None
    }
    fn channels(&self) -> u16 {
        self.channels
    }
    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
    fn total_duration(&self) -> Option<Duration> {
        None
    }
}
