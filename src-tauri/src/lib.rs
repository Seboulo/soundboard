mod analysis;
mod backup;
mod audio;
mod calibration;
mod config;
mod decoder;
mod denoise;
mod dynamics;
mod effects;
mod giphy;
mod journal;
mod mixer;
mod resample;
mod sound_fx;
mod online;
mod phone;
mod youtube;
mod extras;
mod replay;
mod room;
mod share;
mod tts;
mod clip;

use audio::{AudioEvent, AudioHandle, DeviceList, PlayOutcome, PlayRequest};
use config::{Config, Settings, Sound};
use online::{OnlineSound, UpdateInfo};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};
use tauri_plugin_autostart::ManagerExt as AutostartExt;
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};
use tauri_plugin_opener::OpenerExt;
use tauri_plugin_updater::UpdaterExt;

const AUDIO_EXTENSIONS: &[&str] = &["mp3", "wav", "ogg", "flac", "m4a", "aac"];
const COLORS: &[&str] = &[
    "#5865f2", "#57f287", "#fee75c", "#eb459e", "#ed4245", "#3ba55c", "#faa61a", "#9b59b6",
];
pub const VB_CABLE_URL: &str = "https://vb-audio.com/Cable/";
const FREESOUND_KEY_URL: &str = "https://freesound.org/apiv2/apply";

#[derive(Clone)]
enum HotkeyAction {
    Play(String),
    StopAll,
    ToggleMute,
    Random,
    Replay,
}

struct AppState {
    config: Mutex<Config>,
    config_path: PathBuf,
    sounds_dir: PathBuf,
    audio: AudioHandle,
    hotkeys: Mutex<HashMap<u32, HotkeyAction>>,
    /// Problèmes à montrer dans l'interface (raccourci déjà pris, sortie absente…).
    warnings: Mutex<Vec<String>>,
    mic_muted: AtomicBool,
    /// Liste des périphériques audio. Les énumérer prend du temps sous
    /// Windows : on la garde et on ne la relit qu'à la demande.
    devices: Mutex<DeviceList>,
    /// Formes d'onde déjà calculées (décoder un long son prend du temps).
    waveforms: Mutex<HashMap<String, Vec<f32>>>,
    /// Serveur de la page du téléphone, quand il est activé.
    phone: Mutex<Option<phone::PhoneServer>>,
    /// Sons en cours (pour la page du téléphone).
    playing_ids: Mutex<Vec<String>>,
    /// Un seul téléchargement YouTube à la fois.
    youtube_busy: AtomicBool,
    /// Dernier son tiré au hasard, pour ne pas retomber dessus deux fois de suite.
    last_random: Mutex<Option<String>>,
    /// Dernier son supprimé et sa position, pour « Annuler ».
    trash: Mutex<Option<(Sound, usize)>>,
    /// Mode Surprise en marche (jamais au lancement de l'app).
    surprise_on: AtomicBool,
    /// Change à chaque marche / arrêt : l'ancienne attente s'arrête d'elle-même.
    surprise_gen: AtomicU64,
    /// Salon entre potes et ce qui va avec (voir `extras`).
    extras: extras::Extras,
    /// Extraits reçus des potes (voir `clip`).
    clips: clip::Clips,
}

#[derive(Clone, Serialize)]
struct Snapshot {
    config: Config,
    devices: DeviceList,
    warnings: Vec<String>,
    vb_cable_url: &'static str,
    mic_muted: bool,
    version: &'static str,
    /// Sons dont le fichier a disparu (antivirus, nettoyage…).
    missing: Vec<String>,
    surprise_on: bool,
    /// La voix de tes potes est captée dans Discord (rejouer, baisse auto).
    discord_heard: bool,
}

impl AppState {
    fn save(&self, config: &Config) -> Result<(), String> {
        config.save(&self.config_path)
    }

    fn snapshot(&self) -> Snapshot {
        let mut snap = Snapshot {
            missing: Vec::new(),
            config: self.config.lock().unwrap().clone(),
            devices: self.devices.lock().unwrap().clone(),
            warnings: self.warnings.lock().unwrap().clone(),
            vb_cable_url: VB_CABLE_URL,
            mic_muted: self.mic_muted.load(Ordering::Relaxed),
            version: env!("CARGO_PKG_VERSION"),
            surprise_on: self.surprise_on.load(Ordering::Relaxed),
            discord_heard: replay::DISCORD_HEARD.load(Ordering::Relaxed),
        };
        snap.missing = snap.config.sounds.iter().filter(|x| !x.path.exists()).map(|x| x.id.clone()).collect();
        snap
    }

    fn set_warnings(&self, kind: &str, items: Vec<String>) {
        let mut w = self.warnings.lock().unwrap();
        let fresh: Vec<String> = items.into_iter().map(|m| format!("{kind}{m}")).collect();
        for m in fresh.iter().filter(|m| !w.contains(m)) {
            journal::write(m);
        }
        w.retain(|m| !m.starts_with(kind));
        w.extend(fresh);
    }
}

const WARN_AUDIO: &str = "Audio : ";
const WARN_HOTKEY: &str = "Raccourci : ";
const WARN_STARTUP: &str = "Démarrage : ";
const WARN_PHONE: &str = "Téléphone : ";
const WARN_CONFIG: &str = "Réglages : ";
/// Démarre, redémarre ou arrête la page du téléphone selon les réglages.
fn apply_phone(app: &AppHandle) {
    let state = app.state::<AppState>();
    // Arrête l'ancien serveur avant d'en relancer un sur le même port.
    *state.phone.lock().unwrap() = None;
    let settings = state.config.lock().unwrap().settings.clone();
    if !settings.phone_enabled {
        state.set_warnings(WARN_PHONE, vec![]);
        return;
    }
    let handle = app.clone();
    let handler: phone::Handler = std::sync::Arc::new(move |req| phone_request(&handle, req));
    let res = phone::PhoneServer::start(settings.phone_port, settings.phone_token.clone(), handler);
    match res {
        Ok(server) => {
            *state.phone.lock().unwrap() = Some(server);
            state.set_warnings(WARN_PHONE, vec![]);
        }
        Err(e) => state.set_warnings(WARN_PHONE, vec![e]),
    }
}

fn phone_request(app: &AppHandle, req: phone::Request) -> Result<serde_json::Value, String> {
    let state = app.state::<AppState>();
    match req {
        phone::Request::State => {
            let config = state.config.lock().unwrap();
            let sounds: Vec<_> = config
                .sounds
                .iter()
                .map(|s| {
                    serde_json::json!({
                        "id": s.id, "name": s.name, "emoji": s.emoji, "color": s.color,
                        "category": s.category, "favorite": s.favorite,
                    })
                })
                .collect();
            let playing = state.playing_ids.lock().unwrap().clone();
            Ok(serde_json::json!({ "sounds": sounds, "playing": playing }))
        }
        phone::Request::Play(id) => {
            let sound = find_sound(&state, &id)?;
            note_play(&state, &sound);
            state.audio.toggle(play_request(&state, &sound))?;
            let _ = app.emit("sound-triggered", &sound.id);
            Ok(serde_json::json!({ "ok": true }))
        }
        phone::Request::Stop => {
            state.audio.stop_all();
            clip::close(app);
            Ok(serde_json::json!({ "ok": true }))
        }
        phone::Request::Random(category) => {
            let sound = pick_random(&state, category.as_deref()).ok_or("aucun son")?;
            note_play(&state, &sound);
            state.audio.toggle(play_request(&state, &sound))?;
            let _ = app.emit("sound-triggered", &sound.id);
            Ok(serde_json::json!({ "id": sound.id }))
        }
    }
}
const MINIMIZED_ARG: &str = "--minimized";

