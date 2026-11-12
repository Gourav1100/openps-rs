"""Which unlabeled frames to label next: the ones the current model is least sure about, or that look least like anything labeled.

Every frame without a label is read by the shipped model (assets/models). It scores high when its top two scenes are close (uncertainty) and when its
nearest labeled frame, by the whole-frame embedding, is far (novelty); the two are averaged as percentile ranks. The list is picked from the top,
skipping near copies of a frame already picked and keeping one recording below a share of the list. `scene.py label --suggested` shows it as one set.
Frames labeled game_2d, game_3d or in_game count as known (the shipped model does not read them, but they are no surprise).
"""
import json
import time
from pathlib import Path

import numpy as np

from . import data as scene_data
from . import features, probe
from .data import STATES, flat_name, key, read_index, read_labels
from .paths import FRAMES, MODELS, SUGGESTIONS
from .pictures import PICTURES

NEAR_COPY = 0.96  # cosine between whole-frame embeddings above which a frame is a copy of one already picked
MAX_SHARE = 0.25  # of the list, for any one recording (when there are several)


def _frames(only):
    """(key, recording, path) of the frames without a label, and of those labeled game_2d, game_3d or in_game."""
    doc = read_labels()
    labeled = {key(l["recording"], l["file"]): l["context"] for l in doc["labels"]}
    gone = set(doc["deleted"])
    unlabeled, known = [], []
    index = read_index()
    for name in scene_data.extracted_recordings():
        for f in index[name]["frames"]:
            k, path = key(name, f["file"]), FRAMES / flat_name(name, f["file"])
            if k in gone or not path.exists():
                continue
            if k not in labeled and scene_data.matches(name, only):
                unlabeled.append((k, name, path))
            elif labeled.get(k) in scene_data.OTHER:
                known.append((k, name, path))
    return unlabeled, known


def _unit(x):
    return x / np.maximum(np.linalg.norm(x, axis=1, keepdims=True), 1e-9)


def _percentile(a):
    return a.argsort().argsort() / max(len(a) - 1, 1)


def suggest(n=500, only=None, model_dir=MODELS):
    import coremltools as ct

    model_dir = Path(model_dir)
    saved = json.loads((model_dir / "scene-probe.json").read_text())
    unlabeled, known = _frames(only)
    if not unlabeled:
        raise SystemExit("every extracted frame has a label")
    clip = ct.models.MLModel(str(model_dir / "scene-encoder.mlpackage"))
    backbone = ct.models.MLModel(str(model_dir / "scene-backbone.mlpackage"))
    both = unlabeled + known
    x = features.pool([(k, p) for k, _, p in both], clip, backbone)
    xu, xk = x[: len(unlabeled)], x[len(unlabeled):]

    p = probe.context(saved, xu)
    top = np.sort(p, axis=1)[:, ::-1]
    uncertainty = 1 - (top[:, 0] - top[:, 1])

    whole = PICTURES[0][3]  # the whole-frame embedding comes first
    ref = _unit(np.vstack([features.cached()[:, :whole], xk[:, :whole]]))
    unit = _unit(xu[:, :whole])
    nearest = np.concatenate([(unit[i : i + 2000] @ ref.T).max(axis=1) for i in range(0, len(unit), 2000)])
    novelty = 1 - nearest

    score = (_percentile(uncertainty) + _percentile(novelty)) / 2
    recordings = {r for _, r, _ in unlabeled}
    cap = n if len(recordings) <= 4 else max(1, int(n * MAX_SHARE))
    picked, taken, per = [], np.zeros((0, whole)), {}
    for i in np.argsort(-score):
        r = unlabeled[i][1]
        if per.get(r, 0) >= cap or (len(taken) and (taken @ unit[i]).max() > NEAR_COPY):
            continue
        picked.append(int(i))
        taken = np.vstack([taken, unit[i]])
        per[r] = per.get(r, 0) + 1
        if len(picked) == n:
            break

    frames = [{"key": unlabeled[i][0], "recording": unlabeled[i][1], "score": round(float(score[i]), 4),
               "reads": STATES[int(p[i].argmax())], "confidence": round(float(p[i].max()), 3), "novelty": round(float(novelty[i]), 3)}
              for i in picked]
    SUGGESTIONS.parent.mkdir(parents=True, exist_ok=True)
    SUGGESTIONS.write_text(json.dumps({"made": int(time.time()), "pool": len(unlabeled), "frames": frames}))

    print(f"{len(frames)} suggested out of {len(unlabeled)} unlabeled frames -> {SUGGESTIONS}")
    print("  by recording: " + ", ".join(f"{scene_data.display_name(r)} {c}" for r, c in sorted(per.items(), key=lambda kv: -kv[1])))
    reads = np.bincount([STATES.index(f["reads"]) for f in frames], minlength=4)
    print("  the model reads them as: " + ", ".join(f"{s} {c}" for s, c in zip(STATES, reads)))
    print(f"  mean confidence {np.mean([f['confidence'] for f in frames]):.2f} (all unlabeled: {p.max(axis=1).mean():.2f}), "
          f"mean novelty {np.mean([f['novelty'] for f in frames]):.3f} (all unlabeled: {novelty.mean():.3f})")
    print("Label them with `./label.sh --suggested`.")
    return len(frames)
