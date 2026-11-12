// Fenêtre des extraits envoyés par les potes : demande (si réglé), affiche,
// puis se ferme toute seule. Elle ne prend jamais la main sur le jeu.
const { invoke, convertFileSrc } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const $ = (s) => document.querySelector(s);

const ASK_MS = 12000; // la carte « Afficher / Refuser » attend 12 s
const MAX_MS = 30000; // un extrait dure 30 s au plus
const IMAGE_MS = 6000; // une image sans son reste 6 s

const img = $(".stage img");
const video = $(".stage video");
const show = $("#show");
let audio = null;
let timer = null;
let current = null;
let syncId = 0;

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
// Deux images à l'écran, ou 120 ms au plus : si Windows juge la fenêtre
// cachée (jeu en plein écran), les images n'arrivent jamais et l'extrait
// resterait bloqué.
const frames = () =>
  Promise.race([new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r))), sleep(120)]);

function bar(el, ms) {
  el.style.transition = "none";
  el.style.transform = "scaleX(1)";
  void el.offsetWidth;
  el.style.transition = `transform ${ms}ms linear`;
  el.style.transform = "scaleX(0)";
}

function stopSync() {
  if (syncId && video.cancelVideoFrameCallback) video.cancelVideoFrameCallback(syncId);
  syncId = 0;
  video.playbackRate = 1;
}

function reset() {
  clearTimeout(timer);
  stopSync();
  video.onended = video.onerror = video.onloadedmetadata = img.onerror = img.onload = null;
  video.pause();
  video.removeAttribute("src");
  video.load();
  img.removeAttribute("src");
  if (audio) {
    audio.pause();
    audio = null;
  }
  $("#ask").hidden = true;
  show.hidden = true;
  show.classList.add("prep");
}

// `why` : raison notée dans le journal quand l'extrait n'a pas pu être lu.
function close(why) {
  const media = current ? current.media : null;
  reset();
  current = null;
  invoke("clip_close", { media, why: typeof why === "string" ? why : null });
}

function fill(root, c) {
  root.querySelector("[data-from]").textContent = c.from;
  const t = root.querySelector("[data-title]");
  if (t) t.textContent = c.title ? `« ${c.title} »` : "";
}

// La fenêtre prend sa taille pendant que rien n'est affiché : le changement
// ne se voit pas. On laisse deux images à l'écran pour que la page suive.
async function fit(args) {
  await invoke("clip_fit", args).catch(() => {});
  await frames();
}

async function ask(c) {
  await fit({ ask: true });
  if (current !== c) return;
  fill($("#ask"), c);
  $("#ask").hidden = false;
  bar($("#ask .bar"), ASK_MS);
  timer = setTimeout(() => close(), ASK_MS);
}

// Le son part dans le casque choisi dans l'appli (moteur audio). Renvoie
// true quand il est parti. S'il échoue, l'élément joue le son lui-même
// (sortie par défaut de Windows) et on renvoie false.
function playSound(c, path, startMs, endMs, el) {
  return invoke("clip_audio", { path, startMs: Math.round(startMs), endMs: endMs ?? null, volume: c.volume ?? 1 }).then(
    () => true,
    () => {
      if (current !== c) return false;
      el.muted = false;
      if (el.paused && el !== video) {
        // Son à part d'une vidéo déjà partie : il reprend où en est l'image.
        if (!video.paused) el.currentTime = video.currentTime;
        el.play().catch(() => {});
      }
      return false;
    },
  );
}

