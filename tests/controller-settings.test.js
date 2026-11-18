const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");

const root = path.join(__dirname, "..");
const html = fs.readFileSync(path.join(root, "dist", "index.html"), "utf8");
const app = fs.readFileSync(path.join(root, "dist", "app.js"), "utf8");
const modePainter = app.match(/function paintControllerMode\(\) \{[\s\S]*?\n\}/)?.[0];
const mappingPainter = app.match(/function paintMapping\(\) \{[\s\S]*?\n\}/)?.[0];

assert.ok(modePainter, "paintControllerMode exists");
assert.ok(mappingPainter, "paintMapping exists");

test("the Controls toolbar and Settings expose mode status, connected pads, and deadzone", () => {
  assert.match(html, /id="map-btn"[^>]*aria-pressed="true"/);
  assert.match(html, /id="map-text"/);
  assert.match(html, /id="controller-list"[^>]*aria-live="polite"/);
  assert.match(html, /id="controller-deadzone"[^>]*type="range"[^>]*min="0"[^>]*max="0\.5"/);
});

test("the Controls buttons label the current input mode accessibly", () => {
  const use = { attributes: {}, setAttribute(name, value) { this.attributes[name] = value; } };
  const elements = {
    "map-btn": {
      attributes: {},
      setAttribute(name, value) { this.attributes[name] = value; },
      removeAttribute(name) { delete this.attributes[name]; },
      querySelector: () => use,
    },
  };
  const paint = new Function("$", "S", `${modePainter}; ${mappingPainter}; return paintMapping();`);

  paint((id) => elements[id], { input: { mapping: false, notice: "" } });
  assert.equal(elements["map-btn"].attributes["aria-pressed"], "false");
  assert.equal(elements["map-btn"].attributes["aria-label"], "Keys & mouse: off");
  assert.equal(use.attributes.href, "#i-gamepad");

  paint((id) => elements[id], { input: { mapping: true, profile: "default", notice: "" } });
  assert.equal(elements["map-btn"].attributes["aria-pressed"], "true");
  assert.equal(elements["map-btn"].attributes["aria-label"], "Keys & mouse: on · default");
  assert.equal(use.attributes.href, "#i-mouse");
});

test("platforms without local input hide the Controls instead of calling missing commands", async () => {
  const render = app.match(/async function renderControllerSettings\(\) \{[\s\S]*?\n\}/)?.[0];
  const keymap = app.match(/async function paintKeymap\(\) \{[\s\S]*?\n\}/)?.[0];
  assert.ok(render && keymap, "renderControllerSettings and paintKeymap exist");
  const calls = [], toasts = [];
  const run = new Function("$", "S", "call", "toast", `${render}; ${keymap}; return Promise.all([renderControllerSettings(), paintKeymap()]);`);
  await run(() => { throw new Error("no element should be touched"); }, { info: { local_input: false } },
    async (c) => { calls.push(c); return ["unknown command", null]; }, (m) => toasts.push(m));

  assert.deepEqual(calls, []);
  assert.deepEqual(toasts, []);
  assert.match(html, /<div id="controls-section">/);
  assert.match(app, /for \(const id of \["controls-section", "map-btn", "ps-btn"\]\) \$\(id\)\.hidden = S\.info\.local_input === false;/);
});
