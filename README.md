# OpenPS

<span>A native macOS client for **PlayStation Remote Play** (PS5 and PS4), written in Rust with a Tauri shell.</span>
Hardware video decode (VideoToolbox), Metal rendering with optional MetalFX or FSR upscaling, frame generation (built-in or MetalFX), an adaptive frame rate that steps down in 15 fps steps instead of dropping filters, an opt-in immersion mode, keyboard-and-mouse play, and everything stored inside the app.

> **Status:** macOS builds use the release profile; Linux and Windows builds use the development profile. The stack is tested against a built-in demo console and mock consoles written from the
> same specification. **It has not been tried against a real PlayStation yet.** Treat it as a build you are helping validate.

## Requirements

* macOS 13 or newer on Apple Silicon or Intel (developed and measured on an M4 with macOS 27). Metal and VideoToolbox are used directly.
* Rust (stable) to build: <https://rustup.rs>. Xcode is **not** needed.
* A PS4 or PS5 on the same network (Remote Play enabled). PS5 pairing also needs a PlayStation Network account.
* Two Sony constants that are deliberately not in this repository (see **Setup for a real console**).

## Quick start

```sh
scripts/demo.sh            # build with the demo console and start it: no PlayStation needed
scripts/install.sh         # build and install to ./install/OpenPS.app (inside this project)
open install/OpenPS.app
```

In demo mode: **Add console**, pick *Demo PS5*, type any 8 digits except `00000000`, sign in to the (fake) PSN account when asked, then **Connect**.

## Scripts

| Script | What it does |
|---|---|
| `./build-all.sh [all\|linux\|windows\|macos]` | Build Linux/Windows development executables in Docker and a native macOS release DMG. Collects outputs in `artifacts/`; `all` and `macos` require a Mac. |
| `scripts/build.sh [--check] [--demo] [--psn-live] [--app] [--debug]` | Build. `--check` first runs fmt, clippy and all tests. `--app` also makes a bundle with the Tauri CLI (if you already have it). |
| `scripts/install.sh [--dest DIR] [--psn-config F] [--tables F] [--no-credentials]` | Build and create `DIR/OpenPS.app` (default `./install`), ad-hoc signed, optionally with credentials inside. Keeps existing app data. Refuses system folders, never uses `sudo`. |
| `scripts/make-dmg.sh [--output FILE] [--demo] [--pre-release]` | Build a drag-and-drop installer at `artifacts/macos/openps_<version>_macos_<architecture>.dmg` (or `FILE`); `--pre-release` adds a suffix. The image includes an `/Applications` shortcut and excludes local credential override files. |
| `scripts/uninstall.sh [--dest DIR] [--keep-data]` | Remove the app; its data is inside it. `--keep-data` backs the data up first. |
| `scripts/demo.sh [h264\|hevc] [--clip FILE] [--fps N]` | Build with the demo console and run it. |
| `scripts/make_icons.py` | Regenerate every icon (PNG set, `.icns`, `.ico`, `dist/logo.svg`, `dist/logo.css`). Needs Pillow. |

Nothing is written outside this folder. The only thing the scripts never do is install software for you: if the Tauri CLI or Pillow is missing they tell you.

## Setup for a real console

Nothing has to be supplied. The Remote Play key tables and the PSN client constants are compiled in (`crates/openps-rp/src/builtin_tables.json`, `crates/openps-psn/src/builtin_psn.json`), and the installer builds with PSN sign-in on. They are the constants of Sony's own Remote Play apps, used here without Sony's permission (see the notes in `docs/USAGE.md`); do not publish a build or a repository you do not want associated with that.

Optional overrides, for testing other values: put `psn.json` and/or `tables.json` in the app's data folder (`openps-data/`), or point `OPENPS_PSN` / `OPENPS_TABLES` at them. `scripts/install.sh --psn-config F --tables F` copies override files into the installed app; `--no-credentials` ignores `./openps-data`. The same folder can hold your key map (`input.toml`) and the game-to-profile map (`game-keymaps.json`): a fresh install starts with them, and an existing key map in the app is never overwritten.

Then follow `docs/USAGE.md` to pair and play.

## Where data lives

