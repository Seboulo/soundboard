"use strict";

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $ = (sel, root = document) => root.querySelector(sel);
const $$ = (sel, root = document) => [...root.querySelectorAll(sel)];

let state = null; // { config, devices, warnings, vb_cable_url }

// ---------- Utilitaires ----------

function toast(message, isError = false) {
  const el = $("#toast");
  el.textContent = message;
  el.classList.toggle("error", isError);
  el.hidden = false;
  clearTimeout(toast.timer);
  toast.timer = setTimeout(() => (el.hidden = true), isError ? 6000 : 2500);
}

async function call(cmd, args) {
  try {
    const res = await invoke(cmd, args);
    if (res && res.config) render(res);
    return res;
  } catch (e) {
    toast(String(e), true);
    // L'état a pu changer partiellement (ex. certains fichiers ajoutés).
    if (cmd !== "get_state") render(await invoke("get_state"));
    return null;
  }
}

const KEY_LABELS = { Control: "Ctrl", Super: "Win" };

/** "Ctrl+Shift+KeyA" → "Ctrl + Shift + A" */
function hotkeyLabel(hotkey) {
  if (!hotkey) return "";
  return hotkey
    .split("+")
    .map((p) =>
      KEY_LABELS[p] ??
      p.replace(/^Key/, "").replace(/^Digit/, "").replace(/^Numpad/, "Num ").replace(/^Arrow/, "")
    )
    .join(" + ");
}

/** Transforme un appui clavier en raccourci compris par le plugin global-shortcut. */
function hotkeyFromEvent(e) {
  if (["Control", "Shift", "Alt", "Meta"].includes(e.key)) return undefined;
  const parts = [];
  if (e.ctrlKey) parts.push("Ctrl");
  if (e.altKey) parts.push("Alt");
  if (e.shiftKey) parts.push("Shift");
  if (e.metaKey) parts.push("Super");
  parts.push(e.code);
  return parts.join("+");
}

function setupHotkeyInput(input) {
  input.addEventListener("focus", () => {
    input.classList.add("listening");
    input.placeholder = "Appuie sur une touche…";
  });
  input.addEventListener("blur", () => {
    input.classList.remove("listening");
    input.placeholder = "Clique puis appuie sur une touche";
  });
  input.addEventListener("keydown", (e) => {
    if (e.key === "Tab") return;
    e.preventDefault();
    e.stopPropagation();
    const noMods = !e.ctrlKey && !e.altKey && !e.shiftKey && !e.metaKey;
    if (noMods && (e.key === "Backspace" || e.key === "Delete")) {
      input.dataset.hotkey = "";
      input.value = "";
      return;
    }
    if (noMods && e.key === "Escape") {
      input.blur();
      return;
    }
    const hk = hotkeyFromEvent(e);
    if (hk) {
      input.dataset.hotkey = hk;
      input.value = hotkeyLabel(hk);
    }
  });
}

function setHotkeyInput(input, hotkey) {
  input.dataset.hotkey = hotkey ?? "";
  input.value = hotkeyLabel(hotkey);
}

function bindRangeLabels(form) {
  form.querySelectorAll("input[type=range]").forEach((r) => {
    const label = form.querySelector(`.value[data-for="${r.name}"]`);
    const update = () => label && (label.textContent = Math.round(r.value * 100) + " %");
    r.addEventListener("input", update);
    r.update = update;
  });
}

function fillSelect(select, names, value, emptyLabel) {
  select.replaceChildren();
  if (emptyLabel) select.add(new Option(emptyLabel, ""));
  for (const n of names) select.add(new Option(n, n));
  // Un périphérique débranché reste visible pour ne pas perdre le choix.
  if (value && !names.includes(value)) select.add(new Option(value + " (absent)", value));
  select.value = value ?? "";
}

function enterSaves(dialog) {
  $("form", dialog).addEventListener("keydown", (e) => {
    if (e.key === "Enter" && e.target.tagName === "INPUT" && !e.target.classList.contains("hotkey")) {
      e.preventDefault();
      dialog.close("save");
    }
  });
}

// ---------- Rendu ----------


function render(snapshot) {
  state = snapshot;
  renderStatus();
  renderMic();
  renderWarnings();
  renderTabs();
  renderGrid();
  $("#version").textContent = `Soundboard v${state.version}`;
  fetchMissingImages();
}

function renderMic() {
  const pill = $("#mic-toggle");
  const muted = state.mic_muted;
  pill.hidden = !state.config.settings.mic_enabled;
  pill.className = "pill " + (muted ? "ko" : "ok");
  pill.textContent = muted ? "🔇 Micro coupé" : "🎙 Micro";
  const hk = hotkeyLabel(state.config.settings.mute_hotkey);
  pill.title = (muted ? "Rallumer ton micro" : "Couper ton micro (les sons passent toujours)") + (hk ? ` — ${hk}` : "");
}

// ---------- Onglets ----------

const TAB_KEY = "soundboard.tab";
let currentTab = null; // null = « Tous »
try {
  currentTab = localStorage.getItem(TAB_KEY) || null;
} catch {}

function categories() {
  const set = new Set(state.config.sounds.map((s) => s.category).filter(Boolean));
  return [...set].sort((a, b) => a.localeCompare(b, "fr"));
}

// Onglet spécial des derniers sons joués (aucun nom d'onglet ne commence par \u0000).
const RECENT_TAB = "\u0000recent";
const RECENT_KEY = "soundboard.recent";
let recent = [];
try {
  recent = JSON.parse(localStorage.getItem(RECENT_KEY) || "[]");
} catch {}

// Avant la 0.12, les lectures étaient comptées ici : on les confie au
// moteur une seule fois, qui les garde désormais avec les sons.
const PLAYS_KEY = "soundboard.plays";
async function migratePlayCounts() {
  let counts = null;
  try {
    counts = JSON.parse(localStorage.getItem(PLAYS_KEY) || "null");
  } catch {}
  if (!counts || !Object.keys(counts).length) return;
  try {
    render(await invoke("import_play_counts", { counts }));
    localStorage.removeItem(PLAYS_KEY);
  } catch {}
}

function remember(id) {
  recent = [id, ...recent.filter((r) => r !== id)].slice(0, 12);
  try {
    localStorage.setItem(RECENT_KEY, JSON.stringify(recent));
  } catch {}
  if (currentTab === RECENT_TAB) renderGrid();
}

/** Onglet à passer aux commandes (au hasard, export) : « Récents » vaut « Tous ». */
function tabCategory() {
  return currentTab === RECENT_TAB ? null : currentTab;
}

function renderTabs() {
  const cats = categories();
  if (currentTab && currentTab !== RECENT_TAB && !cats.includes(currentTab)) currentTab = null;
  const tab = (label, value, iconName) => {
    const b = document.createElement("button");
    b.type = "button";
    b.className = "tab" + (currentTab === value ? " active" : "");
    if (iconName) b.append(svgIcon(iconName));
    b.append(label);
    b.addEventListener("click", () => {
      currentTab = value;
      try {
        if (value) localStorage.setItem(TAB_KEY, value);
        else localStorage.removeItem(TAB_KEY);
      } catch {}
      renderTabs();
      renderGrid();
    });
    return b;
  };
  $("#tabs").replaceChildren(tab("Tous", null), tab("Récents", RECENT_TAB, "clock"), ...cats.map((c) => tab(c, c)));
  $("#categories").replaceChildren(...cats.map((c) => new Option(c)));
  $("#btn-export").title = tabCategory()
    ? `Exporter « ${tabCategory()} » : enregistrer ses sons dans un .zip à partager`
    : "Exporter : enregistrer tous les sons dans un .zip à partager";
}

function renderStatus() {
  const pill = $("#cable-status");
  const s = state.config.settings;
  const ok = s.virtual_device && state.devices.outputs.includes(s.virtual_device);
  pill.className = "pill " + (ok ? "ok" : "ko");
  pill.textContent = ok ? "● Discord : actif" : "● Micro virtuel non configuré";
}

function renderWarnings() {
  const el = $("#warnings");
  $("[data-text]", el).textContent = state.warnings.join("\n");
  el.hidden = state.warnings.length === 0;
}

/** Icône du jeu d'icônes de index.html. */
function svgIcon(name) {
  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  svg.setAttribute("class", "ico");
  const use = document.createElementNS("http://www.w3.org/2000/svg", "use");
  use.setAttribute("href", "#i-" + name);
  svg.append(use);
  return svg;
}

const fold = (text) => text.normalize("NFD").replace(/[\u0300-\u036f]/g, "").toLowerCase();

