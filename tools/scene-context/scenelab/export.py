"""Training: the two Core ML image models and the probe the app bundles (assets/models).

`train()` is what `train.sh` runs. The slow part, converting MobileCLIP-S1 and DINOv2-B to Core ML (a few minutes, PyTorch needed), only happens
when the models are missing, the pictures or sentences changed, or `reexport` is asked for; otherwise training is: features for the frames that
are new (about 30 ms each), a logistic-regression fit (seconds), the probe file, and the files the Rust tests compare against.
"""
import json
import shutil
import warnings
from pathlib import Path

from . import data as scene_data
from . import features
from .paths import MODELS
from .pictures import BACKBONE_SIZE, HEAD_C, LAYOUT, VERSION, ZERO_SHOT_WEIGHT
from .prompts import PROMPTS, VIEW_PROMPTS, fingerprint

warnings.filterwarnings("ignore")
ARCH, PRETRAINED, CLIP_SIZE = "MobileCLIP-S1", "datacompdr", 256
ENCODER, BACKBONE, PROBE, STAMP = "scene-encoder.mlpackage", "scene-backbone.mlpackage", "scene-probe.json", "models.json"


# ---- the Core ML models (slow: PyTorch) -------------------------------------------------------------------------------

def _convert(traced, shape, out, name, scale=1 / 255.0):
    import coremltools as ct

    ml = ct.convert(
        traced,
        inputs=[ct.ImageType(name="image", shape=shape, scale=scale, color_layout=ct.colorlayout.RGB)],
        outputs=[ct.TensorType(name="embedding")],
        convert_to="mlprogram",
        compute_precision=ct.precision.FLOAT16,
        minimum_deployment_target=ct.target.macOS13,  # the app runs on macOS 13 (scripts/install.sh)
    )
    pkg = out / name
    shutil.rmtree(pkg, ignore_errors=True)
    ml.save(str(pkg))
    return ml


def _clip():
    """MobileCLIP-S1 (reparameterised for inference) and its text tower's tokenizer."""
    import open_clip
    from timm.utils.model import reparameterize_model

    model, _, _ = open_clip.create_model_and_transforms(ARCH, pretrained=PRETRAINED)
    return reparameterize_model(model.eval()), open_clip.get_tokenizer(ARCH)


def _text_embeddings(model, tokenizer):
    import torch

    def enc(texts):
        with torch.no_grad():
            t = model.encode_text(tokenizer(texts))
            return (t / t.norm(dim=-1, keepdim=True)).numpy()

    classes = ["on_foot", "land_vehicle", "air_vehicle", "water_vehicle", "menu_or_cutscene"]
    return enc([PROMPTS[c] for c in classes]), enc(VIEW_PROMPTS)


def export_models(out):
    """Convert both models into `out`; returns the zero-shot sentence embeddings (class, view) made by the same MobileCLIP."""
    import torch

    model, tokenizer = _clip()

    class Encoder(torch.nn.Module):
        def __init__(self):
            super().__init__()
            self.m = model  # a submodule, so tracing sees its weights as parameters

        def forward(self, x):  # 0..1, no mean/std (MobileCLIP uses none)
            f = self.m.encode_image(x)
            return f / f.norm(dim=-1, keepdim=True)

    with torch.no_grad():
        traced = torch.jit.trace(Encoder().eval(), torch.rand(1, 3, CLIP_SIZE, CLIP_SIZE))
    _convert(traced, (1, 3, CLIP_SIZE, CLIP_SIZE), out, ENCODER)
    _export_backbone(out)
    (out / STAMP).write_text(json.dumps({"pictures": VERSION, "prompts": fingerprint()}))
    return _text_embeddings(model, tokenizer)


