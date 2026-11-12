#!/usr/bin/env python3
"""Spike: which small model best tells the GTA 5 scene context (and first/third person) from one frame, using all labeled sessions.

Run from ~/openps-captures/label:  venv/bin/python experiments/eval.py   (features are cached in cache/).
Strict splits: leave-one-segment-out (a whole drive/flight is held out) and leave-one-session-out.
YOLO11n is AGPL: measured here for comparison only, not a shipping candidate.
"""
import itertools, json, warnings
from pathlib import Path

import numpy as np
import torch
from PIL import Image
from sklearn.linear_model import LogisticRegression
from sklearn.metrics import confusion_matrix, f1_score, recall_score
from sklearn.pipeline import make_pipeline
from sklearn.preprocessing import StandardScaler

warnings.filterwarnings("ignore")
base = Path(__file__).resolve().parent.parent / "data"
cache = Path(__file__).resolve().parent / "cache"
cache.mkdir(exist_ok=True)
CLASSES = ["on_foot", "land_vehicle", "air_vehicle", "water_vehicle", "menu_or_cutscene"]
PROMPTS = {
    "on_foot": "a third-person view of a character walking on foot in a video game",
    "land_vehicle": "a car or motorcycle driving on a road in a video game",
    "air_vehicle": "a helicopter or airplane flying in the sky in a video game",
    "water_vehicle": "a boat or jet ski on the water in a video game",
    "menu_or_cutscene": "a menu, map or loading screen in a video game",
}
VIEWS = ["first_person", "third_person"]
VIEW_PROMPTS = [
    "a first-person view in a video game, seen through the character's eyes or from inside a vehicle cockpit",
    "a third-person view in a video game, seen from behind the character or vehicle",
]
# Session 2 ends in 111 frames of one static pause map: keep every 10th so it does not inflate the menu class.
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

files, y, view, sid, seg = [], [], [], [], []
for s, (name, keep) in enumerate(SESSIONS):
    labs = json.load(open(base / name / "labels.json"))["labels"]
    kept = [(i, l) for i, l in enumerate(labs) if keep(i)]
    for k, (_, run) in enumerate(itertools.groupby(kept, key=lambda p: p[1]["context"])):
        for _, l in run:
            files.append(base / name / l["file"])
            y.append(CLASSES.index({"menu": "menu_or_cutscene", "cutscene": "menu_or_cutscene"}.get(l["context"], l["context"])))
            view.append(VIEWS.index(l["view"]) if l.get("view") else -1)
            sid.append(s)
            seg.append(f"{s}-{k}")
y, view, sid, seg = np.array(y), np.array(view), np.array(sid), np.array(seg)
print(f"{len(y)} frames:", {c: int((y == i).sum()) for i, c in enumerate(CLASSES)}, "per session", np.bincount(sid).tolist())
print("view labels:", {v: int((view == i).sum()) for i, v in enumerate(VIEWS)})
images = [Image.open(f).convert("RGB") for f in files]
dev = "mps" if torch.backends.mps.is_available() else "cpu"


def clip_features(arch):
    import open_clip
    model, _, pre = open_clip.create_model_and_transforms(arch, pretrained="datacompdr")
    model = model.to(dev).eval()
    tok = open_clip.get_tokenizer(arch)

    def enc_text(texts):
        t = model.encode_text(tok(texts).to(dev))
        return (t / t.norm(dim=-1, keepdim=True)).cpu().numpy()

    with torch.no_grad():
        txt, vtxt = enc_text([PROMPTS[c] for c in CLASSES]), enc_text(VIEW_PROMPTS)
        feats = []
        for i in range(0, len(images), 16):
            f = model.encode_image(torch.stack([pre(im) for im in images[i:i + 16]]).to(dev))
            feats.append((f / f.norm(dim=-1, keepdim=True)).cpu().numpy())
    return np.concatenate(feats), txt, vtxt


def timm_features(name):
    import timm
    model = timm.create_model(name, pretrained=True, num_classes=0).to(dev).eval()
    pre = timm.data.create_transform(**timm.data.resolve_data_config({}, model=model))
    with torch.no_grad():
        feats = [model(torch.stack([pre(im) for im in images[i:i + 16]]).to(dev)).cpu().numpy() for i in range(0, len(images), 16)]
    return (np.concatenate(feats),)


def yolo_features():
    from ultralytics import YOLO
    model = YOLO("yolo11n.pt")
    out = np.zeros((len(images), 80), dtype=np.float32)
    for i, f in enumerate(files):
        r = model.predict(str(f), imgsz=640, conf=0.05, verbose=False, device=dev)[0]
        for c, p in zip(r.boxes.cls.cpu().numpy().astype(int), r.boxes.conf.cpu().numpy()):
            out[i, c] = max(out[i, c], p)
    return (out,)


def cached(name, fn):
    p = cache / f"{name}.npz"
    if p.exists():
        return dict(np.load(p))
    r = fn()
    d = dict(zip(["x", "text", "vtext"], r))
    np.savez(p, **d)
    return d


def probe():
    return make_pipeline(StandardScaler(), LogisticRegression(max_iter=3000, C=0.5, class_weight="balanced"))


