Télécharge `Soundboard_..._x64-setup.exe` ci-dessous et lance-le (déjà installée : bouton « Mettre à jour » dans l'app).
Au premier lancement, l'assistant t'aide à installer VB-Cable et à régler Discord.

Nouveautés de la 3.1.0 — extraits vidéo :
- Un extrait qui arrive pendant un autre attend son tour (3 au plus) au lieu de le couper. Si tu lances ton propre « Aperçu » pendant l'extrait d'un pote, celui-ci repasse juste après.
- L'extrait s'affiche sur l'écran où se trouve ton jeu.
- Bouton 🔊/🔇 sur la carte : il coupe le son dans ton casque seulement, tes potes continuent de l'entendre dans Discord.
- La barre de temps suit la vraie lecture de la vidéo, la carte s'efface en douceur et le son finit en fondu.
- Corrigé : un extrait remplacé par un autre gardait son son ; un son en retard restait décalé et sa fin était coupée.
- « Ne pas déranger » (🌙) : plus aucun son ni extrait de tes potes chez toi, les extraits t'attendent dans Envoyer → Historique.
- « Mes extraits » : une étoile sur un extrait de l'historique le garde pour le renvoyer en un clic (30 au plus).
- Éditeur d'extrait : ▶ joue l'image et le son ensemble, barre de temps avec la forme du son et un zoom, « Début ici », « Fin ici », durées rapides 5, 10, 15, 30 s.
- Texte en gros : en haut ou en bas, grand ou petit.

Nouveautés de la 3.1.0 — son du micro :
- La télé ou quelqu'un d'autre dans la pièce n'ouvre plus ton micro : la porte de voix apprend le niveau de ta voix et ignore les voix nettement plus lointaines (25 dB sous la tienne). Mesuré : micro ouvert 56 % du temps de silence avant, 11 % maintenant (la demi-seconde de fin de phrase).
- Micro très faible : après le test de voix, la porte s'ouvre dès le premier mot (avant, une voix sous -55 dBFS était coupée). Sans test de voix, elle s'ouvre sur les syllabes fortes.
- Réduction de bruit forte sans test de voix : la première phrase après le lancement n'est plus étouffée sur un micro faible ou une pièce bruyante.
- Volume de voix automatique : il peut monter jusqu'à +24 dB (avant +18), un pote au micro faible n'est plus plus bas que les autres. Il ne se règle plus sur une voix lointaine.
- Nouveau réglage « Voix lointaines » (Réglages avancés du micro) : Ignorées, Ignorées (strict) pour une télé proche (15 dB sous ta voix : micro ouvert 56 % de tes silences en normal, 10 % en strict), ou Acceptées comme avant.
- L'appli retient le niveau de ta voix d'un lancement à l'autre : même sans test de voix, la réduction de bruit, le volume automatique et la porte sont bien réglés dès ton premier mot.
- Si ta voix arrive bien plus faible que la dernière fois (volume du micro baissé dans Windows, micro éloigné), l'appli s'en rend compte en une fraction de seconde au lieu de garder ton micro fermé.
- Le niveau retenu vaut pour un micro précis : si tu changes de micro par défaut dans Windows, l'appli le réapprend.
- Micro branché sur une seule entrée d'une carte son à deux entrées : la voix n'est plus divisée par deux. Micro dont les deux canaux sont inversés : la voix n'est plus annulée.
- Micros qui envoient un format rare (entiers 32 bits, 8 bits) : ils marchent au lieu d'afficher « format non géré ».

Nouveautés de la 3.0.2 :
- Une vidéo envoyée par un pote ne te fait plus sortir de ton jeu : si Windows donne quand même la main à la fenêtre de l'extrait, l'appli la rend aussitôt au jeu.
- Plus de boîte d'erreur Windows « ffmpeg.exe - Image incorrecte » : si ffmpeg est abîmé, l'appli le retélécharge une fois ; si Windows le bloque (Contrôle intelligent des applications), la vidéo part telle quelle, sans message.
- Vidéos reçues : fini les saccades en boucle quand la vidéo est lente à se caler sur le son (elle rattrape en douceur au lieu de sauter sans arrêt).
- Vidéos reçues : un extrait qui arrive pile quand le précédent se ferme n'est plus perdu ; la demande « Afficher / Refuser » ne reste plus bloquée derrière un jeu en plein écran ; une vidéo longue à charger n'est plus fermée en pleine préparation.
- Une vidéo illisible sur ton PC est notée dans le journal (Réglages → « Copier le rapport ») au lieu de disparaître sans trace.
- Téléphone : si le port est déjà pris par un autre programme, l'appli en choisit un libre toute seule (rescanne le QR code).

Nouveautés de la 3.0.1 :
- « Favoris » dans la barre de gauche : tous tes sons favoris au même endroit, quel que soit leur onglet.
- Bouton « ⋯ » au survol d'une tuile : Modifier, Jouer, Favori, Choisir un raccourci, Ranger dans un onglet, Supprimer, sans ouvrir la fiche du son.
- Sur les tuiles, le nom est tout en bas. « + Raccourci » passe dans le menu « ⋯ ».
- Dans « Modifier le son », Échap demande confirmation avant d'abandonner des modifications.
- Textes qui ne sont plus coupés quand la fenêtre est étroite (état du micro, recherche, tri).
- Petites tuiles : elles ne se chevauchent plus quand la taille est réglée au minimum.
- Les questions (abandonner des modifications, restaurer une sauvegarde, remettre les stats à zéro, ne plus suivre un pack) s'affichent dans le style de l'appli, avec des boutons qui disent ce qu'ils font : « Garder les sons » ou « Supprimer les sons » au lieu de OK/Annuler.
- Fenêtre étroite : chaque onglet montre ses deux premières lettres dans la barre de gauche (avant, ils avaient tous le même dossier). « Stop » garde son nom plus longtemps.
- Mode Surprise : quand il est activé, le bouton devient bleu plein avec « Surprise activée », impossible de l'oublier. Son icône est dessinée comme les autres.
- Le curseur de taille des pads a une petite et une grande grille de chaque côté.
- « Modifier le son » : la couleur se choisit en un clic parmi les teintes de l'appli (la pastille arc-en-ciel ouvre toutes les couleurs). « Sans effet » est le premier choix des effets et s'allume quand aucun effet n'est mis.
- Dans « Tous », un favori reste dans son onglet (marqué d'une étoile, en tête avec le tri « Favoris d'abord ») : les nombres de sons sont les mêmes à gauche et sur la page. Tous les favoris restent réunis dans « Favoris », à gauche.
- Menu « ⋯ » : des icônes dessinées partout, plus de mélange avec des emojis.
- Vue liste : l'étoile et les boutons de chaque son sont alignés avec les colonnes.
- Une page vide n'affiche plus « 0 son » à côté du titre.
- Tuiles : le raccourci d'un son s'affiche en petit en haut à droite, donc tous les noms restent alignés en bas.
- Des icônes dessinées remplacent les derniers emojis de l'interface (rubrique « Potes », titres des fenêtres). Ceux de tes sons et des effets ne changent pas.
- Réglages : l'interrupteur « Réglages avancés » est juste sous les rubriques.
- Salon entre potes : « Copier » est grisé tant qu'il n'y a pas de code.
- Micro : le nettoyage de la voix passe en priorité haute, pour éviter les micro-coupures quand un jeu occupe tout le processeur.
- Micro : le volume automatique réagit plus calmement (en 3 s environ). Après un rire ou un cri, la suite de la phrase ne baisse presque plus.
- Micro faible : la réduction de bruit forte ne mange plus le premier mot après un silence (mesuré sur un vrai micro : le premier mot sortait 23 dB trop bas). Le test de voix conseille plus tôt de monter le volume du micro dans Windows.
- Volume automatique : sans test de voix, il se règle dès la première seconde de parole.
- Test de voix : l'égaliseur sur mesure corrige au plus 4 dB (8 avant), pour garder la couleur de ta voix.

Nouveautés de la 3.0 :
- Nouvelle interface, plus claire et plus calme. Les onglets passent dans une barre à gauche (Tous, Récents, Les plus joués, Envoyer, puis tes onglets avec leur nombre de sons). « + Nouvel onglet » est toujours visible.
- Une seule barre d'outils en haut, tous les boutons restent sous la main. Stop ne devient rouge que pendant qu'un son joue.
- Un grand titre indique l'onglet ouvert et son nombre de sons. Le tri, la taille des pads et le menu ⋯ sont juste à côté.
- Nouvelles tuiles : l'emoji en haut à gauche, le nom en bas, plus de sons à l'écran. Réglages → Général → « Style des tuiles » : Douces (pastel), Vives (couleurs pleines) ou Classiques (l'ancien rendu).
- La fenêtre « Modifier le son » tient sur deux colonnes (Apparence, Lecture, Découpe, Effets) : presque plus besoin de défiler. Annuler et Enregistrer sont en haut, et rien ne change tant que tu n'as pas enregistré. Chaque curseur affiche sa valeur.
- Onglet Envoyer : « Étape 1 sur 3 » et un sélecteur d'étapes plus compact.
- Nouveau logo, plus sobre.
- L'appli consomme moins quand elle tourne en fond : l'anti-bruit se met au repos dans les silences, les images des pads ne tournent plus en boucle, et la fenêtre cachée près de l'horloge se fait légère.

Nouveautés de la 2.4.2 :
- Les vidéos longues (dont seul un passage est pris) partent aussi par leur lien : l'appli du pote récupère le même passage elle-même. Fini l'erreur « le relais refuse ce fichier » pour ces vidéos. Ton pote doit aussi être en 2.4.2 (sinon elles lui partent en fichier, comme avant).
- Affichage des extraits vidéo revu : la vidéo est préparée en coulisses (bon moment, première image, son prêt) puis apparaît d'un coup. Fini l'image qui démarre, se fige et repart, et la fenêtre qui change de taille devant toi.
- La fenêtre prend la forme de la vidéo : une vidéo verticale (Shorts, TikTok) s'affiche en carte verticale, sans grandes bandes noires.
- L'image reste calée sur le son pendant tout l'extrait.
- Une vidéo envoyée en fichier est d'abord convertie au format que tout PC lit (H.264 + AAC, 720p au plus) et coupée au passage choisi : elle démarre tout de suite chez le pote et pèse moins lourd. Il faut ffmpeg (il s'installe tout seul au premier envoi de vidéo s'il n'est pas déjà là) ; sinon la vidéo part telle quelle, comme avant.

Nouveautés de la 2.4.1 :
- Correctif : l'onglet « 🎬 Envoyer » restait bloqué (recherche et envoi sans effet, impossible de revenir aux sons) quand une recherche était gardée de la fois d'avant.

Nouveautés de la 2.4 :
- Nouvel onglet « 🎬 Envoyer » à la place de la fenêtre : choisir, régler, envoyer, en 3 étapes. L'état de l'envoi reste affiché à côté du bouton.
- N'importe quelle vidéo, même longue : seul le passage voulu est récupéré (l'outil vidéo ffmpeg s'installe tout seul la première fois, environ 90 Mo).
- Une vidéo YouTube, TikTok… part par son lien : l'appli du pote la récupère elle-même. Plus d'erreur 413 du relais. Si un pote a encore une ancienne version, la vidéo lui part en fichier, comme avant.
- Un fichier déjà envoyé (son demandé par un pote, extrait renvoyé) n'est plus renvoyé pendant 2 h 30 : il part tout de suite et compte moins dans la limite du relais.
- Récupération plus fiable : Deno vraiment utilisé par yt-dlp, récupération de secours si la rapide échoue (erreur « Errno 22 »), et une raison claire quand une vidéo est refusée (privée, réservée aux adultes, bloquée…).
- Historique : un extrait reçu peut maintenant être renvoyé, écouté ou prévisualisé.
- « Tout arrêter » d'un pote empêche aussi l'extrait qui était encore en train d'arriver de s'afficher.
- Après une petite coupure, le salon ne perd plus les messages envoyés entre-temps.
- Les sons de tes potes démarrent plus vite chez toi, et une vidéo que tu as déjà n'est pas retéléchargée.
- Dans l'extrait reçu, l'image attend le son : plus de décalage. Entrée dans le texte de l'extrait l'envoie.

Nouveautés de la 2.3 :
- Extraits jusqu'à 30 secondes (au lieu de 15). Tes potes doivent aussi être en 2.3 pour les voir en entier.
- Nouvel onglet « 🕘 Historique » : tes 20 derniers extraits envoyés et reçus, à revoir ou renvoyer en un clic.
- Envoyé à un seul pote, le son de l'extrait passe aussi dans Discord par son micro (envoyé à tout le salon, chacun l'entend chez soi).
- « Tout le salon et moi » dans « Envoyer à » : l'extrait te revient aussi, pour vérifier que l'envoi marche (même seul dans le salon).
- Récupération de vidéo plus rapide : pourcentage affiché, 480p au plus, téléchargement en plusieurs morceaux, et Deno installé tout seul pour que YouTube marche bien avec yt-dlp.
- Moins de lag à l'aperçu et à la réception : seul le passage choisi est traité, pas toute la vidéo.
- Fenêtre « Envoyer un extrait » plus claire : plus courte tant que rien n'est choisi, dernier onglet retenu.
- Envoi plus patient avec les petites connexions (4 min), et chaque envoi/réception noté dans « Copier le rapport ».