def _export_backbone(out):
    """DINOv2-B/14 on the whole frame (392 x 224): [CLS, mean of the patch tokens], 1536 numbers."""
    import torch
    from transformers import Dinov2Model

    width, height = BACKBONE_SIZE

    class Backbone(torch.nn.Module):
        def __init__(self):
            super().__init__()
            self.m = Dinov2Model.from_pretrained("facebook/dinov2-base").eval()
            with torch.no_grad():  # bicubic resizing of the position codes cannot be converted: do it once here
                tokens = torch.zeros(1, 1 + (height // 14) * (width // 14), self.m.config.hidden_size)
                fixed = self.m.embeddings.interpolate_pos_encoding(tokens, height, width).clone()
            self.m.embeddings.interpolate_pos_encoding = lambda embeddings, h, w: fixed
            self.register_buffer("mean", torch.tensor([0.485, 0.456, 0.406]).view(1, 3, 1, 1))
            self.register_buffer("std", torch.tensor([0.229, 0.224, 0.225]).view(1, 3, 1, 1))

        def forward(self, x):  # 0..1
            h = self.m(pixel_values=(x - self.mean) / self.std).last_hidden_state
            return torch.cat([h[:, 0], h[:, 1:].mean(1)], 1)

    with torch.no_grad():
        traced = torch.jit.trace(Backbone().eval(), torch.rand(1, 3, height, width), strict=False)
    _convert(traced, (1, 3, height, width), out, BACKBONE)


def models_current(out):
    """Both packages are there and were made for these pictures and sentences."""
    stamp = out / STAMP
    if not ((out / ENCODER).exists() and (out / BACKBONE).exists() and stamp.exists()):
        return False
    s = json.loads(stamp.read_text())
    return s.get("pictures") == VERSION and s.get("prompts") == fingerprint()


# ---- the probe ------------------------------------------------------------------------------------------------------------

def fit_probe(text5, vtext, feats, c=HEAD_C, zero_shot_weight=ZERO_SHOT_WEIGHT):
    from sklearn.linear_model import LogisticRegression
    from sklearn.pipeline import make_pipeline
    from sklearn.preprocessing import StandardScaler

    data = scene_data.load()

    def fit(x, y):
        pipe = make_pipeline(StandardScaler(), LogisticRegression(max_iter=3000, C=c, class_weight="balanced")).fit(x, y)
        sc, lr = pipe[0], pipe[1]
        return {"mean": sc.mean_.tolist(), "scale": sc.scale_.tolist(), "coef": lr.coef_.tolist(),
                "intercept": lr.intercept_.tolist(), "classes": lr.classes_.tolist()}

    has_view = data.view >= 0
    return {"states": scene_data.STATES, "views": scene_data.VIEWS, "layout": LAYOUT, "prompts": fingerprint(),
            "context": fit(feats, data.y), "view": fit(feats[has_view], data.view[has_view]),
            "zero_shot": {"temperature": 100.0, "weight": zero_shot_weight, "text": text5.tolist(), "view_text": vtext.tolist()},
            "frames": len(data)}


def _kept_text(out):
    """The sentence embeddings of the probe already in `out`, if it was made for the current sentences."""
    path = out / PROBE
    if not path.exists():
        return None
    old = json.loads(path.read_text())
    if old.get("prompts") != fingerprint():
        return None
    import numpy as np

    return np.array(old["zero_shot"]["text"]), np.array(old["zero_shot"]["view_text"])


def train(out=MODELS, reexport=False, c=HEAD_C, zero_shot_weight=ZERO_SHOT_WEIGHT, check=True):
    """Make (or refresh) the models and probe in `out`, then the Rust tests' fixture and replay sequences, then report."""
    import coremltools as ct

    from . import evaluate, fixture

    out = Path(out)
    out.mkdir(parents=True, exist_ok=True)
    data = scene_data.load()
    if not len(data):
        raise SystemExit("no labeled frames yet: extract a recording, label it, then train")
    text = None
    if reexport or not models_current(out):
        print("converting the models to Core ML (a few minutes) ...", flush=True)
        text = export_models(out)
    else:
        print("models are current: only new frames and the probe are computed")
    text = text or _kept_text(out) or _text_for_new_sentences(out)
    clip = ct.models.MLModel(str(out / ENCODER))
    backbone = ct.models.MLModel(str(out / BACKBONE))
    feats = features.compute(clip, backbone)  # the probe learns from exactly what the app computes
    probe = fit_probe(*text, feats, c, zero_shot_weight)
    (out / PROBE).write_text(json.dumps(probe))
    print(f"wrote {out / PROBE} ({len(data)} frames in {len(data.names)} recordings)")
    fixture.write(out, clip, backbone)
    evaluate.write_sequences(out)
    if check:
        evaluate.report(model_dir=out, c=c, zero_shot_weight=zero_shot_weight)
        evaluate.check(out, clip, backbone)


def _text_for_new_sentences(out):
    """The sentences changed since the probe was made: embed them with MobileCLIP's text tower (no conversion needed)."""
    model, tokenizer = _clip()
    (out / STAMP).write_text(json.dumps({"pictures": VERSION, "prompts": fingerprint()}))
    return _text_embeddings(model, tokenizer)
