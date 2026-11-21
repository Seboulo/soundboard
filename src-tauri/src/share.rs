//! Packs partagés par lien.
//!
//! Celui qui partage choisit un onglet : l'app range le pack (.zip) dans
//! un dossier synchronisé (OneDrive, Dropbox, Google Drive) et le tient à
//! jour tout seul. Il crée une fois un lien de partage vers ce fichier et
//! le donne à ses potes. Leurs apps relisent le lien de temps en temps et
//! ajoutent (ou retirent) les sons qui ont changé.

use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::time::Duration;

/// Un pack peut être gros (des dizaines de sons) : 300 Mo au plus.
const MAX_PACK: usize = 300 * 1024 * 1024;

/// Transforme un lien de partage (page web) en lien qui donne le fichier.
pub fn direct_url(link: &str) -> Result<String, String> {
    let link = link.trim();
    let url = reqwest::Url::parse(link).map_err(|_| "ce n'est pas un lien valide".to_string())?;
    if url.scheme() != "https" && url.scheme() != "http" {
        return Err("seuls les liens http(s) sont acceptés".into());
    }
    let host = url.host_str().unwrap_or("").to_lowercase();
    if host.ends_with("dropbox.com") {
        let mut url = url.clone();
        let pairs: Vec<(String, String)> = url
            .query_pairs()
            .filter(|(k, _)| k != "dl" && k != "raw")
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        url.query_pairs_mut().clear().extend_pairs(pairs).append_pair("dl", "1");
        return Ok(url.to_string());
    }
    if host == "drive.google.com" {
        let id = url
            .path_segments()
            .and_then(|mut s| {
                // /file/d/<id>/view
                while let Some(part) = s.next() {
                    if part == "d" {
                        return s.next().map(String::from);
                    }
                }
                None
            })
            .or_else(|| url.query_pairs().find(|(k, _)| k == "id").map(|(_, v)| v.into_owned()))
            .ok_or("lien Google Drive incomplet : copie le lien du fichier")?;
        return Ok(format!("https://drive.usercontent.google.com/download?id={id}&export=download&confirm=t"));
    }
    if host == "1drv.ms" || host.ends_with("onedrive.live.com") {
        return Ok(format!("https://api.onedrive.com/v1.0/shares/u!{}/root/content", base64_url(link.as_bytes())));
    }
    Ok(url.to_string())
}