Nouveautés de la 2.2 :
- Envoyer un extrait : l'onglet « 🔗 Vidéo » cherche directement sur YouTube avec des mots-clés (plus besoin de lien). Miniatures, titres et durées ; un clic récupère la vidéo.
- Les vidéos de plus de 10 min sont marquées « longue », « Plus de résultats » en affiche d'autres, et ta dernière recherche reste là.
- La même recherche marche depuis le téléphone.

Nouveautés de la 2.1 :
- Extraits vidéo : choisis le passage avec deux poignées (15 s au plus), règle le volume du son et « ▶ Écouter » avant d'envoyer. « 👁 Aperçu » montre l'extrait dans le coin de ton écran comme ton pote le verra.
- Le son des vidéos passe dans le casque du pote, Shorts YouTube compris, et les extraits peuvent faire jusqu'à 14 Mo.
- Tes 5 derniers extraits envoyés se renvoient en un clic. « 💾 Garder » sur un extrait reçu en fait un pad chez toi.
- Bloquer un pote (Réglages → Potes, ou « Bloquer » sur sa carte) : plus aucun son ni extrait de sa part.
- « Tout arrêter » coupe aussi le son ou l'extrait que tu viens d'envoyer chez un pote.
- Depuis le téléphone : jouer tes sons chez un pote du salon, et envoyer un extrait (lien, GIF ou fichier du téléphone).

