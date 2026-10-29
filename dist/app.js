"use strict";
/* openPS front end. Rules: no innerHTML anywhere (names and messages come from the network and the console);
   every control is reachable by keyboard; every state change is announced through a live region. */

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

// ---- tiny helpers ---------------------------------------------------------------------------------------------------
const $ = (id) => document.getElementById(id);
const $$ = (sel, root = document) => [...root.querySelectorAll(sel)];
const svg = (name) => {
  const s = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  s.setAttribute("aria-hidden", "true");
  const u = document.createElementNS("http://www.w3.org/2000/svg", "use");
  u.setAttribute("href", `#i-${name}`);
  s.append(u);
  return s;
};
const el = (tag, props = {}, ...kids) => {
  const { dataset, attrs, ...rest } = props;
  const n = Object.assign(document.createElement(tag), rest);
  if (dataset) Object.assign(n.dataset, dataset);
  if (attrs) for (const [k, v] of Object.entries(attrs)) n.setAttribute(k, v);
  for (const k of kids) if (k != null) n.append(k);
  return n;
};
const errText = (e) => (typeof e === "string" ? e : e?.message || JSON.stringify(e));
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// ---- state ----------------------------------------------------------------------------------------------------------
const S = {
  info: { version: "", psn: false, demo: false, hevc: false, data_dir: "" },
  consoles: [],
  found: new Map(), // paired console id -> discovery result
  nearby: [], // discovered, not paired
  session: { state: "idle" },
  psn: null,
  settings: null,
  activeId: null,
  scanning: false,
  scannedOnce: false,
  waking: new Set(),
  view: "consoles",
};
const SESSION_BUSY = ["connecting", "testing_network", "handshaking", "connected"];
const busy = () => SESSION_BUSY.includes(S.session.state);

// ---- activity log ---------------------------------------------------------------------------------------------------
const LOG = [];
function log(msg, level = "info") {
  LOG.push({ t: new Date(), msg: String(msg), level });
  if (LOG.length > 500) LOG.shift();
  if (S.view === "activity") renderLog();
}
function renderLog() {
  const ol = $("log");
  ol.replaceChildren();
  if (!LOG.length) {
    ol.append(el("li", { className: "log-empty", textContent: "Nothing yet." }));
    return;
  }
  for (const e of LOG) {
    ol.append(el("li", { dataset: { level: e.level } }, el("time", { textContent: e.t.toLocaleTimeString(), dateTime: e.t.toISOString() }), el("span", { textContent: e.msg })));
  }
  ol.scrollTop = ol.scrollHeight;
}

// ---- toasts ---------------------------------------------------------------------------------------------------------
function toast(message, kind = "info", ms = 4500) {
  const box = $("toasts");
  const t = el("div", { className: "toast", dataset: { kind } }, svg(kind === "error" ? "alert" : kind === "ok" ? "check" : "info"), el("p", { textContent: message }));
  const close = el("button", { type: "button", attrs: { "aria-label": "Dismiss" } }, svg("close"));
  const remove = () => t.remove();
  close.onclick = remove;
  t.append(close);
  box.append(t);
  while (box.children.length > 4) box.firstChild.remove();
  if (kind !== "error") setTimeout(remove, ms);
  log(message, kind === "error" ? "error" : kind === "ok" ? "ok" : "info");
}

async function call(cmd, args) {
  try {
    return [null, await invoke(cmd, args)];
  } catch (e) {
    return [errText(e), null];
  }
}

function setLoading(btn, on) {
  btn.classList.toggle("loading", on);
  btn.disabled = on;
  btn.setAttribute("aria-busy", String(on));
}

// ---- navigation -----------------------------------------------------------------------------------------------------
const TITLES = { consoles: "Consoles", settings: "Settings", activity: "Activity", about: "About" };
function showView(name) {
  S.view = name;
  document.body.dataset.view = name;
  for (const v of $$(".view")) v.hidden = v.id !== `view-${name}`;
  for (const b of $$(".nav-btn")) {
    if (b.dataset.nav === name) b.setAttribute("aria-current", "page");
    else b.removeAttribute("aria-current");
  }
  $("page-title").textContent = TITLES[name];
  if (name === "activity") renderLog();
  if (name === "settings") renderSettings();
  if (name === "about") renderAbout();
  if (name === "consoles") refreshConsoles();
}
for (const b of $$(".nav-btn")) b.addEventListener("click", () => showView(b.dataset.nav));

