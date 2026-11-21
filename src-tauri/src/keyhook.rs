//! Raccourcis vus aussi pendant les jeux.
//!
//! `RegisterHotKey` (utilisé par le plugin global-shortcut) ne reçoit rien quand
//! le jeu au premier plan lit le clavier en « raw input » avec l'option qui
//! coupe les raccourcis des autres applis (League of Legends, entre autres).
//! On écoute donc aussi le clavier avec un crochet bas niveau (`WH_KEYBOARD_LL`),
//! comme Discord ou OBS pour leurs propres raccourcis : on lit seulement quelle
//! touche est enfoncée, sans toucher au jeu, sans bloquer la touche et sans
//! simuler de frappe.
//!
//! Les deux voies peuvent voir la même pression. Le crochet la voit toujours
//! en premier (Windows l'appelle avant de reconnaître le raccourci) : il note
//! l'heure, et `seen_by_hook` fait ignorer le doublon de l'autre voie.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use tauri_plugin_global_shortcut::{Code, Modifiers, Shortcut};

/// Le doublon d'une pression arrive en quelques millisecondes, ou plus tard
/// si la fenêtre est occupée : sans marge, le son démarrait puis s'arrêtait
/// aussitôt (deuxième appui = arrêt). Les appuis rapides passent quand même :
/// chacun est vu par le crochet.
const SAME_PRESS: Duration = Duration::from_millis(1500);

pub const MOD_CTRL: u8 = 1;
pub const MOD_SHIFT: u8 = 2;
pub const MOD_ALT: u8 = 4;
pub const MOD_WIN: u8 = 8;

/// Une combinaison telle que Windows la voit : modificateurs + code de touche.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Binding {
    pub mods: u8,
    pub vk: u32,
    pub id: u32,
}

pub fn binding(shortcut: &Shortcut) -> Option<Binding> {
    let m = shortcut.mods;
    let mut mods = 0;
    if m.contains(Modifiers::CONTROL) {
        mods |= MOD_CTRL;
    }
    if m.contains(Modifiers::SHIFT) {
        mods |= MOD_SHIFT;
    }
    if m.contains(Modifiers::ALT) {
        mods |= MOD_ALT;
    }
    if m.intersects(Modifiers::SUPER | Modifiers::META) {
        mods |= MOD_WIN;
    }
    Some(Binding {
        mods,
        vk: vk_of(shortcut.key)?,
        id: shortcut.id(),
    })
}

/// Mêmes codes que le plugin global-shortcut, pour que les deux voies
/// réagissent exactement aux mêmes touches.
fn vk_of(code: Code) -> Option<u32> {
    use Code::*;
    let letters = [
        KeyA, KeyB, KeyC, KeyD, KeyE, KeyF, KeyG, KeyH, KeyI, KeyJ, KeyK, KeyL, KeyM, KeyN, KeyO,
        KeyP, KeyQ, KeyR, KeyS, KeyT, KeyU, KeyV, KeyW, KeyX, KeyY, KeyZ,
    ];
    if let Some(i) = letters.iter().position(|c| *c == code) {
        return Some(0x41 + i as u32);
    }
    let digits = [
        Digit0, Digit1, Digit2, Digit3, Digit4, Digit5, Digit6, Digit7, Digit8, Digit9,
    ];
    if let Some(i) = digits.iter().position(|c| *c == code) {
        return Some(0x30 + i as u32);
    }
    let pad = [
        Numpad0, Numpad1, Numpad2, Numpad3, Numpad4, Numpad5, Numpad6, Numpad7, Numpad8, Numpad9,
    ];
    if let Some(i) = pad.iter().position(|c| *c == code) {
        return Some(0x60 + i as u32);
    }
    let fkeys = [
        F1, F2, F3, F4, F5, F6, F7, F8, F9, F10, F11, F12, F13, F14, F15, F16, F17, F18, F19, F20,
        F21, F22, F23, F24,
    ];
    if let Some(i) = fkeys.iter().position(|c| *c == code) {
        return Some(0x70 + i as u32);
    }
    Some(match code {
        Equal => 0xBB,
        Comma => 0xBC,
        Minus => 0xBD,
        Period => 0xBE,
        Semicolon => 0xBA,
        Slash => 0xBF,
        Backquote => 0xC0,
        BracketLeft => 0xDB,
        Backslash => 0xDC,
        BracketRight => 0xDD,
        Quote => 0xDE,
        Backspace => 0x08,
        Tab => 0x09,
        Space => 0x20,
        Enter | NumpadEnter => 0x0D,
        CapsLock => 0x14,
        Escape => 0x1B,
        PageUp => 0x21,
        PageDown => 0x22,
        End => 0x23,
        Home => 0x24,
        ArrowLeft => 0x25,
        ArrowUp => 0x26,
        ArrowRight => 0x27,
        ArrowDown => 0x28,
        PrintScreen => 0x2C,
        Insert => 0x2D,
        Delete => 0x2E,
        NumLock => 0x90,
        ScrollLock => 0x91,
        Pause => 0x13,
        NumpadMultiply => 0x6A,
        NumpadAdd => 0x6B,
        NumpadSubtract => 0x6D,
        NumpadDecimal => 0x6E,
        NumpadDivide => 0x6F,
        AudioVolumeMute => 0xAD,
        AudioVolumeDown => 0xAE,
        AudioVolumeUp => 0xAF,
        MediaTrackNext => 0xB0,
        MediaTrackPrevious => 0xB1,
        MediaStop => 0xB2,
        MediaPlayPause => 0xB3,
        _ => return None,
    })
}

