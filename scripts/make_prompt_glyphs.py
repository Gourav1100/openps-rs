#!/usr/bin/env python3
"""Build the controller-glyph templates the prompt detector matches against.

Source: Kenney "Input Prompts" 1.5, PlayStation Series (https://kenney.nl/assets/input-prompts, Creative Commons CC0).
Usage: scripts/make_prompt_glyphs.py /path/to/kenney_input-prompts/"PlayStation Series"/Default

Writes
  crates/openps-metal/src/prompt_glyphs.bin       templates: records of  kind(u8) aspect(f32 LE) 24*24 bytes
  crates/openps-metal/tests/data/glyph_scenes.bin 64x64 luma+alpha art of one glyph per kind, for the scene tests
Needs only Pillow. A glyph is reduced to light-on-black luma (alpha * luma), cropped to its tight box and stretched to 24x24, which is
exactly how the detector samples a candidate's box.
"""
import struct, sys, os
from PIL import Image

KINDS = ["Cross", "Circle", "Square", "Triangle", "L1", "R1", "L2", "R2", "LStick", "RStick", "L3", "R3", "DPad", "Options", "Create", "Mute"]
G = 24
FILES = {
    "Cross": ["playstation_button_cross", "playstation_button_cross_outline", "playstation_button_color_cross", "playstation_button_color_cross_outline"],
    "Circle": ["playstation_button_circle", "playstation_button_circle_outline", "playstation_button_color_circle", "playstation_button_color_circle_outline"],
    "Square": ["playstation_button_square", "playstation_button_square_outline", "playstation_button_color_square", "playstation_button_color_square_outline"],
    "Triangle": ["playstation_button_triangle", "playstation_button_triangle_outline", "playstation_button_color_triangle", "playstation_button_color_triangle_outline"],
    "L1": ["playstation_trigger_l1", "playstation_trigger_l1_alternative", "playstation_trigger_l1_outline", "playstation_trigger_l1_alternative_outline"],
    "R1": ["playstation_trigger_r1", "playstation_trigger_r1_alternative", "playstation_trigger_r1_outline", "playstation_trigger_r1_alternative_outline"],
    "L2": ["playstation_trigger_l2", "playstation_trigger_l2_alternative", "playstation_trigger_l2_outline", "playstation_trigger_l2_alternative_outline"],
    "R2": ["playstation_trigger_r2", "playstation_trigger_r2_alternative", "playstation_trigger_r2_outline", "playstation_trigger_r2_alternative_outline"],
    "LStick": ["playstation_stick_l", "playstation_stick_l_press", "playstation_button_analog", "playstation_button_analog_outline"],
    "RStick": ["playstation_stick_r", "playstation_stick_r_press"],
    "L3": ["playstation_button_l3", "playstation_button_l3_outline"],
    "R3": ["playstation_button_r3", "playstation_button_r3_outline"],
    "DPad": ["playstation_dpad", "playstation_dpad_all", "playstation_dpad_none", "playstation_dpad_up", "playstation_dpad_down", "playstation_dpad_left", "playstation_dpad_right",
             "playstation_dpad_horizontal", "playstation_dpad_vertical", "playstation_dpad_outline" if False else "playstation_dpad_up_outline", "playstation_dpad_down_outline",
             "playstation_dpad_left_outline", "playstation_dpad_right_outline", "playstation_dpad_horizontal_outline", "playstation_dpad_vertical_outline"],
    "Options": ["playstation5_button_options", "playstation5_button_options_alternative", "playstation5_button_options_outline", "playstation5_button_options_alternative_outline"],
    "Create": ["playstation5_button_create", "playstation5_button_create_alternative", "playstation5_button_create_outline", "playstation5_button_create_alternative_outline"],
    "Mute": ["playstation5_button_mute", "playstation5_button_mute_outline"],
}
# one glyph per kind for the scene tests
SCENE = {"Cross": "playstation_button_cross", "Circle": "playstation_button_circle", "Square": "playstation_button_square", "Triangle": "playstation_button_triangle",
         "L1": "playstation_trigger_l1", "R1": "playstation_trigger_r1", "L2": "playstation_trigger_l2", "R2": "playstation_trigger_r2",
         "LStick": "playstation_stick_l", "RStick": "playstation_stick_r", "L3": "playstation_button_l3", "R3": "playstation_button_r3",
         "DPad": "playstation_dpad", "Options": "playstation5_button_options", "Create": "playstation5_button_create", "Mute": "playstation5_button_mute"}