fn find_sound(state: &AppState, id: &str) -> Result<Sound, String> {
    state
        .config
        .lock()
        .unwrap()
        .sounds
        .iter()
        .find(|s| s.id == id)
        .cloned()
        .ok_or_else(|| "son introuvable".into())
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Compte une lecture pour les statistiques (pas quand le clic arrête le son).
fn note_play(state: &AppState, sound: &Sound) {
    if state.playing_ids.lock().unwrap().contains(&sound.id) {
        return;
    }
    let mut config = state.config.lock().unwrap();
    if let Some(s) = config.sounds.iter_mut().find(|s| s.id == sound.id) {
        s.plays += 1;
        s.played_ms += sound.play_duration_ms().unwrap_or(0);
        s.last_played = now_secs();
        let _ = state.save(&config);
    }
}

fn play_request(state: &AppState, sound: &Sound) -> PlayRequest {
    let (normalize, target) = {
        let config = state.config.lock().unwrap();
        (config.settings.normalize, config.settings.loudness_target)
    };
    let gain = if normalize { sound.normalize_gain(target) } else { 1.0 };
    PlayRequest {
        id: sound.id.clone(),
        path: sound.path.clone(),
        gain,
        volume: sound.volume,
        target_lufs: target,
        start_ms: sound.start_ms,
        end_ms: sound.end_ms,
        duration_ms: sound.play_duration_ms(),
        looping: sound.looping,
        keep_playing: sound.keep_playing,
        fx: sound.fx,
    }
}

fn show_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

fn apply_autostart(app: &AppHandle, enabled: bool) {
    let autolaunch = app.autolaunch();
    let res = if enabled {
        autolaunch.enable()
    } else if autolaunch.is_enabled().unwrap_or(false) {
        autolaunch.disable()
    } else {
        Ok(())
    };
    let warnings = res.err().map(|e| vec![e.to_string()]).unwrap_or_default();
    app.state::<AppState>().set_warnings(WARN_STARTUP, warnings);
}

/// Mesure en arrière-plan le volume des passages découpés ou avec effets,
/// pour que l'égalisation porte sur ce qui est vraiment joué.
fn measure_segments(app: AppHandle) {
    static RUNNING: AtomicBool = AtomicBool::new(false);
    if RUNNING.swap(true, Ordering::AcqRel) {
        // Une mesure tourne déjà : elle reprendra la liste en finissant.
        PENDING.store(true, Ordering::Release);
        return;
    }
    static PENDING: AtomicBool = AtomicBool::new(false);
    std::thread::spawn(move || {
        let state = app.state::<AppState>();
        loop {
            PENDING.store(false, Ordering::Release);
            let todo: Vec<(String, PathBuf, config::SegmentKey)> = state
                .config
                .lock()
                .unwrap()
                .sounds
                .iter()
                .filter(|s| s.needs_segment())
                .filter_map(|s| Some((s.id.clone(), s.path.clone(), s.segment_key()?)))
                .collect();
            for (id, path, key) in todo {
                let Ok(analysis) = analysis::analyze_segment(&path, &key) else { continue };
                let mut config = state.config.lock().unwrap();
                // La découpe a pu changer pendant la mesure.
                if let Some(s) = config.sounds.iter_mut().find(|s| s.id == id && s.segment_key() == Some(key)) {
                    s.segment = Some(config::SegmentLevel { key, analysis });
                    let _ = state.save(&config);
                }
            }
            if !PENDING.load(Ordering::Acquire) {
                break;
            }
        }
        RUNNING.store(false, Ordering::Release);
        // Une demande arrivée juste entre la dernière vérification et la
        // ligne du dessus aurait été perdue : on la reprend.
        if PENDING.swap(false, Ordering::AcqRel) {
            measure_segments(app.clone());
        }
    });
}

/// Tourne au lancement, une fois l'audio ouvert, sur le fil de démarrage :
/// un seul son décodé à la fois, et chaque mesure est enregistrée tout de
/// suite. Fermer l'appli en cours de route ne fait donc pas tout recommencer
/// au lancement suivant, et un fichier illisible n'est plus relu à chaque
/// démarrage.
fn analyze_missing(app: &AppHandle) {
    let state = app.state::<AppState>();
    let todo: Vec<(String, PathBuf)> = state
        .config
        .lock()
        .unwrap()
        .sounds
        .iter()
        .filter(|s| s.analysis.is_none_or(|a| a.loudness <= 0.0 || a.lufs >= 0.0 || !a.mono_fold))
        .map(|s| (s.id.clone(), s.path.clone()))
        .collect();
    if todo.is_empty() {
        return;
    }
    for (id, path) in todo {
        let result = analysis::analyze(&path);
        let mut config = state.config.lock().unwrap();
        let Some(s) = config.sounds.iter_mut().find(|s| s.id == id) else { continue };
        match result {
            Ok(a) => s.analysis = Some(a),
            // Fichier présent mais illisible : on garde l'ancienne mesure
            // sans la redemander à chaque lancement.
            Err(_) if path.exists() => match s.analysis.as_mut() {
                Some(a) => a.mono_fold = true,
                None => continue,
            },
            Err(_) => continue,
        }
        let _ = state.save(&config);
    }
    let _ = app.emit("state-changed", ());
}

fn apply_audio(state: &AppState, settings: Settings) {
    let warnings = match state.audio.configure(settings) {
        Ok(()) => vec![],
        Err(e) => e.lines().map(String::from).collect(),
    };
    state.set_warnings(WARN_AUDIO, warnings);
}

/// Ce qui compte pour l'audio : le périphérique réellement utilisé pour le
/// casque, le câble et le micro, et s'il est branché.
fn audio_route(settings: &Settings, d: &DeviceList) -> [Option<String>; 3] {
    let pick = |chosen: &Option<String>, default: &Option<String>, list: &[String]| {
        match chosen {
            Some(name) => list.contains(name).then(|| name.clone()),
            None => default.clone(),
        }
    };
    [
        pick(&settings.monitor_device, &d.default_output, &d.outputs),
        settings.virtual_device.as_ref().filter(|n| d.outputs.contains(n)).cloned(),
        pick(&settings.mic_device, &d.default_input, &d.inputs),
    ]
}

/// Surveille les casques et micros : quand l'un est débranché, rebranché ou
/// que Windows change le périphérique par défaut, l'audio se reconnecte seul.
fn watch_devices(app: AppHandle) {
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_secs(3));
        let devices = audio::list_devices();
        let state = app.state::<AppState>();
        let before = state.devices.lock().unwrap().clone();
        if devices == before {
            continue;
        }
        let settings = state.config.lock().unwrap().settings.clone();
        let changed = audio_route(&settings, &devices) != audio_route(&settings, &before);
        *state.devices.lock().unwrap() = devices;
        if changed {
            apply_audio(&state, settings);
        }
        let _ = app.emit("snapshot", state.snapshot());
    });
}

fn register_hotkeys(app: &AppHandle) {
    let state = app.state::<AppState>();
    let config = state.config.lock().unwrap().clone();
    let gs = app.global_shortcut();
    let _ = gs.unregister_all();

    let mut entries: Vec<(String, HotkeyAction, String)> = config
        .sounds
        .iter()
        .filter_map(|s| {
            s.hotkey
                .clone()
                .map(|h| (h, HotkeyAction::Play(s.id.clone()), s.name.clone()))
        })
        .collect();
    for (hotkey, action, label) in [
        (&config.settings.stop_hotkey, HotkeyAction::StopAll, "Tout arrêter"),
        (&config.settings.mute_hotkey, HotkeyAction::ToggleMute, "Couper le micro"),
        (&config.settings.random_hotkey, HotkeyAction::Random, "Son au hasard"),
        (&config.settings.replay_hotkey, HotkeyAction::Replay, "Rejouer 30 s"),
    ] {
        if let Some(h) = hotkey.clone().filter(|h| !h.is_empty()) {
            entries.push((h, action, label.into()));
        }
    }

    let mut map = HashMap::new();
    let mut errors = Vec::new();
    for (hotkey, action, label) in entries {
        let shortcut = match Shortcut::from_str(&hotkey) {
            Ok(s) => s,
            Err(_) => {
                errors.push(format!("« {hotkey} » ({label}) n'est pas reconnu"));
                continue;
            }
        };
        if map.contains_key(&shortcut.id()) {
            errors.push(format!("« {hotkey} » ({label}) est utilisé deux fois"));
            continue;
        }
        match gs.register(shortcut) {
            Ok(()) => {
                map.insert(shortcut.id(), action);
            }
            Err(e) => errors.push(format!("« {hotkey} » ({label}) indisponible : {e}")),
        }
    }
    *state.hotkeys.lock().unwrap() = map;
    state.set_warnings(WARN_HOTKEY, errors);
}

fn on_hotkey(app: &AppHandle, shortcut: &Shortcut) {
    let state = app.state::<AppState>();
    let action = state.hotkeys.lock().unwrap().get(&shortcut.id()).cloned();
    match action {
        Some(HotkeyAction::Play(id)) => {
            let sound = state
                .config
                .lock()
                .unwrap()
                .sounds
                .iter()
                .find(|s| s.id == id)
                .cloned();
            if let Some(sound) = sound {
                extras::play_or_send(app, sound);
            }
        }
        Some(HotkeyAction::StopAll) => {
            state.audio.stop_all();
            clip::close(app);
            let _ = app.emit("stopped", ());
        }
        Some(HotkeyAction::ToggleMute) => {
            let muted = !state.mic_muted.load(Ordering::Relaxed);
            set_muted(app, muted);
        }
        Some(HotkeyAction::Random) => {
            if let Some(sound) = pick_random(&state, None) {
                extras::play_or_send(app, sound);
            }
        }
        Some(HotkeyAction::Replay) => extras::replay_from_hotkey(app),
        None => {}
    }
}

fn set_muted(app: &AppHandle, muted: bool) {
    let state = app.state::<AppState>();
    state.mic_muted.store(muted, Ordering::Relaxed);
    state.audio.set_mic_muted(muted);
    let _ = app.emit("mic-muted", muted);
}

/// Tire un son au hasard (dans une catégorie si on en donne une).
fn pick_random(state: &AppState, category: Option<&str>) -> Option<Sound> {
    pick_random_where(state, |s| category.is_none() || s.category.as_deref() == category)
}

fn pick_random_where(state: &AppState, keep: impl Fn(&Sound) -> bool) -> Option<Sound> {
    let config = state.config.lock().unwrap();
    let mut last = state.last_random.lock().unwrap();
    let pool: Vec<&Sound> = config.sounds.iter().filter(|s| keep(s)).collect();
    let candidates: Vec<&Sound> = if pool.len() > 1 {
        pool.iter().copied().filter(|s| Some(&s.id) != last.as_ref()).collect()
    } else {
        pool
    };
    if candidates.is_empty() {
        return None;
    }
    let index = (random_u64() % candidates.len() as u64) as usize;
    let sound = candidates[index].clone();
    *last = Some(sound.id.clone());
    Some(sound)
}

fn random_u64() -> u64 {
    uuid::Uuid::new_v4().as_u64_pair().0
}

/// Attente avant le prochain son surprise, tirée entre le minimum et le maximum.
fn surprise_delay(min_minutes: u32, max_minutes: u32) -> std::time::Duration {
    let min = u64::from(min_minutes.max(1)) * 60;
    let max = (u64::from(max_minutes) * 60).max(min);
    std::time::Duration::from_secs(min + random_u64() % (max - min + 1))
}

/// Mode Surprise : de temps en temps, un son de la liste part tout seul
/// (vers Discord, comme un son cliqué).
fn start_surprise(app: &AppHandle) {
    let state = app.state::<AppState>();
    let gen = state.surprise_gen.fetch_add(1, Ordering::SeqCst) + 1;
    if !state.surprise_on.load(Ordering::SeqCst) {
        return;
    }
    let app = app.clone();
    std::thread::spawn(move || loop {
        let state = app.state::<AppState>();
        let delay = {
            let s = &state.config.lock().unwrap().settings;
            surprise_delay(s.surprise_min_minutes, s.surprise_max_minutes)
        };
        let deadline = std::time::Instant::now() + delay;
        while std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(500));
            if state.surprise_gen.load(Ordering::SeqCst) != gen {
                return;
            }
        }
        let list = state.config.lock().unwrap().settings.surprise_sounds.clone();
        let playing = state.playing_ids.lock().unwrap().clone();
        let sound = pick_random_where(&state, |s| {
            (list.is_empty() || list.contains(&s.id)) && !playing.contains(&s.id) && s.path.exists()
        });
        if let Some(sound) = sound {
            note_play(&state, &sound);
            state.audio.toggle_detached(play_request(&state, &sound));
            let _ = app.emit("sound-triggered", &sound.id);
        }
    });
}

const MAX_SOUND_FILE: u64 = 200 * 1024 * 1024;

fn import_file(src: &Path, sounds_dir: &Path, color_index: usize) -> Result<Sound, String> {
    let ext = src
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .unwrap_or_default();
    if !AUDIO_EXTENSIONS.contains(&ext.as_str()) {
        return Err(format!("{} : format non pris en charge", src.display()));
    }
    let name = src
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Son")
        .replace(['_', '-'], " ");
    // Un énorme fichier (enregistrement de plusieurs heures) serait chargé
    // en entier en mémoire, puis analysé : on refuse avant.
    let size = fs::metadata(src).map_err(|e| format!("{} : {e}", src.display()))?.len();
    if size > MAX_SOUND_FILE {
        return Err(format!("{} : fichier trop gros (200 Mo au plus)", src.display()));
    }
    let data = fs::read(src).map_err(|e| format!("{} : {e}", src.display()))?;
    store_sound(&data, &ext, name, sounds_dir, color_index)
        .map_err(|e| format!("{} : {e}", src.display()))
}