/// Dernière pression de chaque raccourci vue par le crochet.
fn hook_seen() -> &'static Mutex<HashMap<u32, Instant>> {
    static SEEN: OnceLock<Mutex<HashMap<u32, Instant>>> = OnceLock::new();
    SEEN.get_or_init(Default::default)
}

/// Noté par le crochet, au moment même de la pression.
#[cfg_attr(not(windows), allow(dead_code))]
fn note_hook_press(id: u32) {
    if let Ok(mut seen) = hook_seen().lock() {
        seen.insert(id, Instant::now());
    }
}

/// Vrai si le crochet vient de voir ce raccourci : l'appel du raccourci
/// Windows est alors le doublon de la même pression.
pub fn seen_by_hook(id: u32) -> bool {
    hook_seen()
        .lock()
        .ok()
        .and_then(|seen| seen.get(&id).map(|t| t.elapsed() < SAME_PRESS))
        .unwrap_or(false)
}

static BINDINGS: Mutex<Vec<Binding>> = Mutex::new(Vec::new());

/// Remplace les combinaisons surveillées par le crochet.
pub fn set_bindings(list: Vec<Binding>) {
    *BINDINGS.lock().unwrap() = list;
}

fn matching(vk: u32, mods: u8) -> Option<u32> {
    BINDINGS
        .lock()
        .unwrap()
        .iter()
        .find(|b| b.vk == vk && b.mods == mods)
        .map(|b| b.id)
}

