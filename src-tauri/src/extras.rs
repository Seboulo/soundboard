//! Nouveautés de la 2.0 : rejouer les 30 dernières secondes, enregistrer
//! au micro, texte → voix, packs partagés par lien et salon entre potes.

use super::*;
use crate::room::{Incoming, Msg, Room, RoomStatus};
use std::collections::HashSet;
use std::time::{Duration, Instant};

#[derive(Default)]
pub struct Extras {
    pub room: Room,
    /// Sons envoyés à des potes, par empreinte (pour « envoie-le-moi »).
    sent: Mutex<HashMap<String, Sound>>,
    /// Empreintes déjà calculées des sons d'avant la 2.0 (chemin, taille).
    keys: Mutex<HashMap<PathBuf, (u64, String)>>,
    /// Dernier son reçu de chaque pote (anti-rafale).
    last_from: Mutex<HashMap<String, Instant>>,
    /// Un seul « Rejouer » à la fois.
    replay_busy: AtomicBool,
    /// Une seule vérification des packs à la fois.
    sync_busy: AtomicBool,
    tts_turn: AtomicU64,
    /// Dernier son ou extrait envoyé à des potes (pour « Tout arrêter »).
    pub(crate) last_sent: Mutex<Option<Instant>>,
    /// Son en cours joué de la part de chaque pote.
    playing_from: Mutex<HashMap<String, String>>,
}

fn data_dir(state: &AppState) -> PathBuf {
    state.sounds_dir.parent().map(Path::to_path_buf).unwrap_or_else(|| state.sounds_dir.clone())
}

/// Lancé une fois, quand l'audio est prêt.
pub fn start(app: &AppHandle) {
    let state = app.state::<AppState>();
    {
        let mut config = state.config.lock().unwrap();
        if config.settings.room_id.is_empty() {
            config.settings.room_id = uuid::Uuid::new_v4().simple().to_string();
            let _ = state.save(&config);
        }
        let s = &config.settings;
        replay::configure(s.replay_enabled, s.duck_enabled, s.duck_level);
    }
    replay::start_discord_capture();
    start_room(app);
    // Packs : les potes relisent les liens, celui qui partage tient ses
    // fichiers à jour.
    let handle = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(20));
        let mut last_sync = None::<Instant>;
        loop {
            refresh_published(&handle);
            if last_sync.is_none_or(|t| t.elapsed() > Duration::from_secs(2 * 3600)) {
                last_sync = Some(Instant::now());
                let h = handle.clone();
                tauri::async_runtime::spawn(async move {
                    let _ = sync_packs(h).await;
                });
            }
            std::thread::sleep(Duration::from_secs(60));
        }
    });
}

/// Ajoute un son à partir de données en mémoire et le range.
async fn add_data(app: &AppHandle, data: Vec<u8>, ext: &'static str, name: String, emoji: Option<&str>) -> Result<AddedSound, String> {
    let state = app.state::<AppState>();
    let index = state.config.lock().unwrap().sounds.len();
    let sounds_dir = state.sounds_dir.clone();
    let mut sound = tauri::async_runtime::spawn_blocking(move || store_sound(&data, ext, name, &sounds_dir, index))
        .await
        .map_err(|e| e.to_string())??;
    sound.emoji = emoji.map(String::from);
    let id = sound.id.clone();
    {
        let mut config = state.config.lock().unwrap();
        config.sounds.push(sound);
        state.save(&config)?;
    }
    Ok(AddedSound { id, snapshot: state.snapshot() })
}

/// « Extrait 3 » : premier numéro libre.
fn next_name(state: &AppState, prefix: &str) -> String {
    let config = state.config.lock().unwrap();
    let n = (1..)
        .find(|n| !config.sounds.iter().any(|s| s.name == format!("{prefix} {n}")))
        .unwrap_or(1);
    format!("{prefix} {n}")
}

// ---------- Rejouer les 30 dernières secondes ----------

