"use strict";
/* OpenPS front end. Rules: no innerHTML anywhere (names and messages come from the network and the console);
   every control is reachable by keyboard; every state change is announced through a live region. */

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

// ---- tiny helpers ---------------------------------------------------------------------------------------------------
const $ = (id) => document.getElementById(id);
const $$ = (sel, root = document) => [...root.querySelectorAll(sel)];
const gsvg = (id, cls = "") => {
  const s = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  s.setAttribute("aria-hidden", "true");
  if (cls) s.setAttribute("class", cls);
  const u = document.createElementNS("http://www.w3.org/2000/svg", "use");
  u.setAttribute("href", `#g-${id}`);
  s.append(u);
  return s;
};
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
  controller: null,
  psn: null,
  settings: null,
  activeId: null,
  scanning: false,
  scannedOnce: false,
  waking: new Set(),
  view: "consoles",
};
const SESSION_BUSY = ["connecting", "testing_network", "handshaking", "connected"];
S.input = { mapping: false, profile: "" };
S.ov = null; // last overlay state from the engine
const busy = () => SESSION_BUSY.includes(S.session.state);

// ---- activity log ---------------------------------------------------------------------------------------------------
const LOG = [];
function logRow(e) {
  return el("li", { dataset: { level: e.level } }, el("time", { textContent: e.t.toLocaleTimeString(), dateTime: e.t.toISOString() }), el("span", { textContent: e.msg }));
}
function log(msg, level = "info") {
  const e = { t: new Date(), msg: String(msg), level };
  LOG.push(e);
  const over = LOG.length > 500;
  if (over) LOG.shift();
  if (S.view !== "activity") return;
  // incremental: append one row, drop the oldest, follow the tail only when the reader is already there
  const ol = $("log");
  const atEnd = ol.scrollHeight - ol.scrollTop - ol.clientHeight < 24;
  if (LOG.length === 1) ol.replaceChildren();
  ol.append(logRow(e));
  if (over) ol.firstElementChild.remove();
  if (atEnd) ol.scrollTop = ol.scrollHeight;
}
function renderLog() {
  const ol = $("log");
  if (!LOG.length) {
    ol.replaceChildren(el("li", { className: "log-empty", textContent: "Nothing yet." }));
    return;
  }
  ol.replaceChildren(...LOG.map(logRow));
  ol.scrollTop = ol.scrollHeight;
}

// ---- toasts ---------------------------------------------------------------------------------------------------------
// Every message leaves with a short fade (see `.leaving` in the style sheet) instead of vanishing. Showing the same element again cancels the
// fade, so a message that comes back while it is leaving is not removed behind the new text.
function fadeOut(node, done) {
  if (!node || node.hidden || node.classList.contains("leaving")) return;
  const token = (node._fade = {});
  node.classList.add("leaving");
  const end = () => {
    if (node._fade !== token) return;
    node._fade = null;
    node.classList.remove("leaving");
    done?.();
  };
  node.addEventListener("animationend", end, { once: true });
  setTimeout(end, 520); // the animation may not run (hidden tab, reduced motion): never leave the message behind
}
function unfade(node) {
  node._fade = null;
  node.classList.remove("leaving");
}
function toast(message, kind = "info", ms = kind === "error" ? 12000 : 4000) {
  const box = $("toasts");
  const t = el("div", { className: "toast", dataset: { kind }, attrs: kind === "error" ? { role: "alert" } : {} }, svg(kind === "error" ? "alert" : kind === "ok" ? "check" : "info"), el("p", { textContent: message }));
  const close = el("button", { type: "button", attrs: { "aria-label": "Dismiss" } }, svg("close"));
  let timer = 0;
  const remove = () => { clearTimeout(timer); fadeOut(t, () => t.remove()); };
  const arm = () => { clearTimeout(timer); timer = setTimeout(remove, ms); };
  close.onclick = remove;
  t.append(close);
  box.append(t);
  while (box.querySelectorAll(".toast:not(.leaving)").length > 3) { const old = box.querySelector(".toast:not(.leaving)"); fadeOut(old, () => old.remove()); }
  // top layer: stays above any open modal dialog; re-shown so it also sits above a dialog opened later
  try { if (box.matches(":popover-open")) box.hidePopover(); box.showPopover(); } catch { /* no popover support: z-index fallback */ }
  arm();
  for (const ev of ["mouseenter", "focusin"]) t.addEventListener(ev, () => clearTimeout(timer)); // never vanish under the pointer
  for (const ev of ["mouseleave", "focusout"]) t.addEventListener(ev, arm);
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
  const changed = name !== S.view;
  S.view = name;
  document.body.dataset.view = name;
  for (const v of $$(".view")) v.hidden = v.id !== `view-${name}`;
  for (const b of $$(".nav-btn")) {
    if (b.dataset.nav === name) b.setAttribute("aria-current", "page");
    else b.removeAttribute("aria-current");
  }
  $("page-title").textContent = TITLES[name];
  if (changed) hideBanner(); // a stale error must not follow the user to another page
  if (name === "activity") renderLog();
  if (name === "settings") { renderSettings(); renderControllerSettings(); paintKeymap(); paintPsnApi(); }
  if (name === "about") renderAbout();
  if (name === "consoles") refreshConsoles();
}
for (const b of $$(".nav-btn")) b.addEventListener("click", () => showView(b.dataset.nav));

// ---- consoles -------------------------------------------------------------------------------------------------------
const STATUS_TEXT = { ready: "Ready", standby: "In rest mode", other: "Busy", unknown: "Not found on network" };
// After a session ends the console needs a few seconds to settle (it may be going into rest mode). For COOLDOWN_MS the card
// cannot be connected; then the console is looked for again and the card shows what it really is: ready, or in rest mode.
const COOLDOWN_MS = 10000;
S.flow = null; S.flowId = 0; S.launchId = 0; S.pendingGame = null;
S.cool = new Map();      // console id -> time (ms) when connecting is allowed again
S.rechecking = new Set(); // ids whose status is being re-read after the cooldown
let coolTimer = 0;
const coolLeft = (id) => Math.max(0, Math.ceil(((S.cool.get(id) || 0) - Date.now()) / 1000));
// A console that was just disconnected can take up to a minute to show up again (it is going into rest mode, or still shutting the
// session down). After the cooldown it is looked for every few seconds; the card says "Checking…" until it answers or a minute has passed.
const RECHECK_MS = 60000, RECHECK_EVERY_MS = 4000;
S.recheckUntil = new Map();
S.coolWhy = new Map(); // id -> "wake" when the lock follows a failed wake (else it follows a session)
function startCooldown(id, ms = COOLDOWN_MS, recheckMs = RECHECK_MS, why = "session") {
  if (!id) return;
  S.coolWhy.set(id, why);
  S.cool.set(id, Date.now() + ms);
  S.recheckUntil.set(id, Date.now() + ms + recheckMs);
  S.found.delete(id); // whatever we knew a moment ago is stale
  clearInterval(coolTimer);
  let lastScan = 0, scanning = false;
  coolTimer = setInterval(async () => {
    for (const [cid, until] of [...S.cool]) {
      if (Date.now() < until) continue;
      S.cool.delete(cid);
      S.rechecking.add(cid);
      lastScan = 0;
    }
    if (S.rechecking.size && !scanning && Date.now() - lastScan >= RECHECK_EVERY_MS) {
      scanning = true;
      lastScan = Date.now();
      await scan(false);
      scanning = false;
      for (const cid of [...S.rechecking]) {
        if (S.found.has(cid) || Date.now() > (S.recheckUntil.get(cid) || 0)) { S.rechecking.delete(cid); S.recheckUntil.delete(cid); }
      }
    }
    renderConsoles();
    if (!S.cool.size && !S.rechecking.size) clearInterval(coolTimer);
  }, 1000);
  renderConsoles();
}
function statusOf(c) {
  if (S.waking.has(c.id)) return "waking";
  if (S.cool.has(c.id) || S.rechecking.has(c.id)) return "checking";
  const f = S.found.get(c.id);
  return f ? f.status : S.scannedOnce ? "unknown" : "checking";
}

async function refreshConsoles() {
  const [e, list] = await call("list_consoles");
  if (e) return toast(`Could not read your consoles: ${e}`, "error");
  S.consoles = list;
  renderConsoles();
}

// Cards are kept and patched in place (keyed by console id): a timer tick or a scan that changes nothing touches no DOM, so focus,
// hover and the pulse animation survive. Only changed text/attributes are written.
const CARDS = new Map();  // console id -> { node, refs, c }
const NEARBY = new Map(); // host key -> { node, refs, d }
const setText = (n, t) => { if (n.textContent !== t) n.textContent = t; };
const setAttr = (n, k, v) => { if (n.getAttribute(k) !== v) n.setAttribute(k, v); };
const setIcon = (holder, name) => { const u = holder.querySelector("use"); if (!u || u.getAttribute("href") !== `#i-${name}`) holder.replaceChildren(svg(name)); };
// Put `nodes` into `parent` in order, moving a node only when it is out of place (moving a focused node would drop its focus).
function reconcile(parent, nodes) {
  nodes.forEach((n, i) => { if (parent.children[i] !== n) parent.insertBefore(n, parent.children[i] || null); });
  while (parent.children.length > nodes.length) parent.lastElementChild.remove();
}
// If the focused element is about to disappear with its card, hand focus to something sensible instead of <body>.
function keepFocus(gone, fallback) {
  const a = document.activeElement;
  if (a && gone.some((n) => n.contains(a))) queueMicrotask(() => (fallback() || $("refresh-btn")).focus({ preventScroll: true }));
}

function renderConsoles() {
  const grid = $("console-grid");
  const none = S.consoles.length === 0;
  $("empty").hidden = !none || !S.scannedOnce; // the guide stays visible next to nearby consoles: it says what the code is
  setText($("consoles-sub"), none ? "Paired consoles appear here." : `${S.consoles.length} paired · choose Connect to start playing`);
  const ids = new Set(S.consoles.map((c) => c.id));
  const gone = [];
  for (const [id, card] of CARDS) if (!ids.has(id)) { gone.push(card.node); CARDS.delete(id); }
  const nodes = S.consoles.map((c) => {
    let card = CARDS.get(c.id);
    if (!card) CARDS.set(c.id, (card = makeConsoleCard(c)));
    patchConsoleCard(card, c);
    return card.node;
  });
  if (none && !S.scannedOnce) for (let i = 0; i < 2; i++) nodes.push(grid.querySelectorAll(".skeleton")[i] || el("div", { className: "card console skeleton", attrs: { "aria-hidden": "true" } }));
  if (gone.length) keepFocus(gone, () => grid.querySelector(".console-actions button:not([aria-disabled=true])"));
  reconcile(grid, nodes);
  renderNearby();
  syncGames();
}

function renderNearby() {
  const nb = $("nearby-grid");
  const keys = new Set(S.nearby.map((d) => d.host_id || d.addr));
  const gone = [];
  for (const [k, card] of NEARBY) if (!keys.has(k)) { gone.push(card.node); NEARBY.delete(k); }
  const nodes = S.nearby.map((d) => {
    const k = d.host_id || d.addr;
    let card = NEARBY.get(k);
    if (!card) NEARBY.set(k, (card = makeNearbyCard()));
    card.d = d;
    if (card.node.dataset.status !== d.status) card.node.dataset.status = d.status;
    setIcon(card.refs.tile, d.device === "ps4" ? "ps4" : "ps5");
    setText(card.refs.name, d.name || d.host_id);
    setAttr(card.refs.name, "title", d.name || d.host_id);
    setText(card.refs.meta, `${d.addr} · ${STATUS_TEXT[d.status] || d.status}`);
    return card.node;
  });
  if (gone.length) keepFocus(gone, () => $("nearby-grid").querySelector("button") || $("add-btn"));
  reconcile(nb, nodes);
  $("nearby-wrap").hidden = S.nearby.length === 0;
}

function makeConsoleCard(c) {
  const r = {
    tile: el("div", { className: "tile" }),
    name: el("div", { className: "console-name" }),
    meta: el("div", { className: "console-meta" }),
    dot: el("span", { className: "dot" }),
    stText: el("span"),
    hint: el("p", { className: "hint", hidden: true }),
    connect: el("button", { type: "button" }),
    connectIcon: el("span", { className: "ico" }),
    connectLabel: el("span"),
    forget: el("button", { type: "button", className: "btn btn-ghost btn-icon", attrs: { title: "Forget this console" } }, svg("trash")),
  };
  r.status = el("span", { className: "status", attrs: { role: "status" } }, r.dot, r.stText);
  r.connect.append(r.connectIcon, r.connectLabel);
  const card = { c, refs: r };
  // aria-disabled (not disabled) on Connect: a locked button keeps keyboard focus through cooldown ticks; the click is ignored here.
  r.connect.onclick = () => { if (r.connect.getAttribute("aria-disabled") === "true") return; startSession(card.c, statusOf(card.c) === "standby"); };
  r.forget.onclick = () => forgetConsole(card.c);
  card.node = el("article", { className: "card console" },
    el("div", { className: "console-head" }, r.tile, el("div", {}, r.name, r.meta)),
    r.status, r.hint, el("div", { className: "console-actions" }, r.connect, r.forget));
  return card;
}

function patchConsoleCard(card, c) {
  card.c = c;
  const r = card.refs, st = statusOf(c), f = S.found.get(c.id);
  const waking = S.waking.has(c.id), cooling = S.cool.has(c.id), rechecking = S.rechecking.has(c.id);
  const name = c.nickname || c.id, addr = f?.addr || c.host;
  if (card.node.dataset.status !== st) card.node.dataset.status = st;
  setAttr(card.node, "aria-label", name);
  setIcon(r.tile, c.device === "ps4" ? "ps4" : "ps5");
  setText(r.name, name); setAttr(r.name, "title", name);
  setText(r.meta, `${c.device === "ps4" ? "PlayStation 4" : "PlayStation 5"}${addr ? " · " + addr : ""}`);
  setText(r.stText, st === "waking" ? "Waking up…" : st === "checking" ? (cooling ? (S.coolWhy.get(c.id) === "wake" ? "Could not wake it. Trying again shortly…" : "Settling after the last session…") : "Waiting for the console to appear…") : STATUS_TEXT[st] || st);
  const hint = st === "unknown" ? "Not seen on your network. Make sure it is on and on the same Wi-Fi or Ethernet."
    : st === "other" ? "Another device or a game is using Remote Play. Connecting may be refused." : "";
  setText(r.hint, hint); if (r.hint.hidden === !!hint) r.hint.hidden = !hint;
  const locked = consoleLocked(c);
  const isActive = busy() && S.activeId === c.id;
  const cls = st === "unknown" || st === "other" ? "btn" : "btn btn-primary";
  if (r.connect.className !== cls) r.connect.className = cls;
  setAttr(r.connect, "aria-disabled", String(locked));
  setIcon(r.connectIcon, st === "standby" ? "power" : "play");
  setText(r.connectLabel, isActive ? "In session" : waking ? "Waking…" : cooling ? `Wait ${coolLeft(c.id)} s` : rechecking ? "Checking…" : st === "standby" ? "Wake & connect" : "Connect");
  if (r.forget.disabled !== busy()) r.forget.disabled = busy();
  setAttr(r.forget, "aria-label", `Forget ${name}`);
}

