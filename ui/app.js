"use strict";

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $ = (sel, root = document) => root.querySelector(sel);
const $$ = (sel, root = document) => [...root.querySelectorAll(sel)];

let state = null; // { config, devices, warnings, vb_cable_url }

// Les erreurs de l'interface vont dans le journal (« Copier le rapport »).
const logUi = (msg) => invoke("log_ui", { message: String(msg) }).catch(() => {});
window.addEventListener("error", (e) => logUi(`${e.message} (${e.filename?.split("/").pop()}:${e.lineno})`));
window.addEventListener("unhandledrejection", (e) => logUi(e.reason?.stack || e.reason));

// ---------- Utilitaires ----------

/** Message en bas de l'écran, avec un bouton facultatif ({ label, run }). */
function toast(message, isError = false, action = null) {
  const el = $("#toast");
  el.textContent = message;
  if (action) {
    const btn = document.createElement("button");
    btn.type = "button";
    btn.textContent = action.label;
    btn.addEventListener("click", () => {
      el.hidden = true;
      action.run();
    });
    el.append(btn);
  }
  el.classList.toggle("error", isError);
  el.hidden = false;
  clearTimeout(toast.timer);
  toast.timer = setTimeout(() => (el.hidden = true), action ? 8000 : isError ? 6000 : 2500);
}

// Un son ajouté pendant qu'un onglet est ouvert y est rangé directement.
const ADD_CMDS = new Set(["pick_sounds", "add_sounds", "add_online", "add_from_url", "add_from_youtube"]);

async function call(cmd, args) {
  try {
    const tab = ADD_CMDS.has(cmd) ? tabCategory() : null;
    const before = tab && new Set(state.config.sounds.map((s) => s.id));
    let res = await invoke(cmd, args);
    if (tab && res) {
      const snap = res.config ? res : res.snapshot;
      const ids = (snap?.config.sounds ?? []).filter((s) => !before.has(s.id) && !s.category).map((s) => s.id);
      if (ids.length) {
        const placed = await invoke("set_category", { ids, category: tab });
        if (res.config) res = placed;
        else res.snapshot = placed;
      }
    }
    if (res && res.config) render(res);
    return res;
  } catch (e) {
    toast(String(e), true);
    logUi(`${cmd} : ${e}`);
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

const TYPING_KEY = /^(Key|Digit)|^(Space|Enter|Backspace|Backquote|Minus|Equal|BracketLeft|BracketRight|Backslash|Semicolon|Quote|Comma|Period|Slash|IntlBackslash)$/;

const TYPING_MSG = "Ajoute Ctrl ou Alt : une touche d'écriture seule serait bloquée dans toutes tes applis (pavé numérique et F1 à F12 OK).";
const SETTING_LABELS = { stop_hotkey: "Tout arrêter", mute_hotkey: "Couper le micro", random_hotkey: "Son au hasard", replay_hotkey: "Rejouer 30 s" };

function blocksTyping(e, quiet = false) {
  if (e.ctrlKey || e.altKey || e.metaKey || !TYPING_KEY.test(e.code)) return false;
  if (!quiet) toast(TYPING_MSG, true);
  return true;
}

/** Qui utilise déjà ce raccourci (hors `self` : { soundId } ou { setting }) ? */
function hotkeyOwner(hk, self = {}) {
  const sound = state.config.sounds.find((s) => s.hotkey === hk && s.id !== self.soundId);
  if (sound) return `le son « ${sound.name} »`;
  for (const k of Object.keys(SETTING_LABELS)) {
    if (k === self.setting) continue;
    // Dans les réglages ouverts, on compare à ce qui est tapé (pas encore enregistré).
    const field = formSettings?.[k];
    const value = field?.isConnected && dlgSettings.open ? field.dataset.hotkey : state.config.settings[k];
    if (value === hk) return `« ${SETTING_LABELS[k]} »`;
  }
  return null;
}

/** Message affiché juste sous le champ du raccourci (vide = on l'enlève). */
function hotkeyMessage(input, text) {
  const row = input.closest("label") ?? input;
  let msg = row.nextElementSibling?.classList.contains("hotkey-msg") ? row.nextElementSibling : null;
  if (!text) return msg?.remove();
  if (!msg) {
    msg = document.createElement("p");
    msg.className = "hotkey-msg";
    msg.setAttribute("role", "alert");
    row.after(msg);
  }
  msg.textContent = text;
}

function setupHotkeyInput(input, self = () => ({})) {
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
      hotkeyMessage(input, "");
      return;
    }
    if (noMods && e.key === "Escape") {
      input.blur();
      return;
    }
    const hk = hotkeyFromEvent(e);
    if (!hk) return;
    // Un raccourci global « avale » la touche dans toutes les applis : une
    // lettre seule (ou avec Maj) empêcherait d'écrire dans Discord.
    if (blocksTyping(e, true)) return hotkeyMessage(input, TYPING_MSG);
    input.dataset.hotkey = hk;
    input.value = hotkeyLabel(hk);
    const owner = hotkeyOwner(hk, self());
    hotkeyMessage(input, owner ? `Déjà utilisé par ${owner} : l'un des deux ne marchera pas.` : "");
  });
}

function setHotkeyInput(input, hotkey) {
  input.dataset.hotkey = hotkey ?? "";
  input.value = hotkeyLabel(hotkey);
  hotkeyMessage(input, "");
}

