//! Taille et place de la fenêtre principale, retrouvées au lancement suivant.
//!
//! Gardées dans `fenetre.json` (dossier de l'app). Si l'écran où elle était
//! a disparu (second écran débranché), on garde la place par défaut.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, PhysicalPosition, PhysicalSize, WebviewWindow};

const FILE: &str = "fenetre.json";

/// Fenêtre agrandie la dernière fois : on l'agrandit à la première ouverture.
static MAXIMIZE_ON_SHOW: AtomicBool = AtomicBool::new(false);

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
struct Geometry {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    #[serde(default)]
    maximized: bool,
}

fn file(app: &AppHandle) -> Option<PathBuf> {
    app.path().app_data_dir().ok().map(|d| d.join(FILE))
}

fn load(app: &AppHandle) -> Option<Geometry> {
    serde_json::from_slice(&std::fs::read(file(app)?).ok()?).ok()
}

/// La barre de titre doit tomber sur un écran branché, sinon on ne pourrait
/// plus attraper la fenêtre.
fn title_bar_visible(g: &Geometry, screens: &[(i32, i32, u32, u32)]) -> bool {
    let px = g.x + (g.width as i32 / 2).min(120);
    let py = g.y + 16;
    screens
        .iter()
        .any(|&(x, y, w, h)| px >= x && px < x + w as i32 && py >= y && py < y + h as i32)
}

/// Au lancement, fenêtre encore cachée : on remet la taille et la place d'avant.
pub fn restore(window: &WebviewWindow) {
    let Some(g) = load(window.app_handle()) else { return };
    let screens: Vec<_> = window
        .available_monitors()
        .unwrap_or_default()
        .iter()
        .map(|m| (m.position().x, m.position().y, m.size().width, m.size().height))
        .collect();
    if g.width >= 200 && g.height >= 150 && title_bar_visible(&g, &screens) {
        let _ = window.set_size(PhysicalSize::new(g.width, g.height));
        let _ = window.set_position(PhysicalPosition::new(g.x, g.y));
    }
    MAXIMIZE_ON_SHOW.store(g.maximized, Ordering::Relaxed);
}

/// Appelé juste après l'affichage de la fenêtre.
pub fn after_show(window: &WebviewWindow) {
    if MAXIMIZE_ON_SHOW.swap(false, Ordering::Relaxed) {
        let _ = window.maximize();
    }
}

/// À la fermeture (ou au passage dans la barre des tâches) : on retient tout.
pub fn save(window: &WebviewWindow) {
    if !window.is_visible().unwrap_or(false) || window.is_minimized().unwrap_or(false) {
        return;
    }
    let app = window.app_handle();
    let maximized = window.is_maximized().unwrap_or(false);
    let g = if maximized {
        // Agrandie : on garde la taille « normale » d'avant pour la suite.
        match load(app) {
            Some(old) => Geometry { maximized: true, ..old },
            None => return,
        }
    } else {
        let (Ok(pos), Ok(size)) = (window.outer_position(), window.inner_size()) else { return };
        Geometry { x: pos.x, y: pos.y, width: size.width, height: size.height, maximized: false }
    };
    if load(app) == Some(g) {
        return;
    }
    if let (Some(path), Ok(json)) = (file(app), serde_json::to_vec(&g)) {
        let _ = std::fs::write(path, json);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_bar_must_be_on_a_screen() {
        let screens = [(0, 0, 1920, 1080)];
        let g = |x, y| Geometry { x, y, width: 960, height: 680, maximized: false };
        assert!(title_bar_visible(&g(100, 100), &screens));
        // Second écran débranché, à droite.
        assert!(!title_bar_visible(&g(2000, 100), &screens));
        // Au-dessus de l'écran : barre de titre inaccessible.
        assert!(!title_bar_visible(&g(100, -200), &screens));
        // Second écran à gauche (coordonnées négatives) toujours branché.
        assert!(title_bar_visible(&g(-1500, 100), &[(-1920, 0, 1920, 1080), (0, 0, 1920, 1080)]));
    }
}
