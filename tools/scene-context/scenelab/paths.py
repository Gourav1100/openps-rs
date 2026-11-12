"""Where everything lives, in one place."""
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent  # tools/scene-context
REPO = ROOT.parents[1]
DATA = ROOT / "data"  # the working area, git-ignored: every extracted frame in frames/, their index in index.json, the labeling page
DATASET = ROOT / "dataset"  # the labeled dataset, in git: labels.json (every label) and frames/ (the labeled frames); sorted/ is generated
FRAMES = DATA / "frames"  # <recording>__f_NNNNN.jpg, all recordings together
INDEX = DATA / "index.json"  # {recording: {capture, step, frames: [{file, t, source}]}}
DATASET_FRAMES = DATASET / "frames"
LABELS = DATASET / "labels.json"  # {labels: [{recording, file, t, context, view}], deleted: [key]}; a key is "<id>/frames/f_NNNNN.jpg"
RECORDINGS = DATASET / "recordings.json"  # {id: {order, sequence, thin}}: what the tools need to know about a recording besides its frames and labels
CACHE = ROOT / "cache"  # features per frame, replay sequences
SUGGESTIONS = CACHE / "suggestions.json"  # which frames to label next (scene.py suggest)
TEMPLATES = ROOT / "templates"
MODELS = REPO / "assets" / "models"  # what the app bundles (scripts/install.sh)
FIXTURE = REPO / "crates" / "openps-scene" / "tests" / "fixtures" / "reference.json"  # the Rust tests compare against it
CAPTURES = Path.home() / "openps-captures"  # scripts/record-session.sh writes here
