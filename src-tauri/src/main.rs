// Pas de console en plus de la fenêtre sous Windows (en release).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    soundboard_lib::run()
}
