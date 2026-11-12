#!/usr/bin/env python3
"""Does a center-bottom crop (where the character/vehicle sits in third person) generalize better for first vs third person?"""
import itertools, json, warnings
from pathlib import Path

import numpy as np
import open_clip
import torch
from PIL import Image
from sklearn.linear_model import LogisticRegression
from sklearn.metrics import f1_score, recall_score
from sklearn.pipeline import make_pipeline
from sklearn.preprocessing import StandardScaler

warnings.filterwarnings("ignore")
base = Path(__file__).resolve().parent.parent / "data"
cache = Path(__file__).resolve().parent / "cache"
SESSIONS = [
    ("video-1794137021", lambda i: True),
    ("video-1794138431", lambda i: i < 333 or (i - 333) % 10 == 0),
    ("video-1794140032", lambda i: True),
    ("video-1794142097", lambda i: True),
]
files, view, sid, seg = [], [], [], []
for s, (name, keep) in enumerate(SESSIONS):
    labs = json.load(open(base / name / "labels.json"))["labels"]
    kept = [l for i, l in enumerate(labs) if keep(i)]
    for k, (_, run) in enumerate(itertools.groupby(kept, key=lambda l: l["context"])):
        for l in run:
            files.append(base / name / l["file"])
            view.append({"first_person": 0, "third_person": 1}.get(l.get("view"), -1))
            sid.append(s)
            seg.append(f"{s}-{k}")
view, sid, seg = np.array(view), np.array(sid), np.array(seg)
ok = view >= 0

arch = "MobileCLIP-S2"
glob = np.load(cache / f"{arch}.npz")["x"]
crop_path = cache / "crop_S2.npy"
if crop_path.exists():
    crop = np.load(crop_path)
else:
    dev = "mps" if torch.backends.mps.is_available() else "cpu"
    model, _, pre = open_clip.create_model_and_transforms(arch, pretrained="datacompdr")
    model = model.to(dev).eval()
    feats = []
    with torch.no_grad():
        for i in range(0, len(files), 16):
            batch = []
            for f in files[i:i + 16]:
                im = Image.open(f).convert("RGB")
                w, h = im.size
                batch.append(pre(im.crop((int(w * 0.25), int(h * 0.35), int(w * 0.75), h))))
            e = model.encode_image(torch.stack(batch).to(dev))
            feats.append((e / e.norm(dim=-1, keepdim=True)).cpu().numpy())
    crop = np.concatenate(feats)
    np.save(crop_path, crop)


def probe():
    return make_pipeline(StandardScaler(), LogisticRegression(max_iter=3000, C=0.5, class_weight="balanced"))


def report(tag, x):
    out = []
    for s in range(len(SESSIONS)):
        tr, te = (sid != s) & ok, (sid == s) & ok
        pred = probe().fit(x[tr], view[tr]).predict(x[te])
        present = sorted(set(view[te]))
        r = recall_score(view[te], pred, labels=present, average=None, zero_division=0)
        out.append(f"s{s + 1}: " + "/".join(f"{v:.2f}" for v in r) + ("" if len(present) == 2 else " (third only)"))
    pos = np.arange(len(view))
    pred = np.full(len(view), -1)
    vseg = np.array([f"{a}-{b}" for a, b in zip(sid, np.cumsum(np.r_[0, np.diff(view) != 0]))])
    for g in np.unique(vseg[ok]):
        te = np.where((vseg == g) & ok)[0]
        near = (sid == sid[te[0]]) & (pos >= te[0] - 2) & (pos <= te[-1] + 2)
        tr = ok & ~near
        pred[te] = probe().fit(x[tr], view[tr]).predict(x[te])
    f1 = f1_score(view[ok], pred[ok], average="macro")
    print(f"{tag}: segment-held-out macro-F1 {f1:.2f}; session holdout first/third recall -> " + " | ".join(out))


report("global only         ", glob)
report("center-bottom crop  ", crop)
report("global + crop       ", np.hstack([glob, crop]))
