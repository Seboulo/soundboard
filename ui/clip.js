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
  video.onended = video.onerror = video.onloadedmetadata = img.onerror = null;
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

function play(c) {
  clearTimeout(timer);
  $("#ask").hidden = true;
  fill($("#show"), c);
  $(".meme").textContent = c.title || "";
  $("#show").hidden = false;
  const vol = Math.min(1, Math.max(0, c.volume ?? 1));
  let ms = IMAGE_MS;
  if (c.kind === "video") {
    img.hidden = true;
    video.hidden = false;
    video.volume = vol;
    video.src = convertFileSrc(c.media);
    const from = (c.start_ms || 0) / 1000;
    if (from) video.currentTime = from;
    video.onended = close;
    video.onerror = close;
    video.onloadedmetadata = () => {
      if (current !== c) return;
      if (from && from < video.duration) video.currentTime = from;
      const d = Math.max(2000, Math.min(MAX_MS, ((video.duration || 15) - (from < video.duration ? from : 0)) * 1000));
      bar($("#show .bar"), d);
      clearTimeout(timer);
      timer = setTimeout(close, d + 300);
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
      a.play().catch(() => {});
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
$('[data-action="close"]').addEventListener("click", close);

listen("clip-show", ({ payload }) => start(payload));
listen("clip-stop", () => {
  reset();
  current = null;
});
invoke("clip_pending").then(start).catch(() => {});
