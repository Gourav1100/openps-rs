#!/bin/zsh
# Remove an installed OpenPS.   Usage: scripts/uninstall.sh [--dest DIR] [--keep-data]
# Removes DIR/OpenPS.app. Because the app is portable, its data (pairings, settings, secrets, logs) lives inside the bundle
# and goes with it; --keep-data copies it to DIR/openps-data-backup first.
set -euo pipefail
cd "${0:A:h}/.."
dest="$PWD/install" keep=0
while (( $# )); do
  case $1 in
    --dest) shift; dest=${1/#\~/$HOME} ;;
    --keep-data) keep=1 ;;
    -h|--help) sed -n '2,5p' "$0"; exit 0 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
  shift
done
[[ $dest == /* ]] || dest="$PWD/$dest"
app="$dest/OpenPS.app"
[[ -d $app && -x $app/Contents/MacOS/openps-app ]] || { echo "no OpenPS installation at $app" >&2; exit 1; }
pgrep -f "$app/Contents/MacOS/openps-app" >/dev/null && { echo "OpenPS is running: quit it first." >&2; exit 1; }
if (( keep )) && [[ -d $app/Contents/MacOS/openps-data ]]; then
  cp -Rp "$app/Contents/MacOS/openps-data" "$dest/openps-data-backup"
  echo "data kept in $dest/openps-data-backup"
fi
rm -rf "$app"
echo "removed $app"
(( keep )) || echo "its data (pairings, settings, logs) was inside the app and is gone; use --keep-data next time to keep a copy"