#[cfg(windows)]
pub fn start(on_press: impl Fn(u32) + Send + 'static) {
    win::start(Box::new(on_press));
}
#[cfg(not(windows))]
pub fn start(_on_press: impl Fn(u32) + Send + 'static) {}

#[cfg(windows)]
mod win {
    use std::collections::HashSet;
    use std::sync::mpsc::{channel, Sender};
    use std::sync::{Mutex, OnceLock};

    use super::{matching, note_hook_press, MOD_ALT, MOD_CTRL, MOD_SHIFT, MOD_WIN};

    #[repr(C)]
    struct KbdLlHook {
        vk: u32,
        scan: u32,
        flags: u32,
        time: u32,
        extra: usize,
    }
    #[repr(C)]
    struct Msg {
        hwnd: isize,
        message: u32,
        wparam: usize,
        lparam: isize,
        time: u32,
        pt: [i32; 2],
        private: u32,
    }

    #[link(name = "user32")]
    extern "system" {
        fn SetWindowsHookExW(id: i32, proc_: HookProc, hmod: isize, thread: u32) -> isize;
        fn CallNextHookEx(hook: isize, code: i32, wparam: usize, lparam: isize) -> isize;
        fn GetMessageW(msg: *mut Msg, hwnd: isize, min: u32, max: u32) -> i32;
        fn GetAsyncKeyState(vk: i32) -> i16;
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetModuleHandleW(name: *const u16) -> isize;
    }
    type HookProc = unsafe extern "system" fn(i32, usize, isize) -> isize;

    const WH_KEYBOARD_LL: i32 = 13;
    const WM_KEYDOWN: usize = 0x0100;
    const WM_KEYUP: usize = 0x0101;
    const WM_SYSKEYDOWN: usize = 0x0104;
    const WM_SYSKEYUP: usize = 0x0105;

    static SENDER: OnceLock<Mutex<Sender<u32>>> = OnceLock::new();
    /// Touches tenues : la répétition automatique ne relance pas le son.
    static HELD: Mutex<Option<HashSet<u32>>> = Mutex::new(None);

    fn down(vk: i32) -> bool {
        // SAFETY: lecture de l'état d'une touche, sans effet de bord.
        unsafe { GetAsyncKeyState(vk) as u16 & 0x8000 != 0 }
    }

    fn mods_now() -> u8 {
        let mut m = 0;
        if down(0x11) {
            m |= MOD_CTRL;
        }
        if down(0x10) {
            m |= MOD_SHIFT;
        }
        if down(0x12) {
            m |= MOD_ALT;
        }
        if down(0x5B) || down(0x5C) {
            m |= MOD_WIN;
        }
        m
    }

    unsafe extern "system" fn hook(code: i32, wparam: usize, lparam: isize) -> isize {
        if code >= 0 && lparam != 0 {
            // SAFETY: pour WH_KEYBOARD_LL, lparam pointe vers un KBDLLHOOKSTRUCT.
            let vk = (*(lparam as *const KbdLlHook)).vk;
            let mut held = HELD.lock().unwrap();
            let held = held.get_or_insert_with(HashSet::new);
            match wparam {
                WM_KEYDOWN | WM_SYSKEYDOWN => {
                    if held.insert(vk) {
                        if let Some(id) = matching(vk, mods_now()) {
                            note_hook_press(id);
                            if let Some(tx) = SENDER.get() {
                                let _ = tx.lock().unwrap().send(id);
                            }
                        }
                    }
                }
                WM_KEYUP | WM_SYSKEYUP => {
                    held.remove(&vk);
                }
                _ => {}
            }
        }
        // La touche continue toujours son chemin vers le jeu.
        CallNextHookEx(0, code, wparam, lparam)
    }

    pub fn start(on_press: Box<dyn Fn(u32) + Send>) {
        let (tx, rx) = channel::<u32>();
        if SENDER.set(Mutex::new(tx)).is_err() {
            return;
        }
        // Le son se lance hors du crochet : Windows retire un crochet trop lent.
        std::thread::spawn(move || {
            for id in rx {
                on_press(id);
            }
        });
        std::thread::spawn(|| {
            // SAFETY: crochet installé et servi par la boucle de messages de ce fil.
            unsafe {
                let hook =
                    SetWindowsHookExW(WH_KEYBOARD_LL, hook, GetModuleHandleW(std::ptr::null()), 0);
                if hook == 0 {
                    crate::journal::write("Raccourcis en jeu : crochet clavier refusé par Windows");
                    return;
                }
                let mut msg = std::mem::zeroed::<Msg>();
                while GetMessageW(&mut msg, 0, 0, 0) > 0 {}
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn maps_common_hotkeys() {
        let b = binding(&Shortcut::from_str("Ctrl+Shift+KeyK").unwrap()).unwrap();
        assert_eq!((b.mods, b.vk), (MOD_CTRL | MOD_SHIFT, 0x4B));
        let b = binding(&Shortcut::from_str("Numpad3").unwrap()).unwrap();
        assert_eq!((b.mods, b.vk), (0, 0x63));
        let b = binding(&Shortcut::from_str("Alt+F7").unwrap()).unwrap();
        assert_eq!((b.mods, b.vk), (MOD_ALT, 0x76));
        let b = binding(&Shortcut::from_str("Ctrl+Digit0").unwrap()).unwrap();
        assert_eq!((b.mods, b.vk), (MOD_CTRL, 0x30));
    }

    #[test]
    fn same_press_from_both_paths_counts_once() {
        assert!(!seen_by_hook(987_654));
        note_hook_press(987_654);
        // Le raccourci Windows de la même pression est ignoré, même en retard.
        assert!(seen_by_hook(987_654));
        assert!(!seen_by_hook(987_655));
    }

    #[test]
    fn matches_exact_modifiers_only() {
        set_bindings(vec![Binding {
            mods: MOD_CTRL,
            vk: 0x31,
            id: 7,
        }]);
        assert_eq!(matching(0x31, MOD_CTRL), Some(7));
        assert_eq!(matching(0x31, MOD_CTRL | MOD_SHIFT), None);
        assert_eq!(matching(0x31, 0), None);
    }
}