function renderGrid() {
  const grid = $("#grid");
  const q = fold($("#search").value.trim());
  // Sans accents ni majuscules, dans le nom ou l'onglet.
  const matches = (s) => !q || fold(s.name).includes(q) || fold(s.category || "").includes(q);
  let sounds;
  if (currentTab === RECENT_TAB) {
    const byId = new Map(state.config.sounds.map((s) => [s.id, s]));
    sounds = recent.map((id) => byId.get(id)).filter((s) => s && matches(s));
  } else {
    sounds = sortSounds(state.config.sounds.filter((s) => (!currentTab || s.category === currentTab) && matches(s)));
  }
  const pads = sounds.map(padElement);
  // En liste, une ligne d'en-tête comme une playlist.
  if (viewMode === "list" && pads.length) pads.unshift(listHeader());
  grid.replaceChildren(...pads);
  $("#empty").hidden = state.config.sounds.length > 0;
  $("#recent-empty").hidden = !(currentTab === RECENT_TAB && sounds.length === 0 && state.config.sounds.length > 0 && !q);
  $("#search-empty").hidden = !(q && sounds.length === 0 && state.config.sounds.length > 0);
  renderPlaying();
}

// Sons en cours, envoyés par le moteur audio ~10 fois par seconde.
let playing = [];

// Sons affichés comme « en cours » au dernier passage : seuls eux et les
// nouveaux sons en cours sont mis à jour, pas toute la grille 10 fois par seconde.
let shownPlaying = new Set();

function renderPlaying() {
  const byId = new Map(playing.map((p) => [p.id, p]));
  const ids = new Set([...shownPlaying, ...byId.keys()]);
  shownPlaying = new Set(byId.keys());
  ids.forEach((id) => {
    const pad = document.querySelector(`#grid .pad[data-id="${CSS.escape(id)}"]`);
    if (!pad) return;
    const p = byId.get(id);
    pad.classList.toggle("playing", !!p);
    const img = $(".icon.image img", pad);
    const sound = img && soundById(id);
    // Seulement quand l'image change : remettre la même adresse 10 fois par
    // seconde fait redémarrer le GIF (il « vibre »).
    const src = sound?.image && fileSrc(p ? sound.image : sound.image_still || sound.image);
    if (src && img.getAttribute("src") !== src) img.src = src;
    let ratio = 0;
    if (p && p.duration_ms) {
      ratio = p.looping ? (p.elapsed_ms % p.duration_ms) / p.duration_ms : Math.min(1, p.elapsed_ms / p.duration_ms);
    }
    pad.style.setProperty("--progress", ratio);
  });
}

// ---------- Images et emojis automatiques ----------

const { convertFileSrc } = window.__TAURI__.core;
const fileSrc = (path) => convertFileSrc(path);
const soundById = (id) => state.config.sounds.find((s) => s.id === id);

// Mot du nom du son → emoji, quand le son n'a ni image ni emoji choisi.
const AUTO_EMOJIS = [
  [/(chèvre|chevre|goat)/, "🐐"],
  [/(\bchat\b|\bcats?\b|miaou|meow)/, "🐱"],
  [/(chien|\bdogs?\b|wouf|woof|bark)/, "🐶"],
  [/(canard|duck|\bcoin coin)/, "🦆"],
  [/(vache|\bcow|meuh)/, "🐄"],
  [/(rire|rigol|laugh|\blol\b|mdr|ptdr|haha|\bxd\b)/, "😂"],
  [/(bruh|bro+h)/, "😐"],
  [/(explos|boom|kaboom|bomb|nuke)/, "💥"],
  [/(\bfeu\b|fire|flamme|burn)/, "🔥"],
  [/(\bmort|dead|death|\brip\b|skull|crâne|crane)/, "💀"],
  [/(\bcri\b|\bcris\b|scream|hurle|aaa+h?)/, "😱"],
  [/(applau|\bclap|bravo)/, "👏"],
  [/(fête|\bfete|party|victoire|victory|\bwin\b|\bgg\b)/, "🎉"],
  [/(\bfail|perdu|\blose|loser|\bsad\b|triste|trombone|womp)/, "😢"],
  [/(sirène|sirene|siren|alarm|alerte|police)/, "🚨"],
  [/(\bpet\b|fart|prout)/, "💨"],
  [/(cloche|\bbell|\bding|\bdong)/, "🔔"],
  [/(tambour|drum|ba dum|rimshot|tss)/, "🥁"],
  [/(trompette|trumpet|airhorn|klaxon|\bhorn|\bmlg\b)/, "📯"],
  [/(music|musique|\bsong|chanson|\bbeat|remix)/, "🎵"],
  [/(\bwow\b|\bomg\b|ohlala|oh la la|surpris)/, "😮"],
  [/(\bnon\b|\bnope\b|\bno+\b|\bstop\b)/, "🙅"],
  [/(\boui\b|\byes\b|yeah|\bok\b)/, "👍"],
  [/(\bsus\b|among us|impost)/, "🔴"],
  [/(\bvine\b|\bthud\b|bonk)/, "🪨"],
  [/(clown|honk)/, "🤡"],
  [/(angry|colère|colere|\brage\b|énervé|enerve)/, "😡"],
  [/(coeur|cœur|\blove|amour|bisou|kiss)/, "❤️"],
  [/(argent|money|cash|cha-?ching)/, "💰"],
  [/(fantôme|fantome|ghost|\bboo\b)/, "👻"],
  [/(bonjour|hello|salut|coucou)/, "👋"],
  [/(dodo|sleep|ronfl|snore)/, "😴"],
  [/(manger|\beat\b|miam|food|pizza)/, "🍕"],
  [/(voiture|\bcar\b|vroom|drift)/, "🏎️"],
  [/(pistolet|\bgun|gunshot|\bbang\b)/, "🔫"],
  [/(épée|\bepee|sword|ninja)/, "⚔️"],
  [/(bébé|\bbebe|\bbaby)/, "👶"],
  [/(prière|priere|\bpray|amen|hallelujah|alléluia)/, "🙏"],
  [/(question|\bhein\b|\bhuh\b|\bquoi\b|\bwhat\b)/, "❓"],
];

function autoEmoji(name) {
  const text = ` ${name.toLowerCase()} `;
  for (const [re, emoji] of AUTO_EMOJIS) if (re.test(text)) return emoji;
  return null;
}

// Les sons ajoutés reçoivent une image GIPHY si une clé est réglée (une
// seule tentative par son et par lancement).
const imageTried = new Set();
let autoImageRunning = false;
async function fetchMissingImages() {
  if (autoImageRunning || !state.config.settings.giphy_key) return;
  const ids = state.config.sounds.filter((s) => !s.image && !imageTried.has(s.id)).map((s) => s.id);
  if (!ids.length) return;
  ids.forEach((id) => imageTried.add(id));
  autoImageRunning = true;
  try {
    await invoke("auto_images", { ids });
  } catch (e) {
    toast(String(e), true);
  } finally {
    autoImageRunning = false;
  }
}

// ---------- Tri et affichage ----------

let sortMode = "favorites";
let viewMode = "grid";
try {
  sortMode = localStorage.getItem("soundboard.sort") || sortMode;
  viewMode = localStorage.getItem("soundboard.view") || viewMode;
} catch {}

/** Durée jouée (découpe comprise) en millisecondes, ou null si inconnue. */
function playDuration(sound) {
  const total = sound.analysis?.duration_ms;
  if (total == null) return null;
  return Math.max(0, Math.min(sound.end_ms ?? total, total) - sound.start_ms);
}

function formatDuration(ms) {
  const s = Math.round(ms / 1000);
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
}

function hue(color) {
  const n = parseInt(String(color).slice(1), 16);
  if (Number.isNaN(n)) return 0;
  const [r, g, b] = [(n >> 16) & 255, (n >> 8) & 255, n & 255].map((v) => v / 255);
  const max = Math.max(r, g, b);
  const d = max - Math.min(r, g, b);
  if (!d) return 0;
  const h = max === r ? ((g - b) / d) % 6 : max === g ? (b - r) / d + 2 : (r - g) / d + 4;
  return (h * 60 + 360) % 360;
}

function sortSounds(list) {
  const index = new Map(state.config.sounds.map((s, i) => [s.id, i]));
  const mine = (a, b) => index.get(a.id) - index.get(b.id);
  const by = {
    manual: mine,
    favorites: (a, b) => b.favorite - a.favorite || mine(a, b),
    name: (a, b) => a.name.localeCompare(b.name, "fr", { sensitivity: "base", numeric: true }),
    plays: (a, b) => (b.plays || 0) - (a.plays || 0) || mine(a, b),
    // Les sons d'avant la 0.11 n'ont pas de date : les derniers de la liste
    // sont les plus récents.
    added: (a, b) => (b.added_at || 0) - (a.added_at || 0) || mine(b, a),
    duration: (a, b) => (playDuration(a) ?? Infinity) - (playDuration(b) ?? Infinity) || mine(a, b),
    color: (a, b) => hue(a.color) - hue(b.color) || mine(a, b),
  }[sortMode] || mine;
  return [...list].sort(by);
}

/** Le glisser-déposer ne sert qu'aux ordres choisis à la main. */
function canReorder() {
  return currentTab !== RECENT_TAB && (sortMode === "manual" || sortMode === "favorites");
}

