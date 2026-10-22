mod analysis;
mod audio;
mod config;
mod online;

use audio::{AudioHandle, DeviceList, PlayOutcome, PlayRequest, PlayingStatus};
use config::{Config, Settings, Sound};
use online::OnlineSound;
use serde::Serialize;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Mutex;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};
use tauri_plugin_autostart::ManagerExt as AutostartExt;
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};
use tauri_plugin_opener::OpenerExt;

const AUDIO_EXTENSIONS: &[&str] = &["mp3", "wav", "ogg", "flac", "m4a", "aac"];
const COLORS: &[&str] = &[
    "#5865f2", "#57f287", "#fee75c", "#eb459e", "#ed4245", "#3ba55c", "#faa61a", "#9b59b6",
];
pub const VB_CABLE_URL: &str = "https://vb-audio.com/Cable/";

#[derive(Clone)]
enum HotkeyAction {
    Play(String),
    StopAll,
}

struct AppState {
    config: Mutex<Config>,
    config_path: PathBuf,
    sounds_dir: PathBuf,
    audio: AudioHandle,
    hotkeys: Mutex<HashMap<u32, HotkeyAction>>,
    /// Problèmes à montrer dans l'interface (raccourci déjà pris, sortie absente…).
    warnings: Mutex<Vec<String>>,
}

#[derive(Serialize)]
struct Snapshot {
    config: Config,
    devices: DeviceList,
    warnings: Vec<String>,
    vb_cable_url: &'static str,
}

impl AppState {
    fn save(&self, config: &Config) -> Result<(), String> {
        config.save(&self.config_path)
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            config: self.config.lock().unwrap().clone(),
            devices: audio::list_devices(),
            warnings: self.warnings.lock().unwrap().clone(),
            vb_cable_url: VB_CABLE_URL,
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
    if let Some(h) = config.settings.stop_hotkey.clone() {
        entries.push((h, HotkeyAction::StopAll, "Tout arrêter".into()));
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
        None => {}
    }
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
    })
}

#[tauri::command]
fn get_state(state: State<AppState>) -> Snapshot {
    state.snapshot()
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
fn update_settings(app: AppHandle, state: State<AppState>, settings: Settings) -> Result<Snapshot, String> {
    {
        let mut config = state.config.lock().unwrap();
        config.settings = settings.clone();
        state.save(&config)?;
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
    state.audio.preview(req)
}

#[tauri::command]
async fn get_waveform(app: AppHandle, id: String) -> Result<Vec<f32>, String> {
    let sound = find_sound(&app.state::<AppState>(), &id)?;
    tauri::async_runtime::spawn_blocking(move || analysis::waveform(&sound.path, 600))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn search_online(query: String, page: Option<u32>) -> Result<Vec<OnlineSound>, String> {
    online::search(query.trim(), page.unwrap_or(1)).await
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
fn stop_all(state: State<AppState>) {
    state.audio.stop_all();
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

            // Premier lancement : si VB-Cable est déjà installé, on le choisit d'office.
            if config.settings.virtual_device.is_none() {
                config.settings.virtual_device = audio::list_devices().vb_cable;
            }

            let handle = app.handle().clone();
            let audio = AudioHandle::spawn(move |status: Vec<PlayingStatus>| {
                let _ = handle.emit("playing", status);
            });

            let state = AppState {
                config: Mutex::new(config.clone()),
                config_path,
                sounds_dir,
                audio,
                hotkeys: Mutex::new(HashMap::new()),
                warnings: Mutex::new(Vec::new()),
            };
            apply_audio(&state, config.settings.clone());
            app.manage(state);
            register_hotkeys(app.handle());
            apply_autostart(app.handle(), config.settings.launch_at_startup);
            analyze_missing(app.handle().clone());
            build_tray(app.handle())?;

            // Lancé avec Windows : on reste discret près de l'horloge.
            if !std::env::args().any(|a| a == MINIMIZED_ARG) {
                show_window(app.handle());
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                let state = window.state::<AppState>();
                let to_tray = state.config.lock().unwrap().settings.close_to_tray;
                if to_tray {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_state,
            add_sounds,
            pick_sounds,
            update_sound,
            remove_sound,
            move_sound,
            update_settings,
            play_sound,
            preview_sound,
            get_waveform,
            search_online,
            add_online,
            stop_all,
            open_vb_cable_page,
        ])
        .run(tauri::generate_context!())
        .expect("erreur au lancement de l'application");
}