function bindRangeLabels(form) {
  form.querySelectorAll("input[type=range]").forEach((r) => {
    const label = form.querySelector(`.value[data-for="${r.name}"]`);
    // Au-delà du repère 100 % (list="tick-100"), le son est amplifié : on le dit.
    const boost = r.getAttribute("list") === "tick-100";
    const update = () =>
      label && (label.textContent = Math.round(r.value * 100) + " %" + (boost && r.value > 1 ? " · amplifié" : ""));
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
  renderSurprise();
  renderRoom();
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
  // Neutre quand tout va bien, rouge seulement quand le micro est coupé.
  pill.className = "pill" + (muted ? " off" : "");
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

/** Onglets dans l'ordre choisi (glisser-déposer), y compris ceux encore vides ;
 *  ceux qui n'ont pas encore de place viennent après, par ordre alphabétique. */
function categories() {
  const mine = (state.config.categories || []).filter(Boolean);
  const used = new Set(state.config.sounds.map((s) => s.category).filter(Boolean));
  const extra = [...used].filter((c) => !mine.includes(c)).sort((a, b) => a.localeCompare(b, "fr"));
  return [...mine, ...extra];
}

// Onglet spécial des derniers sons joués (aucun nom d'onglet ne commence par \u0000).
const RECENT_TAB = "\u0000recent";
// Onglet spécial des sons les plus joués.
const TOP_TAB = "\u0000top";
const TOP_COUNT = 20;
const isSpecialTab = (tab) => tab === RECENT_TAB || tab === TOP_TAB;
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
  return isSpecialTab(currentTab) ? null : currentTab;
}

function renderTabs() {
  const cats = categories();
  if (currentTab && !isSpecialTab(currentTab) && !cats.includes(currentTab)) currentTab = null;
  const tab = (label, value, iconName) => {
    const b = document.createElement("button");
    b.type = "button";
    b.className = "tab" + (currentTab === value ? " active" : "");
    if (iconName) b.append(svgIcon(iconName));
    b.append(label);
    // Un son glissé sur l'onglet y est rangé.
    if (value && !isSpecialTab(value)) {
      b.dataset.cat = value;
      b.title = "Glisse pour changer l'ordre · clic droit : renommer, supprimer";
      b.addEventListener("pointerdown", (e) => startTabDrag(e, b, value));
    }
    b.addEventListener("click", () => {
      if (reorder.justDropped) return;
      currentTab = value;
      try {
        if (value) localStorage.setItem(TAB_KEY, value);
        else localStorage.removeItem(TAB_KEY);
      } catch {}
      renderTabs();
      renderGrid();
      // La barre des onglets reste visible : on repart du haut du nouvel onglet.
      window.scrollTo({ top: 0 });
    });
    return b;
  };
  // Sans aucun son, les outils de tri et d'affichage ne servent à rien : on les cache.
  const empty = state.config.sounds.length === 0;
  document.body.classList.toggle("no-sounds", empty);
  const special = empty ? [] : [tab("Tous", null), tab("Récents", RECENT_TAB, "clock"), tab("Top", TOP_TAB, "chart")];
  // Nom court pour garder les onglets sur une ligne ; le nom complet en info-bulle.
  if (special.length) special[2].title = "Les plus utilisés : tes 20 sons les plus joués";
  $("#tabs").replaceChildren(...special, ...cats.map((c) => tab(c, c)));
  // « Récents » et « Les plus utilisés » ont leur propre ordre : le tri n'y sert à rien.
  $("#sort").hidden = currentTab === RECENT_TAB || currentTab === TOP_TAB;
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

/** Bouton adapté à chaque problème : on va droit à ce qui le corrige. */
function warningAction(w) {
  const settings = (tab) => ({ label: "Régler", run: () => openSettings().then(() => showSettingsTab(tab)) });
  if (w.startsWith("Audio : ")) {
    if (/câble/i.test(w) && !state.devices.vb_cable) return { label: "Installer VB-Cable", run: openWizard };
    return settings(/micro/i.test(w) ? "mic" : "sound");
  }
  if (w.startsWith("Raccourci : ")) return settings("keys");
  if (w.startsWith("Téléphone : ")) return settings("remote");
  if (w.startsWith("Démarrage : ")) return settings("general");
  if (w.startsWith("Réglages : ")) return null;
  return { label: "Diagnostic", run: openDiagnostics };
}

// Problèmes masqués par l'utilisateur (reviennent s'ils changent).
const dismissedWarnings = new Set();

function renderWarnings() {
  const el = $("#warnings");
  const list = state.warnings.filter((w) => !dismissedWarnings.has(w));
  el.replaceChildren(
    ...list.map((w) => {
      const row = document.createElement("div");
      row.className = "warning";
      const text = document.createElement("span");
      text.textContent = w;
      row.append(text);
      const action = warningAction(w);
      if (action) {
        const fix = document.createElement("button");
        fix.type = "button";
        fix.textContent = action.label;
        fix.addEventListener("click", action.run);
        row.append(fix);
      }
      const close = document.createElement("button");
      close.type = "button";
      close.className = "ghost";
      close.title = "Masquer";
      close.textContent = "✕";
      close.addEventListener("click", () => {
        dismissedWarnings.add(w);
        renderWarnings();
      });
      row.append(close);
      return row;
    }),
  );
  el.hidden = list.length === 0;
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
  if (currentTab === TOP_TAB) {
    sounds = state.config.sounds
      .filter((s) => s.plays > 0)
      .sort((a, b) => b.plays - a.plays)
      .slice(0, TOP_COUNT)
      .filter(matches);
  } else if (currentTab === RECENT_TAB) {
    const byId = new Map(state.config.sounds.map((s) => [s.id, s]));
    sounds = recent.map((id) => byId.get(id)).filter((s) => s && matches(s));
  } else {
    sounds = sortSounds(state.config.sounds.filter((s) => (!currentTab || s.category === currentTab) && matches(s)));
  }
  // Oubli des sons sélectionnés qui n'existent plus.
  for (const id of selection) if (!soundById(id)) selection.delete(id);
  const pads = [];
  // En liste, une ligne d'en-tête comme une playlist.
  if (viewMode === "list" && sounds.length) pads.push(listHeader());
  const groups = !currentTab && categories().length ? soundGroups(sounds, !q) : null;
  if (groups) {
    let n = 0;
    for (const g of groups) {
      // Une recherche montre tout, même les groupes repliés.
      const open = !!q || !closedGroups.has(g.key);
      pads.push(groupHeader(g, open));
      if (open && !g.sounds.length) pads.push(emptyGroup(g.key));
      if (open) pads.push(...g.sounds.map((s) => padElement(s, n++)));
      else n += g.sounds.length;
    }
  } else {
    pads.push(...sounds.map(padElement));
  }
  grid.replaceChildren(...pads);
  // Entrée dans la recherche joue ce son : on le montre.
  if (q) $("#grid .pad:not(.missing)")?.classList.add("first-hit");
  renderSelectionBar();
  $("#empty").hidden = state.config.sounds.length > 0;
  renderStartSteps();
  $("#top-empty").hidden = !(currentTab === TOP_TAB && sounds.length === 0 && state.config.sounds.length > 0 && !q);
  $("#tab-empty").hidden = !(currentTab && !isSpecialTab(currentTab) && sounds.length === 0 && !q);
  $("#recent-empty").hidden = !(currentTab === RECENT_TAB && sounds.length === 0 && state.config.sounds.length > 0 && !q);
  $("#search-empty").hidden = !(q && sounds.length === 0 && state.config.sounds.length > 0);
  $('#search-empty [data-action="search-online"] span').textContent = `Chercher « ${$("#search").value.trim()} » en ligne`;
  renderPlaying();
}

// ---------- Groupes de « Tous » ----------

// Dans « Tous » : les favoris en haut, puis un groupe repliable par onglet.
const FAV_GROUP = "\u0000fav";
const NO_TAB_GROUP = "\u0000none";
const GROUPS_KEY = "soundboard.closedGroups";
let closedGroups = new Set();
try {
  closedGroups = new Set(JSON.parse(localStorage.getItem(GROUPS_KEY) || "[]"));
} catch {}

function soundGroups(sounds, showEmpty) {
  const groups = [];
  const add = (key, label, list, keepEmpty) => (list.length || keepEmpty) && groups.push({ key, label, sounds: list });
  add(FAV_GROUP, "Favoris", sounds.filter((s) => s.favorite));
  const rest = sounds.filter((s) => !s.favorite);
  // Un onglet tout juste créé reste visible pour qu'on puisse y glisser des sons.
  for (const c of categories()) add(c, c, rest.filter((s) => s.category === c), showEmpty);
  add(NO_TAB_GROUP, "Sans onglet", rest.filter((s) => !s.category));
  return groups;
}

function groupHeader(group, open) {
  const head = document.createElement("button");
  head.type = "button";
  head.className = "group-head" + (open ? "" : " closed");
  head.setAttribute("aria-expanded", open);
  // Glisser un son sur le titre le range dans cet onglet.
  if (group.key === NO_TAB_GROUP) head.dataset.cat = "";
  else if (group.key !== FAV_GROUP) head.dataset.cat = group.key;
  const chev = document.createElement("span");
  chev.className = "chev";
  chev.textContent = "▾";
  const name = document.createElement("span");
  name.className = "group-name";
  if (group.key === FAV_GROUP) name.append(svgIcon("star"));
  name.append(group.label);
  const count = document.createElement("span");
  count.className = "group-count";
  count.textContent = group.sounds.length;
  head.append(chev, name, count);
  head.title = open ? "Replier" : "Déplier";
  if (head.dataset.cat) {
    head.title += " · glisser pour changer l'ordre des onglets · clic droit : renommer, supprimer";
    head.addEventListener("pointerdown", (e) => startTabDrag(e, head, group.key));
  }
  head.addEventListener("click", () => {
    if (reorder.justDropped) return;
    if (closedGroups.has(group.key)) closedGroups.delete(group.key);
    else closedGroups.add(group.key);
    try {
      localStorage.setItem(GROUPS_KEY, JSON.stringify([...closedGroups]));
    } catch {}
    renderGrid();
  });
  return head;
}

/** Onglet vide dans « Tous » : une zone où déposer des sons. */
function emptyGroup(cat) {
  const zone = document.createElement("div");
  zone.className = "group-empty";
  zone.dataset.cat = cat;
  zone.textContent = "Onglet vide : glisse des sons ici";
  return zone;
}

/** Glisser un onglet (ou un titre de groupe) sur un autre change leur ordre. */
function startTabDrag(e, el, name) {
  if (e.button !== 0) return;
  const x0 = e.clientX;
  const y0 = e.clientY;
  let dragging = false;
  let target = null;
  const move = (ev) => {
    if (!dragging && Math.hypot(ev.clientX - x0, ev.clientY - y0) < 8) return;
    dragging = true;
    el.classList.add("dragging");
    let over = document.elementFromPoint(ev.clientX, ev.clientY)?.closest(".tab[data-cat], .group-head[data-cat]");
    if (!over?.dataset.cat || over.dataset.cat === name) over = null;
    if (target && target !== over) target.classList.remove("drag-over");
    target = over;
    target?.classList.add("drag-over");
  };
  const up = () => {
    window.removeEventListener("pointermove", move);
    window.removeEventListener("pointerup", up);
    el.classList.remove("dragging");
    target?.classList.remove("drag-over");
    if (!dragging) return;
    // Le clic qui suit le relâchement ne doit ni ouvrir ni replier.
    reorder.justDropped = true;
    setTimeout(() => (reorder.justDropped = false), 0);
    if (!target) return;
    const order = categories();
    const to = order.indexOf(target.dataset.cat);
    order.splice(order.indexOf(name), 1);
    // Vers la droite : il se pose après la cible ; vers la gauche : avant.
    order.splice(to, 0, name);
    call("set_tabs", { names: order });
  };
  window.addEventListener("pointermove", move);
  window.addEventListener("pointerup", up);
}

// ---------- Créer, renommer, supprimer un onglet ----------

const dlgTab = $("#dlg-tab");
const formTab = $("form", dlgTab);
let tabEdit = null; // { mode: "new" | "rename" | "delete", name }
enterSaves(dlgTab);

function openTabDialog(mode, name = "") {
  tabEdit = { mode, name };
  const n = state.config.sounds.filter((s) => s.category === name).length;
  const pl = n > 1;
  $("h2", dlgTab).textContent = { new: "Nouvel onglet", rename: `Renommer « ${name} »`, delete: `Supprimer « ${name} » ?` }[mode];
  $("#tab-hint").textContent =
    mode === "delete"
      ? n
        ? `${pl ? `Ses ${n} sons ne sont` : "Son son n'est"} pas supprimé${pl ? "s" : ""} : ${pl ? "ils passent" : "il passe"} dans « Sans onglet ».`
        : "Il est vide : aucun son n'est touché."
      : "Range ensuite tes sons dedans en les glissant sur son nom.";
  formTab.tab_name.closest("label").hidden = mode === "delete";
  formTab.tab_name.value = mode === "rename" ? name : "";
  const ok = $('button[value="save"]', dlgTab);
  ok.textContent = { new: "Créer", rename: "Renommer", delete: "Supprimer" }[mode];
  ok.classList.toggle("danger", mode === "delete");
  dlgTab.returnValue = "";
  dlgTab.showModal();
  if (mode === "delete") ok.focus();
  else formTab.tab_name.select();
}
$('[data-action="cancel"]', dlgTab).addEventListener("click", () => dlgTab.close());
dlgTab.addEventListener("close", async () => {
  if (dlgTab.returnValue !== "save" || !tabEdit) return;
  const { mode, name } = tabEdit;
  tabEdit = null;
  const value = formTab.tab_name.value.trim().slice(0, 30);
  const cats = categories();
  if (mode === "new") {
    if (!value) return;
    if (cats.includes(value)) return toast(`L'onglet « ${value} » existe déjà`, true);
    if (await call("set_tabs", { names: [...cats, value] })) toast(`Onglet « ${value} » créé : glisse des sons dessus pour les y ranger`);
  } else if (mode === "rename") {
    if (!value || value === name) return;
    if (currentTab === name) {
      currentTab = value;
      try {
        localStorage.setItem(TAB_KEY, value);
      } catch {}
    }
    // Un groupe replié le reste sous son nouveau nom.
    if (closedGroups.delete(name)) {
      closedGroups.add(value);
      try {
        localStorage.setItem(GROUPS_KEY, JSON.stringify([...closedGroups]));
      } catch {}
    }
    const merged = cats.includes(value);
    if (await call("rename_tab", { old: name, new: value })) {
      toast(merged ? `« ${name} » regroupé avec « ${value} »` : `Onglet renommé en « ${value} »`);
    }
  } else if (await call("rename_tab", { old: name, new: null })) {
    toast(`Onglet « ${name} » supprimé`);
  }
});

// Clic droit sur le fond de la grille ou sur la barre des onglets.
const ctxMenu = $("#ctx-menu");
const closeCtxMenu = () => (ctxMenu.hidden = true);
document.addEventListener("contextmenu", (e) => {
  if (e.target.closest(".pad, dialog, input, textarea, .menu") || !e.target.closest("main, .tabbar")) return;
  e.preventDefault();
  const cat = e.target.closest("[data-cat]")?.dataset.cat || null;
  const item = (icon, label, run, danger) => {
    const b = document.createElement("button");
    b.type = "button";
    b.setAttribute("role", "menuitem");
    if (danger) b.className = "danger";
    b.append(svgIcon(icon), label);
    b.addEventListener("click", () => {
      closeCtxMenu();
      run();
    });
    return b;
  };
  const items = [item("plus", "Nouvel onglet", () => openTabDialog("new"))];
  if (cat) {
    items.push(
      item("pencil", `Renommer « ${cat} »`, () => openTabDialog("rename", cat)),
      item("trash", `Supprimer « ${cat} »`, () => openTabDialog("delete", cat), true),
    );
  }
  ctxMenu.replaceChildren(...items);
  ctxMenu.hidden = false;
  // Le menu reste dans la fenêtre.
  const { width, height } = ctxMenu.getBoundingClientRect();
  ctxMenu.style.left = Math.max(8, Math.min(e.clientX, innerWidth - width - 8)) + "px";
  ctxMenu.style.top = Math.max(8, Math.min(e.clientY, innerHeight - height - 8)) + "px";
  $("button", ctxMenu).focus();
});
document.addEventListener("pointerdown", (e) => {
  if (!ctxMenu.hidden && !ctxMenu.contains(e.target)) closeCtxMenu();
});
window.addEventListener("blur", closeCtxMenu);
window.addEventListener("scroll", closeCtxMenu, true);
ctxMenu.addEventListener("keydown", (e) => {
  const items = $$("button", ctxMenu);
  const i = items.indexOf(document.activeElement);
  if (e.key === "Escape") {
    e.stopPropagation();
    closeCtxMenu();
  } else if (e.key === "ArrowDown" || e.key === "ArrowUp") {
    e.preventDefault();
    items[(i + (e.key === "ArrowDown" ? 1 : items.length - 1)) % items.length].focus();
  }
});

// ---------- Sélection de plusieurs sons ----------

// Ctrl + clic (ou le bouton « Sélectionner ») coche des sons, qu'on range
// ensuite d'un coup dans un onglet.
const selection = new Set();
let selecting = false;

function toggleSelected(id) {
  if (selection.has(id)) selection.delete(id);
  else selection.add(id);
  document.querySelector(`#grid .pad[data-id="${CSS.escape(id)}"]`)?.classList.toggle("selected", selection.has(id));
  renderSelectionBar();
}

function clearSelection() {
  selection.clear();
  selecting = false;
  $$("#grid .pad.selected").forEach((p) => p.classList.remove("selected"));
  renderSelectionBar();
}

function renderSelectionBar() {
  const bar = $("#selection-bar");
  bar.hidden = !selecting && selection.size === 0;
  $("#btn-select").classList.toggle("on", selecting || selection.size > 0);
  $("#grid").classList.toggle("selecting", selecting);
  const n = selection.size;
  $("#selection-count").textContent = n
    ? `${n} son${n > 1 ? "s" : ""} sélectionné${n > 1 ? "s" : ""}`
    : "Clique sur les sons à ranger";
  $("#selection-form").hidden = n === 0;
}

/** Range des sons dans un onglet (vide : aucun onglet). */
async function moveToCategory(ids, category) {
  const name = category?.trim() || null;
  const res = await call("set_category", { ids, category: name });
  if (!res) return;
  const what = ids.length > 1 ? `${ids.length} sons rangés` : `« ${soundById(ids[0])?.name ?? "Son"} » rangé`;
  toast(name ? `${what} dans « ${name} »` : `${what} sans onglet`);
}

$("#btn-select").addEventListener("click", () => {
  if (selecting || selection.size) clearSelection();
  else {
    selecting = true;
    renderSelectionBar();
  }
});
$("#selection-cancel").addEventListener("click", clearSelection);
$("#selection-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  const input = $("#selection-target");
  if (!selection.size) return;
  const ids = [...selection];
  clearSelection();
  await moveToCategory(ids, input.value);
  input.value = "";
});

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
    const animate = p && !reducedMotion();
    const src = sound?.image && fileSrc(animate ? sound.image : sound.image_still || sound.image);
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
  const ms = Math.max(0, Math.min(sound.end_ms ?? total, total) - sound.start_ms);
  return ms / (sound.fx?.speed ?? 1);
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
  return !isSpecialTab(currentTab) && (sortMode === "manual" || sortMode === "favorites");
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
  for (const [cls, text] of [["num", "#"], ["title", "Titre"], ["key", "Raccourci"], ["dur", "Durée"], ["tools", ""]]) {
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
  pad.className = "pad" + (selection.has(sound.id) ? " selected" : "");
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
    icon.className = "icon" + (emoji ? " emoji" : " note");
    // Sans emoji : une note dans la couleur du son (des initiales se
    // répétaient : « S », « S », « S »…).
    if (emoji) icon.textContent = emoji;
    else icon.append(svgIcon("note"));
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
  // Pas d'onglet répété quand l'onglet ouvert ou le groupe le dit déjà.
  const tabShown = (currentTab && currentTab !== RECENT_TAB && currentTab !== TOP_TAB) || (!currentTab && !sound.favorite && categories().length);
  meta.textContent = tabShown ? "" : sound.category || "";
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
  let addKey = null;
  if (sound.hotkey) {
    const kbd = document.createElement("kbd");
    kbd.textContent = hotkeyLabel(sound.hotkey);
    key.append(kbd);
  } else {
    // Visible au survol : poser un raccourci sans ouvrir la fiche.
    addKey = document.createElement("span");
    addKey.className = "add-key";
    addKey.textContent = "+ Raccourci";
    addKey.title = "Choisir une touche pour jouer ce son de n'importe où";
    key.append(addKey);
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

  const missing = state.missing?.includes(sound.id);
  if (missing) {
    pad.classList.add("missing");
    const warn = document.createElement("span");
    warn.className = "missing-tag";
    warn.textContent = "Fichier manquant";
    key.replaceChildren(warn);
  }
  const keyHint = sound.hotkey ? `, raccourci ${hotkeyLabel(sound.hotkey)}` : "";
  pad.setAttribute("aria-label", `${sound.name}${missing ? ", fichier manquant" : ""}${keyHint}`);
  pad.title = missing ? "Le fichier de ce son a disparu : clique pour en choisir un autre" : "";

  pad.addEventListener("click", (e) => {
    if (reorder.justDropped) return;
    if (selecting || e.ctrlKey || e.metaKey) return toggleSelected(sound.id);
    if (edit.contains(e.target)) return openSoundDialog(sound);
    if (fav.contains(e.target)) return call("toggle_favorite", { id: sound.id });
    if (addKey?.contains(e.target)) return captureHotkey(sound, addKey);
    if (missing) return replaceSoundFile(sound);
    flash(sound.id);
    playSound(sound.id); // joue, ou arrête s'il est déjà en cours
  });
  pad.addEventListener("contextmenu", (e) => {
    e.preventDefault();
    openSoundDialog(sound);
  });

  pad.addEventListener("pointerdown", (e) => startReorder(e, pad, sound));
  return pad;
}

/** « + Raccourci » : la prochaine touche appuyée devient le raccourci du son. */
function captureHotkey(sound, chip) {
  chip.classList.add("listening");
  chip.textContent = "Appuie sur une touche… (Échap : annuler)";
  const done = () => {
    window.removeEventListener("keydown", onKey, true);
    window.removeEventListener("pointerdown", cancel, true);
    chip.classList.remove("listening");
    chip.textContent = "+ Raccourci";
  };
  const cancel = (e) => {
    if (e.target !== chip) done();
  };
  const onKey = (e) => {
    e.preventDefault();
    e.stopPropagation();
    if (e.key === "Escape") return done();
    const hk = hotkeyFromEvent(e);
    if (!hk) return; // touche de modification seule : on attend la suite
    if (blocksTyping(e)) return; // on attend une autre combinaison
    done();
    const owner = hotkeyOwner(hk, { soundId: sound.id });
    if (owner) return toast(`${hotkeyLabel(hk)} est déjà utilisé par ${owner}`, true);
    call("update_sound", { sound: { ...sound, hotkey: hk } }).then(
      (res) => res && toast(`« ${sound.name} » : ${hotkeyLabel(hk)}`),
    );
  };
  window.addEventListener("keydown", onKey, true);
  window.addEventListener("pointerdown", cancel, true);
}

/** Joue un son ; si ses effets mettent du temps à se calculer, le pad
 *  affiche « préparation… » pour montrer que le clic a bien été pris. */
async function playSound(id) {
  const padOf = () => document.querySelector(`#grid .pad[data-id="${CSS.escape(id)}"]`);
  const timer = setTimeout(() => padOf()?.classList.add("preparing"), 150);
  try {
    await call("play_sound", { id });
  } finally {
    clearTimeout(timer);
    padOf()?.classList.remove("preparing");
  }
}

async function replaceSoundFile(sound) {
  const res = await call("replace_sound_file", { id: sound.id });
  if (res) toast(`« ${sound.name} » a un nouveau fichier`);
}

// Réorganisation à la souris : on glisse un pad sur un autre. Le glisser-déposer
// HTML5 n'est pas utilisable sous Windows tant que le dépôt de fichiers est actif.
const reorder = { justDropped: false };

function startReorder(e, pad, sound) {
  if (e.button !== 0 || e.ctrlKey || e.metaKey) return;
  const x0 = e.clientX;
  const y0 = e.clientY;
  let dragging = false;
  let target = null;

  const move = (ev) => {
    if (!dragging && Math.hypot(ev.clientX - x0, ev.clientY - y0) < 8) return;
    dragging = true;
    pad.style.opacity = "0.5";
    // Sur un pad : changer l'ordre. Sur un onglet ou un titre de groupe : y ranger le son.
    let over = document.elementFromPoint(ev.clientX, ev.clientY)?.closest("[data-cat], #grid .pad");
    if (over?.classList.contains("pad") && (over === pad || !canReorder())) over = null;
    if (target && target !== over) target.classList.remove("drag-over");
    target = over;
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
    if (target?.dataset.cat != null) {
      // Un son sélectionné emmène toute la sélection.
      const ids = selection.has(sound.id) ? [...selection] : [sound.id];
      if (selection.has(sound.id)) clearSelection();
      moveToCategory(ids, target.dataset.cat);
    } else if (target) {
      const other = soundById(target.dataset.id);
      const to = state.config.sounds.findIndex((s) => s.id === target.dataset.id);
      call("move_sound", { id: sound.id, to }).then((res) => {
        // Dans « Tous », déposé dans le groupe d'un autre onglet : il y passe.
        if (res && !currentTab && other && !other.favorite && !sound.favorite && other.category !== sound.category) {
          moveToCategory([sound.id], other.category ?? "");
        }
      });
    }
  };
  window.addEventListener("pointermove", move);
  window.addEventListener("pointerup", up);
}

function flash(id) {
  if (reducedMotion()) return;
  const pad = document.querySelector(`.pad[data-id="${CSS.escape(id)}"]`);
  if (!pad) return;
  pad.classList.add("flash");
  setTimeout(() => pad.classList.remove("flash"), 250);
}

// ---------- Dialogue « Modifier le son » ----------

const dlgSound = $("#dlg-sound");
const formSound = $("form", dlgSound);
let editing = null;
setupHotkeyInput(formSound.hotkey, () => ({ soundId: editing?.id }));
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
  setFx(sound.fx);
  // Effets repliés tant qu'il n'y en a pas : la fenêtre reste courte.
  fxBox.open = $("#fx-state").classList.contains("on");
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
// view : partie de l'onde affichée (zoom) ; focus : poignée réglée au clavier.
const trim = { duration: 0, start: 0, end: 0, peaks: [], drag: null, view: [0, 0], focus: "start" };
const MAX_ZOOM = 32;

function fmtTime(ms) {
  const s = ms / 1000;
  return s < 60 ? s.toFixed(2) + " s" : `${Math.floor(s / 60)} min ${(s % 60).toFixed(1).padStart(4, "0")} s`;
}

async function openTrim(sound) {
  const box = $(".trim", dlgSound);
  trim.duration = sound.analysis?.duration_ms ?? 0;
  box.hidden = !trim.duration;
  if (!trim.duration) return resetHistory();
  trim.start = sound.start_ms ?? 0;
  trim.end = Math.min(sound.end_ms ?? trim.duration, trim.duration);
  trim.peaks = [];
  trim.view = [0, trim.duration];
  trim.focus = "start";
  resetHistory();
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
  const [v0, v1] = trim.view;
  const x = (ms) => ((ms - v0) / (v1 - v0)) * w;
  const xs = x(trim.start);
  const xe = x(trim.end);

  // Forme d'onde : une barre par pic visible, plus sombre hors de la zone gardée.
  const n = trim.peaks.length;
  const i0 = Math.max(0, Math.floor((v0 / trim.duration) * n));
  const i1 = Math.min(n, Math.ceil((v1 / trim.duration) * n));
  const bw = Math.max(1, w / Math.max(1, i1 - i0) - 1);
  for (let i = i0; i < i1; i++) {
    const px = x((i / n) * trim.duration);
    const bh = Math.max(2, trim.peaks[i] * (h - 10));
    const inside = px >= xs && px <= xe;
    ctx.fillStyle = inside ? editing?.color ?? "#4f46e5" : css.getPropertyValue("--wave-off");
    ctx.fillRect(px, (h - bh) / 2, bw, bh);
  }
  if (!n) {
    ctx.fillStyle = css.getPropertyValue("--muted");
    ctx.font = "26px Segoe UI, sans-serif";
    ctx.fillText("Chargement de la forme d'onde…", 20, h / 2 + 8);
  }
  ctx.fillStyle = css.getPropertyValue("--wave-mask");
  ctx.fillRect(0, 0, xs, h);
  ctx.fillRect(xe, 0, w - xe, h);
  // La poignée réglée aux flèches est en couleur quand l'onde a le focus.
  const keyboard = document.activeElement === canvas;
  for (const [hx, which] of [[xs, "start"], [xe, "end"]]) {
    ctx.fillStyle = css.getPropertyValue(keyboard && trim.focus === which ? "--accent" : "--text");
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
  const zoom = trim.duration / (v1 - v0);
  $("#trim-info").textContent =
    (now !== null ? `▶ ${fmtTime(now)}  ·  ` : "") +
    `${fmtTime(trim.start)} → ${fmtTime(trim.end)}  ·  ${fmtTime(trim.end - trim.start)}` +
    (zoom > 1.01 ? `  ·  zoom × ${Math.round(zoom)}` : "");
  zoomOutBtn.disabled = zoom <= 1.01;
  zoomInBtn.disabled = zoom >= MAX_ZOOM - 0.01;
}

// ---------- Zoom sur l'onde ----------

const zoomInBtn = $('[data-action="zoom-in"]', dlgSound);
const zoomOutBtn = $('[data-action="zoom-out"]', dlgSound);

/** Affiche `span` ms autour de `center` (en restant dans le son). */
function setView(center, span) {
  span = Math.min(trim.duration, Math.max(trim.duration / MAX_ZOOM, span));
  let from = Math.max(0, center - span / 2);
  from = Math.min(from, trim.duration - span);
  trim.view = [from, from + span];
  drawTrim();
}

function zoomBy(factor, center) {
  const [v0, v1] = trim.view;
  setView(center ?? (v0 + v1) / 2, (v1 - v0) / factor);
}

/** Fait défiler l'onde zoomée pour garder la poignée réglée à l'écran. */
function keepVisible(ms) {
  const [v0, v1] = trim.view;
  const span = v1 - v0;
  if (ms < v0) setView(ms + span / 2 - span * 0.1, span);
  else if (ms > v1) setView(ms - span / 2 + span * 0.1, span);
}

zoomInBtn.addEventListener("click", () => zoomBy(2, trim.focus === "end" ? trim.end : trim.start));
zoomOutBtn.addEventListener("click", () => zoomBy(0.5));
// Ctrl + molette : zoom là où est la souris ; molette de côté ou Maj +
// molette : défilement de l'onde zoomée. La molette seule fait défiler la
// fenêtre comme d'habitude.
canvas.addEventListener(
  "wheel",
  (e) => {
    if (!trim.duration) return;
    const [v0, v1] = trim.view;
    if (e.ctrlKey) {
      e.preventDefault();
      zoomBy(e.deltaY < 0 ? 1.5 : 1 / 1.5, msAt(e));
    } else if (e.shiftKey || Math.abs(e.deltaX) > Math.abs(e.deltaY)) {
      if (v1 - v0 >= trim.duration) return;
      e.preventDefault();
      const d = (e.shiftKey ? e.deltaY : e.deltaX) || e.deltaX;
      setView((v0 + v1) / 2 + (d / canvas.clientWidth) * (v1 - v0), v1 - v0);
    }
  },
  { passive: false },
);

function msAt(e) {
  const r = canvas.getBoundingClientRect();
  const ratio = Math.min(1, Math.max(0, (e.clientX - r.left) / r.width));
  const [v0, v1] = trim.view;
  return Math.round(v0 + ratio * (v1 - v0));
}

const MIN_TRIM_MS = 50;

canvas.addEventListener("pointerdown", (e) => {
  if (!trim.duration) return;
  const ms = msAt(e);
  trim.drag = Math.abs(ms - trim.start) <= Math.abs(ms - trim.end) ? "start" : "end";
  trim.focus = trim.drag;
  canvas.focus({ preventScroll: true });
  canvas.setPointerCapture(e.pointerId);
  moveHandle(ms);
});
canvas.addEventListener("pointermove", (e) => trim.drag && moveHandle(msAt(e)));
canvas.addEventListener("pointerup", () => {
  if (!trim.drag) return;
  trim.drag = null;
  commitEdit();
});

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
  trim.view = [0, trim.duration];
  drawTrim();
  commitEdit();
});

// Flèches : la poignée choisie bouge de 0,1 s (Maj : 0,01 s). Tab ou
// Début/Fin passent d'une poignée à l'autre.
canvas.addEventListener("keydown", (e) => {
  if (!trim.duration || e.ctrlKey || e.metaKey || e.altKey) return;
  if (e.key === "ArrowLeft" || e.key === "ArrowRight") {
    const step = (e.key === "ArrowLeft" ? -1 : 1) * (e.shiftKey ? 10 : 100);
    trim.drag = trim.focus;
    moveHandle((trim.focus === "start" ? trim.start : trim.end) + step);
    trim.drag = null;
    keepVisible(trim.focus === "start" ? trim.start : trim.end);
  } else if (e.key === "Home" || e.key === "End") {
    trim.focus = e.key === "Home" ? "start" : "end";
    keepVisible(trim[trim.focus]);
    drawTrim();
  } else if (e.key === "Tab" && trim.focus === (e.shiftKey ? "end" : "start")) {
    trim.focus = e.shiftKey ? "start" : "end";
    keepVisible(trim[trim.focus]);
    drawTrim();
  } else return;
  e.preventDefault();
});
canvas.addEventListener("keyup", (e) => {
  if (e.key === "ArrowLeft" || e.key === "ArrowRight") commitEdit();
});
canvas.addEventListener("focus", () => drawTrim());
canvas.addEventListener("blur", () => drawTrim());

// Coupe le silence au début et à la fin du passage gardé (marge de 30 ms
// avant le son, 80 ms après pour ne pas manger la fin qui s'éteint).
$('[data-action="trim-silence"]', dlgSound).addEventListener("click", () => {
  const n = trim.peaks.length;
  if (!n || !trim.duration) return;
  const per = trim.duration / n;
  const i0 = Math.floor(trim.start / per);
  const i1 = Math.min(n, Math.ceil(trim.end / per));
  const loudest = trim.peaks.slice(i0, i1).reduce((m, v) => Math.max(m, v), 0);
  const floor = Math.max(0.004, loudest * 0.03);
  let a = i0;
  while (a < i1 && trim.peaks[a] < floor) a++;
  let b = i1 - 1;
  while (b > a && trim.peaks[b] < floor) b--;
  if (a >= i1) return toast("Ce passage n'est que du silence.");
  const start = Math.max(trim.start, Math.round(a * per - 30));
  const end = Math.min(trim.end, Math.round((b + 1) * per + 80));
  if (start - trim.start < 20 && trim.end - end < 20) return toast("Pas de blanc à couper.");
  trim.start = start;
  trim.end = Math.max(end, start + MIN_TRIM_MS);
  drawTrim();
  commitEdit();
});

// Double-clic sur un curseur : retour à sa valeur normale.
dlgSound.addEventListener("dblclick", (e) => {
  const el = e.target;
  if (el.type !== "range") return;
  const normal = el.dataset.fx ? FX_DEFAULT[el.dataset.fx] : el.name === "volume" ? 1 : null;
  if (normal == null || Number(el.value) === normal) return;
  el.value = normal;
  el.dispatchEvent(new Event("input", { bubbles: true }));
  el.dispatchEvent(new Event("change", { bubbles: true }));
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
  startPreview();
});

function startPreview() {
  playhead.fx = currentFx();
  playhead.elapsed = 0;
  playhead.at = performance.now();
  call("preview_sound", {
    id: editing.id,
    startMs: trim.start,
    endMs: trim.end >= trim.duration ? null : trim.end,
    volume: Number(formSound.volume.value),
    fx: playhead.fx,
  });
}

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

/** Position dans le fichier d'origine (la vitesse et l'envers comptent). */
function playheadMs() {
  if (!playhead.active) return null;
  const fx = playhead.fx ?? FX_DEFAULT;
  const moved = (playhead.elapsed + (performance.now() - playhead.at)) * fx.speed;
  const ms = fx.reverse ? trim.end - moved : trim.start + moved;
  return Math.min(Math.max(ms, trim.start), trim.end);
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
      fx: currentFx(),
      ...trimValues(),
    },
  });
});