function applyView() {
  $("#grid").classList.toggle("list", viewMode === "list");
  $(".tabbar .size").hidden = viewMode === "list";
  document.querySelectorAll(".view-toggle button").forEach((b) => b.classList.toggle("on", b.dataset.view === viewMode));
}

$("#sort").value = sortMode;
$("#sort").addEventListener("change", (e) => {
  sortMode = e.target.value;
  try {
    localStorage.setItem("soundboard.sort", sortMode);
  } catch {}
  renderGrid();
});
document.querySelectorAll(".view-toggle button").forEach((b) =>
  b.addEventListener("click", () => {
    viewMode = b.dataset.view;
    try {
      localStorage.setItem("soundboard.view", viewMode);
    } catch {}
    applyView();
    renderGrid();
  }),
);
applyView();

function listHeader() {
  const head = document.createElement("div");
  head.className = "list-head";
  for (const [cls, text] of [["num", "#"], ["title", "Titre"], ["key", "Raccourci"], ["dur", "⏱"], ["tools", ""]]) {
    const cell = document.createElement("span");
    cell.className = cls;
    cell.textContent = text;
    head.append(cell);
  }
  return head;
}

function padElement(sound, index) {
  const pad = document.createElement("button");
  pad.type = "button";
  pad.className = "pad";
  pad.dataset.id = sound.id;
  pad.style.setProperty("--pad-color", sound.color);

  const icon = document.createElement("span");
  const emoji = sound.emoji || autoEmoji(sound.name);
  if (sound.image_still) {
    // Image fixe au repos, animée pendant que le son joue (voir renderPlaying).
    icon.className = "icon image";
    const img = document.createElement("img");
    img.alt = "";
    img.draggable = false;
    img.loading = "lazy";
    img.src = fileSrc(sound.image_still);
    icon.append(img);
  } else {
    icon.className = "icon" + (emoji ? " emoji" : "");
    // Sans emoji : l'initiale du son dans une pastille de sa couleur.
    icon.textContent = emoji || [...sound.name.trim()][0]?.toUpperCase() || "♪";
  }
  const eq = document.createElement("span");
  eq.className = "eq";
  eq.append(...[0, 1, 2, 3].map(() => document.createElement("i")));
  const name = document.createElement("span");
  name.className = "name" + (sound.name.split(/\s+/).some((w) => w.length > 12) ? " long-word" : "");
  name.textContent = sound.name;
  // Numéro, catégorie et durée : visibles seulement en liste.
  const num = document.createElement("span");
  num.className = "num";
  num.textContent = index + 1;
  const meta = document.createElement("span");
  meta.className = "meta";
  meta.textContent = sound.category || "";
  const label = document.createElement("span");
  label.className = "label";
  label.append(name, meta);
  const dur = document.createElement("span");
  dur.className = "dur";
  const duration = playDuration(sound);
  dur.textContent = duration != null ? formatDuration(duration) : "";
  const key = document.createElement("span");
  key.className = "key";
  if (sound.looping) {
    const loop = document.createElement("b");
    loop.textContent = "↻ boucle";
    key.append(loop);
  }
  if (sound.hotkey) {
    const kbd = document.createElement("kbd");
    kbd.textContent = hotkeyLabel(sound.hotkey);
    key.append(kbd);
  }
  const edit = document.createElement("span");
  edit.className = "edit";
  edit.append(svgIcon("pencil"));
  edit.title = "Modifier";
  const fav = document.createElement("span");
  fav.className = "fav" + (sound.favorite ? " on" : "");
  fav.append(svgIcon("star"));
  fav.title = sound.favorite ? "Retirer des favoris" : "Épingler en haut (favori)";
  const progress = document.createElement("span");
  progress.className = "progress";
  pad.append(num, eq, icon, label, key, dur, fav, edit, progress);

  pad.addEventListener("click", (e) => {
    if (reorder.justDropped) return;
    if (edit.contains(e.target)) return openSoundDialog(sound);
    if (fav.contains(e.target)) return call("toggle_favorite", { id: sound.id });
    flash(sound.id);
    call("play_sound", { id: sound.id }); // joue, ou arrête s'il est déjà en cours
  });
  pad.addEventListener("contextmenu", (e) => {
    e.preventDefault();
    openSoundDialog(sound);
  });

  pad.addEventListener("pointerdown", (e) => startReorder(e, pad, sound));
  return pad;
}

// Réorganisation à la souris : on glisse un pad sur un autre. Le glisser-déposer
// HTML5 n'est pas utilisable sous Windows tant que le dépôt de fichiers est actif.
const reorder = { justDropped: false };

function startReorder(e, pad, sound) {
  if (e.button !== 0 || !canReorder()) return;
  const x0 = e.clientX;
  const y0 = e.clientY;
  let dragging = false;
  let target = null;

  const move = (ev) => {
    if (!dragging && Math.hypot(ev.clientX - x0, ev.clientY - y0) < 8) return;
    dragging = true;
    pad.style.opacity = "0.5";
    const over = document.elementFromPoint(ev.clientX, ev.clientY)?.closest(".pad");
    if (target && target !== over) target.classList.remove("drag-over");
    target = over && over !== pad ? over : null;
    target?.classList.add("drag-over");
  };
  const up = () => {
    window.removeEventListener("pointermove", move);
    window.removeEventListener("pointerup", up);
    pad.style.opacity = "";
    target?.classList.remove("drag-over");
    if (!dragging) return;
    // Le clic qui suit le relâchement ne doit pas jouer le son.
    reorder.justDropped = true;
    setTimeout(() => (reorder.justDropped = false), 0);
    if (target) {
      const to = state.config.sounds.findIndex((s) => s.id === target.dataset.id);
      call("move_sound", { id: sound.id, to });
    }
  };
  window.addEventListener("pointermove", move);
  window.addEventListener("pointerup", up);
}

function flash(id) {
  const pad = document.querySelector(`.pad[data-id="${CSS.escape(id)}"]`);
  if (!pad) return;
  pad.classList.add("flash");
  setTimeout(() => pad.classList.remove("flash"), 250);
}

// ---------- Dialogue « Modifier le son » ----------

const dlgSound = $("#dlg-sound");
const formSound = $("form", dlgSound);
let editing = null;
setupHotkeyInput(formSound.hotkey);
bindRangeLabels(formSound);
enterSaves(dlgSound);

function openSoundDialog(sound) {
  editing = sound;
  formSound.sound_name.value = sound.name;
  setHotkeyInput(formSound.hotkey, sound.hotkey);
  formSound.volume.value = sound.volume;
  formSound.volume.update();
  formSound.color.value = sound.color;
  formSound.category.value = sound.category ?? "";
  formSound.looping.checked = !!sound.looping;
  formSound.keep_playing.checked = !!sound.keep_playing;
  formSound.emoji.value = sound.emoji ?? "";
  renderEmojiPicks();
  renderImagePreview();
  $("#image-search").hidden = true;
  $("#image-results").replaceChildren();
  $("#image-query").value = sound.name;
  dlgSound.returnValue = "";
  dlgSound.showModal();
  openTrim(sound);
}

// ---------- Image du son ----------

function renderImagePreview() {
  const box = $("#image-preview");
  box.replaceChildren();
  const current = soundById(editing.id) ?? editing;
  if (current.image) {
    const img = document.createElement("img");
    img.alt = "";
    img.src = fileSrc(current.image);
    box.append(img);
  } else {
    box.textContent = formSound.emoji.value || autoEmoji(current.name) || "🖼️";
  }
  $('[data-action="image-clear"]', dlgSound).disabled = !current.image;
}

async function imageAction(cmd, args) {
  const id = editing.id;
  try {
    render(await invoke(cmd, { id, ...args }));
    renderImagePreview();
  } catch (e) {
    toast(String(e), true);
  }
}

async function searchImages() {
  const results = $("#image-results");
  const query = $("#image-query").value.trim() || editing.name;
  results.replaceChildren();
  results.textContent = "Recherche…";
  let found = [];
  try {
    found = await invoke("search_images", { query });
  } catch (e) {
    results.textContent = String(e);
    return;
  }
  results.textContent = found.length ? "" : "Aucune image trouvée : essaie d'autres mots (en anglais).";
  for (const r of found) {
    const b = document.createElement("button");
    b.type = "button";
    b.title = r.title;
    const img = document.createElement("img");
    img.alt = r.title;
    img.loading = "lazy";
    img.src = r.anim;
    b.append(img);
    b.addEventListener("click", async () => {
      b.classList.add("busy");
      await imageAction("set_sound_image", { still: r.still, anim: r.anim });
      b.classList.remove("busy");
    });
    results.append(b);
  }
}

