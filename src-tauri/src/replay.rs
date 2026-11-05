//! « Rejouer les 30 dernières secondes » et baisse auto des sons.
//!
//! Deux mémoires tournantes gardent les 30 dernières secondes :
//! - ce que tu envoies dans Discord (ta voix + tes sons), copié à la sortie
//!   du bus du câble ;
//! - ce que Discord te fait entendre (la voix de tes potes), capté
//!   directement dans l'appli Discord (Windows 10 2004 et plus), sans le
//!   son des jeux ni des autres applis.
//!
//! Un raccourci mélange les deux en un nouveau son. La même capture de
//! Discord sert à baisser les sons de la soundboard quand un pote parle.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

/// Durée gardée en mémoire.
pub const SECONDS: usize = 30;
/// Fréquence des extraits enregistrés.
pub const RATE: u32 = 48_000;

/// Mémoire tournante en mono.
pub struct Ring {
    buf: Mutex<VecDeque<f32>>,
    rate: u32,
    cap: usize,
}

impl Ring {
    pub fn new(rate: u32) -> Arc<Self> {
        let rate = rate.max(1);
        let cap = rate as usize * SECONDS;
        Arc::new(Self { buf: Mutex::new(VecDeque::with_capacity(cap)), rate, cap })
    }

    /// Ajoute des trames entrelacées (repliées en mono). Ne bloque jamais le
    /// fil audio : si l'extrait est en train d'être copié, ce bout est perdu.
    pub fn push(&self, data: &[f32], channels: usize, _rate: u32) {
        if !REPLAY_ON.load(Ordering::Relaxed) {
            return;
        }
        let ch = channels.max(1);
        let Ok(mut buf) = self.buf.try_lock() else { return };
        for frame in data.chunks_exact(ch) {
            buf.push_back(frame.iter().sum::<f32>() / ch as f32);
        }
        let len = buf.len();
        if len > self.cap {
            buf.drain(..len - self.cap);
        }
    }

    /// Copie le contenu, converti à 48 kHz.
    fn take(&self) -> Vec<f32> {
        let data: Vec<f32> = match self.buf.lock() {
            Ok(buf) => buf.iter().copied().collect(),
            Err(_) => return Vec::new(),
        };
        to_rate(&data, self.rate, RATE)
    }

    fn clear(&self) {
        if let Ok(mut buf) = self.buf.lock() {
            buf.clear();
        }
    }
}

/// Changement de fréquence simple (interpolation linéaire) : suffisant pour
/// un extrait de conversation.
fn to_rate(data: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || data.len() < 2 {
        return data.to_vec();
    }
    let step = from as f64 / to as f64;
    let n = ((data.len() - 1) as f64 / step) as usize;
    (0..n)
        .map(|i| {
            let pos = i as f64 * step;
            let k = pos as usize;
            let frac = (pos - k as f64) as f32;
            data[k] + (data[(k + 1).min(data.len() - 1)] - data[k]) * frac
        })
        .collect()
}

static CABLE: Mutex<Option<Arc<Ring>>> = Mutex::new(None);
static DISCORD: Mutex<Option<Arc<Ring>>> = Mutex::new(None);

/// Garder les 30 dernières secondes (« Rejouer »).
pub static REPLAY_ON: AtomicBool = AtomicBool::new(true);
/// Baisser les sons quand un pote parle, et de combien (gain, bits d'un f32).
pub static DUCK_ON: AtomicBool = AtomicBool::new(false);
static DUCK_DEPTH: AtomicU32 = AtomicU32::new(0x3eb3_3333); // 0.35
/// Discord est capté en ce moment.
pub static DISCORD_HEARD: AtomicBool = AtomicBool::new(false);

pub fn configure(replay: bool, duck: bool, depth: f32) {
    REPLAY_ON.store(replay, Ordering::Relaxed);
    DUCK_ON.store(duck, Ordering::Relaxed);
    DUCK_DEPTH.store(depth.clamp(0.05, 1.0).to_bits(), Ordering::Relaxed);
    if !duck {
        crate::mixer::DUCK.store(1.0f32.to_bits(), Ordering::Relaxed);
    }
    if !replay {
        for ring in [&CABLE, &DISCORD] {
            if let Some(r) = ring.lock().unwrap().as_ref() {
                r.clear();
            }
        }
    }
}

/// Mémoire pour la sortie vers Discord (une nouvelle à chaque ouverture du câble).
pub fn cable_ring(rate: u32) -> Option<Arc<Ring>> {
    let ring = Ring::new(rate);
    *CABLE.lock().unwrap() = Some(ring.clone());
    Some(ring)
}

