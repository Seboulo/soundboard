//! Images de memes pour illustrer les sons, cherchées sur GIPHY.
//!
//! Tenor (la base de GIF de Discord) n'est plus ouverte aux autres apps
//! depuis 2026 ; GIPHY demande une clé personnelle gratuite, comme Freesound.
//! Pour chaque image on garde deux fichiers : une version fixe (affichée sur
//! le pad) et une version animée (jouée pendant le son).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

const SEARCH: &str = "https://api.giphy.com/v1/gifs/search";
const MAX_IMAGE: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct GifResult {
    pub title: String,
    /// Petite image fixe, pour la liste de choix.
    pub still: String,
    /// Petite image animée.
    pub anim: String,
    /// Image animée plus grande (pour un extrait envoyé à un pote).
    pub big: String,
}

#[derive(Deserialize)]
struct Page {
    data: Vec<Gif>,
}

#[derive(Deserialize)]
struct Gif {
    #[serde(default)]
    title: String,
    images: HashMap<String, Rendition>,
}

#[derive(Deserialize)]
struct Rendition {
    url: Option<String>,
    webp: Option<String>,
}

/// Seules les images hébergées par GIPHY sont téléchargées.
pub fn allowed(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let host = rest.split(['/', '?']).next().unwrap_or("");
    host == "giphy.com" || host.ends_with(".giphy.com")
}

/// GIPHY refuse (414) les recherches de plus de 50 caractères.
const MAX_QUERY: usize = 50;

/// Raccourcit un nom de son en recherche GIPHY : sans ce qui est entre
/// parenthèses ou crochets, sans ponctuation, coupé entre deux mots.
pub fn clean_query(name: &str) -> String {
    let mut kept = String::new();
    let mut depth = 0u32;
    for c in name.chars() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            _ if depth == 0 => kept.push(c),
            _ => {}
        }
    }
    // Tout était entre parenthèses : on garde quand même les mots.
    if kept.trim().is_empty() {
        kept = name.to_string();
    }
    let words = kept
        .split(|c: char| !(c.is_alphanumeric() || c == '\'' || c == '’'))
        .map(|w| w.trim_matches(|c| c == '\'' || c == '’'))
        .filter(|w| !w.is_empty());
    let mut out = String::new();
    for w in words {
        let len = out.chars().count() + usize::from(!out.is_empty()) + w.chars().count();
        if len > MAX_QUERY {
            if out.is_empty() {
                out = w.chars().take(MAX_QUERY).collect();
            }
            break;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(w);
    }
    out
}

pub async fn search(query: &str, key: &str, limit: u32) -> Result<Vec<GifResult>, String> {
    let query = clean_query(query);
    if query.is_empty() {
        return Ok(Vec::new());
    }
    let res = crate::online::client()?
        .get(SEARCH)
        .query(&[
            ("api_key", key),
            ("q", query.as_str()),
            ("limit", &limit.clamp(1, 24).to_string()),
            ("rating", "pg-13"),
            ("lang", "fr"),
        ])
        .send()
        .await
        .map_err(|e| format!("recherche d'images impossible : {e}"))?;
    match res.status().as_u16() {
        401 | 403 => return Err("clé GIPHY refusée : vérifie-la dans les réglages".into()),
        429 => return Err("trop de recherches GIPHY pour l'instant, réessaie dans une heure".into()),
        _ => {}
    }
    let body = res
        .error_for_status()
        .map_err(|e| format!("recherche d'images impossible : {e}"))?
        .text()
        .await
        .map_err(|e| e.to_string())?;
    parse(&body)
}