$('[data-action="image-search"]', dlgSound).addEventListener("click", () => {
  const box = $("#image-search");
  box.hidden = !box.hidden;
  if (!box.hidden) {
    if (!state.config.settings.giphy_key) {
      $("#image-results").textContent = "Ajoute ta clé GIPHY dans les réglages (⚙ → Général) pour chercher des GIF.";
      return;
    }
    searchImages();
  }
});
$('[data-action="image-go"]', dlgSound).addEventListener("click", searchImages);
$("#image-query").addEventListener("keydown", (e) => {
  if (e.key === "Enter") {
    e.preventDefault();
    e.stopPropagation();
    searchImages();
  }
});
$('[data-action="image-file"]', dlgSound).addEventListener("click", () => imageAction("pick_sound_image"));
$('[data-action="image-clear"]', dlgSound).addEventListener("click", () => imageAction("clear_sound_image"));

const EMOJIS = ["😂", "🤣", "😱", "💀", "🔥", "💥", "👏", "🎉", "📯", "🥁", "🎺", "🎵", "🚨", "🐐", "🤡", "👀", "🍆", "💨", "😡", "🙏"];

function renderEmojiPicks() {
  const box = $("#emoji-picks");
  const current = formSound.emoji.value;
  box.replaceChildren(
    ...EMOJIS.map((e) => {
      const b = document.createElement("button");
      b.type = "button";
      b.textContent = e;
      b.classList.toggle("active", e === current);
      b.addEventListener("click", () => {
        formSound.emoji.value = formSound.emoji.value === e ? "" : e;
        renderEmojiPicks();
      });
      return b;
    })
  );
}
formSound.emoji.addEventListener("input", () => {
  renderEmojiPicks();
  renderImagePreview();
});

// ---------- Découpe (forme d'onde) ----------

const canvas = $("#wave");
const trim = { duration: 0, start: 0, end: 0, peaks: [], drag: null };

function fmtTime(ms) {
  const s = ms / 1000;
  return s < 60 ? s.toFixed(2) + " s" : `${Math.floor(s / 60)} min ${(s % 60).toFixed(1).padStart(4, "0")} s`;
}

async function openTrim(sound) {
  const box = $(".trim", dlgSound);
  trim.duration = sound.analysis?.duration_ms ?? 0;
  box.hidden = !trim.duration;
  if (!trim.duration) return;
  trim.start = sound.start_ms ?? 0;
  trim.end = Math.min(sound.end_ms ?? trim.duration, trim.duration);
  trim.peaks = [];
  drawTrim();
  try {
    trim.peaks = await invoke("get_waveform", { id: sound.id });
  } catch (e) {
    trim.peaks = [];
  }
  drawTrim();
}

function drawTrim() {
  const ctx = canvas.getContext("2d");
  const { width: w, height: h } = canvas;
  const css = getComputedStyle(document.documentElement);
  ctx.clearRect(0, 0, w, h);
  const x = (ms) => (ms / trim.duration) * w;
  const xs = x(trim.start);
  const xe = x(trim.end);

  // Forme d'onde : une barre par pic, plus sombre hors de la zone gardée.
  const n = trim.peaks.length;
  for (let i = 0; i < n; i++) {
    const px = (i / n) * w;
    const bh = Math.max(2, trim.peaks[i] * (h - 10));
    const inside = px >= xs && px <= xe;
    ctx.fillStyle = inside ? editing?.color ?? "#4f46e5" : css.getPropertyValue("--wave-off");
    ctx.fillRect(px, (h - bh) / 2, Math.max(1, w / n - 1), bh);
  }
  if (!n) {
    ctx.fillStyle = css.getPropertyValue("--muted");
    ctx.font = "26px Segoe UI, sans-serif";
    ctx.fillText("Chargement de la forme d'onde…", 20, h / 2 + 8);
  }
  ctx.fillStyle = css.getPropertyValue("--wave-mask");
  ctx.fillRect(0, 0, xs, h);
  ctx.fillRect(xe, 0, w - xe, h);
  for (const hx of [xs, xe]) {
    ctx.fillStyle = css.getPropertyValue("--text");
    ctx.fillRect(hx - 2, 0, 4, h);
    ctx.fillRect(hx - 9, 0, 18, 14);
    ctx.fillRect(hx - 9, h - 14, 18, 14);
  }
  const now = typeof playheadMs === "function" ? playheadMs() : null;
  if (now !== null) {
    const px = x(now);
    ctx.fillStyle = css.getPropertyValue("--accent");
    ctx.fillRect(px - 2, 0, 4, h);
    ctx.beginPath();
    ctx.moveTo(px - 10, 0);
    ctx.lineTo(px + 10, 0);
    ctx.lineTo(px, 14);
    ctx.fill();
  }
  $("#trim-info").textContent =
    (now !== null ? `▶ ${fmtTime(now)}  ·  ` : "") +
    `${fmtTime(trim.start)} → ${fmtTime(trim.end)}  (durée ${fmtTime(trim.end - trim.start)})`;
}

function msAt(e) {
  const r = canvas.getBoundingClientRect();
  const ratio = Math.min(1, Math.max(0, (e.clientX - r.left) / r.width));
  return Math.round(ratio * trim.duration);
}

const MIN_TRIM_MS = 50;

canvas.addEventListener("pointerdown", (e) => {
  if (!trim.duration) return;
  const ms = msAt(e);
  trim.drag = Math.abs(ms - trim.start) <= Math.abs(ms - trim.end) ? "start" : "end";
  canvas.setPointerCapture(e.pointerId);
  moveHandle(ms);
});
canvas.addEventListener("pointermove", (e) => trim.drag && moveHandle(msAt(e)));
canvas.addEventListener("pointerup", () => (trim.drag = null));

function moveHandle(ms) {
  if (trim.drag === "start") trim.start = Math.min(ms, trim.end - MIN_TRIM_MS);
  else trim.end = Math.max(ms, trim.start + MIN_TRIM_MS);
  trim.start = Math.max(0, trim.start);
  trim.end = Math.min(trim.duration, trim.end);
  drawTrim();
}

$('[data-action="reset-trim"]', dlgSound).addEventListener("click", () => {
  trim.start = 0;
  trim.end = trim.duration;
  drawTrim();
});

const PREVIEW_ID = "__preview";
const previewBtn = $('[data-action="preview"]', dlgSound);
// Curseur de lecture : le moteur donne l'avancement ~10 fois par seconde,
// on l'interpole entre deux mises à jour pour un mouvement fluide.
const playhead = { from: 0, elapsed: 0, at: 0, active: false, frame: 0 };

previewBtn.addEventListener("click", () => {
  if (!editing) return;
  if (playhead.active) {
    invoke("stop_preview");
    return;
  }
  playhead.from = trim.start;
  call("preview_sound", {
    id: editing.id,
    startMs: trim.start,
    endMs: trim.end >= trim.duration ? null : trim.end,
    volume: Number(formSound.volume.value),
  });
});

function updatePlayhead(status) {
  const p = status.find((s) => s.id === PREVIEW_ID);
  const wasActive = playhead.active;
  playhead.active = !!p && dlgSound.open;
  previewBtn.textContent = playhead.active ? "■ Arrêter" : "▶ Écouter";
  if (!playhead.active) {
    cancelAnimationFrame(playhead.frame);
    if (wasActive) drawTrim();
    return;
  }
  playhead.elapsed = p.elapsed_ms;
  playhead.at = performance.now();
  if (!wasActive) animatePlayhead();
}

function playheadMs() {
  if (!playhead.active) return null;
  const ms = playhead.from + playhead.elapsed + (performance.now() - playhead.at);
  return Math.min(ms, trim.end);
}

function animatePlayhead() {
  drawTrim();
  if (playhead.active) playhead.frame = requestAnimationFrame(animatePlayhead);
}

function trimValues() {
  if (!trim.duration) return { start_ms: editing.start_ms ?? 0, end_ms: editing.end_ms ?? null };
  return { start_ms: trim.start, end_ms: trim.end >= trim.duration ? null : trim.end };
}

dlgSound.addEventListener("close", () => {
  if (playhead.active) invoke("stop_preview");
  if (dlgSound.returnValue !== "save" || !editing) return;
  call("update_sound", {
    sound: {
      ...editing,
      name: formSound.sound_name.value.trim() || editing.name,
      hotkey: formSound.hotkey.dataset.hotkey || null,
      volume: Number(formSound.volume.value),
      color: formSound.color.value,
      category: formSound.category.value.trim() || null,
      looping: formSound.looping.checked,
      keep_playing: formSound.keep_playing.checked,
      emoji: formSound.emoji.value.trim() || null,
      ...trimValues(),
    },
  });
});

$('[data-action="delete"]', dlgSound).addEventListener("click", () => {
  if (!editing) return;
  dlgSound.close("delete");
  call("remove_sound", { id: editing.id });
});

// ---------- Dialogue « Réglages » ----------

const dlgSettings = $("#dlg-settings");
const formSettings = $("form", dlgSettings);
const SETTINGS_HOTKEYS = ["stop_hotkey", "mute_hotkey", "random_hotkey"];
for (const k of SETTINGS_HOTKEYS) setupHotkeyInput(formSettings[k]);
bindRangeLabels(formSettings);
enterSaves(dlgSettings);

