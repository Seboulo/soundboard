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
use crate::denoise::{self, MicFilters, MicLevel, MicMeter, MicProcessor};
use rodio::cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use rodio::cpal::{self, SampleFormat};
use crate::decoder::AudioFile;
use crate::mixer::{Bus, Voice};
use rodio::{OutputStream, Source};
use serde::Serialize;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// Morceau du nom du périphérique de sortie de VB-Audio Virtual Cable.
pub const VB_CABLE_OUTPUT: &str = "CABLE Input";
/// Côté enregistrement du câble : ce que Discord prend comme micro.
pub const VB_CABLE_CAPTURE: &str = "CABLE Output";


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
    pub looping: bool,
    /// Pas coupé quand un autre son démarre (option « un nouveau son coupe les autres »).
    pub keep_playing: bool,
}

/// Durée des fondus des sons en boucle.
const FADE: Duration = Duration::from_millis(400);

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
    pub looping: bool,
}

/// Ce que le moteur signale à l'interface.
pub enum AudioEvent {
    /// Sons en cours (~10 fois par seconde pendant la lecture, puis une liste vide).
    Playing(Vec<PlayingStatus>),
    /// Niveau du micro, seulement quand l'indicateur est affiché.
    Mic(MicLevel),
}

/// Identifiant réservé à la pré-écoute dans l'éditeur (casque uniquement).
pub const PREVIEW_ID: &str = "__preview";

enum Command {
    Configure(Settings, Reply),
    /// Joue le son, ou l'arrête s'il est déjà en cours.
    Toggle(PlayRequest, Option<PlayReply>),
    Preview(PlayRequest, Reply),
    StopAll,
    Stop(String),
    StopPreview,
    SetMicMuted(bool),
    /// Fait entendre dans le casque ce qui sort du câble (ce que Discord reçoit).
    ListenCable(bool, Reply),
    /// Fait entendre ta voix traitée (antibruit, porte, volume) dans le casque.
    HearMyself(bool, Reply),
}

#[derive(Clone)]
pub struct AudioHandle {
    tx: Sender<Command>,
    meter_on: Arc<AtomicBool>,
}

impl AudioHandle {
    pub fn spawn(on_event: impl Fn(AudioEvent) + Send + 'static) -> Self {
        let (tx, rx) = mpsc::channel();
        let meter_on = Arc::new(AtomicBool::new(false));
        let flag = meter_on.clone();
        thread::Builder::new()
            .name("audio".into())
            .spawn(move || run(rx, flag, on_event))
            .expect("impossible de démarrer le thread audio");
        Self { tx, meter_on }
    }

    /// Coupe ou rallume le micro ; les sons passent toujours.
    pub fn set_mic_muted(&self, muted: bool) {
        let _ = self.send(Command::SetMicMuted(muted));
    }

