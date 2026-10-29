#!/usr/bin/env python3
"""Build a training set for the prompt detector: controller glyphs composited onto game-like pictures, with exact boxes.

  scripts/make_prompt_dataset.py KENNEY_DIR OUT_DIR [--train N] [--val N] [--seed S]

KENNEY_DIR is Kenney's "Input Prompts" PlayStation Series/Default folder (CC0). Output (YOLO text format, one luma PNG per picture):
  OUT_DIR/{train,val}/images/*.png   OUT_DIR/{train,val}/labels/*.txt  ("class cx cy w h", all 0..1)   OUT_DIR/classes.txt
Pillow only. The detector reads the 960x540 luma plane of the stream, so the pictures are luma and glyphs span 9..46 px like there.

Backgrounds: frames of real footage found under target/ (nature video, a game capture) plus procedural ones (gradients, textures, map-like
line work, HUD bars, blocks of text). Glyphs sit beside a text label most of the time, as in a game, and hard negatives (letters, rings,
rounded boxes, crosses drawn as geometry) are scattered in. The real frames kept for testing are NOT used here.
"""
import os, sys, glob, math, random, argparse
from PIL import Image, ImageDraw, ImageFilter, ImageFont, ImageChops, ImageOps

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from make_prompt_glyphs import FILES, KINDS  # noqa: E402

SIZE = 512
FONTS = [f for f in ["/System/Library/Fonts/Supplemental/DIN Condensed Bold.ttf", "/System/Library/Fonts/Supplemental/Arial Narrow Bold.ttf",
                     "/System/Library/Fonts/Supplemental/Arial Bold.ttf", "/System/Library/Fonts/Supplemental/Impact.ttf",
                     "/System/Library/Fonts/Helvetica.ttc", "/System/Library/Fonts/Supplemental/Futura.ttc",
                     "/System/Library/Fonts/Supplemental/Verdana Bold.ttf"] if os.path.exists(f)]
WORDS = ["SELECT", "BACK", "CLOSE", "PAN", "ZOOM", "CONFIRM", "CANCEL", "SET WAYPOINT", "CLEAR ROUTE", "CONTINUE", "MAP", "SETTINGS", "PLAY",
         "ACCEPT", "SKIP", "OPTIONS", "MENU", "RESUME", "INVENTORY", "EQUIP", "DROP", "BOOST", "BRAKE", "LOOK", "MOVE", "AIM", "FIRE",
         "RELOAD", "CROUCH", "JUMP", "SPRINT", "INTERACT", "HOLD", "PRESS", "OK", "YES", "NO", "NEXT", "PREVIOUS", "FILTER"]

def load_glyphs(src):
    """(kind index, name, L image, A image) for every Kenney file of every kind."""
    out = []
    for kind, names in FILES.items():
        for n in names:
            p = os.path.join(src, n + ".png")
            if not os.path.exists(p):
                continue
            la = Image.open(p).convert("LA")
            lum, al = la.split()
            bb = al.point(lambda v: 255 if v > 100 else 0).getbbox()
            if bb:
                out.append((KINDS.index(kind), n, lum.crop(bb), al.crop(bb)))
    return out

# Glyphs cut from real stream frames: (file, width, height, [(class, x, y, w, h)]) in that frame's pixels. The pause-menu frame is kept out.
REAL_GLYPHS = [
    ("live_plane_960x540.gray", 960, 540, [("L1", 54, 36, 15, 11), ("R1", 208, 36, 15, 11), ("L2", 55, 62, 14, 15), ("R2", 247, 62, 14, 15),
                                           ("L3", 754, 487, 16, 16), ("Circle", 72, 510, 15, 15), ("LStick", 154, 510, 17, 17),
                                           ("RStick", 226, 509, 10, 10), ("Cross", 304, 510, 15, 15), ("Triangle", 424, 510, 15, 15)]),
    ("ps5_menu_420x540.gray", 420, 540, [("Cross", 39, 85, 16, 16), ("Circle", 39, 512, 16, 16)]),
]

