//! Extraits sur l'écran d'un pote : une image, un GIF ou une petite vidéo
//! (avec un son si on veut) qui s'affiche dans un coin de son écran.
//!
//! L'extrait voyage par le salon (même relais que les sons) : un seul
//! fichier qui regroupe l'image ou la vidéo et le son. Chez le pote, une
//! petite fenêtre toujours au premier plan l'affiche sans prendre la main
//! sur le jeu, puis disparaît. Chacun choisit : refuser, demander ou
//! afficher direct.

use super::*;
use crate::room::Msg;
use std::time::{Duration, Instant};
use tauri::{LogicalSize, WebviewUrl, WebviewWindowBuilder};

/// Taille d'un extrait envoyé (image ou vidéo + son).
/// ntfy.sh refuse les pièces jointes de plus de 15 Mo : on garde de la marge.
pub const MAX_CLIP: u64 = 14 * 1024 * 1024 + 512 * 1024;
/// Taille de l'image ou de la vidéo choisie.
pub const MAX_MEDIA: u64 = 14 * 1024 * 1024;
const MAGIC: &[u8] = b"SBCLIP1\n";
const IMAGES: &[&str] = &["png", "jpg", "jpeg", "gif", "webp"];
const VIDEOS: &[&str] = &["mp4", "webm"];
/// Les fenêtres d'extrait s'appellent clip-1, clip-2… (une neuve par extrait).
const WINDOW_PREFIX: &str = "clip-";
/// Un extrait par pote toutes les 4 secondes au plus.
const SPAM_GAP: Duration = Duration::from_secs(4);

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
struct Header {
    media_ext: String,
    media_len: u64,
    sound_ext: String,
    sound_len: u64,
    start_ms: u64,
    end_ms: Option<u64>,
    volume: f32,
}

/// Ce qu'il faut afficher (envoyé à la fenêtre de l'extrait).
#[derive(Debug, Clone, Serialize)]
pub struct ClipShow {
    pub from: String,
    pub title: String,
    pub kind: &'static str,
    pub media: String,
    pub sound: Option<String>,
    pub start_ms: u64,
    pub end_ms: Option<u64>,
    pub volume: f32,
    /// Qui l'envoie (pour « Bloquer »).
    pub from_id: String,
    /// Demander avant d'afficher.
    pub ask: bool,
    /// Aperçu de son propre extrait (pas de « Bloquer » ni « Garder »).
    pub preview: bool,
    /// Envoyé à moi seul (pas à tout le salon) : son son part aussi dans
    /// Discord. Envoyé à tous, chacun l'entend chez soi : s'il partait dans
    /// Discord chez chacun, on l'entendrait plusieurs fois.
    pub direct: bool,
}

/// Un extrait envoyé récemment (pour le renvoyer en un clic).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Recent {
    pub path: String,
    pub kind: String,
    pub title: String,
    pub start_s: Option<f64>,
    pub end_s: Option<f64>,
    pub volume: Option<f32>,
    pub sound_id: Option<String>,
    /// Reçu : nom du pote, et son à part (image + son).
    #[serde(default)]
    pub from: Option<String>,
    #[serde(default)]
    pub sound: Option<String>,
    /// Date (secondes depuis 1970).
    #[serde(default)]
    pub at: u64,
    /// Lien d'origine (vidéo d'un site) : renvoyée par lien, sans fichier.
    #[serde(default)]
    pub link: Option<String>,
    /// Passage seul d'une vidéo longue : où le fichier commence dans la
    /// vidéo du lien (ms).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cut_ms: Option<u64>,
}

/// Vidéo reçue par son lien : l'appli la télécharge elle-même.
pub struct LinkClip {
    pub url: String,
    pub start_ms: u64,
    pub end_ms: Option<u64>,
    pub volume: f32,
    /// Vidéo longue : ne récupérer que le passage.
    pub cut: bool,
}

/// Extraits gardés dans l'historique (envoyés, et reçus).
const RECENTS: usize = 20;

fn now_s() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn received_file(state: &AppState) -> PathBuf {
    clips_dir(state).join("recus").join("index.json")
}

fn recents_file(state: &AppState) -> PathBuf {
    clips_dir(state).join("recents.json")
}

fn load_list(file: &Path) -> Vec<Recent> {
    let list: Vec<Recent> = fs::read(file).ok().and_then(|d| serde_json::from_slice(&d).ok()).unwrap_or_default();
    list.into_iter().filter(|r| Path::new(&r.path).exists()).take(RECENTS).collect()
}

fn add_to_list(file: &Path, recent: Recent) {
    let mut list = load_list(file);
    list.retain(|r| r.path != recent.path);
    list.insert(0, recent);
    list.truncate(RECENTS);
    if let Ok(data) = serde_json::to_vec(&list) {
        let _ = fs::write(file, data);
    }
}

fn load_recents(state: &AppState) -> Vec<Recent> {
    load_list(&recents_file(state))
}

/// Lien d'où vient une vidéo préparée (cette session, ou l'historique), et
/// pour un passage seul, où il commence dans la vidéo (ms).
fn link_for(state: &AppState, path: &str) -> Option<(String, Option<u64>)> {
    if let Some(l) = state.clips.links.lock().unwrap().get(path) {
        return Some((l.clone(), state.clips.cuts.lock().unwrap().get(path).copied()));
    }
    load_recents(state).into_iter().find(|r| r.path == path).and_then(|r| Some((r.link?, r.cut_ms)))
}

fn add_recent(state: &AppState, recent: Recent) {
    add_to_list(&recents_file(state), recent);
}

/// Les derniers extraits envoyés (encore sur le disque).
#[tauri::command]
pub fn clip_recents(state: State<AppState>) -> Vec<Recent> {
    load_recents(&state)
}

/// Historique : extraits envoyés et reçus (encore sur le disque).
#[tauri::command]
pub fn clip_history(state: State<AppState>) -> serde_json::Value {
    serde_json::json!({ "sent": load_recents(&state), "received": load_list(&received_file(&state)) })
}

#[derive(Default)]
pub struct Clips {
    pending: Mutex<Option<ClipShow>>,
    last_from: Mutex<HashMap<String, Instant>>,
    /// Fenêtre affichée en ce moment.
    window: Mutex<Option<String>>,
    counter: AtomicU64,
    /// Le son de l'extrait passe par le moteur audio (dans le casque).
    audio_on: AtomicBool,
    audio_discord: AtomicBool,
    /// Fichier préparé → lien d'où il vient.
    links: Mutex<HashMap<String, String>>,
    /// Fichier préparé qui n'est qu'un passage → où il commence (ms).
    cuts: Mutex<HashMap<String, u64>>,
    /// Dernier « Tout arrêter » de chaque pote : un extrait encore en train
    /// d'arriver de sa part ne s'affiche plus.
    stopped: Mutex<HashMap<String, Instant>>,
    /// Niveau du passage écouté (fichier, début, fin) : « Écouter » et
    /// « Aperçu » relancés partent tout de suite.
    levels: Mutex<Vec<((PathBuf, u64, u64), crate::config::Analysis)>>,
}

