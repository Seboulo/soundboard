"use strict";

// Overlay en jeu : une petite grille de sons, toujours au-dessus du jeu.
// Il partage le stockage local de la fenêtre principale (thème, onglet).

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const $ = (sel) => document.querySelector(sel);

const FAV = "\u0000fav";
const TAB_KEY = "soundboard.overlayTab";
let sounds = [];
let playing = new Set();
let tab = FAV;

function storageGet(key) {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}

function applyTheme() {
  const theme = storageGet("soundboard.theme") || "auto";
  const dark = theme === "dark" || (theme === "auto" && matchMedia("(prefers-color-scheme: dark)").matches);
  document.documentElement.dataset.theme = dark ? "dark" : "light";
}

async function load() {
  try {
    const snap = await invoke("get_state");
    sounds = snap.config.sounds;
  } catch {
    sounds = [];
  }
  render();
}

function render() {
  const cats = [...new Set(sounds.map((s) => s.category).filter(Boolean))].sort((a, b) => a.localeCompare(b, "fr"));
  const options = [[FAV, "⭐ Favoris"], ["", "Tous les sons"], ...cats.map((c) => [c, c])];
  const select = $("#ov-tab");
  select.replaceChildren(...options.map(([v, l]) => new Option(l, v)));
  if (!options.some(([v]) => v === tab)) tab = FAV;
  select.value = tab;

  let list = sounds.filter((s) => (tab === FAV ? s.favorite : !tab || s.category === tab));
  const empty = $("#ov-empty");
  empty.hidden = list.length > 0;
  empty.textContent =
    tab === FAV ? "Aucun favori : clique sur ☆ sur un son dans la fenêtre principale." : "Aucun son dans cet onglet.";
  $("#ov-grid").replaceChildren(...list.map(pad));
}

function pad(sound) {
  const b = document.createElement("button");
  b.type = "button";
  b.className = "pad" + (playing.has(sound.id) ? " playing" : "");
  b.dataset.id = sound.id;
  b.title = sound.name;
  b.style.setProperty("--pad-color", sound.color);
  const icon = document.createElement("span");
  icon.className = "icon" + (sound.emoji ? " emoji" : "");
  icon.textContent = sound.emoji || [...sound.name.trim()][0]?.toUpperCase() || "♪";
  const name = document.createElement("span");
  name.className = "name";
  name.textContent = sound.name;
  b.append(icon, name);
  b.addEventListener("click", () => invoke("play_sound", { id: sound.id }).catch(() => {}));
  return b;
}

$("#ov-tab").addEventListener("change", (e) => {
  tab = e.target.value;
  try {
    localStorage.setItem(TAB_KEY, tab);
  } catch {}
  render();
});
$("#ov-stop").addEventListener("click", () => invoke("stop_all"));
$("#ov-close").addEventListener("click", () => invoke("toggle_overlay"));

listen("playing", ({ payload }) => {
  playing = new Set((payload ?? []).map((p) => p.id));
  document.querySelectorAll(".pad").forEach((p) => p.classList.toggle("playing", playing.has(p.dataset.id)));
});
// La fenêtre principale prévient quand la liste des sons change.
listen("sounds-changed", load);
window.addEventListener("storage", (e) => e.key === "soundboard.theme" && applyTheme());
matchMedia("(prefers-color-scheme: dark)").addEventListener("change", applyTheme);

tab = storageGet(TAB_KEY) ?? FAV;
applyTheme();
load();
