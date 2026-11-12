const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");

const root = path.join(__dirname, "..");
// an editor may reflow the page and the styles: compare them without line breaks and indentation
const squash = (s) => s.replace(/\s+/g, " ").replace(/> </g, "><");
const html = squash(fs.readFileSync(path.join(root, "dist", "index.html"), "utf8"));
const css = squash(fs.readFileSync(path.join(root, "dist", "style.css"), "utf8"));
const app = fs.readFileSync(path.join(root, "dist", "app.js"), "utf8");
const grab = (re) => {
  const m = app.match(re)?.[0];
  assert.ok(m, `${re} exists`);
  return m;
};
const sceneTextSrc = [
  grab(/const SCENE_NAMES = [^\n]*\n/),
  grab(/const VIEW_NAMES = [^\n]*\n/),
  grab(/function sceneText\(sc\) \{[\s\S]*?\n\}/),
].join("\n");
const sceneText = new Function(`${sceneTextSrc}; return sceneText;`)();
const slotNameSrc = grab(/const slotName = [^\n]*\n/);
const slotName = new Function(`${slotNameSrc}; return slotName;`)();

test("the key map editor has one switch for advanced mapping (scene detection comes with it) and a column for every layer and the action", () => {
  assert.match(html, /id="ov-adv-switch"[^>]*role="switch"/);
  assert.doesNotMatch(html, /ov-scene-switch|Detect the scene/);
  assert.match(html, /id="ov-scene-state"[^>]*role="status"/);
  for (const col of ["On foot", "In vehicle", "In menu", "Action"]) assert.match(html, new RegExp(`<th[^>]*>${col}</th>`));
  assert.match(html, /<th class="plain"[^>]*>Key<\/th><th class="plain"[^>]*>Alternate<\/th>/);
  assert.match(html, /<table class="ov-table" id="ov-table"/);
});

test("the table shows the plain columns or the layer columns, never both", () => {
  assert.match(css, /\.ov-table:not\(\.adv\) \.adv, \.ov-table\.adv \.plain \{ display: none; \}/);
  for (const layer of ["on_foot", "in_vehicle", "in_menu"]) assert.match(css, new RegExp(`data-active="${layer}"\\] \\.col-${layer}`));
});

test("the layer slots are 3 to 5 and named for the scene", () => {
  assert.deepEqual([1, 2, 3, 4, 5].map(slotName), ["key", "alternate", "on foot", "in vehicle", "in menu"]);
  assert.match(app, /const SLOT_CLASS = \["plain", "plain", "adv col-on_foot", "adv col-in_vehicle", "adv col-in_menu"\]/);
});

test("the editor sends the events the engine understands", () => {
  assert.match(app, /ui\(\{ type: "advanced", enabled: on \}\)/);
  assert.doesNotMatch(app, /type: "scene"/);
  assert.match(app, /ui\(\{ type: "label", control, label: sel\.value \|\| null \}\)/);
  // the switch can be used while streaming (mapping on); the action picker and the keys cannot
  assert.match(app, /setSwitch\("ov-adv-switch", o\.advanced, \(on\) => ui\(\{ type: "advanced", enabled: on \}\)\);/);
});

test("the scene line says what the detector is doing", () => {
  assert.deepEqual(sceneText(null), { text: "Off", tone: "" });
  assert.deepEqual(sceneText({ enabled: false }), { text: "Off", tone: "" });
  assert.deepEqual(sceneText({ enabled: true, state: "idle", context: null }), {
    text: "Starts when the keyboard and mouse are on (not needed with a controller)",
    tone: "",
  });
  assert.deepEqual(sceneText({ enabled: true, state: "loading" }), { text: "Preparing the model…", tone: "" });
  assert.deepEqual(sceneText({ enabled: true, state: "running", context: null }), { text: "Looking…", tone: "" });
  assert.deepEqual(
    sceneText({ enabled: true, state: "running", context: "in_vehicle", view: "first_person", confidence: 91 }),
    { text: "In a vehicle · first person · 91%", tone: "ok" },
  );
  assert.deepEqual(sceneText({ enabled: true, state: "running", context: "cutscene", view: null, confidence: 72 }), { text: "In a cutscene · 72%", tone: "ok" });
  assert.deepEqual(sceneText({ enabled: true, state: "unavailable", note: "The scene model is not installed (assets/models)." }), {
    text: "The scene model is not installed (assets/models).",
    tone: "warn",
  });
});