/** Relit les périphériques audio (lent sous Windows, donc seulement ici). */
async function refreshDevices() {
  try {
    render(await invoke("refresh_devices"));
  } catch {}
}

async function openSettings() {
  await refreshDevices();
  const s = state.config.settings;
  const d = state.devices;
  fillSelect(formSettings.monitor_device, d.outputs, s.monitor_device, `Par défaut (${d.default_output ?? "aucun"})`);
  fillSelect(formSettings.virtual_device, d.outputs, s.virtual_device, "Aucun (sons uniquement dans le casque)");
  fillSelect(formSettings.mic_device, d.inputs, s.mic_device, `Par défaut (${d.default_input ?? "aucun"})`);
  formSettings.mic_enabled.checked = s.mic_enabled;
  formSettings.denoise_mode.value = !s.noise_suppression ? "off" : s.strong_denoise ? "strong" : "light";
  formSettings.voice_gate.checked = s.voice_gate;
  formSettings.gate_auto.checked = s.gate_auto ?? true;
  formSettings.voice_leveler.checked = s.voice_leveler ?? true;
  updateGateRows();
  for (const k of ["mic_volume", "master_volume", "monitor_volume", "gate_threshold"]) {
    formSettings[k].value = s[k];
    formSettings[k].update();
  }
  formSettings.stop_previous.checked = s.stop_previous;
  formSettings.normalize.checked = s.normalize;
  formSettings.loudness_target.value = String(nearestTarget(s.loudness_target));
  formSettings.launch_at_startup.checked = s.launch_at_startup;
  formSettings.close_to_tray.checked = s.close_to_tray;
  for (const k of SETTINGS_HOTKEYS) setHotkeyInput(formSettings[k], s[k]);
  formSettings.phone_enabled.checked = s.phone_enabled;
  formSettings.phone_port.value = s.phone_port;
  renderPhoneInfo();
  formSettings.voice_effect.value = s.voice_effect ?? "none";
  formSettings.voice_eq.value = s.voice_eq ?? "natural";
  $("#calib").hidden = true;
  formSettings.freesound_key.value = s.freesound_key ?? "";
  formSettings.giphy_key.value = s.giphy_key ?? "";
  updateNormalizeRow();
  dlgSettings.returnValue = "";
  dlgSettings.showModal();
  startMeter();
}

function nearestTarget(v) {
  const options = [-31, -28, -25];
  return options.reduce((best, o) => (Math.abs(o - v) < Math.abs(best - v) ? o : best), -28);
}

// Onglets des réglages (le dernier ouvert est retenu).
function showSettingsTab(tab) {
  for (const b of $$(".set-nav [data-tab]", dlgSettings)) b.classList.toggle("on", b.dataset.tab === tab);
  for (const p of $$("[data-pane]", dlgSettings)) p.hidden = p.dataset.pane !== tab;
  try { localStorage.setItem("soundboard.settingsTab", tab); } catch {}
}
for (const b of $$(".set-nav [data-tab]", dlgSettings)) {
  b.addEventListener("click", () => showSettingsTab(b.dataset.tab));
}
try { showSettingsTab(localStorage.getItem("soundboard.settingsTab") || "sound"); } catch {}
// Le volume ne se règle plus quand l'égalisation est coupée.
function updateNormalizeRow() {
  formSettings.loudness_target.disabled = !formSettings.normalize.checked;
}
formSettings.normalize.addEventListener("change", updateNormalizeRow);

// Indicateur du micro : le moteur envoie le niveau tant que les réglages sont ouverts.
const meter = $("#mic-meter");

function startMeter() {
  updateThresholdMark();
  $("[data-level]", meter).style.width = "0%";
  $("[data-voice]", meter).style.width = "0%";
  const s = state.config.settings;
  $("[data-state]", meter).textContent = s.mic_enabled
    ? "Parle pour tester ton micro…"
    : "Ta voix n'est pas envoyée (« Envoyer ma voix » est désactivé).";
  meter.classList.remove("open", "closed");
  invoke("set_meter", { on: true });
}

function updateThresholdMark() {
  $("[data-threshold]", meter).style.left = formSettings.gate_threshold.value * 100 + "%";
  $("[data-threshold]", meter).hidden = formSettings.gate_auto.checked;
}

// En automatique, le seuil ne sert plus : la porte suit le bruit de la pièce.
function updateGateRows() {
  const on = formSettings.voice_gate.checked;
  formSettings.gate_auto.disabled = !on;
  formSettings.gate_threshold.disabled = !on || formSettings.gate_auto.checked;
  updateThresholdMark();
}
formSettings.voice_gate.addEventListener("change", updateGateRows);
formSettings.gate_auto.addEventListener("change", updateGateRows);
formSettings.gate_threshold.addEventListener("input", updateThresholdMark);

listen("mic-level", ({ payload: m }) => {
  if (!dlgSettings.open) return;
  $("[data-level]", meter).style.width = m.level * 100 + "%";
  $("[data-voice]", meter).style.width = m.voice * 100 + "%";
  meter.classList.toggle("open", m.open);
  meter.classList.toggle("closed", !m.open);
  $("[data-state]", meter).textContent = state.mic_muted
    ? "🔇 Micro coupé par le raccourci"
    : m.open
      ? "● Ta voix passe"
      : formSettings.gate_auto.checked
        ? "○ Micro fermé : bruit de fond ignoré automatiquement"
        : "○ Micro fermé (pas de voix détectée)";
});

// QR code de la page du téléphone (seulement quand c'est activé et enregistré).
async function renderPhoneInfo() {
  const box = $("#phone-info");
  box.hidden = true;
  if (!state.config.settings.phone_enabled) return;
  let info = null;
  try {
    info = await invoke("get_phone_info");
  } catch (e) {
    toast(String(e), true);
  }
  if (!info) return;
  // SVG produit par l'app elle-même (bibliothèque qrcode).
  $("#phone-qr").innerHTML = info.qr_svg;
  $("#phone-url").textContent = info.url;
  box.hidden = false;
}

formSettings.phone_enabled.addEventListener("change", () => {
  if (formSettings.phone_enabled.checked && !state.config.settings.phone_enabled) {
    $("#phone-info").hidden = true;
    toast("Clique sur « Enregistrer » : le QR code s'affichera en rouvrant les réglages");
  }
});

$('[data-action="phone-token"]', dlgSettings).addEventListener("click", async () => {
  const res = await call("regenerate_phone_token");
  if (res) {
    renderPhoneInfo();
    toast("Nouveau code : rescanne le QR code sur ton téléphone");
  }
});

$('[data-action="diagnose-settings"]', dlgSettings).addEventListener("click", openDiagnostics);
$('[data-action="listen-settings"]', dlgSettings).addEventListener("click", () => setListening(!listening));
dlgSettings.addEventListener("close", () => listening && !dlgDiag.open && setListening(false));
$('[data-action="hear-myself"]', dlgSettings).addEventListener("click", () => setHearing(!hearing));
$('[data-action="calibrate"]', dlgSettings).addEventListener("click", calibrateVoice);
$('[data-action="open-recordings"]', dlgSettings).addEventListener("click", () => call("open_recordings"));
dlgSettings.addEventListener("close", () => hearing && setHearing(false));

$('[data-action="giphy-key"]', dlgSettings).addEventListener("click", (e) => {
  e.preventDefault();
  call("open_giphy_key_page");
});

$('[data-action="auto-images"]', dlgSettings).addEventListener("click", async (e) => {
  const button = e.currentTarget;
  if (!state.config.settings.giphy_key) {
    toast("Colle ta clé GIPHY puis enregistre avant d'illustrer tes sons.", true);
    return;
  }
  button.disabled = true;
  button.textContent = "Recherche…";
  try {
    const n = await invoke("auto_images", { ids: null });
    toast(n ? `${n} son${n > 1 ? "s" : ""} illustré${n > 1 ? "s" : ""}` : "Tous tes sons ont déjà une image (ou rien n'a été trouvé).");
  } catch (err) {
    toast(String(err), true);
  } finally {
    button.disabled = false;
    button.textContent = "Illustrer";
  }
});

$('[data-action="freesound-key"]', dlgSettings).addEventListener("click", (e) => {
  e.preventDefault();
  call("open_freesound_key_page");
});