$('[data-action="delete"]', dlgSound).addEventListener("click", () => {
  if (!editing) return;
  dlgSound.close("delete");
  deleteSound(editing);
});

async function deleteSound(sound) {
  const res = await call("remove_sound", { id: sound.id });
  if (res) toast(`« ${sound.name} » supprimé`, false, { label: "Annuler", run: () => call("restore_sound", { id: sound.id }) });
}

// ---------- Effets du son ----------

const FX_DEFAULT = { speed: 1, pitch: 0, reverse: false, tone: "normal", echo: 0, fade_in_ms: 0, fade_out_ms: 0 };
const FX_PRESETS = {
  chipmunk: { pitch: 7, speed: 1.15 },
  demon: { pitch: -7 },
  slow: { speed: 0.7 },
  fast: { speed: 1.5 },
  cave: { echo: 0.6 },
  radio: { tone: "radio" },
  wall: { tone: "muffled" },
  bass: { tone: "bass" },
  reverse: { reverse: true },
};
const fxBox = $(".fx", dlgSound);
const fxInputs = [...fxBox.querySelectorAll("[data-fx]")];

function fmtFx(key, v) {
  if (key === "speed") return `× ${Number(v).toFixed(2).replace(/0$/, "")}`;
  if (key === "pitch") return v == 0 ? "normale" : `${v > 0 ? "+" : ""}${v} demi-ton${Math.abs(v) > 1 ? "s" : ""}`;
  if (key === "echo") return v == 0 ? "aucun" : Math.round(v * 100) + " %";
  return v == 0 ? "aucun" : (v / 1000).toFixed(1).replace(".", ",") + " s";
}

