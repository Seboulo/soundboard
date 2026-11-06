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
        .map_err(|e| format!("téléchargement de yt-dlp impossible : {e}"))?;
    let mut data = Vec::new();
    while let Some(chunk) = res.chunk().await.map_err(|e| e.to_string())? {
        data.extend_from_slice(&chunk);
        if data.len() > max {
            return Err("téléchargement de yt-dlp trop gros".into());
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

/// Télécharge une petite vidéo pour un extrait (8 Mo au plus : le relais limite les envois).
/// `on_progress` reçoit l'avancement (0 à 100) de chaque fichier.
pub fn download_video(exe: &Path, url: &str, work_dir: &Path, on_progress: impl Fn(f32)) -> Result<WebVideo, String> {
    if !is_web_link(url) {
        return Err("colle un lien qui commence par https://".into());
    }
    // Récupération rapide (4 morceaux à la fois, avancement en %) ; si yt-dlp
    // la refuse sur ce PC (« [Errno 22] Invalid argument » vu sous Windows),
    // on refait la récupération simple d'avant, sans ces options.
    // Une raison connue (vidéo privée, réservée aux adultes…) : inutile de retenter.
    let title = match run_video(exe, url, work_dir, true, &on_progress) {
        Ok(t) => t,
        Err(line) => match explain(&line) {
            Some(why) => return Err(why),
            None => run_video(exe, url, work_dir, false, &on_progress).map_err(|line| {
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
        .ok_or("vidéo trop lourde (8 Mo au plus) : prends une vidéo plus courte")?;
    Ok(WebVideo { video, audio, title: title.chars().take(60).collect() })
}

/// Lance yt-dlp pour `download_video` ; renvoie le titre, ou la dernière
/// ligne d'erreur. `fast` : morceaux en parallèle et avancement en %.
fn run_video(exe: &Path, url: &str, work_dir: &Path, fast: bool, on_progress: &impl Fn(f32)) -> Result<String, String> {
    use std::io::{BufRead, BufReader, Read};
    use std::process::Stdio;
    let _ = fs::remove_dir_all(work_dir);
    fs::create_dir_all(work_dir).map_err(|e| e.to_string())?;
    let mut cmd = command(exe);
    cmd.args(["--no-playlist", "--no-warnings", "--quiet", "--no-simulate", "--print", "title"]);
    if fast {
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
    let mut child = cmd
        .args(["-f", VIDEO_FORMATS, "--max-filesize", "8M", "-o"])
        .arg(work_dir.join("video.%(format_id)s.%(ext)s"))
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