Everything (pairings, settings, registration keys, key map, logs) is in `openps-data/` next to the executable (inside the `.app`: `Contents/MacOS/openps-data`), or in `$OPENPS_HOME`. Folders are 0700, files 0600. Secrets are in an owner-only file, not the Keychain. Delete the app and nothing remains.

## Layout

| Crate | Role |
|---|---|
| `openps-fec`, `openps-crypto`, `openps-packets` | FEC (Cauchy Reed-Solomon), Takion crypto, packet layouts |
| `openps-takion` | protobuf, control chunks, handshake, network test |
| `openps-stream` | video/audio/haptic receive, FEC recovery, acks, feedback, congestion |
| `openps-session` | session runtime on a UDP link (threads, bounded media delivery) |
| `openps-discovery`, `openps-rp` | discovery and wake; registration, session init, control channel, key derivation |
| `openps-psn` | PSN OAuth sign-in (no live client unless the `live` feature is on) |
| `openps-input`, `openps-pad` | keyboard/mouse to pad mapping engine; pad state to Takion feedback |
| `openps-macinput` | macOS input: web view key events, CoreGraphics mouse deltas, engine runner (no Accessibility permission) |
| `openps-core` | application core: console store, pairing and connect flows, events (no UI dependency) |
| `openps-video` | H.264/HEVC handling and VideoToolbox decode, system-layer fallback renderer |
| `openps-audio` | Opus decode (AudioToolbox), AudioQueue playback with jitter buffer |
| `openps-metal` | Metal renderer: content clock, motion-compensated frame generation, MetalFX and FSR (EASU) upscaling, adaptive frame rate, immersion background, camera compensation |
| `openps-remote` | PSN remote registration of a PS5: session signaling, UDP hole punch, reliable-UDP register request, live push WebSocket (feature `live`) |
| `openps-demo`, `openps-testkit` | pretend PS5 for the demo and end-to-end tests; mock console for tests |
| `src-tauri` | Tauri shell: commands and events over `openps-core` |
| `dist/` | the user interface (plain HTML, CSS and JS; strict CSP, no inline script) |