/// Le fichier à envoyer, rangé dans le dossier d'envoi. Un extrait reçu
/// (onglet « Historique ») y est d'abord copié, avec son son à part et son
/// lien d'origine, pour pouvoir le renvoyer.
fn sendable(state: &AppState, media: &str) -> Result<PathBuf, String> {
    let media = PathBuf::from(media);
    let dir = clips_dir(state).join("envoi");
    let received = clips_dir(state).join("recus");
    let from = media.parent();
    if from == Some(dir.as_path()) {
        return Ok(media);
    }
    if from != Some(received.as_path()) || !media.exists() {
        return Err("choisis d'abord une image ou une vidéo".into());
    }
    let name = media.file_name().ok_or("fichier introuvable")?;
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let copy = dir.join(name);
    if !copy.exists() {
        fs::copy(&media, &copy).map_err(|e| e.to_string())?;
    }
    let side = video_sound(&received, &media);
    if side != media {
        if let Some(n) = side.file_name() {
            let _ = fs::copy(&side, dir.join(n));
        }
    }
    let key = media.to_string_lossy().into_owned();
    if let Some(r) = load_list(&received_file(state)).into_iter().find(|r| r.path == key) {
        let copy_key = copy.to_string_lossy().into_owned();
        if let Some(cut) = r.cut_ms {
            state.clips.cuts.lock().unwrap().insert(copy_key.clone(), cut);
        }
        if let Some(link) = r.link {
            state.clips.links.lock().unwrap().insert(copy_key, link);
        }
    }
    Ok(copy)
}

/// La vidéo de ce lien, si on l'a déjà (envoyée ou reçue) : pas besoin de
/// la retélécharger. Renvoie le fichier et son son à part.
fn already_have(state: &AppState, link: &str) -> Option<(PathBuf, Option<PathBuf>)> {
    // Seulement une vidéo entière (pas un passage seul).
    let cuts = state.clips.cuts.lock().unwrap().clone();
    let from_session: Vec<String> = state
        .clips
        .links
        .lock()
        .unwrap()
        .iter()
        .filter(|(p, l)| *l == link && !cuts.contains_key(*p))
        .map(|(p, _)| p.clone())
        .collect();
    let from_lists = load_recents(state).into_iter().chain(load_list(&received_file(state)));
    let media = from_session
        .into_iter()
        .chain(from_lists.filter(|r| r.link.as_deref() == Some(link) && r.cut_ms.is_none()).map(|r| r.path))
        .map(PathBuf::from)
        .find(|p| p.exists())?;
    let side = video_sound(media.parent()?, &media);
    Some((media.clone(), (side != media).then_some(side)))
}

fn kind_of(ext: &str) -> Option<&'static str> {
    if IMAGES.contains(&ext) {
        Some("image")
    } else if VIDEOS.contains(&ext) {
        Some("video")
    } else {
        None
    }
}

fn pack(header: &Header, media: &[u8], sound: &[u8]) -> Vec<u8> {
    let h = serde_json::to_vec(header).unwrap_or_default();
    let mut out = Vec::with_capacity(MAGIC.len() + 4 + h.len() + media.len() + sound.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&(h.len() as u32).to_le_bytes());
    out.extend_from_slice(&h);
    out.extend_from_slice(media);
    out.extend_from_slice(sound);
    out
}

/// Sépare un extrait reçu ; refuse tout ce qui n'est pas attendu.
fn unpack(data: &[u8]) -> Result<(Header, &[u8], &[u8]), String> {
    let bad = || "extrait abîmé".to_string();
    let rest = data.strip_prefix(MAGIC).ok_or_else(bad)?;
    let len = u32::from_le_bytes(rest.get(..4).ok_or_else(bad)?.try_into().unwrap()) as usize;
    let h: Header = serde_json::from_slice(rest.get(4..4 + len).ok_or_else(bad)?).map_err(|_| bad())?;
    let body = &rest[4 + len..];
    if h.media_len.saturating_add(h.sound_len) != body.len() as u64 || kind_of(&h.media_ext).is_none() {
        return Err(bad());
    }
    if h.sound_len > 0 && !AUDIO_EXTENSIONS.contains(&h.sound_ext.as_str()) {
        return Err(bad());
    }
    let (media, sound) = body.split_at(h.media_len as usize);
    Ok((h, media, sound))
}

fn clips_dir(state: &AppState) -> PathBuf {
    state.sounds_dir.with_file_name("extraits")
}

/// Place maximale d'un dossier d'extraits (envoyés ou reçus).
const DIR_MAX_BYTES: u64 = 200 * 1024 * 1024;

/// Garde au plus `keep` fichiers dans un dossier, et pas plus de
/// `DIR_MAX_BYTES` en tout : les plus anciens partent d'abord.
fn trim_dir(dir: &Path, keep: usize) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = entries
        .flatten()
        .filter(|e| e.path().extension().is_none_or(|x| x != "json"))
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            Some((m.modified().ok()?, m.len(), e.path()))
        })
        .collect();
    files.sort();
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    let mut count = files.len();
    for (_, len, p) in &files {
        if count <= keep && total <= DIR_MAX_BYTES {
            break;
        }
        let _ = fs::remove_file(p);
        count -= 1;
        total -= len;
    }
}

#[derive(Serialize)]
pub struct ClipPick {
    path: String,
    kind: &'static str,
    size: u64,
    /// Titre de la vidéo (lien), pour proposer un texte.
    title: String,
    /// Piste audio à part (Shorts) : rangée à côté, « <nom>-son.<ext> ».
    has_audio: bool,
    /// Seul un passage a été récupéré (vidéo longue) : il commence à 0.
    cut: bool,
}

/// Image, GIF ou vidéo choisie sur le PC, copiée dans le dossier de l'app
/// (pour l'aperçu).
#[tauri::command]
pub async fn pick_clip_media(app: AppHandle) -> Result<Option<ClipPick>, String> {
    let file = app
        .dialog()
        .file()
        .set_title("Choisir une image, un GIF ou une vidéo")
        .add_filter("Images, GIF et vidéos", &["png", "jpg", "jpeg", "gif", "webp", "mp4", "webm"])
        .blocking_pick_file();
    let Some(path) = file.and_then(|f| f.into_path().ok()) else { return Ok(None) };
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    kind_of(&ext).ok_or("choisis une image (PNG, JPG, GIF, WebP) ou une vidéo (MP4, WebM)")?;
    let size = fs::metadata(&path).map_err(|e| e.to_string())?.len();
    if size > MAX_MEDIA {
        return Err(format!("fichier trop lourd ({:.1} Mo) : 14 Mo au plus", size as f64 / 1048576.0));
    }
    let data = fs::read(&path).map_err(|e| e.to_string())?;
    keep_for_sending(&app, &data, &ext, String::new(), None).map(Some)
}