#[tauri::command]
pub async fn save_replay(app: AppHandle) -> Result<AddedSound, String> {
    let state = app.state::<AppState>();
    if !state.config.lock().unwrap().settings.replay_enabled {
        return Err("active « Garder les 30 dernières secondes » dans les réglages".into());
    }
    if state.extras.replay_busy.swap(true, Ordering::SeqCst) {
        return Err("un extrait est déjà en cours d'enregistrement".into());
    }
    let res = async {
        let samples = tauri::async_runtime::spawn_blocking(replay::snapshot)
            .await
            .map_err(|e| e.to_string())?
            .ok_or("rien à rejouer : aucun son n'est passé par Discord ces 30 dernières secondes")?;
        let name = next_name(&state, "Extrait");
        add_data(&app, calibration::wav_bytes(&samples), "wav", name, Some("⏪")).await
    }
    .await;
    state.extras.replay_busy.store(false, Ordering::SeqCst);
    res
}

/// Raccourci « Rejouer » : on prévient l'interface (même cachée).
pub fn replay_from_hotkey(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        match save_replay(app.clone()).await {
            Ok(added) => {
                let _ = app.emit("snapshot", &added.snapshot);
                let _ = app.emit("replay-saved", &added.id);
            }
            Err(e) => {
                let _ = app.emit("replay-error", e);
            }
        }
    });
}

// ---------- Enregistrer un son au micro ----------

#[tauri::command]
pub fn mic_record_start(state: State<AppState>) -> Result<(), String> {
    state.audio.mic_record(true).map(|_| ())
}

#[tauri::command]
pub async fn mic_record_stop(app: AppHandle, keep: bool) -> Result<Option<AddedSound>, String> {
    let state = app.state::<AppState>();
    let samples = state.audio.mic_record(false)?;
    if !keep {
        return Ok(None);
    }
    let samples = trim_silence(&samples);
    if samples.len() < denoise::RATE as usize / 4 {
        return Err("rien d'enregistré : parle un peu plus longtemps (ou vérifie que ton micro n'est pas coupé)".into());
    }
    let name = next_name(&state, "Enregistrement");
    add_data(&app, calibration::wav_bytes(samples), "wav", name, Some("🎙️")).await.map(Some)
}

/// Enlève le silence (micro coupé entre les phrases) au début et à la fin,
/// en gardant 0,1 s de marge.
fn trim_silence(s: &[f32]) -> &[f32] {
    let margin = denoise::RATE as usize / 10;
    let first = s.iter().position(|v| v.abs() > 0.003).unwrap_or(s.len());
    let last = s.iter().rposition(|v| v.abs() > 0.003).map(|i| i + 1).unwrap_or(0);
    if first >= last {
        return &[];
    }
    &s[first.saturating_sub(margin)..(last + margin).min(s.len())]
}

// ---------- Texte → voix ----------

#[tauri::command]
pub async fn tts_voices() -> Result<Vec<tts::VoiceInfo>, String> {
    tauri::async_runtime::spawn_blocking(tts::voices).await.map_err(|e| e.to_string())?
}

async fn speak(text: String, voice: Option<String>, rate: f64) -> Result<Vec<u8>, String> {
    tauri::async_runtime::spawn_blocking(move || tts::speak(&text, voice.as_deref(), rate))
        .await
        .map_err(|e| e.to_string())?
}

/// Dit la phrase tout de suite dans Discord, sans créer de pad.
#[tauri::command]
pub async fn tts_say(app: AppHandle, text: String, voice: Option<String>, rate: f64) -> Result<(), String> {
    let wav = speak(text, voice, rate).await?;
    let state = app.state::<AppState>();
    // Deux fichiers en alternance : le précédent peut encore être en lecture.
    let turn = state.extras.tts_turn.fetch_add(1, Ordering::Relaxed) % 2;
    let path = data_dir(&state).join(format!("voix-{turn}.wav"));
    fs::write(&path, &wav).map_err(|e| e.to_string())?;
    let analysis = analysis::analyze(&path)?;
    let mut sound = store_like(&path, analysis);
    sound.id = "__tts".into();
    state.audio.stop("__tts");
    state.audio.toggle(play_request(&state, &sound)).map(|_| ())
}

