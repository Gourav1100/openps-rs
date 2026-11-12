#!/usr/bin/env python3
"""Zero-shot prompt variants on the cached MobileCLIP features (same frame order as eval.py). No training involved."""
import itertools, json
from pathlib import Path

import numpy as np
import open_clip
import torch
from sklearn.metrics import f1_score, recall_score

base = Path(__file__).resolve().parent.parent / "data"
CLASSES = ["on_foot", "land_vehicle", "air_vehicle", "water_vehicle", "menu_or_cutscene"]
SESSIONS = [
    ("video-1794137021", lambda i: True),
    ("video-1794138431", lambda i: i < 333 or (i - 333) % 10 == 0),
    ("video-1794140032", lambda i: True),
    ("video-1794142097", lambda i: True),
]
y, view = [], []
for name, keep in SESSIONS:
    labs = json.load(open(base / name / "labels.json"))["labels"]
    kept = [l for i, l in enumerate(labs) if keep(i)]
    for l in kept:
        y.append(CLASSES.index({"menu": "menu_or_cutscene", "cutscene": "menu_or_cutscene"}.get(l["context"], l["context"])))
        view.append({"first_person": 0, "third_person": 1}.get(l.get("view"), -1))
y, view = np.array(y), np.array(view)

V1 = {
    "on_foot": ["a third-person view of a character walking on foot in a video game"],
    "land_vehicle": ["a car or motorcycle driving on a road in a video game"],
    "air_vehicle": ["a helicopter or airplane flying in the sky in a video game"],
    "water_vehicle": ["a boat or jet ski on the water in a video game"],
    "menu_or_cutscene": ["a menu, map or loading screen in a video game"],
}
V2 = {
    "on_foot": ["a third-person view of a character walking on foot in a video game",
                "a first-person view of walking on foot on the ground in a video game",
                "a video game character standing or running outdoors"],
    "land_vehicle": ["a car or motorcycle driving on a road in a video game",
                     "a first-person view from inside a car with a steering wheel and dashboard in a video game",
                     "a motorcycle seen from behind on a road in a video game"],
    "air_vehicle": ["a helicopter or airplane flying in the sky in a video game",
                    "a first-person view from the cockpit of an aircraft in a video game"],
    "water_vehicle": ["a boat or jet ski on the water in a video game",
                      "a first-person view from a boat or jet ski over water at night in a video game"],
    "menu_or_cutscene": ["a menu, map or loading screen in a video game",
                         "a video game pause menu or cutscene"],
}

for arch in ["MobileCLIP-S1", "MobileCLIP-S2"]:
    x = np.load(Path(__file__).resolve().parent / "cache" / f"{arch}.npz")["x"]
    model, _, _ = open_clip.create_model_and_transforms(arch, pretrained="datacompdr")
    model.eval()
    tok = open_clip.get_tokenizer(arch)
    for tag, prompts in [("v1 (one prompt per class)", V1), ("v2 (prompt ensemble)", V2)]:
        with torch.no_grad():
            texts = []
            for c in CLASSES:
                t = model.encode_text(tok(prompts[c]))
                t = t / t.norm(dim=-1, keepdim=True)
                t = t.mean(0)
                texts.append((t / t.norm()).numpy())
        pred = (x @ np.stack(texts).T).argmax(1)
        per = recall_score(y, pred, labels=range(5), average=None, zero_division=0)
        f1 = f1_score(y, pred, labels=range(5), average="macro")
        print(f"{arch} zero-shot {tag}: macro-F1 {f1:.2f} | " + " ".join(f"{c[:6]}={r:.2f}" for c, r in zip(CLASSES, per)))
