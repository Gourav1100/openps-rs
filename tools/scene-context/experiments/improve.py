#!/usr/bin/env python3
"""Candidate scene models end to end on cached embeddings: unseen-recording probe + zero-shot mix, then the filter with simulated presses.

  .venv-vlm/bin/python scene.py eval [NAME ...]

Every row is a complete deployable design (which pictures, which backbones, which head); numbers are for recordings the head never saw.
Run views.py and jepa.py first to fill cache.
"""
import sys
import warnings
from pathlib import Path

import numpy as np
from sklearn.linear_model import LogisticRegression
from sklearn.metrics import f1_score, recall_score
from sklearn.neural_network import MLPClassifier
from sklearn.pipeline import make_pipeline
from sklearn.preprocessing import StandardScaler

import fuse

warnings.filterwarnings("ignore")
c = Path(__file__).resolve().parent / "cache"
ld = lambda n: np.load(c / f"{n}.npy")
n = fuse.n
TEXT = fuse.text  # MobileCLIP-S1 text tower, 5 prompts


def lr(C):
    return lambda: make_pipeline(StandardScaler(), LogisticRegression(max_iter=3000, C=C, class_weight="balanced"))


def mlp(alpha=1.0):
    return lambda: make_pipeline(StandardScaler(), MLPClassifier((256,), alpha=alpha, early_stopping=True, max_iter=300, random_state=0))


def proba(clf, xs, k):
    p = np.zeros((len(xs), k))
    p[:, clf.classes_] = clf.predict_proba(xs)
    return p


def oof(x, zs_emb, head, w_zs=0.5):
    """Out-of-recording probabilities: the head, averaged with zero-shot from the MobileCLIP picture `zs_emb`."""
    zs5 = fuse.softmax(100 * zs_emb @ TEXT.T)
    pc, pv = np.zeros((n, 4)), np.full((n, 2), 0.5)
    has = fuse.view >= 0
    for s in range(len(fuse.SESSIONS)):
        te = fuse.sid == s
        p = proba(head().fit(x[~te], fuse.y[~te]), x[te], 4)
        z = zs5[te]
        share = p[:, fuse.MENU] / np.maximum(p[:, fuse.MENU] + p[:, fuse.CUT], 1e-9)
        zs4 = np.stack([z[:, 0], z[:, 1:4].sum(1), z[:, 4] * share, z[:, 4] * (1 - share)], 1)
        pc[te] = w_zs * zs4 + (1 - w_zs) * p
        trv, tev = ~te & has, te & has
        if tev.any():
            pv[tev] = proba(head().fit(x[trv], fuse.view[trv]), x[tev], 2)
    return pc, pv


def deployed_oof():
    """Out-of-recording probabilities of the shipped design, on the Core ML features export_model.py kept."""
    x = np.load(c / "deployed_features.npy")
    return oof(x, x[:, :512], lr(0.1), 0.25)


def row(name, pc, pv, replay=True):
    pred = pc.argmax(1)
    has = fuse.view >= 0
    rec = recall_score(fuse.y, pred, labels=range(4), average=None)
    s = (f"  {name:58}{(pred == fuse.y).mean():6.3f}{f1_score(fuse.y, pred, average='macro'):6.3f}"
         f"{(pv.argmax(1)[has] == fuse.view[has]).mean():6.3f}   " + "/".join(f"{r:.2f}" for r in rec))
    if replay:
        k = fuse.evaluate((pc, pv), seeds=10)["keys"]
        s += f"   {np.mean(k['acc']):6.3f}{np.mean(k['f1']):6.3f}{np.mean(k['view']):6.3f}"
    print(s, flush=True)


