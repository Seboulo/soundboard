# Soundboard

Une soundboard pour Windows qui envoie tes sons dans Discord (et dans n'importe quelle app vocale) avec des raccourcis clavier.

L'app joue chaque son à deux endroits en même temps : dans ton casque, pour que tu l'entendes, et dans un micro virtuel gratuit ([VB-Audio Virtual Cable](https://vb-audio.com/Cable/)) que Discord utilise comme micro. Ta vraie voix est mélangée avec les sons, donc tes amis entendent les deux.

## Installation

1. Télécharge `Soundboard_x.y.z_x64-setup.exe` depuis la [page Releases](https://github.com/Seboulo/soundboard/releases/latest) et lance-le.
   Windows peut afficher « Windows a protégé votre ordinateur » (l'app n'est pas signée) : clique sur **Informations complémentaires** puis **Exécuter quand même**.
2. Au premier lancement, l'assistant te guide :
   - installation de VB-Cable (clic droit sur `VBCABLE_Setup_x64.exe` → *Exécuter en tant qu'administrateur*, puis redémarrage si demandé) ;
   - choix de ton casque et de ton micro ;
   - réglage de Discord.

### Réglages Discord

Dans **Paramètres utilisateur → Voix et vidéo** :

- **Périphérique d'entrée** : `CABLE Output (VB-Audio Virtual Cable)`
- **Suppression du bruit (Krisp)** : désactivée, sinon une partie des sons est filtrée
- En **Détection de la voix**, baisse un peu la sensibilité pour que les sons passent même si tu ne parles pas

## Utilisation

- **Ajouter des sons** : glisse des fichiers MP3, WAV, OGG, FLAC ou M4A dans la fenêtre, ou clique sur « Ajouter des sons ». Les fichiers sont copiés dans le dossier de l'app.
- **Chercher en ligne** : bouton « 🔎 Chercher en ligne » pour trouver des sons sur myinstants.com, les écouter et les ajouter en un clic.
- **Jouer / arrêter** : clic sur un pad ou son raccourci global (marche même quand Discord ou un jeu est au premier plan). Re-cliquer (ou re-appuyer) arrête le son. Une barre montre la progression.
- **Modifier** : clic droit sur un pad (ou ✎) pour le nom, le raccourci, le volume, la couleur et la **découpe** (glisse les poignées sur la forme d'onde pour couper le début et la fin, puis « Écouter »).
- **Réorganiser** : glisse un pad sur un autre.
- **Tout arrêter** : bouton « Stop » ou touche **Pause** par défaut.

Dans les réglages :

- **Égaliser le volume** (activé par défaut) : les sons trop forts ou trop faibles sont ramenés à un niveau commun.
- **Lancer au démarrage de Windows** : l'app démarre discrètement près de l'horloge.
- **Fermer la fenêtre garde l'app près de l'horloge** : les raccourcis restent actifs. Clic sur l'icône pour rouvrir, clic droit → « Quitter » pour fermer vraiment.

Pour mettre à jour : télécharge la dernière version sur la page Releases et installe-la par-dessus (sons et réglages conservés).

Astuce raccourcis : le pavé numérique ou F13–F24 évitent les conflits avec les jeux.

Config et sons sont stockés dans `%APPDATA%\com.sebastien.soundboard\`.

## Développement

Prérequis : [Rust](https://rustup.rs), Node.js 22, et sous Windows les outils de build C++ de Visual Studio.

```sh
npm install
npm run dev     # lance l'app en mode développement
npm run build   # crée l'installeur dans src-tauri/target/release/bundle/nsis/
```

Structure :

- `src-tauri/src/audio.rs` : moteur audio (casque + câble virtuel + mixage du micro), dans un thread dédié
- `src-tauri/src/lib.rs` : commandes appelées par l'interface, raccourcis globaux, assistant
- `src-tauri/src/config.rs` : sauvegarde de la config (JSON)
- `src-tauri/src/analysis.rs` : durée, niveau et forme d'onde des sons
- `src-tauri/src/online.rs` : recherche et téléchargement sur myinstants.com
- `ui/` : interface (HTML/CSS/JS sans framework)

### Publier une version

Le workflow `.github/workflows/release.yml` compile l'installeur Windows à chaque push. Pour publier une release téléchargeable, monte la version dans `tauri.conf.json`, `Cargo.toml` et `package.json`, puis :

```sh
git tag v0.2.1
git push origin v0.2.1
```
