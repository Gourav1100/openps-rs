#!/usr/bin/env python3
"""Replays the filter on sequences read by a VLM instead of the MobileCLIP-S1 probe (needs vlm.py run with VLM_SEQ=1).

  .venv-vlm/bin/python experiments/vlm_replay.py MODEL [MODEL ...]

The VLM's four letter scores are recalibrated by a logistic regression fitted on the other recordings (leave one recording out), the same
treatment the probe gets. The filter and the simulated key presses are those of fuse.py.
"""
import sys
import warnings
from pathlib import Path

import numpy as np
from sklearn.linear_model import LogisticRegression
from sklearn.pipeline import make_pipeline
from sklearn.preprocessing import StandardScaler

warnings.filterwarnings("ignore")
here = Path(__file__).resolve().parent
sys.path.insert(0, str(here))
import fuse  # noqa: E402

seq = np.flatnonzero(np.isin(fuse.sid, fuse.SEQ))
true_sw = sum(fuse.switches(fuse.y[fuse.sid == s]) for s in fuse.SEQ) / len(seq) * 100


def calibrated(scores):
    s = scores - np.logaddexp.reduce(scores, axis=1, keepdims=True)
    pc = np.full((fuse.n, 4), 0.25)
    for r in fuse.SEQ:
        te = fuse.sid[seq] == r
        clf = make_pipeline(StandardScaler(), LogisticRegression(C=1.0, class_weight="balanced", max_iter=1000))
        clf.fit(s[~te], fuse.y[seq][~te])
        p = np.zeros((te.sum(), 4))
        p[:, clf.classes_] = clf.predict_proba(s[te])
        pc[seq[te]] = p
    return pc


def mix(*ps):
    return np.mean(ps, axis=0)


pc_m, pv = fuse.out_of_fold("session", fuse.SEQ)
print(f"{len(seq)} frames in {len(fuse.SEQ)} recordings; true context changes per 100 frames: {true_sw:.1f}")
fuse.show("MobileCLIP-S1 probe (the shipped model)", fuse.evaluate((pc_m, pv)), true_sw)
for repo in sys.argv[1:]:
    d = np.load(here / "cache" / f"vlm_{repo}_seq.npz")
    pc_v = calibrated(d["scores"])
    fuse.show(f"{repo}, recalibrated", fuse.evaluate((pc_v, pv)), true_sw)
    fuse.show(f"{repo} and MobileCLIP-S1, averaged", fuse.evaluate((mix(pc_v, pc_m), pv)), true_sw)
