const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");

const app = fs.readFileSync(path.join(__dirname, "..", "dist", "app.js"), "utf8");
const statusSource = app.match(/function statusOf\(c\) \{[\s\S]*?\n\}/)?.[0];
const lockSource = app.match(/^const consoleLocked = \(c\) => .+;$/m)?.[0];

assert.ok(statusSource, "statusOf exists");
assert.ok(lockSource, "consoleLocked exists");

function isConsoleLocked(state, isBusy) {
  const statusOf = new Function("S", `return (${statusSource});`)(state);
  const expression = lockSource.replace(/^const consoleLocked = /, "").replace(/;$/, "");
  const consoleLocked = new Function("S", "busy", "statusOf", `return (${expression});`)(state, isBusy, statusOf);
  return (id) => consoleLocked({ id });
}

test("a paired console not found on the network cannot be connected", () => {
  const state = { scannedOnce: true, found: new Map(), waking: new Set(), cool: new Map(), rechecking: new Set() };

  assert.equal(isConsoleLocked(state, () => false)("ps5-a"), true);
});

test("a console being checked cannot be connected", () => {
  const state = { scannedOnce: false, found: new Map(), waking: new Set(), cool: new Map(), rechecking: new Set() };

  assert.equal(isConsoleLocked(state, () => false)("ps5-a"), true);
});

test("a discovered console remains connectable when no other lock applies", () => {
  const state = {
    scannedOnce: true,
    found: new Map([["ps5-a", { status: "ready" }]]),
    waking: new Set(),
    cool: new Map(),
    rechecking: new Set(),
  };

  assert.equal(isConsoleLocked(state, () => false)("ps5-a"), false);
});