/// Range l'image ou la vidéo choisie dans le dossier de l'app (pour l'aperçu
/// et l'envoi).
fn keep_for_sending(
    app: &AppHandle,
    data: &[u8],
    ext: &str,
    title: String,
    audio: Option<(Vec<u8>, String)>,
) -> Result<ClipPick, String> {
    let kind = kind_of(ext).ok_or("format non pris en charge")?;
    let audio_len = audio.as_ref().map_or(0, |(a, _)| a.len());
    let total = data.len() + audio_len;
    if total as u64 > MAX_MEDIA {
        return Err(format!("fichier trop lourd ({:.1} Mo) : 14 Mo au plus", total as f64 / 1048576.0));
    }
    let state = app.state::<AppState>();
    let dir = clips_dir(&state).join("envoi");
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    trim_dir(&dir, 40);
    let id = uuid::Uuid::new_v4().simple().to_string();
    let copy = dir.join(format!("{id}.{ext}"));
    fs::write(&copy, data).map_err(|e| e.to_string())?;
    let has_audio = audio.is_some();
    if let Some((a, aext)) = audio {
        fs::write(dir.join(format!("{id}-son.{aext}")), a).map_err(|e| e.to_string())?;
    }
    Ok(ClipPick { path: copy.to_string_lossy().into_owned(), kind, size: total as u64, title, has_audio, cut: false })
}

/// Fichier envoyé depuis la page du téléphone.
pub fn from_upload(app: &AppHandle, ext: &str, data: &[u8]) -> Result<ClipPick, String> {
    let ext = match ext {
        "jpeg" => "jpg",
        "mov" | "m4v" => return Err("vidéo iPhone (MOV) non prise en charge : envoie un MP4, ou un lien".into()),
        e => e,
    };
    kind_of(ext).ok_or("choisis une image (PNG, JPG, GIF, WebP) ou une vidéo (MP4, WebM)")?;
    keep_for_sending(app, data, ext, String::new(), None)
}

/// GIF choisi dans la recherche GIPHY.
#[tauri::command]
pub async fn clip_from_gif(app: AppHandle, url: String) -> Result<ClipPick, String> {
    let (data, ext) = giphy::download(&url).await?;
    keep_for_sending(&app, &data, ext, String::new(), None)
}

/// Vidéos YouTube trouvées par mots-clés, pour l'onglet « Lien vidéo ».
#[tauri::command]
pub async fn clip_search_videos(app: AppHandle, query: String, start: Option<u32>) -> Result<Vec<youtube::Found>, String> {
    // Même recherche refaite (fenêtre rouverte, téléphone) : réponse tout de
    // suite plutôt que de relancer yt-dlp, pendant 10 minutes.
    static CACHE: Mutex<Vec<(String, u32, Instant, Vec<youtube::Found>)>> = Mutex::new(Vec::new());
    let key = query.trim().to_lowercase();
    let start = start.unwrap_or(1);
    {
        let mut cache = CACHE.lock().unwrap();
        cache.retain(|c| c.2.elapsed() < Duration::from_secs(600));
        if let Some(c) = cache.iter().find(|c| c.0 == key && c.1 == start) {
            return Ok(c.3.clone());
        }
    }
    let state = app.state::<AppState>();
    let data_dir = state.sounds_dir.parent().map(Path::to_path_buf).unwrap_or_else(|| state.sounds_dir.clone());
    let status = |msg: &str| {
        let _ = app.emit("youtube-status", msg);
    };
    let exe = youtube::ensure(&data_dir.join("tools"), status).await?;
    let found = tauri::async_runtime::spawn_blocking(move || youtube::search(&exe, &query, start, 12))
        .await
        .map_err(|e| e.to_string())??;
    if !found.is_empty() {
        let mut cache = CACHE.lock().unwrap();
        if cache.len() >= 30 {
            cache.remove(0);
        }
        cache.push((key, start, Instant::now(), found.clone()));
    }
    Ok(found)
}

/// Vidéo d'un lien (YouTube, TikTok, X, Reddit…), avec yt-dlp.
/// `start_s` : début du passage voulu. `cut` : vidéo longue, on ne récupère
/// que ce passage (avec ffmpeg). Sans `cut`, la vidéo entière est récupérée,
/// et si elle est trop lourde, seulement le passage qui commence à `start_s`.
#[tauri::command]
pub async fn clip_from_link(app: AppHandle, url: String, start_s: Option<f64>, cut: Option<bool>) -> Result<ClipPick, String> {
    let state = app.state::<AppState>();
    if !youtube::is_web_link(&url) {
        return Err("colle un lien qui commence par https://".into());
    }
    if state.youtube_busy.swap(true, Ordering::SeqCst) {
        return Err("un téléchargement de vidéo est déjà en cours".into());
    }
    let res = async {
        let data_dir = state.sounds_dir.parent().map(Path::to_path_buf).unwrap_or_else(|| state.sounds_dir.clone());
        let status = |msg: &str| {
            let _ = app.emit("youtube-status", msg);
        };
        let tools = data_dir.join("tools");
        let exe = youtube::ensure(&tools, status).await?;
        let work_dir = data_dir.join("extrait-tmp");
        let start = start_s.unwrap_or(0.0).clamp(0.0, 36_000.0);
        let fetch = |ffmpeg: Option<PathBuf>| {
            let (exe, url, wd, app2) = (exe.clone(), url.clone(), work_dir.clone(), app.clone());
            tauri::async_runtime::spawn_blocking(move || {
                youtube::download_video(&exe, &url, &wd, ffmpeg.as_deref().map(|f| (start, f)), |p| {
                    let _ = app2.emit("clip-progress", p.round() as u32);
                })
            })
        };
        let mut was_cut = cut.unwrap_or(false);
        let got = if was_cut {
            let ffmpeg = youtube::ensure_ffmpeg(&tools, status).await?;
            fetch(Some(ffmpeg)).await.map_err(|e| e.to_string())?
        } else {
            match fetch(None).await.map_err(|e| e.to_string())? {
                // Trop lourde entière : on ne prend que le passage.
                Err(e) if e == youtube::TOO_BIG => {
                    status("Vidéo longue : on ne récupère que le passage (40 s)…");
                    let ffmpeg = youtube::ensure_ffmpeg(&tools, status).await?;
                    was_cut = true;
                    fetch(Some(ffmpeg)).await.map_err(|e| e.to_string())?
                }
                other => other,
            }
        };
        youtube::update_in_background(&exe);
        let got = got?;
        let ext_of = |p: &Path| p.extension().and_then(|e| e.to_str()).unwrap_or("mp4").to_lowercase();
        let ext = ext_of(&got.video);
        let read = || -> Result<(Vec<u8>, Option<(Vec<u8>, String)>), String> {
            let video = fs::read(&got.video).map_err(|e| e.to_string())?;
            let audio = match &got.audio {
                Some(a) => Some((fs::read(a).map_err(|e| e.to_string())?, ext_of(a))),
                None => None,
            };
            Ok((video, audio))
        };
        let files = read();
        let _ = fs::remove_dir_all(&work_dir);
        let (video, audio) = files?;
        let mut pick = keep_for_sending(&app, &video, &ext, got.title, audio)?;
        pick.cut = was_cut;
        // Un passage seul part aussi par lien : le pote récupère le même
        // passage (ffmpeg chez lui aussi), sans passer par le relais.
        state.clips.links.lock().unwrap().insert(pick.path.clone(), url.clone());
        if was_cut {
            state.clips.cuts.lock().unwrap().insert(pick.path.clone(), (start * 1000.0) as u64);
        }
        Ok(pick)
    }
    .await;
    state.youtube_busy.store(false, Ordering::SeqCst);
    res
}

