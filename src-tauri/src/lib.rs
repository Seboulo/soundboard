mod analysis;
mod audio;
mod config;
mod decoder;
mod denoise;
mod effects;
mod online;
mod phone;
mod youtube;

use audio::{AudioEvent, AudioHandle, DeviceList, PlayOutcome, PlayRequest};
use config::{Config, Settings, Sound};
use online::{OnlineSound, UpdateInfo};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder, WindowEvent};
use tauri_plugin_autostart::ManagerExt as AutostartExt;
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};
use tauri_plugin_opener::OpenerExt;

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
    ToggleOverlay,
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
}

#[derive(Serialize)]
struct Snapshot {
    config: Config,
    devices: DeviceList,
    warnings: Vec<String>,
    vb_cable_url: &'static str,
    mic_muted: bool,
    version: &'static str,
}

impl AppState {
    fn save(&self, config: &Config) -> Result<(), String> {
        config.save(&self.config_path)
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            config: self.config.lock().unwrap().clone(),
            devices: self.devices.lock().unwrap().clone(),
            warnings: self.warnings.lock().unwrap().clone(),
            vb_cable_url: VB_CABLE_URL,
            mic_muted: self.mic_muted.load(Ordering::Relaxed),
            version: env!("CARGO_PKG_VERSION"),
        }
    }

    fn set_warnings(&self, kind: &str, items: Vec<String>) {
        let mut w = self.warnings.lock().unwrap();
        w.retain(|m| !m.starts_with(kind));
        w.extend(items.into_iter().map(|m| format!("{kind}{m}")));
    }
}

const WARN_AUDIO: &str = "Audio : ";
const WARN_HOTKEY: &str = "Raccourci : ";
const WARN_STARTUP: &str = "Démarrage : ";
const WARN_PHONE: &str = "Téléphone : ";
const OVERLAY: &str = "overlay";

