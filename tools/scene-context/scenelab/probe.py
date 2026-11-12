"""The probe's arithmetic in numpy: what the app's Rust code (crates/openps-scene/src/probe.rs) does with the two embeddings of a frame.

A logistic-regression head reads the joined embeddings; the zero-shot reading (similarity of the whole-frame embedding to one sentence per
class) is mixed in with a weight, and since the sentences know "menu or cutscene" as one class, the head splits that mass.
"""
import numpy as np

from .data import CUT, MENU


def softmax(z):
    z = np.exp(z - z.max(axis=-1, keepdims=True))
    return z / z.sum(axis=-1, keepdims=True)


def zero_shot(x, text, temperature=100.0):
    """Probability of on foot, land, air and water vehicle, menu-or-cutscene from the first `text.shape[1]` numbers of each row (the whole frame)."""
    return softmax(temperature * x[:, : text.shape[1]] @ text.T)


def mix(p, zs5, weight):
    """Context probabilities: `weight` of the zero-shot reading, the rest the head's `p` (rows of 4)."""
    share = p[:, MENU] / np.maximum(p[:, MENU] + p[:, CUT], 1e-9)
    zero = np.stack([zs5[:, 0], zs5[:, 1:4].sum(1), zs5[:, 4] * share, zs5[:, 4] * (1 - share)], 1)
    return weight * zero + (1 - weight) * p


def linear(p, x):
    """Logits of a saved head (scene-probe.json's context or view table) for the rows of x."""
    return ((x - np.array(p["mean"])) / np.array(p["scale"])) @ np.array(p["coef"]).T + np.array(p["intercept"])


def context(probe, x):
    zs = probe["zero_shot"]
    p = np.zeros((len(x), 4))
    p[:, probe["context"]["classes"]] = softmax(linear(probe["context"], x))
    return mix(p, zero_shot(x, np.array(zs["text"]), zs["temperature"]), zs.get("weight", 0.5))


def view(probe, x):
    logits = linear(probe["view"], x)
    third = 1 / (1 + np.exp(-logits[:, 0])) if logits.shape[1] == 1 else softmax(logits)[:, 1]
    return np.stack([1 - third, third], 1)
