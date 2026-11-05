// Fenêtre des extraits envoyés par les potes : demande (si réglé), affiche,
// puis se ferme toute seule. Elle ne prend jamais la main sur le jeu.
const { invoke, convertFileSrc } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const $ = (s) => document.querySelector(s);

const ASK_MS = 12000; // la carte « Afficher / Refuser » attend 12 s
const MAX_MS = 15000; // un extrait dure 15 s au plus
const IMAGE_MS = 6000; // une image sans son reste 6 s

const img = $(".stage img");
const video = $(".stage video");
let audio = null;
let timer = null;
let current = null;

function bar(el, ms) {
  el.style.transition = "none";
  el.style.transform = "scaleX(1)";
  void el.offsetWidth;
  el.style.transition = `transform ${ms}ms linear`;
  el.style.transform = "scaleX(0)";
}

function reset() {
  clearTimeout(timer);
  video.onended = video.onerror = video.onloadedmetadata = video.onplaying = img.onerror = null;
  video.pause();
  video.removeAttribute("src");
  img.removeAttribute("src");
  if (audio) {
    audio.pause();
    audio = null;
  }
  $("#ask").hidden = true;
  $("#show").hidden = true;
}

function close() {
  reset();
  current = null;
  invoke("clip_close");
}

function fill(root, c) {
  root.querySelector("[data-from]").textContent = c.from;
  const t = root.querySelector("[data-title]");
  if (t) t.textContent = c.title ? `« ${c.title} »` : "";
}

function ask(c) {
  fill($("#ask"), c);
  $("#ask").hidden = false;
  bar($("#ask .bar"), ASK_MS);
  timer = setTimeout(close, ASK_MS);
}

// Le son part dans le casque choisi dans l'appli (moteur audio) ; si ça
// échoue, l'élément joue le son lui-même (sortie par défaut de Windows).
function playSound(c, path, startMs, endMs, el) {
  invoke("clip_audio", { path, startMs: Math.round(startMs), endMs: endMs ?? null, volume: c.volume ?? 1 }).catch(() => {
    if (current !== c) return;
    el.muted = false;
    if (el.paused && el !== video) el.play().catch(() => {});
  });
}

function play(c) {
  clearTimeout(timer);
  $("#ask").hidden = true;
  fill($("#show"), c);
  $(".meme").textContent = c.title || "";
  $("#show").hidden = false;
  $("[data-says]").textContent = c.preview ? "· ce que ton pote verra" : "t'envoie ça";
  const keep = $('[data-action="keep"]');
  // Garder : seulement s'il y a un son (une image seule ne fait pas un pad).
  keep.hidden = c.preview || (c.kind === "image" && !c.sound) || (c.kind === "video" && !c.sound && !/\.mp4$/i.test(c.media));
  keep.disabled = false;
  keep.textContent = "💾 Garder";
  const vol = Math.min(1, Math.max(0, c.volume ?? 1));
  let ms = IMAGE_MS;
  if (c.kind === "video") {
    img.hidden = true;
    video.hidden = false;
    video.volume = vol;
    // Image muette ici : le son passe par le casque (voir playSound).
    video.muted = true;
    video.src = convertFileSrc(c.media);
    const from = (c.start_ms || 0) / 1000;
    if (from) video.currentTime = from;
    video.onended = close;
    video.onerror = close;
    video.onloadedmetadata = () => {
      if (current !== c) return;
      if (from && from < video.duration) video.currentTime = from;
      const until = Math.min(video.duration || 15, c.end_ms ? c.end_ms / 1000 : Infinity);
      const d = Math.max(2000, Math.min(MAX_MS, (until - (from < video.duration ? from : 0)) * 1000));
      bar($("#show .bar"), d);
      clearTimeout(timer);
      timer = setTimeout(close, d + 300);
    };
    video.onplaying = () => {
      video.onplaying = null;
      if (current !== c) return;
      if (c.sound) {
        // Son à part (Shorts) : même départ que l'image.
        const a = new Audio(convertFileSrc(c.sound));
        a.muted = true;
        audio = a;
        a.addEventListener("loadedmetadata", () => {
          if (audio === a) a.currentTime = video.currentTime;
        }, { once: true });
        playSound(c, c.sound, video.currentTime * 1000, c.end_ms, a);
      } else {
        playSound(c, c.media, video.currentTime * 1000, c.end_ms, video);
      }
    };
    video.play().catch(() => {});
    ms = MAX_MS;
  } else {
    video.hidden = true;
    img.hidden = false;
    img.src = convertFileSrc(c.media);
    img.onerror = close;
    if (c.sound) {
      const a = new Audio(convertFileSrc(c.sound));
      audio = a;
      a.volume = vol;
      const start = (c.start_ms || 0) / 1000;
      const end = c.end_ms ? c.end_ms / 1000 : Infinity;
      a.onloadedmetadata = () => {
        if (audio !== a) return;
        a.currentTime = start;
        const d = Math.min(MAX_MS, (Math.min(end, a.duration || 6) - start) * 1000);
        bar($("#show .bar"), Math.max(IMAGE_MS, d));
        clearTimeout(timer);
        timer = setTimeout(close, Math.max(IMAGE_MS, d + 300));
      };
      a.ontimeupdate = () => {
        if (a.currentTime >= end) a.pause();
      };
      // Sert à connaître la durée ; joue seulement si le casque échoue.
      a.muted = true;
      a.preload = "auto";
      a.addEventListener("loadedmetadata", () => {
        if (audio === a) a.currentTime = start;
      }, { once: true });
      playSound(c, c.sound, start * 1000, c.end_ms, a);
      ms = MAX_MS;
    }
  }
  bar($("#show .bar"), ms);
  timer = setTimeout(close, ms);
}

function start(c) {
  if (!c) return;
  reset();
  current = c;
  if (c.ask) ask(c);
  else play(c);
}

$('[data-action="accept"]').addEventListener("click", async () => {
  await invoke("clip_accept");
  if (current) play(current);
});
$('[data-action="refuse"]').addEventListener("click", close);
$('[data-action="block"]').addEventListener("click", async () => {
  if (current) await invoke("set_friend_blocked", { id: current.from_id, name: current.from, blocked: true }).catch(() => {});
  close();
});
$('[data-action="close"]').addEventListener("click", close);
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
