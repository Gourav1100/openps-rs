"""Labeling: a page, served from this machine, in which frames are labeled; every change it makes goes straight into dataset/labels.json.

`serve` builds the page from every extracted frame that has no label (all recordings at once; the page shows one set at a time) and answers its
saves with `apply_changes`. A frame deleted in the page is recorded in labels.json ("deleted") and left out of everything after. `import_export`
merges a labels file the page's Export button wrote, for when the server was not running. The labeled frames are kept sorted by what they show.
"""
import json
import os
import re
import secrets
import shutil
import threading
import time
import webbrowser
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import unquote, urlsplit

from . import data as scene_data
from .data import LABEL_CLASSES, STATES, VIEWS, flat_name, frame_number, key, read_index, read_labels
from .paths import DATASET, DATASET_FRAMES, FRAMES, LABELS, SUGGESTIONS, TEMPLATES

BUCKETS = {"on_foot": "on_foot", "in_vehicle": "in_vehicle",
           "menu": "in_menu", "cutscene": "in_menu", "menu_or_cutscene": "in_menu",  # the last is an older, unsplit label
           "in_game": "in_game", "game_2d": "game_2d", "game_3d": "game_3d"}
FOLDERS = [*VIEWS, "on_foot", "in_vehicle", "in_menu", "in_game", "game_2d", "game_3d"]
MAX_SAVE = 20_000_000
LOCK = threading.Lock()


def _link(src, dst):
    """Point dst at src (relative, so the folder can move)."""
    dst.parent.mkdir(parents=True, exist_ok=True)
    if dst.is_symlink() or dst.exists():
        dst.unlink()
    os.symlink(os.path.relpath(src, dst.parent), dst)


def write_labels(doc):
    """One label per line, so a change shows up as a small diff; replaced in one step."""
    rows = ",\n".join(json.dumps(l) for l in doc["labels"])
    text = f'{{"labels": [\n{rows}\n],\n"deleted": {json.dumps(doc["deleted"])}}}\n'
    LABELS.parent.mkdir(parents=True, exist_ok=True)
    tmp = LABELS.with_name(LABELS.name + ".tmp")
    tmp.write_text(text)
    os.replace(tmp, LABELS)


def build(which="all", only=None, quiet=False):
    """The labeling page's content: {build, items} for the frames to show, or None when there are none. `which`: "all" = every frame, labels prefilled
    (labeled sets can be revisited), "new" = only frames without a label, "suggested" = the unlabeled frames `scene.py suggest` picked, as one set,
    "menus" = frames still labeled menu_or_cutscene, to be split into menu and cutscene. `only`: recordings whose name or id starts with one of these.
    Deleted frames are never shown."""
    doc = read_labels()
    labels = {key(l["recording"], l["file"]): l for l in doc["labels"]}
    deleted = set(doc["deleted"])
    index = read_index()
    names = scene_data.display_names()
    items = []

    def candidates():
        if which == "suggested":  # one set, best first (scene.py suggest wrote the list)
            sug = scene_data.read_json(SUGGESTIONS, None)
            if not sug:
                raise SystemExit("no suggestions yet: run `./scene.sh suggest` first")
            where = {key(n, f["file"]): (n, f) for n in index for f in index[n]["frames"]}
            for s in sug["frames"]:
                if s["key"] in where:
                    yield (*where[s["key"]], "suggested")
            return
        for name in scene_data.extracted_recordings():
            if scene_data.matches(name, only):
                for f in index[name]["frames"]:
                    yield name, f, name

    for name, f, set_name in candidates():
        k = key(name, f["file"])
        flat = flat_name(name, f["file"])
        lab = labels.get(k) or {}
        if k in deleted or not (FRAMES / flat).exists():
            continue
        if which in ("new", "suggested") and lab.get("context"):
            continue
        if which == "all" and not scene_data.keep_frame(name, frame_number(f["file"])):
            continue
        if which == "menus":
            if lab.get("context") != "menu_or_cutscene":
                continue
            # the cutscene video is a head start; everything else is decided by hand
            lab = {"context": "cutscene" if names.get(name) == "mp4-cutscenes_gta_v" else None, "view": None}
        items.append({"key": k, "session": name, "rid": name, "rname": names.get(name, name), "set": set_name, "file": f["file"],
                      "src": f"/frames/{flat}", "t": f["t"], "context": lab.get("context"), "view": lab.get("view")})
    if not items:
        if not quiet:
            print({"new": "every extracted frame already has a label", "menus": "no frame is labeled menu_or_cutscene any more",
                   "suggested": "every suggested frame is labeled; run `./scene.sh suggest` again"}.get(which, "no frames"))
        return None
    per = {}
    for it in items:
        per[it["set"]] = per.get(it["set"], 0) + 1
    if not quiet:
        print(f"{len(items)} frames in {len(per)} sets, {sum(not it['context'] for it in items)} without a context: "
              + ", ".join(f"{names.get(r, r)} {n}" for r, n in list(per.items())[:6]) + (" ..." if len(per) > 6 else ""))
    return {"build": str(int(time.time())), "items": items}