/// Range les données d'un son dans le dossier de l'app et l'analyse.
fn store_sound(
    data: &[u8],
    ext: &str,
    name: String,
    sounds_dir: &Path,
    color_index: usize,
) -> Result<Sound, String> {
    let id = uuid::Uuid::new_v4().to_string();
    fs::create_dir_all(sounds_dir).map_err(|e| e.to_string())?;
    let dest = sounds_dir.join(format!("{id}.{ext}"));
    fs::write(&dest, data).map_err(|e| e.to_string())?;
    let analysis = match analysis::analyze(&dest) {
        Ok(a) => a,
        Err(e) => {
            let _ = fs::remove_file(&dest);
            return Err(e);
        }
    };
    Ok(Sound {
        id,
        name,
        path: dest,
        hotkey: None,
        volume: 1.0,
        color: COLORS[color_index % COLORS.len()].into(),
        start_ms: 0,
        end_ms: None,
        analysis: Some(analysis),
        category: None,
        looping: false,
        emoji: None,
        favorite: false,
        keep_playing: false,
        added_at: now_secs(),
        image: None,
        image_still: None,
        plays: 0,
        played_ms: 0,
        last_played: 0,
        fx: Default::default(),
        segment: None,
        pack_link: None,
        pack_key: Some(share::key_of(data)),
    })
}

fn clean_category(category: Option<String>) -> Option<String> {
    category
        .map(|c| c.trim().chars().take(30).collect::<String>())
        .filter(|c| !c.is_empty())
}

/// Un emoji peut faire plusieurs caractères (drapeaux, couleurs de peau…).
fn clean_emoji(emoji: Option<String>) -> Option<String> {
    emoji
        .map(|e| e.trim().chars().take(8).collect::<String>())
        .filter(|e| !e.is_empty())
}

#[tauri::command]
fn get_state(state: State<AppState>) -> Snapshot {
    state.snapshot()
}

/// Relit la liste des périphériques (réglages, assistant, nouveau casque…).
#[tauri::command]
async fn refresh_devices(app: AppHandle) -> Result<Snapshot, String> {
    let devices = tauri::async_runtime::spawn_blocking(audio::list_devices)
        .await
        .map_err(|e| e.to_string())?;
    let state = app.state::<AppState>();
    *state.devices.lock().unwrap() = devices;
    Ok(state.snapshot())
}

/// Importe des fichiers du PC. L'analyse (décodage complet de chaque son)
/// se fait hors du fil de l'interface et sans bloquer la config : les
/// raccourcis et les clics continuent de jouer pendant ce temps.
#[tauri::command]
async fn add_sounds(app: AppHandle, paths: Vec<String>) -> Result<Snapshot, String> {
    let state = app.state::<AppState>();
    let first_index = state.config.lock().unwrap().sounds.len();
    let sounds_dir = state.sounds_dir.clone();
    let results = tauri::async_runtime::spawn_blocking(move || {
        paths
            .iter()
            .enumerate()
            .map(|(i, p)| import_file(Path::new(p), &sounds_dir, first_index + i))
            .collect::<Vec<_>>()
    })
    .await
    .map_err(|e| e.to_string())?;
    let mut errors = Vec::new();
    {
        let mut config = state.config.lock().unwrap();
        for res in results {
            match res {
                Ok(sound) => config.sounds.push(sound),
                Err(e) => errors.push(e),
            }
        }
        state.save(&config)?;
    }
    register_hotkeys(&app);
    if errors.is_empty() {
        Ok(state.snapshot())
    } else {
        Err(errors.join("\n"))
    }
}

#[tauri::command]
async fn pick_sounds(app: AppHandle) -> Result<Snapshot, String> {
    let files = app
        .dialog()
        .file()
        .set_title("Ajouter des sons")
        .add_filter("Audio", AUDIO_EXTENSIONS)
        .blocking_pick_files();
    let paths: Vec<String> = files
        .unwrap_or_default()
        .into_iter()
        .filter_map(|f| f.into_path().ok())
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    add_sounds(app, paths).await
}

/// Remplace le fichier d'un son (introuvable ou à changer) en gardant son
/// nom, son raccourci, son image et ses effets.
#[tauri::command]
async fn replace_sound_file(app: AppHandle, id: String) -> Result<Option<Snapshot>, String> {
    let Some(src) = app
        .dialog()
        .file()
        .set_title("Choisir le nouveau fichier du son")
        .add_filter("Audio", AUDIO_EXTENSIONS)
        .blocking_pick_file()
        .and_then(|f| f.into_path().ok())
    else {
        return Ok(None);
    };
    let state = app.state::<AppState>();
    let sounds_dir = state.sounds_dir.clone();
    let ext = src
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_lowercase)
        .ok_or("fichier sans extension")?;
    if fs::metadata(&src).map(|m| m.len()).unwrap_or(0) > MAX_SOUND_FILE {
        return Err("fichier trop gros (200 Mo au plus)".into());
    }
    let file_id = id.clone();
    let (dest, analysis) = tauri::async_runtime::spawn_blocking(move || -> Result<_, String> {
        fs::create_dir_all(&sounds_dir).map_err(|e| e.to_string())?;
        // Nom nouveau : l'ancien fichier peut encore être ouvert par le moteur audio.
        let dest = sounds_dir.join(format!("{file_id}-{}.{ext}", now_secs()));
        fs::copy(&src, &dest).map_err(|e| format!("{} : {e}", src.display()))?;
        match analysis::analyze(&dest) {
            Ok(a) => Ok((dest, a)),
            Err(e) => {
                let _ = fs::remove_file(&dest);
                Err(e)
            }
        }
    })
    .await
    .map_err(|e| e.to_string())??;
    state.audio.stop(&id);
    {
        let mut config = state.config.lock().unwrap();
        let sound = config.sounds.iter_mut().find(|s| s.id == id).ok_or("son introuvable")?;
        let old = std::mem::replace(&mut sound.path, dest);
        if old.starts_with(&state.sounds_dir) {
            let _ = fs::remove_file(old);
        }
        sound.analysis = Some(analysis);
        sound.segment = None;
        sound.start_ms = 0;
        sound.end_ms = None;
        state.save(&config)?;
    }
    state.waveforms.lock().unwrap().remove(&id);
    measure_segments(app.clone());
    Ok(Some(state.snapshot()))
}

#[tauri::command]
fn update_sound(app: AppHandle, state: State<AppState>, sound: Sound) -> Result<Snapshot, String> {
    {
        let mut config = state.config.lock().unwrap();
        let existing = config
            .sounds
            .iter_mut()
            .find(|s| s.id == sound.id)
            .ok_or("son introuvable")?;
        // Le chemin n'est jamais modifié depuis l'interface.
        existing.name = sound.name;
        existing.hotkey = sound.hotkey.filter(|h| !h.is_empty());
        existing.volume = sound.volume.clamp(0.0, 2.0);
        existing.color = sound.color;
        existing.category = clean_category(sound.category);
        existing.looping = sound.looping;
        existing.emoji = clean_emoji(sound.emoji);
        existing.favorite = sound.favorite;
        existing.keep_playing = sound.keep_playing;
        existing.fx = sound.fx.sanitized();
        let total = existing.analysis.map(|a| a.duration_ms).unwrap_or(u64::MAX);
        existing.start_ms = sound.start_ms.min(total);
        // Une fin au bout du fichier revient à « pas de découpe ».
        existing.end_ms = sound
            .end_ms
            .filter(|&e| e < total)
            .map(|e| e.max(existing.start_ms + 50));
        state.save(&config)?;
    }
    register_hotkeys(&app);
    measure_segments(app.clone());
    Ok(state.snapshot())
}

#[tauri::command]
fn remove_sound(app: AppHandle, state: State<AppState>, id: String) -> Result<Snapshot, String> {
    {
        let mut config = state.config.lock().unwrap();
        if let Some(pos) = config.sounds.iter().position(|s| s.id == id) {
            let sound = config.sounds.remove(pos);
            // Sous Windows, un fichier en cours de lecture ne peut pas être
            // déplacé ni supprimé : on l'arrête d'abord.
            state.audio.stop(&sound.id);
            // Le son précédent de la corbeille part pour de bon.
            empty_trash(&state);
            let trashed = move_to_trash(&state, sound);
            *state.trash.lock().unwrap() = Some((trashed, pos));
        }
        state.save(&config)?;
    }
    register_hotkeys(&app);
    Ok(state.snapshot())
}

/// Remet le dernier son supprimé à sa place (bouton « Annuler »).
#[tauri::command]
fn restore_sound(app: AppHandle, state: State<AppState>, id: Option<String>) -> Result<Snapshot, String> {
    let (mut sound, pos) = {
        let mut trash = state.trash.lock().unwrap();
        // Le bouton « Annuler » d'un son ne doit jamais en ramener un autre.
        if id.is_some() && trash.as_ref().map(|(s, _)| &s.id) != id.as_ref() {
            return Err("Ce son ne peut plus être restauré : seule la dernière suppression s'annule".into());
        }
        trash.take().ok_or("Rien à restaurer")?
    };
    let trash = trash_dir(&state);
    let back = |path: &mut PathBuf, home: PathBuf| {
        if path.starts_with(&trash) {
            if let Some(name) = path.file_name() {
                let dest = home.join(name);
                if fs::rename(&*path, &dest).is_ok() {
                    *path = dest;
                }
            }
        }
    };
    back(&mut sound.path, state.sounds_dir.clone());
    for img in [&mut sound.image, &mut sound.image_still].into_iter().flatten() {
        back(img, images_dir(&state));
    }
    {
        let mut config = state.config.lock().unwrap();
        // Un raccourci repris entre-temps par un autre son reste à celui-ci.
        if let Some(hk) = &sound.hotkey {
            if config.sounds.iter().any(|s| s.hotkey.as_ref() == Some(hk)) {
                sound.hotkey = None;
            }
        }
        let pos = pos.min(config.sounds.len());
        config.sounds.insert(pos, sound);
        state.save(&config)?;
    }
    register_hotkeys(&app);
    Ok(state.snapshot())
}

