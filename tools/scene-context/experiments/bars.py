#!/usr/bin/env python3
"""Cinematic-bar features: brightness of the top, bottom, left and right bands of the frame (cutscenes are letterboxed).

  .venv-vlm/bin/python experiments/bars.py        writes cache/bars.npy, one row per labeled frame
"""
from pathlib import Path

import numpy as np
from PIL import Image

import fuse

BAND = 0.1


def bars(path):
    g = np.asarray(Image.open(path).convert("L"), dtype=np.float32) / 255
    h, w = g.shape
    bh, bw = int(h * BAND), int(w * BAND)
    parts = [g[:bh], g[-bh:], g[:, :bw], g[:, -bw:], g[bh:-bh, bw:-bw]]
    return [v for p in parts for v in (p.mean(), p.std())]


if __name__ == "__main__":
    out = np.array([bars(p) for p in fuse.frame_paths()])
    np.save(Path(__file__).resolve().parent / "cache" / "bars.npy", out)
    print(out.shape)
