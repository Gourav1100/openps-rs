#!/usr/bin/env python3
"""Zero-shot scene reading by small vision-language models, against the MobileCLIP-S1 probe, on the same labeled frames.

  .venv-vlm/bin/python experiments/vlm.py MODEL [MODEL ...]      (MODEL: a mlx-community repo name, see MODELS below)

A VLM gets the frame and a four-way multiple-choice question; the next-token probabilities of the letters A to D are its answer, so one
forward pass (no text generation) gives a decision in the time the model needs to read the picture. Latency is that pass, warm, per frame.
"""
import os
import sys
import time
import warnings
from pathlib import Path

import numpy as np

warnings.filterwarnings("ignore")
here = Path(__file__).resolve().parent
sys.path.insert(0, str(here))

STATES = ["on_foot", "in_vehicle", "menu", "cutscene"]
PROMPT = (
    "This is one frame of a video game (an open-world action game such as GTA V or Watch Dogs). What is the player doing right now?\n"
    "A. On foot: walking, running, standing, climbing, fighting or aiming, outside any vehicle.\n"
    "B. In a vehicle: driving or riding a car, motorcycle, bicycle, boat, helicopter or plane.\n"
    "C. In a menu: a pause menu, map, phone, inventory, shop, settings or loading screen.\n"
    "D. In a cutscene: a scripted cinematic scene.\n"
    "Answer with a single letter."
)
N_PER_STATE = 60


def sample():
    import fuse
    if os.environ.get("VLM_SEQ"):  # every frame of the sequence recordings, for the filter replay in vlm_replay.py
        return fuse, np.flatnonzero(np.isin(fuse.sid, fuse.SEQ))
    rng = np.random.default_rng(0)
    idx = []
    for s in range(4):
        pool = np.flatnonzero(fuse.y == s)
        idx += rng.choice(pool, size=min(N_PER_STATE, len(pool)), replace=False).tolist()
    return fuse, np.sort(np.array(idx))


def option_ids(processor):
    tok = processor.tokenizer if hasattr(processor, "tokenizer") else processor
    ids = []
    for letter in "ABCD":
        cands = {tok.encode(letter, add_special_tokens=False)[-1], tok.encode(" " + letter, add_special_tokens=False)[-1]}
        ids.append(sorted(cands))
    return ids


def run_model(repo, fuse, idx):
    from mlx_vlm import load, stream_generate
    from mlx_vlm.prompt_utils import apply_chat_template
    from mlx_vlm.utils import load_config
    import mlx.core as mx

    path = repo if "/" in repo else f"mlx-community/{repo}"
    t = time.time()
    model, processor = load(path)
    config = load_config(path)
    load_s = time.time() - t
    prompt = apply_chat_template(processor, config, PROMPT, num_images=1) + "Answer:"
    ids = option_ids(processor)
    paths = [str(p) for p in fuse.frame_paths()]
    out, lat = np.zeros((len(idx), 4)), []
    for k, i in enumerate(idx):
        t = time.time()
        first = next(iter(stream_generate(model, processor, prompt, image=[paths[i]], max_tokens=1, temperature=0.0, verbose=False)))
        lp = np.array(first.logprobs.tolist() if hasattr(first.logprobs, "tolist") else first.logprobs)
        out[k] = [np.logaddexp.reduce(lp[c]) for c in ids]
        lat.append(time.time() - t)
        if k == 0:
            print(f"  first answer token {first.text!r}, peak memory {first.peak_memory:.2f} GB", flush=True)
    return out, np.array(lat[1:]), load_s


def report(name, scores, lat, load_s, fuse, idx):
    from sklearn.linear_model import LogisticRegression
    from sklearn.metrics import f1_score, recall_score
    from sklearn.model_selection import GroupKFold, cross_val_predict
    from sklearn.pipeline import make_pipeline
    from sklearn.preprocessing import StandardScaler

    y = fuse.y[idx]
    p = scores - np.logaddexp.reduce(scores, axis=1, keepdims=True)
    pred = p.argmax(1)
    cal = cross_val_predict(make_pipeline(StandardScaler(), LogisticRegression(C=1.0, class_weight="balanced", max_iter=1000)), p, y,
                            groups=fuse.sid[idx], cv=GroupKFold(5))
    rec = lambda t, q: " / ".join(f"{v:.2f}" for v in recall_score(t, q, labels=range(4), average=None, zero_division=0))
    print(f"\n{name}")
    print(f"  zero-shot      acc {(pred == y).mean():.3f}  F1 {f1_score(y, pred, average='macro'):.3f}  recall {rec(y, pred)}")
    print(f"  recalibrated   acc {(cal == y).mean():.3f}  F1 {f1_score(y, cal, average='macro'):.3f}  recall {rec(y, cal)}   (4 numbers fitted, by recording)")
    print(f"  latency per frame (warm) median {np.median(lat) * 1000:.0f} ms, p95 {np.percentile(lat, 95) * 1000:.0f} ms; load {load_s:.1f} s")
    return {"name": name, "zs": (pred == y).mean(), "cal": (cal == y).mean(), "ms": float(np.median(lat) * 1000)}


if __name__ == "__main__":
    fuse, idx = sample()
    y = fuse.y[idx]
    print(f"{len(idx)} frames: {dict(zip(STATES, np.bincount(y).tolist()))}")
    cache = here / "cache"
    pc, _ = fuse.out_of_fold("session", range(len(fuse.SESSIONS)))
    from sklearn.metrics import f1_score
    base = pc[idx].argmax(1)
    print(f"MobileCLIP-S1 probe (frames of its recording unseen): acc {(base == y).mean():.3f}  F1 {f1_score(y, base, average='macro'):.3f}  about 5 ms")
    for repo in sys.argv[1:]:
        f = cache / f"vlm_{repo.replace('/', '_')}{'_seq' if os.environ.get('VLM_SEQ') else ''}.npz"
        if f.exists():
            d = np.load(f)
            scores, lat, load_s = d["scores"], d["lat"], float(d["load_s"])
        else:
            print(f"\nrunning {repo} ...", flush=True)
            try:
                scores, lat, load_s = run_model(repo, fuse, idx)
            except Exception as e:
                print(f"  {repo} failed: {type(e).__name__}: {e}", flush=True)
                continue
            np.savez(f, scores=scores, lat=lat, load_s=load_s)
        report(repo, scores, lat, load_s, fuse, idx)
