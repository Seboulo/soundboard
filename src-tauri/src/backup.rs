//! Sauvegarde complète de la soundboard dans un seul fichier .zip :
//! réglages, sons et images. Sert à changer de PC ou à se protéger.

use crate::config::Config;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

const CONFIG_ENTRY: &str = "soundboard-sauvegarde.json";

/// Écrit la sauvegarde. Les chemins y sont rangés en relatif
/// (`sounds/…`, `images/…`) pour fonctionner sur un autre PC.
pub fn write(dest: &Path, config: &Config) -> Result<usize, String> {
    let file = fs::File::create(dest).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipWriter::new(file);
    // Les sons sont déjà compressés (mp3, ogg…) : on les range tels quels.
    let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    let mut saved = config.clone();
    let mut count = 0;
    let mut add = |path: &mut PathBuf, folder: &str| -> Result<bool, String> {
        let Some(name) = path.file_name().and_then(|n| n.to_str()).map(String::from) else { return Ok(false) };
        let Ok(data) = fs::read(&*path) else { return Ok(false) };
        let entry = format!("{folder}/{name}");
        zip.start_file(entry.as_str(), options).map_err(|e| e.to_string())?;
        zip.write_all(&data).map_err(|e| e.to_string())?;
        *path = PathBuf::from(entry);
        Ok(true)
    };
    for sound in &mut saved.sounds {
        if add(&mut sound.path, "sounds")? {
            count += 1;
        }
        for img in [&mut sound.image, &mut sound.image_still] {
            if let Some(p) = img {
                if !add(p, "images")? {
                    *img = None;
                }
            }
        }
    }
    zip.start_file(CONFIG_ENTRY, options).map_err(|e| e.to_string())?;
    let json = serde_json::to_vec_pretty(&saved).map_err(|e| e.to_string())?;
    zip.write_all(&json).map_err(|e| e.to_string())?;
    zip.finish().map_err(|e| e.to_string())?;
    Ok(count)
}

/// Lit une sauvegarde : range ses fichiers dans les dossiers de l'app et
/// renvoie la config avec des chemins absolus.
pub fn read(src: &Path, sounds_dir: &Path, images_dir: &Path) -> Result<Config, String> {
    let file = fs::File::open(src).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipArchive::new(file).map_err(|_| "ce fichier n'est pas une sauvegarde de la soundboard".to_string())?;
    let mut config: Config = {
        let mut entry = zip
            .by_name(CONFIG_ENTRY)
            .map_err(|_| "ce fichier n'est pas une sauvegarde de la soundboard (c'est peut-être un pack de sons : utilise « Importer un pack »)".to_string())?;
        let mut text = String::new();
        entry.read_to_string(&mut text).map_err(|e| e.to_string())?;
        serde_json::from_str(&text).map_err(|e| format!("sauvegarde illisible : {e}"))?
    };
    fs::create_dir_all(sounds_dir).map_err(|e| e.to_string())?;
    fs::create_dir_all(images_dir).map_err(|e| e.to_string())?;
    // Seul le nom du fichier est gardé : rien ne peut sortir des dossiers de l'app.
    let mut extract = |path: &Path, dir: &Path| -> Option<PathBuf> {
        let name = path.file_name()?;
        let mut entry = zip.by_name(path.to_str()?).ok()?;
        let dest = dir.join(name);
        let mut out = fs::File::create(&dest).ok()?;
        std::io::copy(&mut entry, &mut out).ok()?;
        Some(dest)
    };
    let mut kept = Vec::new();
    for mut sound in std::mem::take(&mut config.sounds) {
        let Some(path) = extract(&sound.path, sounds_dir) else { continue };
        sound.path = path;
        for img in [&mut sound.image, &mut sound.image_still] {
            *img = img.as_deref().and_then(|p| extract(p, images_dir));
        }
        kept.push(sound);
    }
    config.sounds = kept;
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_keeps_sounds_and_settings() {
        let root = std::env::temp_dir().join(format!("sb-backup-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let (a_sounds, a_images) = (root.join("a/sounds"), root.join("a/images"));
        fs::create_dir_all(&a_sounds).unwrap();
        fs::create_dir_all(&a_images).unwrap();
        fs::write(a_sounds.join("x.mp3"), b"son").unwrap();
        fs::write(a_images.join("x.gif"), b"gif").unwrap();
        let mut config: Config = serde_json::from_str(
            r##"{"sounds":[{"id":"x","name":"Pouet","path":"","hotkey":"Numpad1","volume":1.0,"color":"#fff"}]}"##,
        )
        .unwrap();
        config.sounds[0].path = a_sounds.join("x.mp3");
        config.sounds[0].image = Some(a_images.join("x.gif"));
        config.settings.master_volume = 0.42;
        let zip = root.join("sauvegarde.zip");
        assert_eq!(write(&zip, &config).unwrap(), 1);

        let (b_sounds, b_images) = (root.join("b/sounds"), root.join("b/images"));
        let restored = read(&zip, &b_sounds, &b_images).unwrap();
        assert_eq!(restored.sounds.len(), 1);
        let s = &restored.sounds[0];
        assert_eq!(s.path, b_sounds.join("x.mp3"));
        assert_eq!(fs::read(&s.path).unwrap(), b"son");
        assert_eq!(s.image.as_deref(), Some(b_images.join("x.gif").as_path()));
        assert_eq!(s.hotkey.as_deref(), Some("Numpad1"));
        assert_eq!(restored.settings.master_volume, 0.42);
    }
}