// ---- consoles -------------------------------------------------------------------------------------------------------
const STATUS_TEXT = { ready: "Ready", standby: "In rest mode", other: "Busy", unknown: "Not found on network" };
function statusOf(c) {
  if (S.waking.has(c.id)) return "waking";
  const f = S.found.get(c.id);
  return f ? f.status : S.scannedOnce ? "unknown" : "checking";
}

async function refreshConsoles() {
  const [e, list] = await call("list_consoles");
  if (e) return toast(`Could not read your consoles: ${e}`, "error");
  S.consoles = list;
  renderConsoles();
}

function renderConsoles() {
  const grid = $("console-grid");
  grid.replaceChildren();
  const none = S.consoles.length === 0;
  $("empty").hidden = !none || S.nearby.length > 0 || !S.scannedOnce;
  $("consoles-sub").textContent = none ? "Paired consoles appear here." : `${S.consoles.length} paired · tap Connect to start playing`;
  if (none && !S.scannedOnce) {
    for (let i = 0; i < 2; i++) grid.append(el("div", { className: "card console skeleton", attrs: { "aria-hidden": "true" } }));
  }
  for (const c of S.consoles) grid.append(consoleCard(c));
  const nb = $("nearby-grid");
  nb.replaceChildren();
  for (const d of S.nearby) nb.append(nearbyCard(d));
  $("nearby-wrap").hidden = S.nearby.length === 0;
  if (none && S.nearby.length) $("empty").hidden = true;
}

function consoleCard(c) {
  const st = statusOf(c);
  const f = S.found.get(c.id);
  const isActive = busy() && S.activeId === c.id;
  const status = el("span", { className: "status" }, el("span", { className: "dot" }), el("span", { textContent: st === "waking" ? "Waking up…" : st === "checking" ? "Checking…" : STATUS_TEXT[st] || st }));
  const addr = f?.addr || c.host;
  const connect = el("button", { type: "button", className: "btn btn-primary", disabled: busy() || S.waking.has(c.id) });
  const label = st === "standby" ? "Wake & connect" : "Connect";
  connect.append(svg(st === "standby" ? "power" : "play"), el("span", { textContent: isActive ? "In session" : label }));
  connect.onclick = () => startSession(c, st === "standby");
  const forget = el("button", { type: "button", className: "btn btn-ghost btn-icon", disabled: busy(), attrs: { "aria-label": `Forget ${c.nickname || c.id}`, title: "Forget this console" } }, svg("trash"));
  forget.onclick = () => forgetConsole(c);
  return el(
    "article",
    { className: "card console", dataset: { status: st }, attrs: { "aria-label": c.nickname || c.id } },
    el("div", { className: "console-head" },
      el("div", { className: "tile" }, svg(c.device === "ps4" ? "ps4" : "ps5")),
      el("div", {}, el("div", { className: "console-name", textContent: c.nickname || c.id }), el("div", { className: "console-meta", textContent: `${c.device === "ps4" ? "PlayStation 4" : "PlayStation 5"}${addr ? " · " + addr : ""}` }))),
    status,
    el("div", { className: "console-actions" }, connect, forget),
  );
}

function nearbyCard(d) {
  const pair = el("button", { type: "button", className: "btn btn-primary" }, svg("plus"), el("span", { textContent: "Pair" }));
  pair.onclick = () => openPair(d);
  return el("article", { className: "card console", dataset: { status: d.status } },
    el("div", { className: "console-head" },
      el("div", { className: "tile" }, svg(d.device === "ps4" ? "ps4" : "ps5")),
      el("div", {}, el("div", { className: "console-name", textContent: d.name || d.host_id }), el("div", { className: "console-meta", textContent: `${d.addr} · ${STATUS_TEXT[d.status] || d.status}` }))),
    el("div", { className: "console-actions" }, pair));
}

