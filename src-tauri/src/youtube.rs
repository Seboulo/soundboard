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
        .timeout(Duration::from_secs(120))
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

/// Installe yt-dlp dans `tools_dir` s'il n'y est pas encore et renvoie son chemin.
pub async fn ensure(tools_dir: &Path, on_status: impl Fn(&str)) -> Result<PathBuf, String> {
    let exe = tools_dir.join(EXE);
    if exe.exists() {
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
    Ok(exe)
}

fn command(exe: &Path) -> Command {
    let mut cmd = Command::new(exe);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // Pas de fenêtre de console qui clignote.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
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
        if line.contains("Unable to download") || line.contains("connect") {
            return Err("connexion à YouTube impossible, vérifie ta connexion internet".into());
        }
        if line.contains("does not pass filter") {
            return Err(format!("vidéo ignorée : plus de {} minutes", MAX_DURATION_S / 60));
        }
        let short: String = line.chars().take(200).collect();
        return Err(format!("YouTube : {short}"));
    }
    let title = String::from_utf8_lossy(&output.stdout).lines().next().unwrap_or("").trim().to_string();
    let file = fs::read_dir(work_dir)
        .map_err(|e| e.to_string())?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .find(|p| p.extension().is_some_and(|e| e == "m4a" || e == "mp4"))
        .ok_or_else(|| {
            format!("vidéo ignorée : pas de piste audio compatible, ou plus de {} minutes", MAX_DURATION_S / 60)
        })?;
    let data = fs::read(&file).map_err(|e| e.to_string())?;
    let _ = fs::remove_dir_all(work_dir);
    let title: String = title.chars().take(60).collect();
    Ok((data, if title.is_empty() { "Son YouTube".into() } else { title }))
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
