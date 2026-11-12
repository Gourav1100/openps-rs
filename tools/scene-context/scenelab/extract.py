"""Recordings to frames: a console capture (.opsvid, `scripts/record-session.sh`) or an ordinary video file, sampled into 480 px JPEGs.

A frame is data/frames/<id>__f_NNNNN.jpg, where the id is a short hash of the recording's name; data/index.json lists every recording's frames with their
time in it, and its name. Needs ffmpeg.
"""
import re
import struct
import subprocess
import tempfile
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

from .data import new_recording_id, read_index, recording_id, write_index_entry
from .paths import FRAMES

VIDEO_SUFFIXES = {".mp4", ".mov", ".mkv", ".m4v", ".webm"}
THUMB_WIDTH = 480
DEFAULT_STEP = {".opsvid": 2.0}  # seconds between frames; ordinary videos default to 6


def video_recording_name(source):
    return f"mp4-{re.sub(r'[^A-Za-z0-9_-]+', '_', Path(source).stem).strip('_')}"


def extract(source, step=None, name=None):
    """Sample `source` into data/frames and return the recording's id. The recording's name is made from the file's name unless `name` sets it; a
    default name that a recording made from another file already has is refused, so nothing is overwritten."""
    source = Path(source).expanduser()
    if not source.is_file():
        raise SystemExit(f"{source} does not exist")
    suffix = source.suffix.lower()
    if suffix != ".opsvid" and suffix not in VIDEO_SUFFIXES:
        raise SystemExit(f"{source.name}: expected an .opsvid capture or a video file ({', '.join(sorted(VIDEO_SUFFIXES))})")
    step = step or DEFAULT_STEP.get(suffix, 6.0)
    default = name is None
    name = name or (source.stem if suffix == ".opsvid" else video_recording_name(source))
    rid = recording_id(name)
    if default and rid and read_index()[rid].get("path") != str(source):
        raise SystemExit(f"a recording made from {read_index()[rid]['capture']} already has the name {name}: "
                         "if this is another file, give it a name of its own (--name)")
    rid = rid or new_recording_id(name)
    frames = (_from_capture if suffix == ".opsvid" else _from_video)(source, rid, step)
    write_index_entry(rid, {"name": name, "capture": source.name, "path": str(source), "step": step, "frames": frames})
    print(f"{name} ({rid}): {len(frames)} frames, one every {step:g} s -> {FRAMES}")
    return rid


def fresh_frames_dir(name):
    """data/frames, without the frames an earlier extraction of this recording left."""
    FRAMES.mkdir(parents=True, exist_ok=True)
    for old in FRAMES.glob(f"{name}__f_*.jpg"):
        old.unlink()
    return FRAMES


def frame_file(name, path):
    """The frame's name inside its recording (frames/f_NNNNN.jpg) from its file in data/frames."""
    return f"frames/{path.name[len(name) + 2:]}"


def _from_capture(source, name, step):
    frames_dir = fresh_frames_dir(name)
    times, first = [], None
    with tempfile.TemporaryDirectory() as tmp:  # the raw stream is as large as the capture: it is not kept
        stream = Path(tmp) / "stream.bin"
        with source.open("rb") as f, stream.open("wb") as w:
            if f.read(8) != b"OPSVID1\n":
                raise SystemExit(f"{source.name} is not an opsvid capture")
            while True:
                head = f.read(16)
                if len(head) < 16:
                    break
                kind, _frame_id, _adaptive, at_us, length = struct.unpack("<BHBQI", head)
                blob = f.read(length)
                if len(blob) < length:
                    break
                first = first or blob
                w.write(blob)
                if kind == 0:
                    times.append(at_us / 1e6)
        if not times:
            raise SystemExit(f"{source.name} holds no video frames")
        i = first.find(b"\x00\x00\x01") + 3
        codec = "hevc" if ((first[i] >> 1) & 0x3F) in (32, 33, 34) else "h264"
        fps = (len(times) - 1) / max(times[-1] - times[0], 1e-6)
        every = max(1, round(fps * step))
        picks = list(range(0, len(times), every))
        print(f"{codec}: {len(times)} access units over {times[-1] - times[0]:.0f} s (~{fps:.0f} fps), one thumbnail per {every}")
        cmd = ["ffmpeg", "-y", "-loglevel", "error", "-f", codec, "-i", str(stream),
               "-vf", f"select='not(mod(n\\,{every}))',scale={THUMB_WIDTH}:-2", "-fps_mode", "vfr", "-q:v", "5",
               str(frames_dir / f"{name}__f_%05d.jpg")]
        if subprocess.run(cmd[:4] + ["-hwaccel", "videotoolbox"] + cmd[4:]).returncode != 0:
            subprocess.run(cmd, check=True)
    made = sorted(frames_dir.glob(f"{name}__f_*.jpg"))
    if len(made) != len(picks):
        print(f"warning: {len(made)} thumbnails for {len(picks)} expected; later times may drift")
    return [{"file": frame_file(name, p), "t": round(times[n] - times[0], 1)} for p, n in zip(made, picks)]


def _from_video(source, name, step):
    frames_dir = fresh_frames_dir(name)
    duration = float(subprocess.check_output(
        ["ffprobe", "-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0", str(source)]).strip())
    times = [t for t in (step / 2 + i * step for i in range(int((duration - step / 2) // step) + 1)) if t < duration - 1]

    def grab(i):  # seeking instead of decoding everything
        subprocess.run(["ffmpeg", "-y", "-loglevel", "error", "-ss", f"{times[i]:.2f}", "-i", str(source), "-frames:v", "1",
                        "-vf", f"scale={THUMB_WIDTH}:-2", "-q:v", "5", str(frames_dir / f"{name}__f_{i + 1:05d}.jpg")], check=True)

    with ThreadPoolExecutor(6) as pool:
        list(pool.map(grab, range(len(times))))
    return [{"file": f"frames/f_{i + 1:05d}.jpg", "t": round(t, 1)} for i, t in enumerate(times)]