/// Ta voix et tes sons, et ce que Discord t'a fait entendre, mélangés.
/// None si rien n'a été gardé.
pub fn snapshot() -> Option<Vec<f32>> {
    let get = |r: &Mutex<Option<Arc<Ring>>>| r.lock().unwrap().as_ref().map(|r| r.take()).unwrap_or_default();
    let mine = get(&CABLE);
    let theirs = get(&DISCORD);
    let len = mine.len().max(theirs.len());
    if len < RATE as usize / 2 {
        return None;
    }
    // Les deux finissent « maintenant » : on les aligne par la fin.
    let mut out = vec![0.0f32; len];
    for part in [&mine, &theirs] {
        let offset = len - part.len();
        for (o, v) in out[offset..].iter_mut().zip(part.iter()) {
            *o += v;
        }
    }
    let peak = out.iter().fold(0.0f32, |m, v| m.max(v.abs()));
    if peak < 1e-4 {
        return None;
    }
    // Ni saturé ni trop faible : crête à -1 dB.
    let g = 0.89 / peak;
    out.iter_mut().for_each(|v| *v *= g);
    Some(out)
}

/// Suit le volume de Discord pour baisser les sons quand quelqu'un parle.
#[cfg_attr(not(windows), allow(dead_code))]
struct Ducker {
    gain: f32,
    hold: f32,
}

#[cfg_attr(not(windows), allow(dead_code))]
impl Ducker {
    fn new() -> Self {
        Self { gain: 1.0, hold: 0.0 }
    }

    /// `block` : échantillons mono à 48 kHz.
    fn feed(&mut self, block: &[f32]) -> f32 {
        if block.is_empty() {
            return self.gain;
        }
        let secs = block.len() as f32 / RATE as f32;
        let rms = (block.iter().map(|v| v * v).sum::<f32>() / block.len() as f32).sqrt();
        // Voix au-dessus de ~-42 dBFS ; on garde la baisse 0,5 s après la
        // dernière syllabe pour ne pas faire « pomper » le son entre les mots.
        if rms > 0.008 {
            self.hold = 0.5;
        } else {
            self.hold = (self.hold - secs).max(0.0);
        }
        let depth = f32::from_bits(DUCK_DEPTH.load(Ordering::Relaxed));
        let target = if self.hold > 0.0 { depth } else { 1.0 };
        // Descend en ~60 ms, remonte en ~400 ms.
        let tau = if target < self.gain { 0.06 } else { 0.4 };
        let k = 1.0 - (-secs / tau).exp();
        self.gain += (target - self.gain) * k;
        self.gain
    }
}

/// Lance la capture de Discord (une fois, au démarrage).
pub fn start_discord_capture() {
    #[cfg(windows)]
    std::thread::Builder::new()
        .name("discord".into())
        .spawn(win::run)
        .ok();
}

