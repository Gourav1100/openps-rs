#!/usr/bin/env bash
# Train on everything in dataset/labels.json and write the models into assets/models: imports the newest labels export from ~/Downloads first (only
# if it is newer, for when the page's Export was used), computes features only for new frames, converts the models to Core ML only when they changed.
#   ./train.sh [--reexport] [--no-check] [--c N] [--zs W] [--out DIR]
# Then scripts/install.sh puts the models in the app.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
"$here/scene.sh" import --if-new
exec "$here/scene.sh" train "$@"