async function scan(explicit = false) {
  if (S.scanning) return;
  S.scanning = true;
  const btn = $("refresh-btn");
  if (explicit) setLoading(btn, true);
  const [e, found] = await call("scan");
  S.scanning = false;
  if (explicit) setLoading(btn, false);
  if (e) {
    S.scannedOnce = true;
    renderConsoles();
    if (explicit) toast(`Scan failed: ${e}`, "error");
    else log(`scan failed: ${e}`, "error");
    return;
  }
  S.scannedOnce = true;
  S.found = new Map(found.filter((d) => d.registered_id).map((d) => [d.registered_id, d]));
  S.nearby = found.filter((d) => !d.registered_id);
  log(`scan: ${found.length} console${found.length === 1 ? "" : "s"} found`);
  renderConsoles();
  if (!$("pair-dialog").hidden && $("pair-dialog").open) renderPairList();
}

async function startSession(c, needsWake) {
  S.activeId = c.id;
  hideBanner();
  if (needsWake) {
    S.waking.add(c.id);
    renderConsoles();
    toast(`Waking ${c.nickname || "console"}…`);
    const [e, ok] = await call("wake", { id: c.id });
    S.waking.delete(c.id);
    renderConsoles();
    if (e) return showBanner(`Could not wake the console: ${e}`, c);
    if (!ok) return showBanner("The console did not wake up in time. Check that Remote Play and “Turn on PS5 from network” are enabled.", c);
  }
  const [e] = await call("connect", { id: c.id });
  if (e) showBanner(e, c);
}

async function forgetConsole(c) {
  const name = c.nickname || c.id;
  const ok = await confirmDialog(`Forget ${name}?`, "This removes the pairing from this computer. You will need a new 8-digit code to pair it again.", "Forget");
  if (!ok) return;
  const [e] = await call("forget", { id: c.id });
  if (e) return toast(`Could not forget the console: ${e}`, "error");
  toast(`${name} was removed`, "ok");
  await refreshConsoles();
}

$("refresh-btn").onclick = () => scan(true);
$("add-btn").onclick = () => openPair();
$("empty-add").onclick = () => openPair();

// ---- banner ---------------------------------------------------------------------------------------------------------
function showBanner(msg, retryFor) {
  const b = $("banner");
  b.replaceChildren(svg("alert"), el("span", { textContent: msg }));
  if (retryFor) {
    const r = el("button", { type: "button", className: "btn btn-sm" }, el("span", { textContent: "Try again" }));
    r.onclick = () => startSession(retryFor, false);
    b.append(r);
  }
  const x = el("button", { type: "button", className: "btn btn-ghost btn-sm btn-icon", attrs: { "aria-label": "Dismiss" } }, svg("close"));
  x.onclick = hideBanner;
  b.append(x);
  b.hidden = false;
  log(msg, "error");
}
function hideBanner() {
  $("banner").hidden = true;
}

// ---- confirm dialog -------------------------------------------------------------------------------------------------
function confirmDialog(title, body, okLabel) {
  const d = $("confirm-dialog");
  $("confirm-title").textContent = title;
  $("confirm-body").textContent = body;
  $("confirm-ok").textContent = okLabel;
  return new Promise((resolve) => {
    d.returnValue = "cancel";
    d.addEventListener("close", () => resolve(d.returnValue === "ok"), { once: true });
    d.showModal();
  });
}

// ---- PSN ------------------------------------------------------------------------------------------------------------
async function loadPsn() {
  const [, a] = await call("psn_account");
  S.psn = a || null;
  const chip = $("psn-chip"), btn = $("psn-btn");
  chip.classList.toggle("chip-ok", !!a);
  $("psn-text").textContent = !S.info.psn ? "PSN unavailable" : a ? a.online_id : "PSN: signed out";
  btn.hidden = !S.info.psn;
  btn.textContent = a ? "Sign out" : "Sign in";
  updatePairGate();
}
$("psn-btn").onclick = async () => {
  const btn = $("psn-btn");
  setLoading(btn, true);
  const [e] = await call(S.psn ? "psn_sign_out" : "psn_sign_in");
  setLoading(btn, false);
  if (e) toast(`PSN: ${e}`, "error");
};