function makeNearbyCard() {
  const r = { tile: el("div", { className: "tile" }), name: el("div", { className: "console-name" }), meta: el("div", { className: "console-meta" }) };
  const pair = el("button", { type: "button", className: "btn" }, svg("plus"), el("span", { textContent: "Pair" }));
  const card = { refs: r, d: null };
  pair.onclick = () => openPair(card.d);
  card.node = el("article", { className: "card console" },
    el("div", { className: "console-head" }, r.tile, el("div", {}, r.name, r.meta)),
    el("div", { className: "console-actions" }, pair));
  return card;
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

const WAKE_SETTLE_MS = 6000;
// Before the session exists (waking the console, waiting for it to let go of an old session) the connect screen is driven by S.flow.
const setFlow = (title, sub) => { S.flow = { title, sub }; renderSession(); };
let starting = false; // a connect/wake call is in flight: a second click must not send a second one
async function startSession(c, needsWake, game = null) {
  if (starting || consoleLocked(c)) { if (!starting) toast(lockText(c) || "Not available right now."); return; }
  starting = true;
  try { await startSessionInner(c, needsWake, game); } finally { starting = false; }
}
async function startSessionInner(c, needsWake, game) {
  S.activeId = c.id;
  hideBanner();
  S.pendingGame = game ? { console: c.id, game } : null;
  const flowId = ++S.flowId;
  const what = game ? `Starting ${game.name}` : "";
  if (needsWake) {
    S.waking.add(c.id);
    setFlow(what || `Waking ${c.nickname || "the console"}`, "Waking the console. This usually takes about 15 seconds.");
    renderConsoles();
    const [e, ok] = await call("wake", { id: c.id });
    if (flowId !== S.flowId) return; // cancelled while waking
    if (e || !ok) {
      S.waking.delete(c.id);
      S.flow = null;
      S.pendingGame = null;
      renderSession();
      // small notice, the card is locked for 5 s (same lock as after a session), then the console is looked for again
      toast(e ? "Could not wake the console. Try again in a moment." : "The console did not wake up. Try again in a moment.", "error", 5000);
      log(e ? `wake failed: ${e}` : "wake: the console did not answer in time (is “Turn on PS5 from network” enabled?)", "error");
      return startCooldown(c.id, 5000, 8000, "wake");
    }
    // The console answers discovery a few seconds before its Remote Play service is ready: give it a moment (card keeps "Waking…").
    setFlow(what || `Waking ${c.nickname || "the console"}`, "The console is up. Letting it finish starting…");
    await sleep(WAKE_SETTLE_MS);
    if (flowId !== S.flowId) return;
    S.waking.delete(c.id);
    renderConsoles();
  } else if (game) {
    setFlow(what, `Connecting to ${c.nickname || "the console"}…`);
  }
  // Right after a wake the console answers discovery before its Remote Play service is up (connecting is refused for a little while), and
  // a console that is still closing an earlier session says so (80108b10). Keep trying quietly, with the connect screen up.
  S.wakeRetry = { id: c.id, until: Date.now() + 90000, woke: needsWake };
  const [e] = await call("connect", { id: c.id });
  if (e) { S.flow = null; S.pendingGame = null; renderSession(); showBanner(e, c); }
}

// The game to start once the stream is up: first its key map, then the console's launch command (which replaces whatever is running).
async function launchPending() {
  const p = S.pendingGame;
  S.pendingGame = null;
  if (!p) return;
  const c = S.consoles.find((x) => x.id === p.console);
  if (!c) return;
  applyGameMap(p.game);
  await launchGame(c, p.game);
}

// Pick the game's key map in the input engine. It starts together with the session, so ask a few times until it has taken it.
async function applyGameMap(g) {
  const id = S.gameMaps[gameKey(g)];
  if (!id) return;
  const name = profileName(id);
  if (!name) {
    // the key map was deleted: play with the default, and say so once
    if (!S.goneNoted.has(id)) { S.goneNoted.add(id); toast(`The key map for ${g.name} was removed. Using the default.`, "info", 5000); }
    return;
  }
  let last = "";
  for (let i = 0; i < 6; i++) {
    await sleep(i ? 700 : 1000);
    if (S.session.state !== "connected") return;
    const [e] = await call("input_select_profile", { name });
    if (!e) return toast(`Key map: ${name}`, "ok", 2500);
    last = e;
    if (/does not exist/i.test(e)) break; // retrying will not help
  }
  toast(`The key map “${name}” could not be selected: ${last}`, "error", 6000);
}

const launchText = (e) => (/could not tell which/i.test(String(e)) ? "I could not tell which PlayStation Network console this is." : String(e));
async function launchGame(c, g) {
  const id = ++S.launchId;
  toast(`Starting ${g.name}…`, "info", 5000);
  log(`launch ${g.name} (${g.title_id})`);
  const [e, r] = await call("launch_game", { id: c.id, titleId: g.title_id });
  if (e) return toast(`Could not start ${g.name}: ${launchText(e)}`, "error", 8000);
  for (let i = 0; i < 30; i++) {
    await sleep(2000);
    if (id !== S.launchId) return;
    const [e2, st] = await call("launch_status", { commandId: r.command_id });
    if (e2 || st.status !== "completed") continue;
    log(`launch ${g.name}: ${st.status}/${st.result}`);
    if (/succeed|success/i.test(st.result)) return toast(`${g.name} is starting`, "ok", 4000);
    return toast(`The console could not start ${g.name} (${st.result || st.action_state || "unknown"}).`, "error", 8000);
  }
  toast(`${g.name} was requested; the console is taking a while.`);
}

// Play button: wake if needed, connect, then start this game. While a session is already open the same button just switches game.
async function playGame(g) {
  const c = shelfConsole();
  if (!c) return toast("Add a console first.", "error");
  if (!g.title_id) return toast("The console did not report which title this is.", "error");
  if (S.session.state === "connected" && S.activeId === c.id) { applyGameMap(g); return launchGame(c, g); }
  if (consoleLocked(c)) return toast(lockText(c) || "Not available right now.");
  await startSession(c, statusOf(c) === "standby", g);
}

// Switching game from the stream: a list of what is installed on the console being streamed.
function openSwitch() {
  const c = S.consoles.find((x) => x.id === S.activeId) || shelfConsole();
  const games = S.lib?.library ? gamesFor(c) : [];
  if (!games.length) return toast(S.psnApi?.signed_in ? "No games to show yet." : "Sign in to PlayStation Network to list your games.");
  $("switch-list").replaceChildren(...games.map((g) => {
    const mapName = profileName(S.gameMaps[gameKey(g)] || "");
    const b = el("button", { type: "button", className: "switch-row", disabled: !g.title_id, attrs: { "aria-label": `Play ${g.name}` } },
      svg("play"), el("span", {}, el("strong", { textContent: g.name }), el("small", { textContent: [g.platform, g.trophies && g.trophies.progress > 0 ? `${g.trophies.progress}% trophies` : "", mapName ? `key map: ${mapName}` : ""].filter(Boolean).join(" · ") })));
    b.onclick = () => { $("switch-dialog").close(); applyGameMap(g); launchGame(c, g); };
    return el("li", {}, b);
  }));
  $("switch-dialog").showModal();
}
$("games-btn").onclick = openSwitch;
$("switch-dialog").addEventListener("click", (ev) => { if (ev.target === $("switch-dialog") || ev.target.closest("[data-switch=close]")) $("switch-dialog").close(); });
// "refused/timed out" right after a wake means the service is not up yet; 80108b10 means the console still holds the last session.
const wakeRetryable = (reason, woke) => /80108b10/i.test(String(reason || "")) || (woke && /refused|timed out|timeout|unreachable|reset/i.test(String(reason || "")));

async function forgetConsole(c) {
  const name = c.nickname || c.id;
  const ok = await confirmDialog(`Forget ${name}?`, "This removes the pairing from this computer. You will need a new 8-digit code to pair it again.", "Forget");
  if (!ok) return;
  const [e] = await call("forget", { id: c.id });
  if (e) return toast(`Could not forget the console: ${e}`, "error");
  toast(`${name} was removed`, "ok");
  await refreshConsoles();
  // the dialog gave focus back to a card that no longer exists: move it to the next card, or to the page's first action
  if (!document.activeElement || document.activeElement === document.body) ($("console-grid").querySelector(".console-actions button") || $("add-btn")).focus();
}

$("refresh-btn").onclick = () => scan(true);

// ---- PlayStation Network API (console list; first step of remote registration) --------------------------------------------
async function paintPsnApi() {
  const section = $("psn-api-section");
  const [e, st] = await call("psn_api_status");
  const was = S.psnApi?.signed_in;
  S.psnApi = e ? null : st;
  if (!!st?.signed_in !== !!was || !S.lib) loadLibrary(false);
  section.hidden = !!e || !st?.available;
  if (section.hidden) return syncGames();
  $("psn-api-signin").hidden = st.signed_in;
  $("psn-api-signed").hidden = !st.signed_in;
  S.psnRemote = !!st.remote;
  $("psn-api-status").textContent = st.signed_in ? "Games and internet registration: connected. This is separate from the account shown in the header, which is used for pairing." : "Games and internet registration: not connected. This is separate from the account shown in the header.";
  if (!st.signed_in) $("psn-console-list").replaceChildren();
}
// Inline field errors: next to the field, announced, and cleared on the next edit.
function fieldError(input, errEl, msg) {
  errEl.textContent = msg;
  errEl.hidden = false;
  input.setAttribute("aria-invalid", "true");
  errEl.scrollIntoView({ block: "nearest" });
}
function clearFieldError(input, errEl) {
  errEl.hidden = true;
  input.removeAttribute("aria-invalid");
}
$("npsso").addEventListener("input", () => clearFieldError($("npsso"), $("npsso-error")));
$("psn-api-signin").addEventListener("submit", async (ev) => {
  ev.preventDefault();
  if ($("psn-api-connect").getAttribute("aria-busy") === "true") return;
  const box = $("npsso"), errEl = $("npsso-error");
  const pasted = box.value;
  box.value = ""; // the token leaves the page the moment it is sent
  clearFieldError(box, errEl);
  if (!pasted.trim()) { fieldError(box, errEl, "Paste your login token first."); return box.focus(); }
  setLoading($("psn-api-connect"), true);
  const [e] = await call("psn_api_sign_in", { npsso: pasted });
  setLoading($("psn-api-connect"), false);
  if (e) { fieldError(box, errEl, `${/[.!?]$/.test(e) ? e : `${e}.`} Paste the token again to retry.`); return box.focus(); }
  toast("Connected to the PlayStation Network.", "ok");
  await paintPsnApi();
});
$("psn-api-out").onclick = async () => {
  await call("psn_api_sign_out");
  toast("Disconnected from the PlayStation Network.", "ok");
  paintPsnApi();
};
$("psn-api-list").onclick = async () => {
  setLoading($("psn-api-list"), true);
  const [e, list] = await call("psn_consoles");
  setLoading($("psn-api-list"), false);
  const ul = $("psn-console-list");
  ul.replaceChildren();
  if (e) {
    paintPsnApi(); // a dead sign-in shows the form again
    return toast(e, "error");
  }
  if (!list.length) return ul.append(el("li", { className: "muted", textContent: "No PS5 is linked to this account." }));
  for (const c of list) {
    const id = c.duid.length > 16 ? `${c.duid.slice(0, 8)}…${c.duid.slice(-6)}` : c.duid;
    const li = el("li", {}, el("div", {}, el("strong", { textContent: c.name }), el("span", { className: "muted", textContent: `  ${c.platform} · id ${id}` })));
    if (S.psnRemote) {
      const b = el("button", { type: "button", className: "btn btn-sm", textContent: "Register on this device", attrs: { "aria-label": `Register ${c.name} on this device` } });
      b.onclick = () => registerRemote(c);
      li.append(b);
    }
    ul.append(li);
  }
};

// Registering over the internet: PSN carries the exchange, no 8-digit code. The console must be on or in rest mode.
let remoteBusy = false;
// One state for a running registration, shown in Settings and in the Add dialog, whichever started it.
function remoteUi(on) {
  remoteBusy = on;
  P.busy = on;
  if (on) { $("psn-remote-step").textContent = "Starting…"; $("pair-psn-step").textContent = "Starting…"; }
  $("psn-remote-run").hidden = !on;
  $("pair-psn-run").hidden = !on;
  for (const b of $$("#psn-console-list button, #pair-psn-list button")) b.disabled = on;
  if (!on) updatePairGate();
}
async function registerRemote(c) {
  if (remoteBusy) return toast("A registration is already running.");
  remoteUi(true);
  const err = $("psn-remote-error");
  err.hidden = true;
  const [e, rec] = await call("psn_register_remote", { duid: c.duid });
  remoteUi(false);
  if (e) {
    if (/cancelled/i.test(e)) return toast("Registration cancelled.");
    err.textContent = e;
    err.hidden = false;
    return toast("The console could not be registered.", "error");
  }
  toast(`${rec.nickname || "Your console"} was added. You can connect to it from Consoles.`, "ok");
  refreshConsoles();
}
$("psn-remote-cancel").onclick = () => call("psn_register_cancel");
$("pair-psn-cancel").onclick = () => call("psn_register_cancel");

// ---- games and trophies --------------------------------------------------------------------------------------------------------
// The account's played games (Sony does not expose what is installed) with trophy progress, as one row under the consoles, and the
// account's trophy totals as a shelf on top. Data and covers are cached on disk by the app, so the shelf is the same on every start.
S.lib = null;
S.psnApi = null;
let libBuilt = "";
const TROPHIES = ["platinum", "gold", "silver", "bronze"];
const trophy = (kind) => gsvg("trophy", `tp tp-${kind}`);
const ago = (secs) => {
  if (!secs) return "";
  const d = Math.max(0, Date.now() / 1000 - secs);
  return d < 90 ? "just now" : d < 5400 ? `${Math.round(d / 60)} min ago` : d < 129600 ? `${Math.round(d / 3600)} h ago` : `${Math.round(d / 86400)} days ago`;
};

function renderShelf() {
  const box = $("trophy-shelf"), lib = S.lib?.library;
  if (!lib?.summary) { box.hidden = true; return; }
  const sm = lib.summary;
  box.hidden = false;
  const ring = el("div", { className: "level-ring", attrs: { role: "img", "aria-label": `Trophy level ${sm.level}, ${sm.progress} percent to the next level` } }, el("b", { textContent: String(sm.level) }));
  ring.style.setProperty("--p", String(sm.progress));
  $("shelf-level").replaceChildren(ring, el("div", { className: "lv-text" }, el("strong", { textContent: "Trophy level" }), el("span", { className: "muted small", textContent: `${sm.progress}% to level ${sm.level + 1}` })));
  const withTrophies = lib.games.filter((g) => g.trophies && g.trophies.total && Object.values(g.trophies.total).some((n) => n > 0));
  const done = withTrophies.filter((g) => g.trophies.progress >= 100).length;
  const avg = withTrophies.length ? Math.round(withTrophies.reduce((a, g) => a + g.trophies.progress, 0) / withTrophies.length) : 0;
  const stat = (label, value, kind) => el("div", {}, el("dt", {}, kind ? trophy(kind) : "", el("span", { textContent: label })), el("dd", { textContent: String(value) }));
  $("shelf-stats").replaceChildren(
    ...TROPHIES.map((k) => stat(k[0].toUpperCase() + k.slice(1), sm.earned[k] ?? 0, k)),
    stat("Games on account", lib.games.length), stat("Completed", done), stat("Avg. progress", `${avg}%`));
}

// The same lock the Connect button uses: nothing starts while busy, unavailable, checking, waking, or settling.
const consoleLocked = (c) => busy() || S.waking.has(c.id) || S.cool.has(c.id) || S.rechecking.has(c.id) || statusOf(c) === "checking" || statusOf(c) === "unknown";
function lockText(c) {
  if (S.waking.has(c.id)) return "The console is waking up…";
  if (S.cool.has(c.id)) return `Available in ${coolLeft(c.id)} s`;
  if (S.rechecking.has(c.id)) return "Checking the console…";
  if (statusOf(c) === "unknown") return "The console was not found on your network.";
  if (busy()) return "A session is already running";
  return "";
}
function shelfConsole() {
  return S.consoles.find((c) => statusOf(c) === "ready") || S.consoles[0] || null;
}

const artQueue = [];
let artActive = 0;
function pumpArt() {
  while (artActive < 4 && artQueue.length) {
    const { img, url } = artQueue.shift();
    artActive++;
    call("game_art", { url }).then(([e, uri]) => {
      if (!e && uri && img.isConnected) {
        const back = img.previousElementSibling; // blurred copy behind the poster, so a poster that is not exactly 3:4 has no empty bars
        if (back?.classList.contains("bg")) back.src = uri;
        img.onload = () => { img.classList.add("loaded"); back?.classList.add("loaded"); img.parentElement?.classList.add("has-art"); };
        img.onerror = () => { img.onload = img.onerror = null; img.removeAttribute("src"); };
        img.src = uri;
      }
    }).catch(() => {}).finally(() => { artActive--; pumpArt(); });
  }
}
let artObserver = null;

// What the shelf shows for a console: the titles installed on it (Sony reports them), with cover art and trophies joined in from the
// account's game list. Without that report it falls back to the games the account has played.
const normName = (n) => String(n || "").toLowerCase().replace(/[^\p{L}\p{N}]/gu, "");
function consoleTitles(c) {
  const lib = S.lib?.library;
  if (!lib) return null;
  const want = String(c?.nickname || "").trim().toLowerCase();
  return lib.consoles.find((x) => want && x.name.trim().toLowerCase() === want) || (lib.consoles.length === 1 ? lib.consoles[0] : null);
}
function gamesFor(c) {
  const lib = S.lib?.library;
  if (!lib) return [];
  const byId = new Map(lib.games.filter((g) => g.title_id).map((g) => [g.title_id, g]));
  const byName = new Map(lib.games.map((g) => [normName(g.name), g]));
  const ct = consoleTitles(c);
  if (ct?.installed?.length) {
    return ct.installed.filter((t) => t.kind === "game").map((t) => {
      const g = byId.get(t.title_id) || byName.get(normName(t.name));
      return { title_id: t.title_id, name: t.name, platform: t.platform, size: t.size_bytes, last: t.last_access, installed: true, image_url: g?.image_url, trophies: g?.trophies, play_minutes: g?.play_minutes };
    });
  }
  return lib.games.map((g) => ({ title_id: g.title_id, name: g.name, platform: g.platform, last: g.last_played, installed: false, image_url: g.image_url, trophies: g.trophies, play_minutes: g.play_minutes }));
}
const gameKey = (g) => g.title_id || "";
const fmtSize = (b) => (b >= 1e9 ? `${(b / 1e9).toFixed(b >= 1e11 ? 0 : 1)} GB` : b >= 1e6 ? `${Math.round(b / 1e6)} MB` : "");

S.gameMaps = {};
S.profileNames = [];
S.goneNoted = new Set();
S.profiles = []; // {id, name}: a game remembers a key map by its id, so renaming one never breaks the link
const profileName = (id) => (S.profiles.find((p) => p.id === id) || {}).name || "";
async function loadGameMaps() {
  await call("migrate_game_maps"); // maps saved with a profile name move to its id
  const [, m] = await call("game_maps");
  S.gameMaps = m || {};
  const [, n] = await call("input_profiles");
  S.profiles = n || [];
  S.profileNames = S.profiles.map((p) => p.name);
}

function gameTile(g, c) {
  const t = g.trophies;
  const hasTrophies = t && t.total && Object.values(t.total).some((n) => n > 0);
  const initials = g.name.split(/\s+/).filter(Boolean).slice(0, 2).map((w) => w[0]).join("");
  const img = el("img", { className: "fg", alt: "", attrs: { decoding: "async", draggable: "false" } });
  const back = el("img", { className: "bg", alt: "", attrs: { decoding: "async", draggable: "false", "aria-hidden": "true" } });
  const bar = hasTrophies && t.progress > 0 ? el("div", { className: "game-bar" + (t.progress >= 100 ? " done" : "") }, el("i")) : null;
  if (bar) bar.firstChild.style.width = `${t.progress}%`;
  const play = el("button", { type: "button", className: "play-btn", dataset: { key: gameKey(g) }, attrs: { "aria-label": `Play ${g.name}` } }, svg("play"));
  play.onclick = () => playGame(g);
  const art = el("div", { className: "game-art" }, el("span", { className: "initials", textContent: initials }), back, img, g.platform ? el("span", { className: "game-plat", textContent: g.platform }) : "", bar || "", play);
  const meta = el("span", { className: "game-meta" });
  if (hasTrophies) {
    meta.append(el("span", { textContent: `${t.progress}%` }));
    for (const k of TROPHIES) if (t.earned[k] > 0) meta.append(el("span", {}, trophy(k), String(t.earned[k])));
  } else if (g.size) meta.textContent = fmtSize(g.size);
  else if (g.play_minutes) meta.textContent = g.play_minutes >= 60 ? `${Math.round(g.play_minutes / 60)} h played` : `${g.play_minutes} min played`;
  // which key map this game uses
  const sel = el("select", { className: "map-select", attrs: { "aria-label": `Key map for ${g.name}` }, disabled: !gameKey(g) });
  const cur = S.gameMaps[gameKey(g)] || "";
  const gone = !!cur && !profileName(cur); // the key map this game used was deleted
  sel.append(el("option", { value: "", textContent: "Default key map" }));
  for (const p of S.profiles) sel.append(el("option", { value: p.id, textContent: p.name }));
  sel.value = gone ? "" : cur;
  const warn = gone ? el("span", { className: "map-warn", attrs: { role: "status" } }, svg("alert"), "Key map removed, using default") : null;
  sel.onchange = async () => {
    const [e, m] = await call("set_game_map", { key: gameKey(g), profile: sel.value || null });
    if (e) { toast(`Could not save the key map: ${e}`, "error"); sel.value = S.gameMaps[gameKey(g)] || ""; return; }
    S.gameMaps = m;
    warn?.remove();
    toast(sel.value ? `${g.name} will use “${profileName(sel.value)}”` : `${g.name} will use the default key map`, "ok", 2500);
  };
  const label = hasTrophies ? `${g.name}, ${g.platform || "game"}, ${t.progress} percent of trophies` : `${g.name}, ${g.platform || "game"}`;
  const card = el("div", { className: "game", dataset: { name: g.name }, attrs: { role: "group", "aria-label": label } }, art, el("span", { className: "game-name", textContent: g.name, attrs: { title: g.name } }), meta, sel, ...(warn ? [warn] : []));
  img.dataset.src = g.image_url || "";
  return el("li", {}, card);
}

let shelfFor = "";
function buildGames() {
  const track = $("games-track"), lib = S.lib?.library;
  const c = shelfConsole();
  const games = lib ? gamesFor(c) : [];
  const sig = JSON.stringify([c?.id, S.profiles, S.gameMaps, games.map((g) => [g.title_id, g.name, g.image_url, g.trophies?.progress, g.size])]);
  if (sig === libBuilt && track.childElementCount) return; // nothing changed: keep scroll position and the pictures already shown
  libBuilt = sig;
  shelfFor = c?.id || "";
  artQueue.length = 0; // drop pictures of the old tiles
  railActive = 0;
  artObserver?.disconnect();
  artObserver = "IntersectionObserver" in window ? new IntersectionObserver((entries) => {
    for (const en of entries) {
      if (!en.isIntersecting) continue;
      const img = en.target.querySelector("img.fg");
      artObserver.unobserve(en.target);
      if (img?.dataset.src) artQueue.push({ img, url: img.dataset.src });
    }
    pumpArt();
  }, { root: track, rootMargin: "0px 700px" }) : null;
  track.replaceChildren(...(games.length ? games.map((g) => gameTile(g, c)) : [el("li", { className: "rail-empty muted", textContent: lib ? "No games found for this console." : "Loading your games…" })]));
  setActiveTile(0);
  for (const li of track.children) {
    if (li.classList.contains("rail-empty")) continue;
    if (artObserver) artObserver.observe(li);
    else { const img = li.querySelector("img.fg"); if (img?.dataset.src) artQueue.push({ img, url: img.dataset.src }); }
  }
  if (!artObserver) pumpArt();
  railUpdate();
}

function syncGames() {
  const sec = $("games-sec"), st = S.psnApi;
  if (!S.info?.psn || !st?.available) { sec.hidden = true; return; }
  sec.hidden = false;
  const c = shelfConsole(), lib = S.lib?.library;
  if ((c?.id || "") !== shelfFor && lib) buildGames(); // another console became the one shown
  const n = lib ? gamesFor(c).length : 0;
  const installed = lib && consoleTitles(c)?.installed?.length;
  $("games-title").textContent = c ? (c.nickname || c.id) : "Your games";
  const stText = c ? ` · ${{ waking: "Waking up…", checking: "Checking…" }[statusOf(c)] || STATUS_TEXT[statusOf(c)] || ""}` : "";
  const when = S.lib?.fetched_at ? ` · updated ${ago(S.lib.fetched_at)}` : "";
  $("games-sub").textContent = lib ? `${n} ${installed ? "installed games" : "games on your account"}${when}${stText}` : st.signed_in ? "Loading your games…" : "";
  const prompt = $("games-prompt");
  const need = !st.signed_in;
  prompt.hidden = !need;
  $("games-sec").querySelector(".rail").hidden = need || !lib;
  $("games-refresh").hidden = need;
  if (need && !prompt.childElementCount) {
    const b = el("button", { type: "button", className: "btn btn-primary btn-sm", textContent: "Sign in to PlayStation Network" });
    b.onclick = () => ($("psn-btn").hidden || S.psn ? showView("settings") : $("psn-btn").click());
    prompt.append(svg("info"), el("div", {}, el("strong", { textContent: "See your games and trophies here." }), el("div", { className: "muted small", textContent: "Sign in once; the list comes from your PlayStation account." })), b);
  }
  railUpdate();
  const locked = c ? consoleLocked(c) : false;
  const why = c && locked ? lockText(c) : "";
  for (const b of $$("#games-track .play-btn")) {
    const noId = !b.dataset.key;
    b.setAttribute("aria-disabled", String(locked || noId));
    const name = b.closest(".game")?.dataset.name || "";
    b.title = noId ? `${name} — the console did not report which title this is` : locked && why ? `${name} — ${why}` : `Play ${name}`;
  }
}

let railRaf = 0, railActive = 0;
function railUpdateNow() {
  railRaf = 0;
  const t = $("games-track");
  const sets = [[$("rail-left"), t.scrollLeft <= 4], [$("rail-right"), t.scrollLeft + t.clientWidth >= t.scrollWidth - 2]];
  for (const [b, off] of sets) {
    if (off && document.activeElement === b) activeTile()?.querySelector(".play-btn")?.focus({ preventScroll: true }); // never strand focus on a vanishing arrow
    if (b.disabled !== off) b.disabled = off;
  }
}
const railUpdate = () => { if (!railRaf) railRaf = requestAnimationFrame(railUpdateNow); };
const tilesOf = () => Array.from($("games-track").querySelectorAll(".game"));
const activeTile = () => tilesOf()[railActive] || null;
// roving tabindex: one tab stop for the rail (Play of the active tile, plus its key-map select)
function setActiveTile(i) {
  const tiles = tilesOf();
  railActive = Math.max(0, Math.min(i, tiles.length - 1));
  tiles.forEach((t, k) => { const on = k === railActive; t.querySelector(".play-btn").tabIndex = on ? 0 : -1; t.querySelector(".map-select").tabIndex = on ? 0 : -1; });
}
const railStep = (dir) => { const t = $("games-track"); t.scrollBy({ left: dir * Math.max(240, t.clientWidth * 0.85) }); };
$("rail-left").onclick = () => railStep(-1);
$("rail-right").onclick = () => railStep(1);
$("games-track").addEventListener("scroll", railUpdate, { passive: true });
window.addEventListener("resize", railUpdate, { passive: true });
if ("ResizeObserver" in window) new ResizeObserver(railUpdate).observe($("games-track")); // also fires when the section first becomes visible
$("games-track").addEventListener("focusin", (ev) => { const g = ev.target.closest?.(".game"); if (g) { const i = tilesOf().indexOf(g); if (i !== railActive) setActiveTile(i); } });
$("games-track").addEventListener("keydown", (ev) => {
  const btns = $$("#games-track .play-btn"), i = btns.indexOf(document.activeElement);
  if (i < 0) return;
  const to = ev.key === "ArrowRight" ? i + 1 : ev.key === "ArrowLeft" ? i - 1 : ev.key === "Home" ? 0 : ev.key === "End" ? btns.length - 1 : -1;
  if (to < 0 || to >= btns.length) return;
  ev.preventDefault();
  setActiveTile(to);
  btns[to].focus();
  btns[to].scrollIntoView({ inline: "nearest", block: "nearest" });
});

let lastAutoRefresh = 0;
async function loadLibrary(refresh = false, quiet = false) {
  if (!S.psnApi?.signed_in) { S.lib = null; libBuilt = ""; renderShelf(); buildGames(); syncGames(); return; }
  const [e, r] = await call("psn_library", { refresh });
  if (e) { log(`games: ${e}`, "error"); return; }
  if (r.library) {
    S.lib = { library: r.library, fetched_at: r.fetched_at };
    renderShelf(); buildGames();
  }
  if (r.error) { log(`games: ${r.error}`, "error"); if (refresh && !quiet) toast("Could not update your games. Showing the saved list.", "error", 5000); }
  syncGames();
  // show what is saved at once, then bring it up to date quietly, but only when the saved list is stale and not in a loop
  if (!refresh && (!S.lib || Date.now() / 1000 - (S.lib.fetched_at || 0) > 600) && Date.now() - lastAutoRefresh > 60000) { lastAutoRefresh = Date.now(); loadLibrary(true); }
}
// The games and trophies refresh themselves, quietly: a few seconds after a session ends (what was played is new data) and whenever the list is
// 5 minutes old (checked every 30 s, only while the app is visible and no stream is running, so a stream never competes with it).
const LIB_AUTO_AGE_S = 5 * 60;
let libAutoBusy = false;
async function autoRefreshLibrary() {
  if (libAutoBusy || !S.psnApi?.signed_in) return;
  libAutoBusy = true;
  try { await loadLibrary(true, true); } finally { libAutoBusy = false; }
}
setInterval(() => {
  if (document.hidden || SESSION_BUSY.includes(S.session.state) || !S.lib) return;
  if (Date.now() / 1000 - (S.lib.fetched_at || 0) >= LIB_AUTO_AGE_S) autoRefreshLibrary();
}, 30000);
$("games-refresh").onclick = async () => {
  setLoading($("games-refresh"), true);
  await loadLibrary(true);
  setLoading($("games-refresh"), false);
};

// ---- controls (key map file) ----------------------------------------------------------------------------------------
async function paintKeymap() {
  const [e, info] = await call("input_config_info");
  $("keymap-state").textContent = e ? "Not available." : info.exists ? (S.keymapBad ? "Your key map has a mistake, so the default keys are in use. Open the file in your file manager, fix it, and connect again." : "You have a personal key map. It is saved next to your other app data.") : "Using the default keys.";
  $("keymap-reset").disabled = !!e || !info?.exists;
}
$("keymap-show").onclick = async () => { const [e] = await call("input_config_reveal"); if (e) toast(e, "error"); };
$("keymap-reset").onclick = async () => {
  const ok = await confirmDialog("Reset the key map?", "Your own key changes are removed and the default keys are used from the next time you connect. This cannot be undone.", "Reset");
  if (!ok) return;
  const [e] = await call("input_config_reset");
  if (e) return toast(e, "error");
  toast("Key map reset to the defaults.", "ok");
  paintKeymap();
};
$("add-btn").onclick = () => openPair();
$("empty-add").onclick = () => openPair();

// ---- banner ---------------------------------------------------------------------------------------------------------
// The console answers a refused session with a hex code. Say what it means and what to do; the raw text stays in the activity log.
const REASONS = {
  "80108b10": "The console still has the previous session open. This usually clears within a minute: wait a little, then try again.",
  "80108b11": "The console or its system software needs an update before it can be used for Remote Play.",
  "80108b12": "Remote Play is turned off on the console. Switch it on under Settings → System → Remote Play.",
  "80108b13": "Remote Play is not allowed for this account or at this time.",
  "80108b15": "The console reported an internal error. Restart it from its power menu, then try again.",
  "80108bff": "The console does not accept this pairing any more. Forget it here and add it again.",
};
const friendly = (msg) => {
  const code = (String(msg).match(/80108b[0-9a-f]{2}/i) || [])[0];
  return code && REASONS[code.toLowerCase()] ? REASONS[code.toLowerCase()] : msg;
};
function showBanner(msg, retryFor) {
  const b = $("banner");
  const raw = msg;
  msg = friendly(msg);
  b.replaceChildren(svg("alert"), el("span", { textContent: msg }));
  if (retryFor) {
    const r = el("button", { type: "button", className: "btn btn-sm" }, el("span", { textContent: "Try again" }));
    r.onclick = () => startSession(retryFor, false);
    b.append(r);
  }
  const x = el("button", { type: "button", className: "btn btn-ghost btn-sm btn-icon", attrs: { "aria-label": "Dismiss" } }, svg("close"));
  x.onclick = hideBanner;
  b.append(x);
  unfade(b);
  b.hidden = false;
  log(raw === msg ? msg : `${msg} (${raw})`, "error");
}
function hideBanner() {
  const b = $("banner");
  fadeOut(b, () => { b.hidden = true; });
}
// Non-blocking, dismissible notice (e.g. a damaged data file was set aside). Separate from the error banner so it is never cleared by navigation.
const NOTICE_SEEN = new Set();
function showNotice(kind, detail) {
  const key = `${kind}|${detail}`;
  if (NOTICE_SEEN.has(key)) return;
  NOTICE_SEEN.add(key);
  const n = $("notice");
  const title = kind === "data_damaged" ? "A data file was damaged and has been set aside" : "Notice";
  const x = el("button", { type: "button", className: "btn btn-ghost btn-sm btn-icon", attrs: { "aria-label": "Dismiss notice" } }, svg("close"));
  x.onclick = () => fadeOut(n, () => { n.hidden = true; n.replaceChildren(); });
  const text = el("span", {}, el("strong", { textContent: title }), el("br"), document.createTextNode(detail));
  n.replaceChildren(svg("info"), text, x);
  unfade(n);
  n.hidden = false;
  log(`${title}: ${detail}`, "error");
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
  const why = !S.info.psn ? `PSN unavailable. PS5 pairing needs PSN. ${S.info.psn_reason || ""}` : "";
  chip.title = why;
  if (why) { chip.tabIndex = 0; chip.setAttribute("role", "group"); chip.setAttribute("aria-label", why); }
  else { chip.removeAttribute("tabindex"); chip.removeAttribute("role"); chip.removeAttribute("aria-label"); }
  btn.hidden = !S.info.psn;
  btn.textContent = a ? "Sign out" : "Sign in";
  updatePairGate();
}
$("psn-btn").onclick = async () => {
  const btn = $("psn-btn");
  if (S.psn && !(await confirmDialog("Sign out of PlayStation Network?", "You will not be able to pair PS5 consoles until you sign in again. The games list from Settings is disconnected as well.", "Sign out"))) return;
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
    const inp = el("input", { type: "text", inputMode: "numeric", maxLength: 1, autocomplete: i === 0 ? "one-time-code" : "off", attrs: { "aria-label": `Digit ${i + 1} of 8`, pattern: "[0-9]" } });
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
    if (i === idx) li.setAttribute("aria-current", "step"); else li.removeAttribute("aria-current");
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
  clearFieldError($("pair-host"), $("pair-host-error"));
  renderPairList();
}
function setDevice(dev) {
  P.device = dev;
  buildSeg($("pair-device"), [["ps5", "PS5"], ["ps4", "PS4"]], dev, (v) => { P.device = v; updatePairGate(); });
}

// Consoles on the signed-in PSN account, shown inside the Add dialog; picking one registers it over the internet.
async function paintPairPsn() {
  const box = $("pair-psn"), list = $("pair-psn-list");
  $("pair-psn-error").hidden = true;
  const [, st] = await call("psn_api_status");
  box.hidden = !(st?.available && st.signed_in && st.remote);
  if (box.hidden) return;
  list.replaceChildren(el("div", { className: "pick-empty", textContent: "Loading your consoles…" }));
  const [e, found] = await call("psn_consoles");
  list.replaceChildren();
  if (e) return list.append(el("div", { className: "pick-empty", textContent: `Could not load your consoles: ${e}` }));
  if (!found.length) return list.append(el("div", { className: "pick-empty", textContent: "No PS5 is linked to this account." }));
  for (const c of found) {
    const b = el("button", { type: "button", attrs: { "aria-label": `Add ${c.name} from your account` } },
      el("div", { className: "tile tile-sm" }, svg("ps5")),
      el("div", {}, el("div", { className: "console-name", textContent: c.name }), el("div", { className: "console-meta", textContent: "From your PlayStation account" })));
    b.onclick = () => addFromAccount(c);
    list.append(b);
  }
}
async function addFromAccount(c) {
  if (remoteBusy) return toast("A registration is already running.");
  const err = $("pair-psn-error");
  err.hidden = true;
  remoteUi(true);
  const [e, rec] = await call("psn_register_remote", { duid: c.duid });
  remoteUi(false);
  if (e) {
    if (/cancelled/i.test(e)) return toast("Registration cancelled.");
    err.textContent = e;
    err.hidden = false;
    return;
  }
  $("done-title").textContent = `${rec.nickname || "Your console"} is added`;
  $("done-sub").textContent = "You can connect to it any time from the Consoles screen.";
  P.step = "done";
  wizState();
  $("pair-next").focus();
  refreshConsoles();
}

async function openPair(preselect) {
  const d = $("pair-dialog");
  P.step = "find"; P.selected = null; P.busy = false; P.device = "ps5";
  $("pair-host").value = "";
  $("pair-error").hidden = true;
  clearFieldError($("pair-host"), $("pair-host-error"));
  remoteUi(remoteBusy); // a registration started in Settings is shown here too
  makePin();
  setDevice("ps5");
  if (preselect) selectNearby(preselect);
  renderPairList();
  wizState();
  d.showModal();
  if (!preselect) $("pair-host").focus();

  paintPairPsn();
  await scan(false);
  if (preselect === undefined && S.nearby.length === 1 && !$("pair-host").value) selectNearby(S.nearby[0]);
  renderPairList();
}

function closePair() {
  const d = $("pair-dialog");
  if (d.open) d.close();
}
for (const b of $$("[data-close]")) b.addEventListener("click", closePair);
$("pair-back").onclick = () => { P.step = "find"; wizState(); $("pair-host").focus(); };
$("pair-host").addEventListener("input", () => clearFieldError($("pair-host"), $("pair-host-error")));
// A running registration cannot be dismissed by Escape or the backdrop; Cancel registration is the way out.
$("pair-dialog").addEventListener("cancel", (ev) => { if (P.busy && remoteBusy) { ev.preventDefault(); toast("A registration is running. Use Cancel registration first."); } });
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
      fieldError($("pair-host"), $("pair-host-error"), host ? "That does not look like an address. Use something like 192.168.1.20." : "Choose a console from the list or enter its address, for example 192.168.1.20.");
      return $("pair-host").focus();
    }
    clearFieldError($("pair-host"), $("pair-host-error"));
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
      er.textContent = `${e} Codes expire after a few minutes: generate a new one on the console.`;
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
// An option may carry a third entry: a small badge shown above its label (for example the resolution an upscaling multiple gives).
// An empty badge takes no room, so the options of one row keep their height.
function paintSegLabel(b, label, sub) {
  if (sub === undefined) {
    if (b.textContent !== label || b.firstElementChild) b.textContent = label;
    return;
  }
  let tag = b.querySelector(".seg-sub"), text = b.querySelector(".seg-label");
  if (!tag || !text) {
    b.textContent = "";
    tag = el("span", { className: "seg-sub" });
    text = el("span", { className: "seg-label" });
    b.append(tag, text);
  }
  if (text.textContent !== label) text.textContent = label;
  if (tag.textContent !== sub) tag.textContent = sub;
}
function buildSeg(box, options, value, onChange, disabled = () => false, why = "") {
  const same = box.children.length === options.length && options.every(([v], i) => box.children[i].dataset.v === String(v));
  if (!same) {
    // rebuild only when the options differ; keep focus on the same value
    const keep = box.contains(document.activeElement) ? document.activeElement.dataset.v : null;
    box.replaceChildren();
    for (const [v, label] of options) {
      const b = el("button", { type: "button", textContent: label, attrs: { role: "radio" } });
      b.dataset.v = String(v);
      box.append(b);
    }
    if (keep !== null) box.querySelector(`[data-v="${CSS.escape(keep)}"]`)?.focus();
  }
  options.forEach(([v, label, sub], i) => {
    const b = box.children[i], off = !!disabled(v);
    paintSegLabel(b, label, sub);
    b.setAttribute("aria-checked", String(v === value));
    b.setAttribute("aria-disabled", String(off));
    if (off && why) b.title = why; else b.removeAttribute("title");
    b.onclick = () => {
      if (b.getAttribute("aria-disabled") === "true") return;
      for (const o of $$('[role="radio"]', box)) o.setAttribute("aria-checked", String(o === b));
      rovingFocus(box);
      onChange(v);
    };
  });
  rovingFocus(box);
}