// Le son mène, l'image suit (comme les lecteurs vidéo pros) : à chaque image
// affichée, on compare sa position à celle du son. Petit écart : la vidéo
// accélère ou ralentit à peine (invisible) ; gros écart (PC qui rame) : on
// la recale d'un coup.
function followSound(from, soundAt) {
  if (!video.requestVideoFrameCallback) return;
  // Un recalage d'un coup coûte cher (la vidéo se fige le temps du saut) :
  // pas plus d'un toutes les 2 s et 3 par extrait, sinon une vidéo lente à
  // sauter se recalerait en boucle et saccaderait. Entre deux, la vitesse
  // rattrape l'écart (jusqu'à 8 %, l'image est muette : ça ne s'entend pas).
  let jumps = 0;
  let lastJump = -Infinity;
  const step = (now, meta) => {
    if (video.paused) return;
    if (video.seeking) {
      syncId = video.requestVideoFrameCallback(step);
      return;
    }
    const expected = from + (meta.expectedDisplayTime - soundAt) / 1000;
    const drift = meta.mediaTime - expected;
    if (Math.abs(drift) > 0.4 && jumps < 3 && now - lastJump > 2000) {
      jumps += 1;
      lastJump = now;
      video.currentTime = Math.max(0, expected + 0.08);
      video.playbackRate = 1;
    } else if (Math.abs(drift) > 0.04) {
      video.playbackRate = Math.min(1.08, Math.max(0.92, 1 - drift * 0.5));
    } else {
      video.playbackRate = 1;
    }
    syncId = video.requestVideoFrameCallback(step);
  };
  syncId = video.requestVideoFrameCallback(step);
}

function reveal(ms) {
  show.classList.remove("prep");
  bar($("#show .bar"), ms);
  clearTimeout(timer);
  timer = setTimeout(() => close(), ms + 300);
}

function playVideo(c, vol) {
  img.hidden = true;
  video.hidden = false;
  video.volume = vol;
  // Image muette ici : le son passe par le casque (voir playSound).
  video.muted = true;
  video.preload = "auto";
  video.onerror = () => {
    const e = video.error;
    close(`vidéo illisible sur ce PC${e ? ` : code ${e.code} ${e.message || ""}` : ""}`);
  };
  video.onended = () => close();
  // Au pire, l'extrait se ferme si rien ne se charge.
  timer = setTimeout(() => close("la vidéo ne se charge pas"), 8000);
  video.onloadedmetadata = async () => {
    video.onloadedmetadata = null;
    if (current !== c) return;
    // Chargée : la suite (taille, première image, son) a son propre délai,
    // l'ancien ne doit pas fermer l'extrait en pleine préparation.
    clearTimeout(timer);
    timer = setTimeout(() => close("la vidéo ne démarre pas"), 8000);
    const d = video.duration || 0;
    const askFrom = (c.start_ms || 0) / 1000;
    const from = askFrom && askFrom < d ? askFrom : 0;
    const until = Math.min(d || from + 15, c.end_ms ? c.end_ms / 1000 : Infinity, from + MAX_MS / 1000);
    const showMs = Math.max(2000, Math.min(MAX_MS, (until - from) * 1000));
    // En coulisses : la fenêtre prend la forme de la vidéo et la première
    // image du passage est décodée (saut compris) avant d'afficher quoi que ce soit.
    const ready = new Promise((res) => {
      if (from) {
        video.addEventListener("seeked", res, { once: true });
        video.currentTime = from;
      } else if (video.readyState >= 2) res();
      else video.addEventListener("loadeddata", res, { once: true });
    });
    await Promise.race([Promise.all([ready, fit({ width: video.videoWidth, height: video.videoHeight })]), sleep(3000)]);
    if (current !== c) return;
    let el = video;
    let path = c.media;
    if (c.sound) {
      // Son à part (Shorts) : même départ que l'image.
      const a = new Audio(convertFileSrc(c.sound));
      a.volume = vol;
      a.muted = true;
      a.preload = "auto";
      audio = a;
      a.addEventListener("loadedmetadata", () => {
        if (audio === a) a.currentTime = from;
      }, { once: true });
      el = a;
      path = c.sound;
    }
    // Le son est prêt presque tout de suite (mesuré pendant le chargement) ;
    // au pire, l'image part sans lui après une seconde et demie.
    const sound = playSound(c, path, from * 1000, c.end_ms, el);
    const engine = await Promise.race([sound, sleep(1500).then(() => null)]);
    if (current !== c) return;
    const soundAt = performance.now();
    reveal(showMs);
    video.play().catch(() => {});
    if (engine) followSound(from, soundAt);
    else if (engine === null) {
      // Son en retard : il part du début du passage, l'image se recale sur
      // lui (retour au début) et le compte à rebours repart, sinon la fin
      // du son était coupée.
      sound.then((ok) => {
        if (!ok || current !== c || video.paused) return;
        reveal(showMs);
        followSound(from, performance.now());
      });
    }
  };
  video.src = convertFileSrc(c.media);
}

