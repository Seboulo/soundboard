"use strict";

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $ = (sel, root = document) => root.querySelector(sel);

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
  renderWarnings();
  renderGrid();
}

function renderStatus() {
  const pill = $("#cable-status");
  const s = state.config.settings;
  const ok = s.virtual_device && state.devices.outputs.includes(s.virtual_device);
  pill.className = "pill " + (ok ? "ok" : "ko");
  pill.textContent = ok ? "● Envoi vers Discord actif" : "● Micro virtuel non configuré";
}

function renderWarnings() {
  const el = $("#warnings");
  el.textContent = state.warnings.join("\n");
  el.hidden = state.warnings.length === 0;
}

function renderGrid() {
  const grid = $("#grid");
  const q = $("#search").value.trim().toLowerCase();
  const sounds = state.config.sounds.filter((s) => !q || s.name.toLowerCase().includes(q));
  grid.replaceChildren(...sounds.map(padElement));
  $("#empty").hidden = state.config.sounds.length > 0;
  renderPlaying();
}

// Sons en cours, envoyés par le moteur audio ~10 fois par seconde.
let playing = [];

function renderPlaying() {
  const byId = new Map(playing.map((p) => [p.id, p]));
  document.querySelectorAll(".pad").forEach((pad) => {
    const p = byId.get(pad.dataset.id);
    pad.classList.toggle("playing", !!p);
    const ratio = p && p.duration_ms ? Math.min(1, p.elapsed_ms / p.duration_ms) : 0;
    pad.style.setProperty("--progress", ratio);
  });
}

function padElement(sound) {
  const pad = document.createElement("button");
  pad.type = "button";
  pad.className = "pad";
  pad.dataset.id = sound.id;
  pad.style.setProperty("--pad-color", sound.color);

  const name = document.createElement("span");
  name.className = "name";
  name.textContent = sound.name;
  const key = document.createElement("span");
  key.className = "key";
  key.textContent = hotkeyLabel(sound.hotkey) || "—";
  const edit = document.createElement("span");
  edit.className = "edit";
  edit.textContent = "✎";
  edit.title = "Modifier";
  const progress = document.createElement("span");
  progress.className = "progress";
  pad.append(name, key, edit, progress);

  pad.addEventListener("click", (e) => {
    if (reorder.justDropped) return;
    if (e.target === edit) return openSoundDialog(sound);
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
  if (e.button !== 0) return;
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
  dlgSound.returnValue = "";
  dlgSound.showModal();
  openTrim(sound);
}

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
    ctx.fillStyle = inside ? editing?.color ?? "#5865f2" : "#4e5058";
    ctx.fillRect(px, (h - bh) / 2, Math.max(1, w / n - 1), bh);
  }
  if (!n) {
    ctx.fillStyle = css.getPropertyValue("--muted");
    ctx.font = "26px Segoe UI, sans-serif";
    ctx.fillText("Chargement de la forme d'onde…", 20, h / 2 + 8);
  }
  ctx.fillStyle = "rgba(0, 0, 0, 0.45)";
  ctx.fillRect(0, 0, xs, h);
  ctx.fillRect(xe, 0, w - xe, h);
  for (const hx of [xs, xe]) {
    ctx.fillStyle = "#ffffff";
    ctx.fillRect(hx - 2, 0, 4, h);
    ctx.fillRect(hx - 9, 0, 18, 14);
    ctx.fillRect(hx - 9, h - 14, 18, 14);
  }
  $("#trim-info").textContent =
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

$('[data-action="preview"]', dlgSound).addEventListener("click", () => {
  if (!editing) return;
  call("preview_sound", {
    id: editing.id,
    startMs: trim.start,
    endMs: trim.end >= trim.duration ? null : trim.end,
    volume: Number(formSound.volume.value),
  });
});

function trimValues() {
  if (!trim.duration) return { start_ms: editing.start_ms ?? 0, end_ms: editing.end_ms ?? null };
  return { start_ms: trim.start, end_ms: trim.end >= trim.duration ? null : trim.end };
}

dlgSound.addEventListener("close", () => {
  if (dlgSound.returnValue !== "save" || !editing) return;
  call("update_sound", {
    sound: {
      ...editing,
      name: formSound.sound_name.value.trim() || editing.name,
      hotkey: formSound.hotkey.dataset.hotkey || null,
      volume: Number(formSound.volume.value),
      color: formSound.color.value,
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
setupHotkeyInput(formSettings.stop_hotkey);
bindRangeLabels(formSettings);
enterSaves(dlgSettings);

function openSettings() {
  const s = state.config.settings;
  const d = state.devices;
  fillSelect(formSettings.monitor_device, d.outputs, s.monitor_device, `Par défaut (${d.default_output ?? "aucun"})`);
  fillSelect(formSettings.virtual_device, d.outputs, s.virtual_device, "Aucun (sons uniquement dans le casque)");
  fillSelect(formSettings.mic_device, d.inputs, s.mic_device, `Par défaut (${d.default_input ?? "aucun"})`);
  formSettings.mic_enabled.checked = s.mic_enabled;
  for (const k of ["mic_volume", "master_volume", "monitor_volume"]) {
    formSettings[k].value = s[k];
    formSettings[k].update();
  }
  formSettings.stop_previous.checked = s.stop_previous;
  formSettings.normalize.checked = s.normalize;
  formSettings.launch_at_startup.checked = s.launch_at_startup;
  formSettings.close_to_tray.checked = s.close_to_tray;
  setHotkeyInput(formSettings.stop_hotkey, s.stop_hotkey);
  dlgSettings.returnValue = "";
  dlgSettings.showModal();
}

dlgSettings.addEventListener("close", () => {
  if (dlgSettings.returnValue !== "save") return;
  const f = formSettings;
  saveSettings({
    monitor_device: f.monitor_device.value || null,
    virtual_device: f.virtual_device.value || null,
    mic_device: f.mic_device.value || null,
    mic_enabled: f.mic_enabled.checked,
    mic_volume: Number(f.mic_volume.value),
    master_volume: Number(f.master_volume.value),
    monitor_volume: Number(f.monitor_volume.value),
    stop_previous: f.stop_previous.checked,
    normalize: f.normalize.checked,
    launch_at_startup: f.launch_at_startup.checked,
    close_to_tray: f.close_to_tray.checked,
    stop_hotkey: f.stop_hotkey.dataset.hotkey || null,
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

function openWizard() {
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
  render(await invoke("get_state"));
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

// ---------- Barre du haut ----------

$("#btn-add").addEventListener("click", () => call("pick_sounds"));
$("#btn-stop").addEventListener("click", () => call("stop_all"));
$("#btn-settings").addEventListener("click", openSettings);
$("#cable-status").addEventListener("click", () =>
  state.devices.vb_cable ? openSettings() : openWizard()
);
$("#search").addEventListener("input", renderGrid);

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
  playing = event.payload ?? [];
  renderPlaying();
});
// Analyse des anciens sons terminée en arrière-plan.
listen("state-changed", async () => render(await invoke("get_state")));

// ---------- Recherche en ligne ----------

const dlgOnline = $("#dlg-online");
const onlineForm = $("#online-form");
const results = $("#online-results");
const moreBtn = $("#online-more");
const online = { query: "", page: 1, audio: new Audio(), playingBtn: null };

$("#btn-online").addEventListener("click", () => {
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
    items = await invoke("search_online", { query: online.query, page: online.page });
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
})();