// ---- settings -------------------------------------------------------------------------------------------------------
// The rate actually presented (mirrors openps-metal's `effective_rate`): exactly what was asked for, never above the display; an even fraction
// of the refresh rate is held for whole refreshes, any other rate is scheduled frame by frame ("capped").
function effectiveRate(requested, refresh) {
  const req = Math.max(1, requested);
  if (!(refresh >= 1)) return { hz: req, capped: false, k: 1 };
  if (req >= refresh * 0.99) return { hz: refresh, capped: false, k: 1 };
  const ratio = refresh / req, k = Math.max(1, Math.round(ratio));
  return Math.abs(ratio - k) < 0.01 * ratio ? { hz: refresh / k, capped: false, k } : { hz: req, capped: true, k: 1 };
}
const WIDTH_FOR = { 360: 640, 540: 960, 720: 1280, 1080: 1920 };
async function renderSettings() {
  const [e, s] = await call("get_settings");
  if (e) return toast(`Could not load settings: ${e}`, "error");
  S.settings = s;
  S.saved = { ...s };
  drawSettings();
}
async function renderControllerSettings() {
  const list = $("controller-list");
  list.setAttribute("aria-busy", "true");
  const [error, status] = await call("controller_status");
  list.setAttribute("aria-busy", "false");
  if (error) {
    list.replaceChildren(el("li", { className: "controller-empty", role: "status", textContent: `Could not load controllers: ${error}` }));
    return toast(`Could not load controller settings: ${error}`, "error");
  }
  S.controller = status;
  S.input.mapping = status.keyboardMode;
  paintMapping();
  const deadzone = $("controller-deadzone");
  deadzone.value = String(status.deadzone);
  $("controller-deadzone-out").textContent = `${Math.round(status.deadzone * 100)}%`;
  if (!status.pads.length) {
    list.replaceChildren(el("li", { className: "controller-empty", textContent: "No compatible controller is connected." }));
    return;
  }
  list.replaceChildren(...status.pads.map((pad) => {
    const battery = Number.isFinite(pad.batteryLevel)
      ? `${Math.round(pad.batteryLevel * 100)}%${pad.charging ? " · charging" : ""}`
      : "Battery unavailable";
    return el("li", { className: "controller-item" },
      el("span", { className: "controller-name", textContent: pad.name }),
      el("span", { className: "controller-meta", textContent: `${pad.kind} · Slot ${pad.slot + 1} · ${battery}` }));
  }));
}
function paintControllerMode() {
  // The tile always says "Controls"; its picture shows what is in use: a mouse for keyboard and mouse, a controller otherwise.
  $("map-btn").querySelector("use")?.setAttribute("href", S.input.mapping ? "#i-mouse" : "#i-gamepad");
}
$("controller-deadzone").addEventListener("input", () => {
  $("controller-deadzone-out").textContent = `${Math.round(Number($("controller-deadzone").value) * 100)}%`;
});
$("controller-deadzone").addEventListener("change", async () => {
  const deadzone = Number($("controller-deadzone").value);
  const [error, applied] = await call("controller_set_deadzone", { deadzone });
  if (error) {
    toast(`Could not set stick deadzone: ${error}`, "error");
    if (S.controller) {
      $("controller-deadzone").value = String(S.controller.deadzone);
      $("controller-deadzone-out").textContent = `${Math.round(S.controller.deadzone * 100)}%`;
    }
    return;
  }
  S.controller.deadzone = applied;
});
// Rumble and adaptive-trigger strength: a stepper each (10 % a step) and a Test button on its right. Saved like every setting; the test
// waits for the save so it is felt at the new strength.
const TUNING = [
  { id: "rumble", key: "rumble_pct", min: 0, max: 200, test: "rumble", what: "rumble" },
  { id: "trigger", key: "trigger_pct", min: 0, max: 100, test: "trigger", what: "adaptive trigger" },
];
// A test runs for as long as the backend says (it answers with the length in ms); both Test buttons stay disabled until it is over, because
// a second test would cut the first one short. The clicked one shows a spinner.
let controllerTesting = false;
async function runControllerTest(t) {
  if (controllerTesting) return;
  controllerTesting = true;
  const buttons = TUNING.map((x) => $(`${x.id}-test`)), mine = $(`${t.id}-test`);
  const finish = () => { controllerTesting = false; for (const b of buttons) setLoading(b, false); };
  for (const b of buttons) { b.disabled = true; }
  setLoading(mine, true);
  await saveChain; // feel it at the strength that was just set
  const [e, ms] = await call("controller_test", { kind: t.test });
  if (e) { finish(); toast(String(e), "info", 5000); return; }
  setTimeout(finish, Number(ms) + 200);
}
function drawTuning() {
  const s = S.settings;
  for (const t of TUNING) {
    const v = s[t.key] ?? 100;
    $(`${t.id}-out`).textContent = `${v}%`;
    $(`${t.id}-meter`).style.setProperty("--v", String(v / t.max));
    $(`${t.id}-dec`).disabled = v <= t.min;
    $(`${t.id}-inc`).disabled = v >= t.max;
    $(`${t.id}-dec`).onclick = () => save({ [t.key]: Math.max(t.min, v - 10) });
    $(`${t.id}-inc`).onclick = () => save({ [t.key]: Math.min(t.max, v + 10) });
    $(`${t.id}-test`).onclick = () => runControllerTest(t);
  }
}
// The stream bar's Immersion button is the same saved setting as the switch in Settings (so it is permanent), shown only where the GPU
// renderer is. Pressing it says what it did, like the other switches in the stream.
function paintImmersion() {
  const b = $("imm-btn"), on = !!S.settings?.immersion;
  b.hidden = !S.info.metal;
  b.setAttribute("aria-pressed", String(on));
  b.title = on ? "Immersion is on: the black bars are filled. Click to turn it off." : "Immersion is off. Click to fill the black bars with a soft glow of the picture.";
}
$("imm-btn").onclick = async () => {
  if (!S.settings) await renderSettings();
  if (!S.settings) return;
  const on = !S.settings.immersion;
  save({ immersion: on }, true);
  toast(on ? "Immersion on" : "Immersion off", "info", 2000);
};
// Leave sits just right of the centred bar: tell the style sheet how wide the bar is (it depends on the captions and the tools shown).
{
  const bar = document.querySelector(".hud-bottom"), hud = $("hud");
  const sync = () => hud.style.setProperty("--bar-w", `${bar.offsetWidth}px`);
  new ResizeObserver(sync).observe(bar);
  sync();
}
// Debug tools (Settings -> Display -> Debug tools): the Engine button and its panel switch the engine on the running stream. They are the
// same saved setting as in Settings, so what is chosen here stays chosen.
function closeEnginePop() {
  $("engine-pop").hidden = true;
  $("engine-btn").setAttribute("aria-pressed", "false");
  $("engine-btn").setAttribute("aria-expanded", "false");
}
function paintEnginePop() {
  paintDebugBar();
  const s = S.settings;
  const on = !!s?.debug && (!!S.info.metal || !!S.info.fsr);
  $("engine-btn").hidden = !on;
  if (!on) { closeEnginePop(); return; }
  buildSeg(document.querySelector('[data-hud="engine"]'), [["metalfx", "MetalFX"], ["fsr", "FSR"]], s.engine === "fsr" ? "fsr" : "metalfx", (v) => save({ engine: v }, true), (v) => (v === "fsr" ? !S.info.fsr : !S.info.metal), "Not available on this Mac");
  // the upscaling resolution, live: the multiple of the stream's size the picture is made at (then scaled to fit the window)
  const upTo = (f) => `${Math.round((s.height * f) / 2) * 2}p`;
  buildSeg(document.querySelector('[data-hud="upscale"]'), [["off", "Off", ""], ["1.33x", "1.33×", upTo(4 / 3)], ["1.5x", "1.5×", upTo(1.5)], ["2x", "2×", upTo(2)]], s.upscale || "off", (v) => save({ upscale: v }, true));
}
// what is really doing the work now (from the renderer's statistics), so a choice can be judged
function engineNow(r) {
  const finish = [r.linear_active && !r.fsr_active && "linear light", r.rcas_active && "adaptive sharpening"].filter(Boolean).join(" + ");
  const up = r.upscaled ? `${r.fsr_active ? "FSR" : "MetalFX"} to ${r.upscale_height}p${finish ? ` (${finish})` : ""}` : "not upscaling";
  const fg = !r.generating ? "none" : r.metalfx_active ? "MetalFX" : "our interpolator";
  const a = r.adaptive || {};
  const rate = a.on && a.cap_hz ? ` Frame rate held to ${a.cap_hz} fps (asked for ${Math.round(a.asked_hz)}) because the GPU is too busy.` : "";
  return `In use now: upscaling ${up}; generated frames ${fg}.${rate}`;
}
$("engine-btn").onclick = async () => {
  if (!S.settings) await renderSettings();
  const open = $("engine-pop").hidden;
  if (!open) { closeEnginePop(); return; }
  paintEnginePop();
  $("engine-pop").hidden = false;
  $("engine-btn").setAttribute("aria-pressed", "true");
  $("engine-btn").setAttribute("aria-expanded", "true");
};
document.addEventListener("keydown", (ev) => { if (ev.key === "Escape" && !$("engine-pop").hidden) closeEnginePop(); });
// What each engine is, said at the Settings field (FSR is the part of AMD's FSR family that can work on a finished video picture).
// the rate really shown while the adaptive limit applies: the limit, or lower when a fixed-refresh display can only show a divisor of its refresh rate evenly
function heldRate(r) {
  const cap = r.adaptive.cap_hz, target = Math.round(r.target_hz);
  return target > 0 && target < cap ? target : cap;
}

