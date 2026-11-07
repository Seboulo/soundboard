//! Ajout d'un son depuis un lien YouTube, avec yt-dlp.
//!
//! yt-dlp n'est pas livré avec l'app : il est téléchargé au premier usage
//! depuis ses versions officielles sur GitHub (empreinte SHA-256 vérifiée),
//! puis mis à jour par lui-même au plus une fois par jour, car YouTube
//! change souvent.
//!
//! On demande la piste audio M4A (AAC) que YouTube propose presque toujours :
//! elle se lit sans ffmpeg.

use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

const RELEASE: &str = "https://github.com/yt-dlp/yt-dlp/releases/latest/download/";
const EXE: &str = "yt-dlp.exe";
const MAX_EXE: usize = 80 * 1024 * 1024;
/// Vidéos plus longues ignorées : on veut un son, pas un film.
const MAX_DURATION_S: u32 = 20 * 60;
const UPDATE_EVERY: Duration = Duration::from_secs(24 * 3600);

/// Vrai pour les adresses YouTube reconnues (vidéos, shorts, liens courts).
pub fn is_youtube(url: &str) -> bool {
    let Ok(parsed) = reqwest::Url::parse(url.trim()) else {
        return false;
    };
    if !matches!(parsed.scheme(), "http" | "https") {
        return false;
    }
    let host = parsed.host_str().unwrap_or("").to_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host);
    matches!(host, "youtube.com" | "m.youtube.com" | "music.youtube.com" | "youtu.be")
}

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent(concat!("Soundboard/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(600))
        .build()
        .map_err(|e| e.to_string())
}

async fn fetch(url: &str, max: usize) -> Result<Vec<u8>, String> {
    let mut res = client()?
        .get(url)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| format!("téléchargement impossible ({url}) : {e}"))?;
    let mut data = Vec::new();
    while let Some(chunk) = res.chunk().await.map_err(|e| e.to_string())? {
        data.extend_from_slice(&chunk);
        if data.len() > max {
            return Err("téléchargement trop gros".into());
        }
    }
    Ok(data)
}

/// Trouve l'empreinte d'un fichier dans le SHA2-256SUMS des versions de yt-dlp.
pub fn expected_hash(sums: &str, file: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (hash, name) = line.split_once(char::is_whitespace)?;
        (name.trim().trim_start_matches('*') == file && hash.len() == 64).then(|| hash.to_lowercase())
    })
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Deno : depuis novembre 2025, yt-dlp a besoin d'un moteur JavaScript pour
/// YouTube (sinon des formats manquent). Posé à côté de yt-dlp.exe, il est
/// trouvé tout seul.
const DENO_ZIP: &str = "https://github.com/denoland/deno/releases/latest/download/deno-x86_64-pc-windows-msvc.zip";
const DENO_MAX: usize = 120 * 1024 * 1024;
static DENO_BUSY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Empreinte du fichier « .sha256sum » de Deno (« Hash : ABCD… »).
fn deno_hash(text: &str) -> Option<String> {
    text.split(|c: char| !c.is_ascii_hexdigit()).find(|w| w.len() == 64).map(|h| h.to_lowercase())
}

/// Installe Deno à côté de yt-dlp, en arrière-plan (une seule fois) : la
/// première récupération ne l'attend pas.
fn ensure_deno(tools_dir: &Path) {
    if !cfg!(windows) || tools_dir.join("deno.exe").exists() {
        return;
    }
    if DENO_BUSY.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    let dir = tools_dir.to_path_buf();
    tauri::async_runtime::spawn(async move {
        let res = install_deno(&dir).await;
        crate::journal::write(&match res {
            Ok(()) => "Deno installé pour yt-dlp".to_string(),
            Err(e) => format!("Deno non installé ({e})"),
        });
        DENO_BUSY.store(false, std::sync::atomic::Ordering::SeqCst);
    });
}

async fn install_deno(dir: &Path) -> Result<(), String> {
    let sums = fetch(&format!("{DENO_ZIP}.sha256sum"), 4096).await?;
    let expected = deno_hash(&String::from_utf8_lossy(&sums)).ok_or("empreinte de Deno introuvable")?;
    let data = fetch(DENO_ZIP, DENO_MAX).await?;
    if hex(&Sha256::digest(&data)) != expected {
        return Err("empreinte de Deno différente".into());
    }
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(data)).map_err(|e| e.to_string())?;
    let mut file = zip.by_name("deno.exe").map_err(|e| e.to_string())?;
    let mut exe = Vec::new();
    std::io::Read::read_to_end(&mut file, &mut exe).map_err(|e| e.to_string())?;
    let tmp = dir.join("deno.download");
    fs::write(&tmp, &exe).map_err(|e| e.to_string())?;
    fs::rename(&tmp, dir.join("deno.exe")).map_err(|e| e.to_string())
}