/// Crée un pad avec la phrase.
#[tauri::command]
pub async fn tts_create(app: AppHandle, text: String, voice: Option<String>, rate: f64) -> Result<AddedSound, String> {
    let name: String = text.trim().chars().take(40).collect();
    let wav = speak(text, voice, rate).await?;
    add_data(&app, wav, "wav", name, Some("🗣️")).await
}

/// Un son qui n'est pas dans la soundboard (voix, son reçu d'un pote).
pub(crate) fn store_like(path: &Path, analysis: config::Analysis) -> Sound {
    Sound {
        id: String::new(),
        name: String::new(),
        path: path.to_path_buf(),
        hotkey: None,
        volume: 1.0,
        color: COLORS[0].into(),
        start_ms: 0,
        end_ms: None,
        analysis: Some(analysis),
        category: None,
        looping: false,
        emoji: None,
        favorite: false,
        keep_playing: true,
        added_at: 0,
        image: None,
        image_still: None,
        plays: 0,
        played_ms: 0,
        last_played: 0,
        fx: Default::default(),
        segment: None,
        pack_link: None,
        pack_key: None,
    }
}

// ---------- Packs partagés par lien ----------

#[derive(Serialize)]
pub struct SyncResult {
    added: usize,
    removed: usize,
    errors: Vec<String>,
    snapshot: Snapshot,
}

/// S'abonne au pack d'un pote. `name` : onglet où ranger ses sons.
#[tauri::command]
pub async fn subscribe_pack(app: AppHandle, url: String, name: Option<String>) -> Result<SyncResult, String> {
    let url = url.trim().to_string();
    share::direct_url(&url)?;
    let state = app.state::<AppState>();
    {
        let mut config = state.config.lock().unwrap();
        if config.settings.subscriptions.iter().any(|s| s.url == url) {
            return Err("tu suis déjà ce pack".into());
        }
        config.settings.subscriptions.push(config::Subscription {
            url: url.clone(),
            name: clean_category(name).unwrap_or_default(),
            ..Default::default()
        });
        state.save(&config)?;
    }
    match sync_one(&app, &url).await {
        Ok((added, removed, errors)) => Ok(SyncResult { added, removed, errors, snapshot: state.snapshot() }),
        Err(e) => {
            // Lien qui ne marche pas du tout : on ne le garde pas.
            let mut config = state.config.lock().unwrap();
            config.settings.subscriptions.retain(|s| s.url != url);
            let _ = state.save(&config);
            Err(e)
        }
    }
}

#[tauri::command]
pub fn unsubscribe_pack(app: AppHandle, url: String, remove_sounds: bool) -> Result<Snapshot, String> {
    let state = app.state::<AppState>();
    {
        let mut config = state.config.lock().unwrap();
        config.settings.subscriptions.retain(|s| s.url != url);
        if remove_sounds {
            let (gone, kept): (Vec<Sound>, Vec<Sound>) =
                std::mem::take(&mut config.sounds).into_iter().partition(|s| s.pack_link.as_deref() == Some(&url));
            config.sounds = kept;
            for s in gone {
                let _ = fs::remove_file(&s.path);
            }
        } else {
            // Les sons restent, comme des sons à toi.
            for s in config.sounds.iter_mut().filter(|s| s.pack_link.as_deref() == Some(&url)) {
                s.pack_link = None;
            }
        }
        state.save(&config)?;
    }
    register_hotkeys(&app);
    Ok(state.snapshot())
}

/// Relit tous les packs suivis.
#[tauri::command]
pub async fn sync_packs(app: AppHandle) -> Result<SyncResult, String> {
    let state = app.state::<AppState>();
    if state.extras.sync_busy.swap(true, Ordering::SeqCst) {
        return Err("vérification déjà en cours".into());
    }
    let urls: Vec<String> = state.config.lock().unwrap().settings.subscriptions.iter().map(|s| s.url.clone()).collect();
    let (mut added, mut removed, mut errors) = (0, 0, Vec::new());
    for url in urls {
        match sync_one(&app, &url).await {
            Ok((a, r, e)) => {
                added += a;
                removed += r;
                errors.extend(e);
            }
            Err(e) => errors.push(e),
        }
    }
    state.extras.sync_busy.store(false, Ordering::SeqCst);
    let snapshot = state.snapshot();
    if added + removed > 0 {
        let _ = app.emit("snapshot", &snapshot);
        let _ = app.emit("packs-updated", (added, removed));
    }
    Ok(SyncResult { added, removed, errors, snapshot })
}

