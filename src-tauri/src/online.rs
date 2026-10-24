//! Recherche de sons en ligne (MyInstants et Freesound) et vérification des
//! nouvelles versions.
//!
//! MyInstants n'a pas d'API publique : on lit la page de résultats. Chaque
//! résultat contient un `play('/media/sounds/xxx.mp3', …)` suivi du lien
//! vers la page du son, dont le texte est le nom.
//!
//! Freesound a une API, mais il faut une clé personnelle (gratuite).

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;
use std::time::Duration;

const SITE: &str = "https://www.myinstants.com";
const FREESOUND: &str = "https://freesound.org/apiv2/search/text/";
const FREESOUND_CDN: &str = "https://cdn.freesound.org/previews/";
const RELEASES_API: &str = "https://api.github.com/repos/Seboulo/soundboard/releases/latest";
pub const RELEASES_PAGE: &str = "https://github.com/Seboulo/soundboard/releases";
const MAX_DOWNLOAD: usize = 20 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct OnlineSound {
    pub name: String,
    pub url: String,
}

pub(crate) fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent(concat!("Soundboard/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())
}

pub async fn search(query: &str, page: u32) -> Result<Vec<OnlineSound>, String> {
    let html = client()?
        .get(format!("{SITE}/fr/search/"))
        .query(&[("name", query), ("page", &page.max(1).to_string())])
        .send()
        .await
        .map_err(|e| format!("recherche impossible : {e}"))?;
    // Au-delà de la dernière page, le site répond 404 : ce n'est pas une erreur.
    if html.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(vec![]);
    }
    let html = html
        .error_for_status()
        .map_err(|e| format!("recherche impossible : {e}"))?
        .text()
        .await
        .map_err(|e| e.to_string())?;
    Ok(parse_results(&html))
}