Nouveautés de la 2.0.1 :
- Extrait sur l'écran d'un pote : vidéo depuis un lien (YouTube, TikTok, X, Reddit…) avec le moment où elle commence, recherche de GIF (GIPHY), et ton texte affiché en gros façon mème.
- Les extraits s'affichent à chaque fois (avant, parfois seulement le premier).
- Salon : un son renvoyé par un pote se rejoue depuis le début, et le son qu'il n'avait pas encore n'est plus perdu en route. Le rapport (Réglages → « Copier le rapport ») note chaque son envoyé ou reçu.
- Fenêtre réduite : Réglages, thème et Stop restent toujours visibles.

Nouveautés de la 2.0 :
- Salon entre potes (Réglages → Potes, même code pour tout le monde) : avec le bouton « 👥 », joue tes sons chez un pote (ils partent dans Discord par son micro).
- Extrait sur l'écran d'un pote : « ⋯ » → « Envoyer un extrait ». Une image, un GIF ou une vidéo (avec un de tes sons si tu veux) s'affiche dans un coin de son écran, 15 s au plus, sans le sortir de son jeu. Chacun choisit : me demander, afficher direct ou refuser. Ne se voit pas par-dessus un jeu en plein écran exclusif.
- Rejouer les 30 dernières secondes : un raccourci fait un son de ce qui vient de se dire (ta voix, tes sons et tes potes dans Discord).
- Enregistrer un son au micro, directement dans l'app (1 min max, blancs coupés).
- Texte → voix avec les voix de Windows : le dire tout de suite dans Discord ou en faire un pad.
- Packs partagés par lien (OneDrive, Dropbox, Google Drive) : tes potes suivent ton onglet et reçoivent tes nouveaux sons tout seuls.
- Baisse auto des sons quand un pote parle dans Discord (option, désactivée au départ).
- Onglets : clic droit sur le fond pour créer un onglet, clic droit sur un onglet pour le renommer ou le supprimer, glisse les onglets pour les remettre dans l'ordre.