function paintEngineHint(s) {
  if ($("engine-field").hidden) return;
  $("engine-hint").textContent = s.engine === "fsr"
    ? "AMD FidelityFX Super Resolution 1 (EASU), ported to Metal: an edge-adaptive upscaler that works on the finished picture, followed by the same sharpening as MetalFX. It has no linear-light mode. FSR only upscales: in Smooth mode at about 2× (60 to 120) MetalFX's frame generation still makes the in-between frame, and our own interpolator makes the frames at other rates. FSR 3 and FSR 4 need the game's depth, motion vectors and jitter, which a video stream does not carry, so only FSR 1 is offered; the details are in the user guide."
    : "MetalFX is Apple's GPU upscaler. In Smooth mode at about 2× (60 to 120) it also makes the in-between frame; our own interpolator makes the frames at other rates.";
}

function drawSettings() {
  const s = S.settings;
  drawTuning();
  const hevcOk = S.info.hevc;
  buildSeg(document.querySelector('[data-setting="height"]'), [[360, "360p"], [540, "540p"], [720, "720p"], [1080, "1080p"]], s.height, (v) => save({ height: v }));
  buildSeg(document.querySelector('[data-setting="bitrate"]'), [[-1, "Auto"], [2000, "2 Mbit/s"], [6000, "6 Mbit/s"], [10000, "10 Mbit/s"], [15000, "15 Mbit/s"]], s.bitrate, (v) => save({ bitrate: v }));
  $("bitrate-hint").textContent = s.bitrate === -1
    ? "Auto asks for as much as your connection can carry, up to 100 Mbit/s (the most the original app allows) on a fast wired or Wi-Fi network; a console behind a router gets a normal request. The console decides what it really sends. If it refuses the request or ends the stream right after it starts, OpenPS connects again with a normal value."
    : `Fixed: the console is asked for ${s.bitrate / 1000} Mbit/s.`;
  buildSeg(document.querySelector('[data-setting="codec"]'), [["hevc", "HEVC (H.265)"], ["h264", "H.264"]], s.hevc ? "hevc" : "h264", (v) => save({ hevc: v === "hevc", ...(v === "hevc" ? {} : { hdr: false }) }), (v) => v === "hevc" && !hevcOk, "This device cannot decode HEVC");
  $("codec-hint").textContent = hevcOk ? "HEVC looks better at the same bitrate. H.264 is the compatible fallback." : "This device cannot decode HEVC in hardware, so H.264 is used.";
  // display: upscaling, the engine, each picture filter with its own switch, the frame rate, how extra frames are made, camera compensation
  const hz = Math.round(S.info.display_hz || 0);
  const metal = !!S.info.metal, fsrOk = !!S.info.fsr, gpuOk = metal || fsrOk;
  $("metal-chip").hidden = !gpuOk;
  // each multiple shows the size it draws for the resolution chosen above (rounded to an even number of lines)
  const upTo = (f) => `${Math.round((s.height * f) / 2) * 2}p`;
  buildSeg(document.querySelector('[data-setting="upscale"]'), [["off", "Off", ""], ["1.33x", "1.33×", upTo(4 / 3)], ["1.5x", "1.5×", upTo(1.5)], ["2x", "2×", upTo(2)]], s.upscale || "off", (v) => save({ upscale: v }), (v) => v !== "off" && !gpuOk, "Needs a GPU upscaler, not available on this device");
  const engine = s.engine === "fsr" ? "fsr" : "metalfx";
  $("engine-field").hidden = !gpuOk;
  if (gpuOk) buildSeg(document.querySelector('[data-setting="engine"]'), [["metalfx", "MetalFX"], ["fsr", "FSR"]], engine, (v) => save({ engine: v }), (v) => (v === "fsr" ? !fsrOk : !metal), "Not available on this Mac");
  paintEngineHint(s);
  const upscaled = (s.upscale || "off") !== "off" && gpuOk;
  // Every filter has its own switch; its slider keeps its value while the filter is off. Sliders are in percent and apply when you let go;
  // the number follows the slider while you drag.
  const lin = s.linear_light !== false;
  setSwitch("linear-switch", lin, (on) => save({ linear_light: on }), upscaled && engine === "metalfx", engine === "fsr" ? "FSR has no linear-light mode" : "Needs upscaling: turn it on above");
  $("linear-hint").textContent = engine === "fsr" ? "FSR works on the picture as it is and has no linear-light mode."
    : !upscaled ? "Needs upscaling: turn it on above."
    : lin ? "The MetalFX scaler works in linear light: more accurate colour and edges (measured closer to the real picture at every bitrate) for a little more GPU time."
    : "Off: the MetalFX scaler's perceptual mode.";
  const sharpOn = s.sharpness_on !== false;
  setSwitch("sharp-switch", sharpOn, (on) => save({ sharpness_on: on }), gpuOk, "Needs the GPU, not available on this device");
  rangeSetting("sharp-range", "sharp-out", "sharp-reset", s.sharpness ?? 60, 60, (v) => save({ sharpness: v }));
  setRangeEnabled("sharp", sharpOn && gpuOk);
  $("sharp-hint").textContent = !sharpOn ? (upscaled ? "Off: the picture as the upscaler draws it, with no blend with a plain enlargement." : "Off: the picture is neither sharpened nor softened.")
    : upscaled && engine === "fsr" ? "In percent: 0 is a plain smooth enlargement, 100 is FSR's EASU as it draws it, above 100 is sharper than that. FSR's edges are not over-sharpened the way MetalFX's are, so 100 leaves them as designed."
    : upscaled ? "In percent: 0 is a plain smooth enlargement, 100 is the upscaler as it draws it, above 100 is sharper than that. The upscaler adds more edge contrast than the real picture has; about 60 (the default) is closest to it, and a light adaptive sharpening keeps it crisp there."
    : "In percent: 50 leaves the picture as it is, above sharpens, below softens.";
  const rcasOn = s.rcas_on !== false;
  setSwitch("rcas-switch", rcasOn, (on) => save({ rcas_on: on }), gpuOk, "Needs the GPU, not available on this device");
  rangeSetting("rcas-range", "rcas-out", "rcas-reset", s.rcas ?? 100, 100, (v) => save({ rcas: v }));
  setRangeEnabled("rcas", rcasOn && upscaled);
  $("rcas-hint").textContent = !gpuOk ? "Needs the Metal renderer."
    : !upscaled ? "Needs upscaling: turn it on above. It sharpens the upscaled picture."
    : !rcasOn ? "Off: the upscaled picture is not sharpened beyond the sharpness setting."
    : `Adaptive sharpening (AMD's RCAS) puts back crisp edges that the sharpness blend takes out, and leaves flat areas and noise alone. 0 is off, 100 % is the strength that measured closest to the real picture with MetalFX at sharpness 60 %, 200 % is twice that.${engine === "fsr" ? " FSR is made to be followed by RCAS." : ""}`;
  const satOn = s.saturation_on !== false;
  setSwitch("sat-switch", satOn, (on) => save({ saturation_on: on }), true);
  rangeSetting("sat-range", "sat-out", "sat-reset", s.saturation ?? 100, 100, (v) => save({ saturation: v }));
  setRangeEnabled("sat", satOn);
  $("sat-hint").textContent = satOn ? "100 % is the picture as the console sends it. Higher is more vivid, lower is paler." : "Off: the picture as the console sends it.";
  $("upscale-hint").textContent = gpuOk ? `Draws the picture larger than the stream's own ${s.height}p, using the GPU. The badge on each option is the size it gives for the resolution chosen above. The picture is made at that size even when the screen is smaller, and then scaled down to fit the window, which usually looks better than upscaling only to the window's size (it costs more GPU time the larger it is).` : "Hardware upscaling is not available on this device.";
  // Frame generation has no rate to choose: it fills the display's refresh rate (the console streams 60, the rest are made here), and the
  // adaptive frame rate steps it down in steps of 5 fps when this Mac cannot keep it up with every filter on.
  const nativeHz = 60; // the stream always runs at 60: what the game does not render is dropped as repeats, and at least 30 frames a second are kept
  const fgOn = s.framegen_on !== false;
  setSwitch("fg-switch", fgOn, (on) => save({ framegen_on: on }), gpuOk, "Needs the GPU, not available on this device");
  const eff = effectiveRate(240, S.info.display_hz || 0);
  const adaptive = s.adaptive_fps !== false;
  const fallback = adaptive ? " If this Mac cannot keep that up with every filter on, the rate is lowered automatically, gently and in steps of 5 fps (see Adaptive frame rate)." : "";
  $("fps-hint").textContent = !gpuOk ? "Needs the GPU, not available on this device."
    : !fgOn ? `Frames are shown as they arrive from the console (${nativeHz} per second). Turn it on to have frames made here to fill your display.`
    : !(hz >= 1) ? `Frames are generated to fill your display; the console sends ${nativeHz} per second.${fallback}`
    : hz <= nativeHz && uncap ? `Your display refreshes at ${hz} Hz, but frames are made at up to 240 per second (Run above the display's refresh rate): the screen shows the newest one at each refresh.${fallback}`
    : hz <= nativeHz ? `Your display refreshes at ${hz} Hz, which the stream's ${nativeHz} already fills, so nothing is generated. For a lower delay, turn on Run above the display's refresh rate.`
    : eff.capped ? `${nativeHz} → ${hz} frames per second to fill your ${hz} Hz display: the console sends ${nativeHz}, the rest are generated here. Without variable refresh some frames stay on screen one refresh and some two.${fallback}`
    : `${nativeHz} → ${Math.round(eff.hz)} frames per second to fill your ${hz} Hz display: the console sends ${nativeHz}, the rest are generated here.${fallback}`;
  const uncap = !!s.uncapped_rate;
  const gen = fgOn && gpuOk && (uncap || !(hz >= 1 && hz <= nativeHz));
  buildSeg(document.querySelector('[data-setting="frame_mode"]'), [["smooth", "Smooth"], ["zero_latency", "Low latency"]], s.frame_mode || "smooth", (v) => save({ frame_mode: v }), () => !gen, "Only used when frames are generated");
  $("mode-hint").textContent = !gen ? `Only used when frames are generated (frame generation on, and a display above the stream's ${nativeHz} Hz).` : (s.frame_mode === "smooth" ? "Interpolated between real frames: the smoothest picture. It adds about one frame of delay (17 ms at 60 fps), which is still low enough to feel responsive; camera prediction hides most of it when you look around." : "Lowest delay: real frames are shown the instant they arrive and extra frames are predicted forward. Nothing is added, but fast edges can show small artifacts.");
  setSwitch("adapt-switch", s.adaptive_fps !== false, (on) => save({ adaptive_fps: on }), gpuOk, "Needs the GPU, not available on this device");
  setSwitch("buffer-switch", s.pacing_buffer !== false, (on) => save({ pacing_buffer: on }), gpuOk && s.frame_mode !== "zero_latency", gpuOk ? "Only used in Smooth mode" : "Needs the GPU, not available on this device");
  setSwitch("uncap-switch", uncap, (on) => save({ uncapped_rate: on }), gpuOk && fgOn, gpuOk ? "Needs frame generation" : "Needs the GPU, not available on this device");
  const dbg = $("debug-switch");
  dbg.setAttribute("aria-checked", String(!!s.debug));
  dbg.onclick = () => save({ debug: !s.debug });
  paintEnginePop();
  const imm = $("imm-switch");
  imm.setAttribute("aria-checked", String(!!s.immersion));
  imm.setAttribute("aria-disabled", String(!metal));
  imm.title = metal ? "" : "Needs the GPU, not available on this device";
  imm.onclick = () => { if (metal) save({ immersion: !s.immersion }); };
  paintImmersion();
  const cam = $("cam-switch");
  cam.setAttribute("aria-checked", String(!!s.camera_comp));
  cam.setAttribute("aria-disabled", String(!metal));
  cam.title = metal ? "" : "Needs the GPU, not available on this device";
  cam.onclick = () => { if (metal) save({ camera_comp: !s.camera_comp }); };
  const vol = $("volume");
  vol.value = String(s.volume ?? 100);
  paintVolume();
  const hdr = $("hdr-switch");
  hdr.setAttribute("aria-checked", String(!!s.hdr));
  hdr.setAttribute("aria-disabled", String(!s.hevc));
  hdr.title = s.hevc ? "" : "Needs HEVC: switch the codec to HEVC first";
  hdr.onclick = () => { if (s.hevc) save({ hdr: !s.hdr }); };
}
function paintVolume() {
  const v = $("volume");
  v.style.setProperty("--fill", `${v.value}%`);
  $("volume-out").textContent = `${v.value}%`;
  v.setAttribute("aria-valuetext", `${v.value} percent`);
}
let volRaf = 0;
$("volume").addEventListener("input", () => {
  paintVolume();
  if (!volRaf) volRaf = requestAnimationFrame(() => { volRaf = 0; call("set_volume", { percent: Number($("volume").value) }); }); // instant feedback, even mid-session
});
$("volume").addEventListener("change", () => save({ volume: Number($("volume").value) }));