// ---- pairing wizard -------------------------------------------------------------------------------------------------
const P = { step: "find", selected: null, device: "ps5", busy: false };
const STEPS = ["find", "pin", "done"];

function makePin() {
  const box = $("pin");
  box.replaceChildren();
  for (let i = 0; i < 8; i++) {
    const inp = el("input", { type: "text", inputMode: "numeric", maxLength: 1, autocomplete: "one-time-code", attrs: { "aria-label": `Digit ${i + 1} of 8`, pattern: "[0-9]" } });
    inp.addEventListener("input", () => {
      inp.value = inp.value.replace(/\D/g, "").slice(0, 1);
      inp.removeAttribute("aria-invalid");
      if (inp.value && i < 7) box.children[i + 1].focus();
      updatePairGate();
    });
    inp.addEventListener("keydown", (ev) => {
      if (ev.key === "Backspace" && !inp.value && i > 0) { box.children[i - 1].focus(); box.children[i - 1].value = ""; ev.preventDefault(); updatePairGate(); }
      else if (ev.key === "ArrowLeft" && i > 0) { box.children[i - 1].focus(); ev.preventDefault(); }
      else if (ev.key === "ArrowRight" && i < 7) { box.children[i + 1].focus(); ev.preventDefault(); }
    });
    inp.addEventListener("paste", (ev) => {
      const digits = (ev.clipboardData?.getData("text") || "").replace(/\D/g, "").slice(0, 8);
      if (!digits) return;
      ev.preventDefault();
      [...box.children].forEach((c, k) => (c.value = digits[k] || ""));
      box.children[Math.min(digits.length, 7)].focus();
      updatePairGate();
    });
    inp.addEventListener("focus", () => inp.select());
    box.append(inp);
  }
}
const pinValue = () => $$("input", $("pin")).map((i) => i.value).join("");

function wizState() {
  const idx = STEPS.indexOf(P.step);
  for (const li of $$(".wiz li")) {
    const i = STEPS.indexOf(li.dataset.wiz);
    li.dataset.state = i < idx ? "done" : i === idx ? "active" : "";
  }
  for (const b of $$("#pair-form .dlg-body")) b.hidden = b.dataset.step !== P.step;
  $("pair-back").hidden = P.step !== "pin";
  const next = $("pair-next");
  next.textContent = P.step === "find" ? "Continue" : P.step === "pin" ? "Pair" : "Done";
  $("pair-title").textContent = P.step === "done" ? "All set" : "Add a console";
  updatePairGate();
}

function updatePairGate() {
  if (!$("pair-dialog").open) return;
  const next = $("pair-next");
  if (P.busy) return;
  if (P.step === "pin") {
    const needPsn = P.device === "ps5" && !S.psn;
    $("psn-need").hidden = !needPsn;
    const t = $("psn-need").querySelector(".small");
    t.textContent = S.info.psn ? "Sign in once; your account is only used to identify you to your own console." : "This build was made without PSN sign-in, so PS5 consoles cannot be paired.";
    $("pair-signin").hidden = !S.info.psn;
    next.disabled = needPsn || pinValue().length !== 8;
  } else next.disabled = false;
}