/// Met un pack suivi à jour : nouveaux sons ajoutés, sons retirés par le
/// pote retirés aussi.
async fn sync_one(app: &AppHandle, url: &str) -> Result<(usize, usize, Vec<String>), String> {
    let state = app.state::<AppState>();
    let res = share::download(url).await;
    let note = |error: Option<String>, hash: Option<String>| {
        let mut config = state.config.lock().unwrap();
        if let Some(sub) = config.settings.subscriptions.iter_mut().find(|s| s.url == url) {
            sub.last_check = now_secs();
            sub.error = error;
            if let Some(h) = hash {
                sub.last_hash = h;
            }
        }
        let _ = state.save(&config);
    };
    let (data, hash) = match res {
        Ok(x) => x,
        Err(e) => {
            note(Some(e.clone()), None);
            return Err(e);
        }
    };
    let (unchanged, name) = {
        let config = state.config.lock().unwrap();
        let sub = config.settings.subscriptions.iter().find(|s| s.url == url).ok_or("abonnement retiré")?;
        (sub.last_hash == hash, sub.name.clone())
    };
    if unchanged {
        note(None, None);
        return Ok((0, 0, Vec::new()));
    }
    let tmp = data_dir(&state).join("pack-lien.zip");
    fs::write(&tmp, &data).map_err(|e| e.to_string())?;
    let config = state.config.lock().unwrap().clone();
    let have: HashSet<String> = config
        .sounds
        .iter()
        .filter(|s| s.pack_link.as_deref() == Some(url))
        .filter_map(|s| s.pack_key.clone())
        .collect();
    let sounds_dir = state.sounds_dir.clone();
    let tmp2 = tmp.clone();
    let have2 = have.clone();
    let read = tauri::async_runtime::spawn_blocking(move || read_pack(&tmp2, &config, &sounds_dir, &have2))
        .await
        .map_err(|e| e.to_string())?;
    let _ = fs::remove_file(&tmp);
    let (mut added, errors, keys) = match read {
        Ok(x) => x,
        Err(e) => {
            note(Some(e.clone()), None);
            return Err(e);
        }
    };
    // Onglet : celui choisi, sinon le plus courant dans le pack.
    let tab = if name.is_empty() {
        let mut counts: HashMap<String, usize> = HashMap::new();
        for s in &added {
            *counts.entry(s.category.clone().unwrap_or_default()).or_default() += 1;
        }
        counts
            .into_iter()
            .filter(|(c, _)| !c.is_empty())
            .max_by_key(|(_, n)| *n)
            .map(|(c, _)| c)
            .unwrap_or_else(|| "Pack de potes".into())
    } else {
        name.clone()
    };
    for s in added.iter_mut() {
        s.pack_link = Some(url.to_string());
        s.category = Some(tab.clone());
        // Les raccourcis d'un pote ne prennent pas tes touches.
        s.hotkey = None;
    }
    let keys: HashSet<String> = keys.into_iter().collect();
    let count = added.len();
    let removed;
    {
        let mut config = state.config.lock().unwrap();
        let mut gone = Vec::new();
        config.sounds.retain(|s| {
            let drop = s.pack_link.as_deref() == Some(url) && s.pack_key.as_ref().is_some_and(|k| !keys.contains(k));
            if drop {
                gone.push(s.path.clone());
            }
            !drop
        });
        removed = gone.len();
        for p in gone {
            let _ = fs::remove_file(p);
        }
        config.sounds.extend(added);
        if let Some(sub) = config.settings.subscriptions.iter_mut().find(|s| s.url == url) {
            sub.name = tab;
        }
        state.save(&config)?;
    }
    note(None, Some(hash));
    register_hotkeys(app);
    Ok((count, removed, errors))
}

#[derive(Serialize)]
pub struct CloudFolder {
    name: String,
    path: String,
}