    /// Active l'envoi du niveau du micro (indicateur des réglages).
    pub fn set_meter(&self, on: bool) {
        self.meter_on.store(on, Ordering::Relaxed);
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

    /// Arrête un son tout de suite (sans fondu).
    pub fn stop(&self, id: &str) {
        let _ = self.send(Command::Stop(id.to_string()));
    }

    pub fn listen_cable(&self, on: bool) -> Result<(), String> {
        let (reply, rx) = mpsc::channel();
        self.send(Command::ListenCable(on, reply))?;
        wait(rx)
    }

    pub fn hear_myself(&self, on: bool) -> Result<(), String> {
        let (reply, rx) = mpsc::channel();
        self.send(Command::HearMyself(on, reply))?;
        wait(rx)
    }

    pub fn stop_preview(&self) {
        let _ = self.send(Command::StopPreview);
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

#[derive(Debug, Clone, Serialize)]
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

/// Fréquence et canaux d'un périphérique tels que Windows les règle.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct DeviceFormat {
    pub rate: u32,
    pub channels: u16,
}

pub fn output_format(name: Option<&str>) -> Option<DeviceFormat> {
    let c = find_output(name)?.default_output_config().ok()?;
    Some(DeviceFormat { rate: c.sample_rate().0, channels: c.channels() })
}

pub fn input_format(name: Option<&str>) -> Option<DeviceFormat> {
    let c = find_input(name)?.default_input_config().ok()?;
    Some(DeviceFormat { rate: c.sample_rate().0, channels: c.channels() })
}

/// Côté enregistrement du câble (« CABLE Output »).
pub fn cable_capture_format() -> Option<DeviceFormat> {
    let device = cpal::default_host()
        .input_devices()
        .ok()?
        .find(|d| d.name().is_ok_and(|n| n.contains(VB_CABLE_CAPTURE)))?;
    let c = device.default_input_config().ok()?;
    Some(DeviceFormat { rate: c.sample_rate().0, channels: c.channels() })
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
    /// Tous les sons de la sortie passent par ce bus (mélange + limiteur).
    bus: Bus,
}

impl Output {
    fn open(device: &cpal::Device) -> Result<Self, String> {
        let config = device.default_output_config().map_err(|e| e.to_string())?;
        let (bus, source) = Bus::new(config.channels(), config.sample_rate().0);
        let (stream, handle) =
            OutputStream::try_from_device_config(device, config).map_err(|e| e.to_string())?;
        handle.play_raw(source).map_err(|e| e.to_string())?;
        Ok(Self { _stream: stream, bus })
    }

    /// Les sons sont convertis à la fréquence de la sortie avant le bus.
    fn rate(&self) -> u32 {
        self.bus.rate()
    }
}

struct Playing {
    id: String,
    voices: Vec<Voice>,
    started: Instant,
    duration_ms: Option<u64>,
    looping: bool,
    keep_playing: bool,
    /// Volume visé par les fondus (bits d'un f32), partagé avec les sources.
    fade_target: Option<Arc<AtomicU32>>,
    /// Arrêt en fondu en cours depuis cet instant.
    stopping: Option<Instant>,
}

impl Playing {
    fn is_finished(&self) -> bool {
        self.voices.iter().all(Voice::is_done) || self.stopping.is_some_and(|t| t.elapsed() > FADE + Duration::from_millis(50))
    }
}

#[derive(Default)]
struct Engine {
    settings: Settings,
    monitor: Option<Output>,
    virt: Option<Output>,
    mic_stream: Option<cpal::Stream>,
    mic_voice: Option<Voice>,
    mic_muted: bool,
    meter: Arc<MicMeter>,
    playing: Vec<Playing>,
    /// Écoute du câble en cours : les sons ne passent plus directement par
    /// le casque, on entend seulement ce qui sort du câble.
    cable_stream: Option<cpal::Stream>,
    cable_voice: Option<Voice>,
    /// Copie de la voix traitée, remplie seulement quand le drapeau est levé.
    mic_tap: Option<(Arc<AtomicBool>, SampleBuffer)>,
    /// Ta voix renvoyée dans le casque (« M'écouter »).
    hear_voice: Option<Voice>,
}

fn run(rx: Receiver<Command>, meter_on: Arc<AtomicBool>, on_event: impl Fn(AudioEvent)) {
    let mut engine = Engine::default();
    let mut last_status: Vec<PlayingStatus> = Vec::new();
    loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(cmd) => engine.handle(cmd),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        engine.playing.retain(|p| {
            let done = p.is_finished();
            if done {
                p.voices.iter().for_each(Voice::stop);
            }
            !done
        });
        let status = engine.status();
        if !(status.is_empty() && last_status.is_empty()) {
            on_event(AudioEvent::Playing(status.clone()));
        }
        last_status = status;
        if meter_on.load(Ordering::Relaxed) && engine.mic_stream.is_some() {
            on_event(AudioEvent::Mic(engine.meter.read()));
        }
    }
}

impl Engine {
    fn configure(&mut self, settings: Settings) -> Result<(), String> {
        self.stop_all();
        self.stop_cable();
        self.stop_hearing();
        self.mic_tap = None;
        if let Some(v) = self.mic_voice.take() {
            v.stop();
        }
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

    fn stop_cable(&mut self) {
        if let Some(v) = self.cable_voice.take() {
            v.stop();
        }
        self.cable_stream = None;
    }

    fn stop_hearing(&mut self) {
        if let Some(v) = self.hear_voice.take() {
            v.stop();
        }
        if let Some((flag, _)) = &self.mic_tap {
            flag.store(false, Ordering::Relaxed);
        }
    }

    /// Volume de ta voix (coupée par le raccourci = silence).
    fn mic_level(&self) -> f32 {
        if self.mic_muted {
            0.0
        } else {
            self.settings.mic_volume
        }
    }

    fn hear_myself(&mut self, on: bool) -> Result<(), String> {
        self.stop_hearing();
        if !on {
            return Ok(());
        }
        let monitor = self.monitor.as_ref().ok_or("casque introuvable")?;
        let (flag, tap) = self
            .mic_tap
            .as_ref()
            .ok_or("ta voix n'est pas envoyée : active « Envoyer ma voix dans Discord » et choisis le câble")?;
        // Écouter le câble ferait entendre ta voix deux fois.
        self.cable_voice.take().inspect(Voice::stop);
        self.cable_stream = None;
        if let Ok(mut buf) = tap.lock() {
            buf.clear();
        }
        flag.store(true, Ordering::Relaxed);
        let source = MicSource::new(tap.clone(), 1, denoise::RATE).convert_samples::<i16>();
        let voice = monitor
            .bus
            .add(Box::new(Resample::new(source, monitor.rate())), self.mic_level());
        self.hear_voice = Some(voice);
        Ok(())
    }

    fn listen_cable(&mut self, on: bool) -> Result<(), String> {
        self.stop_cable();
        self.stop_hearing();
        if !on {
            return Ok(());
        }
        let monitor = self.monitor.as_ref().ok_or("casque introuvable")?;
        let device = cpal::default_host()
            .input_devices()
            .map_err(|e| e.to_string())?
            .find(|d| d.name().is_ok_and(|n| n.contains(VB_CABLE_CAPTURE)))
            .ok_or("« CABLE Output » introuvable : VB-Cable est-il installé ?")?;
        let supported = device.default_input_config().map_err(|e| e.to_string())?;
        let channels = supported.channels();
        let rate = supported.sample_rate().0;
        let format = supported.sample_format();
        let config: cpal::StreamConfig = supported.into();
        let max_len = (rate as usize * channels as usize) / 5;
        let buffer = Arc::new(Mutex::new(VecDeque::with_capacity(max_len * 2)));
        let filter: CaptureFilter = Box::new(|samples| samples.collect());
        let stream = open_capture(&device, &config, format, buffer.clone(), max_len, filter)?;
        stream.play().map_err(|e| e.to_string())?;
        let source = MicSource::new(buffer, channels, rate).convert_samples::<i16>();
        let voice = monitor
            .bus
            .add(Box::new(Resample::new(source, monitor.rate())), self.settings.monitor_volume);
        self.cable_stream = Some(stream);
        self.cable_voice = Some(voice);
        Ok(())
    }

    fn start_mic(&mut self) -> Result<(), String> {
        let virt = self.virt.as_ref().ok_or("pas de câble virtuel")?;
        let device = find_input(self.settings.mic_device.as_deref()).ok_or("micro introuvable")?;
        if device.name().is_ok_and(|n| n.contains(VB_CABLE_CAPTURE)) {
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
            strong: self.settings.strong_denoise,
            gate: self.settings.voice_gate,
            gate_auto: self.settings.gate_auto,
            gate_threshold: self.settings.gate_threshold.clamp(0.0, 1.0),
            level: self.settings.voice_leveler,
        };

        // Le micro sort du filtre en mono 48 kHz. 200 ms de marge au plus :
        // la lecture rattrape en douceur la dérive des horloges (voir
        // `MicSource`), ce plafond ne sert qu'en cas de gros à-coup.
        let max_len = denoise::RATE as usize / 5;
        let buffer = Arc::new(Mutex::new(VecDeque::<f32>::with_capacity(max_len * 2)));
        let mut processor =
            MicProcessor::new(channels, sample_rate, filters, self.settings.voice_effect, self.meter.clone());

        // Le nettoyage (IA comprise) tourne dans son propre fil : le fil du
        // micro ne fait que copier, il ne prend jamais de retard (un retard
        // là fait perdre des bouts de son au pilote = micro-coupures).
        let (raw_tx, raw_rx) = mpsc::sync_channel::<Vec<f32>>(64);
        let out = buffer.clone();
        let tap_on = Arc::new(AtomicBool::new(false));
        let tap = Arc::new(Mutex::new(VecDeque::<f32>::with_capacity(max_len * 2)));
        let (tap_on2, tap2) = (tap_on.clone(), tap.clone());
        thread::Builder::new()
            .name("micro".into())
            .spawn(move || {
                // S'arrête tout seul quand le micro est fermé (raw_tx lâché).
                while let Ok(chunk) = raw_rx.recv() {
                    let mut cleaned = Vec::with_capacity(chunk.len());
                    processor.process(chunk.into_iter(), &mut cleaned);
                    if tap_on2.load(Ordering::Relaxed) {
                        push(&tap2, cleaned.clone(), max_len);
                    }
                    push(&out, cleaned, max_len);
                }
            })
            .map_err(|e| e.to_string())?;
        let filter: CaptureFilter = Box::new(move |samples| {
            if raw_tx.try_send(samples.collect()).is_err() {
                CAPTURE_OVERFLOWS.fetch_add(1, Ordering::Relaxed);
            }
            Vec::new()
        });
        let dummy = Arc::new(Mutex::new(VecDeque::new()));
        let stream = open_capture(&device, &config, format, dummy, max_len, filter)?;
        stream.play().map_err(|e| e.to_string())?;

        let mic = MicSource::new(buffer, 1, denoise::RATE);
        let volume = self.mic_level();
        let voice = virt
            .bus
            .add(Box::new(Resample::new(mic.convert_samples::<i16>(), virt.rate())), volume);

        self.mic_stream = Some(stream);
        self.mic_voice = Some(voice);
        self.mic_tap = Some((tap_on, tap));
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
            Command::Stop(id) => {
                self.playing.retain(|p| {
                    if p.id == id {
                        p.voices.iter().for_each(Voice::stop);
                    }
                    p.id != id
                });
            }
            Command::ListenCable(on, reply) => {
                let _ = reply.send(self.listen_cable(on));
            }
            Command::HearMyself(on, reply) => {
                let _ = reply.send(self.hear_myself(on));
            }
            Command::StopPreview => {
                self.stop(PREVIEW_ID);
            }
            Command::SetMicMuted(muted) => {
                self.mic_muted = muted;
                for voice in [&self.mic_voice, &self.hear_voice].into_iter().flatten() {
                    voice.set_volume(self.mic_level());
                }
            }
        }
    }