if __name__ == "__main__":
    mc_c, mc_s = fuse.x, ld("views_squash")
    db_f, db_c = ld("jepa_DINOv2-B_14_full"), ld("jepa_DINOv2-B_14_center")
    sg = ld("jepa_SigLIP2-B_16_center")
    vj = ld("jepa_V-JEPA2 L_16_center")
    hud = np.hstack([ld("views_hud_bl"), ld("views_hud_br"), ld("views_hud_tr")]) if (c / "views_hud_bl.npy").exists() else None
    cat = lambda *a: np.hstack(a)
    designs = {
        "shipped: MobileCLIP middle square, LR C=0.5": (mc_c, mc_c, lr(0.5)),
        "A  MobileCLIP whole frame, LR C=0.5": (mc_s, mc_s, lr(0.5)),
        "B  A + DINOv2-B whole frame, LR C=0.5": (cat(mc_s, db_f), mc_s, lr(0.5)),
        "B  ... LR C=0.1": (cat(mc_s, db_f), mc_s, lr(0.1)),
        "B  ... LR C=0.02": (cat(mc_s, db_f), mc_s, lr(0.02)),
        "B  ... MLP": (cat(mc_s, db_f), mc_s, mlp()),
        "C  A + DINOv2-B + SigLIP2-B, LR C=0.1": (cat(mc_s, db_f, sg), mc_s, lr(0.1)),
        "D  A + V-JEPA2 L, LR C=0.1": (cat(mc_s, vj), mc_s, lr(0.1)),
        "E  A + DINOv2-B + V-JEPA2 L, LR C=0.1": (cat(mc_s, db_f, vj), mc_s, lr(0.1)),
        "F  A + middle-square MobileCLIP + DINOv2-B, C=0.1": (cat(mc_s, mc_c, db_f), mc_s, lr(0.1)),
    }
    if hud is not None:
        designs["G  A + HUD corners, LR C=0.1"] = (cat(mc_s, hud), mc_s, lr(0.1))
        designs["H  B + HUD corners, LR C=0.1"] = (cat(mc_s, db_f, hud), mc_s, lr(0.1))
        designs["I  H + middle square MobileCLIP, C=0.1"] = (cat(mc_s, mc_c, db_f, hud), mc_s, lr(0.1))
        designs["I  ... C=0.03"] = (cat(mc_s, mc_c, db_f, hud), mc_s, lr(0.03))
        designs["J  I + SigLIP2-B, C=0.03"] = (cat(mc_s, mc_c, db_f, hud, sg), mc_s, lr(0.03))
    weights = {"zero-shot weight 0": 0.0, "zero-shot weight 0.25": 0.25}
    for base in ["B  A + DINOv2-B whole frame, LR C=0.5", "H  B + HUD corners, LR C=0.1", "I  H + middle square MobileCLIP, C=0.1"]:
        if base in designs:
            for tag, w in weights.items():
                designs[f"{base[:1]}  ... {tag}"] = designs[base] + (w,)
    for C_ in (0.5, 0.1, 0.03):
        designs[f"N  whole + DINOv2-B whole, C={C_}, zs 0.25"] = (cat(mc_s, db_f), mc_s, lr(C_), 0.25)
        designs[f"N  whole + middle + DINOv2-B whole, C={C_}, zs 0.25"] = (cat(mc_s, mc_c, db_f), mc_s, lr(C_), 0.25)
    designs["N  whole + middle + DINOv2-B whole, C=0.1, zs 0.35"] = (cat(mc_s, mc_c, db_f), mc_s, lr(0.1), 0.35)
    designs["K  shipped picture, zero-shot weight 0"] = (mc_c, mc_c, lr(0.5), 0.0)
    designs["K  shipped picture, zero-shot weight 0.25"] = (mc_c, mc_c, lr(0.5), 0.25)
    designs["K  whole frame MobileCLIP, zero-shot weight 0.25"] = (mc_s, mc_s, lr(0.5), 0.25)
    if hud is not None:
        designs["L  I, C=0.5, zero-shot weight 0.25"] = (cat(mc_s, mc_c, db_f, hud), mc_s, lr(0.5), 0.25)
        designs["L  I, C=0.02, zero-shot weight 0.25"] = (cat(mc_s, mc_c, db_f, hud), mc_s, lr(0.02), 0.25)
        designs["L  MobileCLIP only: whole + middle + HUD, C=0.1, zs 0.25"] = (cat(mc_s, mc_c, hud), mc_s, lr(0.1), 0.25)
        designs["L  MobileCLIP only: whole + HUD, C=0.1, zs 0.25"] = (cat(mc_s, hud), mc_s, lr(0.1), 0.25)
        designs["L  I, C=0.1, zero-shot weight 0.25"] = (cat(mc_s, mc_c, db_f, hud), mc_s, lr(0.1), 0.25)
        bars = ld("bars")
        designs["M  I + letterbox bands"] = (cat(mc_s, mc_c, db_f, hud, bars), mc_s, lr(0.1), 0.25)
        designs["M  I + letterbox bands x20"] = (cat(mc_s, mc_c, db_f, hud, np.tile(bars, 20)), mc_s, lr(0.1), 0.25)
    if (c / "deployed_features.npy").exists():
        x = ld("deployed_features")
        designs["SHIPPED  Core ML features: whole + DINOv2-B whole, C=0.1, zs 0.25"] = (x, x[:, :512], lr(0.1), 0.25)
    names = sys.argv[1:]
    print(f"  {'design':58}{'acc':>6}{'F1':>6}{'view':>6}   recall foot/veh/menu/cut   | with filter+keys: acc F1 view")
    for name, spec in designs.items():
        x, zs, head, *w = spec
        if names and not any(name.startswith(k) for k in names):
            continue
        row(name, *oof(x, zs, head, *w))