function currentFx() {
  const fx = { ...FX_DEFAULT };
  for (const el of fxInputs) {
    const k = el.dataset.fx;
    fx[k] = el.type === "checkbox" ? el.checked : el.tagName === "SELECT" ? el.value : Number(el.value);
  }
  return fx;
}

function setFx(fx) {
  fx = { ...FX_DEFAULT, ...(fx ?? {}) };
  for (const el of fxInputs) {
    const v = fx[el.dataset.fx];
    if (el.type === "checkbox") el.checked = !!v;
    else el.value = v;
  }
  refreshFx();
}

function refreshFx() {
  const fx = currentFx();
  for (const span of fxBox.querySelectorAll("[data-fxv]")) span.textContent = fmtFx(span.dataset.fxv, fx[span.dataset.fxv]);
  const active = [];
  for (const b of fxBox.querySelectorAll("[data-preset]")) {
    const preset = FX_PRESETS[b.dataset.preset];
    const on = Object.entries(preset).every(([k, v]) => fx[k] === v);
    b.classList.toggle("active", on);
    if (on) active.push(b.textContent.trim());
  }
  // Résumé visible même replié : « aucun », les effets choisis, ou « réglages perso ».
  const isDefault = Object.entries(FX_DEFAULT).every(([k, v]) => fx[k] === v);
  const state = $("#fx-state");
  state.textContent = isDefault ? "aucun" : active.length ? active.join(" · ") : "réglages perso";
  state.classList.toggle("on", !isDefault);
}

for (const el of fxInputs) {
  el.addEventListener("input", refreshFx);
  el.addEventListener("change", commitEdit);
}

fxBox.addEventListener("click", (e) => {
  const b = e.target.closest("[data-preset]");
  if (!b) return;
  const preset = FX_PRESETS[b.dataset.preset];
  const fx = currentFx();
  // Un effet actif se retire au second clic ; sinon il s'ajoute aux autres.
  const on = b.classList.contains("active");
  for (const [k, v] of Object.entries(preset)) fx[k] = on ? FX_DEFAULT[k] : v;
  setFx(fx);
  commitEdit();
});

$('[data-action="fx-reset"]', dlgSound).addEventListener("click", () => {
  setFx(FX_DEFAULT);
  commitEdit();
});

// ---------- Annuler / rétablir dans l'éditeur ----------

const editHistory = { stack: [], at: 0 };
const undoBtn = $('[data-action="undo"]', dlgSound);
const redoBtn = $('[data-action="redo"]', dlgSound);

function editState() {
  return JSON.stringify({ start: trim.start, end: trim.end, volume: formSound.volume.value, fx: currentFx() });
}

function resetHistory() {
  editHistory.stack = [editState()];
  editHistory.at = 0;
  updateHistoryButtons();
}

function updateHistoryButtons() {
  undoBtn.disabled = editHistory.at <= 0;
  redoBtn.disabled = editHistory.at >= editHistory.stack.length - 1;
}

/** Enregistre l'état courant comme une étape (si quelque chose a changé). */
function commitEdit() {
  const now = editState();
  if (now === editHistory.stack[editHistory.at]) return;
  editHistory.stack.length = editHistory.at + 1;
  editHistory.stack.push(now);
  if (editHistory.stack.length > 100) editHistory.stack.shift();
  editHistory.at = editHistory.stack.length - 1;
  updateHistoryButtons();
  // La pré-écoute en cours repart avec le nouveau réglage.
  if (playhead.active) startPreview();
}

function stepHistory(delta) {
  const at = editHistory.at + delta;
  if (at < 0 || at >= editHistory.stack.length) return;
  editHistory.at = at;
  const st = JSON.parse(editHistory.stack[at]);
  trim.start = st.start;
  trim.end = st.end;
  formSound.volume.value = st.volume;
  formSound.volume.update();
  setFx(st.fx);
  drawTrim();
  updateHistoryButtons();
  if (playhead.active) startPreview();
}

undoBtn.addEventListener("click", () => stepHistory(-1));
redoBtn.addEventListener("click", () => stepHistory(1));
formSound.volume.addEventListener("change", commitEdit);

dlgSound.addEventListener("keydown", (e) => {
  if (!(e.ctrlKey || e.metaKey)) return;
  // Dans un champ texte, Ctrl+Z garde son rôle normal.
  const t = e.target;
  if (t.tagName === "INPUT" && !["range", "checkbox"].includes(t.type) && !t.readOnly) return;
  const key = e.key.toLowerCase();
  if (key === "z" && !e.shiftKey) stepHistory(-1);
  else if (key === "y" || (key === "z" && e.shiftKey)) stepHistory(1);
  else return;
  e.preventDefault();
});

// ---------- Dialogue « Réglages » ----------

const dlgSettings = $("#dlg-settings");
const formSettings = $("form", dlgSettings);
const SETTINGS_HOTKEYS = ["stop_hotkey", "mute_hotkey", "random_hotkey", "replay_hotkey"];
for (const k of SETTINGS_HOTKEYS) setupHotkeyInput(formSettings[k], () => ({ setting: k }));
bindRangeLabels(formSettings);
enterSaves(dlgSettings);

/** Relit les périphériques audio (lent sous Windows, donc seulement ici). */
async function refreshDevices() {
  try {
    render(await invoke("refresh_devices"));
  } catch {}
}

function fillDeviceSelects(chosen) {
  const d = state.devices;
  fillSelect(formSettings.monitor_device, d.outputs, chosen.monitor_device, `Par défaut (${d.default_output ?? "aucun"})`);
  fillSelect(formSettings.virtual_device, d.outputs, chosen.virtual_device, "Aucun (sons uniquement dans le casque)");
  fillSelect(formSettings.mic_device, d.inputs, chosen.mic_device, `Par défaut (${d.default_input ?? "aucun"})`);
}

// Les réglages s'ouvrent tout de suite avec la liste des périphériques déjà
// connue (tenue à jour toutes les 3 s) ; la liste fraîche, lente à obtenir
// de Windows, arrive juste après sans perdre ce qui a été choisi.
async function openSettings() {
  const s = state.config.settings;
  fillDeviceSelects(s);
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
  fillFriendsSettings(s);
  updateNormalizeRow();
  dlgSettings.returnValue = "";
  dlgSettings.showModal();
  startMeter();
  invoke("refresh_devices").then((snap) => {
    // Rien de neuf : pas besoin de redessiner toute la grille derrière.
    if (JSON.stringify(snap.devices) === JSON.stringify(state.devices)) return;
    render(snap);
    if (!dlgSettings.open) return;
    const f = formSettings;
    fillDeviceSelects({
      monitor_device: f.monitor_device.value || null,
      virtual_device: f.virtual_device.value || null,
      mic_device: f.mic_device.value || null,
    });
  }).catch(() => {});
}

function nearestTarget(v) {
  const options = [-31, -28, -25];
  return options.reduce((best, o) => (Math.abs(o - v) < Math.abs(best - v) ? o : best), -28);
}

// Réglages avancés : cachés par défaut, le choix est gardé sur ce PC.
const advToggle = $('[data-action="advanced"]', dlgSettings);
function applyAdvanced(on) {
  dlgSettings.classList.toggle("show-advanced", on);
  advToggle.checked = on;
}
try { applyAdvanced(localStorage.getItem("soundboard.advanced") === "1"); } catch { applyAdvanced(false); }
advToggle.addEventListener("change", () => {
  applyAdvanced(advToggle.checked);
  try { localStorage.setItem("soundboard.advanced", advToggle.checked ? "1" : "0"); } catch {}
});

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
    replay_hotkey: f.replay_hotkey.dataset.hotkey || null,
    replay_enabled: f.replay_enabled.checked,
    duck_enabled: f.duck_enabled.checked,
    duck_level: Number(f.duck_level.value),
    room_code: f.room_code.value.trim().toUpperCase(),
    room_name: f.room_name.value.trim(),
    room_accept: f.room_accept.checked,
    clips_mode: f.clips_mode.value,
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
  // Déjà installé : on cache la marche à suivre pour l'installer.
  $("[data-install]", dlgWizard).hidden = !!found;
  $('[data-action="open-vb"]', dlgWizard).hidden = !!found;
  $('[data-action="recheck"]', dlgWizard).hidden = !!found;
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

// ---------- Affichage : taille, contraste, animations ----------
// Préférences gardées sur ce PC (comme le thème).

const motionQuery = window.matchMedia("(prefers-reduced-motion: reduce)");
const PREF_DEFAULTS = { zoom: "1", contrast: false, reduceMotion: false };
const ZOOMS = ["0.9", "1", "1.1", "1.25", "1.5"];
const prefs = { ...PREF_DEFAULTS };
try {
  Object.assign(prefs, JSON.parse(localStorage.getItem("soundboard.display") || "{}"));
} catch {}
const reducedMotion = () => prefs.reduceMotion || motionQuery.matches;

function applyDisplay() {
  const root = document.documentElement;
  // Zoom de la fenêtre elle-même (comme Ctrl + dans un navigateur).
  invoke("set_zoom", { scale: Number(prefs.zoom) || 1 }).catch(() => {});
  root.classList.toggle("contrast", !!prefs.contrast);
  root.classList.toggle("reduce-motion", reducedMotion());
  for (const el of $$("[data-pref]", dlgSettings)) {
    if (el.type === "checkbox") el.checked = !!prefs[el.dataset.pref];
    else el.value = prefs[el.dataset.pref];
  }
  if (state) renderPlaying();
}
function setPref(key, value) {
  prefs[key] = value;
  try {
    localStorage.setItem("soundboard.display", JSON.stringify(prefs));
  } catch {}
  applyDisplay();
}
for (const el of $$("[data-pref]", dlgSettings)) {
  // Appliqué tout de suite, sans attendre « Enregistrer ».
  el.addEventListener("change", () => setPref(el.dataset.pref, el.type === "checkbox" ? el.checked : el.value));
}
motionQuery.addEventListener("change", applyDisplay);
document.addEventListener("keydown", (e) => {
  if (!e.ctrlKey || e.altKey) return;
  const i = Math.max(0, ZOOMS.indexOf(String(prefs.zoom)));
  let next = null;
  if (e.key === "+" || e.key === "=") next = ZOOMS[Math.min(ZOOMS.length - 1, i + 1)];
  else if (e.key === "-") next = ZOOMS[Math.max(0, i - 1)];
  else if (e.key === "0") next = "1";
  if (next == null) return;
  e.preventDefault();
  setPref("zoom", next);
  toast(`Taille : ${Math.round(Number(next) * 100)} %`);
});
applyDisplay();

// ---------- Sauvegarde complète ----------

$('[data-action="backup-export"]', dlgSettings).addEventListener("click", async () => {
  const n = await call("export_backup");
  if (n != null) toast(`Sauvegarde enregistrée (${n} son${n > 1 ? "s" : ""})`);
});
$('[data-action="backup-import"]', dlgSettings).addEventListener("click", async () => {
  if (await restoreBackup()) dlgSettings.close();
});

async function restoreBackup(path) {
  if (!confirm("Remplacer ta soundboard actuelle par celle de la sauvegarde ?\nPense à sauvegarder l'actuelle avant si tu veux la garder.")) return false;
  const res = await call("import_backup", { path });
  if (res) toast(`Sauvegarde restaurée (${res.config.sounds.length} sons)`);
  return !!res;
}

// ---------- Clavier ----------

