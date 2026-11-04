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
pub const MAX_CLIP: u64 = 10 * 1024 * 1024;
/// Taille de l'image ou de la vidéo choisie.
pub const MAX_MEDIA: u64 = 8 * 1024 * 1024;
const MAGIC: &[u8] = b"SBCLIP1\n";
const IMAGES: &[&str] = &["png", "jpg", "jpeg", "gif", "webp"];
const VIDEOS: &[&str] = &["mp4", "webm"];
const WINDOW: &str = "clip";
/// Un extrait par pote toutes les 8 secondes au plus.
const SPAM_GAP: Duration = Duration::from_secs(8);

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
    /// Demander avant d'afficher.
    pub ask: bool,
}

#[derive(Default)]
pub struct Clips {
    pending: Mutex<Option<ClipShow>>,
    last_from: Mutex<HashMap<String, Instant>>,
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

/// Garde au plus `keep` fichiers dans un dossier.
fn trim_dir(dir: &Path, keep: usize) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    if files.len() <= keep {
        return;
    }
    files.sort();
    for (_, p) in files.iter().take(files.len() - keep) {
        let _ = fs::remove_file(p);
    }
}

#[derive(Serialize)]
pub struct ClipPick {
    path: String,
    kind: &'static str,
    size: u64,
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
    let kind = kind_of(&ext).ok_or("choisis une image (PNG, JPG, GIF, WebP) ou une vidéo (MP4, WebM)")?;
    let size = fs::metadata(&path).map_err(|e| e.to_string())?.len();
    if size > MAX_MEDIA {
        return Err(format!("fichier trop lourd ({:.1} Mo) : 8 Mo au plus", size as f64 / 1048576.0));
    }
    let state = app.state::<AppState>();
    let dir = clips_dir(&state).join("envoi");
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    trim_dir(&dir, 10);
    let copy = dir.join(format!("{}.{ext}", uuid::Uuid::new_v4().simple()));
    fs::copy(&path, &copy).map_err(|e| e.to_string())?;
    Ok(Some(ClipPick { path: copy.to_string_lossy().into_owned(), kind, size }))
}

/// Envoie l'extrait à un pote (ou à tout le salon si `to` est vide).
#[tauri::command]
pub async fn send_clip(
    app: AppHandle,
    media: String,
    sound_id: Option<String>,
    title: String,
    to: Option<String>,
) -> Result<(), String> {
    let state = app.state::<AppState>();
    let media = PathBuf::from(media);
    let dir = clips_dir(&state).join("envoi");
    if media.parent() != Some(dir.as_path()) {
        return Err("choisis d'abord une image ou une vidéo".into());
    }
    let media_ext = media.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    kind_of(&media_ext).ok_or("format non pris en charge")?;
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
        None => (Vec::new(), Header { volume: 1.0, ..Default::default() }),
    };
    let header = Header { media_ext, media_len: media_data.len() as u64, ..header };
    let data = pack(&header, &media_data, &sound_data);
    if data.len() as u64 > MAX_CLIP {
        return Err("l'image et le son font plus de 10 Mo ensemble : prends un son plus court".into());
    }
    let room = &state.extras.room;
    let to = to.filter(|t| !t.is_empty()).unwrap_or_default();
    if !to.is_empty() && !room.status().members.iter().any(|m| m.id == to) {
        return Err("ce pote n'est plus en ligne".into());
    }
    let title: String = title.trim().chars().take(60).collect();
    let msg = Msg::Clip { from: room.my_id(), name: room.my_name(), to, title };
    room.publish_file(&msg, "extrait.sbclip", data).await
}

/// Un extrait arrive d'un pote.
pub async fn receive(app: &AppHandle, from: String, name: String, title: String, url: Option<String>) -> Result<(), String> {
    let state = app.state::<AppState>();
    let mode = state.config.lock().unwrap().settings.clips_mode.clone();
    if mode == "off" {
        return Ok(());
    }
    let Some(url) = url else { return Ok(()) };
    {
        let mut last = state.clips.last_from.lock().unwrap();
        if last.get(&from).is_some_and(|t| t.elapsed() < SPAM_GAP) {
            return Ok(());
        }
        last.insert(from.clone(), Instant::now());
    }
    let data = room::fetch(&url).await?;
    let (h, media, sound) = unpack(&data)?;
    let dir = clips_dir(&state).join("recus");
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    trim_dir(&dir, 20);
    let id = uuid::Uuid::new_v4().simple().to_string();
    let media_path = dir.join(format!("{id}.{}", h.media_ext));
    fs::write(&media_path, media).map_err(|e| e.to_string())?;
    let sound_path = if sound.is_empty() {
        None
    } else {
        let p = dir.join(format!("{id}-son.{}", h.sound_ext));
        fs::write(&p, sound).map_err(|e| e.to_string())?;
        Some(p.to_string_lossy().into_owned())
    };
    let name: String = name.chars().take(32).collect();
    let title: String = title.chars().take(60).collect();
    let show = ClipShow {
        from: name,
        title,
        kind: kind_of(&h.media_ext).unwrap_or("image"),
        media: media_path.to_string_lossy().into_owned(),
        sound: sound_path,
        start_ms: h.start_ms,
        end_ms: h.end_ms,
        volume: h.volume.clamp(0.0, 2.0),
        ask: mode != "auto",
    };
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
    if let Some(w) = app.get_webview_window(WINDOW) {
        place(app, &w, ask);
        return app.emit_to(WINDOW, "clip-show", &show).map_err(|e| e.to_string());
    }
    let (width, height) = window_size(ask);
    let mut builder = WebviewWindowBuilder::new(app, WINDOW, WebviewUrl::App("clip.html".into()))
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
    builder.build().map(|_| ()).map_err(|e| e.to_string())
}

/// L'extrait en attente (lu par la fenêtre quand elle s'ouvre).
#[tauri::command]
pub fn clip_pending(state: State<AppState>) -> Option<ClipShow> {
    state.clips.pending.lock().unwrap().clone()
}

/// « Afficher » : la carte devient l'extrait.
#[tauri::command]
pub fn clip_accept(app: AppHandle) {
    if let Some(w) = app.get_webview_window(WINDOW) {
        place(&app, &w, false);
    }
}

/// Ferme l'extrait (fin, refus, ✕ ou « Tout arrêter »).
#[tauri::command]
pub fn clip_close(app: AppHandle) {
    close(&app);
}

pub fn close(app: &AppHandle) {
    if let Some(state) = app.try_state::<AppState>() {
        state.clips.pending.lock().unwrap().take();
    }
    if let Some(w) = app.get_webview_window(WINDOW) {
        let _ = w.destroy();
    }
}

pub fn is_clip_window(label: &str) -> bool {
    label == WINDOW
}

#[cfg(test)]
mod tests {
    use super::*;

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
