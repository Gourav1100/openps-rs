#!/bin/zsh
# Records the video of the next session (what the console sends, up to 90 seconds) so that a problem seen on screen, such as shimmering
# in a camera pan, can be replayed and measured offline.
# Usage: scripts/record-session.sh [app]      (default: install/OpenPS.app)
# Start the stream, make the camera move over the thing that shimmers for a while, then leave. The file is written to
# ~/openps-captures/video-<time>.opsvid. It contains the picture you were streaming: keep it to yourself.
set -euo pipefail
cd "${0:A:h}/.."
app=${1:-install/OpenPS.app}
dir=$HOME/openps-captures
mkdir -p "$dir"
echo "Recording to $dir (stops by itself after 90 s). Quit OpenPS when you are done."
OPENPS_VIDEO_CAPTURE="$dir" "$app/Contents/MacOS/openps-app"
ls -la "$dir" | tail -3
echo
echo "Analyse it with:"
echo "  OPENPS_CAPTURE_FILE=<file> OPENPS_REPLAY_OUT=\$HOME/openps-captures/out cargo test -p openps-metal --release --test replay -- --ignored --nocapture"
