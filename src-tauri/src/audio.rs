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
use crate::denoise::{self, MicFilters, MicLevel, MicMeter, MicProcessor, Recording};
use crate::dynamics::LookaheadLimiter;
use crate::resample::{self, Kernel, SincWindow};
use crate::sound_fx::{self, Rendered, RenderedSource, SoundFx};
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
    /// Gain de l'égalisation (volume ressenti commun).
    pub gain: f32,
    /// Curseur de volume du son (0–2), appliqué après le compresseur.
    pub volume: f32,
    /// Volume ressenti visé (LUFS) : le compresseur et le limiteur s'y calent.
    pub target_lufs: f32,
    pub start_ms: u64,
    pub end_ms: Option<u64>,
    /// Durée jouée, pour la barre de progression.
    pub duration_ms: Option<u64>,
    pub looping: bool,
    /// Pas coupé quand un autre son démarre (option « un nouveau son coupe les autres »).
    pub keep_playing: bool,
    /// Effets de l'éditeur.
    pub fx: SoundFx,
}

/// Durée des fondus des sons en boucle.
const FADE: Duration = Duration::from_millis(400);
/// Fondu quand on coupe un son qui n'est pas en boucle (anti-clic).
const STOP_FADE: Duration = Duration::from_millis(10);

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
    /// Niveau habituel de ta voix (dBFS), à garder pour le prochain
    /// lancement : envoyé quand il est connu, puis s'il bouge de 2 dB.
    Learned(f32),
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
    /// Démarre (vrai) ou arrête (faux) l'enregistrement d'un son au micro ;
    /// l'arrêt renvoie ta voix traitée (mono 48 kHz).
    MicRecord(bool, Sender<Result<Vec<f32>, String>>),
    /// Donne accès à l'enregistrement du micro (test de voix).
    RecordSlot(Sender<Result<Arc<Mutex<RecordSlot>>, String>>),
}

/// Échange entre le moteur et le fil du micro pour le test de voix.
#[derive(Default)]
pub enum RecordSlot {
    #[default]
    Idle,
    /// Enregistrer tant d'échantillons (48 kHz).
    Wanted(usize),
    Running,
    Done(Recording),
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

    /// Enregistre `seconds` secondes du micro, brut et traité. Bloquant.
    pub fn record_voice(&self, seconds: usize) -> Result<Recording, String> {
        let (reply, rx) = mpsc::channel();
        self.send(Command::RecordSlot(reply))?;
        let slot = wait(rx)?;
        *slot.lock().map_err(|e| e.to_string())? = RecordSlot::Wanted(seconds * denoise::RATE as usize);
        let deadline = Instant::now() + Duration::from_secs(seconds as u64 + 5);
        while Instant::now() < deadline {
            thread::sleep(Duration::from_millis(50));
            let mut slot = slot.lock().map_err(|e| e.to_string())?;
            if let RecordSlot::Done(_) = &*slot {
                if let RecordSlot::Done(r) = std::mem::take(&mut *slot) {
                    return Ok(r);
                }
            }
        }
        *slot.lock().map_err(|e| e.to_string())? = RecordSlot::Idle;
        Err("le micro n'a rien envoyé : vérifie qu'il est branché et choisi dans les réglages".into())
    }

    pub fn stop_preview(&self) {
        let _ = self.send(Command::StopPreview);
    }

