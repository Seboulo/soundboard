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
- **Suppression du bruit (Krisp)** et **annulation de l'écho** : désactivées, sinon une partie des sons est filtrée
- En **Détection de la voix** : décoche « Déterminer automatiquement la sensibilité » et mets le curseur presque à gauche

Les bruits de clavier, de souris et de fond sont filtrés par l'app elle-même, sur ton micro uniquement (réglages « Réduire les bruits de ton micro » et « Couper ton micro quand tu ne parles pas »).

## Utilisation

- **Ajouter des sons** : glisse des fichiers MP3, WAV, OGG, FLAC ou M4A dans la fenêtre, ou clique sur « Ajouter des sons ». Les fichiers sont copiés dans le dossier de l'app.
- **Chercher en ligne** : bouton « 🔎 En ligne » pour trouver des sons sur **MyInstants** (mèmes, répliques) ou **Freesound** (bruitages, ambiances), les écouter et les ajouter en un clic. Freesound demande une clé gratuite à coller dans les réglages ([demande de clé](https://freesound.org/apiv2/apply)).
- **Jouer / arrêter** : clic sur un pad ou son raccourci global (marche même quand Discord ou un jeu est au premier plan). Re-cliquer (ou re-appuyer) arrête le son. Une barre montre la progression. Le volume des écoutes de la recherche en ligne se règle dans sa fenêtre.
- **Modifier** : clic droit sur un pad (ou ✎) pour le nom, l'emoji, le raccourci, le volume, la couleur et la **découpe** (glisse les poignées sur la forme d'onde pour couper le début et la fin ; « Écouter » fait avancer un curseur qui montre où tu en es).
- **Onglets** : donne un onglet à un son (champ « Onglet » quand tu le modifies) pour ranger tes sons par thème.
- **Boucle** : coche « Jouer en boucle » sur un son (musique d'ambiance…) ; re-clique pour l'arrêter.
- **🎲 Au hasard** : joue un son au hasard de l'onglet affiché (un raccourci peut aussi être réglé).
- **Packs** : « 📦 Exporter » enregistre les sons de l'onglet (ou tous) dans un .zip à envoyer à tes potes ; « 📥 Importer un pack » les ajoute chez eux.
- **Mode nuit** : bouton 🌗 en haut (automatique comme Windows, ☀️ clair ou 🌙 nuit).
- **Favoris** : clique sur ☆ sur un pad pour l'épingler en haut de la grille.
- **🕘 Récents** : onglet avec les 12 derniers sons joués.
- **Taille** : curseur à côté des onglets pour agrandir ou réduire les pads.
- **Ajouter par lien** : copie un lien YouTube ou un lien direct vers un son (.mp3, .wav…) puis Ctrl+V dans la fenêtre, ou colle-le dans « 🔎 En ligne ». Pour YouTube, l'app télécharge [yt-dlp](https://github.com/yt-dlp/yt-dlp) la première fois (empreinte vérifiée) puis ouvre la découpe. Respecte les droits des auteurs des vidéos.
- **Ne jamais être coupé** : option d'un son (musique de fond) pour qu'il continue quand un autre démarre.
- **🖼️ Images de memes** : chaque son reçoit un emoji choisi d'après son nom. Avec une clé GIPHY gratuite (réglages → Général), il reçoit aussi un GIF trouvé avec son nom : fixe sur le pad, animé pendant le son. Clic droit sur un son → « Chercher un GIF » pour en choisir un autre, ou « Image du PC… ».
- **📱 Téléphone** : coche l'option dans les réglages, enregistre, rouvre les réglages et scanne le QR code (téléphone sur le même Wi-Fi). Autorise l'app sur les « réseaux privés » quand Windows le demande.
- **Réorganiser** : glisse un pad sur un autre.
- **Tout arrêter** : bouton « Stop » ou touche **Pause** par défaut.
- **⏪ Rejouer les 30 dernières secondes** : « ⋯ » ou un raccourci (réglages → Raccourcis) crée un son de ce qui vient de se dire : ta voix, tes sons et la voix de tes potes dans Discord (Windows 10 de mai 2020 ou plus récent). Rien n'est enregistré sur le disque tant que tu ne le demandes pas.
- **🎙️ Enregistrer au micro** : « ⋯ » → enregistre ta voix (une minute au plus), puis découpe-la.
- **🗣️ Texte → voix** : « ⋯ » → une voix de Windows lit ta phrase, tout de suite dans Discord ou en pad.
- **🔗 Packs partagés par lien** : ouvre un onglet puis « ⋯ » → « Partager cet onglet par lien » : l'app range le pack dans ton OneDrive, Dropbox ou Google Drive et le tient à jour. Partage ce fichier (« Tout le monde ayant le lien ») ; tes potes font « ⋯ » → « Suivre le pack d'un pote » et reçoivent tes nouveaux sons tout seuls.
- **👥 Salon entre potes** : réglages → Potes, même code pour tout le monde. Le bouton « 👥 » en haut choisit chez qui tes sons sont joués : chez un pote, ils partent dans Discord par son micro.
- **🎬 Extrait sur l'écran d'un pote** : « ⋯ » → « Envoyer un extrait » (ou depuis « 👥 ») : une image, un GIF ou une vidéo (fichier, lien YouTube/TikTok… ou GIF GIPHY ; 14 Mo, 15 s au plus), avec un de tes sons si tu veux, s'affiche dans un coin de l'écran du pote sans le sortir de son jeu. Chacun choisit dans Réglages → Potes : me demander, afficher direct ou refuser. « Tout arrêter » ferme l'extrait. Ne se voit pas par-dessus un jeu en plein écran exclusif (plein écran fenêtré : oui).
- **Baisser les sons quand un pote parle** (réglages → Sons, désactivé par défaut) : tes sons se font discrets le temps qu'il parle.

Dans les réglages :

- **Couper ton micro** : bouton « 🎙 Micro » en haut ou raccourci dédié ; les sons passent toujours.
- **Indicateur du micro** : montre ton niveau et la détection de voix pour régler le seuil.
- **Effet sur ta voix** : robot, aiguë, grave ou écho.
- **Égaliser le volume** (activé par défaut) : les sons trop forts ou trop faibles sont ramenés à un niveau commun.
- **Lancer au démarrage de Windows** : l'app démarre discrètement près de l'horloge.
- **Fermer la fenêtre garde l'app près de l'horloge** : les raccourcis restent actifs. Clic sur l'icône pour rouvrir, clic droit → « Quitter » pour fermer vraiment.

Pour mettre à jour : un bandeau apparaît quand une nouvelle version sort. Télécharge la dernière version sur la page Releases et installe-la par-dessus (sons et réglages conservés).

Astuce raccourcis : le pavé numérique ou F13–F24 évitent les conflits avec les jeux.

Config et sons sont stockés dans `%APPDATA%\com.sebastien.soundboard\`.

## Désinstallation

**Paramètres Windows → Applications → Applications installées → Soundboard → Désinstaller.** Pour effacer aussi tes sons et réglages, supprime ensuite le dossier `%APPDATA%\com.sebastien.soundboard\`.

*Uninstall: Windows Settings → Apps → Installed apps → Soundboard → Uninstall. To also remove sounds and settings, delete `%APPDATA%\com.sebastien.soundboard\`.*

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
git commit -m "Version x.y.z [release]"
git push origin main
```

(Pousser un tag `vX.Y.Z` marche aussi.)

## Code signing policy

Free code signing provided by [SignPath.io](https://about.signpath.io), certificate by [SignPath Foundation](https://signpath.org).

Team roles:

- Committers and reviewers: [Seboulo](https://github.com/Seboulo)
- Approvers: [Seboulo](https://github.com/Seboulo)

Every release is built by GitHub Actions from this repository, and each signing request is approved manually.

## Privacy policy

This program does not collect or send personal data. It only contacts the services listed below.

At startup, the app asks the GitHub API for the latest release number, to show a notice when an update exists. Nothing about the user is sent.

Only when the user acts:

- Searching MyInstants or Freesound: the search words are sent to those sites.
- Adding a sound from a YouTube link: yt-dlp is downloaded from its official GitHub releases on first use, then contacts YouTube.
- Phone control, when enabled: a page served on the local network only, protected by a secret code.
- Friends room, when a room code is set: short messages (a random app id, the chosen nickname, which sound to play) and, when a friend lacks a sound, the sound file itself go through the public relay ntfy.sh, on a channel named after a hash of the code. ntfy.sh keeps attachments for 3 hours at most.
- Following a friend's pack: the app downloads the link the user pasted (every 2 hours and at startup).
- "Replay the last 30 seconds" and auto ducking listen to Discord's audio output in memory only; nothing is written to disk unless the user asks for a replay.

No account, analytics or telemetry. Settings and sounds stay on the user's computer.

## License

[MIT](LICENSE)