def _keep_picture(item):
    dst, src = DATASET_FRAMES / flat_name(item["session"], item["file"]), FRAMES / flat_name(item["session"], item["file"])
    if not dst.exists() and src.exists():
        dst.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(src, dst)


def _drop_picture(item):
    (DATASET_FRAMES / flat_name(item["session"], item["file"])).unlink(missing_ok=True)


def apply_changes(changes, known):
    """Merge what the page changed into labels.json; returns how many changes took. `changes`: [{key, context, view, deleted}]; `known`: {key: {session,
    file, t}} of the frames the page was built from, so a change to any other key is ignored. A context makes a label (with its view, if any), no context
    clears it, deleted removes it and keeps the frame out of everything from then on."""
    with LOCK:
        doc = read_labels()
        by_key = {key(l["recording"], l["file"]): l for l in doc["labels"]}
        deleted = set(doc["deleted"])
        taken = 0
        for ch in changes:
            k = ch.get("key")
            item = known.get(k) if isinstance(k, str) else None
            if not item:
                continue
            context, view = ch.get("context"), ch.get("view")
            if ch.get("deleted") is True:
                by_key.pop(k, None)
                deleted.add(k)
                _drop_picture(item)
            elif context in LABEL_CLASSES:
                by_key[k] = {"recording": item["session"], "file": item["file"], "t": item["t"], "context": context,
                             "view": view if view in VIEWS else None}
                deleted.discard(k)
                _keep_picture(item)
            elif context is None:
                by_key.pop(k, None)
                deleted.discard(k)
                _drop_picture(item)
            else:
                continue
            taken += 1
        if taken:
            write_labels({"labels": sorted(by_key.values(), key=lambda l: (l["recording"], l["file"])), "deleted": sorted(deleted)})
    return taken


def serve(which="all", only=None, open_page=True):
    """Build the page and serve it on this machine only until Ctrl-C; the page saves each change here (apply_changes)."""
    page = build(which, only)
    if not page:
        return 0
    known = {it["key"]: it for it in page["items"]}
    token = secrets.token_urlsafe(16)
    template = (TEMPLATES / "labeler.html").read_text()

    def page_html():  # built again on each load, so a reload shows what is saved already
        current = build(which, only, quiet=True)
        if not current:
            return b"<!doctype html><body style='font:16px system-ui;padding:40px'>Nothing left to label.</body>"
        known.update({it["key"]: it for it in current["items"]})
        return (template.replace("__DATA__", json.dumps(current).replace("</", "<\\/"))
                .replace("__TOKEN__", json.dumps(token))).encode()

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def _send(self, code, body=b"", ctype="text/plain", cache="no-store"):
            self.send_response(code)
            self.send_header("Content-Type", ctype)
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Cache-Control", cache)
            self.end_headers()
            self.wfile.write(body)

        def _local(self):  # a page on another site, or another host name for this one, may not talk to it
            port = self.server.server_address[1]
            return self.headers.get("Host") in (f"127.0.0.1:{port}", f"localhost:{port}")

        def do_GET(self):
            if not self._local():
                return self._send(403)
            path = urlsplit(self.path).path
            if path == "/":
                return self._send(200, page_html(), "text/html; charset=utf-8")
            name = unquote(path[len("/frames/"):]) if path.startswith("/frames/") else ""
            if re.fullmatch(r"[A-Za-z0-9_.-]+\.jpg", name) and (FRAMES / name).is_file():
                return self._send(200, (FRAMES / name).read_bytes(), "image/jpeg", "max-age=3600")
            self._send(404)

        def do_POST(self):
            if self.path != "/save" or not self._local() or not secrets.compare_digest(self.headers.get("X-Token", ""), token):
                return self._send(403)
            size = int(self.headers.get("Content-Length") or 0)
            if not 0 < size <= MAX_SAVE:
                return self._send(413)
            try:
                changes = json.loads(self.rfile.read(size))["changes"]
                if not isinstance(changes, list):
                    raise ValueError
            except (ValueError, KeyError, TypeError):
                return self._send(400)
            self._send(200, json.dumps({"saved": apply_changes(changes, known)}).encode(), "application/json")

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    url = f"http://127.0.0.1:{server.server_address[1]}/"
    print(f"labeling page at {url} (every change is saved to {LABELS.relative_to(DATASET.parent)}); Ctrl-C when done")
    if open_page:
        webbrowser.open(url)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()
    print("stopped")
    status()
    return len(page["items"])