Nouveautés de la 1.7 :
- Nouvel onglet « Top » : tes 20 sons les plus joués, du plus joué au moins joué.
- Affichage plus net : les barres du haut tiennent sur une ligne, et la barre du haut (avec Stop) ne disparaît plus quand tu fais défiler. Les onglets et le nom du groupe restent visibles en défilant.
- Sélection, export et import sont rangés dans le menu « ⋯ » ; le mode Surprise 🎲 est à côté d'« Au hasard ».
- Pads plus colorés, emoji posé directement sur le pad, une note de la couleur du son quand il n'a pas d'emoji, et le son en cours se voit mieux (« ■ Arrêter »).
- Fenêtre « Modifier le son » : boutons Enregistrer / Annuler toujours visibles, ordre plus logique, effets repliés quand il n'y en a pas.
- Curseurs de volume : repère à 100 % et mention « amplifié » au-delà.
- Recherche sans résultat : bouton pour chercher le même mot en ligne. App vide : « En ligne » et « Pack .zip » proposés pour commencer.
- Assistant : réglages Discord plus lisibles, étape VB-Cable raccourcie s'il est déjà installé.

Nouveautés de la 1.6 :
- Mode Surprise (bouton 🎲 en haut) : un son au hasard part tout seul sur Discord, à un moment au hasard (attente entre X et Y minutes, sons au choix). Jamais actif au lancement.
- Sons mieux rangés : « Tous » est groupé par onglet (groupes repliables), favoris épinglés en haut, glisse un son sur un onglet pour l'y ranger, ou sélectionne-en plusieurs (Ctrl + clic ou « Sélectionner ») et range-les d'un coup.
- Recherche au clavier : tape n'importe où pour chercher, Entrée joue le premier résultat, Échap efface.
- Éditeur de son : bouton « ✂ Couper les blancs », zoom sur l'onde (− / + ou Ctrl + molette), poignées réglables aux flèches (0,1 s, Maj : 0,01 s), double-clic sur un curseur pour le remettre à sa valeur normale.
- Plus rapide : les réglages s'ouvrent tout de suite, et le lancement n'est plus ralenti par la mesure du volume des sons.
- Quand une fenêtre est ouverte, la molette ne fait plus défiler la grille derrière.