/// Dossiers synchronisés trouvés (OneDrive, Dropbox, Google Drive).
#[tauri::command]
pub fn cloud_folders() -> Vec<CloudFolder> {
    share::cloud_folders()
        .into_iter()
        .map(|(name, p)| CloudFolder { name, path: p.to_string_lossy().into_owned() })
        .collect()
}

/// Partage un onglet : son pack est rangé dans `folder` (ou un dossier
/// choisi) et tenu à jour.
#[tauri::command]
pub async fn publish_tab(app: AppHandle, category: String, folder: Option<String>) -> Result<Option<Snapshot>, String> {
    let folder = match folder {
        Some(f) => PathBuf::from(f),
        None => match app
            .dialog()
            .file()
            .set_title("Dossier synchronisé (OneDrive, Dropbox, Google Drive…)")
            .blocking_pick_folder()
            .and_then(|f| f.into_path().ok())
        {
            Some(f) => f,
            None => return Ok(None),
        },
    };
    let file = format!(
        "Soundboard - {}.zip",
        category.replace(['/', '\\', ':', '*', '?', '"', '<', '>', '|'], "_")
    );
    let path = folder.join(file);
    let state = app.state::<AppState>();
    {
        let mut config = state.config.lock().unwrap();
        config.settings.published.retain(|p| p.category != category);
        config.settings.published.push(config::Published { category, path, ..Default::default() });
        state.save(&config)?;
    }
    let handle = app.clone();
    tauri::async_runtime::spawn_blocking(move || refresh_published(&handle)).await.map_err(|e| e.to_string())?;
    Ok(Some(state.snapshot()))
}

#[tauri::command]
pub fn unpublish_tab(state: State<AppState>, category: String) -> Result<Snapshot, String> {
    {
        let mut config = state.config.lock().unwrap();
        config.settings.published.retain(|p| p.category != category);
        state.save(&config)?;
    }
    Ok(state.snapshot())
}

/// Ouvre le dossier du fichier partagé, fichier sélectionné.
#[tauri::command]
pub fn reveal_published(app: AppHandle, category: String) -> Result<(), String> {
    let state = app.state::<AppState>();
    let path = state
        .config
        .lock()
        .unwrap()
        .settings
        .published
        .iter()
        .find(|p| p.category == category)
        .map(|p| p.path.clone())
        .ok_or("cet onglet n'est pas partagé")?;
    app.opener().reveal_item_in_dir(path).map_err(|e| e.to_string())
}

/// Ce qui change le pack d'un onglet.
fn signature(sounds: &[Sound]) -> String {
    let parts: Vec<_> = sounds
        .iter()
        .map(|s| {
            serde_json::json!([s.id, s.name, s.start_ms, s.end_ms, s.volume, s.emoji, s.color, s.looping, s.fx,
                fs::metadata(&s.path).map(|m| m.len()).unwrap_or(0)])
        })
        .collect();
    share::key_of(serde_json::to_string(&parts).unwrap_or_default().as_bytes())
}

/// Réécrit les packs partagés dont l'onglet a changé.
fn refresh_published(app: &AppHandle) {
    let state = app.state::<AppState>();
    let (published, sounds) = {
        let config = state.config.lock().unwrap();
        (config.settings.published.clone(), config.sounds.clone())
    };
    for p in published {
        let tab: Vec<Sound> = sounds.iter().filter(|s| s.category.as_deref() == Some(p.category.as_str())).cloned().collect();
        let sig = signature(&tab);
        if sig == p.signature && p.path.exists() {
            continue;
        }
        // Écrit à côté puis remplace : un pote ne télécharge jamais un pack à moitié écrit.
        let tmp = p.path.with_extension("zip.tmp");
        let res = if tab.is_empty() {
            Err("onglet vide".to_string())
        } else {
            write_pack(&tmp, &tab).and_then(|_| fs::rename(&tmp, &p.path).map_err(|e| e.to_string()))
        };
        let _ = fs::remove_file(&tmp);
        let mut config = state.config.lock().unwrap();
        if let Some(q) = config.settings.published.iter_mut().find(|q| q.category == p.category) {
            match res {
                Ok(_) => {
                    q.signature = sig;
                    q.error = None;
                }
                Err(e) => q.error = Some(e),
            }
        }
        let _ = state.save(&config);
    }
}