    /// Enregistrer un son au micro : `true` démarre, `false` arrête et
    /// renvoie la voix enregistrée.
    pub fn mic_record(&self, start: bool) -> Result<Vec<f32>, String> {
        let (reply, rx) = mpsc::channel();
        self.send(Command::MicRecord(start, reply))?;
        wait(rx)
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

#[derive(Debug, Clone, PartialEq, Serialize)]
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
    /// `keep_last` : garder les 30 dernières secondes (sortie vers Discord).
    fn open(device: &cpal::Device, keep_last: bool) -> Result<Self, String> {
        let config = device.default_output_config().map_err(|e| e.to_string())?;
        let tap = if keep_last { crate::replay::cable_ring(config.sample_rate().0) } else { None };
        let (bus, source) = Bus::with_tap(config.channels(), config.sample_rate().0, tap);
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
    /// Durée du fondu d'arrêt.
    fade: Duration,
    /// Arrêt en fondu en cours depuis cet instant.
    stopping: Option<Instant>,
}

impl Playing {
    fn is_finished(&self) -> bool {
        self.voices.iter().all(Voice::is_done) || self.stopping.is_some_and(|t| t.elapsed() > self.fade + Duration::from_millis(50))
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
    /// Test de voix, partagé avec le fil du micro.
    mic_record: Option<Arc<Mutex<RecordSlot>>>,
    /// Son enregistré au micro (Some pendant l'enregistrement).
    mic_take: Option<MicTake>,
    /// Derniers sons avec effets déjà calculés.
    fx_cache: VecDeque<(FxKey, Rendered)>,
}

/// Voix enregistrée pour en faire un son, partagée avec le fil du micro.
type MicTake = Arc<Mutex<Option<Vec<f32>>>>;
/// Une minute au plus.
const MIC_TAKE_MAX: usize = 60 * denoise::RATE as usize;

/// Ce qui change le résultat d'un son avec effets.
#[derive(PartialEq)]
struct FxKey {
    path: PathBuf,
    start_ms: u64,
    end_ms: Option<u64>,
    looping: bool,
    fx: SoundFx,
}

const FX_CACHE: usize = 12;
const FX_CACHE_SAMPLES: usize = 16 << 20;

fn run(rx: Receiver<Command>, meter_on: Arc<AtomicBool>, on_event: impl Fn(AudioEvent)) {
    let mut engine = Engine::default();
    let mut last_status: Vec<PlayingStatus> = Vec::new();
    let mut last_learned: Option<f32> = None;
    let mut ticks = 0u32;
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
        // Toutes les 10 s environ.
        ticks = ticks.wrapping_add(1);
        if ticks.is_multiple_of(100) && engine.mic_stream.is_some() {
            match engine.meter.learned_db() {
                Some(db) if last_learned.is_none_or(|last| (db - last).abs() >= 2.0) => {
                    last_learned = Some(db);
                    on_event(AudioEvent::Learned(db));
                }
                Some(_) => {}
                // Micro rouvert : la prochaine mesure sera envoyée.
                None => last_learned = None,
            }
        }
    }
}

impl Engine {
    fn configure(&mut self, settings: Settings) -> Result<(), String> {
        self.stop_all();
        self.stop_cable();
        self.stop_hearing();
        self.mic_tap = None;
        self.mic_record = None;
        self.mic_take = None;
        if let Some(v) = self.mic_voice.take() {
            v.stop();
        }
        self.mic_stream = None;
        self.monitor = None;
        self.virt = None;
        self.settings = settings;

        let mut errors = Vec::new();

        match find_output(self.settings.monitor_device.as_deref()) {
            Some(dev) => match Output::open(&dev, false) {
                Ok(o) => self.monitor = Some(o),
                Err(e) => errors.push(format!("le casque ne s'ouvre pas ({e}).")),
            },
            None => errors.push("casque introuvable : il est peut-être débranché. Rebranche-le, le son reprendra tout seul.".into()),
        }

        // Sans câble virtuel choisi, on ne joue que dans le casque : jouer deux
        // fois sur la sortie par défaut doublerait le son.
        if let Some(name) = self.settings.virtual_device.clone() {
            match find_output(Some(&name)) {
                Some(dev) => match Output::open(&dev, true) {
                    Ok(o) => self.virt = Some(o),
                    Err(e) => errors.push(format!("le câble virtuel ne s'ouvre pas ({e}).")),
                },
                None => errors.push(format!("câble virtuel « {name} » introuvable : Discord n'entend plus les sons. Réinstalle VB-Cable ou choisis un autre câble.")),
            }
        }

        if self.settings.mic_enabled && self.virt.is_some() {
            if let Err(e) = self.start_mic() {
                errors.push(format!("micro : {e}. Rebranche-le ou choisis-en un autre, ta voix reprendra toute seule."));
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("\n"))
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
        let source = MicSource::new(tap.clone(), 1, denoise::RATE);
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
        let source = MicSource::new(buffer, channels, rate);
        let voice = monitor
            .bus
            .add(Box::new(Resample::new(source, monitor.rate())), self.settings.monitor_volume);
        self.cable_stream = Some(stream);
        self.cable_voice = Some(voice);
        Ok(())
    }

    fn start_mic(&mut self) -> Result<(), String> {
        let virt = self.virt.as_ref().ok_or("pas de câble virtuel")?;
        let device = find_input(self.settings.mic_device.as_deref()).ok_or("introuvable")?;
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
            eq: self.settings.voice_eq,
            calibration: self.settings.mic_calibration,
            far_voices: self.settings.far_voices,
            learned_db: self
                .settings
                .mic_learned
                .as_ref()
                .filter(|l| l.device == self.settings.mic_device && l.speech_db >= denoise::LEARNED_MIN_DB)
                .map(|l| l.speech_db.min(-1.0)),
        };
        self.meter.forget();

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
        let record = Arc::new(Mutex::new(RecordSlot::Idle));
        let record2 = record.clone();
        let take: MicTake = Arc::new(Mutex::new(None));
        let take2 = take.clone();
        thread::Builder::new()
            .name("micro".into())
            .spawn(move || {
                raise_thread_priority();
                // S'arrête tout seul quand le micro est fermé (raw_tx lâché).
                while let Ok(chunk) = raw_rx.recv() {
                    let mut cleaned = Vec::with_capacity(chunk.len());
                    processor.process(chunk.into_iter(), &mut cleaned);
                    if tap_on2.load(Ordering::Relaxed) {
                        push(&tap2, cleaned.clone(), max_len);
                    }
                    if let Ok(mut t) = take2.lock() {
                        if let Some(v) = t.as_mut() {
                            let room = MIC_TAKE_MAX.saturating_sub(v.len());
                            v.extend_from_slice(&cleaned[..room.min(cleaned.len())]);
                        }
                    }
                    push(&out, cleaned, max_len);
                    if let Ok(mut slot) = record2.try_lock() {
                        match &*slot {
                            RecordSlot::Wanted(n) => {
                                processor.start_recording(*n);
                                *slot = RecordSlot::Running;
                            }
                            RecordSlot::Running => {
                                if let Some(r) = processor.take_recording() {
                                    *slot = RecordSlot::Done(r);
                                }
                            }
                            _ => {}
                        }
                    }
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
            .add(Box::new(Resample::new(mic, virt.rate())), volume);

        self.mic_stream = Some(stream);
        self.mic_voice = Some(voice);
        self.mic_tap = Some((tap_on, tap));
        self.mic_record = Some(record);
        self.mic_take = Some(take);
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
            Command::MicRecord(start, reply) => {
                let res = match &self.mic_take {
                    None => Err("ta voix n'est pas envoyée : active « Envoyer ma voix dans Discord » et choisis ton micro dans les réglages".to_string()),
                    Some(take) => {
                        let mut t = take.lock().unwrap();
                        if start {
                            *t = Some(Vec::with_capacity(10 * denoise::RATE as usize));
                            Ok(Vec::new())
                        } else {
                            Ok(t.take().unwrap_or_default())
                        }
                    }
                };
                let _ = reply.send(res);
            }
            Command::RecordSlot(reply) => {
                let _ = reply.send(self.mic_record.clone().ok_or_else(|| {
                    "ta voix n'est pas envoyée : active « Envoyer ma voix dans Discord » et choisis le câble".to_string()
                }));
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
        // Effets : calculés une fois, partagés par le casque et le câble.
        let rendered = if req.fx.is_neutral() { None } else { Some(self.rendered(&req)?) };
        let mut targets = Vec::new();
        // Pendant l'écoute du câble, le son arrive déjà au casque par là.
        if self.cable_voice.is_none() || monitor_only {
            targets.push((&self.monitor, self.settings.monitor_volume, false));
        }
        if !monitor_only {
            targets.push((&self.virt, self.settings.master_volume, true));
        }
        // Les sons en boucle démarrent et s'arrêtent en douceur ; les autres
        // ont un fondu très court, pour ne pas claquer quand on les coupe.
        let fade = if req.looping { FADE } else { STOP_FADE };
        let fade_target = Some(Arc::new(AtomicU32::new(1.0f32.to_bits())));
        let mut voices = Vec::new();
        for (output, out_volume, to_discord) in targets {
            let Some(output) = output else { continue };
            let source = match &rendered {
                Some(r) => Box::new(Resample::new(
                    Leveler::new(RenderedSource::new(r.clone(), req.looping), Levels::of(&req)),
                    output.rate(),
                )),
                None => open_source(&req, output.rate())?,
            };
            let source: Box<dyn Source<Item = f32> + Send> = match &fade_target {
                Some(target) => Box::new(Fader::new(source, target.clone(), fade)),
                None => source,
            };
            // Discord envoie le micro en mono : on replie le son nous-mêmes,
            // sans laisser les passages en opposition de phase s'annuler.
            let source: Box<dyn Source<Item = f32> + Send> =
                if to_discord && source.channels() == 2 { Box::new(MonoSafe::new(source)) } else { source };
            voices.push(output.bus.add_sound(source, out_volume));
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
            fade,
            stopping: None,
        });
        Ok(())
    }

    /// Le son découpé avec ses effets, calculé une fois puis gardé en cache.
    fn rendered(&mut self, req: &PlayRequest) -> Result<Rendered, String> {
        let key = FxKey {
            path: req.path.clone(),
            start_ms: req.start_ms,
            end_ms: req.end_ms,
            looping: req.looping,
            fx: req.fx,
        };
        if let Some(i) = self.fx_cache.iter().position(|(k, _)| *k == key) {
            let entry = self.fx_cache.remove(i).unwrap();
            let r = entry.1.clone();
            self.fx_cache.push_back(entry);
            return Ok(r);
        }
        let mut decoder = AudioFile::open(&req.path)?;
        decoder.seek_ms(req.start_ms);
        let r = sound_fx::render(decoder, req.start_ms, req.end_ms, &req.fx, req.looping);
        // Au plus ~64 Mo d'échantillons gardés.
        let mut total: usize = self.fx_cache.iter().map(|(_, r)| r.data.len()).sum::<usize>() + r.data.len();
        while total > FX_CACHE_SAMPLES || self.fx_cache.len() >= FX_CACHE {
            let Some((_, old)) = self.fx_cache.pop_front() else { break };
            total -= old.data.len();
        }
        self.fx_cache.push_back((key, r.clone()));
        Ok(r)
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

fn open_source(req: &PlayRequest, rate: u32) -> Result<Box<dyn Source<Item = f32> + Send>, String> {
    let decoder = AudioFile::open(&req.path)?;
    let length = match req.end_ms {
        Some(end) => Duration::from_millis(end.saturating_sub(req.start_ms)),
        None => Duration::from_secs(24 * 3600),
    };
    let mut decoder = decoder;
    decoder.seek_ms(req.start_ms);
    // Fondu de 5 ms à la fin du passage : une découpe ne claque plus.
    let frames = req.duration_ms.map(|ms| ms * decoder.sample_rate() as u64 / 1000);
    let source = Leveler::new(EndFade::new(decoder.take_duration(length), frames), Levels::of(req));
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
/// (3:1, 10 dB au-dessus du volume visé : -18 dBFS pour -28 LUFS) calme les
/// cris au milieu d'un son. Puis vient le curseur de volume du son, que le
/// compresseur ne reprend donc plus, et un limiteur à anticipation (2 ms)
/// qui empêche tout pic de dépasser 23 dB au-dessus du volume visé (-5 dBFS
/// pour -28 LUFS ; plus haut si le curseur dépasse 100 %, jamais au-delà
/// de -1 dBFS) sans déformer l'onde. Les trois réglages de volume ressenti
/// sonnent ainsi pareil, juste plus ou moins fort. Tout est en flottant : un MP3 dont les crêtes dépassent
/// 0 dBFS n'est plus écrêté au décodage.
struct Leveler<S> {
    inner: S,
    channels: usize,
    gain: f32,
    volume: f32,
    threshold: f32,
    /// Enveloppe des pics (suit vite la montée, redescend en 50 ms).
    env: f32,
    /// Réduction du compresseur, lissée (attaque 2 ms, relâche 200 ms).
    reduce: f32,
    env_release: f32,
    attack: f32,
    release: f32,
    limiter: LookaheadLimiter,
    frame: Vec<f32>,
    frame_pos: usize,
    /// Trames de silence encore à pousser pour vider le limiteur à la fin.
    tail: Option<usize>,
    /// Trames de retard du limiteur à sauter au début : le son garde
    /// exactement sa durée (une boucle ne gagne pas de petit blanc).
    skip: usize,
}

const COMP_ABOVE_TARGET_DB: f32 = 10.0;
const COMP_RATIO: f32 = 3.0;
const LIMIT_ABOVE_TARGET_DB: f32 = 23.0;
/// Plafond absolu des sons : -1 dBFS.
const LIMIT_MAX: f32 = 0.891;

/// Réglages de niveau d'un son joué.
#[derive(Clone, Copy)]
struct Levels {
    gain: f32,
    volume: f32,
    target_lufs: f32,
}

impl Levels {
    fn of(req: &PlayRequest) -> Self {
        Self { gain: req.gain, volume: req.volume.max(0.0), target_lufs: req.target_lufs.clamp(-40.0, -14.0) }
    }

    fn threshold(&self) -> f32 {
        10f32.powf((self.target_lufs + COMP_ABOVE_TARGET_DB) / 20.0)
    }

    fn ceiling(&self) -> f32 {
        let base = 10f32.powf((self.target_lufs + LIMIT_ABOVE_TARGET_DB) / 20.0);
        (base * self.volume.max(1.0)).min(LIMIT_MAX)
    }
}

impl<S: Source<Item = f32>> Leveler<S> {
    fn new(inner: S, levels: Levels) -> Self {
        let channels = inner.channels().max(1) as usize;
        let rate = inner.sample_rate().max(1) as f32;
        // Le compresseur avance par échantillon (canaux entrelacés), le
        // limiteur par trame.
        let per_second = rate * channels as f32;
        let coef = |seconds: f32| (-1.0 / (per_second * seconds)).exp();
        let lookahead = (rate * 0.002).round() as usize;
        let limiter = LookaheadLimiter::new(channels, lookahead, levels.ceiling(), (-1.0 / (rate * 0.08)).exp());
        Self {
            inner,
            channels,
            gain: levels.gain,
            volume: levels.volume,
            threshold: levels.threshold(),
            env: 0.0,
            reduce: 1.0,
            env_release: coef(0.05),
            attack: coef(0.002),
            release: coef(0.2),
            skip: limiter.latency(),
            limiter,
            frame: vec![0.0; channels],
            frame_pos: channels,
            tail: None,
        }
    }

    fn compress(&mut self, x: f32) -> f32 {
        let x = x * self.gain;
        let level = x.abs();
        self.env = if level > self.env { level } else { level + (self.env - level) * self.env_release };
        let wanted = if self.env > self.threshold {
            (self.env / self.threshold).powf(1.0 / COMP_RATIO - 1.0)
        } else {
            1.0
        };
        let coef = if wanted < self.reduce { self.attack } else { self.release };
        self.reduce = wanted + (self.reduce - wanted) * coef;
        x * self.reduce * self.volume
    }

    /// Trame suivante, passée par le compresseur puis le limiteur.
    fn next_frame(&mut self) -> bool {
        if self.tail.is_none() {
            let mut n = 0;
            for i in 0..self.channels {
                match self.inner.next() {
                    Some(x) => {
                        self.frame[i] = self.compress(x);
                        n += 1;
                    }
                    None => break,
                }
            }
            if n < self.channels {
                self.tail = Some(self.limiter.latency());
            }
        }
        if let Some(t) = self.tail.as_mut() {
            if *t == 0 {
                return false;
            }
            *t -= 1;
            self.frame.iter_mut().for_each(|f| *f = 0.0);
        }
        self.limiter.process(&mut self.frame);
        true
    }
}

impl<S: Source<Item = f32>> Iterator for Leveler<S> {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        if self.frame_pos >= self.channels {
            loop {
                if !self.next_frame() {
                    return None;
                }
                if self.skip == 0 {
                    break;
                }
                self.skip -= 1;
            }
            self.frame_pos = 0;
        }
        let v = self.frame[self.frame_pos];
        self.frame_pos += 1;
        Some(v)
    }
}

impl<S: Source<Item = f32>> Source for Leveler<S> {
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
/// Interpolation par sinus cardinal fenêtré (voir `resample`) : rien ne se
/// perd dans l'aigu et aucun parasite ne s'ajoute (la cubique perdait 2 dB
/// et ajoutait du souffle métallique vers 16 kHz en passant de 44,1 à 48 kHz).
struct Resample<S> {
    inner: S,
    channels: usize,
    rate: u32,
    /// Avancée dans le son d'origine pour chaque échantillon produit.
    step: f64,
    /// Position entre l'échantillon central de la fenêtre et le suivant.
    pos: f64,
    window: SincWindow,
    /// Trames de silence encore à pousser après la fin du son, pour faire
    /// sortir ses derniers échantillons ; `None` tant qu'il n'est pas fini.
    tail: Option<usize>,
    input: Vec<f32>,
    frame: Vec<f32>,
    frame_pos: usize,
}

impl<S: Source<Item = f32>> Resample<S> {
    fn new(inner: S, rate: u32) -> Self {
        let channels = inner.channels().max(1) as usize;
        let step = inner.sample_rate().max(1) as f64 / rate.max(1) as f64;
        let mut this = Self {
            inner,
            channels,
            rate,
            step,
            pos: 0.0,
            window: SincWindow::new(Kernel::for_ratio(1.0 / step), channels),
            tail: None,
            input: Vec::with_capacity(channels),
            frame: vec![0.0; channels],
            frame_pos: channels,
        };
        if step != 1.0 {
            // Le premier échantillon lu doit être le début du son.
            for _ in 0..=resample::DELAY {
                this.push_next();
            }
        }
        this
    }

    /// Pousse la trame suivante du son dans la fenêtre (du silence après la
    /// fin). Faux quand il n'y a plus rien à faire sortir.
    fn push_next(&mut self) -> bool {
        if self.tail.is_none() {
            self.input.clear();
            self.input.extend((&mut self.inner).take(self.channels));
            if self.input.len() == self.channels {
                self.window.push(&self.input);
                return true;
            }
            self.tail = Some(resample::DELAY + 1);
        }
        match self.tail.as_mut() {
            Some(0) => false,
            Some(n) => {
                *n -= 1;
                self.input.clear();
                self.input.resize(self.channels, 0.0);
                self.window.push(&self.input);
                true
            }
            None => unreachable!(),
        }
    }
}

impl<S: Source<Item = f32>> Iterator for Resample<S> {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        if self.step == 1.0 {
            return self.inner.next();
        }
        if self.frame_pos >= self.frame.len() {
            if self.tail == Some(0) {
                return None;
            }
            self.window.read(self.pos, &mut self.frame);
            self.frame_pos = 0;
            self.pos += self.step;
            while self.pos >= 1.0 {
                self.pos -= 1.0;
                if !self.push_next() {
                    break;
                }
            }
        }
        let sample = self.frame[self.frame_pos];
        self.frame_pos += 1;
        Some(sample)
    }
}

impl<S: Source<Item = f32>> Source for Resample<S> {
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
/// Repli stéréo → mono qui résiste aux opposition de phase.
///
/// Un mélange (G + D) / 2 efface ce qui est en opposition de phase entre
/// les deux canaux (basses « élargies », certaines réverbs, vieux memes mal
/// mixés) : le son devient creux ou disparaît chez les autres. Ici, quand
/// la différence G − D porte nettement plus d'énergie que la somme, on
/// bascule en douceur vers la différence. Les deux canaux de sortie
/// reçoivent le même signal.
struct MonoSafe<S> {
    inner: S,
    pending: Option<f32>,
    sum_energy: f32,
    diff_energy: f32,
    weight: f32,
    smooth: f32,
}

impl<S: Source<Item = f32>> MonoSafe<S> {
    fn new(inner: S) -> Self {
        // Énergies lissées sur ~50 ms, bascule sur ~30 ms.
        let rate = inner.sample_rate().max(1) as f32;
        Self { inner, pending: None, sum_energy: 0.0, diff_energy: 0.0, weight: 0.0, smooth: (-1.0 / (rate * 0.05)).exp() }
    }

    /// Mélange d'une trame, et mise à jour des mesures.
    fn fold(&mut self, l: f32, r: f32) -> f32 {
        let (sum, diff) = ((l + r) * 0.5, (l - r) * 0.5);
        let a = self.smooth;
        self.sum_energy = a * self.sum_energy + (1.0 - a) * sum * sum;
        self.diff_energy = a * self.diff_energy + (1.0 - a) * diff * diff;
        // Opposition de phase marquée : la différence est 4× plus forte (6 dB).
        let target = if self.diff_energy > 4.0 * self.sum_energy + 1e-9 { 1.0 } else { 0.0 };
        self.weight += (target - self.weight) * (1.0 - a.powf(1.6));
        sum * (1.0 - self.weight) + diff * self.weight
    }
}

impl<S: Source<Item = f32>> Iterator for MonoSafe<S> {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        if let Some(v) = self.pending.take() {
            return Some(v);
        }
        let l = self.inner.next()?;
        let r = self.inner.next().unwrap_or(l);
        let m = self.fold(l, r);
        self.pending = Some(m);
        Some(m)
    }
}

impl<S: Source<Item = f32>> Source for MonoSafe<S> {
    fn current_frame_len(&self) -> Option<usize> {
        self.inner.current_frame_len()
    }
    fn channels(&self) -> u16 {
        2
    }
    fn sample_rate(&self) -> u32 {
        self.inner.sample_rate()
    }
    fn total_duration(&self) -> Option<Duration> {
        self.inner.total_duration()
    }
}

/// Petit fondu de sortie sur les dernières millisecondes d'un passage dont
/// la longueur est connue.
struct EndFade<S> {
    inner: S,
    pos: u64,
    fade_from: u64,
    total: u64,
}

impl<S: Source<Item = f32>> EndFade<S> {
    fn new(inner: S, frames: Option<u64>) -> Self {
        let ch = inner.channels().max(1) as u64;
        let total = frames.map_or(u64::MAX, |f| f * ch);
        let fade = (crate::sound_fx::DECLICK_MS * inner.sample_rate() as f32 / 1000.0) as u64 * ch;
        Self { inner, pos: 0, fade_from: total.saturating_sub(fade), total }
    }
}

impl<S: Source<Item = f32>> Iterator for EndFade<S> {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        let s = self.inner.next()?;
        let pos = self.pos;
        self.pos += 1;
        if pos < self.fade_from {
            return Some(s);
        }
        let left = self.total.saturating_sub(pos) as f32;
        let span = (self.total - self.fade_from).max(1) as f32;
        Some(s * (left / span).powi(2))
    }
}

impl<S: Source<Item = f32>> Source for EndFade<S> {
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

struct Fader<S> {
    inner: S,
    gain: f32,
    step: f32,
    target: Arc<AtomicU32>,
}

impl<S: Source<Item = f32>> Fader<S> {
    fn new(inner: S, target: Arc<AtomicU32>, fade: Duration) -> Self {
        let per_second = inner.sample_rate() as f32 * inner.channels() as f32;
        Self {
            step: 1.0 / (per_second * fade.as_secs_f32()).max(1.0),
            inner,
            gain: 0.0,
            target,
        }
    }
}

impl<S: Source<Item = f32>> Iterator for Fader<S> {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        let sample = self.inner.next()?;
        let target = f32::from_bits(self.target.load(Ordering::Relaxed));
        if self.gain < target {
            self.gain = (self.gain + self.step).min(target);
        } else if self.gain > target {
            self.gain = (self.gain - self.step).max(target);
        }
        Some(sample * self.gain)
    }
}

impl<S: Source<Item = f32>> Source for Fader<S> {
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

/// Passe le fil courant en priorité haute. Le nettoyage du micro (IA
/// comprise) doit rendre 10 ms de son toutes les 10 ms : en priorité
/// normale, un jeu qui occupe tous les cœurs le fait attendre, et la voix
/// a des micro-coupures. Il ne consomme pas plus pour autant.
#[cfg(windows)]
fn raise_thread_priority() {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentThread() -> *mut std::ffi::c_void;
        fn SetThreadPriority(thread: *mut std::ffi::c_void, priority: i32) -> i32;
    }
    const THREAD_PRIORITY_HIGHEST: i32 = 2;
    // SAFETY: deux fonctions de base de Windows, sur le fil courant ; un
    // refus (valeur 0) laisse simplement la priorité normale.
    unsafe {
        SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_HIGHEST);
    }
}

#[cfg(not(windows))]
fn raise_thread_priority() {}

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
    // Presque tous les micros arrivent en f32 ; quelques pilotes annoncent
    // des entiers (16 ou 32 bits, parfois 8) : on les ramène tous à −1…1.
    macro_rules! capture {
        ($sample:ty, $to_f32:expr) => {
            device.build_input_stream(
                config,
                move |data: &[$sample], _: &_| {
                    let mut it = data.iter().map($to_f32);
                    push(&buffer, filter(&mut it), max_len)
                },
                err_fn,
                None,
            )
        };
    }
    match format {
        SampleFormat::F32 => capture!(f32, |s| *s),
        SampleFormat::F64 => capture!(f64, |s| *s as f32),
        SampleFormat::I16 => capture!(i16, |s| *s as f32 / 32768.0),
        SampleFormat::U16 => capture!(u16, |s| (*s as f32 - 32768.0) / 32768.0),
        SampleFormat::I32 => capture!(i32, |s| *s as f32 / 2_147_483_648.0),
        SampleFormat::I8 => capture!(i8, |s| *s as f32 / 128.0),
        SampleFormat::U8 => capture!(u8, |s| (*s as f32 - 128.0) / 128.0),
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
    window: SincWindow,
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
            window: SincWindow::new(Kernel::for_ratio(1.0), ch),
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
        (0..ch).map(|c| self.window.newest(c) * 0.9).collect()
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
                self.window.push(&next);
            }
            self.window.read(self.pos, &mut self.frame);
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

#[cfg(test)]
mod tests {

    #[test]
    fn mono_fold_survives_phase_inversion() {
        let rate = 48_000;
        let tone: Vec<f32> = (0..rate).flat_map(|i| {
            let v = (i as f32 * 0.05).sin() * 0.5;
            [v, -v] // canaux en opposition de phase
        }).collect();
        let src = rodio::buffer::SamplesBuffer::new(2, rate as u32, tone);
        let out: Vec<f32> = MonoSafe::new(src).collect();
        let tail = &out[out.len() / 2..];
        let rms = (tail.iter().map(|v| v * v).sum::<f32>() / tail.len() as f32).sqrt();
        // Un simple (G + D) / 2 donnerait du silence ; ici on garde le son.
        assert!(rms > 0.3, "rms {rms}");
        assert!(tail.chunks(2).all(|f| f[0] == f[1]));
    }

    use super::*;
    use rodio::buffer::SamplesBuffer;

    #[test]
    fn leveler_keeps_length_and_caps_peaks_without_clipping_overs() {
        // Stéréo 44,1 kHz avec des crêtes au-dessus de 0 dBFS (comme un MP3
        // poussé fort) : rien n'est écrêté, tout passe sous le plafond, et le
        // son garde exactement sa durée (boucles sans blanc).
        let data: Vec<f32> = (0..44_100).flat_map(|i| {
            let v = (i as f32 * 0.03).sin() * 1.3;
            [v, -v]
        }).collect();
        let len = data.len();
        let levels = Levels { gain: 1.0, volume: 1.0, target_lufs: -28.0 };
        let ceiling = levels.ceiling();
        assert!((ceiling - 0.56).abs() < 0.01);
        let out: Vec<f32> = Leveler::new(SamplesBuffer::new(2, 44_100, data), levels).collect();
        assert_eq!(out.len(), len);
        let peak = out.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(peak <= ceiling + 1e-4, "peak {peak}");
        assert!(peak > ceiling * 0.9);
    }

    #[test]
    fn volume_slider_is_not_eaten_by_the_compressor() {
        // Un son déjà au-dessus du seuil du compresseur : 200 % doit donner
        // vraiment +6 dB (avant : à peine +2 dB).
        let tone = |volume: f32| {
            let data: Vec<f32> = (0..44_100).map(|i| (i as f32 * 0.05).sin() * 0.2).collect();
            let levels = Levels { gain: 1.0, volume, target_lufs: -28.0 };
            let out: Vec<f32> = Leveler::new(SamplesBuffer::new(1, 44_100, data), levels).collect();
            (out[22_050..].iter().map(|v| v * v).sum::<f32>() / 22_050.0).sqrt()
        };
        let db = 20.0 * (tone(2.0) / tone(1.0)).log10();
        assert!((db - 6.0).abs() < 0.5, "{db}");
    }

    #[test]
    fn resample_keeps_float_precision() {
        // Un son très faible (-100 dBFS) n'est plus arrondi à zéro.
        let data: Vec<f32> = (0..4410).map(|i| (i as f32 * 0.1).sin() * 1e-5).collect();
        let out: Vec<f32> = Resample::new(SamplesBuffer::new(1, 44_100, data), 48_000).collect();
        let peak = out[100..].iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(peak > 0.5e-5 && peak < 2e-5, "{peak}");
    }
}