def leave_one_segment_out(x, labels, groups, valid=None, purge=2):
    pred = np.full(len(labels), -1)
    pos = np.arange(len(labels))
    valid = np.ones(len(labels), bool) if valid is None else valid
    for g in np.unique(groups[valid]):
        test = np.where((groups == g) & valid)[0]
        near = (sid == sid[test[0]]) & (pos >= test[0] - purge) & (pos <= test[-1] + purge)
        train = valid & ~near
        if len(set(labels[train])) < 2:
            continue
        pred[test] = probe().fit(x[train], labels[train]).predict(x[test])
    return pred


def show(name, truth, pred, labels, names):
    per = recall_score(truth, pred, labels=labels, average=None, zero_division=0)
    f1 = f1_score(truth, pred, labels=labels, average="macro", zero_division=0)
    print(f"{name}: macro-F1 {f1:.2f} acc {(pred == truth).mean():.2f} | " + " ".join(f"{names[l][:6]}={r:.2f}" for l, r in zip(labels, per)))
    return f1


def session_holdout(tag, x):
    for s in range(len(SESSIONS)):
        tr, te = sid != s, sid == s
        pred = probe().fit(x[tr], y[tr]).predict(x[te])
        show(f"  {tag}, hold out session {s + 1}", y[te], pred, sorted(set(y[te])), CLASSES)


def ensemble_report(x, text):
    z = np.exp(100 * (x @ text.T) - (100 * (x @ text.T)).max(1, keepdims=True))
    zs = z / z.sum(1, keepdims=True)

    def mixed(train, test):
        clf = probe().fit(x[train], y[train])
        p = np.zeros((test.sum(), 5))
        p[:, clf.classes_] = clf.predict_proba(x[test])
        return (0.5 * zs[test] + 0.5 * p).argmax(1)

    pos, pred = np.arange(len(y)), np.zeros_like(y)
    for g in np.unique(seg):
        te = np.where(seg == g)[0]
        near = (sid == sid[te[0]]) & (pos >= te[0] - 2) & (pos <= te[-1] + 2)
        mask = np.zeros(len(y), bool)
        mask[te] = True
        pred[te] = mixed(~near, mask)
    show("  context zero-shot+probe, leave-one-segment-out", y, pred, list(range(5)), CLASSES)
    for s in range(len(SESSIONS)):
        te = sid == s
        show(f"  context zero-shot+probe, hold out session {s + 1}", y[te], mixed(sid != s, te), sorted(set(y[te])), CLASSES)


scores = {}
all5 = list(range(5))
has_view = view >= 0
for arch in ["MobileCLIP-S1", "MobileCLIP-S2"]:
    d = cached(arch, lambda a=arch: clip_features(a))
    print(f"\n== {arch}")
    show("  context zero-shot", y, (d["x"] @ d["text"].T).argmax(1), all5, CLASSES)
    scores[arch] = (d["x"], show("  context probe, leave-one-segment-out", y, leave_one_segment_out(d["x"], y, seg), all5, CLASSES))
    session_holdout("context probe", d["x"])
    vseg = np.array([f"{a}-{b}" for a, b in zip(sid, np.cumsum(np.r_[0, np.diff(view) != 0]))])
    show("  view zero-shot", view[has_view], (d["x"][has_view] @ d["vtext"].T).argmax(1), [0, 1], VIEWS)
    vp = leave_one_segment_out(d["x"], view, vseg, valid=has_view)
    show("  view probe, leave-one-segment-out", view[has_view], vp[has_view], [0, 1], VIEWS)
    ensemble_report(d["x"], d["text"])
d = cached("mobilenetv3", lambda: timm_features("mobilenetv3_large_100.ra_in1k"))
print("\n== MobileNetV3-Large (ImageNet)")
scores["MobileNetV3-L"] = (d["x"], show("  context probe, leave-one-segment-out", y, leave_one_segment_out(d["x"], y, seg), all5, CLASSES))
session_holdout("context probe", d["x"])
vseg = np.array([f"{a}-{b}" for a, b in zip(sid, np.cumsum(np.r_[0, np.diff(view) != 0]))])
vp = leave_one_segment_out(d["x"], view, vseg, valid=has_view)
show("  view probe, leave-one-segment-out", view[has_view], vp[has_view], [0, 1], VIEWS)
d = cached("yolo11n", yolo_features)
print("\n== YOLO11n COCO scores")
scores["YOLO11n"] = (d["x"], show("  context probe, leave-one-segment-out", y, leave_one_segment_out(d["x"], y, seg), all5, CLASSES))
session_holdout("context probe", d["x"])

best = max(scores, key=lambda k: scores[k][1])
print("\nconfusion (rows true, cols predicted), leave-one-segment-out,", best, CLASSES)
print(confusion_matrix(y, leave_one_segment_out(scores[best][0], y, seg), labels=all5))

print("\nVIEW HOLDOUT")
for arch in ["MobileCLIP-S1", "MobileCLIP-S2"]:
    x = cached(arch, lambda: None)["x"]
    for s in range(len(SESSIONS)):
        tr, te = (sid != s) & has_view, (sid == s) & has_view
        present = sorted(set(view[te]))
        pred = probe().fit(x[tr], view[tr]).predict(x[te])
        show(f"{arch} view probe, hold out session {s + 1} (first={int((view[te] == 0).sum())}, third={int((view[te] == 1).sum())})", view[te], pred, present, VIEWS)
