#!/bin/zsh
# Try OpenPS without a PlayStation: builds with the demo console and starts it.
# Usage: scripts/demo.sh [h264|hevc] [--clip FILE.annexb] [--fps N]
# In the app: Add console > the Demo PS5 > any 8 digits (not 00000000) > Sign in to PSN (fake in demo mode) when asked > Connect.
set -euo pipefail
cd "${0:A:h}/.."
codec=hevc
while (( $# )); do
  case $1 in
    h264|hevc) codec=$1 ;;
    --clip) shift; export OPENPS_DEMO_CLIP=$1 ;;
    --fps) shift; export OPENPS_DEMO_FPS=$1 ;;
    -h|--help) sed -n '2,4p' "$0"; exit 0 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
  shift
done
./scripts/build.sh --demo
OPENPS_HOME="$PWD/target/demo-home" OPENPS_DEMO=$codec exec ./target/release/openps-app