/// Affiche l'overlay en jeu (créé à la première demande) ou le cache.
fn toggle_overlay_window(app: &AppHandle) -> Result<bool, String> {
    if let Some(w) = app.get_webview_window(OVERLAY) {
        let visible = w.is_visible().unwrap_or(false);
        if visible {
            w.hide().map_err(|e| e.to_string())?;
        } else {
            w.show().map_err(|e| e.to_string())?;
        }
        return Ok(!visible);
    }
    let (w, h) = (300.0, 460.0);
    let mut builder = WebviewWindowBuilder::new(app, OVERLAY, WebviewUrl::App("overlay.html".into()))
        .title("Soundboard — overlay")
        .inner_size(w, h)
        .min_inner_size(200.0, 160.0)
        .decorations(false)
        .transparent(true)
        .always_on_top(true)
        .skip_taskbar(true)
        // Cliquer sur l'overlay ne retire pas le focus au jeu.
        .focusable(false)
        .focused(false);
    // En haut à droite de l'écran principal.
    if let Ok(Some(monitor)) = app.primary_monitor() {
        let scale = monitor.scale_factor();
        let size = monitor.size().to_logical::<f64>(scale);
        builder = builder.position(size.width - w - 24.0, 80.0);
    }
    builder.build().map_err(|e| e.to_string())?;
    Ok(true)
}

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
            state.audio.toggle(play_request(&state, &sound))?;
            let _ = app.emit("sound-triggered", &sound.id);
            Ok(serde_json::json!({ "ok": true }))
        }
        phone::Request::Stop => {
            state.audio.stop_all();
            Ok(serde_json::json!({ "ok": true }))
        }
        phone::Request::Random(category) => {
            let sound = pick_random(&state, category.as_deref()).ok_or("aucun son")?;
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

fn play_request(state: &AppState, sound: &Sound) -> PlayRequest {
    let normalize = state.config.lock().unwrap().settings.normalize;
    let gain = if normalize { sound.normalize_gain() } else { 1.0 };
    PlayRequest {
        id: sound.id.clone(),
        path: sound.path.clone(),
        gain: sound.volume * gain,
        start_ms: sound.start_ms,
        end_ms: sound.end_ms,
        duration_ms: sound.play_duration_ms(),
        looping: sound.looping,
        keep_playing: sound.keep_playing,
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

/// Calcule durée et niveau des sons importés avant cette version.
fn analyze_missing(app: AppHandle) {
    std::thread::spawn(move || {
        let state = app.state::<AppState>();
        let todo: Vec<(String, PathBuf)> = state
            .config
            .lock()
            .unwrap()
            .sounds
            .iter()
            .filter(|s| s.analysis.is_none())
            .map(|s| (s.id.clone(), s.path.clone()))
            .collect();
        if todo.is_empty() {
            return;
        }
        let results: Vec<_> = todo
            .into_iter()
            .filter_map(|(id, path)| analysis::analyze(&path).ok().map(|a| (id, a)))
            .collect();
        let mut config = state.config.lock().unwrap();
        for (id, a) in results {
            if let Some(s) = config.sounds.iter_mut().find(|s| s.id == id) {
                s.analysis = Some(a);
            }
        }
        let _ = state.save(&config);
        drop(config);
        let _ = app.emit("state-changed", ());
    });
}

fn apply_audio(state: &AppState, settings: Settings) {
    let warnings = match state.audio.configure(settings) {
        Ok(()) => vec![],
        Err(e) => vec![e],
    };
    state.set_warnings(WARN_AUDIO, warnings);
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
        (&config.settings.overlay_hotkey, HotkeyAction::ToggleOverlay, "Overlay en jeu"),
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
                state.audio.toggle_detached(play_request(&state, &sound));
                let _ = app.emit("sound-triggered", &sound.id);
            }
        }
        Some(HotkeyAction::StopAll) => {
            state.audio.stop_all();
            let _ = app.emit("stopped", ());
        }
        Some(HotkeyAction::ToggleMute) => {
            let muted = !state.mic_muted.load(Ordering::Relaxed);
            set_muted(app, muted);
        }
        Some(HotkeyAction::ToggleOverlay) => {
            let _ = toggle_overlay_window(app);
        }
        Some(HotkeyAction::Random) => {
            if let Some(sound) = pick_random(&state, None) {
                state.audio.toggle_detached(play_request(&state, &sound));
                let _ = app.emit("sound-triggered", &sound.id);
            }
        }
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
    let config = state.config.lock().unwrap();
    let mut last = state.last_random.lock().unwrap();
    let pool: Vec<&Sound> = config
        .sounds
        .iter()
        .filter(|s| category.is_none() || s.category.as_deref() == category)
        .collect();
    let candidates: Vec<&Sound> = if pool.len() > 1 {
        pool.iter().copied().filter(|s| Some(&s.id) != last.as_ref()).collect()
    } else {
        pool
    };
    if candidates.is_empty() {
        return None;
    }
    let index = (uuid::Uuid::new_v4().as_u128() % candidates.len() as u128) as usize;
    let sound = candidates[index].clone();
    *last = Some(sound.id.clone());
    Some(sound)
}

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

#[tauri::command]
fn add_sounds(app: AppHandle, state: State<AppState>, paths: Vec<String>) -> Result<Snapshot, String> {
    let mut errors = Vec::new();
    {
        let mut config = state.config.lock().unwrap();
        for p in paths {
            let index = config.sounds.len();
            match import_file(Path::new(&p), &state.sounds_dir, index) {
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
    let state = app.state::<AppState>();
    add_sounds(app.clone(), state, paths)
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
    Ok(state.snapshot())
}

#[tauri::command]
fn remove_sound(app: AppHandle, state: State<AppState>, id: String) -> Result<Snapshot, String> {
    {
        let mut config = state.config.lock().unwrap();
        if let Some(pos) = config.sounds.iter().position(|s| s.id == id) {
            let sound = config.sounds.remove(pos);
            if sound.path.starts_with(&state.sounds_dir) {
                let _ = fs::remove_file(&sound.path);
            }
        }
        state.save(&config)?;
    }
    register_hotkeys(&app);
    Ok(state.snapshot())
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
    {
        let mut config = state.config.lock().unwrap();
        // Le code du téléphone ne se change que par « Nouveau code ».
        settings.phone_token = config.settings.phone_token.clone();
        if settings.phone_enabled && settings.phone_token.is_empty() {
            settings.phone_token = phone::new_token();
        }
        settings.phone_port = settings.phone_port.max(1024);
        let old = &config.settings;
        phone_changed = (old.phone_enabled, old.phone_port, &old.phone_token)
            != (settings.phone_enabled, settings.phone_port, &settings.phone_token);
        config.settings = settings.clone();
        state.save(&config)?;
    }
    if phone_changed {
        apply_phone(&app);
    }
    let autostart = settings.launch_at_startup;
    apply_audio(&state, settings);
    apply_autostart(&app, autostart);
    register_hotkeys(&app);
    Ok(state.snapshot())
}

/// Joue le son, ou l'arrête s'il est déjà en cours.
#[tauri::command]
fn play_sound(state: State<AppState>, id: String) -> Result<PlayOutcome, String> {
    let sound = find_sound(&state, &id)?;
    state.audio.toggle(play_request(&state, &sound))
}

/// Pré-écoute dans le casque seulement, avec une découpe pas encore enregistrée.
#[tauri::command]
fn preview_sound(
    state: State<AppState>,
    id: String,
    start_ms: u64,
    end_ms: Option<u64>,
    volume: f32,
) -> Result<(), String> {
    let mut sound = find_sound(&state, &id)?;
    sound.start_ms = start_ms;
    sound.end_ms = end_ms;
    sound.volume = volume;
    let mut req = play_request(&state, &sound);
    req.id = audio::PREVIEW_ID.into();
    // Une seule lecture pour suivre le curseur, même pour un son en boucle.
    req.looping = false;
    state.audio.preview(req)
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
    let peaks = tauri::async_runtime::spawn_blocking(move || analysis::waveform(&sound.path, 600))
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

#[tauri::command]
fn toggle_overlay(app: AppHandle) -> Result<bool, String> {
    toggle_overlay_window(&app)
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
fn stop_all(state: State<AppState>) {
    state.audio.stop_all();
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
    state.audio.toggle(play_request(&state, &sound))?;
    Ok(Some(sound.id))
}

#[tauri::command]
fn set_meter(state: State<AppState>, on: bool) {
    state.audio.set_meter(on);
}

#[tauri::command]
async fn check_update() -> Result<Option<UpdateInfo>, String> {
    online::check_update().await
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
        });
    }
    zip.start_file("pack.json", options).map_err(|e| e.to_string())?;
    let json = serde_json::to_vec_pretty(&pack).map_err(|e| e.to_string())?;
    zip.write_all(&json).map_err(|e| e.to_string())?;
    zip.finish().map_err(|e| e.to_string())?;
    Ok(pack.sounds.len())
}

/// Lit un pack et range ses sons. Les raccourcis déjà pris sont ignorés.
fn read_pack(src: &Path, config: &Config, sounds_dir: &Path) -> Result<(Vec<Sound>, Vec<String>), String> {
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
                added.push(sound);
            }
            Err(e) => errors.push(format!("{} : {e}", p.name)),
        }
    }
    Ok((added, errors))
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
async fn import_pack(app: AppHandle) -> Result<Option<ImportResult>, String> {
    let Some(src) = app
        .dialog()
        .file()
        .set_title("Importer un pack de sons")
        .add_filter("Pack de sons", &["zip"])
        .blocking_pick_file()
        .and_then(|f| f.into_path().ok())
    else {
        return Ok(None);
    };
    let state = app.state::<AppState>();
    let config = state.config.lock().unwrap().clone();
    let sounds_dir = state.sounds_dir.clone();
    let (added, errors) = tauri::async_runtime::spawn_blocking(move || read_pack(&src, &config, &sounds_dir))
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

#[tauri::command]
fn open_vb_cable_page(app: AppHandle) -> Result<(), String> {
    app.opener()
        .open_url(VB_CABLE_URL, None::<&str>)
        .map_err(|e| e.to_string())
}

fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "Ouvrir la soundboard", true, None::<&str>)?;
    let stop = MenuItem::with_id(app, "stop", "Tout arrêter", true, None::<&str>)?;
    let overlay = MenuItem::with_id(app, "overlay", "Overlay en jeu", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quitter", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &overlay, &stop, &quit])?;
    let mut tray = TrayIconBuilder::with_id("main")
        .tooltip("Soundboard")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => show_window(app),
            "overlay" => {
                let _ = toggle_overlay_window(app);
            }
            "stop" => {
                app.state::<AppState>().audio.stop_all();
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
            let config_path = app.path().app_config_dir()?.join("config.json");
            let sounds_dir = app.path().app_data_dir()?.join("sounds");
            let mut config = Config::load(&config_path);
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
            };
            apply_audio(&state, config.settings.clone());
            app.manage(state);
            register_hotkeys(app.handle());
            apply_autostart(app.handle(), config.settings.launch_at_startup);
            analyze_missing(app.handle().clone());
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
                // L'overlay se cache simplement : il garde sa place.
                if window.label() == OVERLAY {
                    api.prevent_close();
                    let _ = window.hide();
                    return;
                }
                let state = window.state::<AppState>();
                let to_tray = state.config.lock().unwrap().settings.close_to_tray;
                if to_tray {
                    api.prevent_close();
                    let _ = window.hide();
                } else {
                    // Sinon l'overlay garderait l'app ouverte.
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
            toggle_overlay,
            get_phone_info,
            regenerate_phone_token,
            set_mic_muted,
            toggle_favorite,
            add_from_url,
            add_from_youtube,
            play_random,
            set_meter,
            check_update,
            open_release_page,
            export_pack,
            import_pack,
        ])
        .run(tauri::generate_context!())
        .expect("erreur au lancement de l'application");
}