fn base64_url(data: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for i in 0..=chunk.len() {
            out.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

/// Ce que le serveur a dit du fichier la dernière fois (en-têtes `ETag` et
/// `Last-Modified`) : de quoi lui demander « seulement s'il a changé ».
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Validators {
    pub etag: String,
    pub modified: String,
}

pub enum Download {
    /// Le serveur répond « pas changé » (304) : rien n'est téléchargé.
    Unchanged,
    Fresh { data: Vec<u8>, hash: String, validators: Validators },
}

/// Télécharge le pack, sauf si le serveur sait qu'il n'a pas changé depuis
/// `known`. Avant, un pack suivi (jusqu'à 300 Mo) était retéléchargé en
/// entier à chaque démarrage et toutes les 2 h, même identique.
pub async fn download(link: &str, known: &Validators) -> Result<Download, String> {
    let url = direct_url(link)?;
    let client = reqwest::Client::builder()
        .user_agent(concat!("Soundboard/", env!("CARGO_PKG_VERSION")))
        // Pas de durée totale : un pack de 300 Mo prend plusieurs minutes sur
        // une connexion ordinaire. On abandonne seulement si plus rien
        // n'arrive.
        .connect_timeout(Duration::from_secs(20))
        .read_timeout(Duration::from_secs(60))
        .build()
        .map_err(|e| e.to_string())?;
    let mut req = client.get(&url);
    for (name, value) in [(reqwest::header::IF_NONE_MATCH, &known.etag), (reqwest::header::IF_MODIFIED_SINCE, &known.modified)] {
        if let Some(v) = (!value.is_empty()).then(|| reqwest::header::HeaderValue::from_str(value).ok()).flatten() {
            req = req.header(name, v);
        }
    }
    let mut res = req.send().await.map_err(|e| format!("lien injoignable ({e})"))?;
    if res.status() == reqwest::StatusCode::NOT_MODIFIED {
        return Ok(Download::Unchanged);
    }
    if !res.status().is_success() {
        return Err(format!(
            "le lien ne donne pas le fichier (erreur {}) : vérifie qu'il est partagé avec « Tout le monde ayant le lien »",
            res.status().as_u16()
        ));
    }
    let header = |name| {
        res.headers().get(name).and_then(|v: &reqwest::header::HeaderValue| v.to_str().ok()).unwrap_or("").to_string()
    };
    let validators = Validators { etag: header(reqwest::header::ETAG), modified: header(reqwest::header::LAST_MODIFIED) };
    let mut data = Vec::new();
    while let Some(chunk) = res.chunk().await.map_err(|e| e.to_string())? {
        data.extend_from_slice(&chunk);
        if data.len() > MAX_PACK {
            return Err("pack trop gros (300 Mo au plus)".into());
        }
    }
    if !data.starts_with(b"PK") {
        return Err("ce lien n'est pas un pack de sons (.zip) : partage le fichier lui-même, avec « Tout le monde ayant le lien »".into());
    }
    let hash = hex(&Sha256::digest(&data)[..16]);
    Ok(Download::Fresh { data, hash, validators })
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Empreinte d'un fichier de son (sert à reconnaître un son d'un pack).
pub fn key_of(data: &[u8]) -> String {
    hex(&Sha256::digest(data)[..12])
}

/// Dossiers synchronisés trouvés sur le PC (OneDrive, Dropbox, Google Drive).
pub fn cloud_folders() -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    for var in ["OneDriveConsumer", "OneDrive", "OneDriveCommercial"] {
        if let Some(p) = std::env::var_os(var).map(PathBuf::from).filter(|p| p.is_dir()) {
            if !out.iter().any(|(_, q)| q == &p) {
                out.push(("OneDrive".to_string(), p));
            }
        }
    }
    if let Some(home) = std::env::var_os("USERPROFILE").map(PathBuf::from) {
        for (name, p) in [("Dropbox", home.join("Dropbox")), ("Google Drive", home.join("Google Drive"))] {
            if p.is_dir() {
                out.push((name.to_string(), p));
            }
        }
    }
    // Google Drive pour ordinateur monte un lecteur « G: » avec « Mon Drive ».
    for letter in b'D'..=b'Z' {
        for sub in ["Mon Drive", "My Drive"] {
            let p = PathBuf::from(format!("{}:\\{sub}", letter as char));
            if p.is_dir() {
                out.push(("Google Drive".to_string(), p));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dropbox_links_download_the_file() {
        assert_eq!(
            direct_url("https://www.dropbox.com/scl/fi/abc/Potes.zip?rlkey=xyz&dl=0").unwrap(),
            "https://www.dropbox.com/scl/fi/abc/Potes.zip?rlkey=xyz&dl=1"
        );
    }

    #[test]
    fn google_drive_links_download_the_file() {
        let want = "https://drive.usercontent.google.com/download?id=1AbC_d&export=download&confirm=t";
        assert_eq!(direct_url("https://drive.google.com/file/d/1AbC_d/view?usp=sharing").unwrap(), want);
        assert_eq!(direct_url("https://drive.google.com/open?id=1AbC_d").unwrap(), want);
    }

    #[test]
    fn onedrive_links_use_the_shares_api() {
        let got = direct_url("https://1drv.ms/u/s!AkBc").unwrap();
        assert_eq!(got, format!("https://api.onedrive.com/v1.0/shares/u!{}/root/content", base64_url(b"https://1drv.ms/u/s!AkBc")));
        assert_eq!(base64_url(b"Man"), "TWFu");
        assert_eq!(base64_url(b"Ma"), "TWE");
    }

    /// Un faux serveur : le pack avec un `ETag` la première fois, puis
    /// « pas changé » quand on lui redonne cet `ETag`.
    #[test]
    fn unchanged_packs_are_not_downloaded_again() {
        use std::io::{Read, Write};
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/pack.zip", server.local_addr().unwrap());
        let seen = std::thread::spawn(move || {
            let mut asked = Vec::new();
            for _ in 0..2 {
                let (mut conn, _) = server.accept().unwrap();
                let mut buf = [0u8; 4096];
                let n = conn.read(&mut buf).unwrap();
                let req = String::from_utf8_lossy(&buf[..n]).to_lowercase();
                let reply = if req.contains("if-none-match: \"v1\"") {
                    "HTTP/1.1 304 Not Modified\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string()
                } else {
                    "HTTP/1.1 200 OK\r\nETag: \"v1\"\r\nLast-Modified: Sat, 21 Nov 2026 09:00:00 GMT\r\nContent-Length: 6\r\nConnection: close\r\n\r\nPKpack".to_string()
                };
                conn.write_all(reply.as_bytes()).unwrap();
                asked.push(req);
            }
            asked
        });
        let first = tauri::async_runtime::block_on(download(&url, &Validators::default())).unwrap();
        let Download::Fresh { data, validators, .. } = first else { panic!("pack attendu") };
        assert_eq!(data, b"PKpack");
        assert_eq!(validators.etag, "\"v1\"");
        assert_eq!(validators.modified, "Sat, 21 Nov 2026 09:00:00 GMT");
        let second = tauri::async_runtime::block_on(download(&url, &validators)).unwrap();
        assert!(matches!(second, Download::Unchanged), "le pack a été retéléchargé");
        let asked = seen.join().unwrap();
        assert!(!asked[0].contains("if-none-match"), "{}", asked[0]);
        assert!(asked[1].contains("if-modified-since: sat, 21 nov 2026 09:00:00 gmt"), "{}", asked[1]);
    }

    #[test]
    fn other_links_are_rejected_or_kept() {
        assert!(direct_url("pas un lien").is_err());
        assert!(direct_url("ftp://x/y.zip").is_err());
        assert_eq!(direct_url("https://exemple.fr/pack.zip").unwrap(), "https://exemple.fr/pack.zip");
    }
}
