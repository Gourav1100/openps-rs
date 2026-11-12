#!/usr/bin/env python3
"""Other heads on the same frozen embeddings, scored the way the shipped one is: every recording read by a head fitted on all the others, with the
zero-shot sentences mixed in as the app does, then the filter replayed with simulated presses.

  .venv/bin/python experiments/heads.py [--only NAME ...]

Heads: the shipped logistic regression, linear SVMs (calibrated), and a small neural network (one hidden layer, dropout, label smoothing). Only the
logistic regression and a linear SVM would run in the app as it is (one linear map); the network would need its layers in crates/openps-scene.
"""
import argparse
import sys
import time
import warnings
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
warnings.filterwarnings("ignore")

import numpy as np
from sklearn.calibration import CalibratedClassifierCV
from sklearn.metrics import f1_score
from sklearn.pipeline import make_pipeline
from sklearn.preprocessing import StandardScaler
from sklearn.svm import LinearSVC
from sklearn.utils.class_weight import compute_class_weight

from scenelab import data as scene_data
from scenelab import evaluate, features, filtersim, probe
from scenelab.pictures import HEAD_C, ZERO_SHOT_WEIGHT


class MLP:
    """A one-hidden-layer network on standardized embeddings: dropout on the input and the hidden layer, weight decay, label smoothing, balanced
    classes. Fitted in a couple of seconds (CPU)."""

    def __init__(self, hidden=256, dropout=0.3, epochs=40, lr=1e-3, weight_decay=1e-2, smoothing=0.1, seed=0):
        self.hidden, self.dropout, self.epochs, self.lr = hidden, dropout, epochs, lr
        self.weight_decay, self.smoothing, self.seed = weight_decay, smoothing, seed

    def fit(self, X, y):
        import torch
        from torch import nn

        torch.manual_seed(self.seed)
        self.scaler = StandardScaler().fit(X)
        self.classes_ = np.unique(y)
        Xt = torch.tensor(self.scaler.transform(X), dtype=torch.float32)
        yt = torch.tensor(np.searchsorted(self.classes_, y))
        w = torch.tensor(compute_class_weight("balanced", classes=self.classes_, y=y), dtype=torch.float32)
        self.net = nn.Sequential(nn.Dropout(self.dropout), nn.Linear(X.shape[1], self.hidden), nn.ReLU(), nn.Dropout(self.dropout),
                                 nn.Linear(self.hidden, len(self.classes_)))
        opt = torch.optim.AdamW(self.net.parameters(), lr=self.lr, weight_decay=self.weight_decay)
        loss = nn.CrossEntropyLoss(weight=w, label_smoothing=self.smoothing)
        g = torch.Generator().manual_seed(self.seed)
        self.net.train()
        for _ in range(self.epochs):
            for b in torch.randperm(len(Xt), generator=g).split(256):
                opt.zero_grad()
                loss(self.net(Xt[b]), yt[b]).backward()
                opt.step()
        return self

    def predict_proba(self, X):
        import torch

        self.net.eval()
        with torch.no_grad():
            return torch.softmax(self.net(torch.tensor(self.scaler.transform(X), dtype=torch.float32)), 1).numpy()


def svm(c):
    return lambda: make_pipeline(StandardScaler(), CalibratedClassifierCV(
        LinearSVC(C=c, class_weight="balanced", dual=False, max_iter=2000), cv=3, method="sigmoid"))


HEADS = {
    "logistic regression C=0.1 (shipped)": lambda: evaluate._head(HEAD_C),
    "logistic regression C=0.03": lambda: evaluate._head(0.03),
    "linear SVM C=0.001": svm(0.001),
    "linear SVM C=0.003": svm(0.003),
    "linear SVM C=0.01": svm(0.01),
    "neural net 256 hidden, dropout 0.3": lambda: MLP(256, 0.3),
    "neural net 64 hidden, dropout 0.5": lambda: MLP(64, 0.5),
}


def out_of_recording(make, x, d):
    """Probabilities of the four scenes for every frame from a head that never saw its recording."""
    p = np.zeros((len(d), 4))
    for s in range(len(d.names)):
        te = d.sid == s
        p[te] = evaluate._proba(make().fit(x[~te], d.y[~te]), x[te], 4)
    return p


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--only", nargs="*", help="names (or parts of names) of the heads to run")
    a = ap.parse_args()
    d = scene_data.load()
    x = features.cached()
    zs5 = probe.zero_shot(x, evaluate.zero_shot_text())
    # the view head is the shipped one for every row: only the scene head differs
    pv = np.full((len(d), 2), 0.5)
    has = d.view >= 0
    for s in range(len(d.names)):
        te, trv, tev = d.sid == s, (d.sid != s) & has, (d.sid == s) & has
        if tev.any():
            pv[tev] = evaluate._proba(evaluate._head(HEAD_C).fit(x[trv], d.view[trv]), x[tev], 2)
    orig = np.isin(d.sid, [i for i, n in enumerate(d.names) if "order" in scene_data.read_recordings().get(n, {})])
    print(f"{len(d)} frames in {len(d.names)} recordings (the original ones: {orig.sum()} frames); recordings unseen, zero-shot weight {ZERO_SHOT_WEIGHT}\n")
    head = f"{'head':38}{'alone':>7}{'+ zero-shot':>12}{'F1':>7}{'orig.':>7}{'other':>7} | {'sequences, filter with presses: acc':>38}{'F1':>7}  recall"
    print(head)
    for name, make in HEADS.items():
        if a.only and not any(o.lower() in name.lower() for o in a.only):
            continue
        started = time.time()
        p = out_of_recording(make, x, d)
        pc = probe.mix(p, zs5, ZERO_SHOT_WEIGHT)
        pred = pc.argmax(1)
        res = filtersim.evaluate((pc, pv), seeds=10)["keys"]
        recall = filtersim.rec(np.concatenate(res["t"]), np.concatenate(res["p"]))
        print(f"{name:38}{(p.argmax(1) == d.y).mean():7.3f}{(pred == d.y).mean():12.3f}{f1_score(d.y, pred, average='macro'):7.3f}"
              f"{(pred[orig] == d.y[orig]).mean():7.3f}{(pred[~orig] == d.y[~orig]).mean():7.3f} | {np.mean(res['acc']):38.3f}{np.mean(res['f1']):7.3f}  {recall}"
              f"  [{time.time() - started:.0f} s]", flush=True)


if __name__ == "__main__":
    main()
