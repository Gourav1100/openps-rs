#!/usr/bin/env python3
"""Does a VLM add to MobileCLIP-S1? Stacks the cached VLM letter scores (vlm.py) with the probe's out-of-fold probabilities.

  .venv-vlm/bin/python experiments/vlm_stack.py MODEL [MODEL ...]
"""
import sys
import warnings
from pathlib import Path

import numpy as np
from sklearn.linear_model import LogisticRegression
from sklearn.metrics import f1_score, recall_score
from sklearn.model_selection import GroupKFold, cross_val_predict
from sklearn.pipeline import make_pipeline
from sklearn.preprocessing import StandardScaler

warnings.filterwarnings("ignore")
here = Path(__file__).resolve().parent
sys.path.insert(0, str(here))
import vlm  # noqa: E402

fuse, idx = vlm.sample()
y, groups = fuse.y[idx], fuse.sid[idx]
pc, _ = fuse.out_of_fold("session", range(len(fuse.SESSIONS)))
mobile = np.log(np.clip(pc[idx], 1e-6, 1))


def norm(s):
    return s - np.logaddexp.reduce(s, axis=1, keepdims=True)


def cv(x):
    clf = make_pipeline(StandardScaler(), LogisticRegression(C=1.0, class_weight="balanced", max_iter=1000))
    return cross_val_predict(clf, x, y, groups=groups, cv=GroupKFold(5))


def line(name, pred):
    rec = " / ".join(f"{v:.2f}" for v in recall_score(y, pred, labels=range(4), average=None))
    print(f"{name:42s} acc {(pred == y).mean():.3f}  F1 {f1_score(y, pred, average='macro'):.3f}  recall {rec}")


line("MobileCLIP-S1 probe alone", mobile.argmax(1))
line("MobileCLIP-S1 probe, recalibrated", cv(mobile))
for repo in sys.argv[1:]:
    d = np.load(here / "cache" / f"vlm_{repo}.npz")
    v = norm(d["scores"])
    line(f"{repo} recalibrated", cv(v))
    line(f"{repo} + MobileCLIP-S1", cv(np.hstack([v, mobile])))