/// ffmpeg (versions préparées par yt-dlp, environ 90 Mo, une seule fois) :
/// permet de ne récupérer que le passage choisi d'une vidéo, même longue.
const FFMPEG_BASE: &str = "https://github.com/yt-dlp/FFmpeg-Builds/releases/latest/download/";
const FFMPEG_ZIP: &str = "ffmpeg-master-latest-win64-gpl-shared.zip";
const FFMPEG_MAX: usize = 200 * 1024 * 1024;
static FFMPEG_LOCK: tauri::async_runtime::Mutex<()> = tauri::async_runtime::Mutex::const_new(());

/// Dossier de ffmpeg s'il est déjà installé.
pub fn ffmpeg_ready(tools_dir: &Path) -> Option<PathBuf> {
    let dir = tools_dir.join("ffmpeg");
    let exe = if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" };
    dir.join(exe).exists().then_some(dir)
}

/// Plus grand côté d'un extrait converti (720p, en large ou en hauteur).
const NORMALIZE_LONG_SIDE: u32 = 1280;

/// Réglages de ffmpeg pour un extrait « propre », que tout PC lit sans
/// à-coup : H.264 (couleurs yuv420p) + AAC dans un MP4, une image clé par
/// seconde, index au début du fichier (`+faststart`), 720p au plus et débit
/// plafonné (30 s tiennent dans le relais). Le passage choisi est coupé ici :
/// chez le pote, la vidéo commence à 0, sans saut à faire.
pub fn normalize_args(video: &Path, audio: Option<&Path>, start_s: f64, dur_s: f64, with_sound: bool, out: &Path) -> Vec<std::ffi::OsString> {
    let n = NORMALIZE_LONG_SIDE;
    let start = format!("{:.3}", start_s.max(0.0));
    let dur = format!("{:.3}", dur_s.clamp(0.5, 30.0));
    let mut a: Vec<std::ffi::OsString> = Vec::new();
    let mut push = |x: &str| a.push(x.into());
    for x in ["-hide_banner", "-loglevel", "error", "-nostdin", "-y", "-ss", &start, "-t", &dur, "-i"] {
        push(x);
    }
    a.push(video.into());
    let sidecar = audio.filter(|_| with_sound);
    if let Some(s) = sidecar {
        for x in ["-ss", &start, "-t", &dur, "-i"] {
            a.push(x.into());
        }
        a.push(s.into());
    }
    let scale = format!("scale=w='if(gt(iw\\,ih)\\,min({n}\\,iw)\\,-2)':h='if(gt(iw\\,ih)\\,-2\\,min({n}\\,ih))'");
    let mut push = |x: &str| a.push(x.into());
    push("-map");
    push("0:v:0");
    if with_sound {
        push("-map");
        push(if sidecar.is_some() { "1:a:0" } else { "0:a:0?" });
    }
    for x in [
        "-c:v", "libx264", "-preset", "veryfast", "-crf", "23", "-maxrate", "2M", "-bufsize", "4M", "-pix_fmt", "yuv420p", "-vf", &scale,
        "-force_key_frames", "expr:gte(t,n_forced*1)",
    ] {
        push(x);
    }
    if with_sound {
        for x in ["-c:a", "aac", "-b:a", "128k", "-ac", "2"] {
            push(x);
        }
    } else {
        push("-an");
    }
    for x in ["-movflags", "+faststart", "-f", "mp4"] {
        push(x);
    }
    a.push(out.into());
    a
}

/// Convertit le passage d'une vidéo en extrait « propre » (voir
/// `normalize_args`) dans `out`.
pub fn normalize_clip(ffmpeg_dir: &Path, video: &Path, audio: Option<&Path>, start_s: f64, dur_s: f64, with_sound: bool, out: &Path) -> Result<(), String> {
    let exe = ffmpeg_dir.join(if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" });
    let _ = fs::remove_file(out);
    let res = command(&exe)
        .args(normalize_args(video, audio, start_s, dur_s, with_sound, out))
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("ffmpeg ne démarre pas ({e})"))?;
    // Un début après la fin de la vidéo donne un MP4 vide sans erreur.
    let size = fs::metadata(out).map(|m| m.len()).unwrap_or(0);
    if !res.status.success() || size < 2048 {
        let _ = fs::remove_file(out);
        let why = String::from_utf8_lossy(&res.stderr).lines().last().unwrap_or("").to_string();
        return Err(format!("conversion impossible ({why})"));
    }
    Ok(())
}

