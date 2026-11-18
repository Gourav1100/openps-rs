const { test } = require("node:test");
const assert = require("node:assert/strict");
const { mkdtempSync, mkdirSync, writeFileSync, copyFileSync, readFileSync, rmSync } = require("node:fs");
const { tmpdir } = require("node:os");
const { join, resolve } = require("node:path");
const { spawnSync } = require("node:child_process");

const root = resolve(__dirname, "..");
const shellTest = process.platform === "win32" ? test.skip : test;

function fixture(t) {
  const dir = mkdtempSync(join(tmpdir(), "openps-build-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  mkdirSync(join(dir, "bin"));
  mkdirSync(join(dir, "scripts"));
  const script = (path, body) => writeFileSync(join(dir, path), `#!/bin/sh\nset -eu\n${body}\n`, { mode: 0o755 });
  script("bin/uname", 'echo "${TEST_OS:-Darwin}"');
  script("bin/docker", `
if [ "$*" = "compose version" ]; then exit "\${TEST_NO_PLUGIN:-0}"; fi
if [ "$*" = "info" ]; then
  if [ "\${TEST_DOCKER_DOWN:-0}" = 1 ] && [ ! -f "$TEST_STARTED" ]; then exit 1; fi
  if [ "\${TEST_DOCKER_STAYS_DOWN:-0}" = 1 ]; then exit 1; fi
  exit 0
fi
echo "docker $*" >> "$TEST_LOG"
exit "\${TEST_BUILD_STATUS:-0}"`);
  script("bin/docker-compose", 'echo "docker-compose $*" >> "$TEST_LOG"');
  script("scripts/make-dmg.sh", 'echo "macos" >> "$TEST_LOG"');
  const env = {
    ...process.env, PATH: `${join(dir, "bin")}:${process.env.PATH}`,
    TEST_LOG: join(dir, "calls"), TEST_STARTED: join(dir, "started"),
  };
  delete env.DOCKER_CONTEXT;
  delete env.DOCKER_HOST;
  return {
    dir, env, script,
    calls: () => readFileSync(env.TEST_LOG, "utf8").trim().split("\n"),
  };
}

shellTest("combined build runs all platforms from outside the project directory", t => {
  const f = fixture(t);
  copyFileSync(join(root, "build-all.sh"), join(f.dir, "build-all.sh"));
  const result = spawnSync("bash", [join(f.dir, "build-all.sh")], { cwd: tmpdir(), env: f.env, encoding: "utf8" });
  assert.equal(result.status, 0, result.stderr);
  assert.deepEqual(f.calls(), [
    "docker compose run --build --rm linux-build",
    "docker compose run --build --rm windows-build",
    "macos",
  ]);
});

shellTest("starts Colima and uses its Docker context when Docker is unavailable on macOS", t => {
  const f = fixture(t);
  copyFileSync(join(root, "build-all.sh"), join(f.dir, "build-all.sh"));
  f.env.TEST_DOCKER_DOWN = "1";
  f.script("bin/colima", '[ "$1" != list ] || exit 0; echo "colima $*" >> "$TEST_LOG"; touch "$TEST_STARTED"');
  f.script("bin/docker-compose", 'echo "context=$DOCKER_CONTEXT docker-compose $*" >> "$TEST_LOG"');
  f.env.TEST_NO_PLUGIN = "1";
  const result = spawnSync("bash", [join(f.dir, "build-all.sh"), "linux"], { env: f.env, encoding: "utf8" });
  assert.equal(result.status, 0, result.stderr);
  assert.deepEqual(f.calls(), [
    `colima start openps-build --activate=false --cpu 4 --memory 8 --disk 40 --mount ${f.dir}:w`,
    "context=colima-openps-build docker-compose run --build --rm linux-build",
  ]);
});

shellTest("does not override an explicitly selected Docker context when it is unavailable", t => {
  const f = fixture(t);
  copyFileSync(join(root, "build-all.sh"), join(f.dir, "build-all.sh"));
  f.env.TEST_DOCKER_DOWN = "1";
  f.env.DOCKER_CONTEXT = "other-context";
  f.script("bin/colima", 'echo "unexpected start" >> "$TEST_LOG"');
  const result = spawnSync("bash", [join(f.dir, "build-all.sh"), "linux"], { env: f.env, encoding: "utf8" });
  assert.notEqual(result.status, 0);
  assert.throws(f.calls, { code: "ENOENT" });
});

shellTest("starts an existing build profile without changing its configuration", t => {
  const f = fixture(t);
  copyFileSync(join(root, "build-all.sh"), join(f.dir, "build-all.sh"));
  f.env.TEST_DOCKER_DOWN = "1";
  f.env.DOCKER_CONTEXT = "colima-openps-build";
  f.script("bin/colima", `
if [ "$1" = list ]; then echo '{"name":"openps-build","status":"Stopped"}'; exit 0; fi
echo "colima $*" >> "$TEST_LOG"
touch "$TEST_STARTED"`);
  const result = spawnSync("bash", [join(f.dir, "build-all.sh"), "linux"], { env: f.env, encoding: "utf8" });
  assert.equal(result.status, 0, result.stderr);
  assert.deepEqual(f.calls(), ["colima start openps-build --activate=false", "docker compose run --build --rm linux-build"]);
});

shellTest("reports failure when Colima starts but Docker remains unavailable", t => {
  const f = fixture(t);
  copyFileSync(join(root, "build-all.sh"), join(f.dir, "build-all.sh"));
  f.env.TEST_DOCKER_DOWN = "1";
  f.env.TEST_DOCKER_STAYS_DOWN = "1";
  f.script("bin/colima", '[ "$1" != list ] || exit 0; echo "started" >> "$TEST_LOG"; touch "$TEST_STARTED"');
  const result = spawnSync("bash", [join(f.dir, "build-all.sh"), "linux"], { env: f.env, encoding: "utf8" });
  assert.notEqual(result.status, 0);
  assert.deepEqual(f.calls(), ["started"]);
});

shellTest("individual Docker builds support standalone Compose on non-Mac hosts", t => {
  const f = fixture(t);
  copyFileSync(join(root, "build-all.sh"), join(f.dir, "build-all.sh"));
  f.env.TEST_NO_PLUGIN = "1";
  f.env.TEST_OS = "Linux";
  const result = spawnSync("bash", [join(f.dir, "build-all.sh"), "windows"], { env: f.env, encoding: "utf8" });
  assert.equal(result.status, 0, result.stderr);
  assert.deepEqual(f.calls(), ["docker-compose run --build --rm windows-build"]);
});

shellTest("a failed Docker build stops the combined build with a failing exit status", t => {
  const f = fixture(t);
  copyFileSync(join(root, "build-all.sh"), join(f.dir, "build-all.sh"));
  f.env.TEST_BUILD_STATUS = "7";
  const result = spawnSync("bash", [join(f.dir, "build-all.sh")], { env: f.env, encoding: "utf8" });
  assert.equal(result.status, 7);
  assert.deepEqual(f.calls(), ["docker compose run --build --rm linux-build"]);
});

shellTest("all platforms fail explicitly on non-Mac hosts before starting any builds", t => {
  const f = fixture(t);
  copyFileSync(join(root, "build-all.sh"), join(f.dir, "build-all.sh"));
  f.env.TEST_OS = "Linux";
  const result = spawnSync("bash", [join(f.dir, "build-all.sh")], { env: f.env, encoding: "utf8" });
  assert.equal(result.status, 1);
  assert.match(result.stderr, /requires a Mac/);
  assert.throws(f.calls, { code: "ENOENT" });
});

test("DMG packaging defaults to release without local credentials and supports prereleases", { skip: process.platform !== "darwin" }, t => {
  const f = fixture(t);
  copyFileSync(join(root, "scripts/make-dmg.sh"), join(f.dir, "scripts/make-dmg.sh"));
  mkdirSync(join(f.dir, "src-tauri"));
  writeFileSync(join(f.dir, "src-tauri/Cargo.toml"), 'version = "1.2.3"\n');
  f.script("scripts/install.sh", `
[ "$1" = "--dest" ]
[ "$3" = "--no-credentials" ]
mkdir -p "$2/OpenPS.app"`);
  f.script("bin/hdiutil", 'for arg do output=$arg; done; printf dmg > "$output"');
  const result = spawnSync("zsh", [join(f.dir, "scripts/make-dmg.sh")], { env: f.env, encoding: "utf8" });
  assert.equal(result.status, 0, result.stderr);
  assert.equal(readFileSync(join(f.dir, "artifacts/macos/openps_1.2.3_macos_Darwin.dmg"), "utf8"), "dmg");
  const prerelease = spawnSync("zsh", [join(f.dir, "scripts/make-dmg.sh"), "--pre-release"], { env: f.env, encoding: "utf8" });
  assert.equal(prerelease.status, 0, prerelease.stderr);
  assert.equal(readFileSync(join(f.dir, "artifacts/macos/openps_1.2.3_macos_Darwin-Pre-release.dmg"), "utf8"), "dmg");
});
