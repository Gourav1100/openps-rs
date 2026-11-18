const { test } = require("node:test");
const assert = require("node:assert/strict");
const { readFileSync } = require("node:fs");
const { join } = require("node:path");
const { runInNewContext } = require("node:vm");

function loginPage({ host = "ca.account.sony.com", subframe = false } = {}) {
  const script = readFileSync(join(__dirname, "../src-tauri/src/psn-loading.js"), "utf8");
  const nodes = [];
  let interval;
  const node = () => ({
    style: {},
    children: [],
    setAttribute() {},
    append(...children) { this.children.push(...children); },
    contains(child) { return this.children.includes(child); },
    addEventListener(event, callback) { this[event] = callback; },
    remove() { this.removed = true; },
    animate() {},
  });
  const document = {
    readyState: "complete",
    createElement: node,
    body: { append: element => { element.isConnected = true; if (!nodes.includes(element)) nodes.push(element); } },
    querySelectorAll: () => controls,
  };
  const controls = [];
  const window = { location: { protocol: "https:", hostname: host }, matchMedia: () => ({ matches: true }) };
  window.top = subframe ? {} : window;
  runInNewContext(script, {
    window, document, getComputedStyle: element => element.computed,
    setInterval: callback => { interval = callback; return 1; },
    clearInterval: () => { interval = null; },
  });
  return { nodes, controls, tick: () => interval?.() };
}

test("a blank sign-in page shows loading until a visible sign-in control appears", () => {
  const page = loginPage();
  assert.equal(page.nodes.length, 1);
  const overlay = page.nodes[0];
  assert.equal(overlay.removed, undefined);
  overlay.isConnected = false;
  page.tick();
  assert.equal(overlay.isConnected, true);
  page.controls.push(overlay.children.find(child => child.textContent === "Show page"));
  page.tick();
  assert.equal(overlay.removed, undefined);
  page.controls.push({
    getBoundingClientRect: () => ({ width: 0, height: 0 }),
    computed: { visibility: "hidden", opacity: "1" },
  });
  page.tick();
  assert.equal(overlay.removed, undefined);
  page.controls.push({
    getBoundingClientRect: () => ({ width: 200, height: 40 }),
    computed: { visibility: "visible", opacity: "1" },
  });
  page.tick();
  assert.equal(overlay.removed, true);
});

test("a user can show the page if it never exposes a detectable control", () => {
  const page = loginPage();
  const overlay = page.nodes[0];
  const button = overlay.children.find(child => child.textContent === "Show page");
  button.click();
  assert.equal(overlay.removed, true);
});

test("the loading UI does not run on unrelated origins or inside subframes", () => {
  assert.equal(loginPage({ host: "sony.com.evil.example" }).nodes.length, 0);
  assert.equal(loginPage({ subframe: true }).nodes.length, 0);
});