/// Installe ffmpeg à côté de yt-dlp (une seule fois) et renvoie son dossier.
pub async fn ensure_ffmpeg(tools_dir: &Path, on_status: impl Fn(&str)) -> Result<PathBuf, String> {
    if let Some(dir) = ffmpeg_ready(tools_dir) {
        return Ok(dir);
    }
    if !cfg!(windows) {
        return Err("ffmpeg n'est installé tout seul que sous Windows".into());
    }
    on_status("Première fois : téléchargement de l'outil vidéo ffmpeg (environ 90 Mo)…");
    // Deux demandes en même temps : la seconde attend la première.
    let _one = FFMPEG_LOCK.lock().await;
    if let Some(dir) = ffmpeg_ready(tools_dir) {
        return Ok(dir);
    }
    let sums = fetch(&format!("{FFMPEG_BASE}checksums.sha256"), 64 * 1024).await?;
    let expected = expected_hash(&String::from_utf8_lossy(&sums), FFMPEG_ZIP).ok_or("empreinte de ffmpeg introuvable")?;
    let data = fetch(&format!("{FFMPEG_BASE}{FFMPEG_ZIP}"), FFMPEG_MAX).await?;
    let dir = tools_dir.to_path_buf();
    let res = tauri::async_runtime::spawn_blocking(move || unpack_ffmpeg(&data, &expected, &dir))
        .await
        .map_err(|e| e.to_string())?;
    crate::journal::write(&match &res {
        Ok(()) => "ffmpeg installé pour les extraits vidéo".to_string(),
        Err(e) => format!("ffmpeg non installé ({e})"),
    });
    res?;
    ffmpeg_ready(tools_dir).ok_or_else(|| "ffmpeg introuvable après l'installation".into())
}

/// Vérifie l'archive de ffmpeg puis garde ffmpeg.exe, ffprobe.exe et leurs DLL
/// dans `tools/ffmpeg` (posé d'un coup : jamais à moitié installé).
fn unpack_ffmpeg(data: &[u8], expected: &str, tools_dir: &Path) -> Result<(), String> {
    if hex(&Sha256::digest(data)) != expected {
        return Err("empreinte de ffmpeg différente".into());
    }
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(data)).map_err(|e| e.to_string())?;
    let tmp = tools_dir.join("ffmpeg.download");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| e.to_string())?;
        let name = entry.name().replace('\\', "/");
        let Some((folder, file)) = name.rsplit_once('/') else { continue };
        let wanted = file == "ffmpeg.exe" || file == "ffprobe.exe" || file.to_lowercase().ends_with(".dll");
        if !folder.ends_with("bin") || !wanted || file.contains("..") {
            continue;
        }
        let mut out = fs::File::create(tmp.join(file)).map_err(|e| e.to_string())?;
        std::io::copy(&mut entry, &mut out).map_err(|e| e.to_string())?;
    }
    if !tmp.join("ffmpeg.exe").exists() {
        let _ = fs::remove_dir_all(&tmp);
        return Err("ffmpeg.exe absent de l'archive".into());
    }
    let dir = tools_dir.join("ffmpeg");
    let _ = fs::remove_dir_all(&dir);
    fs::rename(&tmp, &dir).map_err(|e| e.to_string())
}

/// Installe yt-dlp dans `tools_dir` s'il n'y est pas encore et renvoie son chemin.
pub async fn ensure(tools_dir: &Path, on_status: impl Fn(&str)) -> Result<PathBuf, String> {
    let exe = tools_dir.join(EXE);
    if exe.exists() {
        ensure_deno(tools_dir);
        return Ok(exe);
    }
    on_status("Première fois : téléchargement de yt-dlp (environ 18 Mo)…");
    let sums = fetch(&format!("{RELEASE}SHA2-256SUMS"), 64 * 1024).await?;
    let expected = expected_hash(&String::from_utf8_lossy(&sums), EXE)
        .ok_or("empreinte de yt-dlp introuvable")?;
    let data = fetch(&format!("{RELEASE}{EXE}"), MAX_EXE).await?;
    if hex(&Sha256::digest(&data)) != expected {
        return Err("le fichier yt-dlp téléchargé ne correspond pas à son empreinte officielle ; réessaie dans quelques minutes".into());
    }
    fs::create_dir_all(tools_dir).map_err(|e| e.to_string())?;
    let tmp = tools_dir.join("yt-dlp.download");
    fs::write(&tmp, &data).map_err(|e| e.to_string())?;
    fs::rename(&tmp, &exe).map_err(|e| e.to_string())?;
    // L'installation vaut vérification du jour.
    let _ = fs::write(tools_dir.join("last-update"), b"");
    ensure_deno(tools_dir);
    Ok(exe)
}

