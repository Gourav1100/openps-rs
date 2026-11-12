"""A folder of screenshots and clips (not console captures) into data/, as recordings the labeling page can show.

Stills become `shots-NNN` recordings and short clips `clips-NNN` (a few frames each), `chunk` frames per recording, in name order: one game's
files mostly stay together, so a leave-one-recording-out benchmark is not flattered by near copies. A long clip (continuous gameplay)
is a recording of its own, as `scene.py extract` makes it. Size variants of one screenshot (name-500x281.jpg beside name.jpg) count once;
other file types (gif, svg) are skipped. Nothing is labeled: `scene.py label --only shots-001` opens one recording's frames.
"""
import re
import subprocess
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

from PIL import Image, ImageOps

from . import extract
from .data import new_recording_id, read_index, recording_id, write_index_entry

STILLS = {".jpg", ".jpeg", ".png", ".gif"}  # a gif is read as its first frame
SIZE_SUFFIX = re.compile(r"-\d+x\d+$")
WORKERS = 8


def _thumb(src, dst):
    with Image.open(src) as im:
        im.draft("RGB", (extract.THUMB_WIDTH, 1))  # JPEG decodes at a fraction of the size; other formats ignore it
        im = ImageOps.exif_transpose(im).convert("RGB")
        w, h = im.size
        if w > extract.THUMB_WIDTH:
            im = im.resize((extract.THUMB_WIDTH, max(2, round(h * extract.THUMB_WIDTH / w / 2) * 2)), Image.LANCZOS)
        im.save(dst, quality=85)


def _distinct_stills(files):
    """Every screenshot once: all files without a size suffix (a .jpg and a .png of one name can be different pictures), and for a name that only
    has size variants the largest."""
    groups = {}
    for p in files:
        if p.suffix.lower() in STILLS:
            groups.setdefault(SIZE_SUFFIX.sub("", p.stem).lower(), []).append(p)
    chosen = []
    for group in groups.values():
        plain = [p for p in group if not SIZE_SUFFIX.search(p.stem)]
        chosen += plain or [max(group, key=lambda p: p.stat().st_size)]
    return sorted(chosen, key=lambda p: p.name.lower())


def _chunks(items, size):
    return [items[i : i + size] for i in range(0, len(items), size)]


def _write(rid, name, source, frames):
    write_index_entry(rid, {"name": name, "capture": source, "step": 0, "frames": frames})
    print(f"{name} ({rid}): {len(frames)} frames", flush=True)


def _shots(name, files, source):
    rid = recording_id(name) or new_recording_id(name)
    frames_dir = extract.fresh_frames_dir(rid)

    def one(job):
        i, p = job
        try:
            _thumb(p, frames_dir / f"{rid}__f_{i:05d}.jpg")
            return {"file": f"frames/f_{i:05d}.jpg", "t": 0.0, "source": p.name}
        except Exception as e:
            print(f"skipped {p.name}: {e}")

    with ThreadPoolExecutor(WORKERS) as pool:
        _write(rid, name, source, [f for f in pool.map(one, enumerate(files, 1)) if f])


def _duration(p):
    try:
        out = subprocess.check_output(["ffprobe", "-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0", str(p)], text=True)
        return float(out.split()[0])
    except (subprocess.CalledProcessError, ValueError, IndexError):
        return None


def _clips(name, plans, source):
    rid = recording_id(name) or new_recording_id(name)
    frames_dir = extract.fresh_frames_dir(rid)
    jobs = [(p, t) for p, times in plans for t in times]

    def one(job):
        (p, t), i = job
        dst = frames_dir / f"{rid}__f_{i:05d}.jpg"
        cmd = ["ffmpeg", "-y", "-loglevel", "error", "-ss", f"{t:.2f}", "-i", str(p), "-frames:v", "1",
               "-vf", f"scale={extract.THUMB_WIDTH}:-2", "-q:v", "5", str(dst)]
        if subprocess.run(cmd).returncode == 0 and dst.exists():
            return {"file": extract.frame_file(rid, dst), "t": t, "source": p.name}

    with ThreadPoolExecutor(WORKERS) as pool:
        _write(rid, name, source, [f for f in pool.map(one, zip(jobs, range(1, len(jobs) + 1))) if f])


def ingest(folder, per_clip=8, clip_step=2.0, long_s=120.0, long_step=6.0, chunk=1000, force=False):
    folder = Path(folder).expanduser()
    if not folder.is_dir():
        raise SystemExit(f"{folder} is not a folder")
    files = sorted(p for p in folder.iterdir() if p.is_file() and not p.name.startswith("."))
    stills = _distinct_stills(files)
    clips = [p for p in files if p.suffix.lower() in extract.VIDEO_SUFFIXES]
    every_still = sum(p.suffix.lower() in STILLS for p in files)
    print(f"{len(stills)} screenshots ({every_still - len(stills)} size variants ignored), {len(clips)} clips, "
          f"{len(files) - every_still - len(clips)} other files skipped")

    index_names = [e.get("name", "") for e in read_index().values()]
    seen = set() if force else {f["source"] for e in read_index().values() if e.get("name", "").startswith("shots-") for f in e["frames"]}
    fresh = [p for p in stills if p.name not in seen]
    numbers = [int(n[6:]) for n in index_names if re.fullmatch(r"shots-\d+", n)]
    first = 1 if force or not numbers else max(numbers) + 1  # screenshots that came later go into new recordings
    if not fresh:
        print("no screenshots that were not imported before")
    for n, group in enumerate(_chunks(fresh, chunk), first):
        _shots(f"shots-{n:03d}", group, folder.name)

    short, plans = [], []
    for p in clips:
        d = _duration(p)
        if d is None:
            print(f"skipped {p.name}: not readable")
        elif d >= long_s:
            name = extract.video_recording_name(p)
            if recording_id(name) and not force:
                print(f"{name}: already imported")
            else:
                extract.extract(p, long_step, name)
        else:
            n = min(per_clip, max(1, round(d / clip_step)))
            short.append((p, [round((k + 0.5) * d / n, 2) for k in range(n)]))
    group, count = [], 0
    for p, times in short:
        if group and count + len(times) > chunk:
            plans.append(group)
            group, count = [], 0
        group.append((p, times))
        count += len(times)
    plans += [group] if group else []
    for n, group in enumerate(plans, 1):
        name = f"clips-{n:03d}"
        if recording_id(name) and not force:
            print(f"{name}: already imported")
        else:
            _clips(name, group, folder.name)