dlgSettings.addEventListener("close", () => {
  invoke("set_meter", { on: false });
  if (dlgSettings.returnValue !== "save") return;
  const f = formSettings;
  saveSettings({
    monitor_device: f.monitor_device.value || null,
    virtual_device: f.virtual_device.value || null,
    mic_device: f.mic_device.value || null,
    mic_enabled: f.mic_enabled.checked,
    mic_volume: Number(f.mic_volume.value),
    noise_suppression: f.denoise_mode.value !== "off",
    strong_denoise: f.denoise_mode.value === "strong",
    voice_gate: f.voice_gate.checked,
    gate_auto: f.gate_auto.checked,
    voice_leveler: f.voice_leveler.checked,
    gate_threshold: Number(f.gate_threshold.value),
    master_volume: Number(f.master_volume.value),
    monitor_volume: Number(f.monitor_volume.value),
    stop_previous: f.stop_previous.checked,
    normalize: f.normalize.checked,
    loudness_target: Number(f.loudness_target.value),
    launch_at_startup: f.launch_at_startup.checked,
    close_to_tray: f.close_to_tray.checked,
    stop_hotkey: f.stop_hotkey.dataset.hotkey || null,
    mute_hotkey: f.mute_hotkey.dataset.hotkey || null,
    phone_enabled: f.phone_enabled.checked,
    phone_port: Math.min(65535, Math.max(1024, Number(f.phone_port.value) || 7878)),
    random_hotkey: f.random_hotkey.dataset.hotkey || null,
    voice_effect: f.voice_effect.value,
    voice_eq: f.voice_eq.value,
    freesound_key: f.freesound_key.value.trim() || null,
    giphy_key: f.giphy_key.value.trim() || null,
  });
});

$('[data-action="wizard"]', dlgSettings).addEventListener("click", () => {
  dlgSettings.close("wizard");
  openWizard();
});

async function saveSettings(patch) {
  const settings = { ...state.config.settings, ...patch };
  const res = await call("update_settings", { settings });
  if (res && res.warnings.length === 0) toast("Réglages enregistrés");
}

// ---------- Assistant ----------

const dlgWizard = $("#dlg-wizard");
const steps = [...dlgWizard.querySelectorAll("section")];
let step = 0;

function showStep(i) {
  step = Math.max(0, Math.min(steps.length - 1, i));
  steps.forEach((s, j) => (s.hidden = j !== step));
  if (step === 0) renderWizardCable();
  if (step === 1) {
    const s = state.config.settings;
    const d = state.devices;
    // On ne propose pas le câble virtuel comme casque.
    const outputs = d.outputs.filter((n) => n !== d.vb_cable);
    fillSelect($("[name=monitor_device]", steps[1]), outputs, s.monitor_device, `Par défaut (${d.default_output ?? "aucun"})`);
    fillSelect($("[name=mic_device]", steps[1]), d.inputs.filter((n) => !n.includes("CABLE Output")), s.mic_device, `Par défaut (${d.default_input ?? "aucun"})`);
  }
}

function renderWizardCable() {
  const el = $("[data-cable]", dlgWizard);
  const found = state.devices.vb_cable;
  el.className = "status " + (found ? "ok" : "ko");
  el.textContent = found
    ? `✓ VB-Cable est installé (${found}). Tu peux passer à la suite.`
    : "VB-Cable n'est pas encore détecté sur ce PC.";
}

async function openWizard() {
  await refreshDevices();
  showStep(0);
  dlgWizard.showModal();
}

dlgWizard.addEventListener("cancel", (e) => {
  // Échap ferme l'assistant sans le marquer terminé : il reviendra au prochain lancement.
  e.preventDefault();
  dlgWizard.close();
});
dlgWizard.querySelectorAll("[data-next]").forEach((b) => b.addEventListener("click", () => showStep(step + 1)));
dlgWizard.querySelectorAll("[data-prev]").forEach((b) => b.addEventListener("click", () => showStep(step - 1)));
$('[data-action="open-vb"]', dlgWizard).addEventListener("click", () => call("open_vb_cable_page"));
$('[data-action="recheck"]', dlgWizard).addEventListener("click", async () => {
  await refreshDevices();
  renderWizardCable();
});
$('[data-action="finish"]', dlgWizard).addEventListener("click", async () => {
  const d = state.devices;
  await saveSettings({
    monitor_device: $("[name=monitor_device]", steps[1]).value || null,
    mic_device: $("[name=mic_device]", steps[1]).value || null,
    virtual_device: state.config.settings.virtual_device || d.vb_cable || null,
    setup_done: true,
  });
  dlgWizard.close();
});

// ---------- Mode nuit ----------

// « auto » suit le thème de Windows ; le choix est gardé sur ce PC.
const THEMES = ["auto", "light", "dark"];
const THEME_ICONS = { auto: "auto", light: "sun", dark: "moon" };
const THEME_LABELS = { auto: "Thème : automatique (comme Windows)", light: "Thème : clair", dark: "Thème : nuit" };
const darkQuery = window.matchMedia("(prefers-color-scheme: dark)");
let theme = "auto";
try {
  theme = THEMES.includes(localStorage.getItem("soundboard.theme")) ? localStorage.getItem("soundboard.theme") : "auto";
} catch {}

function applyTheme() {
  const dark = theme === "dark" || (theme === "auto" && darkQuery.matches);
  document.documentElement.dataset.theme = dark ? "dark" : "light";
  const btn = $("#btn-theme");
  btn.replaceChildren(svgIcon(THEME_ICONS[theme]));
  btn.title = THEME_LABELS[theme] + " — clique pour changer";
  if (dlgSound.open) drawTrim();
}
darkQuery.addEventListener("change", applyTheme);
$("#btn-theme").addEventListener("click", () => {
  theme = THEMES[(THEMES.indexOf(theme) + 1) % THEMES.length];
  try {
    localStorage.setItem("soundboard.theme", theme);
  } catch {}
  applyTheme();
  toast(THEME_LABELS[theme]);
});
applyTheme();

// ---------- Diagnostic ----------

const dlgDiag = $("#dlg-diag");
const DIAG_ICONS = { ok: "✓", warn: "!", error: "✕", info: "i" };
let listening = false;

async function setListening(on) {
  try {
    await invoke("listen_cable", { on });
    listening = on;
  } catch (e) {
    listening = false;
    toast(String(e), true);
  }
  // Le moteur coupe « M'écouter » quand on écoute le câble.
  if (listening) hearing = false;
  renderListenButtons();
}

// « M'écouter » : ta voix traitée renvoyée dans ton casque.
let hearing = false;
const EQ_NAMES = { natural: "Naturelle", warm: "Chaude", clear: "Claire", custom: "Sur mesure" };

// Test de voix : le moteur enregistre 10 s d'un coup, l'écran guide la
// personne (silence puis parole) pendant ce temps.
async function calibrateVoice() {
  const box = $("#calib");
  const step = $("[data-step]", box);
  const advice = $("[data-advice]", box);
  const actions = $("[data-actions]", box);
  const button = $('[data-action="calibrate"]', dlgSettings);
  box.hidden = false;
  actions.hidden = true;
  advice.replaceChildren();
  button.disabled = true;
  const started = Date.now();
  const tick = () => {
    const t = (Date.now() - started) / 1000;
    step.textContent =
      t < 5 ? `🤫 Silence, ne parle pas… ${Math.ceil(5 - t)}`
      : t < 10 ? `🗣️ Parle normalement, comme sur Discord… ${Math.ceil(10 - t)}`
      : "Analyse…";
  };
  tick();
  const timer = setInterval(tick, 200);
  try {
    const r = await invoke("calibrate_voice");
    const db = (v) => `${Math.round(v)} dB`;
    step.textContent = `✅ Micro réglé. Bruit de la pièce ${db(r.noise_db)}, ta voix ${db(r.speech_db)}, crêtes ${db(r.peak_db)}. Couleur : ${EQ_NAMES[r.eq]}.`;
    for (const line of r.advice) {
      const li = document.createElement("li");
      li.textContent = line;
      advice.append(li);
    }
    actions.hidden = false;
    formSettings.voice_eq.value = r.eq;
    render(await invoke("get_state"));
    // Le micro a redémarré avec les nouveaux réglages.
    hearing = false;
    renderListenButtons();
  } catch (e) {
    step.textContent = `❌ ${e}`;
  } finally {
    clearInterval(timer);
    button.disabled = false;
  }
}

async function setHearing(on) {
  try {
    await invoke("hear_myself", { on });
    hearing = on;
  } catch (e) {
    hearing = false;
    toast(String(e), true);
  }
  if (hearing) listening = false;
  renderListenButtons();
}

function renderListenButtons() {
  for (const b of $$('[data-action="listen"], [data-action="listen-settings"]')) {
    b.textContent = listening ? "■ Arrêter l'écoute" : "Écouter le câble";
    b.classList.toggle("primary", listening);
  }
  const b = $('[data-action="hear-myself"]');
  b.textContent = hearing ? "■ Arrêter" : "M'écouter";
  b.classList.toggle("primary", hearing);
}