def luma_alpha(path):
    im = Image.open(path).convert("RGBA")
    px = im.load()
    w, h = im.size
    lum = [[0.0] * w for _ in range(h)]
    al = [[0.0] * w for _ in range(h)]
    for y in range(h):
        for x in range(w):
            r, g, b, a = px[x, y]
            al[y][x] = a / 255.0
            lum[y][x] = (0.299 * r + 0.587 * g + 0.114 * b) * a / 255.0   # light-on-black luma
    return lum, al, w, h

def components(al, w, h, thr=0.4):
    seen = [[False] * w for _ in range(h)]
    out = []
    for sy in range(h):
        for sx in range(w):
            if al[sy][sx] < thr or seen[sy][sx]:
                continue
            st, comp = [(sx, sy)], []
            seen[sy][sx] = True
            while st:
                x, y = st.pop()
                comp.append((x, y))
                for nx, ny in ((x - 1, y), (x + 1, y), (x, y - 1), (x, y + 1)):
                    if 0 <= nx < w and 0 <= ny < h and not seen[ny][nx] and al[ny][nx] >= thr:
                        seen[ny][nx] = True
                        st.append((nx, ny))
            out.append(comp)
    return out

# A glyph on the stream is a few pixels to a few dozen across: besides the crisp art, templates are made as the art looks after being
# reduced to this many pixels on its longer side and stretched back up.
RESOLUTIONS = [None, 20, 14, 10]

def record(kind, lum, w, h, box):
    x0, y0, x1, y1 = box
    img = Image.new("F", (w, h))
    img.putdata([lum[y][x] for y in range(h) for x in range(w)])
    crop = img.crop((x0, y0, x1 + 1, y1 + 1))
    bw, bh = crop.size
    out = b""
    for res in RESOLUTIONS:
        c = crop
        if res is not None:
            k = res / max(bw, bh)
            if k >= 1:
                continue
            c = crop.resize((max(2, round(bw * k)), max(2, round(bh * k))), Image.BOX)
        c = c.resize((G, G), Image.BILINEAR if res is not None else (Image.BOX if bw >= G else Image.BILINEAR))
        vals = list(c.getdata())
        top = max(vals) or 1.0
        data = bytes(int(round(min(255.0, v / top * 255.0))) for v in vals)
        out += struct.pack("<Bf", KINDS.index(kind), bw / bh) + data
    return out

def main(src, root):
    recs, n = [], 0
    for kind, names in FILES.items():
        for name in names:
            path = os.path.join(src, name + ".png")
            if not os.path.exists(path):
                print("missing", name)
                continue
            lum, al, w, h = luma_alpha(path)
            comps = components(al, w, h)
            def bbox(c):
                xs, ys = [p[0] for p in c], [p[1] for p in c]
                return min(xs), min(ys), max(xs), max(ys)
            allp = [p for c in comps for p in c]
            recs.append(record(kind, lum, w, h, bbox(allp)))
    open(os.path.join(root, "crates/openps-metal/src/prompt_glyphs.bin"), "wb").write(b"".join(recs))
    print(len(recs), "templates")
    scene = b""
    for kind, name in SCENE.items():
        lum, al, w, h = luma_alpha(os.path.join(src, name + ".png"))
        scene += bytes([KINDS.index(kind)]) + bytes(int(round(lum[y][x])) for y in range(h) for x in range(w)) + bytes(int(round(al[y][x] * 255)) for y in range(h) for x in range(w))
    open(os.path.join(root, "crates/openps-metal/tests/data/glyph_scenes.bin"), "wb").write(scene)

if __name__ == "__main__":
    main(sys.argv[1], os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
