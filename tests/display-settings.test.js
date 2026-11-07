const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");

const root = path.join(__dirname, "..");
const html = fs.readFileSync(path.join(root, "dist", "index.html"), "utf8");
const app = fs.readFileSync(path.join(root, "dist", "app.js"), "utf8");
const css = fs.readFileSync(path.join(root, "dist", "style.css"), "utf8");

const fn = (name) => {
  const m = app.match(new RegExp(`function ${name}\\([^)]*\\) \\{[\\s\\S]*?\\n\\}`));
  assert.ok(m, `${name} exists`);
  return m[0];
};

test("every picture filter and frame generation have their own switch; frame generation has no rate slider", () => {
  for (const id of ["linear", "sharp", "rcas", "sat", "fg", "adapt"]) {
    assert.match(html, new RegExp(`id="${id}-switch"[^>]*role="switch"`), `${id} has a switch`);
    assert.match(app, new RegExp(`setSwitch\\("${id}-switch"`), `${id} switch is wired`);
  }
  for (const id of ["sharp", "rcas", "sat"]) {
    assert.match(html, new RegExp(`id="${id}-range" type="range" min="0" max="200" step="5"`));
  }
  // the frame rate is automatic: the display's, stepped down by the adaptive frame rate
  assert.doesNotMatch(html, /fps-range|fps-out|fps-reset/, "no frame rate slider on the page");
  assert.doesNotMatch(app, /target_fps|fps-range/, "no target rate in the page's code");
  assert.match(app, /framegen_on/);
  assert.match(app, /adaptive_fps/);
});

test("the settings carry one flag per filter, with the old multiples gone from the page", () => {
  for (const key of ["linear_light", "sharpness_on", "rcas_on", "saturation_on"]) {
    assert.match(app, new RegExp(`save\\(\\{ ${key}: on \\}\\)`), key);
  }
  assert.doesNotMatch(html, /data-setting="frame_gen"/, "the 1.5x to 3x multiples are gone");
  assert.doesNotMatch(app, /FACTORS/);
});

test("the engine choice is MetalFX or FSR and nothing of VideoToolbox's processors is left", () => {
  assert.match(app, /\[\["metalfx", "MetalFX"\], \["fsr", "FSR"\]\]/);
  for (const [name, text] of [["index.html", html], ["app.js", app], ["style.css", css]]) {
    assert.doesNotMatch(text, /videotoolbox|VideoToolbox|vt_support|vt_sr|vt_fi/i, name);
  }
});

test("the FSR engine says what it is and why FSR 3 and 4 are not offered", () => {
  const hint = app.match(/function paintEngineHint\(s\) \{[\s\S]*?\n\}/)?.[0];
  assert.ok(hint, "paintEngineHint exists");
  assert.match(hint, /EASU/);
  assert.match(hint, /FSR 3/);
  assert.match(hint, /FSR 4/);
});

test("the generation multiple is shown to a tenth, since the adaptive rate moves in steps of 5 fps", () => {
  const rounding = app.match(/Math\.round\(\(r\.target_hz \/ Math\.max\(1, r\.content_fps\)\) \* 10\) \/ 10/g) || [];
  assert.equal(rounding.length, 2, "the top-bar chip and the Debug bar both round to tenths");
  assert.doesNotMatch(app, /r\.content_fps\)\) \* [24]\) \/ [24]/, "no rounding to halves or quarters is left");
});