fn trash_dir(state: &AppState) -> PathBuf {
    state.sounds_dir.with_file_name("corbeille")
}

/// Déplace les fichiers du son (rangés par l'app) dans la corbeille.
fn move_to_trash(state: &AppState, mut sound: Sound) -> Sound {
    let trash = trash_dir(state);
    let _ = fs::create_dir_all(&trash);
    let images = images_dir(state);
    let stash = |path: &mut PathBuf, home: &Path| {
        if !path.starts_with(home) {
            return;
        }
        if let Some(name) = path.file_name() {
            let dest = trash.join(name);
            if fs::rename(&*path, &dest).is_ok() {
                *path = dest;
            } else {
                let _ = fs::remove_file(&*path);
            }
        }
    };
    let sounds_dir = state.sounds_dir.clone();
    stash(&mut sound.path, &sounds_dir);
    for img in [&mut sound.image, &mut sound.image_still].into_iter().flatten() {
        stash(img, &images);
    }
    sound
}

/// Vide la corbeille (au lancement et à chaque nouvelle suppression).
fn empty_trash(state: &AppState) {
    state.trash.lock().unwrap().take();
    let _ = fs::remove_dir_all(trash_dir(state));
}

fn images_dir(state: &AppState) -> PathBuf {
    state.sounds_dir.with_file_name("images")
}

/// Supprime les fichiers d'image d'un son (seulement ceux rangés par l'app).
fn remove_image_files(state: &AppState, sound: &Sound) {
    let dir = images_dir(state);
    for path in [&sound.image, &sound.image_still].into_iter().flatten() {
        if path.starts_with(&dir) {
            let _ = fs::remove_file(path);
        }
    }
}

fn giphy_key(state: &AppState) -> Option<String> {
    state
        .config
        .lock()
        .unwrap()
        .settings
        .giphy_key
        .as_deref()
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .map(String::from)
}

/// Range les images d'un son et remplace les anciennes.
fn store_image(
    state: &AppState,
    id: &str,
    anim: (Vec<u8>, &str),
    still: Option<(Vec<u8>, &str)>,
) -> Result<(), String> {
    let dir = images_dir(state);
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    // Nom unique à chaque fois : l'interface ne garde pas l'ancienne en cache.
    let stamp = uuid::Uuid::new_v4().simple().to_string();
    let anim_path = dir.join(format!("{id}-{stamp}.{}", anim.1));
    fs::write(&anim_path, &anim.0).map_err(|e| e.to_string())?;
    let still_path = match still {
        Some((data, ext)) => {
            let p = dir.join(format!("{id}-{stamp}-still.{ext}"));
            fs::write(&p, data).map_err(|e| e.to_string())?;
            p
        }
        None => anim_path.clone(),
    };
    let mut config = state.config.lock().unwrap();
    let Some(sound) = config.sounds.iter_mut().find(|s| s.id == id) else {
        let _ = fs::remove_file(&anim_path);
        let _ = fs::remove_file(&still_path);
        return Err("son introuvable".into());
    };
    let old = sound.clone();
    sound.image = Some(anim_path);
    sound.image_still = Some(still_path);
    state.save(&config)?;
    drop(config);
    remove_image_files(state, &old);
    Ok(())
}

#[tauri::command]
async fn search_images(app: AppHandle, query: String) -> Result<Vec<giphy::GifResult>, String> {
    let key = giphy_key(&app.state::<AppState>())
        .ok_or("Ajoute ta clé GIPHY dans les réglages (⚙ → Général) pour chercher des images.")?;
    giphy::search(query.trim(), &key, 18).await
}

#[tauri::command]
async fn set_sound_image(app: AppHandle, id: String, still: String, anim: String) -> Result<Snapshot, String> {
    let anim_file = giphy::download(&anim).await?;
    let still_file = if still != anim { Some(giphy::download(&still).await?) } else { None };
    let state = app.state::<AppState>();
    store_image(&state, &id, anim_file, still_file)?;
    Ok(state.snapshot())
}

/// Image choisie sur le PC (PNG, JPG, GIF ou WebP).
#[tauri::command]
async fn pick_sound_image(app: AppHandle, id: String) -> Result<Snapshot, String> {
    let state = app.state::<AppState>();
    let file = app
        .dialog()
        .file()
        .set_title("Choisir une image")
        .add_filter("Images", &["png", "jpg", "jpeg", "gif", "webp"])
        .blocking_pick_file();
    let Some(path) = file.and_then(|f| f.into_path().ok()) else {
        return Ok(state.snapshot());
    };
    let size = fs::metadata(&path).map_err(|e| e.to_string())?.len();
    if size > 8 * 1024 * 1024 {
        return Err("image trop grosse (8 Mo au plus)".into());
    }
    let data = fs::read(&path).map_err(|e| e.to_string())?;
    let ext = giphy::image_ext(&data).ok_or("ce fichier n'est pas une image")?;
    store_image(&state, &id, (data, ext), None)?;
    Ok(state.snapshot())
}

#[tauri::command]
fn clear_sound_image(state: State<AppState>, id: String) -> Result<Snapshot, String> {
    let old = {
        let mut config = state.config.lock().unwrap();
        let sound = config.sounds.iter_mut().find(|s| s.id == id).ok_or("son introuvable")?;
        let old = sound.clone();
        sound.image = None;
        sound.image_still = None;
        state.save(&config)?;
        old
    };
    remove_image_files(&state, &old);
    Ok(state.snapshot())
}

/// Donne une image GIPHY (la première trouvée avec son nom) aux sons
/// demandés (ou à tous) qui n'en ont pas. Renvoie le nombre de sons illustrés.
#[tauri::command]
async fn auto_images(app: AppHandle, ids: Option<Vec<String>>) -> Result<usize, String> {
    let state = app.state::<AppState>();
    let Some(key) = giphy_key(&state) else {
        return Ok(0);
    };
    let todo: Vec<(String, String)> = state
        .config
        .lock()
        .unwrap()
        .sounds
        .iter()
        .filter(|s| s.image.is_none() && ids.as_ref().is_none_or(|ids| ids.contains(&s.id)))
        .map(|s| (s.id.clone(), s.name.clone()))
        .collect();
    let mut done = 0;
    for (id, name) in todo {
        let results = giphy::search(&name, &key, 1).await?;
        let Some(first) = results.into_iter().next() else {
            continue;
        };
        let Ok(anim) = giphy::download(&first.anim).await else {
            continue;
        };
        let still = if first.still != first.anim { giphy::download(&first.still).await.ok() } else { None };
        if store_image(&state, &id, anim, still).is_ok() {
            done += 1;
            let _ = app.emit("snapshot", state.snapshot());
        }
    }
    Ok(done)
}

#[tauri::command]
fn open_giphy_key_page(app: AppHandle) -> Result<(), String> {
    app.opener()
        .open_url("https://developers.giphy.com/dashboard/?create=true", None::<&str>)
        .map_err(|e| e.to_string())
}

/// Reprend les compteurs de lectures gardés par l'interface avant la 0.12.
#[tauri::command]
fn import_play_counts(state: State<AppState>, counts: HashMap<String, u32>) -> Result<Snapshot, String> {
    {
        let mut config = state.config.lock().unwrap();
        for s in config.sounds.iter_mut() {
            if let Some(&n) = counts.get(&s.id) {
                if s.plays == 0 {
                    s.plays = n;
                    s.played_ms = s.play_duration_ms().unwrap_or(0) * n as u64;
                }
            }
        }
        state.save(&config)?;
    }
    Ok(state.snapshot())
}

#[tauri::command]
fn reset_stats(state: State<AppState>) -> Result<Snapshot, String> {
    {
        let mut config = state.config.lock().unwrap();
        for s in config.sounds.iter_mut() {
            s.plays = 0;
            s.played_ms = 0;
            s.last_played = 0;
        }
        state.save(&config)?;
    }
    Ok(state.snapshot())
}

#[tauri::command]
fn listen_cable(state: State<AppState>, on: bool) -> Result<(), String> {
    state.audio.listen_cable(on)
}

#[tauri::command]
fn hear_myself(state: State<AppState>, on: bool) -> Result<(), String> {
    state.audio.hear_myself(on)
}

fn recordings_dir(app: &AppHandle) -> Result<std::path::PathBuf, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?.join("enregistrements");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

/// Test de voix : 5 s de silence puis 5 s de parole. Enregistre ta voix
/// brute et traitée, mesure ta pièce et ton micro, puis règle le micro
/// (calibrage et égaliseur conseillé).
#[tauri::command]
async fn calibrate_voice(app: AppHandle) -> Result<calibration::VoiceReport, String> {
    let audio = app.state::<AppState>().audio.clone();
    let seconds = calibration::PHASE_SECONDS * 2;
    let recording = tauri::async_runtime::spawn_blocking(move || audio.record_voice(seconds))
        .await
        .map_err(|e| e.to_string())??;
    let dir = recordings_dir(&app)?;
    calibration::write_wav(&dir.join("1-voix-brute.wav"), &recording.raw)?;
    calibration::write_wav(&dir.join("2-voix-traitee.wav"), &recording.processed)?;
    let (cal, mut report) = calibration::analyze(&recording.raw)?;
    report.folder = dir.to_string_lossy().into_owned();

    let state = app.state::<AppState>();
    let settings = {
        let mut config = state.config.lock().unwrap();
        config.settings.mic_calibration = Some(cal);
        config.settings.voice_eq = report.eq;
        state.save(&config)?;
        config.settings.clone()
    };
    apply_audio(&state, settings);
    Ok(report)
}

#[tauri::command]
fn open_recordings(app: AppHandle) -> Result<(), String> {
    let dir = recordings_dir(&app)?;
    app.opener()
        .open_path(dir.to_string_lossy(), None::<&str>)
        .map_err(|e| e.to_string())
}

#[derive(Serialize)]
struct Check {
    /// "ok", "warn", "error" ou "info".
    status: &'static str,
    title: String,
    detail: String,
    /// Bouton proposé : "vbcable", "settings:<onglet>", "wizard" ou "listen".
    action: Option<&'static str>,
}

fn check(status: &'static str, title: impl Into<String>, detail: impl Into<String>, action: Option<&'static str>) -> Check {
    Check { status, title: title.into(), detail: detail.into(), action }
}