    fn status(&self) -> Vec<PlayingStatus> {
        self.playing
            .iter()
            .filter(|p| p.stopping.is_none())
            .map(|p| PlayingStatus {
                id: p.id.clone(),
                elapsed_ms: p.started.elapsed().as_millis() as u64,
                duration_ms: p.duration_ms,
                looping: p.looping,
            })
            .collect()
    }

    fn toggle(&mut self, req: PlayRequest) -> Result<PlayOutcome, String> {
        if self.stop(&req.id) {
            return Ok(PlayOutcome::Stopped);
        }
        if self.settings.stop_previous {
            let others: Vec<String> = self
                .playing
                .iter()
                .filter(|p| !p.keep_playing && p.stopping.is_none())
                .map(|p| p.id.clone())
                .collect();
            for id in others {
                self.stop(&id);
            }
        }
        self.start(req, false)?;
        Ok(PlayOutcome::Started)
    }

    fn start(&mut self, req: PlayRequest, monitor_only: bool) -> Result<(), String> {
        let mut targets = Vec::new();
        // Pendant l'écoute du câble, le son arrive déjà au casque par là.
        if self.cable_voice.is_none() || monitor_only {
            targets.push((&self.monitor, self.settings.monitor_volume));
        }
        if !monitor_only {
            targets.push((&self.virt, self.settings.master_volume));
        }
        // Les sons en boucle démarrent et s'arrêtent en douceur.
        let fade_target = req
            .looping
            .then(|| Arc::new(AtomicU32::new(1.0f32.to_bits())));
        let mut voices = Vec::new();
        for (output, out_volume) in targets {
            let Some(output) = output else { continue };
            let source = open_source(&req, output.rate())?;
            let source: Box<dyn Source<Item = i16> + Send> = match &fade_target {
                Some(target) => Box::new(Fader::new(source, target.clone())),
                None => source,
            };
            voices.push(output.bus.add(source, out_volume));
        }
        if voices.is_empty() {
            return Err("aucune sortie audio configurée".into());
        }
        self.playing.push(Playing {
            id: req.id,
            voices,
            started: Instant::now(),
            duration_ms: req.duration_ms,
            looping: req.looping,
            keep_playing: req.keep_playing,
            fade_target,
            stopping: None,
        });
        Ok(())
    }