function rangeSetting(rangeId, outId, resetId, value, dflt, onFinal) {
  const r = $(rangeId), o = $(outId);
  r.value = String(value);
  o.textContent = `${value} %`;
  r.oninput = () => { o.textContent = `${r.value} %`; };
  r.onchange = () => onFinal(Number(r.value));
  $(resetId).onclick = () => { r.value = String(dflt); o.textContent = `${dflt} %`; onFinal(dflt); };
}
// A filter's slider can be used only while its switch is on (and its needs are met); it keeps its value while off.
function setRangeEnabled(prefix, enabled) {
  $(`${prefix}-range`).disabled = !enabled;
  $(`${prefix}-reset`).disabled = !enabled;
  $(`${prefix}-field`)?.classList.toggle("is-off", !enabled);
}
// A switch (a button with role=switch): its state, whether it can be used (and why not), and what flipping it does.
function setSwitch(id, on, onToggle, usable = true, why = "") {
  const b = $(id);
  b.setAttribute("aria-checked", String(!!on));
  b.setAttribute("aria-disabled", String(!usable));
  b.title = usable ? "" : why;
  b.onclick = () => { if (usable) onToggle(!on); };
}
// Saves go through one queue. The UI changes at once (S.settings is the latest local state); each request sends the LATEST local state, so
// quick successive changes are merged and none is lost. S.saved is the last state the backend accepted: an error rolls back to it.
let saveChain = Promise.resolve(), saveBusy = 0, saveTimer = 0;
function saveState(text) {
  const n = $("save-state");
  if (!n) return;
  clearTimeout(saveTimer);
  n.textContent = text;
  n.classList.toggle("on", !!text);
  if (text === "Saved") saveTimer = setTimeout(() => { n.classList.remove("on"); n.textContent = ""; }, 1800);
}
function save(patch, quiet = false) {
  const next = { ...S.settings, ...patch };
  next.width = WIDTH_FOR[next.height];
  S.settings = next;
  if (!S.saved) S.saved = { ...next };
  drawSettings();
  saveBusy++;
  if (!quiet) saveState("Saving…");
  saveChain = saveChain.then(async () => {
    const out = { ...S.settings };
    let e = null;
    if (JSON.stringify(out) !== JSON.stringify(S.saved)) [e] = await call("set_settings", { settings: out });
    saveBusy--;
    if (e) {
      S.settings = { ...S.saved };
      saveState("");
      toast(`Not saved: ${e}`, "error");
      drawSettings();
    } else {
      S.saved = out;
      if (!saveBusy && !quiet) saveState("Saved");
    }
  });
  return saveChain;
}