/// Est-ce que Discord tourne ? (Windows seulement.)
fn discord_running() -> Option<bool> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let out = std::process::Command::new("tasklist")
            .args(["/FO", "CSV", "/NH"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .ok()?;
        let list = String::from_utf8_lossy(&out.stdout).to_lowercase();
        Some(["\"discord.exe\"", "\"discordptb.exe\"", "\"discordcanary.exe\""].iter().any(|n| list.contains(n)))
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// Vérifie tout ce qui peut empêcher les sons ou la voix d'arriver dans
/// Discord, et dit quoi faire.
#[tauri::command]
async fn run_diagnostics(app: AppHandle) -> Result<Vec<Check>, String> {
    let state = app.state::<AppState>();
    *state.devices.lock().unwrap() = audio::list_devices();
    let devices = state.devices.lock().unwrap().clone();
    let config = state.config.lock().unwrap().clone();
    let settings = &config.settings;
    let warnings = state.warnings.lock().unwrap().clone();
    let warned = |kind: &str| -> Vec<String> {
        warnings.iter().filter_map(|w| w.strip_prefix(kind).map(String::from)).collect()
    };
    let mut checks = Vec::new();

    // Câble virtuel.
    match &devices.vb_cable {
        Some(_) => checks.push(check("ok", "VB-Cable est installé", "Le câble virtuel qui envoie les sons dans Discord est bien là.", None)),
        None => checks.push(check(
            "error",
            "VB-Cable n'est pas installé",
            "Sans lui, les sons ne peuvent pas arriver dans Discord. Installe-le puis redémarre le PC.",
            Some("vbcable"),
        )),
    }
    let audio_errors = warned(WARN_AUDIO);
    match &settings.virtual_device {
        None => checks.push(check(
            "error",
            "Aucune sortie vers Discord",
            "Choisis « CABLE Input » comme sortie vers Discord dans les réglages.",
            Some("settings:sound"),
        )),
        Some(name) if audio_errors.is_empty() => {
            checks.push(check("ok", "Sortie vers Discord", format!("Les sons partent dans « {name} »."), None))
        }
        Some(_) => checks.push(check("error", "Problème de sortie audio", audio_errors.join(" ; "), Some("settings:sound"))),
    }

    // Formats des deux côtés du câble : s'ils diffèrent, Windows convertit
    // en continu et ça peut grésiller.
    let cable_in = settings.virtual_device.as_deref().and_then(|n| audio::output_format(Some(n)));
    let cable_out = audio::cable_capture_format();
    if let (Some(a), Some(b)) = (cable_in, cable_out) {
        if a.rate != b.rate {
            checks.push(check(
                "warn",
                "Les deux côtés du câble n'ont pas la même fréquence",
                format!(
                    "CABLE Input est à {} Hz et CABLE Output à {} Hz : ça peut grésiller. Dans Windows (Paramètres du son → Plus de paramètres de son), règle les deux sur 48000 Hz dans l'onglet Avancé.",
                    a.rate, b.rate
                ),
                None,
            ));
        } else if a.rate != 48_000 {
            checks.push(check(
                "info",
                format!("Le câble tourne à {} Hz", a.rate),
                "Discord travaille en 48000 Hz : régler CABLE Input et CABLE Output sur 48000 Hz (Paramètres du son → Plus de paramètres de son → Avancé) évite une conversion de plus.",
                None,
            ));
        } else {
            checks.push(check("ok", "Format du câble", "48000 Hz des deux côtés, comme Discord.", None));
        }
    }
    match audio::output_format(settings.monitor_device.as_deref()) {
        Some(f) => checks.push(check("ok", "Casque", format!("{} Hz, {} canaux.", f.rate, f.channels), None)),
        None => checks.push(check("error", "Casque introuvable", "Choisis ton casque dans les réglages.", Some("settings:sound"))),
    }

    // Micro.
    if !settings.mic_enabled {
        checks.push(check(
            "info",
            "Ta voix n'est pas envoyée par la soundboard",
            "Si Discord utilise ton vrai micro, il n'entend pas les sons. Pour parler et jouer des sons en même temps, active « Envoyer ma voix » et choisis « CABLE Output » comme micro dans Discord.",
            Some("settings:mic"),
        ));
    } else {
        let mic_errors = warned(WARN_AUDIO).into_iter().filter(|w| w.contains("micro")).collect::<Vec<_>>();
        match audio::input_format(settings.mic_device.as_deref()) {
            Some(_) if mic_errors.is_empty() => checks.push(check("ok", "Micro", "Ta voix part dans le câble avec les sons.", None)),
            Some(_) => checks.push(check("error", "Problème de micro", mic_errors.join(" ; "), Some("settings:mic"))),
            None => checks.push(check("error", "Micro introuvable", "Branche ton micro ou choisis-en un autre dans les réglages.", Some("settings:mic"))),
        }
        let under = audio::CAPTURE_UNDERRUNS.load(Ordering::Relaxed);
        let over = audio::CAPTURE_OVERFLOWS.load(Ordering::Relaxed);
        if under + over > 20 {
            checks.push(check(
                "warn",
                "Ta voix a eu des micro-coupures",
                format!(
                    "{} coupures depuis le lancement : le micro et le câble ne vont pas tout à fait à la même vitesse. Mets le micro et le câble sur la même fréquence (48000 Hz) dans Windows.",
                    under + over
                ),
                None,
            ));
        }
        if settings.strong_denoise && settings.noise_suppression && std::thread::available_parallelism().map_or(4, |n| n.get()) < 4 {
            checks.push(check(
                "info",
                "Processeur modeste",
                "Si ta voix hache, passe la réduction de bruit sur « Légère ».",
                Some("settings:mic"),
            ));
        }
    }

    // Discord.
    match discord_running() {
        Some(true) => checks.push(check("ok", "Discord est lancé", "", None)),
        Some(false) => checks.push(check("warn", "Discord n'est pas lancé", "Lance Discord pour tester.", None)),
        None => {}
    }
    checks.push(check(
        "info",
        "Dans Discord (Paramètres → Voix et vidéo)",
        "Périphérique d'entrée : « CABLE Output ». Suppression du bruit : aucune (la soundboard s'en charge, Krisp couperait les sons). Annulation d'écho et contrôle automatique du gain : désactivés. Mode de saisie : détection de la voix, sensibilité réglée à la main assez bas.",
        None,
    ));

    // Volume.
    if !settings.normalize {
        checks.push(check(
            "warn",
            "Égalisation désactivée",
            "Certains sons risquent d'être beaucoup plus forts que d'autres.",
            Some("settings:sound"),
        ));
    }
    if settings.master_volume > 1.0 {
        checks.push(check(
            "warn",
            "Volume vers Discord au-dessus de 100 %",
            "Les sons risquent de couvrir les voix ; garde-le à 100 % ou moins.",
            Some("settings:sound"),
        ));
    }

    // Raccourcis.
    let hotkey_errors = warned(WARN_HOTKEY);
    if hotkey_errors.is_empty() {
        checks.push(check("ok", "Raccourcis", "Tous les raccourcis sont actifs.", None));
    } else {
        checks.push(check("warn", "Raccourcis indisponibles", hotkey_errors.join(" ; "), None));
    }

    // Fichiers des sons.
    let missing: Vec<&str> = config.sounds.iter().filter(|s| !s.path.exists()).map(|s| s.name.as_str()).collect();
    if missing.is_empty() {
        checks.push(check("ok", "Fichiers des sons", format!("{} sons, tous présents.", config.sounds.len()), None));
    } else {
        checks.push(check(
            "error",
            format!("{} son(s) introuvable(s)", missing.len()),
            format!("Fichier supprimé ou déplacé : {}. Supprime-les et ajoute-les à nouveau.", missing.join(", ")),
            None,
        ));
    }

    if settings.phone_enabled {
        let phone_errors = warned(WARN_PHONE);
        if phone_errors.is_empty() {
            checks.push(check("ok", "Page du téléphone", "Le serveur du téléphone tourne.", None));
        } else {
            checks.push(check("warn", "Page du téléphone", phone_errors.join(" ; "), Some("settings:remote")));
        }
    }

    checks.push(check(
        "info",
        "Tester comme tes potes l'entendent",
        "« Écouter le câble » te fait entendre exactement ce qui part dans Discord (sons et voix), le temps de jouer un son.",
        Some("listen"),
    ));
    Ok(checks)
}

#[tauri::command]
fn move_sound(state: State<AppState>, id: String, to: usize) -> Result<Snapshot, String> {
    {
        let mut config = state.config.lock().unwrap();
        if let Some(from) = config.sounds.iter().position(|s| s.id == id) {
            let sound = config.sounds.remove(from);
            let to = to.min(config.sounds.len());
            config.sounds.insert(to, sound);
        }
        state.save(&config)?;
    }
    Ok(state.snapshot())
}

#[tauri::command]
fn update_settings(app: AppHandle, state: State<AppState>, mut settings: Settings) -> Result<Snapshot, String> {
    let phone_changed;
    let audio_changed;
    let room_changed;
    {
        let mut config = state.config.lock().unwrap();
        // Le code du téléphone ne se change que par « Nouveau code ».
        settings.phone_token = config.settings.phone_token.clone();
        // Tenus à jour par l'app elle-même (abonnements, partages, salon).
        settings.subscriptions = config.settings.subscriptions.clone();
        settings.published = config.settings.published.clone();
        settings.room_id = config.settings.room_id.clone();
        settings.duck_level = settings.duck_level.clamp(0.1, 0.8);
        room_changed = (&config.settings.room_code, &config.settings.room_name)
            != (&settings.room_code, &settings.room_name);
        if settings.phone_enabled && settings.phone_token.is_empty() {
            settings.phone_token = phone::new_token();
        }
        settings.phone_port = settings.phone_port.max(1024);
        let old = &config.settings;
        phone_changed = (old.phone_enabled, old.phone_port, &old.phone_token)
            != (settings.phone_enabled, settings.phone_port, &settings.phone_token);
        audio_changed = audio_settings(old) != audio_settings(&settings);
        config.settings = settings.clone();
        state.save(&config)?;
    }
    if phone_changed {
        apply_phone(&app);
    }
    replay::configure(settings.replay_enabled, settings.duck_enabled, settings.duck_level);
    if room_changed {
        extras::start_room(&app);
    }
    let autostart = settings.launch_at_startup;
    // Relancer l'audio coupe les sons et le micro une seconde : seulement
    // si un réglage du son a changé (pas pour un raccourci ou une clé).
    // (ou pour réessayer quand un périphérique manquait).
    let audio_failed = state.warnings.lock().unwrap().iter().any(|w| w.starts_with(WARN_AUDIO));
    if audio_changed || audio_failed {
        apply_audio(&state, settings);
    }
    apply_autostart(&app, autostart);
    register_hotkeys(&app);
    Ok(state.snapshot())
}

/// Ce que le moteur audio lit dans les réglages.
fn audio_settings(s: &Settings) -> serde_json::Value {
    serde_json::json!([
        s.monitor_device, s.virtual_device, s.mic_device, s.mic_enabled, s.mic_volume,
        s.master_volume, s.monitor_volume, s.stop_previous, s.noise_suppression,
        s.strong_denoise, s.voice_gate, s.gate_auto, s.gate_threshold, s.voice_effect,
        s.voice_leveler, s.voice_eq, s.mic_calibration,
    ])
}

/// Joue le son, ou l'arrête s'il est déjà en cours.
/// Hors du fil principal : calculer les effets d'un long son prend du temps
/// et gelait toute la fenêtre pendant ce temps.
#[tauri::command]
async fn play_sound(app: AppHandle, id: String) -> Result<PlayOutcome, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let sound = find_sound(&state, &id)?;
        if state.extras.room.target().is_some() {
            return extras::send_to_friend(&app, sound);
        }
        note_play(&state, &sound);
        state
            .audio
            .toggle(play_request(&state, &sound))
            .inspect_err(|e| journal::write(&format!("Lecture de « {} » : {e}", sound.name)))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Taille de l'interface (Réglages > Général, ou Ctrl + / Ctrl −).
#[tauri::command]
fn set_zoom(window: tauri::WebviewWindow, scale: f64) -> Result<(), String> {
    window.set_zoom(scale.clamp(0.5, 2.0)).map_err(|e| e.to_string())
}

/// Erreurs de l'interface (JavaScript), notées dans le journal.
#[tauri::command]
fn log_ui(message: String) {
    journal::write(&format!("Interface : {}", message.chars().take(500).collect::<String>()));
}

/// Rapport à copier-coller quand quelque chose ne marche pas.
#[tauri::command]
fn diagnostic_report(state: State<AppState>) -> String {
    let config = state.config.lock().unwrap().clone();
    let devices = state.devices.lock().unwrap().clone();
    let warnings = state.warnings.lock().unwrap().clone();
    let s = &config.settings;
    let missing = config.sounds.iter().filter(|x| !x.path.exists()).count();
    let opt = |o: &Option<String>| o.clone().unwrap_or_else(|| "par défaut".into());
    format!(
        "Soundboard v{}\n\
         Sons : {} (fichiers manquants : {missing})\n\
         Casque : {} | Câble : {} | Micro : {} (voix envoyée : {})\n\
         Par défaut : sortie {} / entrée {}\n\
         VB-Cable : {}\n\
         Débruitage : {} | Porte : {} | Effet : {:?}\n\
         Problèmes affichés :\n{}\n\n\
         Journal :\n{}",
        env!("CARGO_PKG_VERSION"),
        config.sounds.len(),
        opt(&s.monitor_device),
        opt(&s.virtual_device),
        opt(&s.mic_device),
        if s.mic_enabled { "oui" } else { "non" },
        opt(&devices.default_output),
        opt(&devices.default_input),
        devices.vb_cable.as_deref().unwrap_or("absent"),
        if !s.noise_suppression { "non" } else if s.strong_denoise { "fort" } else { "léger" },
        if s.voice_gate { "oui" } else { "non" },
        s.voice_effect,
        if warnings.is_empty() { "  aucun".to_string() } else { warnings.iter().map(|w| format!("  - {w}")).collect::<Vec<_>>().join("\n") },
        journal::tail(150),
    )
}

/// Pré-écoute dans le casque seulement, avec une découpe pas encore enregistrée.
#[tauri::command]
async fn preview_sound(
    app: AppHandle,
    id: String,
    start_ms: u64,
    end_ms: Option<u64>,
    volume: f32,
    fx: Option<sound_fx::SoundFx>,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let mut sound = find_sound(&state, &id)?;
        sound.start_ms = start_ms;
        sound.end_ms = end_ms;
        sound.volume = volume;
        // Pré-écoute jouée une seule fois : pas de boucle dans la durée.
        sound.looping = false;
        if let Some(fx) = fx {
            sound.fx = fx.sanitized();
        }
        let mut req = play_request(&state, &sound);
        req.id = audio::PREVIEW_ID.into();
        // Une seule lecture pour suivre le curseur, même pour un son en boucle.
        req.looping = false;
        state.audio.preview(req)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
fn stop_preview(state: State<AppState>) {
    state.audio.stop_preview();
}

#[tauri::command]
async fn get_waveform(app: AppHandle, id: String) -> Result<Vec<f32>, String> {
    let state = app.state::<AppState>();
    if let Some(peaks) = state.waveforms.lock().unwrap().get(&id) {
        return Ok(peaks.clone());
    }
    let sound = find_sound(&state, &id)?;
    let peaks = tauri::async_runtime::spawn_blocking(move || analysis::waveform(&sound.path, 2400))
        .await
        .map_err(|e| e.to_string())??;
    state.waveforms.lock().unwrap().insert(id, peaks.clone());
    Ok(peaks)
}

#[tauri::command]
async fn search_online(
    app: AppHandle,
    source: Option<String>,
    query: String,
    page: Option<u32>,
) -> Result<Vec<OnlineSound>, String> {
    let page = page.unwrap_or(1);
    match source.as_deref() {
        Some("freesound") => {
            let key = app
                .state::<AppState>()
                .config
                .lock()
                .unwrap()
                .settings
                .freesound_key
                .clone()
                .filter(|k| !k.trim().is_empty())
                .ok_or("Ajoute ta clé Freesound dans les réglages (⚙) pour chercher ici.")?;
            online::search_freesound(query.trim(), page, key.trim()).await
        }
        _ => online::search(query.trim(), page).await,
    }
}

#[tauri::command]
async fn add_online(app: AppHandle, name: String, url: String) -> Result<Snapshot, String> {
    let data = online::download(&url).await?;
    let state = app.state::<AppState>();
    let index = state.config.lock().unwrap().sounds.len();
    let sounds_dir = state.sounds_dir.clone();
    let sound = tauri::async_runtime::spawn_blocking(move || {
        store_sound(&data, "mp3", name, &sounds_dir, index)
    })
    .await
    .map_err(|e| e.to_string())??;
    {
        let mut config = state.config.lock().unwrap();
        config.sounds.push(sound);
        state.save(&config)?;
    }
    Ok(state.snapshot())
}

#[tauri::command]
fn toggle_favorite(state: State<AppState>, id: String) -> Result<Snapshot, String> {
    {
        let mut config = state.config.lock().unwrap();
        let sound = config.sounds.iter_mut().find(|s| s.id == id).ok_or("son introuvable")?;
        sound.favorite = !sound.favorite;
        state.save(&config)?;
    }
    Ok(state.snapshot())
}

/// Range plusieurs sons d'un coup dans un onglet (aucun : « Sans onglet »).
#[tauri::command]
fn set_category(state: State<AppState>, ids: Vec<String>, category: Option<String>) -> Result<Snapshot, String> {
    {
        let mut config = state.config.lock().unwrap();
        let category = clean_category(category);
        for sound in config.sounds.iter_mut().filter(|s| ids.contains(&s.id)) {
            sound.category = category.clone();
        }
        state.save(&config)?;
    }
    Ok(state.snapshot())
}

/// Ajoute un son à partir d'un lien direct vers un fichier audio.
#[tauri::command]
async fn add_from_url(app: AppHandle, url: String) -> Result<Snapshot, String> {
    let (data, ext, name) = online::download_link(url.trim()).await?;
    let state = app.state::<AppState>();
    let index = state.config.lock().unwrap().sounds.len();
    let sounds_dir = state.sounds_dir.clone();
    let sound = tauri::async_runtime::spawn_blocking(move || store_sound(&data, &ext, name, &sounds_dir, index))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|_| "ce lien ne mène pas à un fichier audio (il faut un lien direct vers un .mp3, .wav…)".to_string())?;
    {
        let mut config = state.config.lock().unwrap();
        config.sounds.push(sound);
        state.save(&config)?;
    }
    Ok(state.snapshot())
}

#[derive(Serialize)]
struct AddedSound {
    id: String,
    snapshot: Snapshot,
}

/// Ajoute l'audio d'une vidéo YouTube (yt-dlp est installé au premier usage).
#[tauri::command]
async fn add_from_youtube(app: AppHandle, url: String) -> Result<AddedSound, String> {
    let state = app.state::<AppState>();
    if state.youtube_busy.swap(true, Ordering::SeqCst) {
        return Err("un téléchargement YouTube est déjà en cours".into());
    }
    let res = youtube_import(&app, url.trim().to_string()).await;
    state.youtube_busy.store(false, Ordering::SeqCst);
    res
}

async fn youtube_import(app: &AppHandle, url: String) -> Result<AddedSound, String> {
    if !youtube::is_youtube(&url) {
        return Err("ce n'est pas un lien YouTube".into());
    }
    let state = app.state::<AppState>();
    let data_dir = state.sounds_dir.parent().map(Path::to_path_buf).unwrap_or_else(|| state.sounds_dir.clone());
    let tools_dir = data_dir.join("tools");
    let status = |msg: &str| {
        let _ = app.emit("youtube-status", msg);
    };
    let exe = youtube::ensure(&tools_dir, status).await?;
    status("Récupération du son sur YouTube…");
    let work_dir = data_dir.join("youtube-tmp");
    let (data, title) = tauri::async_runtime::spawn_blocking(move || {
        youtube::update_if_stale(&exe);
        youtube::download_audio(&exe, &url, &work_dir)
    })
    .await
    .map_err(|e| e.to_string())??;
    let index = state.config.lock().unwrap().sounds.len();
    let sounds_dir = state.sounds_dir.clone();
    let sound = tauri::async_runtime::spawn_blocking(move || store_sound(&data, "m4a", title, &sounds_dir, index))
        .await
        .map_err(|e| e.to_string())??;
    let id = sound.id.clone();
    {
        let mut config = state.config.lock().unwrap();
        config.sounds.push(sound);
        state.save(&config)?;
    }
    Ok(AddedSound {
        id,
        snapshot: state.snapshot(),
    })
}

#[derive(Serialize)]
struct PhoneInfo {
    url: String,
    qr_svg: String,
}

#[tauri::command]
fn get_phone_info(state: State<AppState>) -> Result<Option<PhoneInfo>, String> {
    let settings = state.config.lock().unwrap().settings.clone();
    if !settings.phone_enabled || settings.phone_token.is_empty() {
        return Ok(None);
    }
    let url = phone::page_url(settings.phone_port, &settings.phone_token)
        .ok_or("adresse du PC sur le réseau introuvable (es-tu connecté en Wi-Fi ou en câble ?)")?;
    let qr_svg = phone::qr_svg(&url)?;
    Ok(Some(PhoneInfo { url, qr_svg }))
}

/// Change le code secret : les anciens liens du téléphone ne marchent plus.
#[tauri::command]
fn regenerate_phone_token(app: AppHandle, state: State<AppState>) -> Result<Snapshot, String> {
    {
        let mut config = state.config.lock().unwrap();
        config.settings.phone_token = phone::new_token();
        state.save(&config)?;
    }
    apply_phone(&app);
    Ok(state.snapshot())
}

#[tauri::command]
fn stop_all(app: AppHandle, state: State<AppState>) {
    state.audio.stop_all();
    clip::close(&app);
}

#[tauri::command]
fn open_freesound_key_page(app: AppHandle) -> Result<(), String> {
    app.opener()
        .open_url(FREESOUND_KEY_URL, None::<&str>)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn set_mic_muted(app: AppHandle, muted: bool) {
    set_muted(&app, muted);
}

/// Joue un son au hasard de l'onglet affiché et renvoie son id.
#[tauri::command]
fn play_random(state: State<AppState>, category: Option<String>) -> Result<Option<String>, String> {
    let Some(sound) = pick_random(&state, category.as_deref()) else {
        return Ok(None);
    };
    note_play(&state, &sound);
    state.audio.toggle(play_request(&state, &sound))?;
    Ok(Some(sound.id))
}

/// Allume ou éteint le mode Surprise.
#[tauri::command]
fn set_surprise(app: AppHandle, on: bool) -> Snapshot {
    let state = app.state::<AppState>();
    state.surprise_on.store(on, Ordering::SeqCst);
    start_surprise(&app);
    let _ = app.emit("surprise", on);
    state.snapshot()
}

#[tauri::command]
fn set_meter(state: State<AppState>, on: bool) {
    state.audio.set_meter(on);
}

#[tauri::command]
async fn check_update() -> Result<Option<UpdateInfo>, String> {
    online::check_update().await
}

/// Télécharge la nouvelle version, l'installe par-dessus et relance l'app.
/// La mise à jour est vérifiée avec la clé publique de `tauri.conf.json` :
/// seule une version signée par le projet est acceptée.
#[tauri::command]
async fn install_update(app: AppHandle) -> Result<(), String> {
    let update = app
        .updater()
        .map_err(|e| e.to_string())?
        .check()
        .await
        .map_err(|e| format!("vérification impossible : {e}"))?
        .ok_or("cette version ne peut pas s'installer toute seule, télécharge-la depuis la page")?;
    let state = app.state::<AppState>();
    state.audio.stop_all();
    update
        .download_and_install(|_, _| {}, || {})
        .await
        .map_err(|e| format!("mise à jour impossible : {e}"))?;
    app.restart();
}

#[tauri::command]
fn open_release_page(app: AppHandle, url: String) -> Result<(), String> {
    // On n'ouvre que les pages de versions du projet.
    let url = if url.starts_with(online::RELEASES_PAGE) {
        url
    } else {
        online::RELEASES_PAGE.to_string()
    };
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(|e| e.to_string())
}

/// Description d'un son dans un pack (`pack.json`).
#[derive(Serialize, Deserialize)]
struct PackSound {
    name: String,
    file: String,
    #[serde(default)]
    hotkey: Option<String>,
    #[serde(default = "one")]
    volume: f32,
    #[serde(default)]
    color: Option<String>,
    #[serde(default)]
    start_ms: u64,
    #[serde(default)]
    end_ms: Option<u64>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    looping: bool,
    #[serde(default)]
    emoji: Option<String>,
    #[serde(default)]
    favorite: bool,
    #[serde(default)]
    keep_playing: bool,
    #[serde(default)]
    fx: sound_fx::SoundFx,
}

fn one() -> f32 {
    1.0
}

#[derive(Serialize, Deserialize)]
struct Pack {
    sounds: Vec<PackSound>,
}

const PACK_MAX_FILE: u64 = 20 * 1024 * 1024;
const PACK_MAX_TOTAL: u64 = 500 * 1024 * 1024;

fn write_pack(dest: &Path, sounds: &[Sound]) -> Result<usize, String> {
    let file = fs::File::create(dest).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored);
    let mut pack = Pack { sounds: vec![] };
    for (i, s) in sounds.iter().enumerate() {
        let Ok(data) = fs::read(&s.path) else { continue };
        let ext = s.path.extension().and_then(|e| e.to_str()).unwrap_or("mp3");
        let name = format!("sounds/{i}.{ext}");
        zip.start_file(name.as_str(), options).map_err(|e| e.to_string())?;
        zip.write_all(&data).map_err(|e| e.to_string())?;
        pack.sounds.push(PackSound {
            name: s.name.clone(),
            file: name,
            hotkey: s.hotkey.clone(),
            volume: s.volume,
            color: Some(s.color.clone()),
            start_ms: s.start_ms,
            end_ms: s.end_ms,
            category: s.category.clone(),
            looping: s.looping,
            emoji: s.emoji.clone(),
            favorite: s.favorite,
            keep_playing: s.keep_playing,
            fx: s.fx,
        });
    }
    zip.start_file("pack.json", options).map_err(|e| e.to_string())?;
    let json = serde_json::to_vec_pretty(&pack).map_err(|e| e.to_string())?;
    zip.write_all(&json).map_err(|e| e.to_string())?;
    zip.finish().map_err(|e| e.to_string())?;
    Ok(pack.sounds.len())
}

/// Lit un pack et range ses sons. Les raccourcis déjà pris sont ignorés.
/// Sons ajoutés, erreurs, empreintes de tous les sons du pack.
type PackRead = (Vec<Sound>, Vec<String>, Vec<String>);

/// `skip` : sons déjà présents (empreintes), pas réimportés. Renvoie aussi
/// les empreintes de tous les sons du pack.
fn read_pack(
    src: &Path,
    config: &Config,
    sounds_dir: &Path,
    skip: &std::collections::HashSet<String>,
) -> Result<PackRead, String> {
    let file = fs::File::open(src).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipArchive::new(file).map_err(|_| "ce fichier n'est pas un pack de sons".to_string())?;
    let pack: Pack = {
        let mut entry = zip
            .by_name("pack.json")
            .map_err(|_| "pack.json absent : ce n'est pas un pack de sons".to_string())?;
        let mut json = String::new();
        entry
            .by_ref()
            .take(1024 * 1024)
            .read_to_string(&mut json)
            .map_err(|e| e.to_string())?;
        serde_json::from_str(&json).map_err(|e| format!("pack.json illisible : {e}"))?
    };

    let mut used: Vec<String> = config.sounds.iter().filter_map(|s| s.hotkey.clone()).collect();
    used.extend(
        [&config.settings.stop_hotkey, &config.settings.mute_hotkey, &config.settings.random_hotkey]
            .into_iter()
            .flatten()
            .cloned(),
    );
    let mut added = Vec::new();
    let mut errors = Vec::new();
    let mut keys = Vec::new();
    let mut total = 0u64;
    for (i, p) in pack.sounds.into_iter().enumerate() {
        let ext = Path::new(&p.file)
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase())
            .unwrap_or_default();
        if !AUDIO_EXTENSIONS.contains(&ext.as_str()) {
            errors.push(format!("{} : format non pris en charge", p.name));
            continue;
        }
        let data = match zip.by_name(&p.file) {
            Ok(entry) => {
                let mut data = Vec::new();
                let read = entry.take(PACK_MAX_FILE + 1).read_to_end(&mut data);
                if read.is_err() || data.len() as u64 > PACK_MAX_FILE {
                    errors.push(format!("{} : fichier illisible ou trop gros", p.name));
                    continue;
                }
                data
            }
            Err(_) => {
                errors.push(format!("{} : fichier absent du pack", p.name));
                continue;
            }
        };
        let key = share::key_of(&data);
        keys.push(key.clone());
        if skip.contains(&key) {
            continue;
        }
        total += data.len() as u64;
        if total > PACK_MAX_TOTAL {
            errors.push("pack trop gros : la suite n'a pas été importée".into());
            break;
        }
        let name: String = p.name.chars().take(80).collect();
        match store_sound(&data, &ext, name, sounds_dir, config.sounds.len() + i) {
            Ok(mut sound) => {
                sound.hotkey = p.hotkey.filter(|h| !h.is_empty() && !used.contains(h));
                if let Some(h) = &sound.hotkey {
                    used.push(h.clone());
                }
                sound.volume = p.volume.clamp(0.0, 2.0);
                if let Some(c) = p.color.filter(|c| c.starts_with('#') && c.len() <= 9) {
                    sound.color = c;
                }
                let total_ms = sound.analysis.map(|a| a.duration_ms).unwrap_or(u64::MAX);
                sound.start_ms = p.start_ms.min(total_ms);
                sound.end_ms = p.end_ms.filter(|&e| e < total_ms && e > sound.start_ms);
                sound.category = clean_category(p.category);
                sound.looping = p.looping;
                sound.emoji = clean_emoji(p.emoji);
                sound.favorite = p.favorite;
                sound.keep_playing = p.keep_playing;
                sound.fx = p.fx.sanitized();
                added.push(sound);
            }
            Err(e) => errors.push(format!("{} : {e}", p.name)),
        }
    }
    Ok((added, errors, keys))
}

/// Enregistre les sons (d'un onglet ou tous) dans un .zip à partager.
#[tauri::command]
async fn export_pack(app: AppHandle, category: Option<String>) -> Result<Option<usize>, String> {
    let state = app.state::<AppState>();
    let sounds: Vec<Sound> = state
        .config
        .lock()
        .unwrap()
        .sounds
        .iter()
        .filter(|s| category.is_none() || s.category == category)
        .cloned()
        .collect();
    if sounds.is_empty() {
        return Err("aucun son à exporter".into());
    }
    let file_name = format!(
        "{}.zip",
        category.as_deref().unwrap_or("Soundboard").replace(['/', '\\', ':', '*', '?', '"', '<', '>', '|'], "_")
    );
    let Some(dest) = app
        .dialog()
        .file()
        .set_title("Exporter un pack de sons")
        .set_file_name(file_name)
        .add_filter("Pack de sons", &["zip"])
        .blocking_save_file()
        .and_then(|f| f.into_path().ok())
    else {
        return Ok(None);
    };
    tauri::async_runtime::spawn_blocking(move || write_pack(&dest, &sounds))
        .await
        .map_err(|e| e.to_string())?
        .map(Some)
}

#[derive(Serialize)]
struct ImportResult {
    added: usize,
    errors: Vec<String>,
    snapshot: Snapshot,
}

#[tauri::command]
async fn import_pack(app: AppHandle, path: Option<String>) -> Result<Option<ImportResult>, String> {
    // `path` : fichier glissé sur la fenêtre ; sinon on demande lequel.
    let Some(src) = path.map(PathBuf::from).or_else(|| app
        .dialog()
        .file()
        .set_title("Importer un pack de sons")
        .add_filter("Pack de sons", &["zip"])
        .blocking_pick_file()
        .and_then(|f| f.into_path().ok()))
    else {
        return Ok(None);
    };
    let state = app.state::<AppState>();
    let config = state.config.lock().unwrap().clone();
    let sounds_dir = state.sounds_dir.clone();
    let (added, errors, _) =
        tauri::async_runtime::spawn_blocking(move || read_pack(&src, &config, &sounds_dir, &Default::default()))
        .await
        .map_err(|e| e.to_string())??;
    let count = added.len();
    {
        let mut config = state.config.lock().unwrap();
        config.sounds.extend(added);
        state.save(&config)?;
    }
    register_hotkeys(&app);
    Ok(Some(ImportResult {
        added: count,
        errors,
        snapshot: state.snapshot(),
    }))
}

/// Sauvegarde complète (sons, images, réglages) dans un seul fichier.
#[tauri::command]
async fn export_backup(app: AppHandle) -> Result<Option<usize>, String> {
    let Some(dest) = app
        .dialog()
        .file()
        .set_title("Sauvegarder toute ma soundboard")
        .set_file_name("Soundboard-sauvegarde.zip")
        .add_filter("Sauvegarde", &["zip"])
        .blocking_save_file()
        .and_then(|f| f.into_path().ok())
    else {
        return Ok(None);
    };
    let config = app.state::<AppState>().config.lock().unwrap().clone();
    tauri::async_runtime::spawn_blocking(move || backup::write(&dest, &config))
        .await
        .map_err(|e| e.to_string())?
        .map(Some)
}

/// Remplace la soundboard actuelle par une sauvegarde.
#[tauri::command]
async fn import_backup(app: AppHandle, path: Option<String>) -> Result<Option<Snapshot>, String> {
    let Some(src) = path.map(PathBuf::from).or_else(|| app
        .dialog()
        .file()
        .set_title("Restaurer une sauvegarde")
        .add_filter("Sauvegarde", &["zip"])
        .blocking_pick_file()
        .and_then(|f| f.into_path().ok()))
    else {
        return Ok(None);
    };
    let state = app.state::<AppState>();
    let (sounds_dir, images) = (state.sounds_dir.clone(), images_dir(&state));
    // Avant d'écrire : sous Windows, un son en cours de lecture est verrouillé
    // et ne pourrait pas être remplacé (il disparaîtrait de la restauration).
    state.audio.stop_all();
    let restored = tauri::async_runtime::spawn_blocking(move || backup::read(&src, &sounds_dir, &images))
        .await
        .map_err(|e| e.to_string())??;
    empty_trash(&state);
    let settings = restored.settings.clone();
    {
        let mut config = state.config.lock().unwrap();
        *config = restored;
        state.save(&config)?;
    }
    state.waveforms.lock().unwrap().clear();
    journal::write("Sauvegarde restaurée");
    apply_audio(&state, settings);
    register_hotkeys(&app);
    apply_phone(&app);
    measure_segments(app.clone());
    Ok(Some(state.snapshot()))
}

/// Nature d'un .zip glissé sur la fenêtre : « pack », « backup » ou rien.
#[tauri::command]
fn zip_kind(path: String) -> Option<&'static str> {
    let file = fs::File::open(path).ok()?;
    let zip = zip::ZipArchive::new(file).ok()?;
    let has = |name: &str| zip.file_names().any(|n| n == name);
    if has(backup::CONFIG_ENTRY) {
        Some("backup")
    } else if has("pack.json") {
        Some("pack")
    } else {
        None
    }
}