pub fn parse_results(html: &str) -> Vec<OnlineSound> {
    static PLAY: OnceLock<Regex> = OnceLock::new();
    static NAME: OnceLock<Regex> = OnceLock::new();
    let play = PLAY.get_or_init(|| Regex::new(r#"play\(\s*'(/media/sounds/[^']+?)'"#).unwrap());
    let name = NAME.get_or_init(|| {
        Regex::new(r#"<a[^>]*href="/[a-z]{2}/instant/[^"]*"[^>]*>([^<]+)</a>"#).unwrap()
    });

    let matches: Vec<_> = play.captures_iter(html).collect();
    let mut out: Vec<OnlineSound> = Vec::new();
    for (i, cap) in matches.iter().enumerate() {
        let whole = cap.get(0).unwrap();
        let path = &cap[1];
        // Le nom se trouve entre ce bouton et le suivant.
        let end = matches
            .get(i + 1)
            .map(|c| c.get(0).unwrap().start())
            .unwrap_or(html.len());
        let label = name
            .captures(&html[whole.end()..end])
            .map(|c| decode_entities(c[1].trim()))
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| name_from_path(path));
        let url = format!("{SITE}{path}");
        if !out.iter().any(|s| s.url == url) {
            out.push(OnlineSound { name: label, url });
        }
    }
    out
}

fn name_from_path(path: &str) -> String {
    path.rsplit('/')
        .next()
        .unwrap_or(path)
        .trim_end_matches(".mp3")
        .replace(['_', '-'], " ")
}

fn decode_entities(s: &str) -> String {
    s.replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&quot;", "\"")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

#[derive(Deserialize)]
struct FreesoundPage {
    #[serde(default)]
    results: Vec<FreesoundResult>,
}

#[derive(Deserialize)]
struct FreesoundResult {
    name: String,
    #[serde(default)]
    previews: std::collections::HashMap<String, String>,
}

pub async fn search_freesound(query: &str, page: u32, key: &str) -> Result<Vec<OnlineSound>, String> {
    let res = client()?
        .get(FREESOUND)
        .query(&[
            ("query", query),
            ("page", &page.max(1).to_string()),
            ("page_size", "20"),
            ("fields", "id,name,previews"),
            ("token", key),
        ])
        .send()
        .await
        .map_err(|e| format!("recherche impossible : {e}"))?;
    match res.status().as_u16() {
        // Au-delà de la dernière page.
        404 => return Ok(vec![]),
        401 | 403 => return Err("clé Freesound refusée : vérifie-la dans les réglages".into()),
        _ => {}
    }
    let body = res
        .error_for_status()
        .map_err(|e| format!("recherche impossible : {e}"))?
        .text()
        .await
        .map_err(|e| e.to_string())?;
    parse_freesound(&body)
}

pub fn parse_freesound(json: &str) -> Result<Vec<OnlineSound>, String> {
    let page: FreesoundPage = serde_json::from_str(json).map_err(|e| format!("réponse Freesound illisible : {e}"))?;
    Ok(page
        .results
        .into_iter()
        .filter_map(|r| {
            let url = r.previews.get("preview-hq-mp3").or(r.previews.get("preview-lq-mp3"))?.clone();
            if !url.starts_with(FREESOUND_CDN) {
                return None;
            }
            Some(OnlineSound {
                name: strip_extension(&r.name),
                url,
            })
        })
        .collect())
}

/// Les noms Freesound sont souvent des noms de fichiers (« bark.wav »).
fn strip_extension(name: &str) -> String {
    let lower = name.to_lowercase();
    for ext in [".wav", ".mp3", ".ogg", ".flac", ".aiff", ".aif", ".m4a"] {
        if lower.ends_with(ext) {
            return name[..name.len() - ext.len()].replace(['_', '-'], " ").trim().to_string();
        }
    }
    name.trim().to_string()
}

/// Télécharge un lien direct collé ou glissé par l'utilisateur. Renvoie les
/// données, l'extension à utiliser et un nom tiré de l'adresse.
pub async fn download_link(url: &str) -> Result<(Vec<u8>, String, String), String> {
    let parsed = reqwest::Url::parse(url).map_err(|_| "ce n'est pas un lien valide".to_string())?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("seuls les liens http(s) sont acceptés".into());
    }
    let file_name = parsed
        .path_segments()
        .and_then(|mut s| s.next_back())
        .map(|s| s.to_string())
        .unwrap_or_default();
    let mut res = client()?
        .get(parsed.clone())
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| format!("téléchargement impossible : {e}"))?;
    let content_type = res
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_lowercase();
    if content_type.starts_with("text/html") {
        return Err("ce lien mène à une page web, pas à un fichier audio (clic droit sur le son → « Copier l'adresse du fichier audio »)".into());
    }
    let mut data = Vec::new();
    while let Some(chunk) = res.chunk().await.map_err(|e| e.to_string())? {
        data.extend_from_slice(&chunk);
        if data.len() > MAX_DOWNLOAD {
            return Err("fichier trop gros (plus de 20 Mo)".into());
        }
    }
    let (ext, name) = link_name(&file_name, &content_type);
    Ok((data, ext, name))
}

/// Extension et nom d'un son à partir du nom de fichier de l'adresse.
pub fn link_name(file_name: &str, content_type: &str) -> (String, String) {
    const EXTS: &[&str] = &["mp3", "wav", "ogg", "flac", "m4a", "aac"];
    let decoded = percent_decode(file_name);
    let (stem, ext) = match decoded.rsplit_once('.') {
        Some((stem, ext)) if EXTS.contains(&ext.to_lowercase().as_str()) => (stem.to_string(), ext.to_lowercase()),
        _ => {
            let ext = match content_type {
                t if t.contains("wav") => "wav",
                t if t.contains("ogg") => "ogg",
                t if t.contains("flac") => "flac",
                t if t.contains("mp4") || t.contains("aac") || t.contains("m4a") => "m4a",
                _ => "mp3",
            };
            (decoded.clone(), ext.to_string())
        }
    };
    let name: String = stem.replace(['_', '-', '+'], " ").trim().chars().take(60).collect();
    let name = if name.is_empty() { "Son".to_string() } else { name };
    (ext, name)
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            if let Some(v) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[derive(Debug, Clone, Serialize)]
pub struct UpdateInfo {
    pub version: String,
    pub url: String,
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    html_url: String,
}

/// Regarde sur GitHub si une version plus récente est publiée.
pub async fn check_update() -> Result<Option<UpdateInfo>, String> {
    let body = client()?
        .get(RELEASES_API)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| e.to_string())?
        .text()
        .await
        .map_err(|e| e.to_string())?;
    let release: Release = serde_json::from_str(&body).map_err(|e| e.to_string())?;
    let latest = release.tag_name.trim_start_matches('v').to_string();
    if !is_newer(&latest, env!("CARGO_PKG_VERSION")) || !release.html_url.starts_with(RELEASES_PAGE) {
        return Ok(None);
    }
    Ok(Some(UpdateInfo {
        version: latest,
        url: release.html_url,
    }))
}

