#!/usr/bin/env python3
"""Score the trained model on real stream frames it never saw.
  target/train/venv/bin/python scripts/eval_prompt_model.py OUT_DIR [threshold]
Frames: the map screen (960x540 luma plane of a live stream) and a crop of a PS5 menu (tests/data). Expected prompts are listed below.
"""
import os, sys, numpy as np, torch
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from train_prompt_model import Net, decode
KINDS = ["Cross", "Circle", "Square", "Triangle", "L1", "R1", "L2", "R2", "LStick", "RStick", "L3", "R3", "DPad", "Options", "Create", "Mute"]
D = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "crates/openps-metal/tests/data")
FRAMES = [
    ("map screen", "live_plane_960x540.gray", 960, 540,
     [("L1", 54, 36), ("R1", 208, 36), ("L2", 55, 62), ("R2", 247, 62), ("L3", 754, 487), ("Circle", 71, 509), ("LStick", 153, 509),
      ("RStick", 225, 509), ("Cross", 304, 509), ("Triangle", 423, 509)]),
    ("pause menu", "live_pause_960x540.gray", 960, 540, [("L1", 54, 36), ("R1", 207, 36), ("Cross", 53, 509), ("Circle", 140, 509)]),
    ("PS5 menu crop", "ps5_menu_420x540.gray", 420, 540, [("Cross", 39, 85), ("Circle", 39, 512)]),
    ("game (no prompts)", "fp_game_astro_640x352.gray", 640, 352, []),
    ("nature (no prompts)", "fp_bird_005_640x360.gray", 640, 360, []),
]
out, thr = sys.argv[1], float(sys.argv[2]) if len(sys.argv) > 2 else 0.4
m = Net(); m.load_state_dict(torch.load(os.path.join(out, "prompt_net.pt"), map_location="cpu")); m.eval()
found_all = missed_all = extra_all = 0
for name, f, w, h, want in FRAMES:
    img = np.frombuffer(open(os.path.join(D, f), "rb").read(), dtype=np.uint8).reshape(h, w)
    H, W = (h + 7) // 8 * 8, (w + 7) // 8 * 8
    pad = np.zeros((H, W), np.uint8); pad[:h, :w] = img
    x = torch.from_numpy(pad).float().div(255).sub(0.5)[None, None]
    with torch.no_grad():
        hm, wh, off = m(x)
    dets = decode(hm, wh, off, thr)[0]
    print(f"== {name}: {len(dets)} detections")
    used = set(); found = 0
    for k, x0, y0 in want:
        ok = [i for i, d in enumerate(dets) if KINDS[d[0]] == k and abs(d[2] - x0) < 6 and abs(d[3] - y0) < 6 and i not in used]
        if ok:
            used.add(ok[0]); found += 1
        else:
            near = [f"{KINDS[d[0]]}@({d[2]:.0f},{d[3]:.0f}) {d[1]:.2f}" for d in dets if abs(d[2] - x0) < 8 and abs(d[3] - y0) < 8]
            print(f"   MISSED {k} at ({x0},{y0}); nearby: {near}")
    extras = [d for i, d in enumerate(dets) if i not in used]
    for d in extras:
        print(f"   EXTRA {KINDS[d[0]]} ({d[2]:.0f},{d[3]:.0f}) {d[4]:.0f}x{d[5]:.0f} score {d[1]:.2f}")
    print(f"   found {found}/{len(want)}, extras {len(extras)}")
    found_all += found; missed_all += len(want) - found; extra_all += len(extras)
print(f"TOTAL found {found_all}/{found_all + missed_all}, extra detections {extra_all}")