pub fn parse(json: &str) -> Result<Vec<GifResult>, String> {
    let page: Page = serde_json::from_str(json).map_err(|e| format!("réponse GIPHY illisible : {e}"))?;
    Ok(page
        .data
        .into_iter()
        .filter_map(|g| {
            let pick = |names: &[&str], webp: bool| {
                names.iter().find_map(|n| {
                    let r = g.images.get(*n)?;
                    let url = if webp { r.webp.clone().or(r.url.clone()) } else { r.url.clone() };
                    url.filter(|u| allowed(u))
                })
            };
            let anim = pick(&["fixed_height_small", "fixed_height", "downsized"], true)?;
            let still = pick(&["fixed_height_small_still", "fixed_height_still", "downsized_still"], false)
                .unwrap_or_else(|| anim.clone());
            let big = pick(&["downsized_medium", "downsized", "fixed_height"], false).unwrap_or_else(|| anim.clone());
            Some(GifResult { title: g.title, still, anim, big })
        })
        .collect())
}

/// Télécharge une image GIPHY ; renvoie ses octets et son extension.
pub async fn download(url: &str) -> Result<(Vec<u8>, &'static str), String> {
    if !allowed(url) {
        return Err("adresse d'image non autorisée".into());
    }
    let mut res = crate::online::client()?
        .get(url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| format!("téléchargement de l'image impossible : {e}"))?;
    let mut data = Vec::new();
    while let Some(chunk) = res.chunk().await.map_err(|e| e.to_string())? {
        data.extend_from_slice(&chunk);
        if data.len() > MAX_IMAGE {
            return Err("image trop grosse".into());
        }
    }
    let ext = image_ext(&data).ok_or("ce fichier n'est pas une image")?;
    Ok((data, ext))
}

/// Reconnaît le format d'après les premiers octets (on ne se fie pas au nom).
pub fn image_ext(data: &[u8]) -> Option<&'static str> {
    if data.starts_with(b"GIF8") {
        Some("gif")
    } else if data.len() > 12 && &data[..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        Some("webp")
    } else if data.starts_with(&[0x89, b'P', b'N', b'G']) {
        Some("png")
    } else if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("jpg")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleans_long_names() {
        let q = clean_query("Rabbi Jacob (Bande originale du film \"Les aventures de Rabbi Jacob\")");
        assert_eq!(q, "Rabbi Jacob");
        let long = clean_query("un deux trois quatre cinq six sept huit neuf dix onze douze treize");
        assert!(long.chars().count() <= MAX_QUERY && long.starts_with("un deux"));
        assert_eq!(clean_query("(juste ça !)"), "juste ça");
        assert_eq!(clean_query("C'est l'heure — \"bruh\" !!"), "C'est l'heure bruh");
        assert_eq!(clean_query("a".repeat(80).as_str()).len(), MAX_QUERY);
        assert_eq!(clean_query("!!!"), "");
    }

    #[test]
    fn parses_search() {
        let json = r#"{"data":[
          {"title":"Bruh GIF","images":{
            "fixed_height_small":{"url":"https://media2.giphy.com/media/a/100.gif","webp":"https://media2.giphy.com/media/a/100.webp"},
            "fixed_height_small_still":{"url":"https://media2.giphy.com/media/a/100_s.gif"}}},
          {"title":"no small","images":{"fixed_height":{"url":"https://media0.giphy.com/media/b/200.gif"}}},
          {"title":"evil","images":{"fixed_height_small":{"url":"https://evil.example/x.gif"}}}
        ],"pagination":{},"meta":{"status":200}}"#;
        let r = parse(json).unwrap();
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].anim, "https://media2.giphy.com/media/a/100.webp");
        assert_eq!(r[0].still, "https://media2.giphy.com/media/a/100_s.gif");
        assert_eq!(r[1].still, r[1].anim);
    }

    #[test]
    fn only_giphy_hosts() {
        assert!(allowed("https://media1.giphy.com/media/x/100.gif"));
        assert!(!allowed("http://media1.giphy.com/x.gif"));
        assert!(!allowed("https://giphy.com.evil.net/x.gif"));
        assert!(!allowed("https://evil.net/?u=giphy.com"));
    }

    #[test]
    fn sniffs_formats() {
        assert_eq!(image_ext(b"GIF89a..."), Some("gif"));
        assert_eq!(image_ext(b"RIFF\0\0\0\0WEBPVP8 "), Some("webp"));
        assert_eq!(image_ext(b"<html>"), None);
    }
}
