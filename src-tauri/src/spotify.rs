//! Liens Spotify : le son de Spotify est protégé (DRM) et n'est jamais
//! récupéré. On lit seulement le titre et l'artiste affichés sur la page
//! publique du morceau, puis le morceau est cherché sur YouTube.

use regex::Regex;
use std::sync::OnceLock;
use std::time::Duration;

/// Lien Spotify (page d'un morceau, ou lien court partagé depuis l'appli).
pub fn is_spotify(url: &str) -> bool {
    let Ok(u) = reqwest::Url::parse(url.trim()) else { return false };
    let host = u.host_str().unwrap_or("").to_ascii_lowercase();
    matches!(u.scheme(), "http" | "https")
        && (host == "open.spotify.com" || host == "spotify.link" || host.ends_with(".spotify.link") || host == "spotify.app.link")
}

fn decode_entities(s: &str) -> String {
    let s = s
        .replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", " ");
    // En dernier, pour ne pas décoder deux fois « &amp;quot; ».
    s.replace("&amp;", "&").trim().to_string()
}

fn meta(html: &str, key: &str) -> Option<String> {
    // L'ordre des attributs varie : property/name avant ou après content.
    let k = regex::escape(key);
    let before = Regex::new(&format!(r#"<meta[^>]+(?:property|name)="{k}"[^>]+content="([^"]*)""#)).ok()?;
    let after = Regex::new(&format!(r#"<meta[^>]+content="([^"]*)"[^>]+(?:property|name)="{k}""#)).ok()?;
    before
        .captures(html)
        .or_else(|| after.captures(html))
        .map(|c| decode_entities(&c[1]))
        .filter(|v| !v.is_empty())
}

/// Titre et artiste lus sur la page d'un morceau.
pub fn parse_track(html: &str) -> Option<(String, String)> {
    static TITLE: OnceLock<Regex> = OnceLock::new();
    let title_re = TITLE.get_or_init(|| Regex::new(r"(?s)<title>(.*?)</title>").unwrap());
    // « Titre - song and lyrics by Artiste | Spotify »
    let from_title = title_re.captures(html).and_then(|c| {
        let t = decode_entities(&c[1]);
        let t = t.trim_end_matches("| Spotify").trim();
        let (song, artist) = t.split_once(" - song and lyrics by ").or_else(|| t.split_once(" - song by "))?;
        Some((song.trim().to_string(), artist.trim().to_string()))
    });
    let song = meta(html, "og:title").or_else(|| from_title.as_ref().map(|t| t.0.clone()))?;
    // « Artiste · Album · Song · 2020 » (ancien format : « Listen to … on Spotify. Artiste · … »)
    let artist = meta(html, "music:musician_description")
        .or_else(|| {
            let d = meta(html, "og:description")?;
            let d = d.split_once("on Spotify. ").map_or(d.as_str(), |x| x.1);
            d.split(" · ").next().map(|a| a.trim().to_string()).filter(|a| !a.is_empty())
        })
        .or_else(|| from_title.map(|t| t.1))
        .unwrap_or_default();
    Some((song, artist))
}

/// Un lien de morceau caché dans une page (lien court qui redirige en JS).
fn track_link(html: &str) -> Option<String> {
    static LINK: OnceLock<Regex> = OnceLock::new();
    let re = LINK.get_or_init(|| Regex::new(r"https://open\.spotify\.com/(?:intl-[a-z-]+/)?track/[A-Za-z0-9]{10,}").unwrap());
    re.find(html).map(|m| m.as_str().to_string())
}

async fn get(client: &reqwest::Client, url: &str) -> Result<(String, String), String> {
    let res = client.get(url).send().await.map_err(|e| format!("Spotify ne répond pas : {e}"))?;
    let final_url = res.url().to_string();
    let html = res
        .error_for_status()
        .map_err(|_| "morceau introuvable sur Spotify".to_string())?
        .text()
        .await
        .map_err(|e| e.to_string())?;
    Ok((final_url, html))
}

/// Identifiant du morceau dans un lien « …/track/<id> ».
fn track_id(url: &str) -> Option<String> {
    static ID: OnceLock<Regex> = OnceLock::new();
    let re = ID.get_or_init(|| Regex::new(r"/track/([A-Za-z0-9]{10,})").unwrap());
    re.captures(url).map(|c| c[1].to_string())
}

/// Titre et artiste dans le lecteur intégré (« /embed/track/<id> ») : ses
/// données sont en JSON dans la page, plus stables que la page normale.
pub fn parse_embed(html: &str) -> Option<(String, String)> {
    static DATA: OnceLock<Regex> = OnceLock::new();
    let re = DATA.get_or_init(|| Regex::new(r#"(?s)<script[^>]+id="__NEXT_DATA__"[^>]*>(.*?)</script>"#).unwrap());
    let json: serde_json::Value = serde_json::from_str(re.captures(html)?.get(1)?.as_str()).ok()?;
    fn find(v: &serde_json::Value) -> Option<(String, String)> {
        match v {
            serde_json::Value::Object(o) => {
                let name = o.get("name").or(o.get("title")).and_then(|n| n.as_str()).map(str::trim);
                if let (Some(name), Some(artists)) = (name, o.get("artists").and_then(|a| a.as_array())) {
                    if !name.is_empty() {
                        let artist: Vec<_> = artists.iter().filter_map(|a| a.get("name")?.as_str()).collect();
                        return Some((name.to_string(), artist.join(" ")));
                    }
                }
                o.values().find_map(find)
            }
            serde_json::Value::Array(a) => a.iter().find_map(find),
            _ => None,
        }
    }
    find(&json)
}

/// Dernier recours : l'API publique « oEmbed » donne au moins le titre.
async fn oembed_title(client: &reqwest::Client, id: &str) -> Option<String> {
    let url = format!("https://open.spotify.com/oembed?url=https://open.spotify.com/track/{id}");
    let body = client.get(url).send().await.ok()?.error_for_status().ok()?.text().await.ok()?;
    let v: serde_json::Value = serde_json::from_str(&body).ok()?;
    v.get("title")?.as_str().map(|t| t.trim().to_string()).filter(|t| !t.is_empty())
}

/// Mots à chercher sur YouTube pour un lien Spotify : « Artiste Titre ».
/// La page du morceau d'abord, puis le lecteur intégré, puis oEmbed : Spotify
/// change souvent ses pages, une seule source ne suffit pas.
pub async fn track_query(url: &str) -> Result<String, String> {
    if !is_spotify(url) {
        return Err("ce n'est pas un lien Spotify".into());
    }
    let client = reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0 Safari/537.36")
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| e.to_string())?;
    let url = url.trim();
    let mut id = track_id(url);
    let mut found = None;
    match get(&client, url).await {
        Ok((final_url, html)) => {
            // Lien court : il redirige (parfois en JS) vers le morceau.
            if id.is_none() {
                id = track_id(&final_url).or_else(|| track_link(&html).and_then(|l| track_id(&l)));
            }
            if final_url.contains("/track/") {
                found = parse_track(&html);
            }
        }
        Err(e) if id.is_none() => return Err(e),
        Err(_) => {}
    }
    let Some(id) = id else {
        return Err("colle le lien d'un morceau (pas d'un album, d'une playlist ou d'un podcast)".into());
    };
    if found.is_none() {
        if let Ok((_, html)) = get(&client, &format!("https://open.spotify.com/embed/track/{id}")).await {
            found = parse_embed(&html).or_else(|| parse_track(&html));
        }
    }
    let (song, artist) = match found {
        Some(t) => t,
        None => {
            let title = oembed_title(&client, &id).await;
            (title.ok_or("Spotify n'a pas donné le titre du morceau, réessaie dans un moment")?, String::new())
        }
    };
    Ok(format!("{artist} {song}").trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_spotify_links() {
        assert!(is_spotify("https://open.spotify.com/track/4cOdK2wGLETKBW3PvgPWqT?si=abc"));
        assert!(is_spotify("https://open.spotify.com/intl-fr/track/4cOdK2wGLETKBW3PvgPWqT"));
        assert!(is_spotify("https://spotify.link/AbCdEf"));
        assert!(!is_spotify("https://www.youtube.com/watch?v=x"));
        assert!(!is_spotify("https://open.spotify.com.evil.com/track/x"));
    }

    #[test]
    fn reads_title_and_artist() {
        let html = r#"<html><head><title>Never Gonna Give You Up - song and lyrics by Rick Astley | Spotify</title>
<meta property="og:title" content="Never Gonna Give You Up"/>
<meta property="og:description" content="Rick Astley · Whenever You Need Somebody · Song · 1987"/>"#;
        assert_eq!(parse_track(html), Some(("Never Gonna Give You Up".into(), "Rick Astley".into())));
    }

    #[test]
    fn falls_back_to_page_title_and_decodes() {
        let html = "<title>Don&#x27;t Stop Me Now - Remastered 2011 - song by Queen | Spotify</title>";
        assert_eq!(parse_track(html), Some(("Don't Stop Me Now - Remastered 2011".into(), "Queen".into())));
        let old = r#"<meta content="Listen to Bruh on Spotify. Meme &amp; Co · Song · 2019." property="og:description"><meta property="og:title" content="Bruh">"#;
        assert_eq!(parse_track(old), Some(("Bruh".into(), "Meme & Co".into())));
    }

    #[test]
    fn reads_embed_data() {
        let html = r#"<script id="__NEXT_DATA__" type="application/json">{"props":{"pageProps":{"state":{"data":{"entity":{"type":"track","name":"Bruh","artists":[{"name":"Meme","uri":"x"},{"name":"Co"}]}}}}}}</script>"#;
        assert_eq!(parse_embed(html), Some(("Bruh".into(), "Meme Co".into())));
        assert_eq!(track_id("https://open.spotify.com/intl-fr/track/4cOdK2wGLETKBW3PvgPWqT?si=1").as_deref(), Some("4cOdK2wGLETKBW3PvgPWqT"));
    }

    #[test]
    fn finds_track_in_short_link_page() {
        let html = r#"<script>window.location="https://open.spotify.com/track/4cOdK2wGLETKBW3PvgPWqT?si=1"</script>"#;
        assert_eq!(track_link(html).as_deref(), Some("https://open.spotify.com/track/4cOdK2wGLETKBW3PvgPWqT"));
    }
}
