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

/// Mots à chercher sur YouTube pour un lien Spotify : « Artiste Titre ».
pub async fn track_query(url: &str) -> Result<String, String> {
    if !is_spotify(url) {
        return Err("ce n'est pas un lien Spotify".into());
    }
    // Une page comme un navigateur la reçoit (titre et artiste dedans).
    let client = reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0 Safari/537.36")
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| e.to_string())?;
    let (mut final_url, mut html) = get(&client, url.trim()).await?;
    if !final_url.contains("/track/") {
        if let Some(link) = track_link(&html) {
            (final_url, html) = get(&client, &link).await?;
        }
    }
    if !final_url.contains("/track/") {
        return Err("colle le lien d'un morceau (pas d'un album, d'une playlist ou d'un podcast)".into());
    }
    let (song, artist) = parse_track(&html).ok_or("titre du morceau illisible sur la page Spotify")?;
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
    fn finds_track_in_short_link_page() {
        let html = r#"<script>window.location="https://open.spotify.com/track/4cOdK2wGLETKBW3PvgPWqT?si=1"</script>"#;
        assert_eq!(track_link(html).as_deref(), Some("https://open.spotify.com/track/4cOdK2wGLETKBW3PvgPWqT"));
    }
}
