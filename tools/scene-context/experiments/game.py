#!/usr/bin/env python3
"""Transfer tests on the cached MobileCLIP features: other game (Watch Dogs) and other video source (GTA V mp4)."""
import itertools, json, warnings
from pathlib import Path

import numpy as np
from sklearn.linear_model import LogisticRegression
from sklearn.metrics import f1_score, recall_score
from sklearn.pipeline import make_pipeline
from sklearn.preprocessing import StandardScaler

warnings.filterwarnings("ignore")
base = Path(__file__).resolve().parent.parent / "data"
cache = Path(__file__).resolve().parent / "cache"
CLASSES = ["on_foot", "land_vehicle", "air_vehicle", "water_vehicle", "menu_or_cutscene"]
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
y, view, sid = [], [], []
for s, (name, keep) in enumerate(SESSIONS):
    labs = json.load(open(base / name / "labels.json"))["labels"]
    for i, l in enumerate(labs):
        if keep(i):
            y.append(CLASSES.index({"menu": "menu_or_cutscene", "cutscene": "menu_or_cutscene"}.get(l["context"], l["context"])))
            view.append({"first_person": 0, "third_person": 1}.get(l.get("view"), -1))
            sid.append(s)
y, view, sid = np.array(y), np.array(view), np.array(sid)


def probe():
    return make_pipeline(StandardScaler(), LogisticRegression(max_iter=3000, C=0.5, class_weight="balanced"))


def softmax(z):
    z = np.exp(z - z.max(1, keepdims=True))
    return z / z.sum(1, keepdims=True)


def run(title, arch, tr, te):
    d = np.load(cache / f"{arch}.npz")
    x, text = d["x"], d["text"]
    present = sorted(set(y[te]))
    zs = softmax(100 * x[te] @ text.T)
    clf = probe().fit(x[tr], y[tr])
    pp = np.zeros((te.sum(), 5))
    pp[:, clf.classes_] = clf.predict_proba(x[te])
    for name, p in [("zero-shot", zs), ("probe", pp), ("zero-shot+probe", 0.5 * zs + 0.5 * pp)]:
        pred = p.argmax(1)
        f1 = f1_score(y[te], pred, labels=present, average="macro", zero_division=0)
        per = recall_score(y[te], pred, labels=present, average=None, zero_division=0)
        print(f"{title} | {arch} {name}: macro-F1 {f1:.2f} acc {(pred == y[te]).mean():.2f} | "
              + " ".join(f"{CLASSES[c][:6]}={r:.2f}" for c, r in zip(present, per)) + f" (n={te.sum()})")
    pred = (0.5 * zs + 0.5 * pp).argmax(1)
    truth, veh = y[te], {1, 2, 3}
    tv = np.isin(truth, list(veh))
    pv = np.isin(pred, list(veh))
    print(f"   switching view of zero-shot+probe: vehicle frames detected as some vehicle {pv[tv].mean():.2f}, "
          f"right vehicle type {(pred[tv] == truth[tv]).mean():.2f}; false vehicle on on-foot {pv[truth == 0].mean():.2f}, "
          f"on menu/cutscene {pv[truth == 4].mean():.2f}")


ps, wd, gta_mp4, walk_drive = sid <= 3, sid == 5, np.isin(sid, [4, 6, 7, 8, 9]), sid >= 7
for arch in ["MobileCLIP-S1", "MobileCLIP-S2"]:
    run("OTHER GAME  train all GTA -> test Watch Dogs", arch, ~wd, wd)
    run("OTHER SOURCE train PS captures -> test GTA mp4", arch, ps, gta_mp4)
    run("OTHER SOURCE train PS captures + Watch Dogs -> test GTA mp4", arch, ps | wd, gta_mp4)

for arch in ["MobileCLIP-S1", "MobileCLIP-S2"]:
    x = np.load(cache / f"{arch}.npz")["x"]
    for title, tr, te in [("PS captures -> GTA mp4", ps, gta_mp4),
                          ("everything else -> the 3 view videos", ~walk_drive, walk_drive)]:
        tr, te = tr & (view >= 0), te & (view >= 0)
        pred = probe().fit(x[tr], view[tr]).predict(x[te])
        r = recall_score(view[te], pred, labels=[0, 1], average=None, zero_division=0)
        print(f"VIEW train {title} | {arch}: macro-F1 {f1_score(view[te], pred, average='macro'):.2f} "
              f"first={r[0]:.2f} third={r[1]:.2f} (first n={int((view[te] == 0).sum())}, third n={int((view[te] == 1).sum())})")