def real_glyph_crops(root):
    """(kind index, name, L, A) like the Kenney glyphs, but cut from real frames with a soft rounded mask."""
    out = []
    for f, w, h, items in REAL_GLYPHS:
        im = Image.frombytes("L", (w, h), open(os.path.join(root, "crates/openps-metal/tests/data", f), "rb").read())
        for kind, x, y, bw, bh in items:
            crop = im.crop((x - bw // 2 - 1, y - bh // 2 - 1, x - bw // 2 + bw + 1, y - bh // 2 + bh + 1))
            m = Image.new("L", crop.size, 0)
            ImageDraw.Draw(m).rounded_rectangle([0, 0, crop.size[0] - 1, crop.size[1] - 1], radius=max(2, min(crop.size) * 0.4), fill=255)
            out.append((KINDS.index(kind), "real_" + kind, crop.resize((crop.size[0] * 4, crop.size[1] * 4), Image.BICUBIC),
                        m.resize((crop.size[0] * 4, crop.size[1] * 4), Image.BILINEAR).filter(ImageFilter.GaussianBlur(2))))
    return out

XELU_KIND = {"Cross": "Cross", "Circle": "Circle", "Square": "Square", "Triangle": "Triangle", "L1": "L1", "R1": "R1", "L2": "L2", "R2": "R2",
             "Left_Stick": "LStick", "Right_Stick": "RStick", "Left_Stick_Click": "L3", "Right_Stick_Click": "R3", "Dpad": "DPad", "Dpad_Up": "DPad",
             "Dpad_Down": "DPad", "Dpad_Left": "DPad", "Dpad_Right": "DPad", "Options": "Options", "Options_Alt": "Options", "Share": "Create",
             "Share_Alt": "Create", "Microphone": "Mute"}

def load_xelu(root):
    """Xelu's CC0 controller prompts (PS5, PS4, PS3): a second, differently drawn set of the same buttons (grey, bevelled, dark)."""
    out = []
    base = os.path.join(root, "target/refs/xelu")
    for folder, prefix in (("PS5", "PS5_"), ("Others/PS4", "PS4_"), ("Others/PS3", "PS3_")):
        for f in sorted(glob.glob(os.path.join(base, folder, prefix + "*.png"))):
            key = os.path.basename(f)[len(prefix):-4]
            if key not in XELU_KIND:
                continue
            la = Image.open(f).convert("LA")
            lum, al = la.split()
            bb = al.point(lambda v: 255 if v > 100 else 0).getbbox()
            if bb:
                out.append((KINDS.index(XELU_KIND[key]), "xelu_" + key, lum.crop(bb), al.crop(bb)))
    return out

def restyle(g, rnd):
    """A different drawing of the same glyph: outline only, thicker or thinner strokes, flatter or harsher contrast."""
    kind, name, lum, al = g
    t = rnd.choice(["same", "same", "outline", "bold", "thin", "flat", "harsh"])
    if t == "outline":
        edge = ImageChops.subtract(al, al.filter(ImageFilter.MinFilter(3)))
        return (kind, name, Image.new("L", lum.size, 255), edge.filter(ImageFilter.MaxFilter(3)))
    if t == "bold":
        return (kind, name, lum, al.filter(ImageFilter.MaxFilter(3)))
    if t == "thin":
        return (kind, name, lum, al.filter(ImageFilter.MinFilter(3)))
    if t == "flat":
        return (kind, name, lum.point(lambda v: 150 + v // 4), al)
    if t == "harsh":
        return (kind, name, lum.point(lambda v: 255 if v > 110 else 0), al)
    return g

def extra_backgrounds(root, extra_dirs):
    bg = []
    for d in extra_dirs:
        for f in sorted(glob.glob(os.path.join(d, "*.jpg")) + glob.glob(os.path.join(d, "*.png"))):
            try:
                im = Image.open(f).convert("L")
            except Exception:
                continue
            if min(im.size) >= 200:
                k = 960 / max(im.size[0], 1)
                bg.append(im.resize((max(SIZE, int(im.size[0] * k)), max(SIZE, int(im.size[1] * k))), Image.BILINEAR))
    # quiet parts of live captures of a real game (the strip with prompts is cropped away)
    for f, box in (("target/qa/live.png", (700, 250, 1900, 1000)), ("target/qa/live2.png", (700, 250, 1900, 1000))):
        p = os.path.join(root, f)
        if os.path.exists(p):
            bg.append(Image.open(p).convert("L").crop(box).resize((960, 600), Image.BILINEAR))
    return bg

def real_backgrounds(root):
    """Luma pictures from real footage: raw BGRA dumps of the nature clip and the game capture crop. Test frames are excluded."""
    bg = []
    for f in sorted(glob.glob(os.path.join(root, "target/real/dump/*.bgra")))[:40]:
        data = open(f, "rb").read()
        n = len(data) // 4
        w, h = (1920, 1080) if n == 1920 * 1080 else (1280, 720) if n == 1280 * 720 else (0, 0)
        if not w:
            continue
        im = Image.frombytes("RGBA", (w, h), data).convert("L")  # B and R swapped: harmless for luma at this accuracy
        bg.append(im.resize((960, 540), Image.BILINEAR))
    for f in glob.glob(os.path.join(root, "crates/openps-metal/tests/data/fp_*.gray")):
        dims = os.path.basename(f).rsplit("_", 1)[-1][:-5]
        w, h = (int(v) for v in dims.split("x"))
        bg.append(Image.frombytes("L", (w, h), open(f, "rb").read()).resize((960, int(960 * h / w)), Image.BILINEAR))
    return bg

def procedural_background(rnd):
    w = h = SIZE
    kind = rnd.choice(["flat", "ramp", "noise", "lines", "blocks", "map", "dark_ui", "bokeh"])
    base = rnd.randint(10, 190)
    im = Image.new("L", (w, h), base)
    d = ImageDraw.Draw(im)
    if kind == "ramp":
        a, b = rnd.randint(0, 120), rnd.randint(60, 230)
        im = Image.linear_gradient("L").resize((w, h)).rotate(rnd.choice([0, 90, 45, 135])).point(lambda v: int(a + (b - a) * v / 255))
    elif kind == "noise":
        im = ImageChops.add(im, Image.effect_noise((w, h), rnd.randint(8, 50)).point(lambda v: v // 3), 1.0, -40)
    elif kind in ("lines", "map"):
        for _ in range(rnd.randint(40, 220)):
            x, y = rnd.randint(0, w), rnd.randint(0, h)
            pts = [(x, y)]
            for _ in range(rnd.randint(2, 7)):
                x += rnd.randint(-70, 70)
                y += rnd.randint(-70, 70)
                pts.append((x, y))
            d.line(pts, fill=min(255, base + rnd.randint(30, 140)), width=rnd.choice([1, 1, 2, 3]))
    elif kind == "blocks":
        for _ in range(rnd.randint(8, 40)):
            x, y = rnd.randint(0, w), rnd.randint(0, h)
            d.rectangle([x, y, x + rnd.randint(10, 160), y + rnd.randint(8, 90)], fill=rnd.randint(0, 220))
    elif kind == "dark_ui":
        im = Image.new("L", (w, h), rnd.randint(8, 40))
        d = ImageDraw.Draw(im)
        for _ in range(rnd.randint(3, 10)):
            y = rnd.randint(0, h)
            d.rectangle([0, y, w, y + rnd.randint(20, 90)], fill=rnd.randint(20, 80))
    elif kind == "bokeh":
        for _ in range(rnd.randint(6, 30)):
            x, y, r = rnd.randint(0, w), rnd.randint(0, h), rnd.randint(8, 60)
            d.ellipse([x - r, y - r, x + r, y + r], fill=min(255, base + rnd.randint(-30, 90)))
        im = im.filter(ImageFilter.GaussianBlur(rnd.uniform(1, 4)))
    return im

def text_label(rnd, height):
    """Light (or dark) text image about `height` px tall."""
    font_path = rnd.choice(FONTS) if FONTS else None
    size = max(8, int(height * rnd.uniform(0.9, 1.5)))
    font = ImageFont.truetype(font_path, size) if font_path else ImageFont.load_default()
    txt = rnd.choice(WORDS)
    im = Image.new("L", (int(size * len(txt) * 0.75) + 8, size + 8), 0)
    ImageDraw.Draw(im).text((2, 0), txt, fill=255, font=font)
    bb = im.getbbox()
    return im.crop(bb) if bb else im

def paste_glyph(canvas, g, cx, cy, side, rnd, invert):
    """Paste glyph `g` (kind, name, L, A) so its longer side is `side` px, centred at (cx, cy). Returns its box (x0, y0, x1, y1)."""
    _, name, lum, al = g
    k = side / max(lum.size)
    sz = (max(2, round(lum.size[0] * k)), max(2, round(lum.size[1] * k)))
    lum, al = lum.resize(sz, Image.LANCZOS), al.resize(sz, Image.LANCZOS)
    # a glyph is drawn to be seen: light on a dark patch, dark on a light one (games do both)
    x0, y0 = int(round(cx - sz[0] / 2)), int(round(cy - sz[1] / 2))
    region = canvas.crop((max(0, x0), max(0, y0), max(1, min(SIZE, x0 + sz[0])), max(1, min(SIZE, y0 + sz[1]))))
    mean = sum(region.resize((1, 1), Image.BOX).getdata()) / 1.0
    if not name.startswith(("real_", "xelu_")) and (mean > 150 or (mean < 110 and invert)):
        lum = ImageOps.invert(lum)
    gain = 1.0 if name.startswith(("real_", "xelu_")) else rnd.uniform(0.75, 1.0)
    lum = lum.point(lambda v: int(v * gain))
    canvas.paste(lum, (x0, y0), al)
    return (x0, y0, x0 + sz[0], y0 + sz[1])

def hard_negative(canvas, rnd, x, y):
    d = ImageDraw.Draw(canvas)
    v = rnd.randint(150, 255)
    s = rnd.randint(8, 30)
    t = rnd.choice(["O", "ring", "round_rect", "plus", "x", "L", "R1text", "disc", "bar"])
    if t == "O" and FONTS:
        d.text((x, y), "O", fill=v, font=ImageFont.truetype(rnd.choice(FONTS), s))
    elif t == "ring":
        d.ellipse([x, y, x + s, y + s], outline=v, width=max(1, s // 6))
    elif t == "round_rect":
        d.rounded_rectangle([x, y, x + s * 1.5, y + s], radius=s // 3, outline=v, width=max(1, s // 8))
    elif t == "plus":
        d.line([(x, y + s // 2), (x + s, y + s // 2)], fill=v, width=max(1, s // 5)); d.line([(x + s // 2, y), (x + s // 2, y + s)], fill=v, width=max(1, s // 5))
    elif t == "x":
        d.line([(x, y), (x + s, y + s)], fill=v, width=max(1, s // 6)); d.line([(x, y + s), (x + s, y)], fill=v, width=max(1, s // 6))
    elif t in ("L", "R1text") and FONTS:
        d.text((x, y), "L" if t == "L" else "R1", fill=v, font=ImageFont.truetype(rnd.choice(FONTS), s))
    elif t == "disc":
        d.ellipse([x, y, x + s, y + s], fill=v)
    else:
        d.rounded_rectangle([x, y, x + s * 2, y + max(4, s // 3)], radius=3, fill=v)

def make_one(rnd, glyphs, backgrounds, real_glyphs=None):
    # background: real footage most of the time, at a random scale and crop
    if backgrounds and rnd.random() < 0.55:
        src = rnd.choice(backgrounds)
        k = rnd.uniform(0.6, 1.4)
        s = src.resize((max(SIZE, int(src.size[0] * k)), max(SIZE, int(src.size[1] * k))), Image.BILINEAR)
        x0, y0 = rnd.randint(0, s.size[0] - SIZE), rnd.randint(0, s.size[1] - SIZE)
        canvas = s.crop((x0, y0, x0 + SIZE, y0 + SIZE))
        if rnd.random() < 0.5:
            canvas = canvas.point(lambda v: int(v * rnd.uniform(0.3, 1.0)))  # games are often dark behind prompts
    else:
        canvas = procedural_background(rnd)
    if rnd.random() < 0.6:  # a translucent bar under the prompt row, as menus have
        bar = Image.new("L", (SIZE, rnd.randint(28, 60)), rnd.randint(10, 70))
        y = rnd.randint(0, SIZE - bar.size[1])
        canvas.paste(bar, (0, y), Image.new("L", bar.size, rnd.randint(120, 230)))
    boxes, taken = [], []
    for _ in range(rnd.randint(0, 6)):
        g = rnd.choice(real_glyphs) if real_glyphs and rnd.random() < 0.25 else restyle(rnd.choice(glyphs), rnd)
        side = int(math.exp(rnd.uniform(math.log(9), math.log(46))))
        # glyph + optional label to its right
        label = text_label(rnd, side) if rnd.random() < 0.8 else None
        wtot = side + (label.size[0] + side // 2 if label else 0)
        if wtot + 2 * side + 4 >= SIZE:
            continue
        cx, cy = rnd.randint(side, SIZE - wtot), rnd.randint(side, SIZE - side)
        area = (cx - side, cy - side, cx + wtot + side, cy + side)
        if any(not (area[2] < t[0] or area[0] > t[2] or area[3] < t[1] or area[1] > t[3]) for t in taken):
            continue
        taken.append(area)
        invert = rnd.random() < 0.12
        box = paste_glyph(canvas, g, cx + side // 2, cy, side, rnd, invert)
        if label:
            lx, ly = box[2] + side // 2, cy - label.size[1] // 2
            canvas.paste(Image.new("L", label.size, rnd.randint(170, 255)), (lx, ly), label)
        # skip boxes that left the picture
        if box[0] >= 0 and box[1] >= 0 and box[2] <= SIZE and box[3] <= SIZE:
            boxes.append((g[0], box))
    for _ in range(rnd.randint(0, 5)):
        hard_negative(canvas, rnd, rnd.randint(0, SIZE - 60), rnd.randint(0, SIZE - 40))
    # how video looks: soft, noisy, slightly re-contrasted
    if rnd.random() < 0.7:
        canvas = canvas.filter(ImageFilter.GaussianBlur(rnd.uniform(0.2, 0.9)))
    if rnd.random() < 0.6:
        canvas = ImageChops.add(canvas, Image.effect_noise(canvas.size, rnd.randint(2, 10)).point(lambda v: v // 8), 1.0, -16)
    if rnd.random() < 0.5:
        canvas = ImageOps.autocontrast(canvas, cutoff=rnd.uniform(0, 2)) if rnd.random() < 0.5 else canvas.point(lambda v: min(255, int(v * rnd.uniform(0.8, 1.15))))
    return canvas, boxes

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("kenney"); ap.add_argument("out")
    ap.add_argument("--train", type=int, default=6000); ap.add_argument("--val", type=int, default=600); ap.add_argument("--seed", type=int, default=1); ap.add_argument("--bg", nargs="*", default=[])
    a = ap.parse_args()
    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    rnd = random.Random(a.seed)
    glyphs = load_glyphs(a.kenney) + load_xelu(root) * 2  # Xelu twice: its drawing style is the one games copy most
    reals = real_glyph_crops(root)
    backs = real_backgrounds(root) + extra_backgrounds(root, a.bg)
    print(len(glyphs), "glyph files,", len(backs), "real background pictures")
    os.makedirs(a.out, exist_ok=True)
    open(os.path.join(a.out, "classes.txt"), "w").write("\n".join(KINDS) + "\n")
    for split, n in (("train", a.train), ("val", a.val)):
        os.makedirs(os.path.join(a.out, split, "images"), exist_ok=True)
        os.makedirs(os.path.join(a.out, split, "labels"), exist_ok=True)
        counts = [0] * len(KINDS)
        for i in range(n):
            im, boxes = make_one(rnd, glyphs, backs, reals)
            name = f"{split}_{i:05d}"
            im.save(os.path.join(a.out, split, "images", name + ".png"))
            with open(os.path.join(a.out, split, "labels", name + ".txt"), "w") as f:
                for c, (x0, y0, x1, y1) in boxes:
                    counts[c] += 1
                    f.write(f"{c} {(x0 + x1) / 2 / SIZE:.5f} {(y0 + y1) / 2 / SIZE:.5f} {(x1 - x0) / SIZE:.5f} {(y1 - y0) / SIZE:.5f}\n")
        print(split, n, "pictures;", ", ".join(f"{k}={c}" for k, c in zip(KINDS, counts)))

if __name__ == "__main__":
    main()
