#!/usr/bin/env python3
"""Which part of the picture should MobileCLIP-S1 see? Compares the shipped middle square with the whole frame and with side crops.

  .venv/bin/python experiments/views.py

Same probe and splits as compare.py, so the numbers are comparable with it. Stored frames are 16:9; the app crops the middle square.
"""
import warnings
from pathlib import Path

import numpy as np
import open_clip
import torch
from PIL import Image
from timm.utils.model import reparameterize_model

import fuse
import probe_eval

warnings.filterwarnings("ignore")
cache = Path(__file__).resolve().parent / "cache"
dev = "mps" if torch.backends.mps.is_available() else "cpu"
S = 256

model, _, _ = open_clip.create_model_and_transforms("MobileCLIP-S1", pretrained="datacompdr")
model = reparameterize_model(model.eval()).to(dev)
paths = fuse.frame_paths()


def square(im, where):
    w, h = im.size
    s = min(w, h)
    left = {"left": 0, "center": (w - s) // 2, "right": w - s}[where]
    return im.crop((left, 0, left + s, s)).resize((S, S), Image.BILINEAR)


def corner(im, x, y):
    w, h = im.size
    return im.crop((int(w * x), int(h * y), int(w * (x + 0.3)), int(h * (y + 0.5)))).resize((S, S), Image.BILINEAR)


VIEWS = {
    "center": lambda im: [square(im, "center")],
    "squash": lambda im: [im.resize((S, S), Image.BILINEAR)],
    "wide": lambda im: [im.resize((448, S), Image.BILINEAR)],
    "left": lambda im: [square(im, "left")],
    "right": lambda im: [square(im, "right")],
    "hud_bl": lambda im: [corner(im, 0.0, 0.5)],  # minimap
    "hud_br": lambda im: [corner(im, 0.7, 0.5)],
    "hud_tr": lambda im: [corner(im, 0.7, 0.0)],
}


def embed(view, bs=32):
    f = cache / f"views_{view}.npy"
    if f.exists():
        return np.load(f)
    out = []
    for i in range(0, len(paths), bs):
        ims = [VIEWS[view](Image.open(p).convert("RGB")) for p in paths[i:i + bs]]
        x = torch.stack([torch.from_numpy(np.asarray(im[0])).permute(2, 0, 1) for im in ims]).float().to(dev) / 255
        with torch.no_grad():
            e = model.encode_image(x)
        out.append((e / e.norm(dim=-1, keepdim=True)).float().cpu().numpy())
    out = np.concatenate(out)
    np.save(f, out)
    return out


e = {v: embed(v) for v in VIEWS}
print(f"{len(paths)} frames")
probe_eval.report([
    ("whole frame squashed + minimap corner", np.hstack([e["squash"], e["hud_bl"]])),
    ("whole frame squashed + three HUD corners", np.hstack([e["squash"], e["hud_bl"], e["hud_br"], e["hud_tr"]])),
    ("middle square (shipped)", e["center"]),
    ("whole frame squashed to a square", e["squash"]),
    ("whole frame at 16:9 (256x448)", e["wide"]),
    ("left + middle + right squares, concatenated", np.hstack([e["left"], e["center"], e["right"]])),
    ("left + middle + right squares, averaged", (e["left"] + e["center"] + e["right"]) / 3),
    ("middle square + whole frame 16:9, concatenated", np.hstack([e["center"], e["wide"]])),
])
