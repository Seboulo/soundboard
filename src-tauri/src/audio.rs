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
use crate::denoise::{self, MicFilters, MicProcessor};
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
use std::time::{Duration, Instant};

/// Morceau du nom du périphérique de sortie de VB-Audio Virtual Cable.
pub const VB_CABLE_OUTPUT: &str = "CABLE Input";


type Reply = Sender<Result<(), String>>;
type PlayReply = Sender<Result<PlayOutcome, String>>;

/// Ce qu'il faut pour jouer un son (découpe et gain déjà calculés).
pub struct PlayRequest {
    pub id: String,
    pub path: PathBuf,
    /// Volume du son × égalisation.
    pub gain: f32,
    pub start_ms: u64,
    pub end_ms: Option<u64>,
    /// Durée jouée, pour la barre de progression.
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum PlayOutcome {
    Started,
    Stopped,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PlayingStatus {
    pub id: String,
    pub elapsed_ms: u64,
    pub duration_ms: Option<u64>,
}

/// Identifiant réservé à la pré-écoute dans l'éditeur (casque uniquement).
pub const PREVIEW_ID: &str = "__preview";

enum Command {
    Configure(Settings, Reply),
    /// Joue le son, ou l'arrête s'il est déjà en cours.
    Toggle(PlayRequest, Option<PlayReply>),
    Preview(PlayRequest, Reply),
    StopAll,
}

#[derive(Clone)]
pub struct AudioHandle {
    tx: Sender<Command>,
}

impl AudioHandle {
    /// `on_status` reçoit la liste des sons en cours ~10 fois par seconde
    /// pendant la lecture, puis une liste vide quand tout est fini.
    pub fn spawn(on_status: impl Fn(Vec<PlayingStatus>) + Send + 'static) -> Self {
        let (tx, rx) = mpsc::channel();
        thread::Builder::new()
            .name("audio".into())
            .spawn(move || run(rx, on_status))
            .expect("impossible de démarrer le thread audio");
        Self { tx }
    }

    pub fn configure(&self, settings: Settings) -> Result<(), String> {
        let (reply, rx) = mpsc::channel();
        self.send(Command::Configure(settings, reply))?;
        wait(rx)
    }

    pub fn toggle(&self, req: PlayRequest) -> Result<PlayOutcome, String> {
        let (reply, rx) = mpsc::channel();
        self.send(Command::Toggle(req, Some(reply)))?;
        wait(rx)
    }

    /// Variante sans attente, utilisée par les raccourcis globaux.
    pub fn toggle_detached(&self, req: PlayRequest) {
        let _ = self.send(Command::Toggle(req, None));
    }

    pub fn preview(&self, req: PlayRequest) -> Result<(), String> {
        let (reply, rx) = mpsc::channel();
        self.send(Command::Preview(req, reply))?;
        wait(rx)
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

fn wait<T>(rx: Receiver<Result<T, String>>) -> Result<T, String> {
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

struct Playing {
    id: String,
    sinks: Vec<Sink>,
    started: Instant,
    duration_ms: Option<u64>,
}

#[derive(Default)]
struct Engine {
    settings: Settings,
    monitor: Option<Output>,
    virt: Option<Output>,
    mic_stream: Option<cpal::Stream>,
    mic_sink: Option<Sink>,
    playing: Vec<Playing>,
}

fn run(rx: Receiver<Command>, on_status: impl Fn(Vec<PlayingStatus>)) {
    let mut engine = Engine::default();
    let mut last_status: Vec<PlayingStatus> = Vec::new();
    loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(cmd) => engine.handle(cmd),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        engine.playing.retain(|p| p.sinks.iter().any(|s| !s.empty()));
        let status = engine.status();
        if !(status.is_empty() && last_status.is_empty()) {
            on_status(status.clone());
        }
        last_status = status;
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
        let filters = MicFilters {
            denoise: self.settings.noise_suppression,
            gate: self.settings.voice_gate,
            gate_threshold: self.settings.gate_threshold.clamp(0.0, 1.0),
        };

        // Le micro sort du filtre en mono 48 kHz. ~100 ms de marge : au-delà on
        // jette le plus ancien pour garder une latence basse si les horloges du
        // micro et du câble dérivent.
        let max_len = denoise::RATE as usize / 10;
        let buffer = Arc::new(Mutex::new(VecDeque::<f32>::with_capacity(max_len * 2)));
        let new_filter = || {
            let mut processor = MicProcessor::new(channels, sample_rate, filters);
            move |samples: &mut dyn Iterator<Item = f32>| -> Vec<f32> {
                let mut out = Vec::new();
                processor.process(samples, &mut out);
                out
            }
        };

        let err_fn = |e| eprintln!("erreur micro : {e}");
        let stream = match format {
            SampleFormat::F32 => {
                let buf = buffer.clone();
                let mut filter = new_filter();
                device.build_input_stream(
                    &config,
                    move |data: &[f32], _: &_| {
                        push(&buf, filter(&mut data.iter().copied()), max_len)
                    },
                    err_fn,
                    None,
                )
            }
            SampleFormat::I16 => {
                let buf = buffer.clone();
                let mut filter = new_filter();
                device.build_input_stream(
                    &config,
                    move |data: &[i16], _: &_| {
                        let mut it = data.iter().map(|s| *s as f32 / 32768.0);
                        push(&buf, filter(&mut it), max_len)
                    },
                    err_fn,
                    None,
                )
            }
            SampleFormat::U16 => {
                let buf = buffer.clone();
                let mut filter = new_filter();
                device.build_input_stream(
                    &config,
                    move |data: &[u16], _: &_| {
                        let mut it = data.iter().map(|s| (*s as f32 - 32768.0) / 32768.0);
                        push(&buf, filter(&mut it), max_len)
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
            channels: 1,
            sample_rate: denoise::RATE,
        });

        self.mic_stream = Some(stream);
        self.mic_sink = Some(sink);
        Ok(())
    }


    fn handle(&mut self, cmd: Command) {
        match cmd {
            Command::Configure(settings, reply) => {
                let _ = reply.send(self.configure(settings));
            }
            Command::Toggle(req, reply) => {
                let res = self.toggle(req);
                match reply {
                    Some(reply) => {
                        let _ = reply.send(res);
                    }
                    None => {
                        if let Err(e) = res {
                            eprintln!("lecture impossible : {e}");
                        }
                    }
                }
            }
            Command::Preview(req, reply) => {
                self.stop(PREVIEW_ID);
                let res = self.start(req, true);
                let _ = reply.send(res);
            }
            Command::StopAll => self.stop_all(),
        }
    }

    fn status(&self) -> Vec<PlayingStatus> {
        self.playing
            .iter()
            .filter(|p| p.id != PREVIEW_ID)
            .map(|p| PlayingStatus {
                id: p.id.clone(),
                elapsed_ms: p.started.elapsed().as_millis() as u64,
                duration_ms: p.duration_ms,
            })
            .collect()
    }

    fn toggle(&mut self, req: PlayRequest) -> Result<PlayOutcome, String> {
        if self.stop(&req.id) {
            return Ok(PlayOutcome::Stopped);
        }
        if self.settings.stop_previous {
            self.stop_all();
        }
        self.start(req, false)?;
        Ok(PlayOutcome::Started)
    }

    fn start(&mut self, req: PlayRequest, monitor_only: bool) -> Result<(), String> {
        let mut targets = vec![(&self.monitor, self.settings.monitor_volume)];
        if !monitor_only {
            targets.push((&self.virt, self.settings.master_volume));
        }
        let mut sinks = Vec::new();
        for (output, out_volume) in targets {
            let Some(output) = output else { continue };
            let sink = Sink::try_new(&output.handle).map_err(|e| e.to_string())?;
            sink.set_volume(out_volume);
            sink.append(open_source(&req)?);
            sinks.push(sink);
        }
        if sinks.is_empty() {
            return Err("aucune sortie audio configurée".into());
        }
        self.playing.push(Playing {
            id: req.id,
            sinks,
            started: Instant::now(),
            duration_ms: req.duration_ms,
        });
        Ok(())
    }

    /// Arrête un son ; renvoie vrai s'il était en cours.
    fn stop(&mut self, id: &str) -> bool {
        let before = self.playing.len();
        self.playing.retain(|p| {
            if p.id == id {
                p.sinks.iter().for_each(Sink::stop);
            }
            p.id != id
        });
        self.playing.len() != before
    }

    fn stop_all(&mut self) {
        for p in self.playing.drain(..) {
            p.sinks.iter().for_each(Sink::stop);
        }
    }
}

fn open_source(req: &PlayRequest) -> Result<impl Source<Item = i16> + Send, String> {
    let file = File::open(&req.path).map_err(|e| format!("fichier illisible : {e}"))?;
    let decoder =
        Decoder::new(BufReader::new(file)).map_err(|e| format!("format audio non reconnu : {e}"))?;
    let length = match req.end_ms {
        Some(end) => Duration::from_millis(end.saturating_sub(req.start_ms)),
        None => Duration::from_secs(24 * 3600),
    };
    Ok(decoder
        .skip_duration(Duration::from_millis(req.start_ms))
        .take_duration(length)
        .amplify(req.gain))
}

fn push(buf: &Mutex<VecDeque<f32>>, samples: Vec<f32>, max_len: usize) {
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
