#!/bin/zsh
# Install OpenPS.   Usage: scripts/install.sh [--dest DIR] [--demo] [--psn-live] [--psn-config FILE] [--tables FILE]
#   --psn-config FILE  copy Sony PSN credentials (psn.json) into the app's data folder (mode 0600); implies --psn-live
#   --tables FILE      copy the key tables (tables.json) there too
#   --no-credentials   ignore ./openps-data even if it exists (a build with nothing inside)
# The PSN client constants and key tables are compiled in; the two files below are only OVERRIDES. PSN sign-in is always built in.
# DEFAULT: if ./openps-data/psn.json and/or ./openps-data/tables.json exist (inside this project, git-ignored) they are used
# automatically, so a plain `scripts/install.sh` installs a ready-to-pair app with PSN enabled.
# With these the app finds its credentials when double-clicked: no environment variables needed.
# Re-installing keeps the app's data (pairings, settings, credentials).
# Default destination: ./install inside this project (nothing outside the project is touched). Use --dest ~/Applications
# for a normal per-user location; this script never uses sudo and never writes to /Applications or ~/Library.
# Portable mode: the app keeps pairings, settings, secrets and logs in `openps-data/` next to its executable.
set -euo pipefail
cd "${0:A:h}/.."

dest="$PWD/install" extra=(--psn-live) psncfg="" tables="" nocreds=0
while (( $# )); do
  case $1 in
    --dest) shift; [[ $# -gt 0 ]] || { echo "--dest needs a folder" >&2; exit 2; }; dest=${1/#\~/$HOME} ;;
    --demo|--psn-live) extra+=("$1") ;;
    --no-credentials) nocreds=1 ;;
    --psn-config) shift; [[ $# -gt 0 ]] || { echo "--psn-config needs a file" >&2; exit 2; }; psncfg=${1/#\~/$HOME}; extra+=(--psn-live) ;;
    --tables) shift; [[ $# -gt 0 ]] || { echo "--tables needs a file" >&2; exit 2; }; tables=${1/#\~/$HOME} ;;
    -h|--help) sed -n '2,13p' "$0"; exit 0 ;;
    *) echo "unknown option: $1 (try --help)" >&2; exit 2 ;;
  esac
  shift
done
[[ $dest == /* ]] || dest="$PWD/$dest"
[[ $dest != "/" && $dest != "/System"* && $dest != "/usr"* && $dest != "/Library"* && $dest != "/Applications"* ]] \
  || { echo "refusing to install into a system location: $dest" >&2; exit 1; }

# defaults: the project's own data folder
if (( ! nocreds )); then
  if [[ -z $psncfg && -f openps-data/psn.json ]]; then psncfg="$PWD/openps-data/psn.json"; extra+=(--psn-live); echo "using $psncfg"; fi
  if [[ -z $tables && -f openps-data/tables.json ]]; then tables="$PWD/openps-data/tables.json"; echo "using $tables"; fi
fi
for f in "$psncfg" "$tables"; do
  [[ -z $f || -f $f ]] || { echo "no such file: $f" >&2; exit 1; }
done
[[ -z $psncfg ]] || python3 -c "import json,sys; d=json.load(open(sys.argv[1])); assert all(d.get(k) for k in ('client_id','client_secret','token_url','redirect_uri','scope'))" "$psncfg" 2>/dev/null \
  || { echo "$psncfg is not a valid psn.json (run extract-psn-config, see crates/openps-psn/README.md)" >&2; exit 1; }
[[ -z $tables ]] || python3 -c "import json,sys; json.load(open(sys.argv[1]))" "$tables" 2>/dev/null \
  || { echo "$tables is not valid JSON" >&2; exit 1; }

./scripts/build.sh "${extra[@]}"

app="$dest/OpenPS.app"
keep=""
if [[ -d $app/Contents/MacOS/openps-data ]]; then   # an upgrade must not delete pairings or credentials
  keep=$(mktemp -d "$dest/.keep.XXXXXX"); chmod 700 "$keep"
  mv "$app/Contents/MacOS/openps-data" "$keep/openps-data"
fi
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp target/release/openps-app "$app/Contents/MacOS/openps-app"
cp src-tauri/icons/icon.icns "$app/Contents/Resources/icon.icns"
version=$(sed -n 's/^version *= *"\(.*\)"/\1/p' src-tauri/Cargo.toml | head -1)
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleName</key><string>OpenPS</string>
<key>CFBundleDisplayName</key><string>OpenPS</string>
<key>CFBundleIdentifier</key><string>app.openps.client</string>
<key>CFBundleExecutable</key><string>openps-app</string>
<key>CFBundleIconFile</key><string>icon</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>${version:-0.1.0}</string>
<key>CFBundleVersion</key><string>${version:-0.1.0}</string>
<key>LSMinimumSystemVersion</key><string>13.0</string>
<key>NSHighResolutionCapable</key><true/>
<key>LSApplicationCategoryType</key><string>public.app-category.games</string>
<key>GCSupportsGameMode</key><true/>
<key>LSSupportsGameMode</key><true/>
<key>NSLocalNetworkUsageDescription</key><string>OpenPS talks to your PlayStation on your local network.</string>
</dict></plist>
PLIST
data="$app/Contents/MacOS/openps-data"
if [[ -n $keep ]]; then mv "$keep/openps-data" "$data"; rmdir "$keep"; else mkdir -m 700 -p "$data"; fi
chmod 700 "$data"
[[ -z $psncfg ]] || { install -m 600 "$psncfg" "$data/psn.json"; echo "credentials: $data/psn.json"; }
[[ -z $tables ]] || { install -m 600 "$tables" "$data/tables.json"; echo "key tables:  $data/tables.json"; }
# ad-hoc signature so macOS runs the bundle on this Mac (not a Developer ID signature: not for distribution)
codesign --force --deep --sign - "$app" >/dev/null 2>&1 || echo "note: ad-hoc signing failed; the app may need right-click > Open the first time"
echo
echo "Installed: $app"
echo "Run:       open \"$app\""
echo "Remove:    scripts/uninstall.sh --dest \"$dest\""
