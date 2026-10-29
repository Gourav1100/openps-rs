# OpenPS

<span>A native macOS client for **PlayStation Remote Play** (PS5 and PS4), written in Rust with a Tauri shell.</span>
Hardware video decode (VideoToolbox), Metal rendering with optional MetalFX upscaling and frame generation, keyboard-and-mouse play, and everything stored inside the app.

> **Status: pre-release.** The whole stack is implemented and tested against a built-in demo console and mock consoles written from the
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
| `scripts/build.sh [--check] [--demo] [--psn-live] [--app] [--debug]` | Build. `--check` first runs fmt, clippy and all tests. `--app` also makes a bundle with the Tauri CLI (if you already have it). |
| `scripts/install.sh [--dest DIR] [--psn-config F] [--tables F] [--no-credentials]` | Build and create `DIR/OpenPS.app` (default `./install`), ad-hoc signed, optionally with credentials inside. Keeps existing app data. Refuses system folders, never uses `sudo`. |
| `scripts/uninstall.sh [--dest DIR] [--keep-data]` | Remove the app; its data is inside it. `--keep-data` backs the data up first. |
| `scripts/demo.sh [h264\|hevc] [--clip FILE] [--fps N]` | Build with the demo console and run it. |
| `scripts/make_icons.py` | Regenerate every icon (PNG set, `.icns`, `.ico`, `dist/logo.svg`, `dist/logo.css`). Needs Pillow. |

Nothing is written outside this folder. The only thing the scripts never do is install software for you: if the Tauri CLI or Pillow is missing they tell you.

## Setup for a real console

Nothing has to be supplied. The Remote Play key tables and the PSN client constants are compiled in (`crates/openps-rp/src/builtin_tables.json`, `crates/openps-psn/src/builtin_psn.json`), and the installer builds with PSN sign-in on. They are the constants of Sony's own Remote Play apps, used here without Sony's permission (see the notes in `docs/USAGE.md`); do not publish a build or a repository you do not want associated with that.

Optional overrides, for testing other values: put `psn.json` and/or `tables.json` in the app's data folder (`openps-data/`), or point `OPENPS_PSN` / `OPENPS_TABLES` at them. `scripts/install.sh --psn-config F --tables F` copies override files into the installed app; `--no-credentials` ignores `./openps-data`.

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
| `openps-metal` | Metal renderer: content clock, motion-compensated frame generation, MetalFX upscaling, camera compensation |
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

## Documentation

* `docs/USAGE.md`: pairing, playing, controls, settings, troubleshooting
* `../analysis/spec/`: protocol specifications, implementation status (`05`), frame-generation research and measurements (`06`), test report (`07`), UX requirements (`08`)

## Security and privacy

No telemetry. The app talks to your console on the local network, and to Sony only for PS5 sign-in in builds made with `--features psn-live`. The web view is incognito, the CSP blocks eval, inline script and outside requests, and the Tauri capability list is minimal. Hand-edited data is validated on load. See `../analysis/spec/07-test-report.md`.

## Provenance and licence

OpenPS is a Rust re-implementation of the protocol and behaviour of an existing macOS Remote Play client, specified from analysis of that client (`../analysis/`). The repository does not contain Sony's key tables or PSN credentials. It was written with the original developer's permission and has no licence file yet: decide on one before publishing it. Use it only with consoles and accounts you own.