/// Fin du passage choisi (30 s au plus après le début) ; `None` = jusqu'au bout.
fn video_end(start_ms: u64, end_s: Option<f64>) -> Option<u64> {
    let end_ms = (end_s?.clamp(0.0, 36_030.0) * 1000.0) as u64;
    (end_ms > start_ms).then(|| end_ms.min(start_ms + 30_000))
}

/// Le son d'une vidéo prête à l'envoi : à part (« <nom>-son.m4a ») ou dans la vidéo.
fn video_sound(dir: &Path, media: &Path) -> PathBuf {
    let stem = media.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
    ["m4a", "mp3", "aac"].iter().map(|e| dir.join(format!("{stem}-son.{e}"))).find(|p| p.exists()).unwrap_or_else(|| media.to_path_buf())
}

/// « Écouter » avant d'envoyer : joue chez soi (casque) le son tel que le pote
/// l'entendra (même passage, même volume, même réglage du niveau).
#[tauri::command]
pub async fn clip_listen(
    app: AppHandle,
    media: String,
    sound_id: Option<String>,
    start_s: Option<f64>,
    end_s: Option<f64>,
    volume: Option<f32>,
) -> Result<(), String> {
    let state = app.state::<AppState>();
    let media = sendable(&state, &media)?;
    let dir = clips_dir(&state).join("envoi");
    let ext = media.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    let (path, start_ms, end_ms, vol) = match sound_id.filter(|s| !s.is_empty()) {
        Some(id) => {
            let s = find_sound(&state, &id)?;
            (s.path.clone(), s.start_ms, s.end_ms, s.volume)
        }
        None if kind_of(&ext) == Some("video") => {
            let start_ms = (start_s.unwrap_or(0.0).clamp(0.0, 36_000.0) * 1000.0) as u64;
            (video_sound(&dir, &media), start_ms, video_end(start_ms, end_s), volume.unwrap_or(1.0))
        }
        None => return Err("pas de son à écouter".into()),
    };
    play_audio(app.clone(), path, start_ms, end_ms, vol, false).await
}

/// « Envoyer à » : tout le salon, et soi-même aussi pour tester.
const ME_TOO: &str = "*moi";