test("the top bar averages the last ten seconds, and each item has a width of its own", () => {
  const windowAverages = new Function(`${fn("windowAverages")}; return windowAverages;`)();
  // readings one a second: only the last 10 s count, the older ones are dropped
  let hist = [];
  let avg;
  for (let s = 0; s < 25; s++) {
    avg = windowAverages(hist, { fps: s < 15 ? 30 : 60, ms: s < 15 ? 33.3 : 16.7 }, s * 1000, 10000);
    hist = avg.hist;
  }
  assert.equal(hist.length, 10, "readings from the last ten seconds, the newest included");
  assert.ok(Math.abs(avg.fps - 60) < 1e-9 && Math.abs(avg.ms - 16.7) < 1e-9, "the old 30 fps readings are gone from the average");
  // half old, half new: a mix
  const mixed = windowAverages(windowAverages([], { fps: 30, ms: 40 }, 0, 10000).hist, { fps: 60, ms: 20 }, 1000, 10000);
  assert.equal(mixed.fps, 45);
  assert.equal(mixed.ms, 30);
  // the first reading is its own average
  assert.deepEqual(windowAverages([], { fps: 59, ms: 17 }, 5, 10000).fps, 59);
  // the items: the game's rate with its average, the frame time with its average, the frame generation rate, the deviation
  const poll = app.slice(app.indexOf("async function pollRender()"), app.indexOf("// ---- debug bar"));
  assert.match(poll, /fps · avg \$\{avgFps\.toFixed\(1\)\}/, "the fps item shows the current and the average rate");
  assert.match(poll, /ms · avg \$\{avgMs\.toFixed\(1\)\}/, "the frame time item shows the current and the average time");
  assert.doesNotMatch(poll, /→/, "the fps item shows the game's rate only, no arrow to the generated rate");
  assert.match(poll, /textContent: `\$\{Math\.round\(outFps\)\} fps`/, "the frame generation item shows the rate that is shown");
  const items = poll.match(/fixedWidth\(/g) || [];
  assert.ok(items.length >= 7, `every item of the bar has a fixed width (${items.length})`);
  assert.match(css, /\.hud-stats \.fixed \{[^}]*width: var\(--w\)/, "the width comes from the item's own --w");
  assert.match(css, /\.hud-stats \.fixed \{[^}]*flex: none/, "and a flex row does not shrink or grow it");
});

test("a switch that cannot be used does nothing when pressed, and says why", () => {
  const make = () => ({
    attrs: {},
    title: "",
    setAttribute(k, v) { this.attrs[k] = v; },
  });
  const b = make();
  const setSwitch = new Function("$", `${fn("setSwitch")}; return setSwitch;`)(() => b);
  let flips = [];
  setSwitch("x", true, (on) => flips.push(on), false, "Needs upscaling");
  b.onclick();
  assert.deepEqual(flips, [], "disabled: nothing happens");
  assert.equal(b.attrs["aria-checked"], "true");
  assert.equal(b.attrs["aria-disabled"], "true");
  assert.equal(b.title, "Needs upscaling");
  setSwitch("x", true, (on) => flips.push(on), true);
  b.onclick();
  assert.deepEqual(flips, [false], "enabled: the new state is passed on");
  assert.equal(b.attrs["aria-disabled"], "false");
  assert.equal(b.title, "");
});

test("a filter's slider is disabled and dimmed while its filter is off, and keeps its value", () => {
  const els = {};
  const get = (id) => (els[id] ??= { disabled: false, value: "60", classList: { on: {}, toggle(c, v) { this.on[c] = v; } } });
  const setRangeEnabled = new Function("$", `${fn("setRangeEnabled")}; return setRangeEnabled;`)(get);
  setRangeEnabled("sharp", false);
  assert.equal(els["sharp-range"].disabled, true);
  assert.equal(els["sharp-reset"].disabled, true);
  assert.equal(els["sharp-field"].classList.on["is-off"], true);
  assert.equal(els["sharp-range"].value, "60", "the value is untouched");
  assert.match(css, /\.field\.is-off \.range-row/);
});

test("the slider readout is in percent and Reset puts the default back", () => {
  const els = { r: { value: "", oninput: null, onchange: null }, o: { textContent: "" }, x: { onclick: null } };
  const rangeSetting = new Function("$", `${fn("rangeSetting")}; return rangeSetting;`)((id) => els[id]);
  const finals = [];
  rangeSetting("r", "o", "x", 90, 100, (v) => finals.push(v));
  assert.equal(els.o.textContent, "90 %");
  els.r.value = "105";
  els.r.oninput();
  assert.equal(els.o.textContent, "105 %");
  els.r.onchange();
  els.x.onclick();
  assert.deepEqual(finals, [105, 100]);
  assert.equal(els.o.textContent, "100 %");
});

test("the smoothing buffer and running above the display's refresh rate have switches, and the Debug bar says what they do", () => {
  for (const [id, key] of [["buffer", "pacing_buffer"], ["uncap", "uncapped_rate"]]) {
    assert.match(html, new RegExp(`id="${id}-switch"[^>]*role="switch"`), `${id} has a switch`);
    assert.match(app, new RegExp(`setSwitch\\("${id}-switch"[^;]*${key}`), `${id} switch saves ${key}`);
  }
  assert.match(html, /20 to 30 ms gaps/, "the hint names the problem it solves");
  assert.match(app, /"Smoothing buffer"/);
  assert.match(app, /"Above the display's rate"/);
  // with the option on, a 60 Hz display no longer reads as 'nothing is generated'
  assert.match(app, /const gen = fgOn && gpuOk && \(uncap \|\| /);
});