fn command(exe: &Path) -> Command {
    let mut cmd = Command::new(exe);
    // Deno est posé à côté de yt-dlp, mais yt-dlp ne le cherche que dans le
    // PATH : sans lui, YouTube ne donne qu'une partie des formats (« Requested
    // format is not available »). On ajoute donc le dossier au PATH.
    if let Some(dir) = exe.parent() {
        let mut paths = vec![dir.to_path_buf()];
        if let Some(old) = std::env::var_os("PATH") {
            paths.extend(std::env::split_paths(&old));
        }
        if let Ok(joined) = std::env::join_paths(paths) {
            cmd.env("PATH", joined);
        }
    }
    // L'appli n'a pas de console : sans entrée explicite, yt-dlp hérite
    // d'une entrée invalide.
    cmd.stdin(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // Pas de fenêtre de console qui clignote.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Traduit les refus connus de yt-dlp en une raison claire. `None` : erreur
/// inattendue (on peut retenter autrement).
pub fn explain(line: &str) -> Option<String> {
    let l = line.to_lowercase();
    let says = |keys: &[&str]| keys.iter().any(|k| l.contains(k));
    let why = if says(&["confirm your age", "age-restricted", "inappropriate for some users"]) {
        "vidéo réservée aux adultes : YouTube demande d'être connecté, elle ne peut pas être récupérée"
    } else if says(&["not a bot", "sign in to confirm"]) {
        "YouTube bloque les récupérations depuis ta connexion pour le moment : réessaie dans quelques minutes"
    } else if says(&["private video", "this video is private"]) {
        "vidéo privée : seul son auteur peut la voir"
    } else if says(&["members-only", "join this channel", "members only"]) {
        "vidéo réservée aux membres de la chaîne"
    } else if says(&["not available in your country", "blocked it in your country", "geo restrict", "geo-restrict"]) {
        "vidéo bloquée dans ton pays"
    } else if says(&["is live", "live event", "premieres in", "this live"]) {
        "c'est un direct (ou une première pas encore diffusée) : prends une vidéo déjà publiée"
    } else if says(&["video unavailable", "has been removed", "no longer available", "account associated", "terminated"]) {
        "vidéo supprimée ou indisponible"
    } else if says(&["copyright"]) {
        "vidéo retirée pour droits d'auteur"
    } else if says(&["login required", "log in", "logged-in", "requires authentication", "rate-limit reached", "cookies"]) {
        "ce site demande d'être connecté pour cette vidéo (Instagram, Facebook…) : essaie un autre lien"
    } else if says(&["unsupported url"]) {
        "ce site n'est pas pris en charge"
    } else if says(&["unable to download webpage", "getaddrinfo", "timed out", "connection", "network is unreachable"]) {
        "vidéo injoignable, vérifie le lien et ta connexion"
    } else if says(&["http error 404", "not found"]) {
        "lien introuvable : vérifie qu'il est complet"
    } else {
        return None;
    };
    Some(why.to_string())
}

/// Met yt-dlp à jour au plus une fois par jour. Une erreur n'est pas bloquante.
pub fn update_if_stale(exe: &Path) {
    let Some(dir) = exe.parent() else { return };
    let stamp = dir.join("last-update");
    let fresh = fs::metadata(&stamp)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .is_some_and(|age| age < UPDATE_EVERY);
    if fresh {
        return;
    }
    let _ = command(exe).arg("-U").output();
    let _ = fs::write(&stamp, b"");
}

/// Télécharge la piste audio d'une vidéo. Renvoie les données et le titre.
pub fn download_audio(exe: &Path, url: &str, work_dir: &Path) -> Result<(Vec<u8>, String), String> {
    if !is_youtube(url) {
        return Err("ce n'est pas un lien YouTube".into());
    }
    let _ = fs::remove_dir_all(work_dir);
    fs::create_dir_all(work_dir).map_err(|e| e.to_string())?;
    let output = command(exe)
        .args([
            "--no-playlist",
            "--no-warnings",
            "--quiet",
            "--no-simulate",
            "--print",
            "title",
            "-f",
            "bestaudio[ext=m4a]/bestaudio[acodec^=mp4a]",
            "--max-filesize",
            "60M",
            "--match-filter",
            &format!("duration <= {MAX_DURATION_S}"),
            "-o",
        ])
        .arg(work_dir.join("audio.%(ext)s"))
        // Après « -- », le lien ne peut pas être pris pour une option.
        .arg("--")
        .arg(url.trim())
        .output()
        .map_err(|e| format!("yt-dlp n'a pas pu démarrer : {e}"))?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        let line = err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("erreur inconnue");
        let line = line.trim().trim_start_matches("ERROR: ");
        if line.contains("does not pass filter") {
            return Err(format!("vidéo ignorée : plus de {} minutes", MAX_DURATION_S / 60));
        }
        if let Some(why) = explain(line) {
            return Err(why);
        }
        let short: String = line.chars().take(200).collect();
        return Err(format!("YouTube : {short}"));
    }
    let file = fs::read_dir(work_dir)
        .map_err(|e| e.to_string())?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .find(|p| p.extension().is_some_and(|e| e == "m4a" || e == "mp4"))
        .ok_or_else(|| {
            format!("vidéo ignorée : pas de piste audio compatible, ou plus de {} minutes", MAX_DURATION_S / 60)
        })?;
    let title = String::from_utf8_lossy(&output.stdout).lines().next().unwrap_or("").trim().to_string();
    let data = fs::read(&file).map_err(|e| e.to_string())?;
    let _ = fs::remove_dir_all(work_dir);
    let title: String = title.chars().take(60).collect();
    Ok((data, if title.is_empty() { "Son YouTube".into() } else { title }))
}

/// Vrai pour un lien web qu'on peut donner à yt-dlp (YouTube, TikTok, X…).
/// Sites dont un extrait peut partir par lien seul : l'appli du pote ne
/// télécharge que depuis ces sites connus (jamais une adresse de son réseau).
pub fn is_shareable_link(url: &str) -> bool {
    const SITES: [&str; 12] = [
        "youtube.com", "youtu.be", "tiktok.com", "x.com", "twitter.com", "reddit.com",
        "redd.it", "instagram.com", "twitch.tv", "streamable.com", "dailymotion.com", "vimeo.com",
    ];
    reqwest::Url::parse(url.trim()).is_ok_and(|u| {
        u.scheme() == "https"
            && u.host_str().is_some_and(|h| SITES.iter().any(|s| h == *s || h.ends_with(&format!(".{s}"))))
    })
}

pub fn is_web_link(url: &str) -> bool {
    reqwest::Url::parse(url.trim()).is_ok_and(|u| matches!(u.scheme(), "http" | "https") && u.host_str().is_some())
}

/// Une vidéo récupérée pour un extrait : le fichier vidéo, et à part la
/// piste audio quand YouTube ne donne pas les deux ensemble (Shorts, HD).
pub struct WebVideo {
    pub video: PathBuf,
    pub audio: Option<PathBuf>,
    pub title: String,
}

/// Choix des formats, du plus simple au plus courant :
/// 1. un seul fichier image + son (MP4 H.264) assez léger ;
/// 2. image (MP4 H.264, 480p au plus, en largeur comme en hauteur pour les
///    Shorts verticaux) + son (M4A) séparés : sans ffmpeg, yt-dlp les garde
///    en deux fichiers, que l'appli rejoue ensemble ;
/// 3. vidéo plus longue : la plus petite image en H.264 (240p au moins) ;
/// 4. pareil sans exiger le H.264.
/// Vérifié avec yt-dlp sur des fiches de Short, de vidéo et de longue vidéo.
const VIDEO_FORMATS: &str = "b[ext=mp4][filesize<8M]/b[ext=mp4][filesize_approx<8M]\
/bv[ext=mp4][vcodec^=avc1][height<=854][width<=854][filesize<7M]+ba[ext=m4a]\
/bv[ext=mp4][vcodec^=avc1][height<=854][width<=854][filesize_approx<7M]+ba[ext=m4a]\
/wv[ext=mp4][vcodec^=avc1][height>=240][width>=240]+ba[ext=m4a]\
/bv[height<=854][width<=854]+ba[ext=m4a]/b";

/// Passage avec ffmpeg : 480p au plus (H.264 + AAC de préférence), réunis en MP4.
const CUT_FORMATS: &str = "bv*[ext=mp4][vcodec^=avc1][height<=480]+ba[ext=m4a]\
/b[ext=mp4][vcodec^=avc1][height<=480]/bv*[height<=480]+ba/b[height<=480]/b";

/// Lance la mise à jour du jour sans attendre (elle ralentissait la
/// récupération qui la précédait).
pub fn update_in_background(exe: &Path) {
    let exe = exe.to_path_buf();
    std::thread::spawn(move || update_if_stale(&exe));
}

/// Pourcentage d'une ligne de progression (« SBPCT  42.3% »).
fn progress_of(line: &str) -> Option<f32> {
    line.trim().strip_prefix("SBPCT")?.trim().trim_end_matches('%').trim().parse().ok()
}

/// Vidéo entière trop lourde : on peut alors n'en récupérer qu'un passage.
pub const TOO_BIG: &str = "vidéo trop lourde (8 Mo au plus) : prends une vidéo plus courte";

/// Passage récupéré avec ffmpeg : 40 s, de quoi choisir les 30 s à envoyer.
pub const CUT_WINDOW_S: f64 = 40.0;

/// Comment yt-dlp récupère la vidéo.
enum Fetch<'a> {
    /// Vidéo entière, 4 morceaux à la fois, avec l'avancement.
    Fast,
    /// Vidéo entière, sans options (secours).
    Simple,
    /// Seulement le passage qui commence à `start` (secondes), avec ffmpeg :
    /// toute vidéo marche, même longue.
    Cut { start: f64, ffmpeg: &'a Path },
}

/// Télécharge une petite vidéo pour un extrait (8 Mo au plus : le relais limite les envois).
/// Avec `cut` (début en secondes, dossier de ffmpeg), seul le passage de
/// `CUT_WINDOW_S` secondes est récupéré ; le fichier commence alors à 0.
/// `on_progress` reçoit l'avancement (0 à 100) de chaque fichier.
pub fn download_video(exe: &Path, url: &str, work_dir: &Path, cut: Option<(f64, &Path)>, on_progress: impl Fn(f32)) -> Result<WebVideo, String> {
    if !is_web_link(url) {
        return Err("colle un lien qui commence par https://".into());
    }
    // Récupération rapide (4 morceaux à la fois, avancement en %) ; si yt-dlp
    // la refuse sur ce PC (« [Errno 22] Invalid argument » vu sous Windows),
    // on refait la récupération simple d'avant, sans ces options.
    // Une raison connue (vidéo privée, réservée aux adultes…) : inutile de retenter.
    let first = match cut {
        Some((start, ffmpeg)) => Fetch::Cut { start, ffmpeg },
        None => Fetch::Fast,
    };
    let title = match run_video(exe, url, work_dir, first, &on_progress) {
        Ok(t) => t,
        Err(line) => match explain(&line) {
            Some(why) => return Err(why),
            None => run_video(exe, url, work_dir, Fetch::Simple, &on_progress).map_err(|line| {
                crate::journal::write(&format!("yt-dlp : {line}"));
                explain(&line).unwrap_or_else(|| {
                    let short: String = line.chars().take(200).collect();
                    format!("vidéo : {short}")
                })
            })?,
        },
    };
    let files: Vec<PathBuf> = fs::read_dir(work_dir)
        .map_err(|e| e.to_string())?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| !p.to_string_lossy().ends_with(".part"))
        .collect();
    let ext_is = |p: &PathBuf, exts: &[&str]| p.extension().and_then(|e| e.to_str()).is_some_and(|e| exts.contains(&e));
    let audio = files.iter().find(|p| ext_is(p, &["m4a", "mp3", "aac"])).cloned();
    let video = files
        .iter()
        .find(|p| ext_is(p, &["mp4", "webm"]))
        .cloned()
        .ok_or(TOO_BIG)?;
    Ok(WebVideo { video, audio, title: title.chars().take(60).collect() })
}