function renderPairList() {
  const list = $("pair-list");
  list.replaceChildren();
  if (!S.nearby.length) {
    list.append(el("div", { className: "pick-empty", textContent: S.scanning ? "Looking for consoles…" : "No unpaired consoles found. Enter its address below." }));
    return;
  }
  S.nearby.forEach((d, i) => {
    const b = el("button", { type: "button", attrs: { role: "radio", "aria-checked": String(P.selected?.host_id === d.host_id), tabindex: i === 0 ? "0" : "-1" } },
      el("div", { className: "tile tile-sm" }, svg(d.device === "ps4" ? "ps4" : "ps5")),
      el("div", {}, el("div", { className: "console-name", textContent: d.name || d.host_id }), el("div", { className: "console-meta", textContent: `${d.addr} · ${STATUS_TEXT[d.status] || d.status}` })));
    b.onclick = () => selectNearby(d);
    list.append(b);
  });
  rovingFocus(list);
}
function selectNearby(d) {
  P.selected = d;
  setDevice(d.device);
  $("pair-host").value = d.addr;
  renderPairList();
}
function setDevice(dev) {
  P.device = dev;
  buildSeg($("pair-device"), [["ps5", "PS5"], ["ps4", "PS4"]], dev, (v) => { P.device = v; updatePairGate(); });
}

async function openPair(preselect) {
  const d = $("pair-dialog");
  P.step = "find"; P.selected = null; P.busy = false; P.device = "ps5";
  $("pair-host").value = "";
  $("pair-error").hidden = true;
  makePin();
  setDevice("ps5");
  if (preselect) selectNearby(preselect);
  renderPairList();
  wizState();
  d.showModal();
  $("pair-host").focus();
  await scan(false);
  if (preselect === undefined && S.nearby.length === 1 && !$("pair-host").value) selectNearby(S.nearby[0]);
  renderPairList();
}

function closePair() {
  const d = $("pair-dialog");
  if (d.open) d.close();
}
for (const b of $$("[data-close]")) b.addEventListener("click", closePair);
$("pair-back").onclick = () => { P.step = "find"; wizState(); };
$("pair-signin").onclick = async () => {
  const btn = $("pair-signin");
  setLoading(btn, true);
  const [e] = await call("psn_sign_in");
  setLoading(btn, false);
  if (e) toast(`PSN: ${e}`, "error");
};
const HOST_RE = /^[A-Za-z0-9._:\-\[\]]{1,253}$/;
$("pair-form").addEventListener("submit", async (ev) => {
  ev.preventDefault();
  if (P.busy) return;
  if (P.step === "find") {
    const host = $("pair-host").value.trim();
    if (!host || !HOST_RE.test(host)) {
      $("pair-host").setAttribute("aria-invalid", "true");
      $("pair-host").focus();
      return toast("Choose a console from the list or enter a valid address.", "error");
    }
    $("pair-host").removeAttribute("aria-invalid");
    P.step = "pin";
    wizState();
    $("pin").children[0].focus();
  } else if (P.step === "pin") {
    const pin = pinValue();
    if (pin.length !== 8) return;
    P.busy = true;
    const next = $("pair-next");
    setLoading(next, true);
    $("pair-error").hidden = true;
    const [e, rec] = await call("register", { host: $("pair-host").value.trim(), device: P.device, pin });
    P.busy = false;
    setLoading(next, false);
    if (e) {
      const er = $("pair-error");
      er.textContent = e;
      er.hidden = false;
      const box = $("pin");
      box.classList.remove("shake"); void box.offsetWidth; box.classList.add("shake");
      for (const i of $$("input", box)) { i.value = ""; i.setAttribute("aria-invalid", "true"); }
      box.children[0].focus();
      updatePairGate();
      return;
    }
    $("done-title").textContent = `${rec.nickname || "Your console"} is paired`;
    $("done-sub").textContent = "You can connect to it any time from the Consoles screen.";
    P.step = "done";
    wizState();
    log(`paired ${rec.nickname || rec.id}`, "ok");
    refreshConsoles().then(() => scan(false));
  } else closePair();
});

