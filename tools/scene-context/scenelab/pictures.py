"""The pictures the scene model reads, as the app cuts them (Vision) and as the training code cuts them (PIL).

One decoded frame gives two pictures, both the whole frame squashed to the model's input; their embeddings, in this order, are the probe's input:
  whole      MobileCLIP-S1, 256 x 256 (the zero-shot sentences read this embedding too)
  backbone   DINOv2-B/14, 392 x 224 (16:9 in 14-pixel patches)
crates/openps-scene/src/encoder.rs holds the same table; tests check that both cut the same pictures.

What the first version read, the middle square, left out the sides of the frame, and the sides are where the game keeps its HUD
(results/improve.txt). Corner crops of the HUD and the middle square were tried next to the whole frame and did not help.
"""
from PIL import Image

VERSION = "mobileclip-s1 256x256 + dinov2-base 392x224, whole frame squashed"  # change it when what the models see changes: cached features are then recomputed
CLIP_SIZE = (256, 256)
BACKBONE_SIZE = (392, 224)  # width, height

# The head on top of the two embeddings: logistic regression strength, and how much of the answer is the zero-shot reading (`scene.py eval --c --zs`)
HEAD_C = 0.1
ZERO_SHOT_WEIGHT = 0.25

PICTURES = [("whole", "clip", CLIP_SIZE, 512), ("backbone", "backbone", BACKBONE_SIZE, 1536)]
LAYOUT = [p[0] for p in PICTURES]
DIM = sum(p[3] for p in PICTURES)


def all_pictures(path):
    im = Image.open(path).convert("RGB")
    return [im.resize(size, Image.BILINEAR) for _, _, size, _ in PICTURES]
