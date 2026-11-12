const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");

const app = fs.readFileSync(path.join(__dirname, "..", "dist", "app.js"), "utf8");

test("a notice says once that the scene model has loaded, whether it was announced or asked for", () => {
    const fn = app.match(/let sceneModelShown = false;\nfunction sceneModelReady\(\) \{[\s\S]*?\n\}/)?.[0];
    assert.ok(fn, "sceneModelReady exists");
    const toasts = [];
    new Function("toast", `${fn}; sceneModelReady(); sceneModelReady();`)((...args) => toasts.push(args));
    assert.deepEqual(toasts, [["Scene model loaded", "ok"]], "shown once");
    assert.match(app, /e\.kind === "scene_model_ready"\) sceneModelReady\(\)/, "the event shows it");
    assert.match(app, /call\("scene_model_ready"\)\.then\(\(\[, ready\]\) => \{ if \(ready\) sceneModelReady\(\); \}\)/, "a page that opened later asks");
});
