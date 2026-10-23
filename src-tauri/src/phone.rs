//! Contrôle depuis un téléphone : une petite page web servie sur le réseau
//! local. L'adresse contient un code secret (`t=`) sans lequel rien ne
//! répond, pour que les autres appareils du réseau ne puissent pas jouer de
//! sons.

use serde_json::Value;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use tiny_http::{Header, Method, Response, Server};

const PAGE: &str = include_str!("../../ui/phone.html");

/// Ce que la page du téléphone demande à l'app.
#[derive(Debug, PartialEq)]
pub enum Request {
    /// Sons, onglets et sons en cours.
    State,
    Play(String),
    Stop,
    Random(Option<String>),
}

pub type Handler = Arc<dyn Fn(Request) -> Result<Value, String> + Send + Sync>;

pub struct PhoneServer {
    server: Arc<Server>,
    thread: Option<JoinHandle<()>>,
}

impl PhoneServer {
    pub fn start(port: u16, token: String, handler: Handler) -> Result<Self, String> {
        let server = Arc::new(
            Server::http(("0.0.0.0", port))
                .map_err(|e| format!("téléphone : le port {port} est indisponible ({e})"))?,
        );
        let srv = server.clone();
        let thread = thread::Builder::new()
            .name("phone".into())
            .spawn(move || {
                for request in srv.incoming_requests() {
                    respond(request, &token, &handler);
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Self {
            server,
            thread: Some(thread),
        })
    }
}

impl Drop for PhoneServer {
    fn drop(&mut self) {
        self.server.unblock();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn header(name: &str, value: &str) -> Header {
    Header::from_bytes(name.as_bytes(), value.as_bytes()).expect("en-tête valide")
}

fn send(request: tiny_http::Request, status: u16, content_type: &str, body: String) {
    let response = Response::from_string(body)
        .with_status_code(status)
        .with_header(header("Content-Type", content_type))
        .with_header(header("Cache-Control", "no-store"));
    let _ = request.respond(response);
}

/// Sépare le chemin et les paramètres d'une adresse (`/api/play?t=…&id=…`).
pub fn parse_url(url: &str) -> (&str, Vec<(&str, &str)>) {
    let (path, query) = url.split_once('?').unwrap_or((url, ""));
    let params = query
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .collect();
    (path, params)
}

fn param<'a>(params: &[(&str, &'a str)], key: &str) -> Option<&'a str> {
    params.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

/// Compare sans s'arrêter au premier caractère différent.
fn same_token(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Décide quoi faire d'une requête ; `None` si la route n'existe pas.
pub fn route(method: &Method, url: &str, token: &str) -> Result<Option<Option<Request>>, ()> {
    let (path, params) = parse_url(url);
    if token.is_empty() || !param(&params, "t").is_some_and(|t| same_token(t, token)) {
        return Err(());
    }
    let action = match (method, path) {
        (Method::Get, "/") => Some(None),
        (Method::Get, "/api/state") => Some(Some(Request::State)),
        (Method::Post, "/api/play") => param(&params, "id")
            .filter(|id| id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
            .map(|id| Some(Request::Play(id.to_string()))),
        (Method::Post, "/api/stop") => Some(Some(Request::Stop)),
        (Method::Post, "/api/random") => Some(Some(Request::Random(
            param(&params, "cat").map(percent_decode).filter(|c| !c.is_empty()),
        ))),
        _ => None,
    };
    Ok(action)
}

fn respond(request: tiny_http::Request, token: &str, handler: &Handler) {
    let route = route(request.method(), request.url(), token);
    match route {
        Err(()) => send(
            request,
            403,
            "text/html; charset=utf-8",
            "<meta name=viewport content='width=device-width'><p style='font:16px sans-serif;padding:20px'>Code invalide : rescanne le QR code dans les réglages de la soundboard.</p>".into(),
        ),
        Ok(None) => send(request, 404, "text/plain; charset=utf-8", "introuvable".into()),
        Ok(Some(None)) => send(request, 200, "text/html; charset=utf-8", PAGE.into()),
        Ok(Some(Some(action))) => match handler(action) {
            Ok(value) => send(request, 200, "application/json", value.to_string()),
            Err(e) => send(request, 400, "application/json", serde_json::json!({ "error": e }).to_string()),
        },
    }
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
                if let Some(v) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    out.push(v);
                    i += 3;
                    continue;
                }
                out.push(b'%');
            }
            b'+' => out.push(b' '),
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Adresse à ouvrir sur le téléphone (IP du PC sur le réseau local).
pub fn page_url(port: u16, token: &str) -> Option<String> {
    let ip = local_ip_address::local_ip().ok()?;
    Some(format!("http://{ip}:{port}/?t={token}"))
}

/// QR code de l'adresse, en SVG.
pub fn qr_svg(url: &str) -> Result<String, String> {
    let code = qrcode::QrCode::new(url.as_bytes()).map_err(|e| e.to_string())?;
    Ok(code
        .render::<qrcode::render::svg::Color>()
        .min_dimensions(200, 200)
        .quiet_zone(true)
        .build())
}

pub fn new_token() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_need_the_token() {
        let t = "abc123";
        assert_eq!(route(&Method::Get, "/", t), Err(()));
        assert_eq!(route(&Method::Get, "/?t=wrong1", t), Err(()));
        assert_eq!(route(&Method::Get, "/?t=abc123", ""), Err(()));
        assert_eq!(route(&Method::Get, "/?t=abc123", t), Ok(Some(None)));
        assert_eq!(route(&Method::Get, "/api/state?t=abc123", t), Ok(Some(Some(Request::State))));
        assert_eq!(
            route(&Method::Post, "/api/play?t=abc123&id=1f2e-3d", t),
            Ok(Some(Some(Request::Play("1f2e-3d".into()))))
        );
        // Un identifiant bizarre est refusé.
        assert_eq!(route(&Method::Post, "/api/play?t=abc123&id=../x", t), Ok(None));
        // Jouer demande un POST.
        assert_eq!(route(&Method::Get, "/api/play?t=abc123&id=a", t), Ok(None));
        assert_eq!(
            route(&Method::Post, "/api/random?t=abc123&cat=M%C3%A8mes+fun", t),
            Ok(Some(Some(Request::Random(Some("Mèmes fun".into())))))
        );
        assert_eq!(route(&Method::Post, "/api/random?cat=&t=abc123", t), Ok(Some(Some(Request::Random(None)))));
    }

    #[test]
    fn makes_qr() {
        let svg = qr_svg("http://192.168.1.20:7878/?t=0123456789abcdef0123456789abcdef").unwrap();
        assert!(svg.starts_with("<?xml") || svg.starts_with("<svg"));
    }
}