async function runDiagnostics() {
  const list = $("#diag-list");
  list.replaceChildren();
  list.textContent = "Vérification…";
  let checks = [];
  try {
    checks = await invoke("run_diagnostics");
    render(await invoke("get_state"));
  } catch (e) {
    list.textContent = String(e);
    return;
  }
  // Les problèmes d'abord.
  const order = { error: 0, warn: 1, info: 2, ok: 3 };
  checks.sort((a, b) => order[a.status] - order[b.status]);
  list.textContent = "";
  for (const c of checks) {
    if (c.action === "listen") continue;
    const row = document.createElement("div");
    row.className = "check " + c.status;
    const icon = document.createElement("span");
    icon.className = "check-icon";
    icon.textContent = DIAG_ICONS[c.status] ?? "•";
    const txt = document.createElement("div");
    txt.className = "txt";
    const b = document.createElement("b");
    b.textContent = c.title;
    txt.append(b);
    if (c.detail) {
      const small = document.createElement("small");
      small.textContent = c.detail;
      txt.append(small);
    }
    row.append(icon, txt);
    const fix = diagAction(c.action);
    if (fix) row.append(fix);
    list.append(row);
  }
  const problems = checks.filter((c) => c.status === "error" || c.status === "warn").length;
  $("h2", dlgDiag).textContent = problems ? `🩺 Diagnostic : ${problems} point${problems > 1 ? "s" : ""} à régler` : "🩺 Diagnostic : tout est bon";
}

function diagAction(action) {
  if (!action) return null;
  const b = document.createElement("button");
  b.type = "button";
  if (action === "vbcable") {
    b.textContent = "Installer";
    b.addEventListener("click", () => call("open_vb_cable_page"));
  } else if (action === "wizard") {
    b.textContent = "Assistant";
    b.addEventListener("click", () => {
      dlgDiag.close();
      openWizard();
    });
  } else if (action.startsWith("settings:")) {
    b.textContent = "Régler";
    b.addEventListener("click", () => {
      dlgDiag.close();
      openSettings();
      showSettingsTab(action.slice(9));
    });
  } else {
    return null;
  }
  return b;
}

function openDiagnostics() {
  if (dlgSettings.open) dlgSettings.close();
  dlgDiag.showModal();
  runDiagnostics();
}

$('[data-action="rerun"]', dlgDiag).addEventListener("click", runDiagnostics);
$('[data-action="close"]', dlgDiag).addEventListener("click", () => dlgDiag.close());
$('[data-action="listen"]', dlgDiag).addEventListener("click", () => setListening(!listening));
dlgDiag.addEventListener("close", () => listening && setListening(false));

// ---------- Statistiques ----------

const dlgStats = $("#dlg-stats");

function formatLong(ms) {
  const min = Math.round(ms / 60000);
  if (ms < 60000) return `${Math.round(ms / 1000)} s`;
  if (min < 60) return `${min} min`;
  return `${Math.floor(min / 60)} h ${String(min % 60).padStart(2, "0")}`;
}

function statBars(box, rows, unit) {
  box.replaceChildren();
  const max = Math.max(1, ...rows.map((r) => r.value));
  for (const r of rows) {
    const row = document.createElement("div");
    row.className = "stat-row";
    row.style.setProperty("--pad-color", r.color || "var(--accent)");
    const name = document.createElement("span");
    name.className = "stat-name";
    name.textContent = r.label;
    const bar = document.createElement("span");
    bar.className = "stat-bar";
    const fill = document.createElement("i");
    fill.style.width = (r.value / max) * 100 + "%";
    bar.append(fill);
    const value = document.createElement("span");
    value.className = "stat-value";
    value.textContent = `${r.value} ${unit}`;
    row.append(name, bar, value);
    box.append(row);
  }
  if (!rows.length) box.textContent = "Joue quelques sons pour voir apparaître ton classement.";
}

function renderStats() {
  const sounds = state.config.sounds;
  const total = sounds.reduce((n, s) => n + (s.plays || 0), 0);
  const time = sounds.reduce((n, s) => n + (s.played_ms || 0), 0);
  const top = [...sounds].filter((s) => s.plays).sort((a, b) => b.plays - a.plays);
  const never = sounds.filter((s) => !s.plays);
  const tiles = [
    ["Sons", sounds.length],
    ["Lectures", total],
    ["Temps joué", formatLong(time)],
    ["Préféré", top[0] ? top[0].name : "—"],
  ];
  $("#stats-tiles").replaceChildren(
    ...tiles.map(([label, value]) => {
      const t = document.createElement("div");
      t.className = "stat-tile";
      const v = document.createElement("b");
      v.textContent = value;
      const l = document.createElement("span");
      l.textContent = label;
      t.append(v, l);
      return t;
    })
  );
  statBars($("#stats-top"), top.slice(0, 10).map((s) => ({ label: s.name, value: s.plays, color: s.color })), "×");
  $("#stats-never-count").textContent = never.length ? `(${never.length})` : "";
  const chips = $("#stats-never");
  chips.replaceChildren(
    ...never.map((s) => {
      const c = document.createElement("button");
      c.type = "button";
      c.className = "chip";
      c.textContent = s.name;
      c.title = "Ouvrir la fiche du son (pour l'écouter, le renommer ou le supprimer)";
      c.addEventListener("click", () => {
        dlgStats.close();
        openSoundDialog(s);
      });
      return c;
    })
  );
  if (!never.length) chips.textContent = "Tous tes sons ont été joués au moins une fois. 👏";
  const cats = new Map();
  for (const s of sounds) {
    const k = s.category || "Sans onglet";
    cats.set(k, (cats.get(k) || 0) + (s.plays || 0));
  }
  statBars(
    $("#stats-cats"),
    [...cats].filter(([, v]) => v).sort((a, b) => b[1] - a[1]).map(([label, value]) => ({ label, value })),
    "×"
  );
}

function openStats() {
  renderStats();
  dlgStats.showModal();
}

$('[data-action="close"]', dlgStats).addEventListener("click", () => dlgStats.close());
$('[data-action="reset"]', dlgStats).addEventListener("click", async () => {
  if (!confirm("Remettre toutes les statistiques à zéro ?")) return;
  try {
    render(await invoke("reset_stats"));
    renderStats();
  } catch (e) {
    toast(String(e), true);
  }
});

// ---------- Barre du haut ----------

$("#btn-add").addEventListener("click", () => call("pick_sounds"));
$("#btn-stop").addEventListener("click", () => call("stop_all"));
$("#btn-settings").addEventListener("click", openSettings);
$("#cable-status").addEventListener("click", () => (state.devices.vb_cable ? openDiagnostics() : openWizard()));
$("#btn-stats").addEventListener("click", openStats);
$('#warnings [data-action="diagnose"]').addEventListener("click", openDiagnostics);
$("#search").addEventListener("input", renderGrid);
$("#search").addEventListener("keydown", (e) => {
  if (e.key === "Escape" && e.target.value) {
    e.target.value = "";
    renderGrid();
  }
});
$("#mic-toggle").addEventListener("click", () => call("set_mic_muted", { muted: !state.mic_muted }));
listen("mic-muted", ({ payload }) => {
  state.mic_muted = payload;
  renderMic();
});

$("#btn-random").addEventListener("click", async () => {
  if (!state.config.sounds.some((s) => !tabCategory() || s.category === tabCategory())) {
    return toast("Aucun son dans cet onglet", true);
  }
  const id = await call("play_random", { category: tabCategory() });
  if (id) flash(id);
});

$("#btn-export").addEventListener("click", async () => {
  const n = await call("export_pack", { category: tabCategory() });
  if (n) toast(n > 1 ? `Pack enregistré (${n} sons)` : "Pack enregistré (1 son)");
});

$("#btn-import").addEventListener("click", async () => {
  const res = await call("import_pack");
  if (!res) return;
  render(res.snapshot);
  if (res.errors.length) toast(`${res.added} son(s) ajouté(s). Problèmes :\n${res.errors.join("\n")}`, true);
  else toast(res.added > 1 ? `${res.added} sons ajoutés` : `${res.added} son ajouté`);
});

// ---------- Taille des pads ----------

const sizeInput = $("#pad-size");
function applyPadSize() {
  $("#grid").style.setProperty("--pad-size", sizeInput.value + "px");
}
try {
  sizeInput.value = localStorage.getItem("soundboard.padSize") || 140;
} catch {}
applyPadSize();
sizeInput.addEventListener("input", () => {
  applyPadSize();
  try {
    localStorage.setItem("soundboard.padSize", sizeInput.value);
  } catch {}
});

// ---------- Ajout par lien ----------

const LINK_RE = /^https?:\/\/\S+$/i;

const YOUTUBE_RE = /^https?:\/\/((www|m|music)\.)?(youtube\.com|youtu\.be)\//i;

async function addFromLink(url) {
  if (YOUTUBE_RE.test(url)) return addFromYoutube(url);
  toast("Téléchargement du son…");
  const res = await call("add_from_url", { url });
  if (res) toast("Son ajouté depuis le lien");
  return !!res;
}