// ---------- Salon entre potes ----------

#[tauri::command]
pub fn room_status(state: State<AppState>) -> RoomStatus {
    state.extras.room.status()
}

/// Choisit chez qui les sons sont joués (None : chez moi).
#[tauri::command]
pub fn set_room_target(app: AppHandle, target: Option<String>) -> RoomStatus {
    let state = app.state::<AppState>();
    state.extras.room.set_target(target);
    let status = state.extras.room.status();
    let _ = app.emit("room", &status);
    status
}

#[tauri::command]
pub fn new_room_code() -> String {
    room::new_code()
}

/// (Re)joint le salon des réglages.
pub fn start_room(app: &AppHandle) {
    let state = app.state::<AppState>();
    let s = state.config.lock().unwrap().settings.clone();
    let name = if s.room_name.trim().is_empty() {
        std::env::var("USERNAME").unwrap_or_else(|_| "Un pote".into())
    } else {
        s.room_name.trim().to_string()
    };
    let (h1, h2) = (app.clone(), app.clone());
    state.extras.room.start(
        &s.room_code,
        s.room_id.clone(),
        name,
        move |inc| on_room_message(&h1, inc),
        move || {
            if let Some(state) = h2.try_state::<AppState>() {
                let _ = h2.emit("room", state.extras.room.status());
            }
        },
    );
}

/// Joue le son ici, ou chez le pote choisi dans le salon.
pub fn play_or_send(app: &AppHandle, sound: Sound) {
    let state = app.state::<AppState>();
    if state.extras.room.target().is_some() {
        if let Err(e) = send_to_friend(app, sound) {
            let _ = app.emit("room-error", e);
        }
        return;
    }
    note_play(&state, &sound);
    state.audio.toggle_detached(play_request(&state, &sound));
    let _ = app.emit("sound-triggered", &sound.id);
}

/// Empreinte du fichier d'un son (calculée une fois).
fn sound_key(state: &AppState, sound: &Sound) -> Result<String, String> {
    if let Some(k) = &sound.pack_key {
        return Ok(k.clone());
    }
    let size = fs::metadata(&sound.path).map_err(|e| e.to_string())?.len();
    if let Some((s, k)) = state.extras.keys.lock().unwrap().get(&sound.path) {
        if *s == size {
            return Ok(k.clone());
        }
    }
    let key = share::key_of(&fs::read(&sound.path).map_err(|e| e.to_string())?);
    state.extras.keys.lock().unwrap().insert(sound.path.clone(), (size, key.clone()));
    Ok(key)
}

fn play_msg(state: &AppState, sound: &Sound, to: String, hash: String) -> Msg {
    Msg::Play {
        from: state.extras.room.my_id(),
        name: state.extras.room.my_name(),
        to,
        hash,
        title: sound.name.chars().take(60).collect(),
        ext: sound.path.extension().and_then(|e| e.to_str()).unwrap_or("mp3").to_lowercase(),
        start_ms: sound.start_ms,
        end_ms: sound.end_ms,
        volume: sound.volume,
        fx: sound.fx,
    }
}

/// Demande au pote choisi de jouer ce son chez lui.
pub fn send_to_friend(app: &AppHandle, sound: Sound) -> Result<PlayOutcome, String> {
    let state = app.state::<AppState>();
    let to = state.extras.room.target().ok_or("aucun pote choisi")?;
    let hash = sound_key(&state, &sound)?;
    let msg = play_msg(&state, &sound, to, hash.clone());
    state.extras.sent.lock().unwrap().insert(hash, sound.clone());
    let room = state.extras.room.clone();
    let app2 = app.clone();
    journal::write(&format!("Salon : « {} » envoyé", sound.name));
    *state.extras.last_sent.lock().unwrap() = Some(Instant::now());
    tauri::async_runtime::spawn(async move {
        if let Err(e) = room.publish(&msg).await {
            journal::write(&format!("Salon : envoi impossible ({e})"));
            let _ = app2.emit("room-error", e);
        }
    });
    let _ = app.emit("room-sent", &sound.name);
    Ok(PlayOutcome::Started)
}