// Flèches pour passer d'un pad à l'autre, Entrée ou Espace pour jouer,
// F2 pour modifier, Suppr pour supprimer (avec « Annuler »).
$("#grid").addEventListener("keydown", (e) => {
  const pad = e.target.closest?.(".pad");
  if (!pad || e.ctrlKey || e.altKey || e.metaKey) return;
  const pads = $$("#grid .pad");
  const i = pads.indexOf(pad);
  const sound = soundById(pad.dataset.id);
  let next = null;
  if (e.key === "ArrowRight" || e.key === "ArrowLeft") {
    next = pads[i + (e.key === "ArrowRight" ? 1 : -1)];
  } else if (e.key === "ArrowDown" || e.key === "ArrowUp") {
    // Même colonne sur la ligne voisine (en liste : pad suivant / précédent).
    const x = pad.offsetLeft;
    const dir = e.key === "ArrowDown" ? 1 : -1;
    for (let j = i + dir; j >= 0 && j < pads.length; j += dir) {
      if (pads[j].offsetTop !== pad.offsetTop && Math.abs(pads[j].offsetLeft - x) < 4) {
        next = pads[j];
        break;
      }
    }
    next ??= pads[i + dir];
  } else if (e.key === "Home") next = pads[0];
  else if (e.key === "End") next = pads[pads.length - 1];
  else if (e.key === "F2" && sound) {
    e.preventDefault();
    return openSoundDialog(sound);
  } else if (e.key === "Delete" && sound) {
    e.preventDefault();
    return deleteSound(sound);
  }
  if (next) {
    e.preventDefault();
    next.focus();
  }
});

// Boutons avec seulement une icône : on leur donne un nom lisible par
// les lecteurs d'écran (celui de l'infobulle).
function labelIconButtons(root = document) {
  for (const b of $$("button[title]:not([aria-label])", root)) {
    if (!b.textContent.trim()) b.setAttribute("aria-label", b.title);
  }
}
labelIconButtons();
new MutationObserver(() => labelIconButtons()).observe(document.body, { subtree: true, childList: true, attributes: true, attributeFilter: ["title"] });

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
$('[data-action="report"]', dlgDiag).addEventListener("click", async () => {
  try {
    await navigator.clipboard.writeText(await invoke("diagnostic_report"));
    toast("Rapport copié : colle-le dans ton message (Ctrl + V)");
  } catch (e) {
    toast(String(e), true);
  }
});
$('[data-action="close"]', dlgDiag).addEventListener("click", () => dlgDiag.close());
$('[data-action="listen"]', dlgDiag).addEventListener("click", () => setListening(!listening));
dlgDiag.addEventListener("close", () => listening && setListening(false));

// ---------- Mode Surprise ----------

const dlgSurprise = $("#dlg-surprise");
const formSurprise = $("form", dlgSurprise);

function renderSurprise() {
  const pill = $("#surprise-toggle");
  const s = state.config.settings;
  pill.classList.toggle("on", state.surprise_on);
  pill.querySelector(".lbl").textContent = state.surprise_on ? "Surprise activée" : "Surprise";
  pill.title = state.surprise_on
    ? `Un son au hasard toutes les ${s.surprise_min_minutes} à ${s.surprise_max_minutes} min. Clique pour régler ou arrêter.`
    : "Des sons qui partent tout seuls, au hasard";
}

function surpriseCount() {
  const boxes = $$("#surprise-list input");
  const n = boxes.filter((b) => b.checked).length;
  $("#surprise-count").textContent =
    n === 0 ? "Aucun coché : tous les sons peuvent sortir." : `${n} son${n > 1 ? "s" : ""} sur ${boxes.length}`;
}

function openSurprise() {
  const s = state.config.settings;
  const chosen = new Set(s.surprise_sounds || []);
  formSurprise.on.checked = state.surprise_on;
  formSurprise.min.value = s.surprise_min_minutes;
  formSurprise.max.value = s.surprise_max_minutes;
  const list = $("#surprise-list");
  list.replaceChildren(
    ...state.config.sounds.map((snd) => {
      const row = document.createElement("label");
      const box = document.createElement("input");
      box.type = "checkbox";
      box.value = snd.id;
      box.checked = chosen.has(snd.id);
      const dot = document.createElement("span");
      dot.className = "dot";
      dot.style.setProperty("--pad-color", snd.color);
      const name = document.createElement("span");
      name.className = "name";
      name.textContent = (snd.emoji ? snd.emoji + " " : "") + snd.name;
      row.append(box, dot, name);
      return row;
    }),
  );
  if (!state.config.sounds.length) list.textContent = "Ajoute d'abord quelques sons.";
  surpriseCount();
  dlgSurprise.returnValue = "";
  dlgSurprise.showModal();
}

$("#surprise-list").addEventListener("change", surpriseCount);
for (const [action, checked] of [["all", true], ["none", false]]) {
  $(`[data-action="${action}"]`, dlgSurprise).addEventListener("click", () => {
    $$("#surprise-list input").forEach((b) => (b.checked = checked));
    surpriseCount();
  });
}
$('[data-action="cancel"]', dlgSurprise).addEventListener("click", () => dlgSurprise.close());
dlgSurprise.addEventListener("close", async () => {
  if (dlgSurprise.returnValue !== "save") return;
  const minutes = (v, d) => Math.min(1440, Math.max(1, Math.round(Number(v)) || d));
  let min = minutes(formSurprise.min.value, 10);
  let max = minutes(formSurprise.max.value, 60);
  if (max < min) [min, max] = [max, min];
  const settings = {
    ...state.config.settings,
    surprise_min_minutes: min,
    surprise_max_minutes: max,
    surprise_sounds: $$("#surprise-list input").filter((b) => b.checked).map((b) => b.value),
  };
  if (!(await call("update_settings", { settings }))) return;
  const on = formSurprise.on.checked;
  // Relancer aussi quand c'était déjà en marche : le nouvel intervalle s'applique tout de suite.
  if (on || state.surprise_on) await call("set_surprise", { on });
  toast(on ? `Mode Surprise activé : un son toutes les ${min} à ${max} min` : "Réglages enregistrés");
});
$("#surprise-toggle").addEventListener("click", openSurprise);

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