/// Envoie l'extrait à un pote (ou à tout le salon si `to` est vide).
#[tauri::command]
pub async fn send_clip(
    app: AppHandle,
    media: String,
    sound_id: Option<String>,
    title: String,
    to: Option<String>,
    start_s: Option<f64>,
    end_s: Option<f64>,
    volume: Option<f32>,
) -> Result<(), String> {
    let state = app.state::<AppState>();
    let media = sendable(&state, &media)?;
    let dir = clips_dir(&state).join("envoi");
    let media_ext = media.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    kind_of(&media_ext).ok_or("format non pris en charge")?;
    let path_key = media.to_string_lossy().into_owned();
    // Par lien seulement si tous ceux qui reçoivent ont une appli qui sait
    // le faire : une version d'avant ne recevrait rien (envoi muet).
    let receivers = to.as_deref().filter(|t| *t != ME_TOO).unwrap_or("");
    let link = (kind_of(&media_ext) == Some("video") && sound_id.as_deref().unwrap_or("").is_empty())
        .then(|| link_for(&state, &path_key))
        .flatten()
        .filter(|(l, _)| youtube::is_shareable_link(l))
        .filter(|(_, cut)| match cut {
            Some(_) => state.extras.room.cut_links_ok(receivers),
            None => state.extras.room.links_ok(receivers),
        });
    if let Some((link, cut_ms)) = link {
        // Vidéo d'un site : seul le lien et le passage partent.
        let room = &state.extras.room;
        let with_me = to.as_deref() == Some(ME_TOO);
        let to = to.filter(|t| !t.is_empty() && t != ME_TOO).unwrap_or_default();
        if !to.is_empty() && !room.status().members.iter().any(|m| m.id == to) {
            return Err("ce pote n'est plus en ligne".into());
        }
        let title: String = title.trim().chars().take(60).collect();
        let start_ms = (start_s.unwrap_or(0.0).clamp(0.0, 36_000.0) * 1000.0) as u64;
        // Passage seul : les temps sont donnés dans la vidéo du lien.
        let offset = cut_ms.unwrap_or(0);
        let msg = Msg::Clip {
            from: room.my_id(),
            name: room.my_name(),
            to,
            title: title.clone(),
            link: link.clone(),
            start_ms: start_ms + offset,
            end_ms: video_end(start_ms, end_s).map(|e| e + offset),
            volume: Some(volume.unwrap_or(1.0).clamp(0.0, 2.0)),
            cut: cut_ms.is_some(),
        };
        if with_me {
            room.expect_echo();
        }
        room.publish(&msg).await?;
        journal::write("Extrait : lien envoyé (le pote télécharge la vidéo)");
        *state.extras.last_sent.lock().unwrap() = Some(Instant::now());
        add_recent(&state, Recent {
            path: path_key,
            kind: "video".into(),
            title,
            start_s,
            end_s,
            volume,
            sound_id: None,
            from: None,
            sound: None,
            at: now_s(),
            link: Some(link),
            cut_ms,
        });
        return Ok(());
    }
    let media_data = fs::read(&media).map_err(|_| "image ou vidéo introuvable, choisis-la à nouveau".to_string())?;
    let sound = match sound_id.filter(|s| !s.is_empty()) {
        Some(id) => Some(find_sound(&state, &id)?),
        None => None,
    };
    let (sound_data, header) = match &sound {
        Some(s) => {
            let data = fs::read(&s.path).map_err(|e| e.to_string())?;
            let ext = s.path.extension().and_then(|e| e.to_str()).unwrap_or("mp3").to_lowercase();
            let h = Header {
                sound_ext: ext,
                sound_len: data.len() as u64,
                start_ms: s.start_ms,
                end_ms: s.end_ms,
                volume: s.volume,
                ..Default::default()
            };
            (data, h)
        }
        None => {
            // Vidéo : elle peut commencer plus loin que le début, et son son
            // peut être à part (« <nom>-son.m4a », Shorts YouTube).
            let start_ms = (start_s.unwrap_or(0.0).clamp(0.0, 36_000.0) * 1000.0) as u64;
            let video_volume = volume.unwrap_or(1.0).clamp(0.0, 2.0);
            let end_ms = video_end(start_ms, end_s);
            let stem = media.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
            let side = ["m4a", "mp3", "aac"].iter().map(|e| dir.join(format!("{stem}-son.{e}"))).find(|p| p.exists());
            // Une image reçue avec son son (renvoyée depuis l'historique) le garde.
            match side {
                Some(p) => {
                    let data = fs::read(&p).map_err(|e| e.to_string())?;
                    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("m4a").to_string();
                    let h = Header { sound_ext: ext, sound_len: data.len() as u64, start_ms, volume: video_volume, end_ms, ..Default::default() };
                    (data, h)
                }
                None => (Vec::new(), Header { volume: video_volume, start_ms, end_ms, ..Default::default() }),
            }
        }
    };
    let header = Header { media_ext, media_len: media_data.len() as u64, ..header };
    let data = pack(&header, &media_data, &sound_data);
    if data.len() as u64 > MAX_CLIP {
        return Err("l'extrait et son son font plus de 14 Mo ensemble : prends plus court".into());
    }
    let room = &state.extras.room;
    let with_me = to.as_deref() == Some(ME_TOO);
    let to = to.filter(|t| !t.is_empty() && t != ME_TOO).unwrap_or_default();
    if !to.is_empty() && !room.status().members.iter().any(|m| m.id == to) {
        return Err("ce pote n'est plus en ligne".into());
    }
    let title: String = title.trim().chars().take(60).collect();
    let msg_title = title.clone();
    let to_all = to.is_empty();
    let msg = Msg::Clip { from: room.my_id(), name: room.my_name(), to, title, link: String::new(), start_ms: 0, end_ms: None, volume: None, cut: false };
    if with_me {
        room.expect_echo();
    }
    let mb = data.len() as f64 / 1_048_576.0;
    journal::write(&format!("Extrait : envoi de {mb:.1} Mo{}", if to_all { " au salon" } else { "" }));
    let sent = Instant::now();
    // Le même extrait renvoyé (à un autre pote, ou depuis l'historique)
    // ne repart pas : le relais l'a encore.
    let key = format!("clip-{}", crate::share::key_of(&data));
    if let Err(e) = room.send_file(&msg, &key, "extrait.sbclip", || Ok(data)).await {
        journal::write(&format!("Extrait : envoi raté après {} s ({e})", sent.elapsed().as_secs()));
        return Err(e);
    }
    journal::write(&format!("Extrait : envoyé en {} s", sent.elapsed().as_secs()));
    *state.extras.last_sent.lock().unwrap() = Some(Instant::now());
    let is_video = kind_of(&media.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase()) == Some("video");
    add_recent(&state, Recent {
        path: media.to_string_lossy().into_owned(),
        kind: if is_video { "video" } else { "image" }.into(),
        title: msg_title,
        start_s: start_s.filter(|_| is_video),
        end_s: end_s.filter(|_| is_video),
        volume: volume.filter(|_| is_video),
        sound_id: sound.map(|s| s.id),
        from: None,
        sound: None,
        at: now_s(),
        link: None,
        cut_ms: None,
    });
    Ok(())
}

