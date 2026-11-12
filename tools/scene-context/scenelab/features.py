"""The probe's input for every labeled frame, as the app computes it (Core ML), cached per frame.

A frame costs about 30 ms, so a new recording only pays for its own frames: the cache (cache/features.npz) is keyed by "recording/file" and by
pictures.VERSION, and everything else in it is reused. `scene.py train` fills it; the evaluations read it.
"""
import time

import numpy as np

from . import data as scene_data
from .paths import CACHE
from .pictures import PICTURES, VERSION, all_pictures

FILE = CACHE / "features.npz"
POOL = CACHE / "features-pool.npz"  # the same for frames that are not labeled (scene.py suggest)


def _read(file=FILE):
    """{key: vector} of what is cached for the current pictures."""
    if not file.exists():
        return {}
    d = np.load(file, allow_pickle=False)
    return dict(zip(d["keys"].tolist(), d["x"])) if str(d["version"]) == VERSION else {}


def _write(cache, file=FILE):
    file.parent.mkdir(parents=True, exist_ok=True)
    keys = sorted(cache)
    np.savez(file, version=VERSION, keys=np.array(keys), x=np.stack([cache[k] for k in keys]).astype(np.float32))


def embed(clip, backbone, path):
    """The two Core ML embeddings of one frame, joined in pictures.PICTURES order."""
    return np.concatenate([
        (clip if model == "clip" else backbone).predict({"image": im})["embedding"].reshape(-1)
        for (_, model, _, _), im in zip(PICTURES, all_pictures(path))
    ])


def compute(clip, backbone):
    """Features of every labeled frame (rows in data order); frames not cached yet go through the two Core ML models."""
    data = scene_data.load()
    cache = _read()
    todo = [i for i, f in enumerate(data.files) if f not in cache]
    if todo:  # a frame that was in the pool (scene.py suggest) already has its features
        pooled = _read(POOL)
        for i in todo:
            if data.files[i] in pooled:
                cache[data.files[i]] = pooled[data.files[i]]
        todo = [i for i in todo if data.files[i] not in cache]
        _write(cache)
    if todo:
        print(f"{len(todo)} of {len(data)} frames need their features ({len(data) - len(todo)} cached)", flush=True)
        started = time.time()
        for n, i in enumerate(todo, 1):
            cache[data.files[i]] = embed(clip, backbone, data.paths[i])
            if n % 500 == 0:
                print(f"  {n}/{len(todo)} ({time.time() - started:.0f} s)", flush=True)
        _write(cache)
    return np.stack([cache[f] for f in data.files]).astype(np.float64)


def cached():
    """The cached features (rows in data order). `scene.py train` computes what is missing."""
    data = scene_data.load()
    cache = _read()
    missing = [f for f in data.files if f not in cache]
    if missing:
        cache = {**_read(POOL), **cache}
        missing = [f for f in data.files if f not in cache]
    if missing:
        raise SystemExit(f"{len(missing)} labeled frames have no features yet (first: {missing[0]}); run train.sh")
    return np.stack([cache[f] for f in data.files]).astype(np.float64)


def pool(frames, clip, backbone):
    """Features of frames that are not in the training set: `frames` is [(key, path)]; the rows come back in that order. Cached in POOL, so a frame
    costs its 30 ms once."""
    cache = _read(POOL)
    todo = [(k, p) for k, p in frames if k not in cache]
    if todo:
        print(f"{len(todo)} of {len(frames)} frames need their features ({len(frames) - len(todo)} cached)", flush=True)
        started = time.time()
        for n, (k, p) in enumerate(todo, 1):
            cache[k] = embed(clip, backbone, p)
            if n % 1000 == 0:
                print(f"  {n}/{len(todo)} ({time.time() - started:.0f} s)", flush=True)
                _write(cache, POOL)
        _write(cache, POOL)
    return np.stack([cache[k] for k, _ in frames]).astype(np.float64)


def vectors(frames, model_dir):
    """Features of any frames, labeled or not: `frames` is [(key, path)]. From the training cache, then the pool cache; the rest go through the Core ML
    models (and are kept in the pool cache)."""
    have = {**_read(POOL), **_read()}
    missing = [(k, p) for k, p in frames if k not in have]
    if missing:
        import coremltools as ct
        from pathlib import Path

        clip = ct.models.MLModel(str(Path(model_dir) / "scene-encoder.mlpackage"))
        backbone = ct.models.MLModel(str(Path(model_dir) / "scene-backbone.mlpackage"))
        pool(missing, clip, backbone)
        have = {**_read(POOL), **_read()}
    return np.stack([have[k] for k, _ in frames]).astype(np.float32)