fn on_room_message(app: &AppHandle, inc: Incoming) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(e) = handle_room_message(&app, inc).await {
            journal::write(&format!("Salon : {e}"));
        }
    });
}

async fn handle_room_message(app: &AppHandle, inc: Incoming) -> Result<(), String> {
    let state = app.state::<AppState>();
    match inc.msg {
        Msg::Need { from, hash, .. } => {
            // Le pote n'a pas le son : on le lui envoie.
            let Some(sound) = state.extras.sent.lock().unwrap().get(&hash).cloned() else {
                journal::write("Salon : un pote demande un son inconnu");
                return Ok(());
            };
            journal::write(&format!("Salon : un pote n'a pas « {} », envoi du fichier", sound.name));
            let data = fs::read(&sound.path).map_err(|e| e.to_string())?;
            if data.len() as u64 > room::MAX_ATTACHMENT {
                let _ = app.emit("room-error", format!("« {} » est trop lourd pour être envoyé (15 Mo au plus)", sound.name));
                return Ok(());
            }
            let msg = play_msg(&state, &sound, from, hash.clone());
            let file = format!("{hash}.{}", sound.path.extension().and_then(|e| e.to_str()).unwrap_or("mp3"));
            state.extras.room.publish_file(&msg, &file, data).await
        }
        Msg::Play { from, name, hash, title, ext, start_ms, end_ms, volume, fx, .. } => {
            journal::write(&format!(
                "Salon : « {title} » reçu de {name}{}",
                if inc.attachment.is_some() { " (avec le fichier)" } else { "" }
            ));
            if is_blocked(&state, &from) {
                journal::write(&format!("Salon : ignoré ({name} est bloqué)"));
                return Ok(());
            }
            if !state.config.lock().unwrap().settings.room_accept {
                journal::write("Salon : ignoré (« Mes potes peuvent jouer des sons chez moi » est coupé)");
                return Ok(());
            }
            // Pas plus d'un son par seconde et demie et par pote (sauf le
            // fichier qu'on vient de lui demander, qui arrive juste après).
            if inc.attachment.is_none() {
                let mut last = state.extras.last_from.lock().unwrap();
                if last.get(&from).is_some_and(|t| t.elapsed() < Duration::from_millis(1500)) {
                    journal::write("Salon : ignoré (trop rapproché du précédent)");
                    return Ok(());
                }
                last.insert(from.clone(), Instant::now());
            }
            let ext = if AUDIO_EXTENSIONS.contains(&ext.as_str()) { ext } else { "mp3".into() };
            let hash: String = hash.chars().filter(|c| c.is_ascii_hexdigit()).take(32).collect();
            let cache = data_dir(&state).join("salon").join(format!("{hash}.{ext}"));
            // Le son est-il déjà là ? Dans la soundboard, ou reçu avant.
            let sounds = state.config.lock().unwrap().sounds.clone();
            let local = sounds.into_iter().find(|s| sound_key(&state, s).is_ok_and(|k| k == hash));
            let path = match (local, &inc.attachment) {
                (Some(s), _) => s.path,
                (None, _) if cache.exists() => cache,
                (None, Some(url)) => {
                    let data = room::fetch(url).await?;
                    if share::key_of(&data) != hash {
                        return Err(format!("fichier reçu abîmé ({} octets)", data.len()));
                    }
                    fs::create_dir_all(cache.parent().unwrap()).map_err(|e| e.to_string())?;
                    trim_cache(cache.parent().unwrap());
                    fs::write(&cache, data).map_err(|e| e.to_string())?;
                    cache
                }
                (None, None) => {
                    let msg = Msg::Need { from: state.extras.room.my_id(), to: from, hash };
                    return state.extras.room.publish(&msg).await;
                }
            };
            let p2 = path.clone();
            let analysis = tauri::async_runtime::spawn_blocking(move || analysis::analyze(&p2))
                .await
                .map_err(|e| e.to_string())??;
            let mut sound = store_like(&path, analysis);
            sound.id = format!("salon-{hash}");
            sound.start_ms = start_ms;
            // 30 s au plus, jamais en boucle. La vraie fin du son, pas « dans
            // 30 s » : sinon le son passait pour « en cours » 30 s et le
            // suivant identique l'arrêtait au lieu de le rejouer.
            let total = sound.analysis.map_or(u64::MAX, |a| a.duration_ms);
            sound.end_ms = Some(end_ms.unwrap_or(total).min(total).min(start_ms + 30_000));
            sound.volume = volume.clamp(0.0, 2.0);
            sound.fx = fx.sanitized();
            // Un pote qui renvoie le même son le rejoue depuis le début (on
            // n'arrête pas celui en cours comme un clic sur un pad).
            state.audio.stop(&sound.id);
            state.audio.toggle_detached(play_request(&state, &sound));
            state.extras.playing_from.lock().unwrap().insert(from.clone(), sound.id.clone());
            let _ = app.emit("room-played", (name, title));
            Ok(())
        }
        Msg::Stop { from, .. } => {
            // Le pote arrête ce qu'il nous a envoyé.
            if let Some(id) = state.extras.playing_from.lock().unwrap().remove(&from) {
                state.audio.stop(&id);
            }
            clip::close_from(app, &from);
            journal::write("Salon : un pote a arrêté son envoi");
            Ok(())
        }
        Msg::Clip { from, name, title, to, link, start_ms, end_ms, volume } => {
            let direct = !to.is_empty() && to == state.extras.room.my_id();
            let link = (!link.is_empty()).then_some(clip::LinkClip { url: link, start_ms, end_ms, volume: volume.unwrap_or(1.0) });
            clip::receive(app, from, name, title, inc.attachment, direct, link).await
        }
        _ => Ok(()),
    }
}