/// Télécharge la vidéo d'un extrait reçu par lien, rangée dans `dir` sous
/// `<id>.<ext>` (et `<id>-son.<ext>` quand le son est à part). Si la vidéo
/// entière est trop lourde ici (formats différents chez le pote) et que
/// ffmpeg est déjà là, seul le passage est pris : le dernier élément dit
/// alors où commence le fichier (ms) dans la vidéo d'origine.
async fn fetch_link(app: &AppHandle, url: &str, dir: &Path, id: &str, start_ms: u64, cut: bool) -> Result<(PathBuf, Option<PathBuf>, u64), String> {
    if !youtube::is_shareable_link(url) {
        return Err("lien non accepté".into());
    }
    let state = app.state::<AppState>();
    let data_dir = state.sounds_dir.parent().map(Path::to_path_buf).unwrap_or_else(|| state.sounds_dir.clone());
    let tools = data_dir.join("tools");
    let exe = youtube::ensure(&tools, |_| {}).await?;
    // Vidéo longue : ffmpeg (installé la première fois) pour n'en prendre
    // que le passage.
    let ffmpeg = if cut { Some(youtube::ensure_ffmpeg(&tools, |_| {}).await?) } else { None };
    let work = data_dir.join(format!("extrait-recu-{id}"));
    let (url, dir, id) = (url.to_string(), dir.to_path_buf(), id.to_string());
    let rt = tauri::async_runtime::handle();
    tauri::async_runtime::spawn_blocking(move || {
        let section = |ff: &Path| youtube::download_video(&exe, &url, &work, Some((start_ms as f64 / 1000.0, ff)), |_| {});
        let mut offset = 0;
        let mut res = match &ffmpeg {
            Some(ff) => {
                offset = start_ms;
                section(ff)
            }
            None => youtube::download_video(&exe, &url, &work, None, |_| {}),
        };
        if ffmpeg.is_none() && res.as_ref().err().is_some_and(|e| e == youtube::TOO_BIG) {
            // Trop lourde entière ici : le passage seul.
            if let Ok(ff) = rt.block_on(youtube::ensure_ffmpeg(&tools, |_| {})) {
                offset = start_ms;
                res = section(&ff);
            }
        }
        let res = res.and_then(|got| {
            let ext = |p: &Path| p.extension().and_then(|e| e.to_str()).unwrap_or("mp4").to_lowercase();
            let media = dir.join(format!("{id}.{}", ext(&got.video)));
            fs::copy(&got.video, &media).map_err(|e| e.to_string())?;
            let sound = match &got.audio {
                Some(a) => {
                    let p = dir.join(format!("{id}-son.{}", ext(a)));
                    fs::copy(a, &p).map_err(|e| e.to_string())?;
                    Some(p)
                }
                None => None,
            };
            Ok((media, sound, offset))
        });
        let _ = fs::remove_dir_all(&work);
        res
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Un extrait arrive d'un pote.
pub async fn receive(
    app: &AppHandle,
    from: String,
    name: String,
    title: String,
    url: Option<String>,
    direct: bool,
    link: Option<LinkClip>,
) -> Result<(), String> {
    let state = app.state::<AppState>();
    let mode = state.config.lock().unwrap().settings.clips_mode.clone();
    if mode == "off" {
        return Ok(());
    }
    if extras::is_blocked(&state, &from) {
        journal::write(&format!("Extrait de {name} ignoré (bloqué)"));
        return Ok(());
    }
    if url.is_none() && link.is_none() {
        return Ok(());
    }
    {
        let mut last = state.clips.last_from.lock().unwrap();
        if last.get(&from).is_some_and(|t| t.elapsed() < SPAM_GAP) {
            return Ok(());
        }
        last.insert(from.clone(), Instant::now());
    }
    let dir = clips_dir(&state).join("recus");
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    trim_dir(&dir, 40);
    let id = uuid::Uuid::new_v4().simple().to_string();
    let got = Instant::now();
    let link_url = link.as_ref().map(|l| l.url.clone());
    let mut cut_ms = None;
    let (kind, media_path, sound_path, start_ms, end_ms, volume) = match (link, url) {
        (Some(l), _) => {
            // Vidéo déjà là (son propre extrait « moi compris », ou déjà
            // reçue) : copiée tout de suite au lieu d'être retéléchargée.
            let have = already_have(&state, &l.url).and_then(|(media, sound)| {
                let ext = |p: &Path| p.extension().and_then(|e| e.to_str()).unwrap_or("mp4").to_lowercase();
                let m = dir.join(format!("{id}.{}", ext(&media)));
                fs::copy(&media, &m).ok()?;
                let s = match sound {
                    Some(s) => {
                        let p = dir.join(format!("{id}-son.{}", ext(&s)));
                        fs::copy(&s, &p).ok()?;
                        Some(p)
                    }
                    None => None,
                };
                Some((m, s))
            });
            let (media, sound, offset) = match have {
                Some((m, s)) => (m, s, 0),
                None => match fetch_link(app, &l.url, &dir, &id, l.start_ms, l.cut).await {
                    Ok(f) => {
                        journal::write(&format!("Extrait de {name} : vidéo téléchargée par lien en {} s", got.elapsed().as_secs()));
                        f
                    }
                    Err(e) => {
                        journal::write(&format!("Extrait de {name} : vidéo du lien pas récupérée ({e})"));
                        return Err(e);
                    }
                },
            };
            // Passage seul : il commence au début du fichier.
            cut_ms = (offset > 0).then_some(offset);
            ("video", media, sound, l.start_ms - offset, l.end_ms.map(|e| e.saturating_sub(offset)), l.volume)
        }
        (None, Some(url)) => {
            let data = match room::fetch(&url).await {
                Ok(d) => d,
                Err(e) => {
                    journal::write(&format!("Extrait de {name} : réception ratée après {} s ({e})", got.elapsed().as_secs()));
                    return Err(e);
                }
            };
            journal::write(&format!("Extrait de {name} reçu ({:.1} Mo en {} s)", data.len() as f64 / 1_048_576.0, got.elapsed().as_secs()));
            let (h, media, sound) = unpack(&data)?;
            let media_path = dir.join(format!("{id}.{}", h.media_ext));
            fs::write(&media_path, media).map_err(|e| e.to_string())?;
            let sound_path = if sound.is_empty() {
                None
            } else {
                let p = dir.join(format!("{id}-son.{}", h.sound_ext));
                fs::write(&p, sound).map_err(|e| e.to_string())?;
                Some(p)
            };
            (kind_of(&h.media_ext).unwrap_or("image"), media_path, sound_path, h.start_ms, h.end_ms, h.volume)
        }
        (None, None) => return Ok(()),
    };
    // Arrêté par le pote (« Tout arrêter ») pendant que ça arrivait.
    if state.clips.stopped.lock().unwrap().get(&from).is_some_and(|t| *t >= got) {
        journal::write(&format!("Extrait de {name} : arrêté par lui avant d'arriver"));
        return Ok(());
    }
    let name: String = name.chars().take(32).collect();
    let title: String = title.chars().take(60).collect();
    let show = ClipShow {
        from_id: from.clone(),
        from: name,
        title,
        kind,
        media: media_path.to_string_lossy().into_owned(),
        sound: sound_path.map(|p| p.to_string_lossy().into_owned()),
        start_ms,
        end_ms,
        volume: volume.clamp(0.0, 2.0),
        ask: mode != "auto",
        preview: false,
        direct,
    };
    add_to_list(&received_file(&state), Recent {
        path: show.media.clone(),
        kind: show.kind.into(),
        title: show.title.clone(),
        start_s: (show.kind == "video").then(|| show.start_ms as f64 / 1000.0),
        end_s: show.end_ms.map(|e| e as f64 / 1000.0),
        volume: Some(show.volume),
        sound_id: None,
        from: Some(show.from.clone()),
        sound: show.sound.clone(),
        at: now_s(),
        link: link_url,
        cut_ms,
    });
    let _ = app.emit("clip-received", (&show.from, &show.title));
    show_window(app, show)
}

/// Taille de la fenêtre : petite carte pour demander, plus grande pour l'extrait.
fn window_size(ask: bool) -> (f64, f64) {
    if ask { (380.0, 132.0) } else { (500.0, 380.0) }
}

/// Coin en bas à droite de l'écran principal, au-dessus de la barre des
/// tâches (en pixels logiques).
fn corner(app: &AppHandle, ask: bool) -> Option<(f64, f64)> {
    let (width, height) = window_size(ask);
    let m = app.primary_monitor().ok().flatten()?;
    let scale = m.scale_factor();
    let area = m.work_area();
    let x = (area.position.x as f64 + area.size.width as f64) / scale - width - 20.0;
    let y = (area.position.y as f64 + area.size.height as f64) / scale - height - 20.0;
    Some((x, y))
}

/// Taille et place de la fenêtre déjà ouverte. (Pas de show/hide après la
/// création : sous Windows, `show()` active la fenêtre et sortirait le
/// pote de son jeu ; on ferme la fenêtre à la fin et on la recrée.)
fn place(app: &AppHandle, w: &tauri::WebviewWindow, ask: bool) {
    let (width, height) = window_size(ask);
    let _ = w.set_size(LogicalSize::new(width, height));
    if let Some((x, y)) = corner(app, ask) {
        let _ = w.set_position(tauri::LogicalPosition::new(x, y));
    }
}

fn show_window(app: &AppHandle, show: ClipShow) -> Result<(), String> {
    let state = app.state::<AppState>();
    let ask = show.ask;
    *state.clips.pending.lock().unwrap() = Some(show.clone());
    let current = state.clips.window.lock().unwrap().clone();
    if let Some(w) = current.as_deref().and_then(|l| app.get_webview_window(l)) {
        place(app, &w, ask);
        return app.emit_to(w.label(), "clip-show", &show).map_err(|e| e.to_string());
    }
    // Une fenêtre neuve à chaque fois : on ne réutilise jamais une fenêtre
    // en train de se fermer.
    let label = format!("{WINDOW_PREFIX}{}", state.clips.counter.fetch_add(1, Ordering::SeqCst) + 1);
    let (width, height) = window_size(ask);
    let mut builder = WebviewWindowBuilder::new(app, &label, WebviewUrl::App("clip.html".into()))
        .title("Soundboard — extrait")
        .inner_size(width, height)
        .decorations(false)
        .transparent(true)
        .shadow(false)
        .resizable(false)
        .always_on_top(true)
        .skip_taskbar(true)
        // Créée visible mais sans le focus (affichée sans être activée), et
        // cliquer dessus ne retire pas la main au jeu.
        .focusable(false)
        .focused(false)
        .visible(true);
    if let Some((x, y)) = corner(app, ask) {
        builder = builder.position(x, y);
    }
    // La fenêtre se charge puis lit l'extrait en attente (clip_pending).
    builder.build().map_err(|e| e.to_string())?;
    *state.clips.window.lock().unwrap() = Some(label);
    Ok(())
}

/// Joue le son de l'extrait (piste audio de la vidéo, ou son de l'image)
/// dans le casque choisi dans les réglages, comme une pré-écoute. La
/// fenêtre, elle, passerait par la sortie par défaut de Windows, qui n'est
/// pas toujours le casque. En cas d'échec (vidéo WebM en Opus…), la fenêtre
/// joue le son elle-même.
#[tauri::command]
pub async fn clip_audio(app: AppHandle, path: String, start_ms: u64, end_ms: Option<u64>, volume: f32) -> Result<(), String> {
    let state = app.state::<AppState>();
    let path = PathBuf::from(path);
    let dir = clips_dir(&state);
    if path.parent() != Some(dir.join("recus").as_path()) && path.parent() != Some(dir.join("envoi").as_path()) {
        return Err("fichier non autorisé".into());
    }
    // Extrait reçu d'un pote : son son part aussi dans Discord, comme un son
    // du salon, pour que tout le monde l'entende. L'aperçu reste au casque.
    let discord = state.clips.pending.lock().unwrap().as_ref().is_some_and(|c| !c.preview && c.direct);
    play_audio(app.clone(), path, start_ms, end_ms, volume, discord).await
}

/// Son d'un extrait reçu, joué dans le casque et dans Discord.
const CLIP_SOUND_ID: &str = "extrait-recu";

async fn play_audio(app: AppHandle, path: PathBuf, start_ms: u64, end_ms: Option<u64>, volume: f32, discord: bool) -> Result<(), String> {
    let app2 = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let state = app2.state::<AppState>();
        // Seul le passage joué est décodé et mesuré (pas toute la vidéo,
        // qui peut durer des minutes) : moins de calcul, pas d'à-coup.
        let end = end_ms.unwrap_or(u64::MAX).min(start_ms + 30_500);
        let cache_key = (path.clone(), start_ms, end);
        let known = state.clips.levels.lock().unwrap().iter().find(|(k, _)| *k == cache_key).map(|(_, a)| *a);
        let analysis = match known {
            Some(a) => a,
            None => {
                let key = crate::config::SegmentKey { start_ms, end_ms: Some(end), fx: Default::default() };
                let mut a = analysis::analyze_segment(&path, &key)?;
                a.duration_ms += start_ms;
                let mut levels = state.clips.levels.lock().unwrap();
                if levels.len() >= 16 {
                    levels.remove(0);
                }
                levels.push((cache_key, a));
                a
            }
        };
        let mut sound = extras::store_like(&path, analysis);
        sound.id = audio::PREVIEW_ID.into();
        sound.start_ms = start_ms;
        sound.end_ms = Some(end.min(analysis.duration_ms));
        sound.volume = volume.clamp(0.0, 2.0);
        if discord {
            sound.id = CLIP_SOUND_ID.into();
            state.audio.stop(CLIP_SOUND_ID);
            state.audio.toggle_detached(play_request(&state, &sound));
            state.clips.audio_discord.store(true, Ordering::SeqCst);
            return Ok(());
        }
        let req = play_request(&state, &sound);
        state.audio.preview(req)?;
        state.clips.audio_on.store(true, Ordering::SeqCst);
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// « Aperçu » : montre son propre extrait dans le coin de son écran, comme
/// le pote le verra (sans rien envoyer).
#[tauri::command]
pub async fn clip_preview(
    app: AppHandle,
    media: String,
    sound_id: Option<String>,
    title: String,
    start_s: Option<f64>,
    end_s: Option<f64>,
    volume: Option<f32>,
) -> Result<(), String> {
    let state = app.state::<AppState>();
    let media = sendable(&state, &media)?;
    let dir = clips_dir(&state).join("envoi");
    if !media.exists() {
        return Err("choisis d'abord une image ou une vidéo".into());
    }
    let ext = media.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    let kind = kind_of(&ext).ok_or("format non pris en charge")?;
    let (sound, start_ms, end_ms, volume) = if kind == "video" {
        let start_ms = (start_s.unwrap_or(0.0).clamp(0.0, 36_000.0) * 1000.0) as u64;
        let side = video_sound(&dir, &media);
        let sound = (side != media).then(|| side.to_string_lossy().into_owned());
        (sound, start_ms, video_end(start_ms, end_s), volume.unwrap_or(1.0).clamp(0.0, 2.0))
    } else {
        match sound_id.filter(|s| !s.is_empty()) {
            Some(id) => {
                // Copié à côté : la fenêtre ne lit que le dossier des extraits.
                let s = find_sound(&state, &id)?;
                let sext = s.path.extension().and_then(|e| e.to_str()).unwrap_or("mp3").to_lowercase();
                let copy = dir.join(format!("apercu-son.{sext}"));
                fs::copy(&s.path, &copy).map_err(|e| e.to_string())?;
                (Some(copy.to_string_lossy().into_owned()), s.start_ms, s.end_ms, s.volume)
            }
            None => (None, 0, None, 1.0),
        }
    };
    close(&app);
    let show = ClipShow {
        from: "Aperçu".into(),
        title: title.trim().chars().take(60).collect(),
        kind,
        media: media.to_string_lossy().into_owned(),
        sound,
        start_ms,
        end_ms,
        volume,
        from_id: String::new(),
        ask: false,
        preview: true,
        direct: false,
    };
    show_window(&app, show)
}

/// « Garder » : l'extrait reçu devient un pad (son + image).
#[tauri::command]
pub async fn clip_keep(app: AppHandle) -> Result<String, String> {
    let state = app.state::<AppState>();
    let c = state.clips.pending.lock().unwrap().clone().ok_or("l'extrait est déjà fermé")?;
    if c.preview {
        return Err("c'est ton propre extrait".into());
    }
    let media = PathBuf::from(&c.media);
    let ext_of = |p: &Path| p.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    // Le son : à part, ou celui de la vidéo (MP4 = même conteneur que M4A).
    let (sound_path, sound_ext) = match &c.sound {
        Some(p) => (PathBuf::from(p), ext_of(Path::new(p))),
        None if c.kind == "video" && ext_of(&media) == "mp4" => (media.clone(), "m4a".to_string()),
        None if c.kind == "video" => return Err("le son de cette vidéo (WebM) ne peut pas devenir un pad".into()),
        None => return Err("cet extrait n'a pas de son à garder".into()),
    };
    let data = fs::read(&sound_path).map_err(|_| "l'extrait a déjà été effacé".to_string())?;
    let name = if c.title.trim().is_empty() { format!("Extrait de {}", c.from) } else { c.title.trim().to_string() };
    let sounds_dir = state.sounds_dir.clone();
    let index = state.config.lock().unwrap().sounds.len();
    let mut sound = tauri::async_runtime::spawn_blocking(move || store_sound(&data, &sound_ext, name, &sounds_dir, index))
        .await
        .map_err(|e| e.to_string())??;
    sound.start_ms = c.start_ms;
    sound.end_ms = c.end_ms.or_else(|| (c.kind == "video").then_some(c.start_ms + 30_000));
    sound.volume = c.volume;
    let id = sound.id.clone();
    let name = sound.name.clone();
    {
        let mut config = state.config.lock().unwrap();
        config.sounds.push(sound);
        state.save(&config)?;
    }
    if c.kind == "image" {
        if let Ok(img) = fs::read(&media) {
            if let Some(ext) = giphy::image_ext(&img) {
                let _ = store_image(&state, &id, (img, ext), None);
            }
        }
    }
    let _ = app.emit("snapshot", state.snapshot());
    journal::write(&format!("Extrait de {} gardé en pad « {name} »", c.from));
    Ok(name)
}

/// L'extrait en attente (lu par la fenêtre quand elle s'ouvre).
#[tauri::command]
pub fn clip_pending(state: State<AppState>) -> Option<ClipShow> {
    state.clips.pending.lock().unwrap().clone()
}

/// « Afficher » : la carte devient l'extrait.
#[tauri::command]
pub fn clip_accept(app: AppHandle) {
    let label = app.state::<AppState>().clips.window.lock().unwrap().clone();
    if let Some(w) = label.and_then(|l| app.get_webview_window(&l)) {
        place(&app, &w, false);
    }
}

/// Ferme l'extrait (fin, refus, ✕ ou « Tout arrêter »).
#[tauri::command]
pub async fn clip_close(app: AppHandle) {
    close(&app);
}

pub fn close(app: &AppHandle) {
    if let Some(state) = app.try_state::<AppState>() {
        state.clips.pending.lock().unwrap().take();
        state.clips.window.lock().unwrap().take();
        if state.clips.audio_on.swap(false, Ordering::SeqCst) {
            state.audio.stop_preview();
        }
        if state.clips.audio_discord.swap(false, Ordering::SeqCst) {
            state.audio.stop(CLIP_SOUND_ID);
        }
    }
    for (label, w) in app.webview_windows() {
        if is_clip_window(&label) {
            let _ = w.destroy();
        }
    }
}

/// Ferme l'extrait s'il vient de ce pote.
pub fn close_from(app: &AppHandle, from: &str) {
    let state = app.state::<AppState>();
    state.clips.stopped.lock().unwrap().insert(from.to_string(), Instant::now());
    let from_him = state.clips.pending.lock().unwrap().as_ref().is_some_and(|c| c.from_id == from);
    if from_him {
        close(app);
    }
}

pub fn is_clip_window(label: &str) -> bool {
    label.starts_with(WINDOW_PREFIX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_end_keeps_15_seconds_at_most() {
        assert_eq!(video_end(42_000, Some(52.0)), Some(52_000));
        assert_eq!(video_end(0, Some(40.0)), Some(30_000));
        assert_eq!(video_end(10_000, Some(5.0)), None);
        assert_eq!(video_end(0, None), None);
    }

    #[test]
    fn trims_oldest_files_but_keeps_json() {
        let dir = std::env::temp_dir().join(format!("sb-trim-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("index.json"), b"[]").unwrap();
        for i in 0..5 {
            fs::write(dir.join(format!("{i}.png")), b"x").unwrap();
            std::thread::sleep(Duration::from_millis(15));
        }
        trim_dir(&dir, 3);
        let mut left: Vec<String> = fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        left.sort();
        assert_eq!(left, ["2.png", "3.png", "4.png", "index.json"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn clips_are_packed_and_unpacked() {
        let h = Header { media_ext: "gif".into(), media_len: 3, sound_ext: "mp3".into(), sound_len: 2, volume: 1.0, ..Default::default() };
        let data = pack(&h, b"GIF", b"ID");
        let (got, media, sound) = unpack(&data).unwrap();
        assert_eq!(got, h);
        assert_eq!(media, b"GIF");
        assert_eq!(sound, b"ID");
    }

    #[test]
    fn strange_clips_are_refused() {
        let h = Header { media_ext: "exe".into(), media_len: 3, ..Default::default() };
        assert!(unpack(&pack(&h, b"MZ!", b"")).is_err());
        let h = Header { media_ext: "png".into(), media_len: 9, ..Default::default() };
        assert!(unpack(&pack(&h, b"PNG", b"")).is_err());
        let h = Header { media_ext: "png".into(), media_len: 3, sound_ext: "bat".into(), sound_len: 1, ..Default::default() };
        assert!(unpack(&pack(&h, b"PNG", b"x")).is_err());
        assert!(unpack(b"bonjour").is_err());
        assert!(unpack(b"SBCLIP1\n\xff\xff\xff\xff").is_err());
    }
}