#[cfg(windows)]
mod win {
    use super::*;
    use std::time::Duration;
    use wasapi::{AudioClient, Direction, SampleType, StreamMode, WaveFormat};
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
    };

    const NAMES: [&str; 3] = ["discord.exe", "discordptb.exe", "discordcanary.exe"];

    /// Processus principal de Discord (le son vient d'un de ses enfants).
    fn discord_pid() -> Option<u32> {
        let mut procs = Vec::new();
        unsafe {
            let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).ok()?;
            let mut entry = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
            if Process32FirstW(snap, &mut entry).is_ok() {
                loop {
                    let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
                    let name = String::from_utf16_lossy(&entry.szExeFile[..len]).to_lowercase();
                    procs.push((entry.th32ProcessID, entry.th32ParentProcessID, name));
                    if Process32NextW(snap, &mut entry).is_err() {
                        break;
                    }
                }
            }
            let _ = CloseHandle(snap);
        }
        let is_discord = |pid: u32| procs.iter().any(|(p, _, n)| *p == pid && NAMES.contains(&n.as_str()));
        procs
            .iter()
            .find(|(_, parent, name)| NAMES.contains(&name.as_str()) && !is_discord(*parent))
            .map(|(pid, _, _)| *pid)
    }

    pub fn run() {
        let _ = wasapi::initialize_mta();
        loop {
            let wanted = REPLAY_ON.load(Ordering::Relaxed) || DUCK_ON.load(Ordering::Relaxed);
            let pid = if wanted { discord_pid() } else { None };
            match pid {
                Some(pid) => {
                    if let Err(e) = capture(pid) {
                        crate::journal::write(&format!("Capture de Discord : {e}"));
                    }
                    DISCORD_HEARD.store(false, Ordering::Relaxed);
                    crate::mixer::DUCK.store(1.0f32.to_bits(), Ordering::Relaxed);
                    std::thread::sleep(Duration::from_secs(3));
                }
                None => std::thread::sleep(Duration::from_secs(5)),
            }
        }
    }

    fn capture(pid: u32) -> Result<(), String> {
        let format = WaveFormat::new(32, 32, &SampleType::Float, RATE as usize, 2, None);
        let mut client = AudioClient::new_application_loopback_client(pid, true).map_err(|e| e.to_string())?;
        let mode = StreamMode::EventsShared { autoconvert: true, buffer_duration_hns: 0 };
        client.initialize_client(&format, &Direction::Capture, &mode).map_err(|e| e.to_string())?;
        let event = client.set_get_eventhandle().map_err(|e| e.to_string())?;
        let capture = client.get_audiocaptureclient().map_err(|e| e.to_string())?;
        client.start_stream().map_err(|e| e.to_string())?;

        let ring = Ring::new(RATE);
        *DISCORD.lock().unwrap() = Some(ring.clone());
        DISCORD_HEARD.store(true, Ordering::Relaxed);
        let mut ducker = Ducker::new();
        let mut bytes: VecDeque<u8> = VecDeque::new();
        let mut mono = Vec::new();
        loop {
            // Plus rien de demandé, ou Discord fermé : on s'arrête.
            if !REPLAY_ON.load(Ordering::Relaxed) && !DUCK_ON.load(Ordering::Relaxed) {
                break;
            }
            if event.wait_for_event(2000).is_err() {
                // Pas de son depuis 2 s : normal quand personne ne parle. On
                // vérifie juste que Discord tourne encore.
                if discord_pid() != Some(pid) {
                    break;
                }
                ducker.feed(&[0.0; 960]);
                continue;
            }
            capture.read_from_device_to_deque(&mut bytes).map_err(|e| e.to_string())?;
            let frames = bytes.len() / 8;
            if frames == 0 {
                continue;
            }
            mono.clear();
            let raw: Vec<u8> = bytes.drain(..frames * 8).collect();
            for f in raw.chunks_exact(8) {
                let l = f32::from_le_bytes([f[0], f[1], f[2], f[3]]);
                let r = f32::from_le_bytes([f[4], f[5], f[6], f[7]]);
                mono.push((l + r) * 0.5);
            }
            if REPLAY_ON.load(Ordering::Relaxed) {
                ring.push(&mono, 1, RATE);
            }
            if DUCK_ON.load(Ordering::Relaxed) {
                let g = ducker.feed(&mono);
                crate::mixer::DUCK.store(g.to_bits(), Ordering::Relaxed);
            }
        }
        let _ = client.stop_stream();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_keeps_only_the_last_seconds() {
        let ring = Ring::new(1000);
        let data: Vec<f32> = (0..40_000).map(|i| i as f32).collect();
        ring.push(&data, 1, 1000);
        let buf = ring.buf.lock().unwrap();
        assert_eq!(buf.len(), 30_000);
        assert_eq!(*buf.back().unwrap(), 39_999.0);
    }

    #[test]
    fn stereo_is_folded_to_mono() {
        let ring = Ring::new(48_000);
        ring.push(&[1.0, 0.0, 0.5, 0.5], 2, 48_000);
        assert_eq!(ring.take(), vec![0.5, 0.5]);
    }

    #[test]
    fn rate_conversion_keeps_duration() {
        let data = vec![0.0f32; 44_100];
        let out = to_rate(&data, 44_100, 48_000);
        assert!((out.len() as i64 - 48_000).abs() < 3, "{}", out.len());
    }

    #[test]
    fn ducker_drops_fast_and_comes_back() {
        DUCK_DEPTH.store(0.35f32.to_bits(), Ordering::Relaxed);
        let mut d = Ducker::new();
        let voice: Vec<f32> = (0..480).map(|i| (i as f32 * 0.1).sin() * 0.2).collect();
        let mut g = 1.0;
        for _ in 0..20 {
            g = d.feed(&voice); // 200 ms de voix
        }
        assert!(g < 0.4, "{g}");
        for _ in 0..300 {
            g = d.feed(&[0.0; 480]); // 3 s de silence
        }
        assert!(g > 0.95, "{g}");
    }
}