/// « Tout arrêter » : arrête aussi, chez les potes, ce qu'on vient de leur
/// envoyer (dans la dernière minute).
pub fn stop_sent(app: &AppHandle) {
    let state = app.state::<AppState>();
    let recent = state.extras.last_sent.lock().unwrap().take().is_some_and(|t| t.elapsed() < Duration::from_secs(60));
    if !recent {
        return;
    }
    let room = state.extras.room.clone();
    let msg = Msg::Stop { from: room.my_id(), to: String::new() };
    tauri::async_runtime::spawn(async move {
        let _ = room.publish(&msg).await;
    });
}

/// Ce pote est-il bloqué (ni sons ni extraits) ?
pub(crate) fn is_blocked(state: &AppState, id: &str) -> bool {
    state.config.lock().unwrap().settings.room_blocked.iter().any(|b| b.id == id)
}

/// Bloque ou débloque un pote du salon.
#[tauri::command]
pub fn set_friend_blocked(app: AppHandle, id: String, name: String, blocked: bool) -> Result<Snapshot, String> {
    let state = app.state::<AppState>();
    {
        let mut config = state.config.lock().unwrap();
        let list = &mut config.settings.room_blocked;
        list.retain(|b| b.id != id);
        if blocked {
            list.push(config::BlockedFriend { id: id.clone(), name: name.chars().take(32).collect() });
        }
        state.save(&config)?;
    }
    if blocked {
        if state.extras.room.target().as_deref() == Some(id.as_str()) {
            state.extras.room.set_target(None);
        }
        // Ce qui joue ou s'affiche de sa part s'arrête.
        clip::close(&app);
    }
    let _ = app.emit("room", state.extras.room.status());
    Ok(state.snapshot())
}

/// Garde au plus 60 sons reçus.
fn trim_cache(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    if files.len() < 60 {
        return;
    }
    files.sort();
    for (_, p) in files.iter().take(files.len() - 59) {
        let _ = fs::remove_file(p);
    }
}

#[cfg(test)]
mod tests {
    use super::trim_silence;

    #[test]
    fn silence_is_trimmed_with_a_margin() {
        let mut s = vec![0.0f32; 48_000];
        s.extend(std::iter::repeat_n(0.5, 4800));
        s.extend(vec![0.0f32; 48_000]);
        let t = trim_silence(&s);
        assert_eq!(t.len(), 4800 + 2 * 4800);
        assert!(trim_silence(&[0.0; 1000]).is_empty());
    }
}
