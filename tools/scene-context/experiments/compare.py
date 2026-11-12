#!/usr/bin/env python3
"""Compare small image backbones for the scene model: YOLO11n-cls, MobileNetV3, EfficientNet-Lite0 (and MobileCLIP-S1 for reference).

  .venv/bin/python experiments/compare.py

Every model gets the same picture (the middle square of the frame, 224 px) and the same head (standardise + logistic regression on its
embedding), so the difference is the backbone. Two splits on all labeled frames, 4 states (on foot, in vehicle, menu, cutscene) and the
first/third-person view: leave-one-set-out (a whole recording is unseen) and a block split (80/20 blocks of 40 frames, 3 frames purged).
"""
import time
import warnings
from pathlib import Path

import numpy as np
import torch
from PIL import Image
from sklearn.linear_model import LogisticRegression
from sklearn.metrics import f1_score, recall_score
from sklearn.pipeline import make_pipeline
from sklearn.preprocessing import StandardScaler

warnings.filterwarnings("ignore")
here = Path(__file__).resolve().parent
import fuse  # noqa: E402

dev = "mps" if torch.backends.mps.is_available() else "cpu"
paths = fuse.frame_paths()
n = len(paths)
SIZE = 224


def square(path):
    im = Image.open(path).convert("RGB")
    w, h = im.size
    s = min(w, h)
    im = im.crop(((w - s) // 2, (h - s) // 2, (w - s) // 2 + s, (h - s) // 2 + s))
    return im.resize((SIZE, SIZE), Image.BILINEAR)


def batches(fn, norm, bs=32):
    out = []
    mean, std = (torch.tensor(v, device=dev).view(1, 3, 1, 1) for v in norm)
    for i in range(0, n, bs):
        x = torch.stack([torch.from_numpy(np.asarray(square(p))).permute(2, 0, 1) for p in paths[i:i + bs]]).float().to(dev) / 255
        with torch.no_grad():
            out.append(fn((x - mean) / std).float().cpu().numpy())
    return np.concatenate(out)


def timm_model(name):
    import timm
    m = timm.create_model(name, pretrained=True, num_classes=0).to(dev).eval()
    cfg = timm.data.resolve_data_config({}, model=m)
    return m, (cfg["mean"], cfg["std"])


def yolo_cls():
    from ultralytics import YOLO
    net = YOLO("yolo11n-cls.pt").model.to(dev).eval()
    head = net.model[-1]

    def f(x):
        for layer in net.model[:-1]:
            x = layer(x)
        return head.pool(head.conv(x)).flatten(1)

    return f, net, ((0.0, 0.0, 0.0), (1.0, 1.0, 1.0))


def params(m):
    return sum(p.numel() for p in m.parameters()) / 1e6


def latency(fn, runs=40):
    x = torch.rand(1, 3, SIZE, SIZE, device=dev)
    with torch.no_grad():
        for _ in range(8):
            fn(x)
        if dev == "mps":
            torch.mps.synchronize()
        t = time.perf_counter()
        for _ in range(runs):
            fn(x)
        if dev == "mps":
            torch.mps.synchronize()
    return (time.perf_counter() - t) / runs * 1000


models = {}
cache = here / "cache"
for label, name in [("MobileNetV3-L", "mobilenetv3_large_100.ra_in1k"), ("EfficientNet-Lite0", "efficientnet_lite0.ra_in1k")]:
    f = cache / f"cmp_{label}.npz"
    m, norm = timm_model(name)
    if not f.exists():
        np.savez(f, x=batches(m, norm))
    models[label] = (np.load(f)["x"], params(m), latency(m))
f = cache / "cmp_YOLO11n-cls.npz"
yf, ynet, ynorm = yolo_cls()
if not f.exists():
    np.savez(f, x=batches(yf, ynorm))
models["YOLO11n-cls"] = (np.load(f)["x"], params(ynet), latency(yf))
models["MobileCLIP-S1 (reference)"] = (fuse.x, 21.5, float("nan"))  # image tower, from the Core ML export measurement: about 5 ms


def probe():
    return make_pipeline(StandardScaler(), LogisticRegression(max_iter=2000, C=0.5, class_weight="balanced"))


def folds(mode):
    pos = np.arange(n)
    for s in range(len(fuse.SESSIONS)):
        te = fuse.sid == s
        if mode == "set":
            yield ~te, te
        else:
            blk = (np.cumsum(te) - 1) // 40
            for k in range(5):
                test = te & (blk % 5 == k)
                near = np.zeros(n, bool)
                for t in np.flatnonzero(test):
                    near[max(0, t - 3):t + 4] = True
                yield ~test & ~near, test


def run(x, mode):
    pred, vpred = np.zeros(n, int), np.full(n, -1)
    for tr, te in folds(mode):
        c = probe().fit(x[tr], fuse.y[tr])
        pred[te] = c.predict(x[te])
        trv, tev = tr & (fuse.view >= 0), te & (fuse.view >= 0)
        if tev.any():
            vpred[tev] = probe().fit(x[trv], fuse.view[trv]).predict(x[tev])
    return pred, vpred


print(f"{n} frames; device {dev}")
rows = []
for name, (x, mp, ms) in models.items():
    r = {"name": name, "dim": x.shape[1], "params": mp, "ms": ms}
    for mode in ["set", "block"]:
        pred, vpred = run(x, mode)
        has = fuse.view >= 0
        r[mode] = ((pred == fuse.y).mean(), f1_score(fuse.y, pred, average="macro"),
                   recall_score(fuse.y, pred, labels=range(4), average=None), (vpred[has] == fuse.view[has]).mean())
    rows.append(r)

for mode, title in [("set", "A whole recording unseen (leave-one-set-out)"), ("block", "Same recordings, held-out blocks of frames")]:
    print(f"\n{title}")
    print(f"  {'model':27}{'acc':>6}{'F1':>6}{'view':>6}   recall on_foot / vehicle / menu / cutscene")
    for r in rows:
        a, f1, rec, v = r[mode]
        print(f"  {r['name']:27}{a:6.3f}{f1:6.3f}{v:6.3f}   " + " / ".join(f"{x:.2f}" for x in rec))
print("\nSize and speed (PyTorch on this GPU, batch 1, 224 px)")
for r in rows:
    print(f"  {r['name']:27}{r['params']:6.1f} M params  {r['dim']:5} numbers  " + (f"{r['ms']:.1f} ms" if r["ms"] == r["ms"] else ""))
