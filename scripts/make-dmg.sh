#!/bin/zsh
# Create a drag-and-drop macOS installer. Usage: scripts/make-dmg.sh [--output FILE] [--demo] [--release]
set -euo pipefail
cd "${0:A:h}/.."

output=""
channel="-Pre-release"
extra=()
while (( $# )); do
  case $1 in
    --output)
      shift
      [[ $# -gt 0 ]] || { echo "--output needs a file path" >&2; exit 2; }
      output=${1/#\~/$HOME}
      ;;
    --demo) extra+=(--demo) ;;
    --release) channel="" ;;
    -h|--help) sed -n '2p' "$0"; exit 0 ;;
    *) echo "unknown option: $1 (try --help)" >&2; exit 2 ;;
  esac
  shift
done

[[ $(uname -s) == Darwin ]] || { echo "DMG creation requires macOS." >&2; exit 1; }
command -v hdiutil >/dev/null || { echo "hdiutil is required to create a DMG." >&2; exit 1; }

version=$(sed -n 's/^version *= *"\(.*\)"/\1/p' src-tauri/Cargo.toml | head -1)
[[ -n $version ]] || { echo "could not read the app version from src-tauri/Cargo.toml" >&2; exit 1; }
arch=$(uname -m)
[[ -n $output ]] || output="$PWD/target/openps_${version}_macos_${arch}${channel}.dmg"
[[ $output == /* ]] || output="$PWD/$output"
[[ ! -d $output ]] || { echo "output path is a directory: $output" >&2; exit 1; }

mkdir -p "$PWD/target" "${output:h}"
stage=$(mktemp -d "$PWD/target/openps-dmg.XXXXXX")
trap 'rm -rf "$stage"' EXIT

./scripts/install.sh --dest "$stage/app" --no-credentials "${extra[@]}"
mkdir "$stage/image"
mv "$stage/app/OpenPS.app" "$stage/image/OpenPS.app"
ln -s /Applications "$stage/image/Applications"

hdiutil create -volname OpenPS -srcfolder "$stage/image" -ov -format UDZO "$output"
echo "Created: $output"
