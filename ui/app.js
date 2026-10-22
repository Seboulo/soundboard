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
  pad.append(name, key, edit);

  pad.addEventListener("click", (e) => {
    if (reorder.justDropped) return;
    if (e.target === edit) return openSoundDialog(sound);
    flash(sound.id);
    call("play_sound", { id: sound.id });
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

// ---------- Démarrage ----------

(async () => {
  render(await invoke("get_state"));
  if (!state.config.settings.setup_done) openWizard();
})();
