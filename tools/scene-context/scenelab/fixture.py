"""Reference values for the Rust scene code (crates/openps-scene/tests/reference.rs), written from the Python the models and the filter were evaluated with:
the joined embedding Core ML gives for a few labeled frames with the probabilities the probe makes of it, and a sequence of observations and actions with the
filter's belief after each step. The frames' probabilities depend on the probe, so `scene.py train` rewrites this file every time it writes a probe.
"""
import json
from pathlib import Path

import numpy as np

from . import data as scene_data
from . import features, filtersim, probe
from .paths import DATASET, FIXTURE, MODELS


def write(model_dir=MODELS, clip=None, backbone=None, out=FIXTURE):
    model_dir, out = Path(model_dir), Path(out)
    if clip is None:
        import coremltools as ct

        clip = ct.models.MLModel(str(model_dir / "scene-encoder.mlpackage"))
        backbone = ct.models.MLModel(str(model_dir / "scene-backbone.mlpackage"))
    saved = json.loads((model_dir / "scene-probe.json").read_text())
    data = scene_data.load()
    picks = []  # three frames of each state, spread over the recordings
    for state in range(4):
        idx = np.flatnonzero(data.y == state)
        picks += [int(i) for i in idx[np.linspace(0, len(idx) - 1, 3).astype(int)]]
    emb = np.stack([features.embed(clip, backbone, data.paths[i]) for i in picks])
    ctx, view = probe.context(saved, emb), probe.view(saved, emb)
    frames = [{"file": str(data.paths[i].relative_to(DATASET)), "embedding": e.tolist(), "context": c.tolist(), "view": v.tolist()}
              for i, e, c, v in zip(picks, emb, ctx, view)]

    # the filter: a made-up drive with irregular spacing, all three actions available
    rng = np.random.default_rng(7)
    n = 60
    t = np.cumsum(np.full(n, 2.0))  # the evaluation spacing (alpha 0.7)
    truth = np.repeat([0, 1, 1, 2, 0, 3], 10)
    pc = np.stack([rng.dirichlet(np.where(np.arange(4) == s, 6.0, 1.0)) for s in truth])
    pv = np.stack([rng.dirichlet(np.where(np.arange(2) == (i // 20) % 2, 4.0, 1.0)) for i in range(n)])
    actions = sorted([(float(t[9]) + 0.7, "key_enter_or_exit_vehicle"), (float(t[29]) + 1.1, "key_pause_game"),
                      (float(t[39]) + 0.4, "key_pause_game"), (float(t[19]) - 0.5, "key_change_camera"),
                      (float(t[45]) + 1.9, "key_change_camera")])
    filtersim.ALPHA = 0.7
    belief = filtersim.filter_context(pc, t, actions, 0.003, 0.6)
    view_belief = filtersim.filter_view(pv, t, actions, belief.argmax(1), 0.002, 1.0)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps({
        "frames": frames,
        "filter": {"t": t.tolist(), "context_probs": pc.tolist(), "view_probs": pv.tolist(), "actions": actions,
                   "context_belief": belief.tolist(), "view_belief": view_belief.tolist()},
    }))
    print(f"wrote {out} ({len(frames)} frames, {n} filter steps)")
