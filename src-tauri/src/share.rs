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

/// Télécharge le pack. Renvoie le fichier et son empreinte.
pub async fn download(link: &str) -> Result<(Vec<u8>, String), String> {
    let url = direct_url(link)?;
    let client = reqwest::Client::builder()
        .user_agent(concat!("Soundboard/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(|e| e.to_string())?;
    let mut res = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("lien injoignable ({e})"))?;
    if !res.status().is_success() {
        return Err(format!(
            "le lien ne donne pas le fichier (erreur {}) : vérifie qu'il est partagé avec « Tout le monde ayant le lien »",
            res.status().as_u16()
        ));
    }
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
    Ok((data, hash))
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

    #[test]
    fn other_links_are_rejected_or_kept() {
        assert!(direct_url("pas un lien").is_err());
        assert!(direct_url("ftp://x/y.zip").is_err());
        assert_eq!(direct_url("https://exemple.fr/pack.zip").unwrap(), "https://exemple.fr/pack.zip");
    }
}
