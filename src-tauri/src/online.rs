//! Recherche de sons sur MyInstants.
//!
//! Le site n'a pas d'API publique : on lit la page de résultats. Chaque
//! résultat contient un `play('/media/sounds/xxx.mp3', …)` suivi du lien
//! vers la page du son, dont le texte est le nom.

use regex::Regex;
use serde::Serialize;
use std::sync::OnceLock;
use std::time::Duration;

const SITE: &str = "https://www.myinstants.com";
const MAX_DOWNLOAD: usize = 20 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct OnlineSound {
    pub name: String,
    pub url: String,
}

fn client() -> Result<reqwest::Client, String> {
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

/// Télécharge un son trouvé par `search`. Seuls les fichiers du site sont acceptés.
pub async fn download(url: &str) -> Result<Vec<u8>, String> {
    if !url.starts_with(&format!("{SITE}/media/sounds/")) {
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
}