test("the Controls tile names the scene the keys follow", () => {
  const painter = grab(/function paintMapping\(\) \{[\s\S]*?\n\}/);
  const mode = grab(/function paintControllerMode\(\) \{[\s\S]*?\n\}/);
  const attrs = {};
  const use = { setAttribute() { } };
  const b = { setAttribute: (k, v) => (attrs[k] = v), removeAttribute() { }, querySelector: () => use };
  const paint = new Function("$", "S", `${mode}; ${painter}; return paintMapping();`);
  paint(() => b, { input: { mapping: true, profile: "gta_v", notice: "", scene: "in a vehicle" } });
  assert.equal(attrs["aria-label"], "Keys & mouse: on · gta_v · in a vehicle");
  paint(() => b, { input: { mapping: true, profile: "gta_v", notice: "" } });
  assert.equal(attrs["aria-label"], "Keys & mouse: on · gta_v");
});

const modeBadge = new Function(
  `${grab(/const LAYER_BADGE = \{[\s\S]*?\n\};/)}\n${grab(/function modeBadge\(o\) \{[\s\S]*?\n\}/)}; return modeBadge;`,
)();
const running = (layer, extra = {}) => ({ advanced: true, scene: { enabled: true, state: "running", context: "on_foot", layer, ...extra } });

test("the key mode is one icon over the stream, only while advanced mapping is on and the scene is known", () => {
  assert.deepEqual(modeBadge(running("on_foot")), { icon: "i-foot", name: "On foot keys", mode: "on_foot" });
  assert.deepEqual(modeBadge(running("in_vehicle")), { icon: "i-car", name: "Vehicle keys", mode: "in_vehicle" });
  assert.deepEqual(modeBadge(running("in_menu")), { icon: "i-menu", name: "Menu keys", mode: "in_menu" });
  assert.equal(modeBadge(null), null);
  assert.equal(modeBadge({ advanced: false, scene: { enabled: true, state: "running", context: "on_foot", layer: "on_foot" } }), null);
  assert.equal(modeBadge({ advanced: true, scene: { enabled: false } }), null);
  assert.equal(modeBadge({ advanced: true, scene: { enabled: true, state: "idle" } }), null, "a controller is driving: there is no key mode");
  assert.equal(modeBadge({ advanced: true, scene: { enabled: true, state: "loading" } }), null);
  assert.equal(modeBadge({ advanced: true, scene: { enabled: true, state: "running", context: null } }), null);
  assert.deepEqual(modeBadge({ advanced: true, scene: { enabled: true, state: "unavailable", note: "No model" } }), {
    icon: "i-alert",
    name: "No model",
    mode: "warn",
  });
});

test("the badge sits in the top-right corner of the stage, outside the fading HUD, and the pop-up on a scene change is gone", () => {
  const stage = html.slice(html.indexOf('id="stage"'));
  const badge = stage.indexOf('id="mode-badge"');
  assert.ok(badge > 0, "the badge is in the stage");
  assert.ok(badge < stage.indexOf('id="hud"'), "and not inside the HUD, which fades out");
  for (const icon of ["i-foot", "i-car", "i-menu"]) assert.match(html, new RegExp(`<symbol id="${icon}"`));
  assert.match(css, /\.mode-badge \{[^}]*top: max\(0\.75rem[^}]*right: 0\.75rem[^}]*pointer-events: none/);
  assert.match(css, /body\.ov-open \.mode-badge \{ display: none;/, "the editor covers that corner");
  assert.match(app, /paintModeBadge\(o\);/);
  assert.doesNotMatch(app, /Keys: \$\{SCENE_NAMES/);
  assert.doesNotMatch(app, /toast\([^)]*SCENE_NAMES/);
});

test("each layer can be switched off for a game that has no use for it", () => {
  for (const layer of ["on_foot", "in_vehicle", "in_menu"]) assert.match(html, new RegExp(`<input type="checkbox" data-layer="${layer}"`));
  assert.match(html, /id="ov-layers"[^>]*hidden/);
  assert.match(app, /ui\(\{ type: "layer_enabled", layer: box\.dataset\.layer, enabled: box\.checked \}\)/);
  assert.match(app, /box\.disabled = on && inUse === 1;/, "the last layer in use stays on");
  for (const layer of ["on_foot", "in_vehicle", "in_menu"]) assert.match(css, new RegExp(`\\.ov-table\\.off-${layer} \\.col-${layer}`));
});