function playImage(c, vol) {
  video.hidden = true;
  img.hidden = false;
  img.onerror = () => close("image illisible");
  timer = setTimeout(() => close("l'image ne se charge pas"), 8000);
  img.onload = async () => {
    img.onload = null;
    await Promise.race([fit({ width: img.naturalWidth, height: img.naturalHeight }), sleep(1500)]);
    if (current !== c) return;
    if (!c.sound) {
      reveal(IMAGE_MS);
      return;
    }
    const a = new Audio(convertFileSrc(c.sound));
    audio = a;
    a.volume = vol;
    // Sert à connaître la durée ; joue seulement si le casque échoue.
    a.muted = true;
    a.preload = "auto";
    const start = (c.start_ms || 0) / 1000;
    const end = c.end_ms ? c.end_ms / 1000 : Infinity;
    a.ontimeupdate = () => {
      if (a.currentTime >= end) a.pause();
    };
    const known = new Promise((res) => {
      a.onloadedmetadata = () => {
        if (audio === a) a.currentTime = start;
        res(a.duration || 6);
      };
      a.onerror = () => res(6);
    });
    const duration = await Promise.race([known, sleep(1500).then(() => 6)]);
    if (current !== c) return;
    const d = Math.min(MAX_MS, (Math.min(end, duration) - start) * 1000);
    const sound = playSound(c, c.sound, start * 1000, c.end_ms, a);
    const engine = await Promise.race([sound, sleep(1500).then(() => null)]);
    if (current !== c) return;
    reveal(Math.max(IMAGE_MS, d));
    // Son en retard : le compte à rebours repart quand il part, pour ne pas
    // couper sa fin.
    if (engine === null) sound.then((ok) => ok && current === c && reveal(Math.max(IMAGE_MS, d)));
  };
  img.src = convertFileSrc(c.media);
}

function play(c) {
  clearTimeout(timer);
  $("#ask").hidden = true;
  fill(show, c);
  $(".meme").textContent = c.title || "";
  show.classList.add("prep");
  show.hidden = false;
  $("[data-says]").textContent = c.preview ? "· ce que ton pote verra" : "t'envoie ça";
  const keep = $('[data-action="keep"]');
  // Garder : seulement s'il y a un son (une image seule ne fait pas un pad).
  keep.hidden = c.preview || (c.kind === "image" && !c.sound) || (c.kind === "video" && !c.sound && !/\.mp4$/i.test(c.media));
  keep.disabled = false;
  keep.textContent = "💾 Garder";
  const vol = Math.min(1, Math.max(0, c.volume ?? 1));
  if (c.kind === "video") playVideo(c, vol);
  else playImage(c, vol);
}

function start(c) {
  if (!c) return;
  reset();
  current = c;
  if (c.ask) ask(c);
  else play(c);
}

$('[data-action="accept"]').addEventListener("click", () => {
  if (current) play(current);
});
$('[data-action="refuse"]').addEventListener("click", () => close());
$('[data-action="block"]').addEventListener("click", async () => {
  if (current) await invoke("set_friend_blocked", { id: current.from_id, name: current.from, blocked: true }).catch(() => {});
  close();
});
$('[data-action="close"]').addEventListener("click", () => close());
$('[data-action="keep"]').addEventListener("click", async (e) => {
  const btn = e.currentTarget;
  btn.disabled = true;
  btn.textContent = "…";
  try {
    await invoke("clip_keep");
    btn.textContent = "✓ Gardé";
  } catch (err) {
    btn.textContent = "Impossible";
    btn.title = String(err);
  }
});

listen("clip-show", ({ payload }) => start(payload));
listen("clip-stop", () => {
  reset();
  current = null;
});
invoke("clip_pending").then(start).catch(() => {});