pub fn is_newer(candidate: &str, current: &str) -> bool {
    let parse = |v: &str| -> Vec<u64> { v.split('.').map(|p| p.parse().unwrap_or(0)).collect() };
    parse(candidate) > parse(current)
}

/// Télécharge un son trouvé par une recherche. Seuls les fichiers des deux
/// sites sont acceptés.
pub async fn download(url: &str) -> Result<Vec<u8>, String> {
    if !url.starts_with(&format!("{SITE}/media/sounds/")) && !url.starts_with(FREESOUND_CDN) {
        return Err("adresse de son non autorisée".into());
    }
    let mut res = client()?
        .get(url)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| format!("téléchargement impossible : {e}"))?;
    let mut data = Vec::new();
    while let Some(chunk) = res.chunk().await.map_err(|e| e.to_string())? {
        data.extend_from_slice(&chunk);
        if data.len() > MAX_DOWNLOAD {
            return Err("fichier trop gros (plus de 20 Mo)".into());
        }
    }
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_results() {
        let html = r#"
        <div class="instant">
          <button class="small-button" onclick="play('/media/sounds/bruh.mp3', 'loader-1', 'bruh-1')"></button>
          <a href="/fr/instant/bruh/" class="instant-link link-secondary">BRUH</a>
        </div>
        <div class="instant">
          <button class="small-button" onclick="play('/media/sounds/sad_trombone.mp3', 'loader-2', 'x')"></button>
          <a href="/fr/instant/sad/" class="instant-link">Sad &amp; slow</a>
        </div>
        <div class="instant">
          <button onclick="play('/media/sounds/no-name.mp3')"></button>
        </div>"#;
        assert_eq!(
            parse_results(html),
            vec![
                OnlineSound {
                    name: "BRUH".into(),
                    url: "https://www.myinstants.com/media/sounds/bruh.mp3".into()
                },
                OnlineSound {
                    name: "Sad & slow".into(),
                    url: "https://www.myinstants.com/media/sounds/sad_trombone.mp3".into()
                },
                OnlineSound {
                    name: "no name".into(),
                    url: "https://www.myinstants.com/media/sounds/no-name.mp3".into()
                },
            ]
        );
    }

    #[test]
    fn parses_freesound() {
        let json = r#"{"count":2,"next":null,"results":[
            {"id":1,"name":"Dog_bark.WAV","previews":{"preview-hq-mp3":"https://cdn.freesound.org/previews/0/1_2-hq.mp3","preview-lq-mp3":"https://cdn.freesound.org/previews/0/1_2-lq.mp3"}},
            {"id":2,"name":"evil","previews":{"preview-hq-mp3":"https://evil.example/x.mp3"}}
        ]}"#;
        assert_eq!(
            parse_freesound(json).unwrap(),
            vec![OnlineSound {
                name: "Dog bark".into(),
                url: "https://cdn.freesound.org/previews/0/1_2-hq.mp3".into()
            }]
        );
    }

    #[test]
    fn names_links() {
        assert_eq!(link_name("Vine%20Boom_sound.MP3", ""), ("mp3".into(), "Vine Boom sound".into()));
        assert_eq!(link_name("download", "audio/wav"), ("wav".into(), "download".into()));
        assert_eq!(link_name("", "audio/mpeg"), ("mp3".into(), "Son".into()));
        assert_eq!(link_name("a%2", ""), ("mp3".into(), "a%2".into()));
    }

    #[test]
    fn compares_versions() {
        assert!(is_newer("0.4.0", "0.3.0"));
        assert!(is_newer("0.10.0", "0.9.1"));
        assert!(!is_newer("0.3.0", "0.3.0"));
        assert!(!is_newer("0.2.9", "0.3.0"));
    }
}