    /// Arrête un son ; renvoie vrai s'il était en cours.
    fn stop(&mut self, id: &str) -> bool {
        let mut found = false;
        self.playing.retain_mut(|p| {
            if p.id != id || p.stopping.is_some() {
                return true;
            }
            found = true;
            match &p.fade_target {
                // Fondu de sortie : le son est retiré une fois silencieux.
                Some(target) => {
                    target.store(0.0f32.to_bits(), Ordering::Relaxed);
                    p.stopping = Some(Instant::now());
                    true
                }
                None => {
                    p.voices.iter().for_each(Voice::stop);
                    false
                }
            }
        });
        found
    }

    fn stop_all(&mut self) {
        for p in self.playing.drain(..) {
            p.voices.iter().for_each(Voice::stop);
        }
    }
}

fn open_source(req: &PlayRequest, rate: u32) -> Result<Box<dyn Source<Item = i16> + Send>, String> {
    let decoder = AudioFile::open(&req.path)?;
    let length = match req.end_ms {
        Some(end) => Duration::from_millis(end.saturating_sub(req.start_ms)),
        None => Duration::from_secs(24 * 3600),
    };
    let mut decoder = decoder;
    decoder.seek_ms(req.start_ms);
    let source = Leveler::new(decoder.take_duration(length), req.gain);
    if req.looping {
        // Gardé en mémoire pour pouvoir le rejouer sans relire le fichier.
        Ok(Box::new(Resample::new(source.buffered().repeat_infinite(), rate)))
    } else {
        Ok(Box::new(Resample::new(source, rate)))
    }
}

/// Applique le gain du son puis aplatit ce qui dépasse encore.
///
/// L'égalisation règle un seul gain par son ; ici, un compresseur doux
/// (3:1 au-dessus de -18 dBFS) calme les cris au milieu d'un son, et un
/// limiteur empêche tout pic de dépasser -5 dBFS. Les deux sont en
/// flottant : plus de saturation quand le volume du son est poussé.
struct Leveler<S> {
    inner: S,
    gain: f32,
    /// Enveloppe des pics (suit vite la montée, redescend en 50 ms).
    env: f32,
    /// Réduction du compresseur, lissée (attaque 2 ms, relâche 200 ms).
    reduce: f32,
    env_release: f32,
    attack: f32,
    release: f32,
}

const COMP_THRESHOLD: f32 = 0.126;
const COMP_RATIO: f32 = 3.0;
const LIMIT_CEILING: f32 = 0.56;

impl<S: Source<Item = i16>> Leveler<S> {
    fn new(inner: S, gain: f32) -> Self {
        let per_second = inner.sample_rate().max(1) as f32 * inner.channels().max(1) as f32;
        let coef = |seconds: f32| (-1.0 / (per_second * seconds)).exp();
        Self {
            inner,
            gain,
            env: 0.0,
            reduce: 1.0,
            env_release: coef(0.05),
            attack: coef(0.002),
            release: coef(0.2),
        }
    }
}

impl<S: Source<Item = i16>> Iterator for Leveler<S> {
    type Item = i16;