def latest_export():
    """The newest labels-dataset-*.json in ~/Downloads, where the page's Export button puts it."""
    found = sorted((Path.home() / "Downloads").glob("labels-dataset-*.json"), key=lambda p: p.stat().st_mtime)
    return found[-1] if found else None


def import_export(path=None):
    path = Path(path).expanduser() if path else latest_export()
    if not path or not path.is_file():
        raise SystemExit("no export to import: press Export in the labeling page, or pass the file")
    items = json.loads(path.read_text())["items"]
    known = {it["key"]: it for it in items}
    changes = [{"key": it["key"], "context": it.get("context"), "view": it.get("view"), "deleted": bool(it.get("deleted"))}
               for it in items if it.get("context") or it.get("deleted")]
    taken = apply_changes(changes, known)
    print(f"{path.name}: {taken} labels merged ({len(items) - taken} items had none)")
    sort()


def sync_frames():
    """Copy every labeled frame that is not in dataset/frames yet from data/frames: the dataset keeps the labeled frames, not the rest."""
    copied = 0
    for lab in read_labels()["labels"]:
        flat = flat_name(lab["recording"], lab["file"])
        if not (DATASET_FRAMES / flat).exists() and (FRAMES / flat).exists():
            DATASET_FRAMES.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(FRAMES / flat, DATASET_FRAMES / flat)
            copied += 1
    if copied:
        print(f"copied {copied} labeled frames into {DATASET_FRAMES}")


def sort():
    """Rebuild dataset/sorted/: first_person, third_person, on_foot, in_vehicle and in_menu (menus and cutscenes), each a folder of links to the
    frames; a frame with a view is in its view's folder as well as its state's. Nothing is copied."""
    sync_frames()
    top = DATASET / "sorted"
    shutil.rmtree(top, ignore_errors=True)
    for f in FOLDERS:
        (top / f).mkdir(parents=True)
    manifest, counts = {}, dict.fromkeys(FOLDERS, 0)
    for lab in read_labels()["labels"]:
        flat = flat_name(lab["recording"], lab["file"])
        picked = [BUCKETS[lab["context"]]] + ([lab["view"]] if lab.get("view") in VIEWS else [])
        for f in picked:
            _link(DATASET_FRAMES / flat, top / f / flat)
            counts[f] += 1
            manifest[f"{f}/{flat}"] = {"recording": lab["recording"], "file": lab["file"], "context": lab["context"], "view": lab.get("view")}
    (top / "manifest.json").write_text(json.dumps(manifest, indent=1))
    print("sorted: " + ", ".join(f"{f} {counts[f]}" for f in FOLDERS) + f" -> {top}")


def status():
    doc = read_labels()
    done = {key(l["recording"], l["file"]) for l in doc["labels"]} | set(doc["deleted"])
    left = {}
    for name, entry in read_index().items():
        n = sum(1 for f in entry["frames"] if key(name, f["file"]) not in done)
        if n:
            left[name] = n
    data = scene_data.Data()
    print(f"{len(data)} labeled frames in {len(data.names)} recordings, {len(doc['deleted'])} deleted")
    for k, state in enumerate(STATES):
        print(f"  {state:14}{(data.y == k).sum():6}")
    for name in scene_data.OTHER:
        print(f"  {name:14}{sum(l['context'] == name for l in doc['labels']):6}  (for the tree, not the shipped model)")
    for v, view in enumerate(VIEWS):
        print(f"  {view:14}{(data.view == v).sum():6}")
    print(f"without a label: {sum(left.values())} frames in {len(left)} recordings" + (f" ({', '.join(f'{n} {scene_data.display_name(r)}' for r, n in list(left.items())[:5])}{' ...' if len(left) > 5 else ''})" if left else ""))