// ---- segmented controls (radio groups with arrow-key navigation) ------------------------------------------------------
function rovingFocus(group) {
  const items = $$('[role="radio"]', group).filter((b) => b.getAttribute("aria-disabled") !== "true");
  if (!items.length) return;
  const checked = items.find((b) => b.getAttribute("aria-checked") === "true") || items[0];
  for (const b of $$('[role="radio"]', group)) b.tabIndex = b === checked ? 0 : -1;
  group.onkeydown = (ev) => {
    const k = ev.key;
    if (!["ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown"].includes(k)) return;
    const i = items.indexOf(document.activeElement);
    if (i < 0) return;
    const n = items[(i + (k === "ArrowLeft" || k === "ArrowUp" ? items.length - 1 : 1)) % items.length];
    n.focus();
    n.click();
    ev.preventDefault();
  };
}
function buildSeg(box, options, value, onChange, disabled = () => false) {
  box.replaceChildren();
  for (const [v, label] of options) {
    const b = el("button", { type: "button", textContent: label, attrs: { role: "radio", "aria-checked": String(v === value), "aria-disabled": String(disabled(v)) } });
    b.onclick = () => {
      if (b.getAttribute("aria-disabled") === "true") return;
      for (const o of $$('[role="radio"]', box)) o.setAttribute("aria-checked", String(o === b));
      rovingFocus(box);
      onChange(v);
    };
    box.append(b);
  }
  rovingFocus(box);
}

// ---- settings -------------------------------------------------------------------------------------------------------
const WIDTH_FOR = { 360: 640, 540: 960, 720: 1280, 1080: 1920 };
async function renderSettings() {
  const [e, s] = await call("get_settings");
  if (e) return toast(`Could not load settings: ${e}`, "error");
  S.settings = s;
  drawSettings();
}
function drawSettings() {
  const s = S.settings;
  const hevcOk = S.info.hevc;
  buildSeg(document.querySelector('[data-setting="height"]'), [[360, "360p"], [540, "540p"], [720, "720p"], [1080, "1080p"]], s.height, (v) => save({ height: v }));
  buildSeg(document.querySelector('[data-setting="fps"]'), [[30, "30 fps"], [60, "60 fps"]], s.fps, (v) => save({ fps: v }));
  buildSeg(document.querySelector('[data-setting="bitrate"]'), [[-1, "Auto"], [2000, "2 Mbit/s"], [6000, "6 Mbit/s"], [10000, "10 Mbit/s"], [15000, "15 Mbit/s"]], s.bitrate, (v) => save({ bitrate: v }));
  buildSeg(document.querySelector('[data-setting="codec"]'), [["hevc", "HEVC (H.265)"], ["h264", "H.264"]], s.hevc ? "hevc" : "h264", (v) => save({ hevc: v === "hevc", ...(v === "hevc" ? {} : { hdr: false }) }), (v) => v === "hevc" && !hevcOk);
  $("codec-hint").textContent = hevcOk ? "HEVC looks better at the same bitrate. H.264 is the compatible fallback." : "This Mac cannot decode HEVC in hardware, so H.264 is used.";
  const vol = $("volume");
  vol.value = String(s.volume ?? 100);
  paintVolume();
  const hdr = $("hdr-switch");
  hdr.setAttribute("aria-checked", String(!!s.hdr));
  hdr.setAttribute("aria-disabled", String(!s.hevc));
  hdr.onclick = () => { if (s.hevc) save({ hdr: !s.hdr }); };
}
function paintVolume() {
  const v = $("volume");
  v.style.setProperty("--fill", `${v.value}%`);
  $("volume-out").textContent = `${v.value}%`;
  v.setAttribute("aria-valuetext", `${v.value} percent`);
}
$("volume").addEventListener("input", () => {
  paintVolume();
  call("set_volume", { percent: Number($("volume").value) }); // instant feedback, even mid-session
});
$("volume").addEventListener("change", () => save({ volume: Number($("volume").value) }));

async function save(patch) {
  const next = { ...S.settings, ...patch };
  next.width = WIDTH_FOR[next.height];
  const [e] = await call("set_settings", { settings: next });
  if (e) {
    toast(`Not saved: ${e}`, "error");
    return renderSettings();
  }
  S.settings = next;
  drawSettings();
  toast("Saved", "ok", 1600);
}