    fn next(&mut self) -> Option<i16> {
        let x = self.inner.next()? as f32 / 32768.0 * self.gain;
        let level = x.abs();
        self.env = if level > self.env { level } else { level + (self.env - level) * self.env_release };
        let wanted = if self.env > COMP_THRESHOLD {
            (self.env / COMP_THRESHOLD).powf(1.0 / COMP_RATIO - 1.0)
        } else {
            1.0
        };
        let coef = if wanted < self.reduce { self.attack } else { self.release };
        self.reduce = wanted + (self.reduce - wanted) * coef;
        // Le limiteur agit tout de suite, sans attendre l'attaque.
        let reduce = self.reduce.min(LIMIT_CEILING / self.env.max(1e-6));
        let y = (x * reduce).clamp(-1.0, 1.0);
        Some((y * 32767.0) as i16)
    }
}

impl<S: Source<Item = i16>> Source for Leveler<S> {
    fn current_frame_len(&self) -> Option<usize> {
        self.inner.current_frame_len()
    }
    fn channels(&self) -> u16 {
        self.inner.channels()
    }
    fn sample_rate(&self) -> u32 {
        self.inner.sample_rate()
    }
    fn total_duration(&self) -> Option<Duration> {
        self.inner.total_duration()
    }
}

/// Convertit un son à la fréquence de la sortie, d'un seul tenant.
///
/// rodio sait le faire, mais il recommence sa conversion à chaque bloc du
/// fichier (ou tous les 512 échantillons dans un mélangeur), ce qui laisse un
/// petit clic à chaque raccord : le son grésille. Une fois à la bonne
/// fréquence, rodio n'y touche plus.
///
/// Interpolation cubique (Catmull-Rom) sur 4 points : bien plus propre que
/// la droite entre 2 points, qui étouffe les aigus et ajoute du souffle
/// métallique quand on passe de 44,1 à 48 kHz.
struct Resample<S> {
    inner: S,
    channels: usize,
    rate: u32,
    /// Avancée dans le son d'origine pour chaque échantillon produit.
    step: f64,
    /// Position entre `points[1]` et `points[2]` (0 à 1).
    pos: f64,
    /// Quatre trames consécutives du son d'origine ; vide = fini.
    points: [Vec<f32>; 4],
    /// Trames restantes après la fin du son (on prolonge par la dernière).
    tail: u8,
    frame: Vec<i16>,
    frame_pos: usize,
}

impl<S: Source<Item = i16>> Resample<S> {
    fn new(inner: S, rate: u32) -> Self {
        let channels = inner.channels().max(1) as usize;
        let step = inner.sample_rate().max(1) as f64 / rate.max(1) as f64;
        let mut this = Self {
            inner,
            channels,
            rate,
            step,
            pos: 0.0,
            points: Default::default(),
            tail: 2,
            frame: Vec::with_capacity(channels),
            frame_pos: 0,
        };
        if step != 1.0 {
            let first = this.read_frame();
            if !first.is_empty() {
                this.points[0] = first.clone();
                this.points[1] = first;
                this.points[2] = this.next_point();
                this.points[3] = this.next_point();
            }
        }
        this
    }

