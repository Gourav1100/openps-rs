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
  const elements = {
    "map-text": { textContent: "" },
    "map-btn": {
      attributes: {},
      setAttribute(name, value) { this.attributes[name] = value; },
      removeAttribute(name) { delete this.attributes[name]; },
    },
  };
  const paint = new Function("$", "S", `${modePainter}; ${mappingPainter}; return paintMapping();`);

  paint((id) => elements[id], { input: { mapping: false, notice: "" } });
  assert.equal(elements["map-text"].textContent, "Controller");
  assert.equal(elements["map-btn"].attributes["aria-pressed"], "false");
  assert.equal(elements["map-btn"].attributes["aria-label"], "Keys & mouse: off");

  paint((id) => elements[id], { input: { mapping: true, profile: "default", notice: "" } });
  assert.equal(elements["map-text"].textContent, "Keyboard + mouse");
  assert.equal(elements["map-btn"].attributes["aria-pressed"], "true");
  assert.equal(elements["map-btn"].attributes["aria-label"], "Keys & mouse: on · default");
});