// ---- about ----------------------------------------------------------------------------------------------------------
function renderAbout() {
  $("about-version").textContent = `Version ${S.info.version}`;
  const rows = [
    ["Mode", S.info.demo ? "Demo (built-in pretend console)" : "Live"],
    ["PSN sign-in", S.info.psn ? "Available" : "Not included in this build"],
    ["HEVC decoding", S.info.hevc ? "Hardware" : "Not available"],
    ["Data folder", S.info.data_dir || "—"],
    ["Privacy", "Everything the app stores (pairings, settings, keys, logs) lives in the data folder above; there is no keychain use and no cache. The system itself creates a few empty helper folders for the web view in its temp folder; they hold no data."],
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
  const flowOnly = !!S.flow && !busy();
  const active = busy() || flowOnly;
  stage.hidden = !active;
  const streaming = s.state === "connected";
  document.body.dataset.stream = streaming ? "on" : "off";
  $("connecting").hidden = streaming || !active;
  $("hud").hidden = !streaming;
  paintDebugBar();
  if (!active || streaming) { clearInterval(S.connTimer); S.connStart = 0; $("connect-hint").hidden = true; }
  if (active && !streaming && !S.connStart) {
    S.connStart = Date.now();
    clearInterval(S.connTimer);
    S.connTimer = setInterval(() => {
      const secs = Math.floor((Date.now() - S.connStart) / 1000);
      $("connect-hint").hidden = secs < 8 || !!S.flow;
      $("connect-hint").textContent = `Still trying (${secs} s). Check that the console is on or in rest mode, on the same network, and that Remote Play is enabled. You can cancel at any time.`;
    }, 1000);
  }
  if (active && !streaming) {
    const idx = STEP_OF[s.state] ?? 0;
    $("steps").hidden = flowOnly;
    $("connecting-title").textContent = flowOnly ? S.flow.title : S.pendingGame ? `Starting ${S.pendingGame.game.name}` : `Connecting to ${consoleName()}`;
    $("connecting-sub").textContent = flowOnly ? S.flow.sub : s.state === "handshaking" && s.attempt > 1 ? `Still trying… (attempt ${s.attempt})` : S.pendingGame ? `Connecting to ${consoleName()}…` : "This usually takes a few seconds.";
    for (const li of $$("#steps li")) {
      const i = Number(li.dataset.step);
      li.dataset.state = i < idx ? "done" : i === idx ? "active" : "";
    }
  }
  if (!streaming) { S.input = { mapping: false, profile: "" }; S.ov = null; $("overlay").hidden = true; }
  if (active && !S.activeId) {
    // the session was not started from this page (reload): ask the backend which console it is
    call("active_console").then(([e, id]) => { if (!e && id) { S.activeId = id; $("hud-name").textContent = consoleName(); } });
  }
  if (streaming) {
    $("hud-name").textContent = consoleName();
    if (!S.hudOn) { muted = false; paintMute(); syncFullscreen(); } // reset only when the stream (re)starts, not on every state refresh
    S.hudOn = true;
    wakeHud();
    if (!S.hintShown) { S.hintShown = true; toast("Press F10 to play with the keyboard and mouse. F11 closes OpenPS, F12 ends the stream.", "info", 9000); }
    pollAudio();
    clearInterval(audioTimer);
    audioTimer = setInterval(pollAudio, 3000);
    pollRender();
    clearInterval(renderTimer);
    renderTimer = setInterval(pollRender, 1000);
  } else {
    S.hudOn = false;
    clearTimeout(hudTimer);
    clearInterval(audioTimer);
    clearInterval(renderTimer);
  }
  renderConsoles();
}
function wakeHud() {
  const hud = $("hud");
  hud.classList.remove("idle");
  document.body.classList.remove("hud-idle");
  clearTimeout(hudTimer);
  hudTimer = setTimeout(() => {
    // unlocked: never hide controls under the pointer. Locked: the pointer is the game's, so hovering must not keep the bar up.
    if (!S.input.mapping && $("hud").querySelector(".hud-bar:hover")) return wakeHud();
    // never hide under an open dialog (focus returns to the bar when it closes, see the close handlers below)
    if (!$("hud").contains(document.activeElement) && !document.querySelector("dialog[open]") && S.session.state === "connected") {
      hud.classList.add("idle");
      document.body.classList.add("hud-idle");
    }
  }, 3000);
}
// While keyboard and mouse control is ON the pointer is locked and every move is game input: it must not pop the bar up.
// (Switching control on or off still shows it once, see paintMapping callers.)
for (const ev of ["mousemove", "pointerdown", "keydown", "touchstart"]) {
  document.addEventListener(ev, () => S.session.state === "connected" && !S.input.mapping && wakeHud(), { passive: true });
}

// "no session is running" just means the session already ended before the click arrived: nothing to report
const notRunning = (e) => /no session/i.test(String(e));
$("cancel-connect").onclick = async () => {
  S.flowId++; S.flow = null; S.wakeRetry = null; S.pendingGame = null;
  if (S.activeId) S.waking.delete(S.activeId);
  renderSession();
  const [e] = await call("disconnect");
  if (e && !notRunning(e)) toast(e, "error");
};
// PS button: pressed while held, so a tap and a long press both behave like the real one.
let psDown = false, psSince = 0, psTimer = 0;
const PS_MIN_MS = 140; // a mouse click can be over in a few milliseconds; the console needs to see the button held for a moment
function psSend(down) {
  psDown = down;
  $("ps-btn").classList.toggle("down", down);
  invoke("ps_button", { down }).catch((e) => { psDown = false; $("ps-btn").classList.remove("down"); if (down) log(`PS button not sent: ${errText(e)}`, "error"); });
}
function psSet(down) {
  if (down) {
    clearTimeout(psTimer);
    if (!psDown) { psSince = performance.now(); psSend(true); }
  } else if (psDown) {
    const wait = Math.max(0, PS_MIN_MS - (performance.now() - psSince));
    clearTimeout(psTimer);
    if (wait > 0) psTimer = setTimeout(() => psSend(false), wait); else psSend(false);
  }
}
{
  const b = $("ps-btn");
  b.addEventListener("pointerdown", (ev) => {
    if (ev.button !== 0) return;
    try { b.setPointerCapture(ev.pointerId); } catch { /* capture is a nicety (keeps the release); the press must never depend on it */ }
    psSet(true);
  });
  for (const t of ["pointerup", "pointercancel", "lostpointercapture"]) b.addEventListener(t, () => psSet(false));
  b.addEventListener("keydown", (ev) => { if ((ev.key === "Enter" || ev.key === " ") && !ev.repeat) { ev.preventDefault(); psSet(true); } });
  b.addEventListener("keyup", (ev) => { if (ev.key === "Enter" || ev.key === " ") psSet(false); });
  b.addEventListener("blur", () => psSet(false));
  window.addEventListener("blur", () => psSet(false));
}
// Leaving a session asks what should happen to the console: stay on, or go to rest mode.
function chooseEnd() {
  const d = $("end-dialog");
  d.returnValue = "cancel";
  return new Promise((resolve) => {
    d.addEventListener("close", () => resolve(d.returnValue), { once: true });
    d.showModal();
  });
}
for (const b of $$("[data-end]")) b.addEventListener("click", () => $("end-dialog").close(b.dataset.end));
$("end-btn").onclick = async () => {
  if ($("end-dialog").open) return;
  psSet(false);
  const choice = await chooseEnd();
  if (choice !== "disconnect" && choice !== "rest") return; // keep playing
  setLoading($("end-btn"), true);
  const [e] = await call(choice === "rest" ? "rest_console" : "disconnect");
  setLoading($("end-btn"), false);
  if (e && !notRunning(e)) toast(e, "error");
  else if (!e && choice === "rest") toast("Rest mode requested. The console will power down.", "info");
};

// The window can enter or leave full screen without us (green button, Ctrl+Cmd+F): ask the window, never trust the flag.
const appWindow = () => window.__TAURI__.window.getCurrentWindow();
// A tool button keeps its icon and caption; only the icon's picture and the caption's words change.
function paintTool(b, icon, caption) {
  const u = b.querySelector("use");
  if (u && icon) u.setAttribute("href", `#i-${icon}`);
  const c = b.querySelector(".cap");
  if (c && caption !== undefined) c.textContent = caption;
}
function paintFullscreen() {
  const b = $("fs-btn"), on = !!S.fullscreen;
  b.setAttribute("aria-pressed", String(on));
  b.setAttribute("aria-label", on ? "Exit full screen" : "Full screen");
  b.title = on ? "Exit full screen (F)" : "Full screen (F)";
  paintTool(b, on ? "shrink" : "expand", on ? "Exit full" : "Full screen");
}
async function syncFullscreen() {
  try { S.fullscreen = await appWindow().isFullscreen(); } catch { /* keep the last known value */ }
  paintFullscreen();
}
async function setFullscreen(on) {
  try {
    await appWindow().setFullscreen(on);
    S.fullscreen = on;
    paintFullscreen();
  } catch (e) {
    toast(`Full screen is not available: ${errText(e)}`, "error");
  }
}
async function toggleFullscreen() { await syncFullscreen(); return setFullscreen(!S.fullscreen); }
{
  let t = 0;
  window.addEventListener("resize", () => { clearTimeout(t); t = setTimeout(syncFullscreen, 200); });
}
for (const id of ["end-dialog", "switch-dialog"]) $(id).addEventListener("close", () => S.session.state === "connected" && wakeHud());
let muted = false;
function paintMute() {
  const b = $("mute-btn");
  b.setAttribute("aria-pressed", String(muted));
  b.setAttribute("aria-label", muted ? "Unmute" : "Mute");
  b.title = muted ? "Unmute (M)" : "Mute (M)";
  paintTool(b, muted ? "mute" : "volume", muted ? "Muted" : "Sound");
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

$("fs-btn").onclick = toggleFullscreen;
document.addEventListener("keydown", (ev) => {
  // while keys & mouse control the game, every key belongs to the game: the app's own shortcuts stand down
  if (S.session.state !== "connected" || S.input.mapping || (S.ov && S.ov.visible) || ev.metaKey || ev.ctrlKey || ev.altKey) return;
  if (ev.key === "f" || ev.key === "F") { ev.preventDefault(); toggleFullscreen(); }
  else if (ev.key === "m" || ev.key === "M") { ev.preventDefault(); toggleMute(); }
  else if (ev.key === "Escape" && S.fullscreen && !$("hud").classList.contains("idle")) setFullscreen(false);
});

// ---- keyboard and mouse -> console (the engine runs natively; the web view only forwards raw events) ----------------------
const streaming = () => S.session.state === "connected";
const onHud = (t) => t instanceof Element && !!t.closest(".hud-bar");
function sendKey(ev, down) {
  if (!streaming() || ev.isComposing) return;
  if (down && ev.repeat && S.ov && S.ov.visible && (ev.key === "Enter" || ev.key === " ") && ev.target instanceof Element && ev.target.closest("#ov-rows, #ov-tabs")) { ev.preventDefault(); return; }
  invoke("input_key", { code: ev.code, down, repeat: ev.repeat }).catch(() => {});
  // the game owns the keyboard while mapping is on (and Cmd shortcuts such as Cmd+Q still reach macOS)
  const typingIntoEditor = S.ov && S.ov.visible && !S.ov.mapping && (S.ov.capturing || S.ov.renaming);
  const filtering = document.activeElement === $("ov-filter") && !typingIntoEditor;
  if ((S.input.mapping || typingIntoEditor) && !ev.metaKey && !filtering) ev.preventDefault();
}
document.addEventListener("keydown", (ev) => sendKey(ev, true), true);
document.addEventListener("keyup", (ev) => sendKey(ev, false), true);
const onOverlay = (t) => t instanceof Element && !!t.closest("#overlay");
function sendButton(ev, down) {
  if (onOverlay(ev.target)) { // a click on the editor itself is never a binding
    if (down) invoke("input_ui", { event: { type: "overlay_click" } }).catch(() => {});
    return;
  }
  if (!streaming() || (!S.input.mapping && onHud(ev.target))) return; // HUD buttons keep working when mapping is off
  invoke("input_button", { button: ev.button, down }).catch(() => {});
  if (S.input.mapping) ev.preventDefault();
}
document.addEventListener("mousedown", (ev) => sendButton(ev, true), true);
document.addEventListener("mouseup", (ev) => sendButton(ev, false), true);
document.addEventListener("contextmenu", (ev) => { if (streaming() && S.input.mapping) ev.preventDefault(); }, true);
document.addEventListener("wheel", (ev) => {
  if (!streaming() || !S.input.mapping) return;
  invoke("input_wheel", { dx: ev.deltaX, dy: ev.deltaY }).catch(() => {});
  ev.preventDefault();
}, { capture: true, passive: false });

function paintMapping() {
  const b = $("map-btn");
  b.setAttribute("aria-pressed", String(S.input.mapping));
  const mapLabel = S.input.mapping ? `Keys & mouse: on${S.input.profile ? " · " + S.input.profile : ""}` : "Keys & mouse: off";
  // the tile says "Controls" and lights up when on; the profile name is in the tooltip and the accessible name (captions can be hidden)
  b.title = S.input.notice ? S.input.notice : `${mapLabel} (F10)`;
  b.setAttribute("aria-label", mapLabel);
  paintControllerMode();
}
$("map-btn").onclick = () => invoke("input_toggle").catch(() => {});
// ---- the key-map editor ----------------------------------------------------------------------------------------------------
const ui = (event) => invoke("input_ui", { event }).catch(() => {});
const nice = (c) => c.replaceAll("_", " ").toLowerCase();
// The console's own button symbols instead of words. The name stays as hidden text for screen readers and as a tooltip.
// Keys and mouse buttons as little key caps and mouse pictures; the plain name stays as the tooltip and for screen readers.
const KEYCAP = {
  SPACE: "space", RETURN: "⏎", KP_ENTER: "⌅", ESCAPE: "⎋", TAB: "⇥", BACKSPACE: "⌫", FORWARD_DELETE: "⌦", CAPS_LOCK: "⇪",
  LEFT_SHIFT: "⇧", RIGHT_SHIFT: "⇧", LEFT_CONTROL: "⌃", RIGHT_CONTROL: "⌃", LEFT_OPTION: "⌥", RIGHT_OPTION: "⌥", LEFT_COMMAND: "⌘", RIGHT_COMMAND: "⌘", FN: "fn",
  UP_ARROW: "↑", DOWN_ARROW: "↓", LEFT_ARROW: "←", RIGHT_ARROW: "→", PAGE_UP: "⇞", PAGE_DOWN: "⇟", HOME: "↖", END: "↘",
  MINUS: "−", EQUAL: "=", GRAVE: "`", LEFT_BRACKET: "[", RIGHT_BRACKET: "]", BACKSLASH: "\\", SEMICOLON: ";", QUOTE: "'", COMMA: ",", PERIOD: ".", SLASH: "/",
  KP_PLUS: "+", KP_MINUS: "−", KP_MULTIPLY: "×", KP_DIVIDE: "÷", KP_DECIMAL: ".", KP_EQUALS: "=", VOLUME_UP: "🔊", VOLUME_DOWN: "🔉", VOLUME_MUTE: "🔇",
};
const MOUSE = { MOUSE_LEFT: "left", MOUSE_RIGHT: "right", MOUSE_MIDDLE: "middle", MOUSE_BUTTON4: "side", MOUSE_BUTTON5: "side", SCROLL_UP: "up", SCROLL_DOWN: "down", SCROLL_LEFT: "up", SCROLL_RIGHT: "down" };
const MOUSE_BADGE = { MOUSE_BUTTON4: "4", MOUSE_BUTTON5: "5", SCROLL_LEFT: "←", SCROLL_RIGHT: "→" }; // two bindings must never look identical
function keycap(name) {
  if (MOUSE[name]) return el("span", { className: "kcap mouse", attrs: { title: nice(name) } }, gsvg(`m-${MOUSE[name]}`), MOUSE_BADGE[name] ? el("i", { className: "mbadge", textContent: MOUSE_BADGE[name] }) : null);
  let t = KEYCAP[name];
  if (t === undefined) t = /^KEY_\d$/.test(name) ? name.slice(4) : /^KP_\d$/.test(name) ? name.slice(3) : name;
  if (t.length > 3) t = t.replaceAll("_", " ").toLowerCase();
  return el("span", { className: "kcap" + (name === "SPACE" ? " word wide" : t.length > 1 && /^[A-Za-z0-9 ]+$/.test(t) && !/^F\d+$/.test(t) ? " word" : ""), textContent: t, attrs: { title: nice(name) } });
}
function chordEl(text) {
  const wrap = el("span", { className: "chord" });
  text.split(", ").forEach((chord, i) => {
    if (i) wrap.append(el("i", { className: "or" }));
    chord.split("+").forEach((k, j) => { if (j) wrap.append(el("i", { className: "plus", textContent: "+" })); wrap.append(keycap(k)); });
  });
  return wrap;
}

function glyph(control) {
  const c = control.toLowerCase();
  const wrap = el("span", { className: "glyph", attrs: { title: nice(control) } });
  const dir = (c.match(/_(up|down|left|right)$/) || [])[1];
  let main = c, extra = "";
  if (c.startsWith("dpad_")) main = `dpad-${dir}`;
  else if (c.startsWith("left_stick")) { main = "ls"; extra = dir; }
  else if (c.startsWith("right_stick")) { main = "rs"; extra = dir; }
  else if (c === "touchpad_tap") { main = "touchpad"; extra = "tap"; }
  else if (c.startsWith("touchpad_swipe")) { main = "touchpad"; extra = dir; }
  wrap.append(gsvg(main));
  if (extra && extra !== "tap") wrap.append(gsvg("arrow", `dir dir-${extra}`));
  if (extra === "tap") wrap.append(el("i", { className: "tap-dot" }));
  wrap.append(el("span", { className: "sr-only", textContent: nice(control) }));
  return wrap;
}
// Patching helpers: assigning an unchanged value would still be a DOM mutation, so every write is compared first.
const setHidden = (n, h) => { if (n.hidden !== h) n.hidden = h; };
const ovRows = new Map(); // control -> { tr, cells: [cell, cell], hay }
const ovTabs = new Map(); // profile name -> tab button
let ovTabSeq = 0, ovTabSel = "", ovCapKey = "", ovFactsKey = "";
const slotName = (slot) => (slot === 1 ? "key" : "alternate");
function bindCell(control, slot) {
  const b = el("button", { type: "button", className: "bind" });
  b.onclick = () => S.ov && !S.ov.mapping && ui({ type: "edit", control, slot });
  const x = el("button", { type: "button", className: "clear" }, svg("close"));
  x.onclick = () => { // the X is about to be disabled: keep the keyboard user on the cell
    if (S.ov && !S.ov.mapping) { b.focus({ preventScroll: true }); ui({ type: "clear", control, slot }); }
  };
  return { td: el("td", {}, el("div", { className: "slot" }, b, x)), b, x, state: "" };
}
function paintCell(c, control, slot, text, capturing, editable) {
  const state = `${text}|${capturing}|${editable}`;
  if (c.state === state) return;
  const empty = text === "-";
  if (!c.state || c.state.split("|")[0] !== text) {
    c.b.replaceChildren(empty ? el("span", { className: "dash", textContent: "—" }) : chordEl(text));
    c.b.title = empty ? "Not set" : nice(text);
    c.x.title = `Clear ${nice(control)} ${slotName(slot)}`;
    setAttr(c.x, "aria-label", c.x.title);
  }
  c.state = state;
  c.b.className = "bind" + (empty ? " empty" : "");
  setAttr(c.b, "aria-pressed", String(capturing));
  setAttr(c.b, "aria-disabled", String(!editable));
  setAttr(c.b, "aria-label", `${nice(control)} ${slotName(slot)}: ${empty ? "not set" : nice(text)}. ${capturing ? "Listening for a key. Press Escape to cancel." : "Activate to change."}`);
  c.x.disabled = empty || !editable;
  c.td.firstChild.classList.toggle("is-empty", empty);
}
function paintTabs(o, editable) {
  const tabs = $("ov-tabs"), seen = new Set();
  let i = 0, selNode = null;
  for (const t of o.profile_tabs) {
    seen.add(t.name);
    let b = ovTabs.get(t.name);
    if (!b) {
      b = el("button", { type: "button", id: `ov-tab-${++ovTabSeq}`, attrs: { role: "tab", "aria-controls": "ov-panel" } }, el("span"), el("small"));
      b.onclick = () => S.ov && !S.ov.mapping && ui({ type: "profile_select", name: t.name });
      ovTabs.set(t.name, b);
    }
    setText(b.firstChild, t.name);
    setText(b.lastChild, t.slot ? String(t.slot) : "");
    setHidden(b.lastChild, !t.slot);
    const label = t.slot ? `${t.name} (${t.slot})` : t.name;
    if (b.title !== label) { b.title = label; setAttr(b, "aria-label", label); }
    setAttr(b, "aria-selected", String(!!t.active));
    setAttr(b, "tabindex", t.active ? "0" : "-1");
    setAttr(b, "aria-disabled", String(!editable));
    if (t.active) { selNode = b; setAttr($("ov-panel"), "aria-labelledby", b.id); }
    if (tabs.children[i] !== b) tabs.insertBefore(b, tabs.children[i] || null);
    i++;
  }
  for (const [name, b] of ovTabs) if (!seen.has(name)) { b.remove(); ovTabs.delete(name); }
  if (!selNode && tabs.firstChild) setAttr(tabs.firstChild, "tabindex", "0"); // never leave the strip without a tab stop
  const sel = selNode ? selNode.id : "";
  if (sel !== ovTabSel) { ovTabSel = sel; selNode?.scrollIntoView({ inline: "nearest", block: "nearest" }); }
}
$("ov-tabs").addEventListener("keydown", (ev) => {
  const list = [...$("ov-tabs").children], i = list.indexOf(document.activeElement);
  if (i < 0 || ev.metaKey || ev.ctrlKey || ev.altKey) return;
  const to = { ArrowRight: (i + 1) % list.length, ArrowLeft: (i - 1 + list.length) % list.length, Home: 0, End: list.length - 1 }[ev.key];
  if (to === undefined) return;
  ev.preventDefault();
  list[to].focus(); // manual activation: Enter/Space selects, so arrowing never switches profile on every step
  list[to].scrollIntoView({ inline: "nearest", block: "nearest" });
});
function applyFilter() {
  const f = $("ov-filter").value.trim().toLowerCase();
  let shown = 0;
  for (const r of ovRows.values()) { const ok = !f || r.hay.includes(f); setHidden(r.tr, !ok); if (ok) shown++; }
  const empty = $("ov-empty");
  if (empty) { setHidden(empty, shown > 0); setText(empty.firstChild, f ? `No control matches "${$("ov-filter").value.trim()}"` : "No controls"); }
}
let ovWasOpen = false;
function renderOverlay() {
  const o = S.ov, box = $("overlay");
  if (ovWasOpen && !(o && o.visible)) loadGameMaps().then(() => buildGames()); // the profile list may have changed
  ovWasOpen = !!(o && o.visible);
  const show = !!(o && o.visible && streaming());
  document.body.classList.toggle("ov-open", show);
  if (box.hidden === show) {
    const had = !show && box.contains(document.activeElement);
    box.hidden = !show;
    if (show) $("ov-close").focus({ preventScroll: true });
    else if (had) $("edit-keys-btn").focus({ preventScroll: true }); // focus must not fall back to <body>
  }
  if (!show) return;
  const editable = !o.mapping;
  setText($("ov-mode"), editable ? "Editing" : "Mapping on");
  $("ov-mode").classList.toggle("chip-ok", !editable);
  setHidden($("ov-lock"), editable);
  setText($("ov-notice"), o.notice || "");
  paintTabs(o, editable);
  for (const id of ["ov-add", "ov-rename", "ov-reset", "ov-delete"]) if ($(id).disabled === editable) $(id).disabled = !editable;
  setHidden($("ov-renaming"), !o.renaming);
  if (o.renaming) setText($("ov-rename-text"), o.renaming.text + "▏");
  // sensitivity (don't fight the user's thumb while dragging)
  const sens = $("ov-sens");
  if (sens.min !== String(o.sensitivity.min)) sens.min = o.sensitivity.min;
  if (sens.max !== String(o.sensitivity.max)) sens.max = o.sensitivity.max;
  if (document.activeElement !== sens) sens.value = o.sensitivity.value;
  setText($("ov-sens-out"), Number(sens.value).toFixed(3));
  if (sens.disabled === editable) sens.disabled = !editable;
  // capture banner
  const cap = o.capturing;
  setHidden($("ov-capture"), !cap);
  const capKey = cap ? `${cap.control}/${cap.slot}` : "";
  if (capKey !== ovCapKey && cap) $("ov-cap-what").replaceChildren("Binding ", glyph(cap.control), ` (${slotName(cap.slot)})…`);
  // rows: one keyed <tr> per control, patched in place
  const body = $("ov-rows"), seen = new Set();
  let i = 0;
  for (const r of o.rows) {
    seen.add(r.control);
    let row = ovRows.get(r.control);
    if (!row) {
      const cells = [bindCell(r.control, 1), bindCell(r.control, 2)];
      row = { tr: el("tr", {}, el("td", { className: "ctl" }, glyph(r.control)), cells[0].td, cells[1].td), cells, hay: "" };
      ovRows.set(r.control, row);
    }
    row.hay = `${nice(r.control)}\n${r.primary.toLowerCase()}\n${r.secondary.toLowerCase()}`;
    paintCell(row.cells[0], r.control, 1, r.primary, !!cap && cap.control === r.control && cap.slot === 1, editable);
    paintCell(row.cells[1], r.control, 2, r.secondary, !!cap && cap.control === r.control && cap.slot === 2, editable);
    if (body.children[i] !== row.tr) body.insertBefore(row.tr, body.children[i] || null);
    i++;
  }
  for (const [c, row] of ovRows) if (!seen.has(c)) { row.tr.remove(); ovRows.delete(c); }
  if (!$("ov-empty")) body.append(el("tr", { id: "ov-empty", className: "ov-empty", hidden: true }, el("td", { colSpan: 3, attrs: { role: "status" } })));
  else if (body.lastChild.id !== "ov-empty") body.append($("ov-empty"));
  applyFilter();
  if (capKey !== ovCapKey) { // a capture began or moved: bring its cell into view once the banner has taken its space
    ovCapKey = capKey;
    const c = cap && ovRows.get(cap.control)?.cells[cap.slot - 1];
    if (c) requestAnimationFrame(() => c.b.scrollIntoView({ block: "nearest" }));
  }
  const facts = JSON.stringify([o.mouse, o.hotkeys]);
  if (facts !== ovFactsKey) {
    ovFactsKey = facts;
    const row = ([k, v]) => el("div", {}, el("dt", { textContent: k }), el("dd", { textContent: v }));
    $("ov-mouse").replaceChildren(...o.mouse.map(row));
    $("ov-hotkeys").replaceChildren(...o.hotkeys.map(row));
  }
}
$("ov-close").onclick = () => invoke("input_overlay").catch(() => {});
$("ov-off").onclick = () => invoke("input_toggle").catch(() => {});
$("edit-keys-btn").onclick = () => invoke("input_overlay").catch(() => {});
// A new key map starts as a copy of one that exists: the one being edited is highlighted (the default one when nothing else was picked).
function chooseCopySource(names, current) {
  const dlg = $("copy-dialog"), list = $("copy-list");
  let pick = names.includes(current) ? current : names[0];
  const rows = names.map((n) => {
    const b = el("button", { type: "button", className: "copy-row", textContent: n, attrs: { role: "radio", "aria-checked": String(n === pick) } });
    if (n === names[0]) b.append(el("small", { textContent: "default" }));
    b.onclick = () => { pick = n; rows.forEach((r) => r.setAttribute("aria-checked", String(r === b))); };
    b.ondblclick = () => dlg.close("ok");
    return b;
  });
  list.replaceChildren(...rows);
  return new Promise((resolve) => {
    dlg.onclose = () => resolve(dlg.returnValue === "ok" ? pick : null);
    for (const b of dlg.querySelectorAll("[data-copy]")) b.onclick = () => dlg.close(b.dataset.copy);
    dlg.returnValue = "cancel";
    dlg.showModal();
    (rows.find((r) => r.getAttribute("aria-checked") === "true") || rows[0]).focus();
  });
}
$("ov-add").onclick = async () => {
  if (!S.ov || S.ov.mapping) return;
  const names = S.ov.profiles || [];
  const source = await chooseCopySource(names, S.ov.profile);
  if (source) ui({ type: "profile_add", source });
};
$("ov-reset").onclick = async () => { if (await confirmDialog(`Reset ${S.ov.profile}?`, "Every binding in this profile goes back to its default.", "Reset")) ui({ type: "profile_reset" }); };
$("ov-delete").onclick = async () => { if (await confirmDialog(`Delete ${S.ov.profile}?`, "This profile and its bindings are removed.", "Delete")) ui({ type: "profile_delete", name: S.ov.profile }); };
$("ov-rename").onclick = () => ui({ type: "profile_rename", name: S.ov.profile });
$("ov-cancel").onclick = () => ui({ type: "cancel" });
for (const b of $$("[data-pick]")) b.onclick = () => ui({ type: "pick", input: b.dataset.pick });
$("ov-filter").addEventListener("input", applyFilter);
let sensTimer = 0;
$("ov-sens").addEventListener("input", () => {
  $("ov-sens-out").textContent = Number($("ov-sens").value).toFixed(3);
  clearTimeout(sensTimer);
  sensTimer = setTimeout(() => ui({ type: "slider", value: Number($("ov-sens").value), final: false }), 30);
});
$("ov-sens").addEventListener("change", () => ui({ type: "slider", value: Number($("ov-sens").value), final: true }));

function onInputEvent(e) {
  const o = e.overlay || {};
  S.ov = e.overlay || null;
  renderOverlay();
  const was = S.input.mapping;
  S.input = { mapping: !!o.mapping, profile: o.profile || "", notice: o.notice || "" };
  paintMapping();
  $("hud").classList.toggle("locked", S.input.mapping);
  if (was !== S.input.mapping && streaming()) {
    wakeHud(); // the cursor is released (or hidden): show where the controls are
    // say so at the top of the stream, where it is seen (F10 or the Controls tile)
    toast(S.input.mapping ? `Keyboard and mouse on${S.input.profile ? ` · ${S.input.profile.replaceAll("_", " ")}` : ""}` : "Keyboard and mouse off", "info", 2600);
  }
  if (o.notice) log(`input: ${o.notice}`);
}

// The numbers at the top of the stream can be hidden (the choice is remembered).
let statsOn = true;
try { statsOn = localStorage.getItem("openps.stats") !== "off"; } catch { /* storage unavailable: keep them on */ }
function paintStats() {
  $("stats-btn").setAttribute("aria-pressed", String(statsOn));
  $("stats-btn").title = statsOn ? "Hide the stream numbers" : "Show the stream numbers";
  document.querySelector(".hud-top").classList.toggle("quiet", !statsOn);
}
$("stats-btn").onclick = () => {
  statsOn = !statsOn;
  try { localStorage.setItem("openps.stats", statsOn ? "on" : "off"); } catch { /* not remembered */ }
  paintStats();
};
paintStats();
// The top bar can be pinned: it stays on screen when the controls fade away.
let statsPinned = false;
try { statsPinned = localStorage.getItem("openps.pin") === "on"; } catch { /* storage unavailable: not pinned */ }
function paintPin() {
  const b = $("pin-btn");
  b.setAttribute("aria-pressed", String(statsPinned));
  b.setAttribute("aria-label", statsPinned ? "Let the top bar fade away" : "Keep the top bar on screen");
  b.title = b.getAttribute("aria-label");
  $("hud").classList.toggle("top-pinned", statsPinned);
}
$("pin-btn").onclick = () => {
  statsPinned = !statsPinned;
  try { localStorage.setItem("openps.pin", statsPinned ? "on" : "off"); } catch { /* not remembered */ }
  paintPin();
};
paintPin();

// Records what the console sends for 30 s. Pressed during a stream it starts at once; pressed with no stream it waits for the next one.
// A timer shows at the top left of the stream while it runs, and a message when it is done.
const DIAG_SECONDS = 30;
let diagTick = 0, diagHide = 0;
function diagShow(text, cls) {
  const box = $("diag-timer");
  unfade(box);
  box.hidden = false;
  box.className = `diag-timer ${cls || ""}`;
  $("diag-timer-text").textContent = text;
}
function paintDiagBtn() {
  const b = $("diag-btn");
  b.disabled = !!S.diag;
  b.textContent = S.diag?.armed ? "Waiting for a stream…" : S.diag ? "Recording…" : `Record ${DIAG_SECONDS} s`;
  b.setAttribute("aria-busy", String(!!S.diag));
}
async function runDiag() {
  const until = Date.now() + DIAG_SECONDS * 1000;
  S.diag = { until };
  clearTimeout(diagHide);
  const left = () => Math.max(0, Math.ceil((until - Date.now()) / 1000));
  diagShow(`Recording ${left()} s`);
  clearInterval(diagTick);
  diagTick = setInterval(() => diagShow(`Recording ${left()} s`), 250);
  paintDiagBtn();
  const [e, text] = await call("frame_diagnostic", { seconds: DIAG_SECONDS });
  clearInterval(diagTick);
  S.diag = null;
  paintDiagBtn();
  if (e) {
    diagShow("Recording failed", "bad");
    toast(`Not recorded: ${e}`, "error", 7000);
  } else {
    $("diag-out").textContent = text;
    log(`frame diagnostic: ${text}`);
    diagShow("Recording completed", "ok");
    toast(`Recording completed. ${text}`, "ok", 9000);
  }
  diagHide = setTimeout(() => fadeOut($("diag-timer"), () => { $("diag-timer").hidden = true; }), 8000);
}
$("diag-btn").onclick = () => {
  if (S.diag) return;
  if (S.session.state === "connected") return runDiag();
  S.diag = { armed: true };
  paintDiagBtn();
  toast(`Open a stream and it will record ${DIAG_SECONDS} s by itself.`, "info", 5000);
};
// an armed recording starts a few seconds after the next stream connects (the first frames need to be flowing)
function diagOnConnected() {
  if (S.diag?.armed) setTimeout(() => { if (S.diag?.armed && S.session.state === "connected") { S.diag = null; runDiag(); } }, 3000);
}

// ---- help popovers: a small "?" that explains on hover or focus, and stays open when clicked ---------------------------------------------------
document.addEventListener("click", (ev) => {
  const btn = ev.target.closest?.(".help-btn");
  for (const h of $$(".help.open")) if (!btn || h !== btn.parentElement) { h.classList.remove("open"); h.querySelector(".help-btn").setAttribute("aria-expanded", "false"); }
  if (!btn) return;
  btn.parentElement.classList.remove("dismissed");
  const open = btn.parentElement.classList.toggle("open");
  btn.setAttribute("aria-expanded", String(open));
});
// Esc closes every explanation that is showing, also one that is only there because the "?" has focus or the pointer is over it;
// it stays closed until the pointer leaves or focus moves away.
document.addEventListener("keydown", (ev) => {
  if (ev.key !== "Escape") return;
  for (const h of $$(".help")) {
    if (!h.matches(".open, :hover, :focus-within")) continue;
    h.classList.remove("open");
    h.classList.add("dismissed");
    h.querySelector(".help-btn").setAttribute("aria-expanded", "false");
  }
});
const undismiss = (ev) => {
  const h = ev.target.closest?.(".help.dismissed");
  if (h && !h.contains(ev.relatedTarget)) h.classList.remove("dismissed");
};
document.addEventListener("mouseout", undismiss);
document.addEventListener("focusout", undismiss);
// A popover is laid out even while it is hidden, so one that reaches past the right edge of the page makes the whole page scroll sideways
// (the "?" next to a control on the right, in a narrow window). Each one is moved left, as far as needed, to end inside the page.
function fitHelpPops() {
  const page = document.querySelector("main");
  if (!page) return;
  const edge = page.getBoundingClientRect().left + page.clientWidth - 12;
  for (const pop of $$(".help-pop", page)) {
    const host = pop.parentElement.getBoundingClientRect();
    if (!host.width) continue; // on a page that is not showing
    pop.style.left = "";
    const over = host.left - 5 + pop.offsetWidth - edge;
    if (over > 0) pop.style.left = `${-5 - over}px`;
  }
}
{
  let queued = 0;
  const later = () => { clearTimeout(queued); queued = setTimeout(fitHelpPops, 30); };
  window.addEventListener("resize", later, { passive: true });
  const page = document.querySelector("main");
  if (page) new MutationObserver(later).observe(page, { childList: true, subtree: true, attributeFilter: ["hidden"], attributes: true });
}

// ---- frame pacing readout ----------------------------------------------------------------------------------------------------
let renderTimer = 0;
// The last ten seconds of readings (one a second): what the averages in the top bar are made of.
const STAT_WINDOW_MS = 10000;
let statHist = [];
// `sample` ({ fps, ms }) joins the readings of the last `windowMs`; returns what is left of them and their averages.
function windowAverages(hist, sample, now, windowMs) {
  const kept = [...hist, { ...sample, t: now }].filter((h) => now - h.t < windowMs);
  const mean = (key) => kept.reduce((a, h) => a + h[key], 0) / kept.length;
  return { hist: kept, fps: mean("fps"), ms: mean("ms") };
}
// Every item of the top bar has a width of its own (in characters of the monospace font), so one whose number gains or loses a digit does
// not push the others around.
function fixedWidth(node, chars) {
  node.style.setProperty("--w", `${chars}ch`);
  node.classList.add("fixed");
  return node;
}
async function pollRender() {
  const [, r] = await call("render_stats");
  S.render = r || null;
  paintDebugBar();
  const box = $("hud-render");
  if (!r || r.renderer !== "metal" || !r.presented) { statHist = []; box.replaceChildren(); return; }
  const chips = [];
  // what the display really showed (Metal's presented times); the submit cadence is the fallback
  const shown = r.shown_interval_ms > 0;
  // what the game renders (distinct pictures per second), not what the stream or the display carries
  const fps = r.content_fps > 0 ? r.content_fps : shown ? 1000 / r.shown_interval_ms : 0;
  const jitter = shown ? r.shown_jitter_ms : r.present_jitter_ms;
  // what is on the screen now: its rate and the time between two frames
  const outFps = shown ? 1000 / r.shown_interval_ms : r.target_hz || fps;
  const frameMs = shown ? r.shown_interval_ms : outFps > 0 ? 1000 / outFps : 0;
  const avg = windowAverages(statHist, { fps, ms: frameMs }, Date.now(), STAT_WINDOW_MS);
  statHist = avg.hist;
  const avgFps = avg.fps, avgMs = avg.ms;
  // the frames per second now, then a small icon for each thing that is switched on (the details are in the tooltips)
  const icon = (name, title) => el("span", { className: "ico", title, attrs: { role: "img", "aria-label": title } }, svg(name));
  // the game's own rate only: what frame generation adds is in the frame generation item
  chips.push(fixedWidth(el("span", { textContent: `${Math.round(fps)} fps · avg ${avgFps.toFixed(1)}`, title: `The game renders ${Math.round(fps)} frames per second now; ${avgFps.toFixed(1)} on average over the last ${STAT_WINDOW_MS / 1000} seconds${r.repeat_share > 0.05 ? ` (the stream repeats ${Math.round(r.repeat_share * 100)}% of its frames)` : ""}` }), 18));
  if (r.generating) {
    const mult = Math.round((r.target_hz / Math.max(1, r.content_fps)) * 10) / 10;
    const chip = icon("sparkles", `Frame generation: ${mult}× the game's ${Math.round(r.content_fps)} frames per second, ${Math.round(outFps)} frames per second on a ${Math.round(r.refresh_hz || 0)} Hz display; the extra frames are generated here`);
    chip.append(fixedWidth(el("b", { textContent: `${Math.round(outFps)} fps` }), 7));
    chips.push(chip);
  }
  if (r.upscaled) {
    // the icon with the height the picture is drawn at now (the multiple chosen in settings, or less when the window is smaller)
    const chip = icon("scaling", r.upscale_height > 0 ? `Upscaled by the GPU to ${r.upscale_height}p` : "Upscaled by the GPU");
    if (r.upscale_height > 0) chip.append(fixedWidth(el("b", { textContent: `${r.upscale_height}p` }), 5));
    chips.push(chip);
  }
  if (r.metalfx_active) chips.push(icon("zap", "MetalFX makes the generated frames"));
  if (r.fsr_active) chips.push(icon("sparkles", "FSR (AMD FidelityFX Super Resolution 1) upscales the picture"));
  if (r.adaptive && r.adaptive.on && r.adaptive.cap_hz > 0) {
    // the GPU could not keep the rate that was asked for with every filter on: it is held to a step of 5 fps (lower still on a fixed-refresh display, see heldRate) and the picture is as set
    const held = heldRate(r);
    const chip = icon("info", `Adaptive frame rate: this Mac is held to ${held} fps (${Math.round(r.adaptive.asked_hz)} was asked for) so that every filter can stay on. It goes back up when there is room.`);
    chip.classList.add("warn");
    chip.append(fixedWidth(el("b", { textContent: `${held}` }), 3));
    chips.push(chip);
  }
  chips.push(fixedWidth(el("span", { textContent: `${frameMs.toFixed(1)} ms · avg ${avgMs.toFixed(1)}`, title: `Frame time: the time between two frames on the screen, ${frameMs.toFixed(1)} ms now and ${avgMs.toFixed(1)} ms on average over the last ${STAT_WINDOW_MS / 1000} seconds` }), 19));
  chips.push(fixedWidth(el("span", { className: jitter > 2.5 ? "warn" : "", textContent: `±${jitter.toFixed(1)} ms`, title: `Frame-to-frame timing deviation on screen${shown ? `; longest gap ${r.shown_max_ms.toFixed(1)} ms, ${r.shown_late} late frames in the last two seconds` : ""}` }), 8));
  if (r.camera_active) chips.push(fixedWidth(el("span", { textContent: `cam ${Math.round(r.camera_lag_ms)} ms`, title: "Camera latency compensation is on" }), 10));
  box.replaceChildren(...chips);
  if (!$("engine-pop").hidden) $("engine-now").textContent = engineNow(r);
}

// ---- debug bar ---------------------------------------------------------------------------------------------------------------
// Everything the backend is doing while streaming, each thing with its state: on (green), off by choice (grey), degraded (amber), failing
// (red), not available on this Mac (hollow). Shown only while "Debug tools" is on in Settings; built from the renderer's own statistics
// (the settings it is running with, not the ones the page thinks it sent), the session's, the audio output's and the controller's.
let dbgFolded = false;
try { dbgFolded = localStorage.getItem("openps.dbgfold") === "on"; } catch { /* not remembered */ }
let dbgTimer = 0;
const dbgPct = (x) => `${Math.round((x ?? 0) * 100)} %`;
const dbgMs = (x, d = 1) => `${Number(x ?? 0).toFixed(d)} ms`;
function debugModel() {
  const r = S.render, st = S.settings || {}, info = S.info || {};
  const metal = !!r && r.renderer === "metal";
  const c = (metal && r.config) || {};
  const fg = (metal && r.finish_guard) || {}, ad = (metal && r.adaptive) || {};
  const sec = [];
  const add = (title, rows) => sec.push({ title, rows });
  const upscaling = metal && c.upscale_factor > 0;
  const mult = metal ? Math.round((r.target_hz / Math.max(1, r.content_fps)) * 10) / 10 : 0;

  // what is running
  const run = [];
  run.push(metal ? ["on", "Renderer", "Metal (the GPU draws every picture)"] : ["warn", "Renderer", "the system video view: no Metal statistics, no upscaling or frame generation"]);
  if (metal) {
    run.push(!upscaling ? ["off", "Upscaling", "off"]
      : r.upscaled ? ["on", "Upscaling", `${r.fsr_active ? "FSR (EASU)" : "MetalFX"} to ${r.upscale_height}p (${Math.round(c.upscale_factor * 100) / 100}× of ${st.height}p)`]
      : ["warn", "Upscaling", `asked for ${Math.round(c.upscale_factor * 100) / 100}×, nothing to upscale to yet`]);
    run.push(!r.upscaled ? ["off", "Finishing pass", "off (no upscaled picture)"]
      : r.finish_pass ? ["on", "Finishing pass", `on: ${r.fsr_active ? "" : "colour conversion, "}sharpness blend and adaptive sharpening in one GPU pass`]
      : fg.off ? ["warn", "Finishing pass", "given up by the guard (the GPU was too busy): plain upscaler"] : ["off", "Finishing pass", "off: the blend is made in the present pass"]);
    run.push(!r.upscaled ? ["off", "Linear light", "off (no upscaling)"]
      : r.fsr_active ? ["na", "Linear light", "FSR has no linear-light mode"]
      : st.linear_light === false ? ["off", "Linear light", "off by choice: the scaler's perceptual mode"]
      : r.linear_active ? ["on", "Linear light", "on: the scaler works in linear light (half floats)"]
      : r.linear_unavailable ? ["warn", "Linear light", "this Mac could not make the linear scaler: perceptual mode"]
      : ["off", "Linear light", fg.off ? "off (guard): perceptual mode" : "off: perceptual mode"]);
    run.push(st.sharpness_on === false ? ["off", r.upscaled ? "Sharpness blend" : "Sharpness (unsharp)", "off by choice: the picture is left as the upscaler draws it"]
      : ["on", r.upscaled ? "Sharpness blend" : "Sharpness (unsharp)", r.upscaled ? `${dbgPct(c.sharpness)}: ${r.finish_pass ? "blended in the finishing pass" : "blended in the present pass"} (0 plain, 100 as the upscaler draws it)` : `${dbgPct(c.sharpness)} (50 % leaves the picture alone)`]);
    run.push(!r.upscaled ? ["off", "Adaptive sharpening", "off (needs upscaling)"]
      : st.rcas_on === false ? ["off", "Adaptive sharpening", "off by choice"]
      : r.rcas_active ? ["on", "Adaptive sharpening", `RCAS on, ${dbgPct(c.rcas)} (lobe ${(0.25 * c.rcas).toFixed(3)})`]
      : c.rcas > 0 ? ["warn", "Adaptive sharpening", `set to ${dbgPct(c.rcas)} but not applied (${fg.off ? "guard" : "no finishing pass"})`] : ["off", "Adaptive sharpening", "off (0 %)"]);
    run.push(st.saturation_on === false ? ["off", "Saturation", "off by choice (the picture as the console sends it)"]
      : [c.saturation === 1 ? "off" : "on", "Saturation", `${dbgPct(c.saturation)}${c.saturation === 1 ? " (untouched)" : ""}`]);
    const who = r.metalfx_active ? "MetalFX" : "the built-in interpolator";
    run.push(!r.generating ? ["off", "Frame generation", st.framegen_on === false ? "off by choice: frames are shown as they arrive" : `nothing to generate: the display (${Math.round(r.refresh_hz)} Hz) shows no more than the stream's rate, or the adaptive frame rate holds it there`]
      : ["on", "Frame generation", `${mult}× to ${Math.round(r.target_hz)} Hz by ${who}, ${c.smooth ? "Smooth (one frame of delay)" : "Low latency (predicts forward)"}`]);
    run.push(c.camera ? ["on", "Camera compensation", r.camera_active ? `active, ${Math.round(r.camera_lag_ms)} ms lag, ${dbgPct(r.camera_explained)} explained` : "armed, nothing to compensate now"] : ["off", "Camera compensation", "off"]);
    run.push(r.generating ? [r.input_prior_active ? "on" : "off", "Input motion prior", r.input_prior_active ? `the stick's motion (${Math.round(r.camera_lag_ms)} ms lag, ${dbgPct(r.camera_explained)} explained) guides the motion search` : "learning the game's response to the stick (needs a few seconds of camera movement), or the picture alone is used"] : ["off", "Input motion prior", "frame generation is off"]);
    run.push([c.immersion ? "on" : "off", "Immersion", c.immersion ? "on (glow in the black bars)" : "off"]);
  }
  run.push(["on", "Engine chosen", st.engine === "fsr" ? "FSR upscales; MetalFX makes the generated midpoints at about 2×, the built-in interpolator the other rates" : "MetalFX (it also makes the midpoints at about 2×; the built-in interpolator covers the other rates)"]);
  add("Running", run);

  // what the machine offers
  add("This Mac", [
    [info.metal ? "on" : "na", "Metal renderer", info.metal ? "available" : "not available"],
    [info.metalfx ? "on" : "na", "MetalFX", info.metalfx ? "spatial scaler" + (metal && r.metalfx_available ? " + frame interpolator" : "") : "not available"],
    [info.fsr ? "on" : "na", "FSR", info.fsr ? "EASU upscaler (AMD FSR 1, ported to Metal compute shaders)" : "not available"],
    ["on", "Display", `${Math.round((metal && r.refresh_hz) || info.display_hz || 0)} Hz${metal && r.variable_refresh ? ", variable refresh" : ""}${metal ? `, window ${r.drawable?.[0]}×${r.drawable?.[1]}, ${r.drawables} drawables` : ""}`],
  ]);

  // the safety guards
  if (metal) {
    const gr = [];
    gr.push(!ad.on ? ["off", "Adaptive frame rate", "off: the rate is kept; the finishing-pass guard below gives the filters up instead"]
      : ad.cap_hz > 0 ? ["warn", "Adaptive frame rate", `holding ${heldRate(r)} fps (${Math.round(ad.asked_hz)} asked for): the GPU is busy ${Math.round(ad.load * 100)} % of the time (${Math.round(ad.peak * 100)} % of the time between two pictures for the slowest) at this rate${heldRate(r) < ad.cap_hz ? `; it could hold ${ad.cap_hz}, but a ${Math.round(r.refresh_hz)} Hz display shows ${heldRate(r)} evenly (every picture for the same number of refreshes)` : ""}. Steps of ${ad.step_hz} fps; it goes back up when there is room`]
      : [ad.over_s > 0 ? "warn" : "on", "Adaptive frame rate", `watching: GPU busy ${Math.round(ad.load * 100)} % of the time (down above ${Math.round(ad.heavy_mean * 100)} %), the slowest pictures take ${Math.round(ad.peak * 100)} % of the time between two (down above ${Math.round(ad.heavy_peak * 100)} %)${ad.over_s > 0 ? `; too high for ${ad.over_s.toFixed(1)} of ${ad.down_after_s} s` : ""}; ${Math.round(ad.asked_hz)} fps asked for, nothing held back`]);
    gr.push(ad.on ? ["off", "Finishing-pass guard", "not used: the adaptive frame rate lowers the rate instead of giving filters up"]
      : !upscaling ? ["off", "Finishing-pass guard", "idle (not upscaling)"]
      : fg.off ? ["warn", "Finishing-pass guard", "TRIPPED: linear light and adaptive sharpening are off until the upscaling, sharpness, adaptive sharpening or frame settings change"]
      : [fg.slow > 0 ? "warn" : "on", "Finishing-pass guard", `watching: ${fg.slow}/${fg.limit} pictures in a row over ${dbgMs(fg.ms)} of GPU time (now ${dbgMs(r.gpu_ms)} on average)`]);
    add("Safety guards", gr);

    // timing
    const interval = 1000 / Math.max(1, r.effective_hz || 60);
    add("Timing and GPU", [
      ["on", "Game renders", `${Math.round(r.content_fps)} fps of distinct pictures (stream ${Math.round(r.source_fps)} fps${r.repeat_share > 0.05 ? `, ${dbgPct(r.repeat_share)} repeats` : ""})`],
      ["on", "Shown", `${r.shown_interval_ms > 0 ? Math.round(1000 / r.shown_interval_ms) : 0} fps on screen, target ${Math.round(r.effective_hz)} Hz`],
      [r.shown_jitter_ms > 2.5 || r.shown_late > 0 ? "warn" : "on", "Frame pacing", `±${dbgMs(r.shown_jitter_ms)}, longest gap ${dbgMs(r.shown_max_ms)}, ${r.shown_late} late in the last 2 s`],
      [r.gpu_ms > 0.7 * interval ? "warn" : "on", "GPU time", `${dbgMs(r.gpu_ms, 2)} per picture of ${dbgMs(interval)}; ${r.gpu_inflight} in flight`],
      ["on", "Latency", `frame wait ${dbgMs(r.frame_wait_ms)}, to the screen ${dbgMs(r.present_latency_ms)}, queue ${dbgMs(r.queue_delay_ms)}, network jitter ${dbgMs(r.network_jitter_ms)}`],
      [r.playout_ms > 0.5 ? "on" : "off", "Smoothing buffer", c.smooth ? (st.pacing_buffer === false ? "off by choice: a late frame is a stall of its own lateness" : `holding the picture back ${dbgMs(r.playout_ms)} (up to 25 ms) so frames that arrive late are still in time`) : "not used in Low latency mode"],
      [r.uncapped_active ? "on" : "off", "Above the display's rate", r.uncapped_active ? `frames made and handed over at up to ${Math.round(r.target_hz)} per second on a ${Math.round(r.refresh_hz)} Hz display: the newest is shown at each refresh` : st.uncapped_rate ? "chosen, but not in use (full screen, or no frame generation)" : "off (the display's refresh rate is the limit)"],
      [r.mailbox_drops > 0 ? "warn" : "on", "Frames", `${r.real_frames} real, ${r.generated} generated, ${r.presented} shown; ${r.mailbox_drops} dropped for being late, ${r.mailbox_len} waiting`],
      [r.errors > 0 ? "bad" : "on", "GPU errors", String(r.errors)],
    ]);
  }

  // the stream
  const sm = S.stream;
  add("Stream", [
    [S.session.state === "connected" ? "on" : "warn", "Session", S.session.state],
    ["on", "Asked for", `${st.height}p, ${st.fps} fps, ${st.hevc ? "HEVC" : "H.264"}${st.hdr ? ", HDR" : ""}, ${st.bitrate === -1 ? "auto bitrate" : (st.bitrate / 1000) + " Mbit/s"}`],
    sm ? ["on", "Receiving", `${(sm.kbps / 1000).toFixed(1)} Mbit/s, ${Math.round(sm.fps)} fps`] : ["off", "Receiving", "waiting for the first second"],
    sm ? [sm.video_frames_dropped > 0 ? "warn" : "on", "Network", `${sm.video_frames_dropped} video frames dropped, ${sm.datagrams_in} packets in, ${sm.datagrams_out} out`] : ["off", "Network", "-"],
  ]);

  // sound and controls
  const a = S.audio;
  const pads = S.controller?.pads || [];
  add("Sound and controls", [
    !a ? ["off", "Audio", "reading…"] : !a.available || a.problem ? ["bad", "Audio", a.problem || "not available"]
      : [a.underruns > 0 || a.errors > 0 ? "warn" : "on", "Audio", `on, volume ${Math.round(a.volume ?? 100)} %; ${a.decoded} packets decoded, ${a.errors} errors, ${a.underruns} underruns, ${a.overruns} overruns`],
    pads.length ? ["on", "Controller", pads.map((p) => `${p.kind}${Number.isFinite(p.batteryLevel) ? ` ${Math.round(p.batteryLevel * 100)} %${p.charging ? " charging" : ""}` : ""}`).join(", ")] : ["off", "Controller", "none connected"],
    [S.input.mapping ? "on" : "off", "Keys and mouse", S.input.mapping ? `on${S.input.profile ? `, profile ${S.input.profile}` : ""}` : "off (the controller plays)"],
    ["on", "Feel", `rumble ${st.rumble_pct ?? 100} %, adaptive triggers ${st.trigger_pct ?? 100} %${S.controller ? `, stick dead zone ${Math.round(S.controller.deadzone * 100)} %` : ""}`],
  ]);
  add("App", [["on", "Version", `${info.version}${info.demo ? " (demo build)" : ""}`]]);
  return sec;
}
function debugText() {
  return debugModel().map((s) => `${s.title}\n${s.rows.map(([state, k, v]) => `  [${state}] ${k}: ${v}`).join("\n")}`).join("\n\n");
}
async function refreshDebugExtras() {
  const [, a] = await call("audio_status");
  if (a) S.audio = a;
  const [, c] = await call("controller_status");
  if (c) S.controller = c;
}
function paintDebugBar() {
  const bar = $("debug-bar");
  const on = !!S.settings?.debug && S.session.state === "connected";
  bar.hidden = !on;
  if (!on) { clearInterval(dbgTimer); dbgTimer = 0; return; }
  if (!dbgTimer) { refreshDebugExtras(); dbgTimer = setInterval(refreshDebugExtras, 3000); }
  bar.classList.toggle("folded", dbgFolded);
  $("dbg-fold").setAttribute("aria-expanded", String(!dbgFolded));
  $("dbg-fold").querySelector("use").setAttribute("href", dbgFolded ? "#i-plus" : "#i-minus");
  const r = S.render, m = r && r.renderer === "metal";
  $("dbg-sub").textContent = m ? `${r.upscaled ? r.upscale_height + "p" : "no upscaling"} · ${Math.round(r.content_fps)}${r.generating ? "→" + Math.round(r.target_hz) : ""} fps · GPU ${Number(r.gpu_ms).toFixed(1)} ms` : "no renderer statistics";
  if (dbgFolded) return;
  const body = $("dbg-body"), top = body.scrollTop;
  body.replaceChildren(...debugModel().map((s) => el("section", { className: "dbg-sec" }, el("h4", { textContent: s.title }),
    el("dl", {}, ...s.rows.map(([state, k, v]) => el("div", { className: "dbg-row", dataset: { s: state } }, el("span", { className: "dbg-dot", attrs: { "aria-hidden": "true" } }), el("dt", { textContent: k }), el("dd", { textContent: v })))))));
  body.scrollTop = top;
}
$("dbg-fold").onclick = () => {
  dbgFolded = !dbgFolded;
  try { localStorage.setItem("openps.dbgfold", dbgFolded ? "on" : "off"); } catch { /* not remembered */ }
  paintDebugBar();
};
$("dbg-copy").onclick = async () => {
  try { await navigator.clipboard.writeText(debugText()); toast("Debug readout copied", "info", 1800); } catch { toast("Could not copy the debug readout", "error"); }
};

function onStats(s) {
  S.stream = s;
  const box = $("hud-stats");
  box.replaceChildren(el("span", { textContent: `${(s.kbps / 1000).toFixed(1)} Mbit/s` }));
  if (s.video_frames_dropped > 0) box.append(el("span", { className: "warn", textContent: `${s.video_frames_dropped} dropped` }));
}

function onSessionEvent(e) {
  const prev = S.session.state;
  if (e.state !== "connected") psSet(false);
  // remember which console the session belongs to, however it was started (card, game tile or hotkey): the cooldown needs it
  if ((e.state === "connecting" || e.state === "connected")) call("active_console").then(([, id]) => { if (id) S.activeId = id; });
  if (SESSION_BUSY.includes(e.state)) S.flow = null; // the session itself drives the connect screen from here
  if (e.state === "connected") S.wakeRetry = null;
  S.session = e;
  if (e.state === "connected" && prev !== "connected" && S.pendingGame) setTimeout(launchPending, 1500); // let the first picture and the input engine come up
  const detail = e.reason ? `: ${e.reason}` : e.attempt ? ` (attempt ${e.attempt})` : "";
  log(`session ${e.state}${detail}`, e.state === "failed" ? "error" : "info");
  renderSession();
  if (e.state === "connected") { hideBanner(); diagOnConnected(); if (!S.settings) renderSettings(); else { paintImmersion(); paintEnginePop(); } }
  if (prev === "connected" && (e.state === "closed" || e.state === "failed")) setTimeout(autoRefreshLibrary, 3000); // played games and trophies have changed
  if (e.state === "failed" && S.wakeRetry && S.wakeRetry.id === S.activeId && Date.now() < S.wakeRetry.until && wakeRetryable(e.reason, S.wakeRetry.woke)) {
    // keep the connect screen up and say what is being waited for, instead of flashing an error
    setFlow(S.pendingGame ? `Starting ${S.pendingGame.game.name}` : "Connecting",
      /80108b10/i.test(e.reason) ? "The console is closing the previous session. Reconnecting automatically…" : "The console is still starting up. Trying again…");
    const id = S.activeId;
    setTimeout(async () => {
      if (S.wakeRetry?.id !== id || busy()) return;
      const [err] = await call("connect", { id });
      if (err) { S.wakeRetry = null; showBanner(err, S.consoles.find((x) => x.id === id)); }
    }, 4000);
    return;
  }
  if (e.state === "failed") {
    S.wakeRetry = null; S.flow = null; S.pendingGame = null;
    const c = S.consoles.find((x) => x.id === S.activeId);
    showBanner(`The connection failed: ${e.reason}`, c);
    if (document.fullscreenElement || S.fullscreen) setFullscreen(false);
  } else if (e.state === "closed" && SESSION_BUSY.includes(prev)) {
    // also when a connection attempt is cancelled: the console may still be holding that half-open session
    if (prev === "connected") toast(e.reason ? `Session ended: ${e.reason}` : "Session ended", e.reason ? "error" : "info");
    S.pendingGame = null;
    startCooldown(S.activeId);
    if (S.fullscreen) setFullscreen(false);
  }
}

// ---- events from the core -------------------------------------------------------------------------------------------------
listen("openps://event", ({ payload: e }) => {
  switch (e.type) {
    case "session": onSessionEvent(e); break;
    case "consoles_changed": refreshConsoles(); break;
    case "psn_changed": loadPsn(); paintPsnApi(); break;
    case "stats": onStats(e); break;
    case "input": onInputEvent(e); break;
    case "control":
      if (e.kind === "data_damaged") showNotice(e.kind, e.detail);
      else if (e.kind === "input_error") toast(`Keyboard and mouse control did not start: ${e.detail}`, "error");
      else if (e.kind === "input_config_error") {
        toast(e.detail, "error");
        S.keymapBad = true;
      }
      else if (e.kind === "remote_step") { $("psn-remote-step").textContent = `${e.detail}…`; $("pair-psn-step").textContent = `${e.detail}…`; }
      else if (e.kind === "psn_error") toast(`PSN sign-in did not complete: ${e.detail}`, "error");
      else log(`console: ${e.kind}${e.detail ? " " + e.detail : ""}`);
      break;
  }
});
// ---- controller battery on the home page ---------------------------------------------------------------------------------------
// A small strip while a controller is connected: its name, a battery bar and the percentage. The bar is red up to 25 %, yellow up to 50 %,
// green above, and its coloured part is as wide as the level, so it shrinks as the battery drains. A pill is kept per controller and patched
// in place, so the bar moves smoothly when the level changes; the strip fades away when the last controller goes.
// (The width goes through the CSS object model: the page's content security policy forbids `style` attributes.)
const padPills = new Map();
const batteryBand = (pct) => (pct <= 25 ? "low" : pct <= 50 ? "mid" : "ok");
function padPill(slot) {
  let p = padPills.get(slot);
  if (p) return p;
  const fill = el("i");
  fill.style.setProperty("--v", "0");
  p = {
    fill,
    name: el("span", { className: "pad-name" }),
    pct: el("b"),
    bolt: svg("zap"),
    root: null,
  };
  p.bolt.classList.add("charging");
  p.root = el("div", { className: "pad-pill" }, svg("gamepad"), p.name, el("span", { className: "batt", attrs: { role: "img" } }, fill), p.pct, p.bolt);
  padPills.set(slot, p);
  return p;
}
async function renderPadBattery() {
  const box = $("pad-battery");
  const [e, status] = await call("controller_status");
  const pads = e ? [] : status.pads;
  if (!pads.length) {
    padPills.clear();
    fadeOut(box, () => { box.hidden = true; box.replaceChildren(); });
    return;
  }
  unfade(box);
  box.hidden = false;
  const seen = new Set();
  pads.forEach((pad, i) => {
    seen.add(pad.slot);
    const p = padPill(pad.slot);
    const known = Number.isFinite(pad.batteryLevel);
    const level = known ? Math.min(1, Math.max(0, pad.batteryLevel)) : 0;
    const pct = Math.round(level * 100);
    const text = known ? `${pct}%${pad.charging ? " · charging" : ""}` : "battery unknown";
    p.root.dataset.level = known ? batteryBand(pct) : "unknown";
    p.root.title = `${pad.name}: ${text}`;
    p.name.textContent = pad.name;
    p.pct.textContent = known ? `${pct}%` : "–";
    p.bolt.style.display = pad.charging ? "" : "none";
    p.root.querySelector(".batt").setAttribute("aria-label", `Battery ${text}`);
    if (box.children[i] !== p.root) box.insertBefore(p.root, box.children[i] || null);
    void p.fill.offsetWidth; // the bar is drawn empty first, so the first fill animates (no animation frame needed: it may not run in a hidden window)
    p.fill.style.setProperty("--v", String(level));
  });
  for (const [slot, p] of padPills) if (!seen.has(slot)) { p.root.remove(); padPills.delete(slot); }
}
listen("openps://pads", () => {
  if (S.view === "settings") renderControllerSettings();
  renderPadBattery();
}).catch((e) => log(`Could not listen for controller changes: ${errText(e)}`, "error"));

window.addEventListener("error", (ev) => /ResizeObserver/i.test(ev.message) || toast(`Unexpected error: ${ev.message}`, "error"));
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
  for (const n of S.info.notices || []) showNotice(n.kind, n.detail);
  log(`OpenPS ${S.info.version}${S.info.demo ? " (demo mode)" : ""}`);
  const [, st] = await call("session_state");
  if (st) S.session = st;
  await loadPsn();
  await refreshConsoles();
  await loadGameMaps();
  paintPsnApi(); // also loads the games shelf when the account is connected
  renderPadBattery();
  renderSession();
  scan(false);
  setInterval(() => { if (S.view === "consoles" && !busy() && !document.hidden) scan(false); }, 20000);
})();
