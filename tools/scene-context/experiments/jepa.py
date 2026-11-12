#!/usr/bin/env python3
"""Self-supervised image backbones (I-JEPA, V-JEPA 2, DINOv2, DINOv3) and SigLIP 2 against MobileCLIP-S1, on the same frames and probe.

  .venv-vlm/bin/python experiments/jepa.py [NAME ...]        (no names: all; see MODELS)

"center" is the middle square the app uses; "full" is the whole 16:9 frame at its own aspect (the ViTs interpolate their position codes).
V-JEPA 2 is a video model, so it is given the frame twice.
"""
import sys
import time
import warnings
from pathlib import Path

import numpy as np
import torch
from PIL import Image

import fuse
import probe_eval

warnings.filterwarnings("ignore")
cache = Path(__file__).resolve().parent / "cache"
dev = "mps" if torch.backends.mps.is_available() else "cpu"
paths = fuse.frame_paths()
IMAGENET = ((0.485, 0.456, 0.406), (0.229, 0.224, 0.225))


def load_hf(cls_name, repo, **kw):
    import transformers
    return getattr(transformers, cls_name).from_pretrained(repo, **kw).to(dev).eval()


def dinov2(repo):
    m = load_hf("Dinov2Model", repo)

    def f(x):
        h = m(pixel_values=x).last_hidden_state
        return torch.cat([h[:, 0], h[:, 1:].mean(1)], 1)
    return f, IMAGENET, 14


def ijepa(repo):
    m = load_hf("IJepaModel", repo, add_pooling_layer=False)
    return (lambda x: m(pixel_values=x, interpolate_pos_encoding=True).last_hidden_state.mean(1)), IMAGENET, 14


def vjepa2(repo):
    m = load_hf("VJEPA2Model", repo)
    return (lambda x: m(pixel_values_videos=x[:, None].repeat(1, 2, 1, 1, 1)).last_hidden_state.mean(1)), IMAGENET, 16


def dinov3(name):
    import timm
    m = timm.create_model(name, pretrained=True, num_classes=0, dynamic_img_size=True).to(dev).eval()

    def f(x):
        h = m.forward_features(x)
        return torch.cat([h[:, 0], h[:, m.num_prefix_tokens:].mean(1)], 1)
    return f, IMAGENET, 16


def siglip2(repo):
    m = load_hf("SiglipVisionModel", repo)
    return (lambda x: m(pixel_values=x).pooler_output), ((0.5,) * 3, (0.5,) * 3), 16


MODELS = {
    "DINOv2-S/14": lambda: dinov2("facebook/dinov2-small"),
    "DINOv2-B/14": lambda: dinov2("facebook/dinov2-base"),
    "DINOv3-S/16": lambda: dinov3("vit_small_patch16_dinov3.lvd1689m"),
    "I-JEPA H/14": lambda: ijepa("facebook/ijepa_vith14_1k"),
    "V-JEPA2 L/16": lambda: vjepa2("facebook/vjepa2-vitl-fpc64-256"),
    "SigLIP2-B/16": lambda: siglip2("google/siglip2-base-patch16-224"),
}


def picture(im, view, patch, short=224):
    w, h = im.size
    if view == "center":
        s = min(w, h)
        im = im.crop(((w - s) // 2, 0, (w - s) // 2 + s, s)).resize((short, short), Image.BILINEAR)
    else:
        W = round(short * w / h / patch) * patch
        im = im.resize((W, short), Image.BILINEAR)
    return torch.from_numpy(np.asarray(im)).permute(2, 0, 1)


def embed(name, make, view, bs=16):
    f = cache / f"jepa_{name.replace('/', '_')}_{view}.npy"
    if f.exists():
        return np.load(f), None
    fn, (mean, std), patch = make()
    mean, std = (torch.tensor(v, device=dev).view(1, 3, 1, 1) for v in (mean, std))
    short = 256 if name.startswith("V-JEPA") else 224
    out, ms = [], None
    for i in range(0, len(paths), bs):
        x = torch.stack([picture(Image.open(p).convert("RGB"), view, patch, short) for p in paths[i:i + bs]]).float().to(dev) / 255
        with torch.no_grad():
            out.append(fn((x - mean) / std).float().cpu().numpy())
        if i == bs:
            one = x[:1]
            with torch.no_grad():
                for _ in range(3):
                    fn((one - mean) / std)
                torch.mps.synchronize() if dev == "mps" else None
                t = time.perf_counter()
                for _ in range(10):
                    fn((one - mean) / std)
                torch.mps.synchronize() if dev == "mps" else None
            ms = (time.perf_counter() - t) / 10 * 1000
    out = np.concatenate(out)
    np.save(f, out)
    return out, ms


if __name__ == "__main__":
    names = sys.argv[1:] or list(MODELS)
    rows, speeds = [(("MobileCLIP-S1 center (shipped)"), fuse.x)], []
    for name in names:
        for view in ["center", "full"]:
            if name.startswith("V-JEPA") and view == "full":
                continue
            try:
                x, ms = embed(name, MODELS[name], view)
            except Exception as e:
                print(f"{name} {view} failed: {type(e).__name__}: {e}", flush=True)
                continue
            rows.append((f"{name} {view}", x))
            if ms:
                speeds.append(f"{name} {view}: {ms:.0f} ms/frame at batch 1")
    print("\n".join(speeds))
    probe_eval.report(rows)