    fn read_frame(&mut self) -> Vec<f32> {
        let frame: Vec<f32> = (&mut self.inner).take(self.channels).map(f32::from).collect();
        if frame.len() == self.channels {
            frame
        } else {
            Vec::new()
        }
    }

    /// Trame suivante ; après la fin, on répète la dernière deux fois pour
    /// finir l'interpolation, puis plus rien.
    fn next_point(&mut self) -> Vec<f32> {
        let frame = self.read_frame();
        if !frame.is_empty() {
            return frame;
        }
        if self.tail == 0 {
            return Vec::new();
        }
        self.tail -= 1;
        self.points.iter().rev().find(|p| !p.is_empty()).cloned().unwrap_or_default()
    }
}

impl<S: Source<Item = i16>> Iterator for Resample<S> {
    type Item = i16;

    fn next(&mut self) -> Option<i16> {
        if self.step == 1.0 {
            return self.inner.next();
        }
        if self.frame_pos >= self.frame.len() {
            if self.points[2].is_empty() {
                return None;
            }
            let t = self.pos as f32;
            let [p0, p1, p2, p3] = &self.points;
            self.frame.clear();
            for c in 0..self.channels {
                let (a, b, c2, d) = (p0[c], p1[c], p2[c], p3.get(c).copied().unwrap_or(p2[c]));
                let v = b + 0.5
                    * t
                    * (c2 - a + t * (2.0 * a - 5.0 * b + 4.0 * c2 - d + t * (3.0 * (b - c2) + d - a)));
                self.frame.push(v.round().clamp(-32768.0, 32767.0) as i16);
            }
            self.frame_pos = 0;
            self.pos += self.step;
            while self.pos >= 1.0 && !self.points[2].is_empty() {
                self.pos -= 1.0;
                let next = self.next_point();
                self.points.rotate_left(1);
                self.points[3] = next;
            }
        }
        let sample = self.frame[self.frame_pos];
        self.frame_pos += 1;
        Some(sample)
    }
}

impl<S: Source<Item = i16>> Source for Resample<S> {
    fn current_frame_len(&self) -> Option<usize> {
        None
    }
    fn channels(&self) -> u16 {
        self.channels as u16
    }
    fn sample_rate(&self) -> u32 {
        if self.step == 1.0 {
            self.inner.sample_rate()
        } else {
            self.rate
        }
    }
    fn total_duration(&self) -> Option<Duration> {
        self.inner.total_duration()
    }
}

/// Applique un fondu qui suit `target` (0 à 1), échantillon par échantillon.
struct Fader<S> {
    inner: S,
    gain: f32,
    step: f32,
    target: Arc<AtomicU32>,
}

impl<S: Source<Item = i16>> Fader<S> {
    fn new(inner: S, target: Arc<AtomicU32>) -> Self {
        let per_second = inner.sample_rate() as f32 * inner.channels() as f32;
        Self {
            step: 1.0 / (per_second * FADE.as_secs_f32()).max(1.0),
            inner,
            gain: 0.0,
            target,
        }
    }
}

impl<S: Source<Item = i16>> Iterator for Fader<S> {
    type Item = i16;

