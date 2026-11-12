#!/usr/bin/env python3
"""Train/test split on the combined dataset using the cached MobileCLIP features (run eval.py first).

  random split  stratified 80/20 by frame; neighbouring frames leak across it, so it is optimistic
  block split   80/20 by blocks of consecutive frames, with 2 frames purged around each test block
Reports the mean over several seeds.
"""
import json
import warnings
from pathlib import Path

import numpy as np
from sklearn.linear_model import LogisticRegression
from sklearn.metrics import confusion_matrix, f1_score, recall_score
from sklearn.model_selection import train_test_split
from sklearn.pipeline import make_pipeline
from sklearn.preprocessing import StandardScaler

warnings.filterwarnings("ignore")
here = Path(__file__).resolve().parent
base = here.parent / "data"
CLASSES = ["on_foot", "land_vehicle", "air_vehicle", "water_vehicle", "menu_or_cutscene"]
VIEWS = ["first_person", "third_person"]
SESSIONS = [
    ("video-1794137021", lambda i: True),
    ("video-1794138431", lambda i: i < 333 or (i - 333) % 10 == 0),
    ("video-1794140032", lambda i: True),
    ("video-1794142097", lambda i: True),
    ("mp4-gameplay_gta_v", lambda i: True),
    ("mp4-gameplay_watch_dog", lambda i: True),
    ("mp4-cutscenes_gta_v", lambda i: True),
    ("mp4-gta_v_first_person_diriving_App", lambda i: True),
    ("mp4-gta_v_first_person_walking_App", lambda i: True),
    ("mp4-gta_v_third_person_walking_App", lambda i: True),
]
BLOCK, PURGE, SEEDS, TEST = 10, 2, range(5), 0.2

y, view, sid, pos = [], [], [], []
for s, (name, keep) in enumerate(SESSIONS):
    for i, l in enumerate(json.load(open(base / name / "labels.json"))["labels"]):
        if keep(i):
            y.append(CLASSES.index({"menu": "menu_or_cutscene", "cutscene": "menu_or_cutscene"}.get(l["context"], l["context"])))
            view.append(VIEWS.index(l["view"]) if l.get("view") else -1)
            sid.append(s)
            pos.append(len(y) - 1)
y, view, sid, pos = map(np.array, (y, view, sid, pos))
n = len(y)
print(f"{n} frames from {len(SESSIONS)} sets:", {c: int((y == i).sum()) for i, c in enumerate(CLASSES)})
print("view labels:", {v: int((view == i).sum()) for i, v in enumerate(VIEWS)})


def probe():
    return make_pipeline(StandardScaler(), LogisticRegression(max_iter=3000, C=0.5, class_weight="balanced"))


def softmax(z):
    z = np.exp(z - z.max(1, keepdims=True))
    return z / z.sum(1, keepdims=True)


def random_split(seed):
    idx = np.arange(n)
    return train_test_split(idx, test_size=TEST, stratify=y, random_state=seed)


def block_split(seed):
    rng = np.random.default_rng(seed)
    within = np.concatenate([np.arange(c) for c in np.bincount(sid)])
    blocks = np.array([f"{s}-{w // BLOCK}" for s, w in zip(sid, within)])
    names = np.unique(blocks)
    test_blocks = set(rng.choice(names, size=int(len(names) * TEST), replace=False))
    test = np.flatnonzero(np.isin(blocks, list(test_blocks)))
    near = np.zeros(n, bool)
    for t in test:
        near[max(0, t - PURGE):t + PURGE + 1] = True
    near &= ~np.isin(np.arange(n), test)
    train = np.flatnonzero(~np.isin(np.arange(n), test) & ~near)
    return train, test


def fit_predict(x, text, labels, num, train, test, valid):
    tr, te = train[valid[train]], test[valid[test]]
    clf = probe().fit(x[tr], labels[tr])
    pp = np.zeros((len(te), num))
    pp[:, clf.classes_] = clf.predict_proba(x[te])
    zs = softmax(100 * x[te] @ text.T)
    return te, {"zero-shot": zs, "probe": pp, "zero-shot+probe": 0.5 * zs + 0.5 * pp}


def report(title, runs, labels, names):
    print(f"\n{title}")
    for model in runs[0][1]:
        accs, f1s, recs = [], [], []
        for te, preds in runs:
            pred = preds[model].argmax(1)
            accs.append((pred == labels[te]).mean())
            f1s.append(f1_score(labels[te], pred, labels=range(len(names)), average="macro", zero_division=0))
            recs.append(recall_score(labels[te], pred, labels=range(len(names)), average=None, zero_division=0))
        rec = np.mean(recs, axis=0)
        print(f"  {model:16} acc {np.mean(accs):.3f} +-{np.std(accs):.3f}  macro-F1 {np.mean(f1s):.3f}  recall "
              + " ".join(f"{nm[:6]}={r:.2f}" for nm, r in zip(names, rec)))


for arch in ["MobileCLIP-S1", "MobileCLIP-S2"]:
    d = np.load(here / "cache" / f"{arch}.npz")
    x, text, vtext = d["x"], d["text"], d["vtext"]
    assert len(x) == n, "cache is out of date; delete cache and run eval.py"
    print(f"\n==== {arch}")
    for split_name, split in [("random 80/20 split (optimistic)", random_split), ("block 80/20 split", block_split)]:
        ctx_runs, view_runs, last = [], [], None
        for seed in SEEDS:
            train, test = split(seed)
            ctx_runs.append(fit_predict(x, text, y, 5, train, test, np.ones(n, bool)))
            view_runs.append(fit_predict(x, vtext, view, 2, train, test, view >= 0))
            last = (train, test)
        print(f"\n-- {split_name}: train {len(last[0])}, test {len(last[1])} (seed {SEEDS[-1]}), mean of {len(SEEDS)} seeds")
        report("context (5 classes)", ctx_runs, y, CLASSES)
        report("view (first/third person, frames that have one)", view_runs, view, VIEWS)
        te, preds = ctx_runs[-1]
        print("\n  context confusion, zero-shot+probe (rows true, cols predicted), last seed", CLASSES)
        print(confusion_matrix(y[te], preds["zero-shot+probe"].argmax(1), labels=range(5)))