/// Lance yt-dlp pour `download_video` ; renvoie le titre, ou la dernière
/// ligne d'erreur.
fn run_video(exe: &Path, url: &str, work_dir: &Path, how: Fetch, on_progress: &impl Fn(f32)) -> Result<String, String> {
    use std::io::{BufRead, BufReader, Read};
    use std::process::Stdio;
    let _ = fs::remove_dir_all(work_dir);
    fs::create_dir_all(work_dir).map_err(|e| e.to_string())?;
    let mut cmd = command(exe);
    cmd.args(["--no-playlist", "--no-warnings", "--quiet", "--no-simulate", "--print", "title"]);
    if !matches!(how, Fetch::Simple) {
        cmd.args([
            // Avancement, une ligne par étape, sur la sortie standard.
            "--progress",
            "--newline",
            "--progress-template",
            "download:SBPCT %(progress._percent_str)s",
            // Plusieurs morceaux à la fois : YouTube bride une connexion seule.
            "-N",
            "4",
        ]);
    }
    match how {
        Fetch::Cut { start, ffmpeg } => {
            // Image et son réunis en un MP4 (H.264 de préférence, lu partout).
            // Pas de --max-filesize : il jugerait la taille de la vidéo entière.
            let start = start.max(0.0);
            cmd.arg("--ffmpeg-location")
                .arg(ffmpeg)
                .args(["--download-sections", &format!("*{start:.1}-{:.1}", start + CUT_WINDOW_S)])
                .args(["-f", CUT_FORMATS, "--merge-output-format", "mp4", "-o"])
                .arg(work_dir.join("video.%(ext)s"));
        }
        _ => {
            cmd.args(["-f", VIDEO_FORMATS, "--max-filesize", "8M", "-o"]).arg(work_dir.join("video.%(format_id)s.%(ext)s"));
        }
    }
    let mut child = cmd
        .arg("--")
        .arg(url.trim())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("yt-dlp n'a pas pu démarrer : {e}"))?;
    let mut stderr = child.stderr.take();
    let err_reader = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(e) = stderr.as_mut() {
            let _ = e.read_to_string(&mut s);
        }
        s
    });
    let mut title = String::new();
    if let Some(out) = child.stdout.take() {
        for line in BufReader::new(out).lines().map_while(Result::ok) {
            match progress_of(&line) {
                Some(p) => on_progress(p),
                None if title.is_empty() && !line.trim().is_empty() => title = line.trim().to_string(),
                None => {}
            }
        }
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    let err = err_reader.join().unwrap_or_default();
    if !status.success() {
        let line = err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("erreur inconnue");
        return Err(line.trim().trim_start_matches("ERROR: ").to_string());
    }
    Ok(title)
}