    fn next(&mut self) -> Option<i16> {
        let sample = self.inner.next()?;
        let target = f32::from_bits(self.target.load(Ordering::Relaxed));
        if self.gain < target {
            self.gain = (self.gain + self.step).min(target);
        } else if self.gain > target {
            self.gain = (self.gain - self.step).max(target);
        }
        Some((sample as f32 * self.gain) as i16)
    }
}

impl<S: Source<Item = i16>> Source for Fader<S> {
    fn current_frame_len(&self) -> Option<usize> {
        self.inner.current_frame_len()
    }
    fn channels(&self) -> u16 {
        self.inner.channels()
    }
    fn sample_rate(&self) -> u32 {
        self.inner.sample_rate()
    }
    fn total_duration(&self) -> Option<Duration> {
        self.inner.total_duration()
    }
}

/// Coupures du son capté depuis le lancement (micro ou écoute du câble) :
/// tampon vide (le son manque) ou trop plein (on a jeté du son). Chacune
/// s'entend comme un petit clic : le diagnostic les affiche.
pub static CAPTURE_UNDERRUNS: AtomicU64 = AtomicU64::new(0);
pub static CAPTURE_OVERFLOWS: AtomicU64 = AtomicU64::new(0);

fn push(buf: &Mutex<VecDeque<f32>>, samples: Vec<f32>, max_len: usize) {
    let Ok(mut buf) = buf.lock() else { return };
    buf.extend(samples);
    if buf.len() > max_len {
        // Gros à-coup seulement : on revient à mi-hauteur d'un coup.
        let excess = buf.len() - max_len / 2;
        buf.drain(..excess);
        CAPTURE_OVERFLOWS.fetch_add(1, Ordering::Relaxed);
    }
}

/// Échantillons captés en attente d'être joués.
type SampleBuffer = Arc<Mutex<VecDeque<f32>>>;

type CaptureFilter = Box<dyn FnMut(&mut dyn Iterator<Item = f32>) -> Vec<f32> + Send>;

/// Ouvre un périphérique d'entrée et range ce qu'il capte (passé par
/// `filter`) dans `buffer`.
fn open_capture(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    format: SampleFormat,
    buffer: Arc<Mutex<VecDeque<f32>>>,
    max_len: usize,
    mut filter: CaptureFilter,
) -> Result<cpal::Stream, String> {
    let err_fn = |e| eprintln!("erreur de capture : {e}");
    match format {
        SampleFormat::F32 => device.build_input_stream(
            config,
            move |data: &[f32], _: &_| push(&buffer, filter(&mut data.iter().copied()), max_len),
            err_fn,
            None,
        ),
        SampleFormat::I16 => device.build_input_stream(
            config,
            move |data: &[i16], _: &_| {
                let mut it = data.iter().map(|s| *s as f32 / 32768.0);
                push(&buffer, filter(&mut it), max_len)
            },
            err_fn,
            None,
        ),
        SampleFormat::U16 => device.build_input_stream(
            config,
            move |data: &[u16], _: &_| {
                let mut it = data.iter().map(|s| (*s as f32 - 32768.0) / 32768.0);
                push(&buffer, filter(&mut it), max_len)
            },
            err_fn,
            None,
        ),
        other => return Err(format!("format non géré : {other:?}")),
    }
    .map_err(|e| e.to_string())
}

/// Source rodio infinie qui lit les échantillons du micro (ou du câble).
///
/// Le micro et le câble ont chacun leur horloge : l'un va toujours un poil
/// plus vite que l'autre. Au lieu de jeter ou d'inventer des bouts de son
/// (des micro-coupures), la lecture accélère ou ralentit de quelques
/// dixièmes de pour cent pour garder ~40 ms d'avance : inaudible.
/// Elle attend aussi d'avoir cette avance avant de démarrer, et si le son
/// manque quand même, la dernière valeur s'éteint en douceur (pas de clic).
struct MicSource {
    buffer: Arc<Mutex<VecDeque<f32>>>,
    local: VecDeque<f32>,
    channels: u16,
    sample_rate: u32,
    primed: bool,
    /// Avance visée dans le tampon, en trames, et avance moyenne mesurée.
    target: f64,
    fill: f64,
    /// Avancée dans le son capté pour chaque trame jouée (≈ 1).
    step: f64,
    pos: f64,
    /// Quatre trames consécutives pour l'interpolation cubique.
    points: [Vec<f32>; 4],
    frame: Vec<f32>,
    frame_pos: usize,
}

impl MicSource {
    fn new(buffer: Arc<Mutex<VecDeque<f32>>>, channels: u16, sample_rate: u32) -> Self {
        let ch = channels.max(1) as usize;
        Self {
            buffer,
            local: VecDeque::new(),
            channels: channels.max(1),
            sample_rate,
            primed: false,
            target: sample_rate as f64 * 0.04,
            fill: 0.0,
            step: 1.0,
            pos: 0.0,
            points: [vec![0.0; ch], vec![0.0; ch], vec![0.0; ch], vec![0.0; ch]],
            frame: vec![0.0; ch],
            frame_pos: ch,
        }
    }