// Les vidéos sont longues : on ouvre la découpe juste après l'ajout.
async function addFromYoutube(url) {
  toast("Récupération du son sur YouTube…");
  const res = await call("add_from_youtube", { url });
  if (!res) return false;
  render(res.snapshot);
  toast("Son ajouté : garde le passage qui t'intéresse avec la découpe");
  const sound = state.config.sounds.find((s) => s.id === res.id);
  if (sound) {
    dlgOnline.close();
    openSoundDialog(sound);
  }
  return true;
}
listen("youtube-status", ({ payload }) => toast(payload));

// Ctrl+V d'un lien n'importe où dans la fenêtre (hors champ de texte).
document.addEventListener("paste", (e) => {
  if (e.target.closest?.("input, textarea") || document.querySelector("dialog[open]")) return;
  const text = e.clipboardData?.getData("text")?.trim() ?? "";
  if (LINK_RE.test(text)) {
    e.preventDefault();
    addFromLink(text);
  }
});

$("#link-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  const input = $("#link-form").link;
  const url = input.value.trim();
  if (!LINK_RE.test(url)) return toast("Colle un lien qui commence par http:// ou https://", true);
  const btn = $("#link-form button");
  btn.disabled = true;
  btn.textContent = "…";
  const ok = await addFromLink(url);
  btn.disabled = false;
  btn.textContent = "Ajouter";
  if (ok) input.value = "";
});

// ---------- Nouvelle version ----------

const banner = $("#update-banner");
async function checkUpdate() {
  let info = null;
  try {
    info = await invoke("check_update");
  } catch {
    return; // Pas de connexion : on réessaiera au prochain lancement.
  }
  if (!info) return;
  $("span", banner).textContent = `Une nouvelle version (${info.version}) est disponible. Tes sons et réglages sont gardés.`;
  banner.hidden = false;
  $('[data-action="download"]', banner).onclick = () => call("open_release_page", { url: info.url });
  const install = $('[data-action="install"]', banner);
  install.onclick = async () => {
    install.disabled = true;
    install.textContent = "Mise à jour…";
    $("span", banner).textContent = `Téléchargement de la version ${info.version}, l'app va redémarrer toute seule.`;
    try {
      await invoke("install_update");
    } catch (e) {
      // Version publiée sans mise à jour intégrée, ou pas de connexion.
      install.disabled = false;
      install.textContent = "Mettre à jour";
      $("span", banner).textContent = `Mise à jour impossible : ${e}`;
    }
  };
}
$('[data-action="dismiss"]', banner).addEventListener("click", () => (banner.hidden = true));

// ---------- Fichiers glissés depuis l'Explorateur ----------

const overlay = $("#drop-overlay");
listen("tauri://drag-enter", () => (overlay.hidden = false));
listen("tauri://drag-leave", () => (overlay.hidden = true));
listen("tauri://drag-drop", async (event) => {
  overlay.hidden = true;
  const paths = event.payload?.paths ?? [];
  if (paths.length === 0) return;
  const res = await call("add_sounds", { paths });
  if (res) toast(paths.length > 1 ? `${paths.length} sons ajoutés` : "Son ajouté");
});

// ---------- Événements venant des raccourcis globaux ----------

listen("sound-triggered", (event) => flash(event.payload));
listen("playing", (event) => {
  const before = new Set(playing.map((p) => p.id));
  playing = event.payload ?? [];
  updatePlayhead(playing);
  playing.filter((p) => !before.has(p.id) && p.id !== PREVIEW_ID).forEach((p) => remember(p.id));
  renderPlaying();
});
// Analyse des anciens sons terminée en arrière-plan.
listen("state-changed", async () => render(await invoke("get_state")));
// Une image vient d'être trouvée pour un son.
listen("snapshot", ({ payload }) => render(payload));

// ---------- Recherche en ligne ----------

const dlgOnline = $("#dlg-online");
const onlineForm = $("#online-form");
const results = $("#online-results");
const moreBtn = $("#online-more");
const online = { query: "", page: 1, source: "myinstants", audio: new Audio(), playingBtn: null };
const CREDITS = {
  myinstants: "Sons proposés par myinstants.com",
  freesound: "Sons proposés par freesound.org (vérifie la licence avant de les partager)",
};

try {
  online.source = localStorage.getItem("soundboard.source") || "myinstants";
} catch {}

// Volume des écoutes de la recherche en ligne (ne touche pas aux sons de la soundboard).
const volumeInput = $("#online-volume");
function applyOnlineVolume() {
  online.audio.volume = Number(volumeInput.value);
  $("#online-volume-value").textContent = Math.round(volumeInput.value * 100) + " %";
}
try {
  const saved = localStorage.getItem("soundboard.onlineVolume");
  volumeInput.value = saved !== null ? saved : 0.5;
} catch {
  volumeInput.value = 0.5;
}
applyOnlineVolume();
volumeInput.addEventListener("input", () => {
  applyOnlineVolume();
  try {
    localStorage.setItem("soundboard.onlineVolume", volumeInput.value);
  } catch {}
});

function setSource(source) {
  online.source = source;
  try {
    localStorage.setItem("soundboard.source", source);
  } catch {}
  document.querySelectorAll("#online-source button").forEach((b) => b.classList.toggle("active", b.dataset.source === source));
  $("#online-credit").textContent = CREDITS[source];
  stopOnlinePreview();
  results.replaceChildren();
  moreBtn.hidden = true;
  if (source === "freesound" && !state.config.settings.freesound_key) {
    const msg = document.createElement("div");
    msg.className = "empty-msg";
    msg.append("Pour chercher sur Freesound, il faut une clé gratuite. ");
    const link = document.createElement("a");
    link.href = "#";
    link.textContent = "Comment l'obtenir ?";
    link.addEventListener("click", (e) => {
      e.preventDefault();
      dlgOnline.close();
      openSettings().then(() => formSettings.freesound_key.focus());
    });
    msg.append(link);
    results.append(msg);
    return;
  }
  if (online.query) {
    online.page = 1;
    searchOnline();
  }
}
document.querySelectorAll("#online-source button").forEach((b) => b.addEventListener("click", () => setSource(b.dataset.source)));

$("#btn-online").addEventListener("click", () => {
  setSource(online.source);
  dlgOnline.showModal();
  onlineForm.q.focus();
  onlineForm.q.select();
});
$('[data-action="close"]', dlgOnline).addEventListener("click", () => dlgOnline.close());
dlgOnline.addEventListener("close", stopOnlinePreview);

onlineForm.addEventListener("submit", (e) => {
  e.preventDefault();
  online.query = onlineForm.q.value.trim();
  online.page = 1;
  results.replaceChildren();
  searchOnline();
});
moreBtn.addEventListener("click", () => {
  online.page += 1;
  searchOnline();
});

async function searchOnline() {
  if (!online.query) return;
  moreBtn.hidden = true;
  const loading = document.createElement("div");
  loading.className = "empty-msg";
  loading.textContent = "Recherche…";
  results.append(loading);
  let items = [];
  try {
    items = await invoke("search_online", { source: online.source, query: online.query, page: online.page });
  } catch (e) {
    loading.textContent = String(e);
    return;
  }
  loading.remove();
  if (items.length === 0 && online.page === 1) {
    const msg = document.createElement("div");
    msg.className = "empty-msg";
    msg.textContent = "Aucun son trouvé. Essaie un autre mot, en anglais par exemple.";
    results.append(msg);
  }
  results.append(...items.map(resultRow));
  moreBtn.hidden = items.length === 0;
}

function resultRow(item) {
  const row = document.createElement("div");
  row.className = "row";
  const play = document.createElement("button");
  play.type = "button";
  play.textContent = "▶";
  play.title = "Écouter (dans tes haut-parleurs seulement)";
  const name = document.createElement("span");
  name.className = "name";
  name.textContent = item.name;
  name.title = item.name;
  const add = document.createElement("button");
  add.type = "button";
  add.className = "primary";
  add.textContent = "+ Ajouter";
  row.append(play, name, add);

  play.addEventListener("click", () => toggleOnlinePreview(item, play));
  add.addEventListener("click", async () => {
    add.disabled = true;
    add.textContent = "…";
    const res = await call("add_online", { name: item.name, url: item.url });
    add.textContent = res ? "✓ Ajouté" : "+ Ajouter";
    add.disabled = !!res;
  });
  return row;
}

function toggleOnlinePreview(item, btn) {
  const wasThis = online.playingBtn === btn;
  stopOnlinePreview();
  if (wasThis) return;
  online.audio.src = item.url;
  online.audio.play().catch((e) => toast("Lecture impossible : " + e.message, true));
  online.playingBtn = btn;
  btn.textContent = "■";
  online.audio.onended = stopOnlinePreview;
}

function stopOnlinePreview() {
  online.audio.pause();
  if (online.playingBtn) online.playingBtn.textContent = "▶";
  online.playingBtn = null;
}

// ---------- Démarrage ----------

(async () => {
  render(await invoke("get_state"));
  if (!state.config.settings.setup_done) openWizard();
  migratePlayCounts();
  checkUpdate();
})();
