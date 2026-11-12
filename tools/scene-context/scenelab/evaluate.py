"""How well does the shipped design read recordings it has not seen, and is an export what it should be?

`report`: every recording is read by a head fitted on all the others (the cached Core ML features, see features.py), with the zero-shot sentences
mixed in as the app does; then the filter is replayed with simulated presses on the sequence recordings (filtersim.py).
`write_sequences`: those readings and the presses, for the Rust replay test (crates/openps-scene/tests/replay.rs).
`check`: the exported models and probe against the cached features.
"""
import json
import warnings
from pathlib import Path

import numpy as np
from sklearn.linear_model import LogisticRegression
from sklearn.metrics import f1_score, recall_score
from sklearn.pipeline import make_pipeline
from sklearn.preprocessing import StandardScaler

from . import data as scene_data
from . import features, filtersim, probe
from .paths import CACHE, MODELS
from .pictures import HEAD_C, LAYOUT, PICTURES, ZERO_SHOT_WEIGHT

warnings.filterwarnings("ignore")
SEQUENCES = CACHE / "sequences.json"


def _head(c):
    return make_pipeline(StandardScaler(), LogisticRegression(max_iter=3000, C=c, class_weight="balanced"))


def _proba(clf, xs, k):
    p = np.zeros((len(xs), k))
    p[:, clf.classes_] = clf.predict_proba(xs)
    return p


def zero_shot_text(model_dir=MODELS):
    return np.array(json.loads((Path(model_dir) / "scene-probe.json").read_text())["zero_shot"]["text"])


def out_of_recording(x, text, c=HEAD_C, zero_shot_weight=ZERO_SHOT_WEIGHT):
    """Context and view probabilities of every frame from a head that never saw its recording."""
    data = scene_data.load()
    if len(data.names) < 2:
        raise SystemExit("one recording cannot be read by a head that has seen no other")
    zs5 = probe.zero_shot(x, text)
    pc, pv = np.zeros((len(data), 4)), np.full((len(data), 2), 0.5)
    has_view = data.view >= 0
    for s in range(len(data.names)):
        te = data.sid == s
        p = _proba(_head(c).fit(x[~te], data.y[~te]), x[te], 4)
        pc[te] = probe.mix(p, zs5[te], zero_shot_weight)
        trv, tev = ~te & has_view, te & has_view
        if tev.any():
            pv[tev] = _proba(_head(c).fit(x[trv], data.view[trv]), x[tev], 2)
    return pc, pv


def shipped_oof(model_dir=MODELS, c=HEAD_C, zero_shot_weight=ZERO_SHOT_WEIGHT):
    return out_of_recording(features.cached(), zero_shot_text(model_dir), c, zero_shot_weight)


def per_recording(pc, pv):
    data = scene_data.load()
    pred, has = pc.argmax(1), data.view >= 0
    print(f"  {'recording':42}{'frames':>7}{'state':>7}{'view':>7}   recall on foot / vehicle / menu / cutscene")
    for s, name in enumerate(data.display):
        m = data.sid == s
        rec = recall_score(data.y[m], pred[m], labels=range(4), average=None, zero_division=0)
        seen = [f"{r:.2f}" if (data.y[m] == k).any() else "  - " for k, r in enumerate(rec)]
        v = m & has
        view = f"{(pv.argmax(1)[v] == data.view[v]).mean():.3f}" if v.any() else "    -"
        print(f"  {name:42}{m.sum():7}{(pred[m] == data.y[m]).mean():7.3f}{view:>7}   " + " / ".join(seen))
    print(f"  {'all':42}{len(data):7}{(pred == data.y).mean():7.3f}{(pv.argmax(1)[has] == data.view[has]).mean():7.3f}"
          f"   F1 {f1_score(data.y, pred, average='macro'):.3f}")


def report(model_dir=MODELS, c=HEAD_C, zero_shot_weight=ZERO_SHOT_WEIGHT, seeds=10):
    data = scene_data.load()
    print(f"\n{len(data)} frames in {len(data.names)} recordings; head C={c}, zero-shot weight {zero_shot_weight}")
    pc, pv = shipped_oof(model_dir, c, zero_shot_weight)
    print("One picture at a time, recordings unseen:")
    per_recording(pc, pv)
    seq = np.isin(data.sid, filtersim.SEQ)
    if seq.any():
        true_sw = sum(filtersim.switches(data.y[data.sid == s]) for s in filtersim.SEQ) / seq.sum() * 100
        filtersim.show(f"with the filter and simulated presses, {len(filtersim.SEQ)} sequence recordings",
                       filtersim.evaluate((pc, pv), seeds=seeds), true_sw)


def write_sequences(model_dir=MODELS, path=SEQUENCES):
    """What the Rust replay test runs the filter on: out-of-recording readings with simulated presses (five sets per recording)."""
    data = scene_data.load()
    if len(data.names) < 2:
        print("replay sequences skipped: they need at least two labeled recordings")
        return
    pc, pv = shipped_oof(model_dir)
    sessions = []
    for s in filtersim.SEQ:
        m = data.sid == s
        t = data.t[m]
        runs = [filtersim.simulate_actions(data.y[m], data.view[m], t, np.random.default_rng(seed), 1.0) for seed in range(5)]
        sessions.append({"name": data.names[s], "t": t.tolist(), "truth": data.y[m].tolist(), "view_truth": data.view[m].tolist(),
                         "context": pc[m].tolist(), "view": pv[m].tolist(), "actions": runs})
    Path(path).parent.mkdir(parents=True, exist_ok=True)
    Path(path).write_text(json.dumps({"sessions": sessions}))
    print(f"wrote {path} ({sum(len(s['t']) for s in sessions)} frames)")


def check(model_dir=MODELS, clip=None, backbone=None, sample=40):
    """The probe was made for these pictures; the cached features are what the models give now; the probe reads them as the app will."""
    model_dir = Path(model_dir)
    saved = json.loads((model_dir / "scene-probe.json").read_text())
    if saved["layout"] != LAYOUT:
        raise SystemExit(f"the probe reads {saved['layout']}, the code cuts {LAYOUT}: train again")
    data = scene_data.load()
    cached = features.cached()
    if clip is None:
        import coremltools as ct

        clip = ct.models.MLModel(str(model_dir / "scene-encoder.mlpackage"))
        backbone = ct.models.MLModel(str(model_dir / "scene-backbone.mlpackage"))
    idx = np.sort(np.random.default_rng(0).choice(len(data), min(sample, len(data)), replace=False))
    fresh = np.stack([features.embed(clip, backbone, data.paths[i]) for i in idx])
    at = 0
    for name, _, _, dim in PICTURES:
        a, b = fresh[:, at:at + dim], cached[idx, at:at + dim]
        cos = (a * b).sum(1) / np.linalg.norm(a, axis=1) / np.linalg.norm(b, axis=1)
        print(f"check {name}: the models now vs the cached features, cosine min {cos.min():.4f}")
        at += dim
    pred = probe.context(saved, cached).argmax(1)
    print(f"check probe: reads {(pred == data.y).mean():.3f} of the frames it was trained on right")
