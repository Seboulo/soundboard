//! Salon entre potes : un pote joue un son chez toi depuis son appli.
//!
//! Les apps d'un même salon (même code) s'échangent de petits messages par
//! ntfy.sh, un relais public gratuit sans compte. Le code n'y passe pas en
//! clair : le nom du canal est une empreinte du code.
//!
//! Pour jouer un son chez un pote, l'app envoie son empreinte. Si le pote
//! l'a déjà (pack partagé), il le joue tout de suite ; sinon il le demande
//! et le fichier lui est envoyé (pièce jointe gardée 3 h au plus par ntfy).

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const SERVER: &str = "https://ntfy.sh";
/// Limite des pièces jointes de ntfy.sh.
pub const MAX_ATTACHMENT: u64 = 15 * 1024 * 1024;
/// Un pote disparaît de la liste après 40 min sans nouvelles.
const MEMBER_TTL: Duration = Duration::from_secs(2400);
/// On redit « je suis là » toutes les 15 min (ntfy.sh limite le nombre de
/// messages par jour : une app ouverte toute la journée en envoie ~100).
const HELLO_EVERY: Duration = Duration::from_secs(900);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Msg {
    /// « Je suis là » ; `reply` : réponse à un nouveau venu (pas de nouvelle réponse).
    Hi { from: String, name: String, #[serde(default)] reply: bool },
    Bye { from: String },
    /// Joue ce son chez `to`.
    Play {
        from: String,
        name: String,
        to: String,
        hash: String,
        title: String,
        #[serde(default)]
        ext: String,
        #[serde(default)]
        start_ms: u64,
        #[serde(default)]
        end_ms: Option<u64>,
        #[serde(default = "one")]
        volume: f32,
        #[serde(default)]
        fx: crate::sound_fx::SoundFx,
    },
    /// « Je n'ai pas ce son, envoie-le. »
    Need { from: String, to: String, hash: String },
    /// « Arrête ce que je t'ai envoyé » (son et extrait) ; `to` vide : tout le salon.
    Stop { from: String, #[serde(default)] to: String },
    /// Extrait à afficher sur l'écran de `to` (vide : tout le salon), en pièce jointe.
    Clip { from: String, name: String, #[serde(default)] to: String, #[serde(default)] title: String },
}

fn one() -> f32 {
    1.0
}

impl Msg {
    pub fn from(&self) -> &str {
        match self {
            Msg::Hi { from, .. } | Msg::Bye { from } | Msg::Play { from, .. } | Msg::Need { from, .. } | Msg::Clip { from, .. } | Msg::Stop { from, .. } => from,
        }
    }
}

/// Ce que reçoit l'app : le message, et la pièce jointe s'il y en a une.
pub struct Incoming {
    pub msg: Msg,
    pub attachment: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Member {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RoomStatus {
    pub on: bool,
    pub connected: bool,
    pub members: Vec<Member>,
    /// Pote chez qui les sons sont joués (None : chez moi).
    pub target: Option<String>,
}

#[derive(Default)]
struct Inner {
    topic: String,
    my_id: String,
    my_name: String,
    connected: bool,
    members: HashMap<String, (String, Instant)>,
    target: Option<String>,
    last_hello: Option<Instant>,
    /// Extrait envoyé « moi compris » : on affiche aussi le sien en retour.
    echo_until: Option<Instant>,
}

/// Le salon, partagé dans l'app.
#[derive(Clone, Default)]
pub struct Room {
    inner: Arc<Mutex<Inner>>,
    /// Change à chaque nouveau code : l'ancienne connexion s'arrête.
    gen: Arc<AtomicU64>,
}

/// Canal ntfy d'un code de salon.
pub fn topic_of(code: &str) -> String {
    let code = normalize_code(code);
    let h = Sha256::digest(format!("soundboard-salon:{code}").as_bytes());
    format!("sbsalon_{}", crate::share::hex(&h[..12]))
}

pub fn normalize_code(code: &str) -> String {
    code.trim().to_uppercase().chars().filter(|c| c.is_ascii_alphanumeric()).collect()
}

/// Nouveau code, facile à dicter : 12 lettres et chiffres sans ambiguïté.
pub fn new_code() -> String {
    const A: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let bytes = uuid::Uuid::new_v4().into_bytes();
    let s: String = bytes.iter().take(12).map(|b| A[*b as usize % A.len()] as char).collect();
    format!("{}-{}-{}", &s[..4], &s[4..8], &s[8..])
}

/// Envoi ou réception d'un fichier (son, extrait).
const FILE_TIMEOUT: Duration = Duration::from_secs(240);

fn client(streaming: bool) -> Result<reqwest::Client, String> {
    let b = reqwest::Client::builder()
        .user_agent(concat!("Soundboard/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(15));
    // ntfy envoie un « keepalive » toutes les 45 s : au-delà, la connexion est morte.
    let b = if streaming { b.read_timeout(Duration::from_secs(120)) } else { b.timeout(Duration::from_secs(60)) };
    b.build().map_err(|e| e.to_string())
}

impl Room {
    pub fn status(&self) -> RoomStatus {
        let mut inner = self.inner.lock().unwrap();
        inner.members.retain(|_, (_, seen)| seen.elapsed() < MEMBER_TTL);
        let mut members: Vec<Member> =
            inner.members.iter().map(|(id, (name, _))| Member { id: id.clone(), name: name.clone() }).collect();
        members.sort_by_key(|m| m.name.to_lowercase());
        if inner.target.as_ref().is_some_and(|t| !inner.members.contains_key(t)) {
            inner.target = None;
        }
        RoomStatus { on: !inner.topic.is_empty(), connected: inner.connected, members, target: inner.target.clone() }
    }

    /// Le prochain extrait qu'on envoie nous revient aussi (pour tester),
    /// s'il arrive dans les 2 minutes.
    pub fn expect_echo(&self) {
        self.inner.lock().unwrap().echo_until = Some(Instant::now() + Duration::from_secs(120));
    }

    pub fn my_id(&self) -> String {
        self.inner.lock().unwrap().my_id.clone()
    }
    pub fn my_name(&self) -> String {
        self.inner.lock().unwrap().my_name.clone()
    }

    pub fn target(&self) -> Option<String> {
        self.status().target
    }

    pub fn set_target(&self, target: Option<String>) {
        let mut inner = self.inner.lock().unwrap();
        inner.target = target.filter(|t| inner.members.contains_key(t));
    }

    /// Rejoint le salon (code vide : quitte). `on_event` reçoit les
    /// messages des potes ; `on_change` est appelé quand la liste change.
    pub fn start(
        &self,
        code: &str,
        my_id: String,
        my_name: String,
        on_event: impl Fn(Incoming) + Send + Sync + 'static,
        on_change: impl Fn() + Send + Sync + 'static,
    ) {
        let gen = self.gen.fetch_add(1, Ordering::SeqCst) + 1;
        let old_topic = {
            let mut inner = self.inner.lock().unwrap();
            let old = std::mem::take(&mut inner.topic);
            let old_id = inner.my_id.clone();
            *inner = Inner { my_id: my_id.clone(), my_name: my_name.clone(), ..Default::default() };
            if !normalize_code(code).is_empty() {
                inner.topic = topic_of(code);
            }
            (old, old_id)
        };
        if !old_topic.0.is_empty() {
            let me = self.clone();
            tauri::async_runtime::spawn(async move {
                let _ = me.publish_to(&old_topic.0, &Msg::Bye { from: old_topic.1 }).await;
            });
        }
        on_change();
        if normalize_code(code).is_empty() {
            return;
        }
        let me = self.clone();
        let on_event = Arc::new(on_event);
        let on_change = Arc::new(on_change);
        tauri::async_runtime::spawn(async move {
            while me.gen.load(Ordering::SeqCst) == gen {
                if let Err(e) = me.listen(gen, on_event.clone(), on_change.clone()).await {
                    crate::journal::write(&format!("Salon : {e}"));
                }
                me.inner.lock().unwrap().connected = false;
                on_change();
                tokio_sleep(Duration::from_secs(5)).await;
            }
        });
    }

    async fn listen(
        &self,
        gen: u64,
        on_event: Arc<impl Fn(Incoming) + Send + Sync + 'static>,
        on_change: Arc<impl Fn() + Send + Sync + 'static>,
    ) -> Result<(), String> {
        let topic = self.inner.lock().unwrap().topic.clone();
        let mut res = client(true)?
            .get(format!("{SERVER}/{topic}/json"))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !res.status().is_success() {
            return Err(format!("relais : erreur {}", res.status().as_u16()));
        }
        self.inner.lock().unwrap().connected = true;
        self.hello(false).await;
        on_change();
        let mut line = Vec::new();
        loop {
            if self.gen.load(Ordering::SeqCst) != gen {
                return Ok(());
            }
            let Some(chunk) = res.chunk().await.map_err(|e| e.to_string())? else {
                return Err("connexion fermée".into());
            };
            for &b in chunk.iter() {
                if b != b'\n' {
                    line.push(b);
                    continue;
                }
                let text = String::from_utf8_lossy(&line).into_owned();
                line.clear();
                if let Some(incoming) = parse_line(&text) {
                    self.receive(incoming, &*on_event, &*on_change).await;
                }
            }
            let stale = self.inner.lock().unwrap().last_hello.is_none_or(|t| t.elapsed() > HELLO_EVERY);
            if stale {
                self.hello(false).await;
            }
        }
    }

    async fn receive(&self, incoming: Incoming, on_event: &impl Fn(Incoming), on_change: &impl Fn()) {
        let me = self.my_id();
        if incoming.msg.from() == me {
            // Son propre extrait, envoyé « moi compris » : on le reçoit
            // comme les potes, par le même chemin.
            let echo = matches!(incoming.msg, Msg::Clip { .. }) && {
                let mut inner = self.inner.lock().unwrap();
                inner.echo_until.take().is_some_and(|t| Instant::now() < t)
            };
            if echo {
                on_event(incoming);
            }
            return;
        }
        match &incoming.msg {
            Msg::Hi { from, name, reply } => {
                let new = {
                    let mut inner = self.inner.lock().unwrap();
                    let name: String = name.chars().take(32).collect();
                    inner.members.insert(from.clone(), (name, Instant::now())).is_none()
                };
                // Un nouveau venu ne connaît personne : on se présente.
                if !reply && new {
                    self.hello(true).await;
                }
                on_change();
            }
            Msg::Bye { from } => {
                self.inner.lock().unwrap().members.remove(from);
                on_change();
            }
            Msg::Play { to, from, name, .. } if *to == me => {
                {
                    let mut inner = self.inner.lock().unwrap();
                    let name: String = name.chars().take(32).collect();
                    inner.members.insert(from.clone(), (name, Instant::now()));
                }
                on_event(incoming);
            }
            Msg::Clip { to, from, name, .. } if to.is_empty() || *to == me => {
                {
                    let mut inner = self.inner.lock().unwrap();
                    let name: String = name.chars().take(32).collect();
                    inner.members.insert(from.clone(), (name, Instant::now()));
                }
                on_event(incoming);
            }
            Msg::Need { to, .. } if *to == me => on_event(incoming),
            Msg::Stop { to, .. } if to.is_empty() || *to == me => on_event(incoming),
            _ => {}
        }
    }

    async fn hello(&self, reply: bool) {
        let (from, name) = {
            let mut inner = self.inner.lock().unwrap();
            inner.last_hello = Some(Instant::now());
            (inner.my_id.clone(), inner.my_name.clone())
        };
        let _ = self.publish(&Msg::Hi { from, name, reply }).await;
    }

    pub async fn publish(&self, msg: &Msg) -> Result<(), String> {
        let topic = self.inner.lock().unwrap().topic.clone();
        if topic.is_empty() {
            return Err("tu n'es dans aucun salon".into());
        }
        self.publish_to(&topic, msg).await
    }

    async fn publish_to(&self, topic: &str, msg: &Msg) -> Result<(), String> {
        let body = serde_json::to_string(msg).map_err(|e| e.to_string())?;
        let res = client(false)?
            .post(format!("{SERVER}/{topic}"))
            .body(body)
            .send()
            .await
            .map_err(|e| format!("relais injoignable ({e})"))?;
        if !res.status().is_success() {
            return Err(format!("relais : erreur {}", res.status().as_u16()));
        }
        Ok(())
    }

    /// Envoie le son en pièce jointe, avec le message qui dit quoi en faire.
    pub async fn publish_file(&self, msg: &Msg, file_name: &str, data: Vec<u8>) -> Result<(), String> {
        let topic = self.inner.lock().unwrap().topic.clone();
        if topic.is_empty() {
            return Err("tu n'es dans aucun salon".into());
        }
        let body = serde_json::to_string(msg).map_err(|e| e.to_string())?;
        let mut url = reqwest::Url::parse(&format!("{SERVER}/{topic}")).map_err(|e| e.to_string())?;
        url.query_pairs_mut().append_pair("message", &body).append_pair("filename", file_name);
        // Un extrait de 14 Mo met plus d'une minute à partir avec une petite
        // connexion : on lui laisse 4 minutes.
        let res = client(false)?
            .put(url)
            .timeout(FILE_TIMEOUT)
            .body(data)
            .send()
            .await
            .map_err(|e| format!("relais injoignable ({e})"))?;
        match res.status().as_u16() {
            200..=299 => Ok(()),
            413 => Err("le relais refuse ce fichier : trop lourd, ou limite d'envois atteinte (environ 100 Mo toutes les 3 h). Réessaie plus tard, ou envoie un passage plus court ou une image".into()),
            429 => Err("trop d'envois d'un coup pour le relais : réessaie dans quelques minutes".into()),
            code => Err(format!("envoi refusé par le relais (erreur {code})")),
        }
    }
}

/// Une ligne du flux ntfy.
fn parse_line(line: &str) -> Option<Incoming> {
    #[derive(Deserialize)]
    struct Line {
        event: String,
        #[serde(default)]
        message: String,
        #[serde(default)]
        attachment: Option<Attachment>,
    }
    #[derive(Deserialize)]
    struct Attachment {
        url: String,
    }
    let l: Line = serde_json::from_str(line).ok()?;
    if l.event != "message" {
        return None;
    }
    let msg: Msg = serde_json::from_str(&l.message).ok()?;
    let attachment = l.attachment.map(|a| a.url).filter(|u| u.starts_with(SERVER));
    Some(Incoming { msg, attachment })
}

/// Télécharge une pièce jointe (taille limitée).
pub async fn fetch(url: &str) -> Result<Vec<u8>, String> {
    if !url.starts_with(SERVER) {
        return Err("adresse non autorisée".into());
    }
    let mut res = client(false)?.get(url).timeout(FILE_TIMEOUT).send().await.map_err(|e| e.to_string())?;
    if !res.status().is_success() {
        return Err(format!("erreur {}", res.status().as_u16()));
    }
    let mut data = Vec::new();
    while let Some(chunk) = res.chunk().await.map_err(|e| e.to_string())? {
        data.extend_from_slice(&chunk);
        if data.len() as u64 > MAX_ATTACHMENT {
            return Err("fichier trop gros".into());
        }
    }
    Ok(data)
}

async fn tokio_sleep(d: Duration) {
    let _ = tauri::async_runtime::spawn_blocking(move || std::thread::sleep(d)).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_code_same_topic_whatever_the_typing() {
        assert_eq!(topic_of("abcd-efgh-2345"), topic_of(" ABCDEFGH2345 "));
        assert_ne!(topic_of("ABCD-EFGH-2345"), topic_of("ABCD-EFGH-2346"));
        assert!(!topic_of("x").contains("X"));
    }

    #[test]
    fn new_codes_are_readable() {
        let c = new_code();
        assert_eq!(c.len(), 14);
        assert_eq!(normalize_code(&c).len(), 12);
        assert!(!c.contains('O') && !c.contains('0') && !c.contains('I') && !c.contains('1'));
    }

    #[test]
    fn ntfy_lines_are_parsed() {
        let msg = Msg::Need { from: "a".into(), to: "b".into(), hash: "h".into() };
        let line = serde_json::json!({
            "id": "x", "time": 1, "event": "message", "topic": "t",
            "message": serde_json::to_string(&msg).unwrap(),
            "attachment": {"name": "s.mp3", "url": "https://ntfy.sh/file/abc.mp3"}
        })
        .to_string();
        let got = parse_line(&line).unwrap();
        assert_eq!(got.msg, msg);
        assert_eq!(got.attachment.as_deref(), Some("https://ntfy.sh/file/abc.mp3"));
        assert!(parse_line(r#"{"event":"keepalive"}"#).is_none());
        assert!(parse_line(r#"{"event":"message","message":"bonjour"}"#).is_none());
        // Une pièce jointe hébergée ailleurs est ignorée.
        let evil = line.replace("https://ntfy.sh/file", "https://ailleurs.fr");
        assert!(parse_line(&evil).unwrap().attachment.is_none());
    }
}