## Development

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace -- --test-threads=2         # ~560 tests, no network or hardware needed
```

Timing tests (input latency under load) are sensitive to a busy machine; run them with few threads and other apps idle.
Real-footage frame-generation report: `OPENPS_REAL_DIR=<dir> cargo test -p openps-metal --release --test real_clip -- --nocapture`.

### Docker desktop builds and tests

On a Mac with Rust, Xcode Command Line Tools and Docker, build
all platforms from the project root:

```sh
./build-all.sh
```

This runs the Docker development builds sequentially, then creates the native,
release-profile, ad-hoc-signed macOS installer in `artifacts/macos/`. The DMG excludes local
credential overrides. Linux and Windows outputs remain in `artifacts/linux/`
and `artifacts/windows/`. If Docker is unavailable and Colima is installed, the
script starts its `openps-build` profile and uses that context for this build.
It leaves Colima running afterwards; stop it with `colima stop openps-build`.
It does not override an explicitly selected `DOCKER_HOST` or unrelated
`DOCKER_CONTEXT`, change the active Docker context, or install dependencies.
The script stops on a failed build. It accepts a
single platform argument (for example `./build-all.sh macos` for just the DMG).
On Linux or in WSL, use `linux` or `windows`; `all` fails explicitly because
macOS cannot be built in a Linux container. Both Compose plugin and standalone
`docker-compose` installations are supported.

Docker Engine/Desktop with Compose v2 or newer can test Linux and build unsigned
Linux and Windows **development executables** without a host Rust, Node, or GTK installation:

```sh
docker compose run --build --rm linux-test
docker compose run --build --rm linux-build
docker compose run --build --rm windows-build
```

On macOS, Colima is an alternative to Docker Desktop. With `docker`, `colima`
and `docker-compose` installed, run from the repository root:

```sh
colima start openps-build --activate=false --cpu 4 --memory 8 --disk 40 --mount "$PWD:w"
DOCKER_CONTEXT=colima-openps-build docker-compose run --build --rm linux-test
DOCKER_CONTEXT=colima-openps-build docker-compose run --build --rm linux-build
DOCKER_CONTEXT=colima-openps-build docker-compose run --build --rm windows-build
# Or build all three platforms, including the native macOS DMG:
DOCKER_CONTEXT=colima-openps-build ./build-all.sh
colima stop openps-build
```

This uses a dedicated VM without changing your active Docker context.

Always use `--build` after editing source: the checkout is copied into the image,
not bind-mounted. Only source/test inputs are sent to Docker; local app data,
credential overrides, captures and trained models are excluded. Do not add secrets
to source files. No container mounts the Docker socket or the host home directory.

Outputs are `artifacts/linux/openps-app` and
`artifacts/windows/openps-app.exe`. Linux follows the container's CPU architecture
(ARM64 on Apple Silicon, x86-64 on most CI hosts), requires glibc 2.36 or newer and
the WebKitGTK 4.1/GTK 3 runtime libraries. To build x86-64 Linux on an ARM host, set
`DOCKER_DEFAULT_PLATFORM=linux/amd64` for **both** Linux commands; this needs Docker
emulation and is slower. Use a separate Compose project (`-p openps-amd64`) to keep
its architecture-specific caches separate.

Named volumes cache build products and the Windows SDK; source is image-layer
cached. Do not run services sharing a build cache concurrently. `docker compose
down --volumes` removes these caches, not exported artifacts. Homebrew users can
use `docker-compose` in place of `docker compose` if the plugin is not configured.
Compilation/tests run as an unprivileged user. Build services use root only to
export the executable into the bind-mounted artifacts directory, which Docker
may create as root on Linux.

The Windows cross-build uses Tauri's documented `cargo-xwin` MSVC path and
downloads Microsoft's SDK (subject to Microsoft's license terms). It compiles
workspace test targets but **does not execute Windows tests**. WebView2 is required
to run the executable on Windows. These commands do not produce installers,
sign releases or test desktop GUI interaction.

Docker cannot run macOS containers or supply Apple's frameworks. On a Mac with
Rust and Xcode Command Line Tools, use the existing native build:

```sh
mkdir -p target/tmp
node --test tests/*.test.js
cargo test --locked -p openps-app -p openps-core -p openps-psn -p openps-gamepad --features psn-live -- --test-threads=2
scripts/build.sh --psn-live
# Optional .app bundle, with the Tauri CLI already installed:
scripts/build.sh --psn-live --app
```

The portability workflow builds/tests Linux in Docker, cross-builds Windows in
Docker, and builds/tests the app on native Windows and macOS runners, including
uploading the macOS DMG. Native
jobs are essential: cross-compilation is not runtime verification. GPU, model,
controller hardware and real-console qualification remain separate.

**Platform limits:** Linux/Windows builds currently validate the shell and
portable core, not full Remote Play parity. Video/audio/render and keyboard/mouse
backends remain macOS-only. File permissions are owner-only on Unix; Windows
inherits directory ACLs and does not yet have equivalent secret protection.
These builds are development artifacts, not production-ready ports.

References: [Tauri prerequisites](https://v2.tauri.app/start/prerequisites/) and
[Windows cross-compilation](https://v2.tauri.app/distribute/windows-installer/#build-windows-apps-on-linux-and-macos).

## Documentation

* `docs/USAGE.md`: pairing, playing, controls, settings, troubleshooting
* `RELEASE_NOTES.md`: release notes and known limitations
* `../analysis/spec/`: protocol specifications, implementation status (`05`), frame-generation research and measurements (`06`), test report (`07`), UX requirements (`08`)

## Security and privacy

No telemetry. The app talks to your console on the local network, and to Sony only for PS5 sign-in in builds made with `--features psn-live`. The web view is incognito, the CSP blocks eval, inline script and outside requests, and the Tauri capability list is minimal. Hand-edited data is validated on load. See `../analysis/spec/07-test-report.md`.

## Provenance and licence

OpenPS is a Rust re-implementation of the protocol and behaviour of an existing macOS Remote Play client, specified from analysis of that client (`../analysis/`). The repository does not contain Sony's key tables or PSN credentials. It was written with the original developer's permission and has no licence file yet: decide on one before publishing it. Use it only with consoles and accounts you own.