// ---- about ----------------------------------------------------------------------------------------------------------
function renderAbout() {
  $("about-version").textContent = `Version ${S.info.version}`;
  const rows = [
    ["Mode", S.info.demo ? "Demo (built-in pretend console)" : "Live"],
    ["PSN sign-in", S.info.psn ? "Available" : "Not included in this build"],
    ["HEVC decoding", S.info.hevc ? "Hardware" : "Not available"],
    ["Data folder", S.info.data_dir || "—"],
    ["Privacy", "Everything the app stores (pairings, settings, keys, logs) lives in the data folder above; there is no keychain use and no cache. macOS itself creates a few empty helper folders for the web view in its temp folder; they hold no data."],
  ];
  const dl = $("about-facts");
  dl.replaceChildren();
  for (const [k, v] of rows) dl.append(el("div", {}, el("dt", { textContent: k }), el("dd", { textContent: v })));
}

// ---- session stage --------------------------------------------------------------------------------------------------
const STEP_OF = { connecting: 0, testing_network: 1, handshaking: 2, connected: 3 };
let hudTimer = 0;
function consoleName() {
  const c = S.consoles.find((x) => x.id === S.activeId);
  return c ? c.nickname || c.id : "your console";
}
function renderSession() {
  const s = S.session;
  const stage = $("stage");
  const active = busy();
  stage.hidden = !active;
  const streaming = s.state === "connected";
  document.body.dataset.stream = streaming ? "on" : "off";
  $("connecting").hidden = streaming || !active;
  $("hud").hidden = !streaming;
  if (active && !streaming) {
    const idx = STEP_OF[s.state] ?? 0;
    $("connecting-title").textContent = `Connecting to ${consoleName()}`;
    $("connecting-sub").textContent = s.state === "handshaking" && s.attempt > 1 ? `Still trying… (attempt ${s.attempt})` : "This usually takes a few seconds.";
    for (const li of $$("#steps li")) {
      const i = Number(li.dataset.step);
      li.dataset.state = i < idx ? "done" : i === idx ? "active" : "";
    }
  }
  if (streaming) {
    $("hud-name").textContent = consoleName();
    muted = false;
    paintMute();
    wakeHud();
    pollAudio();
    clearInterval(audioTimer);
    audioTimer = setInterval(pollAudio, 3000);
  } else {
    clearTimeout(hudTimer);
    clearInterval(audioTimer);
  }
  renderConsoles();
}
function wakeHud() {
  const hud = $("hud");
  hud.classList.remove("idle");
  document.body.classList.remove("hud-idle");
  clearTimeout(hudTimer);
  hudTimer = setTimeout(() => {
    if ($("hud").querySelector(".hud-bar:hover")) return wakeHud(); // never hide controls under the pointer
    if (!$("hud").contains(document.activeElement) && S.session.state === "connected") {
      hud.classList.add("idle");
      document.body.classList.add("hud-idle");
    }
  }, 3000);
}
for (const ev of ["mousemove", "pointerdown", "keydown", "touchstart"]) document.addEventListener(ev, () => S.session.state === "connected" && wakeHud(), { passive: true });

// "no session is running" just means the session already ended before the click arrived: nothing to report
const notRunning = (e) => /no session/i.test(String(e));
$("cancel-connect").onclick = async () => {
  const [e] = await call("disconnect");
  if (e && !notRunning(e)) toast(e, "error");
};
$("end-btn").onclick = async () => {
  setLoading($("end-btn"), true);
  const [e] = await call("disconnect");
  setLoading($("end-btn"), false);
  if (e && !notRunning(e)) toast(e, "error");
};

async function setFullscreen(on) {
  try {
    await window.__TAURI__.window.getCurrentWindow().setFullscreen(on);
    S.fullscreen = on;
  } catch (e) {
    toast(`Full screen is not available: ${errText(e)}`, "error");
  }
}
let muted = false;
function paintMute() {
  const b = $("mute-btn");
  b.setAttribute("aria-pressed", String(muted));
  b.setAttribute("aria-label", muted ? "Unmute" : "Mute");
  b.title = muted ? "Unmute (M)" : "Mute (M)";
  b.replaceChildren(svg(muted ? "mute" : "volume"));
}
async function toggleMute() {
  muted = !muted;
  paintMute();
  const restore = S.settings?.volume ?? 100;
  const [e] = await call("set_volume", { percent: muted ? 0 : restore });
  if (e) toast(e, "error");
}
$("mute-btn").onclick = toggleMute;
let audioTimer = 0;
async function pollAudio() {
  const [, a] = await call("audio_status");
  const warn = $("hud-audio-warn");
  const bad = a && (!a.available || a.problem);
  warn.hidden = !bad;
  if (bad) warn.title = a.problem || "audio is not available";
}

