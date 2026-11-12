"""The labeled frames in one place: which recordings there are, and what every frame in them is.

Everything that needs labels reads them through here (dataset/labels.json), so a newly labeled recording is picked up without editing a list.
A recording is known by a short id (a prefix of the hash of its name), in the dataset and everywhere else; its name, the file it came from, stays in
data/index.json, on this machine. A frame is named by its key, "<id>/frames/f_NNNNN.jpg" (what the feature cache is keyed by); its picture is
dataset/frames/<id>__f_NNNNN.jpg. dataset/recordings.json holds what the tools need to know about a recording besides its frames: whether it is a sequence
(continuous gameplay, to replay the filter on), how its frames are thinned, and the order the first evaluations used.
"""
import hashlib
import json
import os
from pathlib import Path

import numpy as np

from .paths import DATASET_FRAMES, INDEX, LABELS, RECORDINGS

STATES = ["on_foot", "in_vehicle", "menu", "cutscene"]
FOOT, VEH, MENU, CUT = range(4)
# Labels the shipped model does not read yet (it has four scenes), kept in the dataset and sorted, and read by the tree in hierarchy.py (`scene.py tree`),
# but never trained on or scored on here: game_3d (a 3D game scene with other in-game interactions: talking, buying, ...), game_2d (a game that is not 3D),
# and in_game (a game not yet told apart as 2D or 3D: to be remapped).
OTHER = ["in_game", "game_2d", "game_3d"]
LABEL_CLASSES = STATES + OTHER  # how frames are labeled in labels.json: a vehicle of any kind is "in_vehicle"
VIEWS = ["first_person", "third_person"]


def key(recording, file):
    return f"{recording}/{file}"


def flat_name(recording, file):
    """The picture's file name when every recording's frames share one folder."""
    return f"{recording}__{Path(file).name}"


def frame_number(file):
    return int(Path(file).stem.split("_")[-1])


def read_json(path, default):
    return json.loads(path.read_text()) if path.exists() else default


def read_labels():
    return read_json(LABELS, {"labels": [], "deleted": []})


def read_index():
    """{recording: {capture, step, frames}} of every extracted recording."""
    return read_json(INDEX, {})


def write_json(path, doc):
    """Replace the file in one step, so a crash or a second reader never sees half of it."""
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_name(path.name + ".tmp")
    tmp.write_text(json.dumps(doc))
    os.replace(tmp, path)


def write_index_entry(rid, entry):
    index = read_index()
    index[rid] = entry
    write_json(INDEX, index)


_registry = {}


def read_recordings():
    """{id: {order, sequence, thin}} from dataset/recordings.json (read again when the file changes)."""
    stamp = RECORDINGS.stat().st_mtime_ns if RECORDINGS.exists() else None
    if _registry.get("stamp") != stamp:
        _registry.update(stamp=stamp, doc=read_json(RECORDINGS, {}))
    return _registry["doc"]


def display_names():
    """{id: name} of the recordings extracted on this machine; a recording without one is shown by its id."""
    return {rid: e.get("name", rid) for rid, e in read_index().items()}


def display_name(rid):
    return display_names().get(rid, rid)


def matches(rid, prefixes):
    """Whether a recording is wanted by `--only`: no prefixes means all; else its name or its id starts with one of them."""
    return not prefixes or rid.startswith(tuple(prefixes)) or display_name(rid).startswith(tuple(prefixes))


def recording_id(name):
    """The id of the recording of this name, if there is one on this machine."""
    for rid, e in read_index().items():
        if e.get("name") == name:
            return rid
    return None


def new_recording_id(name):
    """An id for a recording that has none: a prefix of the hash of its name, as long as it takes to be new."""
    taken = set(read_index()) | set(read_recordings()) | {l["recording"] for l in read_labels()["labels"]}
    digest = hashlib.sha1(name.encode()).hexdigest()
    return next(digest[:size] for size in range(4, 41) if digest[:size] not in taken)


def keep_frame(rid, number):
    """Whether the frame (its number in the recording, counted from 1) is used. One recording ends in a long run of one static pause map: only the
    first `first` frames and then one in `every` count ("thin" in the registry), so it does not inflate the menu class."""
    thin = read_recordings().get(rid, {}).get("thin")
    return not thin or number <= thin[0] or (number - thin[0] - 1) % thin[1] == 0


def ordered(ids):
    """The recordings of the first evaluations in their order (so old results stay comparable), then the others by name."""
    registry, names = read_recordings(), display_names()
    return sorted(set(ids), key=lambda i: (registry.get(i, {}).get("order", 10**6), names.get(i, i)))


def labeled_recordings():
    """Ids of the recordings with labels the model trains on: the first ones in their old order, then the others by name."""
    return ordered(l["recording"] for l in read_labels()["labels"] if l["context"] not in OTHER)


def extracted_recordings():
    """Every extracted recording, labeled or not: what the labeling page is built from."""
    return ordered(read_index())


def is_sequence(rid):
    """Frames close enough in time (a console capture, or continuous gameplay) to replay the filter on; the rest are single shots."""
    return bool(read_recordings().get(rid, {}).get("sequence"))


class Data:
    """Arrays over every labeled frame: state (0-3), view (0 first, 1 third, -1 none), recording index, time, and where the picture is."""

    def __init__(self):
        doc = read_labels()
        trained = [l for l in doc["labels"] if l["context"] not in OTHER]
        self.names = ordered(l["recording"] for l in trained)
        by_recording = {n: [] for n in self.names}
        for lab in trained:
            by_recording[lab["recording"]].append(lab)
        y, view, sid, t, files, paths = [], [], [], [], [], []
        for s, name in enumerate(self.names):
            for lab in sorted(by_recording[name], key=lambda l: l["file"]):
                if keep_frame(name, frame_number(lab["file"])):
                    y.append(STATES.index(lab["context"]))
                    view.append(VIEWS.index(lab["view"]) if lab.get("view") else -1)
                    sid.append(s)
                    t.append(float(lab["t"]))
                    files.append(key(name, lab["file"]))
                    paths.append(DATASET_FRAMES / flat_name(name, lab["file"]))
        self.y, self.view, self.sid, self.t = map(np.array, (y, view, sid, t))
        self.files = files
        self.paths = paths
        self.sequences = [s for s, n in enumerate(self.names) if is_sequence(n)]
        self.display = [display_name(n) for n in self.names]  # what to print for each recording (its name, if it is on this machine)

    def __len__(self):
        return len(self.y)


_loaded = []


def load():
    if not _loaded:
        _loaded.append(Data())
    return _loaded[0]
