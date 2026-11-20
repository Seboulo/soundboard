//! Sauvegarde complète de la soundboard dans un seul fichier .zip :
//! réglages, sons et images. Sert à changer de PC ou à se protéger.

use crate::config::Config;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub const CONFIG_ENTRY: &str = "soundboard-sauvegarde.json";
/// Limites à la lecture, contre un .zip piégé qui remplirait le disque.
const MAX_CONFIG: u64 = 16 * 1024 * 1024;
const MAX_FILE: u64 = 200 * 1024 * 1024;

/// Écrit la sauvegarde. Les chemins y sont rangés en relatif
/// (`sounds/…`, `images/…`) pour fonctionner sur un autre PC.
pub fn write(dest: &Path, config: &Config) -> Result<usize, String> {
    let file = fs::File::create(dest).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipWriter::new(file);
    // Les sons sont déjà compressés (mp3, ogg…) : on les range tels quels.
    let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    let mut saved = config.clone();
    let mut count = 0;
    // Un même fichier (image animée = image fixe) n'est rangé qu'une fois,
    // et deux fichiers de même nom reçoivent des noms distincts : le .zip
    // refuse les doublons et toute la sauvegarde échouait.
    let mut stored: HashMap<PathBuf, PathBuf> = HashMap::new();
    let mut names: HashSet<String> = HashSet::new();
    let mut add = |path: &mut PathBuf, folder: &str| -> Result<bool, String> {
        if let Some(entry) = stored.get(&*path) {
            *path = entry.clone();
            return Ok(true);
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()).map(String::from) else { return Ok(false) };
        let Ok(data) = fs::read(&*path) else { return Ok(false) };
        let mut entry = format!("{folder}/{name}");
        let mut n = 2;
        while names.contains(&entry) {
            entry = format!("{folder}/{n}-{name}");
            n += 1;
        }
        zip.start_file(entry.as_str(), options).map_err(|e| e.to_string())?;
        zip.write_all(&data).map_err(|e| e.to_string())?;
        names.insert(entry.clone());
        stored.insert(path.clone(), PathBuf::from(&entry));
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
        entry.by_ref().take(MAX_CONFIG).read_to_string(&mut text).map_err(|e| e.to_string())?;
        serde_json::from_str(&text).map_err(|e| format!("sauvegarde illisible : {e}"))?
    };
    fs::create_dir_all(sounds_dir).map_err(|e| e.to_string())?;
    fs::create_dir_all(images_dir).map_err(|e| e.to_string())?;
    // Seul le nom du fichier est gardé : rien ne peut sortir des dossiers de l'app.
    // Un fichier déjà là et identique est repris tel quel (sous Windows, un son
    // en cours de lecture est verrouillé) ; s'il diffère, on en crée un autre
    // à côté au lieu d'écraser un son de la soundboard actuelle.
    let mut extract = |path: &Path, dir: &Path| -> Option<PathBuf> {
        let name = Path::new(path.file_name()?);
        let entry_name = path.to_str()?;
        if zip.by_name(entry_name).ok()?.size() > MAX_FILE {
            return None;
        }
        let mut dest = dir.join(name);
        let mut n = 2;
        while dest.exists() {
            if same_content(zip.by_name(entry_name).ok()?, &dest) {
                return Some(dest);
            }
            let stem = name.file_stem()?.to_string_lossy();
            dest = match name.extension() {
                Some(ext) => dir.join(format!("{stem}-{n}.{}", ext.to_string_lossy())),
                None => dir.join(format!("{stem}-{n}")),
            };
            n += 1;
        }
        let mut entry = zip.by_name(entry_name).ok()?;
        let mut out = fs::File::create(&dest).ok()?;
        // La taille annoncée peut mentir : on coupe quand même à la limite.
        let copied = std::io::copy(&mut entry.by_ref().take(MAX_FILE + 1), &mut out).ok()?;
        if copied > MAX_FILE {
            drop(out);
            let _ = fs::remove_file(&dest);
            return None;
        }
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

/// Même contenu qu'un fichier déjà sur le disque ?
fn same_content(mut entry: impl Read, path: &Path) -> bool {
    let Ok(mut file) = fs::File::open(path) else { return false };
    let (mut a, mut b) = (vec![0u8; 64 * 1024], vec![0u8; 64 * 1024]);
    loop {
        let Ok(n) = read_full(&mut entry, &mut a) else { return false };
        let Ok(m) = read_full(&mut file, &mut b) else { return false };
        if n != m || a[..n] != b[..m] {
            return false;
        }
        if n == 0 {
            return true;
        }
    }
}

fn read_full(r: &mut impl Read, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match r.read(&mut buf[filled..])? {
            0 => break,
            k => filled += k,
        }
    }
    Ok(filled)
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

    #[test]
    fn same_image_twice_and_same_names() {
        // Une image choisie sur le PC sert d'image animée ET fixe (même
        // chemin), et deux sons peuvent avoir le même nom de fichier.
        let root = std::env::temp_dir().join(format!("sb-backup2-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("a/x")).unwrap();
        fs::create_dir_all(root.join("a/y")).unwrap();
        fs::write(root.join("a/x/son.mp3"), b"un").unwrap();
        fs::write(root.join("a/y/son.mp3"), b"deux").unwrap();
        fs::write(root.join("a/x/img.png"), b"png").unwrap();
        let mut config: Config = serde_json::from_str(
            r##"{"sounds":[{"id":"x","name":"A","path":"","volume":1.0,"color":"#fff"},{"id":"y","name":"B","path":"","volume":1.0,"color":"#fff"}]}"##,
        )
        .unwrap();
        config.sounds[0].path = root.join("a/x/son.mp3");
        config.sounds[0].image = Some(root.join("a/x/img.png"));
        config.sounds[0].image_still = Some(root.join("a/x/img.png"));
        config.sounds[1].path = root.join("a/y/son.mp3");
        let zip = root.join("s.zip");
        assert_eq!(write(&zip, &config).unwrap(), 2);
        let restored = read(&zip, &root.join("b/sounds"), &root.join("b/images")).unwrap();
        assert_eq!(restored.sounds.len(), 2);
        assert_eq!(fs::read(&restored.sounds[0].path).unwrap(), b"un");
        assert_eq!(fs::read(&restored.sounds[1].path).unwrap(), b"deux");
        assert_eq!(restored.sounds[0].image, restored.sounds[0].image_still);
        assert!(restored.sounds[0].image.as_ref().unwrap().exists());

        // Restaurer une 2e fois au même endroit : fichiers identiques repris,
        // et un fichier différent du même nom n'est pas écrasé.
        let again = read(&zip, &root.join("b/sounds"), &root.join("b/images")).unwrap();
        assert_eq!(again.sounds[0].path, restored.sounds[0].path);
        fs::write(&restored.sounds[1].path, b"autre").unwrap();
        let third = read(&zip, &root.join("b/sounds"), &root.join("b/images")).unwrap();
        assert_ne!(third.sounds[1].path, restored.sounds[1].path);
        assert_eq!(fs::read(&third.sounds[1].path).unwrap(), b"deux");
        assert_eq!(fs::read(&restored.sounds[1].path).unwrap(), b"autre");
    }
}