    /// Prochaine trame captée, ou la précédente qui s'éteint s'il n'y a rien.
    fn pull_frame(&mut self) -> Vec<f32> {
        let ch = self.channels as usize;
        if self.local.len() < ch {
            // Par petits paquets (~1 ms) : on mesure l'avance souvent, et on
            // ne verrouille pas à chaque trame.
            if let Ok(mut buf) = self.buffer.lock() {
                let waiting = (buf.len() / ch) as f64;
                self.fill += (waiting - self.fill) * 0.002;
                // Après un manque, on reprend avec toute l'avance : repartir
                // avec la moitié en provoquait un autre juste après (des
                // micro-coupures en rafale au lieu d'une seule).
                if !self.primed && waiting >= self.target {
                    self.primed = true;
                    self.fill = waiting;
                }
                if self.primed {
                    // Avance trop grande : on lit un peu plus vite, et inversement.
                    let error = (self.fill - self.target) / self.target;
                    let wanted = 1.0 + (error * 0.004).clamp(-0.005, 0.005);
                    self.step += (wanted - self.step) * 0.05;
                    let n = (buf.len() - buf.len() % ch).min(64 * ch);
                    self.local.extend(buf.drain(..n));
                }
            }
        }
        if self.local.len() >= ch {
            return self.local.drain(..ch).collect();
        }
        if self.primed {
            CAPTURE_UNDERRUNS.fetch_add(1, Ordering::Relaxed);
            self.primed = false;
        }
        self.points[3].iter().map(|v| v * 0.9).collect()
    }
}

impl Iterator for MicSource {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        let ch = self.channels as usize;
        if self.frame_pos >= ch {
            while self.pos >= 1.0 {
                self.pos -= 1.0;
                let next = self.pull_frame();
                self.points.rotate_left(1);
                self.points[3] = next;
            }
            let t = self.pos as f32;
            let [p0, p1, p2, p3] = &self.points;
            for i in 0..ch {
                let (a, b, c, d) = (p0[i], p1[i], p2[i], p3[i]);
                self.frame[i] =
                    b + 0.5 * t * (c - a + t * (2.0 * a - 5.0 * b + 4.0 * c - d + t * (3.0 * (b - c) + d - a)));
            }
            self.pos += self.step;
            self.frame_pos = 0;
        }
        let v = self.frame[self.frame_pos];
        self.frame_pos += 1;
        Some(v)
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