$("fs-btn").onclick = () => setFullscreen(!S.fullscreen);
document.addEventListener("keydown", (ev) => {
  if (S.session.state !== "connected" || ev.metaKey || ev.ctrlKey || ev.altKey) return;
  if (ev.key === "f" || ev.key === "F") { ev.preventDefault(); setFullscreen(!S.fullscreen); }
  else if (ev.key === "m" || ev.key === "M") { ev.preventDefault(); toggleMute(); }
  else if (ev.key === "Escape" && S.fullscreen && !$("hud").classList.contains("idle")) setFullscreen(false);
});

function onStats(s) {
  const box = $("hud-stats");
  box.replaceChildren(el("span", { textContent: `${Math.round(s.fps)} fps` }), el("span", { textContent: `${(s.kbps / 1000).toFixed(1)} Mbit/s` }));
  if (s.video_frames_dropped > 0) box.append(el("span", { className: "warn", textContent: `${s.video_frames_dropped} dropped` }));
}

function onSessionEvent(e) {
  const prev = S.session.state;
  S.session = e;
  const detail = e.reason ? `: ${e.reason}` : e.attempt ? ` (attempt ${e.attempt})` : "";
  log(`session ${e.state}${detail}`, e.state === "failed" ? "error" : "info");
  renderSession();
  if (e.state === "failed") {
    const c = S.consoles.find((x) => x.id === S.activeId);
    showBanner(`The connection failed: ${e.reason}`, c);
    if (document.fullscreenElement || S.fullscreen) setFullscreen(false);
  } else if (e.state === "closed" && prev === "connected") {
    toast(e.reason ? `Session ended: ${e.reason}` : "Session ended", e.reason ? "error" : "info");
    if (S.fullscreen) setFullscreen(false);
  }
}

// ---- events from the core -------------------------------------------------------------------------------------------------
listen("openps://event", ({ payload: e }) => {
  switch (e.type) {
    case "session": onSessionEvent(e); break;
    case "consoles_changed": refreshConsoles(); break;
    case "psn_changed": loadPsn(); break;
    case "stats": onStats(e); break;
    case "control":
      if (e.kind === "psn_error") toast(`PSN sign-in did not complete: ${e.detail}`, "error");
      else log(`console: ${e.kind}${e.detail ? " " + e.detail : ""}`);
      break;
  }
});

window.addEventListener("error", (ev) => toast(`Unexpected error: ${ev.message}`, "error"));
window.addEventListener("unhandledrejection", (ev) => toast(`Unexpected error: ${errText(ev.reason)}`, "error"));

// ---- activity buttons -----------------------------------------------------------------------------------------------
$("log-clear").onclick = () => { LOG.length = 0; renderLog(); };
$("log-copy").onclick = async () => {
  const text = LOG.map((e) => `${e.t.toISOString()} ${e.msg}`).join("\n");
  try { await navigator.clipboard.writeText(text); toast("Copied", "ok", 1500); } catch { toast("Could not copy; select the text instead.", "error"); }
};

// ---- start ----------------------------------------------------------------------------------------------------------
(async function main() {
  const [, info] = await call("app_info");
  if (info) S.info = info;
  $("demo-badge").hidden = !S.info.demo;
  log(`openPS ${S.info.version}${S.info.demo ? " (demo mode)" : ""}`);
  const [, st] = await call("session_state");
  if (st) S.session = st;
  await loadPsn();
  await refreshConsoles();
  renderSession();
  scan(false);
  setInterval(() => { if (S.view === "consoles" && !busy() && !document.hidden) scan(false); }, 20000);
})();
