"""Shared scoring for embeddings: the same probe and the same two splits as compare.py (whole recording unseen; held-out blocks)."""
import numpy as np
from sklearn.linear_model import LogisticRegression
from sklearn.metrics import f1_score, recall_score
from sklearn.pipeline import make_pipeline
from sklearn.preprocessing import StandardScaler

import fuse

n = len(fuse.y)


def probe():
    return make_pipeline(StandardScaler(), LogisticRegression(max_iter=2000, C=0.5, class_weight="balanced"))


def folds(mode):
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


def score(x, mode):
    pred, vpred = np.zeros(n, int), np.full(n, -1)
    for tr, te in folds(mode):
        pred[te] = probe().fit(x[tr], fuse.y[tr]).predict(x[te])
        trv, tev = tr & (fuse.view >= 0), te & (fuse.view >= 0)
        if tev.any():
            vpred[tev] = probe().fit(x[trv], fuse.view[trv]).predict(x[tev])
    has = fuse.view >= 0
    rec = recall_score(fuse.y, pred, labels=range(4), average=None)
    return (pred == fuse.y).mean(), f1_score(fuse.y, pred, average="macro"), (vpred[has] == fuse.view[has]).mean(), rec


def report(rows):
    """rows: [(name, embedding)]"""
    for mode, title in [("set", "A whole recording unseen"), ("block", "Held-out blocks of frames")]:
        print(f"\n{title}\n  {'model / picture':44}{'acc':>6}{'F1':>6}{'view':>6}   recall on_foot / vehicle / menu / cutscene")
        for name, x in rows:
            a, f1, v, rec = score(x, mode)
            print(f"  {name:44}{a:6.3f}{f1:6.3f}{v:6.3f}   " + " / ".join(f"{r:.2f}" for r in rec), flush=True)
