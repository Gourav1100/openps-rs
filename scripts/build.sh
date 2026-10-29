#!/bin/zsh
# Build OpenPS.   Usage: scripts/build.sh [--check] [--demo] [--psn-live] [--app] [--debug]
#   --check     run fmt, clippy and the full test suite first (stops on the first problem)
#   --demo      include the built-in pretend console (for trying the app without a PlayStation)
#   --psn-live  allow contacting Sony for PS5 sign-in (needs OPENPS_PSN credentials; off by default)
#   --app       also produce a macOS .app bundle (needs `cargo install tauri-cli`, which this script never installs)
#   --debug     debug instead of release profile
# Everything is built inside this folder (target/). Nothing is installed or written elsewhere.
set -euo pipefail
cd "${0:A:h}/.."

check=0 demo=0 psn=0 app=0 profile=release
for a in "$@"; do
  case $a in
    --check) check=1 ;; --demo) demo=1 ;; --psn-live) psn=1 ;; --app) app=1 ;; --debug) profile=debug ;;
    -h|--help) sed -n '2,9p' "$0"; exit 0 ;;
    *) echo "unknown option: $a (try --help)" >&2; exit 2 ;;
  esac
done

[[ $(uname -s) == Darwin ]] || { echo "OpenPS builds on macOS only (VideoToolbox, Metal)." >&2; exit 1; }
command -v cargo >/dev/null || { echo "Rust is required: https://rustup.rs (not installed by this script)." >&2; exit 1; }

features=(tauri/custom-protocol)
(( demo )) && features+=(demo)
(( psn )) && features+=(psn-live)
flags=()
[[ $profile == release ]] && flags+=(--release)

if (( check )); then
  echo "==> format";  cargo fmt --all -- --check
  echo "==> clippy";  cargo clippy --workspace --all-targets -- -D warnings
  echo "==> tests";   cargo test --workspace -- --test-threads=2
fi

echo "==> build ($profile, features: ${(j:,:)features})"
cargo build -p openps-app "${flags[@]}" --features "${(j:,:)features}"
bin=target/$profile/openps-app
echo "built: $bin"

if (( app )); then
  command -v cargo-tauri >/dev/null || { echo "--app needs the Tauri CLI: cargo install tauri-cli (ask first; this script does not install anything)." >&2; exit 1; }
  echo "==> bundle"
  (cd src-tauri && cargo tauri build ${flags[@]/--release/} --bundles app --features "${(j:,:)features}")
  ls -d target/$profile/bundle/macos/*.app 2>/dev/null || true
fi