// Écran vide : les 3 étapes, cochées au fur et à mesure (l'assistant fait le détail).
function renderStartSteps() {
  const s = state.config.settings;
  const done = {
    cable: !!state.devices.vb_cable,
    discord: !!state.devices.vb_cable && !!s.virtual_device && !!s.setup_done,
    sound: state.config.sounds.length > 0,
  };
  for (const li of $$("#empty [data-step]")) {
    const ok = done[li.dataset.step];
    li.classList.toggle("done", ok);
    $(".mark", li).textContent = ok ? "✓" : String([...li.parentNode.children].indexOf(li) + 1);
    const btn = $("button", li);
    if (btn) btn.hidden = ok;
  }
}
for (const b of $$('#empty [data-action="start-wizard"]')) b.addEventListener("click", () => openWizard());
$('#empty [data-action="start-add"]').addEventListener("click", () => call("pick_sounds"));
$('#empty [data-action="start-online"]').addEventListener("click", () => $("#btn-online").click());
$('#empty [data-action="start-import"]').addEventListener("click", () => importPack());
$("#btn-stop").addEventListener("click", () => call("stop_all"));
$("#btn-settings").addEventListener("click", openSettings);
$("#cable-status").addEventListener("click", () => (state.devices.vb_cable ? openDiagnostics() : openWizard()));
$("#btn-stats").addEventListener("click", openStats);
$("#search").addEventListener("input", renderGrid);
$("#search").addEventListener("keydown", (e) => {
  if (e.key === "Escape") {
    if (!e.target.value) return e.target.blur();
    e.target.value = "";
    renderGrid();
  } else if (e.key === "Enter") {
    // Entrée joue le premier son trouvé.
    const pad = $("#grid .pad.first-hit");
    if (!pad) return;
    e.preventDefault();
    flash(pad.dataset.id);
    playSound(pad.dataset.id);
  } else if (e.key === "ArrowDown") {
    const pad = $("#grid .pad");
    if (pad) {
      e.preventDefault();
      pad.focus();
    }
  }
});
// Taper une lettre n'importe où lance la recherche, sans cliquer dedans.
document.addEventListener("keydown", (e) => {
  if (e.key === "Escape" && (selecting || selection.size) && !document.querySelector("dialog[open]")) {
    return clearSelection();
  }
  if (e.defaultPrevented || e.ctrlKey || e.altKey || e.metaKey || e.key.length !== 1 || e.key === " ") return;
  if (document.querySelector("dialog[open]")) return;
  if (e.target.closest?.("input, textarea, select, [contenteditable]")) return;
  const search = $("#search");
  e.preventDefault();
  search.focus();
  search.value += e.key;
  renderGrid();
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

$("#btn-import").addEventListener("click", () => importPack());

// Barre des onglets collée sous la barre du haut : sa position sert aux titres
// de groupe, qui restent visibles juste en dessous pendant le défilement.
{
  const root = document.documentElement;
  const sync = () => {
    const top = $(".topbar").offsetHeight;
    root.style.setProperty("--topbar-h", top + "px");
    root.style.setProperty("--sticky-h", top + $(".tabbar").offsetHeight + "px");
  };
  new ResizeObserver(sync).observe($(".tabbar"));
  new ResizeObserver(sync).observe($(".topbar"));
  sync();
}

// Menu « ⋯ » de la barre des onglets : sélection, export et import.
const tabMoreBtn = $("#btn-more");
const tabMoreMenu = $("#more-menu");
function setTabMoreMenu(open) {
  tabMoreMenu.hidden = !open;
  tabMoreBtn.setAttribute("aria-expanded", String(open));
  tabMoreBtn.classList.toggle("on", open);
  if (open) $("button", tabMoreMenu).focus();
}
tabMoreBtn.addEventListener("click", () => setTabMoreMenu(tabMoreMenu.hidden));
// Une action choisie referme le menu.
tabMoreMenu.addEventListener("click", (e) => e.target.closest("button") && setTabMoreMenu(false));
document.addEventListener("pointerdown", (e) => {
  if (!tabMoreMenu.hidden && !e.target.closest(".more")) setTabMoreMenu(false);
});
tabMoreMenu.addEventListener("keydown", (e) => {
  const items = $$("button", tabMoreMenu);
  const i = items.indexOf(document.activeElement);
  if (e.key === "Escape") {
    e.stopPropagation();
    setTabMoreMenu(false);
    tabMoreBtn.focus();
  } else if (e.key === "ArrowDown" || e.key === "ArrowUp") {
    e.preventDefault();
    items[(i + (e.key === "ArrowDown" ? 1 : items.length - 1)) % items.length].focus();
  }
});

async function importPack(path) {
  const res = await call("import_pack", { path });
  if (!res) return;
  render(res.snapshot);
  if (res.errors.length) toast(`${res.added} son(s) ajouté(s). Problèmes :\n${res.errors.join("\n")}`, true);
  else toast(res.added > 1 ? `${res.added} sons ajoutés` : `${res.added} son ajouté`);
}

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
// Télécharge et installe la nouvelle version ; `onStatus` affiche l'avancement.
async function installUpdate(info, onStatus) {
  onStatus(`Téléchargement de la version ${info.version}, l'app va redémarrer toute seule.`);
  try {
    await invoke("install_update");
    return true;
  } catch (e) {
    // Version publiée sans mise à jour intégrée, ou pas de connexion.
    onStatus(`Mise à jour impossible : ${e}`);
    return false;
  }
}
async function checkUpdate() {
  let info = null;
  try {
    info = await invoke("check_update");
  } catch {
    return; // Pas de connexion : on réessaiera au prochain lancement.
  }
  if (!info) return;
  const text = $("span", banner);
  text.textContent = `Une nouvelle version (${info.version}) est disponible. Tes sons et réglages sont gardés.`;
  banner.hidden = false;
  $('[data-action="download"]', banner).onclick = () => call("open_release_page", { url: info.url });
  const install = $('[data-action="install"]', banner);
  install.onclick = async () => {
    install.disabled = true;
    install.textContent = "Mise à jour…";
    if (!(await installUpdate(info, (m) => (text.textContent = m)))) {
      install.disabled = false;
      install.textContent = "Mettre à jour";
    }
  };
}

// Bouton des réglages : vérifie à la demande, puis installe si une version est là.
const updateStatus = $("#update-status");
const updateBtn = $('[data-action="check-update"]', dlgSettings);
let currentVersion = "";
window.__TAURI__.app.getVersion().then((v) => {
  currentVersion = v;
  updateStatus.textContent = `Version actuelle : ${v}`;
});
let pendingUpdate = null;
updateBtn.addEventListener("click", async () => {
  updateBtn.disabled = true;
  if (pendingUpdate) {
    updateBtn.textContent = "Mise à jour…";
    if (!(await installUpdate(pendingUpdate, (m) => (updateStatus.textContent = m)))) {
      updateBtn.disabled = false;
      updateBtn.textContent = "Réessayer";
    }
    return;
  }
  updateBtn.textContent = "Recherche…";
  try {
    pendingUpdate = await invoke("check_update");
    updateStatus.textContent = pendingUpdate
      ? `Version ${pendingUpdate.version} disponible (tu as la ${currentVersion}).`
      : `Tu as la dernière version (${currentVersion}).`;
    updateBtn.textContent = pendingUpdate ? "Mettre à jour" : "Rechercher";
  } catch (e) {
    updateStatus.textContent = `Pas de connexion à GitHub : ${e}`;
    updateBtn.textContent = "Réessayer";
  }
  updateBtn.disabled = false;
});
$('[data-action="dismiss"]', banner).addEventListener("click", () => (banner.hidden = true));

// ---------- Fichiers glissés depuis l'Explorateur ----------

const overlay = $("#drop-overlay");
listen("tauri://drag-enter", () => (overlay.hidden = false));
listen("tauri://drag-leave", () => (overlay.hidden = true));
listen("tauri://drag-drop", async (event) => {
  overlay.hidden = true;
  const all = event.payload?.paths ?? [];
  // Un .zip glissé : pack de sons ou sauvegarde complète, reconnu tout seul.
  const zips = all.filter((p) => /\.zip$/i.test(p));
  const paths = all.filter((p) => !/\.zip$/i.test(p));
  for (const zip of zips) {
    const kind = await invoke("zip_kind", { path: zip });
    if (kind === "pack") await importPack(zip);
    else if (kind === "backup") await restoreBackup(zip);
    else toast("Ce .zip n'est ni un pack de sons ni une sauvegarde de la soundboard", true);
  }
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
// Recherche sans résultat dans mes sons : on la relance sur internet.
$('#search-empty [data-action="search-online"]').addEventListener("click", () => {
  $("#btn-online").click();
  onlineForm.q.value = $("#search").value.trim();
  onlineForm.requestSubmit();
});
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


// ---------- 2.0 : rejouer, enregistrer, texte → voix ----------

/** Un son vient d'être créé : on le montre et on propose de le découper. */
function offerTrim(id, message) {
  const sound = state.config.sounds.find((s) => s.id === id);
  if (!sound) return toast(message);
  flash(id);
  toast(message, false, { label: "Découper", run: () => openSoundDialog(sound) });
}

async function saveReplay() {
  toast("Enregistrement des 30 dernières secondes…");
  const res = await call("save_replay");
  if (!res) return;
  render(res.snapshot);
  offerTrim(res.id, "Extrait ajouté : garde le passage qui t'intéresse");
}
$("#btn-replay").addEventListener("click", saveReplay);
listen("replay-saved", ({ payload }) => offerTrim(payload, "Extrait des 30 dernières secondes ajouté"));
listen("replay-error", ({ payload }) => toast(String(payload), true));

const dlgRecord = $("#dlg-record");
const rec = { on: false, started: 0, timer: 0 };
function recUi() {
  const btn = $('[data-action="rec"]', dlgRecord);
  btn.classList.toggle("on", rec.on);
  btn.setAttribute("aria-label", rec.on ? "Arrêter et garder" : "Commencer l'enregistrement");
  $("[data-rec-state]", dlgRecord).textContent = rec.on
    ? "Parle… Appuie sur le carré pour arrêter et garder le son."
    : "Appuie sur le bouton rouge pour commencer.";
  const secs = rec.on ? Math.floor((Date.now() - rec.started) / 1000) : 0;
  $(".rec-time", dlgRecord).textContent = `${Math.floor(secs / 60)}:${String(secs % 60).padStart(2, "0")}`;
  // Une minute au plus : on s'arrête tout seul.
  if (rec.on && secs >= 60) stopRecording(true);
}
async function stopRecording(keep) {
  if (!rec.on) return;
  rec.on = false;
  clearInterval(rec.timer);
  recUi();
  const res = await call("mic_record_stop", { keep });
  if (!keep || !res) return;
  dlgRecord.close();
  render(res.snapshot);
  offerTrim(res.id, "Enregistrement ajouté");
}
$('[data-action="rec"]', dlgRecord).addEventListener("click", async () => {
  if (rec.on) return stopRecording(true);
  try {
    await invoke("mic_record_start");
  } catch (e) {
    return toast(String(e), true);
  }
  rec.on = true;
  rec.started = Date.now();
  rec.timer = setInterval(recUi, 250);
  recUi();
});
$('[data-action="close"]', dlgRecord).addEventListener("click", () => dlgRecord.close());
dlgRecord.addEventListener("close", () => stopRecording(false));
$("#btn-record").addEventListener("click", () => {
  if (!state.config.settings.mic_enabled) return toast("Active d'abord « Envoyer ma voix dans Discord » dans les réglages (Micro)", true);
  recUi();
  dlgRecord.showModal();
});

const dlgTts = $("#dlg-tts");
const formTts = $("form", dlgTts);
bindRangeLabels(formTts);
formTts.rate.update = () => ($('.value[data-for="rate"]', dlgTts).textContent = `× ${Number(formTts.rate.value).toFixed(1)}`);
formTts.rate.addEventListener("input", formTts.rate.update);
let ttsVoices = null;
const ttsPrefs = () => {
  try { return JSON.parse(localStorage.getItem("soundboard.tts") || "{}"); } catch { return {}; }
};
$("#btn-tts").addEventListener("click", async () => {
  const prefs = ttsPrefs();
  if (!ttsVoices) ttsVoices = (await call("tts_voices")) || [];
  formTts.voice.replaceChildren(...ttsVoices.map((v) => new Option(`${v.name} (${v.language})`, v.id)));
  if (!ttsVoices.length) formTts.voice.add(new Option("Voix par défaut de Windows", ""));
  if (prefs.voice && ttsVoices.some((v) => v.id === prefs.voice)) formTts.voice.value = prefs.voice;
  formTts.rate.value = prefs.rate ?? 1;
  formTts.rate.update();
  dlgTts.showModal();
  formTts.text.focus();
});
function ttsArgs() {
  const args = { text: formTts.text.value, voice: formTts.voice.value || null, rate: Number(formTts.rate.value) };
  try { localStorage.setItem("soundboard.tts", JSON.stringify({ voice: args.voice, rate: args.rate })); } catch {}
  return args;
}
$('[data-action="say"]', dlgTts).addEventListener("click", async (e) => {
  e.currentTarget.disabled = true;
  await call("tts_say", ttsArgs());
  e.currentTarget.disabled = false;
});
$('[data-action="create"]', dlgTts).addEventListener("click", async (e) => {
  const btn = e.currentTarget;
  btn.disabled = true;
  const res = await call("tts_create", ttsArgs());
  btn.disabled = false;
  if (!res) return;
  dlgTts.close();
  formTts.text.value = "";
  render(res.snapshot);
  offerTrim(res.id, "Pad créé");
});
$('[data-action="close"]', dlgTts).addEventListener("click", () => dlgTts.close());
formTts.text.addEventListener("keydown", (e) => {
  if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) $('[data-action="say"]', dlgTts).click();
});

// ---------- 2.0 : packs partagés par lien ----------

const dlgFollow = $("#dlg-follow");
const formFollow = $("form", dlgFollow);
function openFollow() {
  formFollow.reset();
  dlgFollow.returnValue = "";
  dlgFollow.showModal();
}
$("#btn-follow").addEventListener("click", openFollow);
$('[data-action="close"]', dlgFollow).addEventListener("click", () => dlgFollow.close());
dlgFollow.addEventListener("close", async () => {
  if (dlgFollow.returnValue !== "follow") return;
  toast("Téléchargement du pack…");
  const res = await call("subscribe_pack", { url: formFollow.url.value.trim(), name: formFollow.name.value.trim() || null });
  if (!res) return;
  render(res.snapshot);
  const sub = state.config.settings.subscriptions.find((s) => s.url === formFollow.url.value.trim());
  toast(`${res.added} son${res.added > 1 ? "s" : ""} ajouté${res.added > 1 ? "s" : ""} dans « ${sub?.name ?? "l'onglet du pack"} ». Les nouveaux arriveront tout seuls.`);
});
listen("packs-updated", ({ payload: [added, removed] }) => {
  const parts = [];
  if (added) parts.push(`${added} nouveau${added > 1 ? "x" : ""} son${added > 1 ? "s" : ""}`);
  if (removed) parts.push(`${removed} retiré${removed > 1 ? "s" : ""}`);
  toast(`Packs de tes potes : ${parts.join(", ")}`);
});

const dlgPublish = $("#dlg-publish");
let publishTab = null;
function showPublishDone(category) {
  const pub = state.config.settings.published.find((p) => p.category === category);
  $('[data-step="pick"]', dlgPublish).hidden = !!pub;
  $('[data-step="done"]', dlgPublish).hidden = !pub;
  if (pub) $("[data-path]", dlgPublish).textContent = pub.path;
  if (pub?.error) toast(`Pack pas encore écrit : ${pub.error}`, true);
}
async function publish(folder) {
  const res = await call("publish_tab", { category: publishTab, folder });
  if (res) showPublishDone(publishTab);
}
$("#btn-publish").addEventListener("click", async () => {
  publishTab = tabCategory();
  if (!publishTab) return toast("Ouvre d'abord l'onglet à partager (pas « Tous »)", true);
  $("[data-tab-name]", dlgPublish).textContent = publishTab;
  const folders = (await call("cloud_folders")) || [];
  const list = $("#cloud-list");
  list.replaceChildren(
    ...folders.map((f) => {
      const b = document.createElement("button");
      b.type = "button";
      const name = document.createElement("b");
      name.textContent = f.name;
      const path = document.createElement("small");
      path.textContent = f.path;
      b.append(name, path);
      b.addEventListener("click", () => publish(f.path));
      return b;
    }),
  );
  if (!folders.length) list.textContent = "Aucun OneDrive, Dropbox ou Google Drive trouvé sur ce PC : choisis un dossier synchronisé.";
  showPublishDone(publishTab);
  dlgPublish.showModal();
});
$('[data-action="other"]', dlgPublish).addEventListener("click", () => publish(null));
$('[data-action="reveal"]', dlgPublish).addEventListener("click", () => call("reveal_published", { category: publishTab }));
$('[data-action="close"]', dlgPublish).addEventListener("click", () => dlgPublish.close());

function linkItem(title, sub, isError, buttons) {
  const row = document.createElement("div");
  row.className = "link-item";
  const txt = document.createElement("span");
  txt.className = "txt";
  const b = document.createElement("b");
  b.textContent = title;
  const small = document.createElement("small");
  small.textContent = sub;
  small.title = sub;
  small.classList.toggle("err", isError);
  txt.append(b, small);
  row.append(txt);
  for (const [label, run] of buttons) {
    const btn = document.createElement("button");
    btn.type = "button";
    btn.textContent = label;
    btn.addEventListener("click", run);
    row.append(btn);
  }
  return row;
}

function ago(secs) {
  if (!secs) return "jamais vérifié";
  const min = Math.round((Date.now() / 1000 - secs) / 60);
  return min < 1 ? "vérifié à l'instant" : min < 60 ? `vérifié il y a ${min} min` : `vérifié il y a ${Math.round(min / 60)} h`;
}

function renderBlockList() {
  const blocked = state.config.settings.room_blocked ?? [];
  const isBlocked = (id) => blocked.some((b) => b.id === id);
  const toggle = (id, name, on) => async () => {
    const res = await call("set_friend_blocked", { id, name, blocked: on });
    if (res) {
      toast(on ? `${name} est bloqué : plus de sons ni d'extraits de sa part` : `${name} est débloqué`);
      renderBlockList();
    }
  };
  const rows = [
    ...blocked.map((b) => linkItem(`🚫 ${b.name || "Pote"}`, "Bloqué", false, [["Débloquer", toggle(b.id, b.name, false)]])),
    ...room.members.filter((m) => !isBlocked(m.id)).map((m) => linkItem(m.name, "En ligne", false, [["Bloquer", toggle(m.id, m.name, true)]])),
  ];
  if (!rows.length) {
    const note = document.createElement("p");
    note.className = "hint";
    note.textContent = "Tes potes en ligne apparaîtront ici.";
    rows.push(note);
  }
  $("#block-list").replaceChildren(...rows);
}

function renderLinkLists() {
  renderBlockList();
  const s = state.config.settings;
  $("#subs-list").replaceChildren(
    ...s.subscriptions.map((sub) =>
      linkItem(sub.name || "Pack de potes", sub.error ? `⚠ ${sub.error}` : `${ago(sub.last_check)} · ${sub.url}`, !!sub.error, [
        ["Ne plus suivre", async () => {
          const remove = confirm(`Ne plus suivre « ${sub.name} ».\n\nSupprimer aussi ses sons ? (OK : supprimer, Annuler : les garder)`);
          await call("unsubscribe_pack", { url: sub.url, removeSounds: remove });
          renderLinkLists();
        }],
      ]),
    ),
  );
  $("#pubs-list").replaceChildren(
    ...s.published.map((p) =>
      linkItem(p.category, p.error ? `⚠ ${p.error}` : p.path, !!p.error, [
        ["Ouvrir le dossier", () => call("reveal_published", { category: p.category })],
        ["Arrêter", async () => {
          await call("unpublish_tab", { category: p.category });
          renderLinkLists();
        }],
      ]),
    ),
  );
}
$('[data-action="follow"]', dlgSettings).addEventListener("click", openFollow);
$('[data-action="sync"]', dlgSettings).addEventListener("click", async () => {
  const res = await call("sync_packs");
  if (!res) return;
  renderLinkLists();
  if (res.errors.length) toast(res.errors.join("\n"), true);
  else toast(res.added + res.removed ? `${res.added} ajouté(s), ${res.removed} retiré(s)` : "Tout est à jour");
});

// ---------- 2.0 : salon entre potes ----------

let room = { on: false, connected: false, members: [], target: null };
const roomPill = $("#room-pill");
const roomMenu = $("#room-menu");

function renderRoom() {
  const wrap = $(".room-wrap");
  wrap.hidden = !state.config.settings.room_code;
  const target = room.members.find((m) => m.id === room.target);
  roomPill.className = "pill" + (target ? " away" : "") + (room.connected ? "" : " offline");
  roomPill.textContent = target ? `👥 Joue chez ${target.name}` : room.connected ? `👥 Salon · ${room.members.length}` : "👥 Salon…";
  roomPill.title = target
    ? `Tes sons partent chez ${target.name} (dans Discord par son micro). Clique pour revenir chez toi.`
    : room.connected
      ? "Salon entre potes : choisis chez qui jouer tes sons"
      : "Connexion au salon…";
  const st = $("#room-state");
  if (st) {
    st.textContent = !state.config.settings.room_code
      ? ""
      : room.connected
        ? room.members.length
          ? `Connecté · en ligne : ${room.members.map((m) => m.name).join(", ")}`
          : "Connecté · aucun pote en ligne pour l'instant"
        : "Connexion au salon…";
  }
}

function setRoomMenu(open) {
  roomMenu.hidden = !open;
  roomPill.setAttribute("aria-expanded", String(open));
  if (!open) return;
  const item = (label, id) => {
    const b = document.createElement("button");
    b.type = "button";
    b.setAttribute("role", "menuitemradio");
    b.setAttribute("aria-checked", String(room.target === id));
    b.classList.toggle("on", room.target === id);
    b.textContent = (room.target === id ? "✓ " : "") + label;
    b.addEventListener("click", async () => {
      setRoomMenu(false);
      room = (await call("set_room_target", { target: id })) || room;
      renderRoom();
      const t = room.members.find((m) => m.id === room.target);
      toast(t ? `Tes sons partent maintenant chez ${t.name}` : "Tes sons sont de nouveau joués chez toi");
    });
    return b;
  };
  const title = document.createElement("div");
  title.className = "room-menu-title";
  title.textContent = "Jouer mes sons…";
  const items = [title, item("Chez moi", null), ...room.members.map((m) => item(`Chez ${m.name}`, m.id))];
  if (!room.members.length) {
    const note = document.createElement("div");
    note.className = "empty-note";
    note.textContent = room.connected
      ? "Aucun pote en ligne : ils doivent mettre le même code dans Réglages → Potes."
      : "Connexion au salon…";
    items.push(note);
  }
  const clipBtn = document.createElement("button");
  clipBtn.type = "button";
  clipBtn.setAttribute("role", "menuitem");
  clipBtn.className = "room-clip";
  clipBtn.textContent = "🎬 Envoyer un extrait…";
  clipBtn.addEventListener("click", () => {
    setRoomMenu(false);
    openClipDialog();
  });
  items.push(clipBtn);
  roomMenu.replaceChildren(...items);
}
roomPill.addEventListener("click", () => setRoomMenu(roomMenu.hidden));
document.addEventListener("pointerdown", (e) => {
  if (!roomMenu.hidden && !e.target.closest(".room-wrap")) setRoomMenu(false);
});
roomMenu.addEventListener("keydown", (e) => {
  if (e.key === "Escape") {
    e.stopPropagation();
    setRoomMenu(false);
    roomPill.focus();
  }
});
listen("room", ({ payload }) => {
  room = payload;
  renderRoom();
  if (!roomMenu.hidden) setRoomMenu(true);
  if (dlgSettings.open) renderBlockList();
});
listen("room-played", ({ payload: [who, title] }) => toast(`${who} a joué « ${title} » chez toi`));
listen("room-sent", ({ payload }) => {
  const t = room.members.find((m) => m.id === room.target);
  toast(`« ${payload} » envoyé chez ${t?.name ?? "ton pote"}`);
});
listen("room-error", ({ payload }) => toast(String(payload), true));
listen("clip-received", ({ payload: [who, title] }) => toast(`🎬 ${who} t'envoie un extrait${title ? ` : « ${title} »` : ""}`));

// ---------- 2.0 : extrait sur l'écran d'un pote ----------

const dlgClip = $("#dlg-clip");
const formClip = $("form", dlgClip);
const clipImg = $("img.clip-preview", dlgClip);
const clipVideo = $("video.clip-preview", dlgClip);
const clipGifs = $(".clip-gifs", dlgClip);
let clipMedia = null;
let clipSource = "file";
try { clipSource = localStorage.getItem("soundboard.clipSource") || "file"; } catch {}

function clipUi() {
  $(".clip-empty", dlgClip).hidden = !!clipMedia;
  clipImg.hidden = clipMedia?.kind !== "image";
  clipVideo.hidden = clipMedia?.kind !== "video";
  // Une vidéo garde son propre son.
  $("[data-for-image]", dlgClip).hidden = clipMedia?.kind === "video";
  dlgClip.querySelectorAll("[data-for-video]").forEach((el) => (el.hidden = clipMedia?.kind !== "video"));
  const hasSound = clipMedia?.kind === "video" || (clipMedia?.kind === "image" && !!formClip.sound.value);
  $("[data-for-sound]", dlgClip).hidden = !hasSound;
  $(".clip-listen > span", dlgClip).replaceChildren(
    clipMedia?.kind === "video" ? "Son de la vidéo " : "Son avec l'image ",
    Object.assign(document.createElement("span"), { className: "value", textContent: clipMedia?.kind === "video" ? `${Math.round(Number(formClip.cvol.value) * 100)} %` : "" }),
  );
  dlgClip.classList.toggle("no-media", !clipMedia);
  $('[data-action="send"]', dlgClip).disabled = !clipMedia;
  $('[data-action="send"]', dlgClip).title = clipMedia ? "" : "Choisis d'abord une image, une vidéo ou un GIF";
  $('[data-action="preview"]', dlgClip).disabled = !clipMedia;
  $(".meme-text", dlgClip).textContent = clipMedia ? formClip.title.value.trim() : "";
}
function setClipSource(source) {
  clipSource = source;
  try { localStorage.setItem("soundboard.clipSource", source); } catch {}
  dlgClip.classList.toggle("src-file", source === "file");
  dlgClip.querySelectorAll("#clip-source button").forEach((b) => b.classList.toggle("active", b.dataset.source === source));
  dlgClip.querySelectorAll("[data-pane]").forEach((p) => (p.hidden = p.dataset.pane !== source));
  clipUi();
  if (source === "link") formClip.link.focus();
  if (source === "history") renderClipHistory();
  if (source === "gif") {
    if (!state.config.settings.giphy_key) toast("Ajoute ta clé GIPHY gratuite dans les réglages (⚙ → Général) pour chercher des GIF", true);
    formClip.gifq.focus();
  }
}
dlgClip.querySelectorAll("#clip-source button").forEach((b) => b.addEventListener("click", () => setClipSource(b.dataset.source)));

function showClipMedia(picked, startS = 0) {
  stopClipListen();
  clipMedia = picked;
  const src = fileSrc(picked.path);
  if (picked.kind === "video") {
    clipImg.removeAttribute("src");
    clipVideo.src = src;
    clipVideo.currentTime = startS;
    clipVideo.play().catch(() => {});
  } else {
    clipVideo.removeAttribute("src");
    clipImg.src = src;
  }
  clipTrimFrom = startS;
  formClip.start.value = String(startS);
  clipUi();
  // L'aperçu peut être sous la liste : on le montre.
  requestAnimationFrame(() => $(".clip-frame", dlgClip).scrollIntoView({ behavior: "smooth", block: "nearest" }));
}
function openClipDialog(to) {
  if (!state.config.settings.room_code) return toast("Rejoins d'abord un salon avec tes potes : Réglages → Potes", true);
  const sounds = [...state.config.sounds].sort((a, b) => a.name.localeCompare(b.name, "fr"));
  formClip.sound.replaceChildren(new Option("Pas de son", ""), ...sounds.map((s) => new Option(`${s.emoji ? s.emoji + " " : ""}${s.name}`, s.id)));
  formClip.to.replaceChildren(
    new Option(`Tout le salon (${room.members.length} en ligne)`, ""),
    new Option("Tout le salon et moi (pour vérifier que ça marche)", "*moi"),
    ...room.members.map((m) => new Option(m.name, m.id)),
  );
  formClip.to.value = to ?? room.target ?? "";
  setClipSource(clipSource);
  clipUi();
  dlgClip.showModal();
  renderClipRecents();
}
const tileObserver = new IntersectionObserver((entries) => {
  for (const e of entries) {
    if (!e.isIntersecting) continue;
    e.target.src = e.target.dataset.src;
    tileObserver.unobserve(e.target);
  }
});
// Vignette d'un extrait envoyé ou reçu : un clic le remet tel quel.
function clipTile(r, caption) {
  const b = document.createElement("button");
  b.type = "button";
  b.title = [caption, r.title ? `« ${r.title} »` : r.kind === "video" ? "Vidéo" : "Image"].filter(Boolean).join(" · ");
  if (r.kind === "video") {
    const v = document.createElement("video");
    v.muted = true;
    v.preload = "metadata";
    // L'image de la vidéo n'est lue que quand la vignette devient visible.
    v.dataset.src = `${fileSrc(r.path)}#t=${(r.start_s || 0) + 0.5}`;
    tileObserver.observe(v);
    const tag = document.createElement("span");
    tag.className = "tag";
    tag.textContent = "▶";
    b.append(v, tag);
  } else {
    const img = document.createElement("img");
    img.loading = "lazy";
    img.decoding = "async";
    img.src = fileSrc(r.path);
    img.alt = "";
    b.append(img);
  }
  if (caption) {
    const name = document.createElement("span");
    name.className = "name";
    name.textContent = caption;
    b.append(name);
  }
  b.addEventListener("click", () => {
    clipTrimEnd = r.kind === "video" ? r.end_s ?? null : null;
    showClipMedia({ path: r.path, kind: r.kind }, r.start_s || 0);
    formClip.title.value = r.title || "";
    formClip.cvol.value = String(r.volume ?? 1);
    if (r.kind === "image") formClip.sound.value = r.sound_id || "";
    clipUi();
  });
  return b;
}
const fmtWhen = (at) => (at ? new Date(at * 1000).toLocaleString("fr-FR", { weekday: "short", hour: "2-digit", minute: "2-digit" }) : "");
// Les 5 derniers extraits envoyés, en haut de la fenêtre.
async function renderClipRecents() {
  const recents = (await invoke("clip_recents").catch(() => [])).slice(0, 5);
  const box = $(".clip-recents", dlgClip);
  box.hidden = !recents.length;
  $(".clip-recents-list", dlgClip).replaceChildren(...recents.map((r) => clipTile(r)));
}
// Onglet « Historique » : les 20 derniers envoyés et reçus.
async function renderClipHistory() {
  const h = await invoke("clip_history").catch(() => ({ sent: [], received: [] }));
  for (const [key, list] of Object.entries(h)) {
    const box = $(`[data-list="${key}"]`, dlgClip);
    if (!list.length) {
      const note = document.createElement("p");
      note.className = "hint";
      note.textContent = key === "sent" ? "Rien envoyé pour l'instant." : "Rien reçu pour l'instant.";
      box.replaceChildren(note);
      continue;
    }
    box.replaceChildren(...list.map((r) => clipTile(r, [r.from ? `de ${r.from}` : "", fmtWhen(r.at)].filter(Boolean).join(" · "))));
  }
}
$('[data-action="preview"]', dlgClip).addEventListener("click", () => {
  if (!clipMedia) return;
  stopClipListen();
  const video = clipMedia.kind === "video";
  invoke("clip_preview", {
    media: clipMedia.path,
    soundId: video ? null : formClip.sound.value || null,
    title: formClip.title.value,
    startS: video ? Number(formClip.start.value) || 0 : null,
    endS: video ? Number(formClip.end.value) || null : null,
    volume: video ? Number(formClip.cvol.value) : null,
  }).catch((err) => toast(String(err), true));
});
$("#btn-clip").addEventListener("click", () => openClipDialog());
$('[data-action="pick"]', dlgClip).addEventListener("click", async () => {
  const picked = await call("pick_clip_media");
  if (picked) showClipMedia(picked);
});
// Lien YouTube : « ?t=42 » ou « &t=1m30s » donne le début.
function linkStart(url) {
  try {
    const t = new URL(url).searchParams.get("t") || "";
    const m = t.match(/^(?:(\d+)h)?(?:(\d+)m)?(?:(\d+)s?)?$/);
    return m ? (Number(m[1] || 0) * 3600 + Number(m[2] || 0) * 60 + Number(m[3] || 0)) : 0;
  } catch {
    return 0;
  }
}
const clipVideos = $(".clip-videos", dlgClip);
const fetchLinkBtn = $('[data-action="fetch-link"]', dlgClip);
const isLink = (v) => /^https?:\/\/\S+$/i.test(v.trim());
formClip.link.addEventListener("input", () => {
  fetchLinkBtn.textContent = isLink(formClip.link.value) ? "Récupérer" : "Chercher";
});
// Avancement du téléchargement, sur la vignette ou le bouton.
let clipFetchTile = null;
listen("clip-progress", ({ payload }) => {
  const text = `Récupération… ${payload} %`;
  if (clipFetchTile) clipFetchTile.dataset.progress = text;
  else if (fetchLinkBtn.disabled) fetchLinkBtn.textContent = text;
});
async function fetchClipLink(url, tile) {
  clipFetchTile = tile || null;
  fetchLinkBtn.disabled = true;
  const label = fetchLinkBtn.textContent;
  if (tile) { tile.dataset.progress = "Récupération…"; tile.classList.add("loading"); }
  else fetchLinkBtn.textContent = "Récupération…";
  try {
    const picked = await invoke("clip_from_link", { url });
    showClipMedia(picked, linkStart(url));
    if (!formClip.title.value.trim() && picked.title) formClip.title.placeholder = picked.title;
  } catch (err) {
    toast(String(err), true);
  } finally {
    fetchLinkBtn.disabled = false;
    fetchLinkBtn.textContent = label;
    tile?.classList.remove("loading");
    if (tile) delete tile.dataset.progress;
    clipFetchTile = null;
  }
}
// Mots-clés : on cherche sur YouTube et on montre les miniatures ;
// un clic récupère la vidéo comme un lien collé. La dernière recherche est
// gardée (même après redémarrage) pour ne pas avoir à la retaper.
const LONG_VIDEO_S = 10 * 60;
const VSEARCH_KEY = "soundboard.clipSearch";
let vSearch = { q: "", results: [] };
function saveVSearch() {
  try { localStorage.setItem(VSEARCH_KEY, JSON.stringify(vSearch)); } catch {}
}
function videoTile(r) {
  const b = document.createElement("button");
  b.type = "button";
  b.setAttribute("role", "option");
  const long = (r.duration_s ?? 0) > LONG_VIDEO_S;
  b.title = long ? `${r.title}\nVidéo longue : plus lente à récupérer, et parfois trop lourde` : r.title;
  const img = document.createElement("img");
  img.src = r.thumb;
  img.alt = "";
  img.loading = "lazy";
  const name = document.createElement("span");
  name.className = "name";
  name.textContent = r.title;
  b.append(img, name);
  if (r.duration_s != null) {
    const tag = document.createElement("span");
    tag.className = long ? "tag long" : "tag";
    tag.textContent = (long ? "longue · " : "") + `${Math.floor(r.duration_s / 60)}:${String(r.duration_s % 60).padStart(2, "0")}`;
    b.append(tag);
  }
  b.addEventListener("click", () => {
    if (fetchLinkBtn.disabled) return;
    clipVideos.querySelectorAll("button").forEach((x) => x.classList.toggle("on", x === b));
    fetchClipLink(r.url, b);
  });
  return b;
}
function renderVideos() {
  if (!vSearch.results.length) {
    if (!vSearch.q) return clipVideos.replaceChildren();
    const note = document.createElement("p");
    note.className = "hint";
    note.textContent = "Aucune vidéo trouvée.";
    return clipVideos.replaceChildren(note);
  }
  const tiles = vSearch.results.map(videoTile);
  // YouTube ne renvoie que 12 vidéos à la fois : on peut en demander d'autres.
  if (vSearch.results.length % 12 === 0 && vSearch.results.length < 96) {
    const more = document.createElement("button");
    more.type = "button";
    more.className = "more";
    more.textContent = "Plus de résultats";
    more.addEventListener("click", () => searchClipVideos(vSearch.q, true, more));
    tiles.push(more);
  }
  clipVideos.replaceChildren(...tiles);
}
async function searchClipVideos(q, more = false, moreBtn = null) {
  fetchLinkBtn.disabled = true;
  if (moreBtn) { moreBtn.disabled = true; moreBtn.textContent = "Recherche…"; }
  else fetchLinkBtn.textContent = "Recherche…";
  if (!more) { vSearch = { q, results: [] }; clipVideos.replaceChildren(); }
  try {
    const found = await invoke("clip_search_videos", { query: q, start: vSearch.results.length + 1 });
    const seen = new Set(vSearch.results.map((r) => r.url));
    vSearch.results.push(...found.filter((r) => !seen.has(r.url)));
    saveVSearch();
  } catch (err) {
    toast(String(err), true);
  }
  fetchLinkBtn.disabled = false;
  fetchLinkBtn.textContent = "Chercher";
  const scroll = clipVideos.scrollTop;
  renderVideos();
  clipVideos.scrollTop = scroll;
}
try {
  const saved = JSON.parse(localStorage.getItem(VSEARCH_KEY) || "null");
  if (saved?.q && Array.isArray(saved.results)) {
    vSearch = saved;
    formClip.link.value = saved.q;
    renderVideos();
  }
} catch {}
fetchLinkBtn.addEventListener("click", () => {
  const v = formClip.link.value.trim();
  if (!v) return formClip.link.focus();
  if (isLink(v)) fetchClipLink(v);
  else searchClipVideos(v);
});
$('[data-action="search-gif"]', dlgClip).addEventListener("click", async (e) => {
  const btn = e.currentTarget;
  const q = formClip.gifq.value.trim();
  if (!q) return formClip.gifq.focus();
  btn.disabled = true;
  clipGifs.replaceChildren();
  let results = [];
  try {
    results = await invoke("search_images", { query: q });
  } catch (err) {
    toast(String(err), true);
  }
  btn.disabled = false;
  if (!results.length) {
    const note = document.createElement("p");
    note.className = "hint";
    note.textContent = "Aucun GIF trouvé.";
    return clipGifs.replaceChildren(note);
  }
  clipGifs.replaceChildren(...results.map((r) => {
    const b = document.createElement("button");
    b.type = "button";
    b.setAttribute("role", "option");
    b.title = r.title || "GIF";
    const img = document.createElement("img");
    img.src = r.anim;
    img.alt = r.title || "GIF";
    img.loading = "lazy";
    b.append(img);
    b.addEventListener("click", async () => {
      clipGifs.querySelectorAll("button").forEach((x) => x.classList.toggle("on", x === b));
      b.dataset.progress = "Récupération…"; b.classList.add("loading");
      try {
        showClipMedia(await invoke("clip_from_gif", { url: r.big || r.anim }));
      } catch (err) {
        toast(String(err), true);
      }
      b.classList.remove("loading");
    });
    return b;
  }));
});
// Entrée dans un champ : lance l'action au lieu de fermer la fenêtre.
formClip.addEventListener("keydown", (e) => {
  if (e.key !== "Enter" || e.target.tagName !== "INPUT") return;
  e.preventDefault();
  if (e.target.name === "link") $('[data-action="fetch-link"]', dlgClip).click();
  else if (e.target.name === "gifq") $('[data-action="search-gif"]', dlgClip).click();
  // Entrée après le texte : envoie (avant, il ne se passait rien).
  else if (e.target.name === "title" && clipMedia) $('[data-action="send"]', dlgClip).click();
});
formClip.title.addEventListener("input", clipUi);
formClip.sound.addEventListener("change", () => { stopClipListen(); clipUi(); });
formClip.cvol.addEventListener("input", clipUi);
formClip.cvol.addEventListener("change", () => { if (clipListening) clipListen(); });
// « Écouter » : joue le son dans le casque, tel que le pote l'entendra.
let clipListening = false;
let clipListenTimer = 0;
const listenBtn = $('[data-action="listen"]', dlgClip);
function stopClipListen() {
  if (clipListening) invoke("stop_preview");
  clipListening = false;
  clearTimeout(clipListenTimer);
  listenBtn.textContent = "▶ Écouter";
}
async function clipListen() {
  try {
    await invoke("clip_listen", {
      media: clipMedia.path,
      soundId: clipMedia.kind === "image" ? formClip.sound.value || null : null,
      startS: Number(formClip.start.value) || 0,
      endS: clipMedia.kind === "video" ? Number(formClip.end.value) : null,
      volume: Number(formClip.cvol.value),
    });
    clipListening = true;
    listenBtn.textContent = "■ Arrêter";
    clearTimeout(clipListenTimer);
    clipListenTimer = setTimeout(() => { clipListening = false; listenBtn.textContent = "▶ Écouter"; }, 30_500);
    if (clipMedia.kind === "video") {
      clipVideo.currentTime = Number(formClip.start.value) || 0;
      clipVideo.play().catch(() => {});
    }
  } catch (err) {
    stopClipListen();
    toast(String(err), true);
  }
}
listenBtn.addEventListener("click", () => (clipListening ? stopClipListen() : clipListen()));

// ---------- Découpe de la vidéo : début et fin, 30 s au plus ----------
const CLIP_MAX_S = 30;
let clipTrimFrom = 0;
let clipTrimEnd = null;
const fmtS = (s) => `${Math.floor(s / 60)}:${(s % 60).toFixed(1).padStart(4, "0")}`;
function trimUi() {
  const max = Number(formClip.start.max) || 1;
  const a = Number(formClip.start.value), b = Number(formClip.end.value);
  const sel = $(".clip-range-sel", dlgClip);
  sel.style.left = `${(a / max) * 100}%`;
  sel.style.width = `${((b - a) / max) * 100}%`;
  $("[data-trim-label]", dlgClip).textContent = `${fmtS(a)} → ${fmtS(b)} (${(b - a).toFixed(1)} s)`;
}
let trimLast = [0, CLIP_MAX_S];
let trimDrag = null;
function setTrim(moved) {
  const dur = Number(formClip.start.max) || 0;
  let a = Number(formClip.start.value), b = Number(formClip.end.value);
  if (moved === "start") {
    // Le début emmène tout le passage (même durée), comme on s'y attend.
    const len = Math.max(0.5, trimLast[1] - trimLast[0]);
    b = Math.min(dur, a + len);
    if (b - a < 0.5) a = Math.max(0, b - 0.5);
  } else {
    if (b - a > CLIP_MAX_S) a = b - CLIP_MAX_S;
    if (a > b - 0.5) a = Math.max(0, b - 0.5);
    if (b - a < 0.5) b = Math.min(dur, a + 0.5);
  }
  formClip.start.value = String(a);
  formClip.end.value = String(b);
  trimLast = [a, b];
  trimUi();
}
for (const which of ["start", "end"]) {
  formClip[which].addEventListener("input", () => {
    setTrim(which);
    if (clipMedia?.kind !== "video") return;
    // Pendant qu'on glisse : l'image montre l'endroit de la poignée.
    trimDrag = which;
    clipVideo.pause();
    clipVideo.currentTime = Number(formClip[which].value);
  });
  formClip[which].addEventListener("change", () => {
    trimDrag = null;
    if (clipMedia?.kind !== "video") return;
    clipVideo.currentTime = Number(formClip.start.value);
    clipVideo.play().catch(() => {});
    if (clipListening) clipListen();
  });
}
clipVideo.addEventListener("loadedmetadata", () => {
  const dur = Math.round((clipVideo.duration || 0) * 10) / 10;
  if (!dur || !isFinite(dur)) return;
  for (const r of [formClip.start, formClip.end]) r.max = String(dur);
  const a = Math.min(clipTrimFrom, Math.max(0, dur - 0.5));
  formClip.start.value = String(a);
  const wanted = clipTrimEnd != null && clipTrimEnd > a ? clipTrimEnd : a + CLIP_MAX_S;
  clipTrimEnd = null;
  formClip.end.value = String(Math.min(dur, wanted, a + CLIP_MAX_S));
  trimLast = [a, Number(formClip.end.value)];
  trimUi();
  clipVideo.currentTime = a;
});
// Poignées collées (longue vidéo) : on attrape celle qui est la plus proche.
$(".clip-range", dlgClip).addEventListener("pointermove", (e) => {
  if (e.buttons) return;
  const r = e.currentTarget.getBoundingClientRect();
  const max = Number(formClip.start.max) || 1;
  const x = ((e.clientX - r.left) / r.width) * max;
  const startCloser = Math.abs(x - Number(formClip.start.value)) < Math.abs(x - Number(formClip.end.value));
  formClip.start.style.zIndex = startCloser ? "2" : "1";
  formClip.end.style.zIndex = startCloser ? "1" : "2";
});
// L'aperçu tourne en boucle sur le passage choisi.
clipVideo.addEventListener("timeupdate", () => {
  if (clipMedia?.kind !== "video" || trimDrag) return;
  const a = Number(formClip.start.value), b = Number(formClip.end.value);
  if (clipVideo.currentTime >= b || clipVideo.currentTime < a - 0.3) clipVideo.currentTime = a;
});
$('[data-action="send"]', dlgClip).addEventListener("click", async (e) => {
  const btn = e.currentTarget;
  if (!clipMedia) return;
  if (!room.members.length && formClip.to.value !== "*moi") return toast("Aucun pote en ligne dans le salon pour l'instant : choisis « Tout le salon et moi » pour tester seul", true);
  btn.disabled = true;
  btn.textContent = "Envoi… (jusqu'à 1 min)";
  const to = formClip.to.value || null;
  try {
    await invoke("send_clip", {
      media: clipMedia.path,
      soundId: clipMedia.kind === "image" ? formClip.sound.value || null : null,
      title: formClip.title.value,
      to,
      startS: clipMedia.kind === "video" ? Number(formClip.start.value) || 0 : null,
      endS: clipMedia.kind === "video" ? Number(formClip.end.value) || null : null,
      volume: clipMedia.kind === "video" ? Number(formClip.cvol.value) : null,
    });
  } catch (err) {
    return toast(String(err), true);
  } finally {
    btn.textContent = "Envoyer";
    btn.disabled = !clipMedia;
  }
  const who = room.members.find((m) => m.id === to)?.name;
  toast(to === "*moi" ? "🎬 Extrait envoyé : il va aussi s'afficher chez toi dans quelques secondes" : `🎬 Extrait envoyé ${who ? `à ${who}` : "au salon"}`);
  dlgClip.close();
});
$('[data-action="close"]', dlgClip).addEventListener("click", () => dlgClip.close());
dlgClip.addEventListener("close", () => {
  stopClipListen();
  formClip.cvol.value = "1";
  clipVideo.pause();
  clipVideo.removeAttribute("src");
  clipImg.removeAttribute("src");
  clipMedia = null;
  formClip.title.value = "";
  formClip.title.placeholder = "Ex. : QUAND KEVIN RATE SON TIR";
  clipTrimFrom = 0;
  for (const r of [formClip.start, formClip.end]) r.max = "15";
  formClip.start.value = "0";
  formClip.end.value = "15";
});
invoke("room_status").then((r) => {
  room = r;
  if (state) renderRoom();
}).catch(() => {});

function fillFriendsSettings(s) {
  const f = formSettings;
  f.replay_enabled.checked = s.replay_enabled ?? true;
  f.duck_enabled.checked = !!s.duck_enabled;
  const levels = [0.6, 0.35, 0.15];
  f.duck_level.value = String(levels.reduce((b, o) => (Math.abs(o - (s.duck_level ?? 0.35)) < Math.abs(b - (s.duck_level ?? 0.35)) ? o : b), 0.35));
  updateDuckRow();
  $("#discord-status").textContent = state.discord_heard
    ? "✓ Discord est capté : la voix de tes potes compte pour « Rejouer » et la baisse auto."
    : "Discord n'est pas capté pour l'instant (lancé ? Windows 10 de mai 2020 ou plus récent ?). « Rejouer » garde quand même ta voix et tes sons.";
  f.room_code.value = s.room_code ?? "";
  f.room_name.value = s.room_name ?? "";
  f.room_accept.checked = s.room_accept ?? true;
  f.clips_mode.value = s.clips_mode || "ask";
  renderRoom();
  renderLinkLists();
}
function updateDuckRow() {
  formSettings.duck_level.disabled = !formSettings.duck_enabled.checked;
}
formSettings.duck_enabled.addEventListener("change", updateDuckRow);
$('[data-action="room-new"]', dlgSettings).addEventListener("click", async () => {
  const code = await call("new_room_code");
  if (code) formSettings.room_code.value = code;
  toast("Code créé : copie-le et donne-le à tes potes, puis enregistre");
});
$('[data-action="room-copy"]', dlgSettings).addEventListener("click", async () => {
  const code = formSettings.room_code.value.trim();
  if (!code) return toast("Crée d'abord un code", true);
  try {
    await navigator.clipboard.writeText(code);
    toast("Code copié");
  } catch {
    formSettings.room_code.select();
  }
});

// ---------- Démarrage ----------

(async () => {
  render(await invoke("get_state"));
  if (!state.config.settings.setup_done) openWizard();
  migratePlayCounts();
  checkUpdate();
})();
