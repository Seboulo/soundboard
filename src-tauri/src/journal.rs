//! Petit journal d'erreurs dans `journal.log` (dossier de données de l'app),
//! pour garder une trace des plantages et des sons qui refusent de jouer.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

/// Au-delà, l'ancien journal devient `journal.old.log`.
const MAX_BYTES: u64 = 512 * 1024;

static PATH: OnceLock<PathBuf> = OnceLock::new();
static LOCK: Mutex<()> = Mutex::new(());

/// À appeler une fois au démarrage. Les plantages sont aussi notés.
pub fn init(dir: PathBuf) {
    let _ = fs::create_dir_all(&dir);
    let _ = PATH.set(dir.join("journal.log"));
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        write(&format!("PLANTAGE ({}) : {info}", thread.name().unwrap_or("?")));
        previous(info);
    }));
    write(&format!("--- Démarrage v{} ---", env!("CARGO_PKG_VERSION")));
}

pub fn write(message: &str) {
    let Some(path) = PATH.get() else { return };
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if fs::metadata(path).is_ok_and(|m| m.len() > MAX_BYTES) {
        let _ = fs::rename(path, path.with_extension("old.log"));
    }
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "[{}] {message}", timestamp());
    }
}

/// Les dernières lignes du journal, pour le rapport.
pub fn tail(lines: usize) -> String {
    let Some(path) = PATH.get() else { return String::new() };
    let text = fs::read_to_string(path).unwrap_or_default();
    let all: Vec<&str> = text.lines().collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

/// Date et heure UTC lisibles, sans dépendance de plus.
fn timestamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (days, rem) = (secs / 86_400, secs % 86_400);
    // Conversion jours -> date (algorithme de Howard Hinnant).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y}-{m:02}-{d:02} {:02}:{:02}:{:02} UTC", rem / 3600, rem % 3600 / 60, rem % 60)
}

#[cfg(test)]
mod tests {
    #[test]
    fn timestamp_format() {
        let t = super::timestamp();
        assert_eq!(t.len(), 23, "{t}");
        assert!(t.starts_with("20"));
    }
}