Nouveautés de la 1.5 :
- Voix plus naturelle : l'antibruit IA s'adapte au bruit de ta pièce (moins fort au calme, plus fort dans le bruit) et nettoie mieux le petit souffle entre les syllabes.
- Moins de mots coupés : l'IA aide la porte à rester ouverte tant que tu parles, même doucement dans le bruit.
- Plus de souffle qui monte dans les pauses : entre deux phrases, le son baisse en douceur.
- Traitement de la voix remis dans le bon ordre, comme en studio : volume auto, égaliseur, compresseur et dé-esseur ne se gênent plus entre eux.
- Réduction de bruit légère plus douce : fini l'effet « voix sous l'eau ».
- Sons : un meme mono et un meme stéréo sortent au même volume chez tes potes (tes sons sont remesurés une fois au lancement). Le curseur de volume d'un son agit vraiment jusqu'à 200 %, et les 3 réglages de volume ressenti sonnent pareil, juste plus ou moins fort.
- Test de voix : repère les micros qui coupent les aigus (casque Bluetooth en « mains libres ») et ne remonte plus du souffle avec l'égaliseur sur mesure.
- Assistant Discord : nouveaux conseils pour que Discord et Windows ne baissent plus les sons de l'app pendant un appel (Atténuation à 0 %, onglet Communications de Windows sur « Ne rien faire »), et pour garder l'annulation d'écho si tu joues sur enceintes.