/// Une vidéo trouvée par mots-clés.
#[derive(serde::Serialize, Clone, Debug, PartialEq)]
pub struct Found {
    pub url: String,
    pub title: String,
    pub thumb: String,
    pub duration_s: Option<u32>,
}

/// Lit les lignes « id \t durée \t titre » données par yt-dlp. Les id
/// bizarres sont écartés : l'adresse et la miniature en sont tirées.
pub fn parse_found(out: &str) -> Vec<Found> {
    out.lines()
        .filter_map(|l| {
            let mut it = l.splitn(3, '\t');
            let id = it.next()?.trim();
            let dur = it.next()?.trim();
            let title = it.next()?.trim();
            if id.len() != 11 || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
                return None;
            }
            Some(Found {
                url: format!("https://www.youtube.com/watch?v={id}"),
                title: title.chars().take(100).collect(),
                thumb: format!("https://i.ytimg.com/vi/{id}/mqdefault.jpg"),
                duration_s: dur.parse::<f64>().ok().map(|d| d.round() as u32),
            })
        })
        .collect()
}

/// Cherche des vidéos YouTube par mots-clés (sans rien télécharger) :
/// `count` résultats à partir du n° `start` (1 pour la première page).
pub fn search(exe: &Path, query: &str, start: u32, count: u32) -> Result<Vec<Found>, String> {
    let q: String = query.trim().chars().filter(|c| !c.is_control()).take(100).collect();
    if q.is_empty() {
        return Err("tape quelques mots".into());
    }
    let start = start.clamp(1, 100);
    let end = start + count.clamp(1, 24) - 1;
    let output = command(exe)
        .args(["--flat-playlist", "--no-warnings", "--ignore-errors", "--print", "%(id)s\t%(duration)s\t%(title)s"])
        .arg("-I")
        .arg(format!("{start}:{end}"))
        .arg("--")
        .arg(format!("ytsearch{end}:{q}"))
        .output()
        .map_err(|e| format!("yt-dlp n'a pas pu démarrer : {e}"))?;
    let found = parse_found(&String::from_utf8_lossy(&output.stdout));
    if found.is_empty() && !output.status.success() {
        return Err("recherche impossible, vérifie ta connexion".into());
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Avec un ffmpeg sur la machine (Linux de test), la conversion donne
    /// bien un MP4 H.264 + AAC, à la bonne taille et coupé au passage.
    #[test]
    fn normalized_clip_is_h264_aac_720p() {
        let Some(bin) = ["/usr/bin/ffmpeg", "/usr/local/bin/ffmpeg"].into_iter().map(Path::new).find(|p| p.exists()) else { return };
        let ff = bin.parent().unwrap();
        let dir = std::env::temp_dir().join(format!("sb-norm-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let src = dir.join("haute.webm");
        let ok = Command::new(bin)
            .args(["-loglevel", "error", "-y", "-f", "lavfi", "-i", "testsrc=size=1080x1920:rate=30", "-f", "lavfi", "-i", "sine=frequency=440"])
            .args(["-t", "8", "-c:v", "libvpx-vp9", "-deadline", "realtime", "-c:a", "libopus"])
            .arg(&src)
            .status()
            .unwrap();
        assert!(ok.success());
        let out = dir.join("pret.mp4");
        normalize_clip(ff, &src, None, 2.0, 4.0, true, &out).unwrap();
        let probe = Command::new(ff.join("ffprobe"))
            .args(["-v", "error", "-show_entries", "stream=codec_name,width,height,pix_fmt:format=duration", "-of", "default=nw=1"])
            .arg(&out)
            .output()
            .unwrap();
        let info = String::from_utf8_lossy(&probe.stdout).to_string();
        assert!(info.contains("codec_name=h264") && info.contains("codec_name=aac"), "{info}");
        assert!(info.contains("width=720") && info.contains("height=1280") && info.contains("pix_fmt=yuv420p"), "{info}");
        let d: f64 = info.lines().find_map(|l| l.strip_prefix("duration=")).unwrap().parse().unwrap();
        assert!((d - 4.0).abs() < 0.2, "{d}");
        // Début après la fin : erreur claire, pas de fichier vide.
        assert!(normalize_clip(ff, &src, None, 20.0, 4.0, true, &out).is_err());
        assert!(!out.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn recognizes_youtube_links() {
        assert!(is_youtube("https://www.youtube.com/watch?v=dQw4w9WgXcQ"));
        assert!(is_youtube("https://youtu.be/dQw4w9WgXcQ?t=42"));
        assert!(is_youtube("https://youtube.com/shorts/abc"));
        assert!(is_youtube(" https://music.youtube.com/watch?v=x "));
        assert!(!is_youtube("https://evil.com/youtube.com"));
        assert!(!is_youtube("https://youtube.com.evil.com/watch"));
        assert!(!is_youtube("file:///C:/youtube.com"));
        assert!(!is_youtube("--exec calc"));
        assert!(is_web_link("https://www.tiktok.com/@a/video/1"));
        assert!(!is_web_link("--exec calc"));
        assert!(!is_web_link("file:///C:/x.mp4"));
        assert!(is_shareable_link("https://www.youtube.com/watch?v=x"));
        assert!(is_shareable_link("https://vm.tiktok.com/abc"));
        assert!(!is_shareable_link("https://192.168.1.1/video.mp4"));
        assert!(!is_shareable_link("https://evil-youtube.com/x"));
        assert!(!is_shareable_link("http://youtube.com/x"));
    }

    #[test]
    fn reads_deno_hash() {
        let text = "\r\nAlgorithm : SHA256\r\nHash      : CBA84B26E0DF5AE7F74FC66B86A95B866872A86292EBD84FA1E959927EDD93BF\r\nPath      : C:\\a\\deno.zip\r\n";
        assert_eq!(deno_hash(text).as_deref(), Some("cba84b26e0df5ae7f74fc66b86a95b866872a86292ebd84fa1e959927edd93bf"));
    }

    #[test]
    fn reads_progress_lines() {
        assert_eq!(progress_of("SBPCT   42.3%"), Some(42.3));
        assert_eq!(progress_of("SBPCT 100.0%"), Some(100.0));
        assert_eq!(progress_of("Mon titre"), None);
    }

    #[test]
    fn unpacks_ffmpeg_bin_only() {
        use std::io::Write;
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut z = zip::ZipWriter::new(&mut buf);
            let o = zip::write::SimpleFileOptions::default();
            z.add_directory("ff/bin/", o).unwrap();
            for (name, data) in [("ff/bin/ffmpeg.exe", "a"), ("ff/bin/ffprobe.exe", "b"), ("ff/bin/avcodec-63.dll", "c"), ("ff/bin/ffplay.exe", "d"), ("ff/doc/x.dll", "e")] {
                z.start_file(name, o).unwrap();
                z.write_all(data.as_bytes()).unwrap();
            }
            z.finish().unwrap();
        }
        let data = buf.into_inner();
        let dir = std::env::temp_dir().join(format!("sb-ff-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        assert!(unpack_ffmpeg(&data, "bad", &dir).is_err());
        unpack_ffmpeg(&data, &hex(&Sha256::digest(&data)), &dir).unwrap();
        let mut got: Vec<_> = fs::read_dir(dir.join("ffmpeg")).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        got.sort();
        assert_eq!(got, ["avcodec-63.dll", "ffmpeg.exe", "ffprobe.exe"]);
        assert!(!dir.join("ffmpeg.download").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn explains_known_refusals() {
        assert!(explain("ERROR: [youtube] x: Sign in to confirm your age. This video may be inappropriate for some users.").unwrap().contains("adultes"));
        assert!(explain("ERROR: [youtube] x: Sign in to confirm you're not a bot").unwrap().contains("bloque"));
        assert!(explain("ERROR: [youtube] x: Private video. Sign in if you've been granted access").unwrap().contains("privée"));
        assert!(explain("ERROR: [youtube] x: Video unavailable").unwrap().contains("supprimée"));
        assert!(explain("[Errno 22] Invalid argument").is_none());
    }

    #[test]
    fn parses_search_results() {
        let out = "dQw4w9WgXcQ\t212.0\tRick Astley - Never Gonna Give You Up\nbad id\t3\tx\nabcdefghij_\tNA\tSans durée\n";
        let f = parse_found(out);
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].url, "https://www.youtube.com/watch?v=dQw4w9WgXcQ");
        assert_eq!(f[0].thumb, "https://i.ytimg.com/vi/dQw4w9WgXcQ/mqdefault.jpg");
        assert_eq!(f[0].duration_s, Some(212));
        assert_eq!(f[1].duration_s, None);
        assert_eq!(f[1].title, "Sans durée");
    }

    #[test]
    fn finds_hash() {
        let sums = "aa  yt-dlp\n33121d192d706e62f684a06f80918e3f3a8ba52ac3a65213b6951c39c7d9f555  yt-dlp.exe\nbb  yt-dlp_x86.exe\n";
        assert_eq!(
            expected_hash(sums, "yt-dlp.exe").as_deref(),
            Some("33121d192d706e62f684a06f80918e3f3a8ba52ac3a65213b6951c39c7d9f555")
        );
        assert_eq!(expected_hash(sums, "nope.exe"), None);
    }
}
