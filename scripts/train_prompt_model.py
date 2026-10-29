#!/usr/bin/env python3
"""Train the prompt detector: a small fully convolutional centre-point network (CenterNet style) on the stream's luma plane.

  target/train/venv/bin/python scripts/train_prompt_model.py DATA_DIR OUT_DIR [--epochs N]

Input: 1 x H x W luma (H, W multiples of 8), values /255 - 0.5. Output at stride 4: class heatmap (sigmoid), box size (w, h in cells), and sub-cell
offset. About 0.25 M parameters and about 2 GMAC for a 960x544 plane. Exports OUT_DIR/prompt_net.onnx (fixed 544x960) and prompt_net.pt.
"""
import os, sys, glob, math, time, argparse, random
import numpy as np
import torch, torch.nn as nn, torch.nn.functional as F
from PIL import Image

NC = 16
STRIDE = 4
WIDTH = (20, 40, 80)

def conv(i, o, k=3, s=1, d=1):
    return nn.Sequential(nn.Conv2d(i, o, k, s, padding=d * (k // 2), dilation=d, bias=False), nn.BatchNorm2d(o), nn.ReLU(inplace=True))

class Res(nn.Module):
    def __init__(self, c, d):
        super().__init__()
        self.a, self.b = conv(c, c, 3, 1, d), nn.Sequential(nn.Conv2d(c, c, 3, padding=d, dilation=d, bias=False), nn.BatchNorm2d(c))
    def forward(self, x):
        return F.relu(x + self.b(self.a(x)))

class Net(nn.Module):
    def __init__(self):
        super().__init__()
        a, b, c = WIDTH
        self.s2 = conv(1, a, 3, 2)
        self.s4 = nn.Sequential(conv(a, b, 3, 2), conv(b, b), conv(b, b))
        self.s8 = conv(b, c, 3, 2)
        self.ctx = nn.Sequential(Res(c, 1), Res(c, 2), Res(c, 4), Res(c, 8), Res(c, 1))
        self.up = nn.Conv2d(c, b, 1)
        self.head = nn.Sequential(conv(b, b), conv(b, b))
        self.hm = nn.Conv2d(b, NC, 1)
        self.wh = nn.Conv2d(b, 2, 1)
        self.off = nn.Conv2d(b, 2, 1)
        nn.init.constant_(self.hm.bias, -4.0)  # rare objects: start with low scores

    def forward(self, x):
        f4 = self.s4(self.s2(x))
        f8 = self.ctx(self.s8(f4))
        f = f4 + F.interpolate(self.up(f8), size=f4.shape[-2:], mode="nearest")
        f = self.head(f)
        return torch.sigmoid(self.hm(f)), self.wh(f), self.off(f)

def load_split(root, split):
    imgs, labels = [], []
    for p in sorted(glob.glob(os.path.join(root, split, "images", "*.png"))):
        imgs.append(np.array(Image.open(p).convert("L"), dtype=np.uint8))
        rows = []
        lp = p.replace("images", "labels")[:-4] + ".txt"
        for l in open(lp):
            c, cx, cy, w, h = l.split()
            rows.append((int(c), float(cx), float(cy), float(w), float(h)))
        labels.append(rows)
    return np.stack(imgs), labels

def targets(labels, size):
    """Heatmap (B, NC, S, S), and per-object (b, class, cell y, cell x, w, h, offx, offy) lists."""
    S = size // STRIDE
    B = len(labels)
    hm = np.zeros((B, NC, S, S), dtype=np.float32)
    objs = []
    yy, xx = np.mgrid[0:S, 0:S]
    for b, rows in enumerate(labels):
        for c, cx, cy, w, h in rows:
            px, py, pw, ph = cx * size / STRIDE, cy * size / STRIDE, w * size / STRIDE, h * size / STRIDE
            ix, iy = min(S - 1, int(px)), min(S - 1, int(py))
            sigma = max(0.7, (pw + ph) / 2 / 6)
            g = np.exp(-((xx - ix) ** 2 + (yy - iy) ** 2) / (2 * sigma * sigma))
            hm[b, c] = np.maximum(hm[b, c], g)
            hm[b, c, iy, ix] = 1.0
            objs.append((b, c, iy, ix, pw, ph, px - ix, py - iy))
    return torch.from_numpy(hm), objs

def focal(pred, gt):
    pos = gt.eq(1).float()
    neg = 1 - pos
    pred = pred.clamp(1e-4, 1 - 1e-4)
    pl = torch.log(pred) * (1 - pred) ** 2 * pos
    nl = torch.log(1 - pred) * pred ** 2 * (1 - gt) ** 4 * neg
    n = pos.sum().clamp(min=1)
    return -(pl.sum() + nl.sum()) / n

def loss_fn(out, hm, objs, dev):
    ph, pwh, poff = out
    l_hm = focal(ph, hm)
    if not objs:
        return l_hm, l_hm.detach() * 0, l_hm.detach() * 0
    b = torch.tensor([o[0] for o in objs], device=dev); c = torch.tensor([o[1] for o in objs], device=dev)
    iy = torch.tensor([o[2] for o in objs], device=dev); ix = torch.tensor([o[3] for o in objs], device=dev)
    wh = torch.tensor([[o[4], o[5]] for o in objs], device=dev); off = torch.tensor([[o[6], o[7]] for o in objs], device=dev)
    pw = pwh[b, :, iy, ix]; po = poff[b, :, iy, ix]
    return l_hm, F.l1_loss(pw, wh), F.l1_loss(po, off)

def decode(hm, wh, off, thr=0.3, k=100):
    """List per picture of (class, score, cx, cy, w, h) in input pixels."""
    pooled = F.max_pool2d(hm, 3, 1, 1)
    keep = (pooled == hm).float() * hm
    B, C, H, W = keep.shape
    res = []
    for b in range(B):
        sc, idx = keep[b].reshape(-1).topk(k)
        out = []
        for s, i in zip(sc.tolist(), idx.tolist()):
            if s < thr:
                break
            c, rem = divmod(i, H * W)
            y, x = divmod(rem, W)
            ox, oy = off[b, 0, y, x].item(), off[b, 1, y, x].item()
            w, h = wh[b, 0, y, x].item(), wh[b, 1, y, x].item()
            out.append((c, s, (x + ox) * STRIDE, (y + oy) * STRIDE, w * STRIDE, h * STRIDE))
        res.append(out)
    return res

def iou(a, b):
    ax0, ay0, ax1, ay1 = a[2] - a[4] / 2, a[3] - a[5] / 2, a[2] + a[4] / 2, a[3] + a[5] / 2
    bx0, by0, bx1, by1 = b[2] - b[4] / 2, b[3] - b[5] / 2, b[2] + b[4] / 2, b[3] + b[5] / 2
    iw, ih = max(0, min(ax1, bx1) - max(ax0, bx0)), max(0, min(ay1, by1) - max(ay0, by0))
    inter = iw * ih
    return inter / (a[4] * a[5] + b[4] * b[5] - inter + 1e-9)

def evaluate(model, dev, X, labels, thr=0.4, bs=32):
    model.eval()
    tp = np.zeros(NC); fp = np.zeros(NC); fn = np.zeros(NC)
    size = X.shape[-1]
    with torch.no_grad():
        for s in range(0, len(X), bs):
            x = torch.from_numpy(X[s:s + bs]).float().div(255).sub(0.5).unsqueeze(1).to(dev)
            hm, wh, off = (t.cpu() for t in model(x))
            for rows, dets in zip(labels[s:s + bs], decode(hm, wh, off, thr)):
                gts = [(c, 1, cx * size, cy * size, w * size, h * size) for c, cx, cy, w, h in rows]
                used = set()
                for d in dets:
                    best, bj = 0, -1
                    for j, g in enumerate(gts):
                        if g[0] == d[0] and j not in used:
                            v = iou(d, g)
                            if v > best:
                                best, bj = v, j
                    if best >= 0.5:
                        tp[d[0]] += 1; used.add(bj)
                    else:
                        fp[d[0]] += 1
                for j, g in enumerate(gts):
                    if j not in used:
                        fn[g[0]] += 1
    model.train()
    return tp, fp, fn

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("data"); ap.add_argument("out"); ap.add_argument("--epochs", type=int, default=24); ap.add_argument("--bs", type=int, default=16)
    a = ap.parse_args()
    os.makedirs(a.out, exist_ok=True)
    torch.manual_seed(1); random.seed(1); np.random.seed(1)
    dev = torch.device("mps" if torch.backends.mps.is_available() else "cpu")
    print("device", dev, flush=True)
    Xtr, Ltr = load_split(a.data, "train"); Xva, Lva = load_split(a.data, "val")
    print("loaded", Xtr.shape, Xva.shape, flush=True)
    model = Net().to(dev)
    print("params", sum(p.numel() for p in model.parameters()), flush=True)
    opt = torch.optim.AdamW(model.parameters(), lr=2e-3, weight_decay=1e-4)
    steps = a.epochs * math.ceil(len(Xtr) / a.bs)
    sched = torch.optim.lr_scheduler.OneCycleLR(opt, max_lr=2e-3, total_steps=steps, pct_start=0.1)
    size = Xtr.shape[-1]
    t0 = time.time()
    for ep in range(a.epochs):
        perm = np.random.permutation(len(Xtr))
        tot = np.zeros(3); n = 0
        for s in range(0, len(perm), a.bs):
            ids = perm[s:s + a.bs]
            x = torch.from_numpy(Xtr[ids]).float().div(255)
            # picture-wide gain and bias, as streams differ
            g = torch.empty(len(ids), 1, 1).uniform_(0.7, 1.2); bia = torch.empty(len(ids), 1, 1).uniform_(-0.08, 0.08)
            x = (x * g + bia).clamp(0, 1).sub(0.5).unsqueeze(1).to(dev)
            hm, objs = targets([Ltr[i] for i in ids], size)
            out = model(x)
            lh, lw, lo = loss_fn(out, hm.to(dev), objs, dev)
            loss = lh + 0.1 * lw + lo
            opt.zero_grad(); loss.backward(); opt.step(); sched.step()
            tot += [lh.item(), lw.item(), lo.item()]; n += 1
        tp, fp, fn = evaluate(model, dev, Xva, Lva)
        P, R = tp.sum() / max(1, tp.sum() + fp.sum()), tp.sum() / max(1, tp.sum() + fn.sum())
        print(f"epoch {ep + 1}/{a.epochs} loss hm {tot[0] / n:.3f} wh {tot[1] / n:.3f} off {tot[2] / n:.3f} | val precision {P:.3f} recall {R:.3f} | {time.time() - t0:.0f}s", flush=True)
        torch.save(model.state_dict(), os.path.join(a.out, "prompt_net.pt"))
    tp, fp, fn = evaluate(model, dev, Xva, Lva)
    names = open(os.path.join(a.data, "classes.txt")).read().split()
    for c in range(NC):
        print(f"  {names[c]:9s} tp {int(tp[c]):4d} fp {int(fp[c]):4d} fn {int(fn[c]):4d}")
    model.cpu().eval()
    torch.onnx.export(model, torch.zeros(1, 1, 544, 960), os.path.join(a.out, "prompt_net.onnx"), input_names=["luma"], output_names=["hm", "wh", "off"], opset_version=17, dynamo=False)
    print("exported", flush=True)

if __name__ == "__main__":
    main()
