const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");

const app = fs.readFileSync(path.join(__dirname, "..", "dist", "app.js"), "utf8");
const fit = app.match(/function fitHelpPops\(\) \{[\s\S]*?\n\}/)?.[0];
assert.ok(fit, "fitHelpPops exists");

// a popover whose host "?" sits at `hostLeft`, 308 px wide; the page is `pageWidth` wide and starts at x = 0
function run({ hostLeft, pageWidth, hostWidth = 17.5 }) {
  const pop = { style: { left: "" }, offsetWidth: 308, parentElement: { getBoundingClientRect: () => ({ left: hostLeft, width: hostWidth }) } };
  const page = { getBoundingClientRect: () => ({ left: 0 }), clientWidth: pageWidth };
  const document = { querySelector: () => page };
  const $$ = () => [pop];
  new Function("document", "$$", `${fit}; fitHelpPops();`)(document, $$);
  return pop;
}

test("a help popover that would reach past the page is moved left so that it ends inside it", () => {
  const pop = run({ hostLeft: 199, pageWidth: 485 });
  assert.match(pop.style.left, /px$/);
  const left = 199 + parseFloat(pop.style.left);
  assert.ok(left + 308 <= 485 - 12 + 0.01, `ends at ${left + 308}`);
  assert.ok(left >= 0, "still starts inside the page");
});

test("a help popover that fits is left where the stylesheet puts it", () => {
  assert.equal(run({ hostLeft: 40, pageWidth: 485 }).style.left, "");
});

test("a popover on a page that is not showing is not touched", () => {
  assert.equal(run({ hostLeft: 199, pageWidth: 485, hostWidth: 0 }).style.left, "");
});

test("the fit runs again when the page changes or the window is resized", () => {
  assert.match(app, /new MutationObserver\(later\)\.observe\(page/);
  assert.match(app, /addEventListener\("resize", later/);
});