#[tauri::command]
fn open_vb_cable_page(app: AppHandle) -> Result<(), String> {
    app.opener()
        .open_url(VB_CABLE_URL, None::<&str>)
        .map_err(|e| e.to_string())
}

fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "Ouvrir la soundboard", true, None::<&str>)?;
    let stop = MenuItem::with_id(app, "stop", "Tout arrêter", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quitter", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &stop, &quit])?;
    let mut tray = TrayIconBuilder::with_id("main")
        .tooltip("Soundboard")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => show_window(app),
            "stop" => {
                app.state::<AppState>().audio.stop_all();
                clip::close(app);
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_window(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            // Une seule instance, sinon les raccourcis globaux se marchent dessus.
            show_window(app);
        }))
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec![MINIMIZED_ARG]),
        ))
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, shortcut, event| {
                    if event.state() == ShortcutState::Pressed {
                        on_hotkey(app, shortcut);
                    }
                })
                .build(),
        )
        .setup(|app| {
            journal::init(app.path().app_data_dir()?);
            let config_path = app.path().app_config_dir()?.join("config.json");
            let sounds_dir = app.path().app_data_dir()?.join("sounds");
            let (mut config, config_note) = Config::load(&config_path);
            let devices = audio::list_devices();

            // Premier lancement : si VB-Cable est déjà installé, on le choisit d'office.
            if config.settings.virtual_device.is_none() {
                config.settings.virtual_device = devices.vb_cable.clone();
            }

            let handle = app.handle().clone();
            let audio = AudioHandle::spawn(move |event| match event {
                AudioEvent::Playing(status) => {
                    if let Some(state) = handle.try_state::<AppState>() {
                        *state.playing_ids.lock().unwrap() = status
                            .iter()
                            .filter(|s| s.id != audio::PREVIEW_ID)
                            .map(|s| s.id.clone())
                            .collect();
                    }
                    let _ = handle.emit("playing", status);
                }
                AudioEvent::Mic(level) => {
                    let _ = handle.emit("mic-level", level);
                }
            });

            let state = AppState {
                config: Mutex::new(config.clone()),
                config_path,
                sounds_dir,
                audio,
                hotkeys: Mutex::new(HashMap::new()),
                warnings: Mutex::new(Vec::new()),
                mic_muted: AtomicBool::new(false),
                youtube_busy: AtomicBool::new(false),
                phone: Mutex::new(None),
                devices: Mutex::new(devices),
                waveforms: Mutex::new(HashMap::new()),
                playing_ids: Mutex::new(Vec::new()),
                last_random: Mutex::new(None),
                trash: Mutex::new(None),
                surprise_on: AtomicBool::new(false),
                surprise_gen: AtomicU64::new(0),
                extras: Default::default(),
                clips: Default::default(),
            };
            empty_trash(&state);
            if let Some(note) = config_note {
                state.set_warnings(WARN_CONFIG, vec![note]);
            }
            app.manage(state);
            register_hotkeys(app.handle());
            // Ouvrir le casque, le câble et le micro (et charger le
            // débruiteur) prend du temps : la fenêtre s'affiche sans attendre.
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                let state = handle.state::<AppState>();
                // Réglages du moment : ils ont pu changer entre-temps.
                let settings = state.config.lock().unwrap().settings.clone();
                apply_autostart(&handle, settings.launch_at_startup);
                apply_audio(&state, settings);
                let _ = handle.emit("snapshot", state.snapshot());
                watch_devices(handle.clone());
                // Les mesures de volume en retard passent après l'audio, une
                // à la fois, pour ne pas ralentir l'ouverture.
                extras::start(&handle);
                analyze_missing(&handle);
                measure_segments(handle.clone());
            });
            build_tray(app.handle())?;
            apply_phone(app.handle());

            // Lancé avec Windows : on reste discret près de l'horloge.
            if !std::env::args().any(|a| a == MINIMIZED_ARG) {
                show_window(app.handle());
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                // La fenêtre des extraits se ferme simplement (sans quitter l'app).
                if clip::is_clip_window(window.label()) {
                    return;
                }
                let state = window.state::<AppState>();
                let to_tray = state.config.lock().unwrap().settings.close_to_tray;
                if to_tray {
                    api.prevent_close();
                    let _ = window.hide();
                } else {
                    window.app_handle().exit(0);
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_state,
            refresh_devices,
            add_sounds,
            pick_sounds,
            update_sound,
            remove_sound,
            restore_sound,
            export_backup,
            import_backup,
            zip_kind,
            replace_sound_file,
            log_ui,
            set_zoom,
            diagnostic_report,
            move_sound,
            update_settings,
            play_sound,
            preview_sound,
            stop_preview,
            get_waveform,
            search_online,
            add_online,
            stop_all,
            open_vb_cable_page,
            open_freesound_key_page,
            open_giphy_key_page,
            search_images,
            set_sound_image,
            pick_sound_image,
            clear_sound_image,
            auto_images,
            import_play_counts,
            reset_stats,
            listen_cable,
            hear_myself,
            calibrate_voice,
            open_recordings,
            run_diagnostics,
            get_phone_info,
            regenerate_phone_token,
            set_mic_muted,
            toggle_favorite,
            set_category,
            add_from_url,
            add_from_youtube,
            play_random,
            set_surprise,
            set_meter,
            check_update,
            install_update,
            open_release_page,
            export_pack,
            import_pack,
            extras::save_replay,
            extras::mic_record_start,
            extras::mic_record_stop,
            extras::tts_voices,
            extras::tts_say,
            extras::tts_create,
            extras::subscribe_pack,
            extras::unsubscribe_pack,
            extras::sync_packs,
            extras::cloud_folders,
            extras::publish_tab,
            extras::unpublish_tab,
            extras::reveal_published,
            extras::room_status,
            extras::set_room_target,
            extras::new_room_code,
            clip::pick_clip_media,
            clip::send_clip,
            clip::clip_pending,
            clip::clip_accept,
            clip::clip_close,
        ])
        .run(tauri::generate_context!())
        .expect("erreur au lancement de l'application");
}

#[cfg(test)]
mod tests {
    use super::surprise_delay;

    #[test]
    fn surprise_delay_stays_between_min_and_max() {
        for _ in 0..200 {
            let d = surprise_delay(10, 60).as_secs();
            assert!((600..=3600).contains(&d));
        }
        // Minimum au-dessus du maximum, ou zéro : on reste raisonnable.
        assert_eq!(surprise_delay(30, 5).as_secs(), 1800);
        assert_eq!(surprise_delay(0, 0).as_secs(), 60);
    }
}
