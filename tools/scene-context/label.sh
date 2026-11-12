#!/usr/bin/env bash
# Serve the labeling page (every recording, one set at a time, labeled ones too so they can be revisited; each change is saved into dataset/labels.json).
# New captures in ~/openps-captures are extracted first; or name recordings:
#   ./label.sh [CAPTURE-OR-VIDEO ...]      (--new: only unlabeled frames, --menus: split menus from cutscenes, --only PREFIX, --no-open)
# Label the frames, Ctrl-C when done, then ./train.sh
exec "$(dirname "${BASH_SOURCE[0]}")/scene.sh" label "$@"