Nouveautés de la 1.4 :
- Éditeur de son : change la vitesse, rends la voix aiguë ou grave, ajoute de l'écho, un effet radio ou « derrière un mur », joue le son à l'envers, avec des préréglages en un clic. Le fichier d'origine n'est jamais modifié, et tu peux annuler / rétablir (Ctrl+Z / Ctrl+Y).
- Plus fiable : tes réglages sont sauvegardés et réparés tout seuls s'ils s'abîment, « Annuler » après la suppression d'un son, le casque et le micro se reconnectent tout seuls, et un bandeau t'explique les problèmes avec un bouton pour les régler.
- Sauvegarde complète de ta soundboard (sons, images, réglages) dans un seul fichier, pour changer de PC. Glisse un .zip sur la fenêtre pour importer un pack ou restaurer une sauvegarde.
- Un son dont le fichier a disparu est signalé, et tu peux lui redonner un fichier sans perdre ses réglages.
- Plus simple à prendre en main : réglages essentiels et avancés séparés, « premiers pas », bouton « + Raccourci » directement sur les pads.
- Confort : taille de l'interface (Ctrl + / Ctrl −), mode contraste élevé, animations réduites, navigation au clavier (flèches, Entrée, F2, Suppr).
- Son : le volume est égalisé sur le passage vraiment joué, plus de clic quand un son est coupé, les sons stéréo ne s'annulent plus chez tes potes sur Discord.
- Raccourcis : une lettre seule est refusée (elle t'empêcherait d'écrire partout) et un raccourci déjà pris est signalé sous le champ.
- Corrections : la sauvegarde échouait avec une image choisie sur le PC, la fenêtre pouvait geler sur un long son avec effets, très gros fichiers refusés.
- « Copier le rapport » pour envoyer facilement les infos en cas de souci.

Nouveautés de la 1.3 :
- Plus aucune micro-coupure en fin de mots : l'antibruit IA ne coupe plus net les syllabes douces (mesuré sur de vrais enregistrements).
- Son de qualité studio : conversion de fréquence pro (aigus intacts, plus de souffle métallique) et traitement en 32 bits flottants de bout en bout.
- Les sons trop forts ne sont plus écrêtés, et leur limiteur anticipe les crêtes au lieu de les écraser.
- Égaliseur « Sur mesure » calculé par le test de voix, dé-esseur plus doux, volume automatique qui remonte mieux les voix faibles.
- Le test de voix repère un micro déjà filtré par un autre logiciel.

Nouveautés de la 1.2 :
- Voix plus naturelle : plus de R rugueux ni de syllabes coupées par l'antibruit.
- Traitement studio de la voix : compresseur doux, dé-esseur (S et CH moins sifflants) et limiteur qui évite le grésillement sur les éclats de voix.
- Test de voix : 5 s de silence puis 5 s de parole, l'app mesure ta pièce et ton micro, se règle toute seule et garde un enregistrement avant/après.
- Couleur de ta voix (égaliseur) : Naturelle, Chaude ou Claire.
- Assistant : conseil pour couper le contrôle automatique du gain de Discord.

Nouveautés de la 1.1.1 :
- Plus de phrases coupées avec la détection automatique de la parole : une voix douce dans une pièce bruyante passe, le micro reste ouvert entre les mots et la première syllabe n'est plus mangée.

Nouveautés de la 1.1 :
- Micro sans micro-coupures : traitement de la voix dans son propre fil et rattrapage en douceur des horloges micro/câble.
- Détection automatique de la parole, comme sur Discord : le micro apprend le bruit de ta pièce et ignore les clics de clavier.
- Volume de voix automatique : voix douce remontée, cris adoucis.
- Antibruit IA plus naturel (fins de mots préservées).
- Bouton « M'écouter » pour entendre ta voix comme tes potes la reçoivent.
- Plus de saturation quand plusieurs sons et ta voix se cumulent (table de mixage avec limiteur), aigus plus nets.
- GIF qui ne « vibrent » plus pendant la lecture.
- Corrections : réglages qui coupaient le son, import de nombreux fichiers qui figeait l'appli, fichiers de sons supprimés qui restaient sur le disque.
