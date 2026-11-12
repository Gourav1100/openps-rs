#!/usr/bin/env python3
"""The scene reader's training tools, one command.

Everyday use is two scripts: `./label.sh` (extract new recordings, serve the labeling page) and `./train.sh` (train on what is labeled, write the
models into assets/models). The rest of the steps, each on its own:

  scene.py extract SOURCE [--step S]   a capture (.opsvid) or a video file into frames in data/
  scene.py label [SOURCE ...]          extract the given recordings (by default the captures in ~/openps-captures that have not been), then serve the
                                       labeling page on this machine: every recording, one set at a time (labeled ones too, to revisit), each change saved
                                       into dataset/labels.json (--new: only unlabeled frames, --menus: split menu / cutscene, --only PREFIX: just those
                                       recordings, --no-open: do not open the browser)
  scene.py ingest FOLDER               a folder of screenshots and clips into shots-NNN / clips-NNN recordings (and one per long clip) to label
  scene.py tree                        score the scenes read as a tree: game / cutscene / menu, then 2D / 3D, then on foot / vehicle / game_3d
  scene.py suggest [-n N] [--only P]   which unlabeled frames to label next: those the model is least sure about or that look new to it
                                       (`label --suggested` shows them as one set, best first)
  scene.py import [FILE]               merge a labels file the page's Export button wrote (the newest labels-dataset-*.json in ~/Downloads)
  scene.py sort                        dataset/sorted/: first_person, third_person, on_foot, in_vehicle, in_menu
  scene.py status                      what is labeled, what is not
  scene.py train                       features for new frames, probe, models (converted to Core ML only when needed), Rust test files, report
  scene.py eval                        how recordings the head has not seen are read, with and without the filter
  scene.py check                       the exported models and probe against the cached features
  scene.py fixtures                    rewrite the files the Rust tests compare against, from the installed models
"""
import argparse
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from scenelab.paths import CAPTURES, MODELS  # noqa: E402


def cmd_extract(a):
    from scenelab import extract

    extract.extract(a.source, a.step, a.name)


def pending_captures():
    """Captures in ~/openps-captures that no recording in data/ was made from."""
    from scenelab.data import read_index

    have = {e.get("name") for e in read_index().values()}
    return sorted(p for p in CAPTURES.glob("*.opsvid") if p.stem not in have)


def cmd_ingest(a):
    from scenelab import ingest

    ingest.ingest(a.folder, a.per_clip, a.clip_step, chunk=a.chunk, force=a.force)


def cmd_tree(a):
    from scenelab import hierarchy

    hierarchy.report(a.model_dir, a.c)


def cmd_suggest(a):
    from scenelab import suggest

    suggest.suggest(a.n, a.only, a.model_dir)


def cmd_label(a):
    from scenelab import extract, labeling

    sources = a.source or pending_captures()
    for s in sources:
        extract.extract(s)
    which = "menus" if a.menus else "suggested" if a.suggested else "new" if a.new else "all"
    labeling.serve(which, a.only, open_page=not a.no_open)


def cmd_import(a):
    from scenelab import labeling
    from scenelab.paths import LABELS

    if a.if_new:
        latest = labeling.latest_export()
        if not latest or (LABELS.exists() and latest.stat().st_mtime <= LABELS.stat().st_mtime):
            return print("no new label export")
        a.file = latest
    labeling.import_export(a.file)


def cmd_sort(_):
    from scenelab import labeling

    labeling.sort()


def cmd_status(_):
    from scenelab import labeling

    labeling.status()
    print(f"captures not extracted yet: {len(pending_captures())}")


def cmd_train(a):
    from scenelab import export

    export.train(a.out, a.reexport, a.c, a.zs, check=not a.no_check)
    print("\nTo use them in the app: scripts/install.sh")


def cmd_eval(a):
    from scenelab import evaluate

    evaluate.report(a.model_dir, a.c, a.zs, a.seeds)


def cmd_check(a):
    from scenelab import evaluate

    evaluate.check(a.model_dir)


def cmd_fixtures(a):
    from scenelab import evaluate, fixture

    fixture.write(a.model_dir)
    evaluate.write_sequences(a.model_dir)


def main(argv=None):
    from scenelab.pictures import HEAD_C, ZERO_SHOT_WEIGHT

    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="command", required=True, metavar="command")

    def add(name, fn, help):
        sp = sub.add_parser(name, help=help)
        sp.set_defaults(fn=fn)
        return sp

    s = add("extract", cmd_extract, "sample a recording into frames")
    s.add_argument("source", help=".opsvid capture or video file")
    s.add_argument("--step", type=float, help="seconds between frames (2 for captures, 6 for video files)")
    s.add_argument("--name", help="the recording's name (default: made from the file's name; needed when that name is taken by another file)")
    s = add("label", cmd_label, "extract new recordings and serve the labeling page")
    s.add_argument("source", nargs="*", help="recordings to extract first (default: the captures not extracted yet)")
    s.add_argument("--suggested", action="store_true", help="only the frames `scene.py suggest` picked, best first, as one set")
    s.add_argument("--new", action="store_true", help="only the frames without a label (the default is every frame, labels prefilled)")
    s.add_argument("--all", action="store_true", help="every frame (the default; kept for old habits)")
    s.add_argument("--menus", action="store_true", help="only frames still labeled menu_or_cutscene")
    s.add_argument("--no-open", action="store_true", help="serve the page without opening the browser")
    s.add_argument("--only", action="append", metavar="PREFIX", help="only recordings whose name starts with this (repeatable), e.g. shots-001")
    s = add("tree", cmd_tree, "read the scenes as a tree (game / cutscene / menu, then 2D / 3D, then on foot / vehicle / game_3d) and score it")
    s.add_argument("--c", type=float, default=HEAD_C, help=f"head regularisation (default {HEAD_C})")
    s.add_argument("--model-dir", default=MODELS, help="where the models are (default: assets/models)")
    s = add("suggest", cmd_suggest, "which unlabeled frames to label next")
    s.add_argument("-n", type=int, default=500, help="how many frames to suggest")
    s.add_argument("--only", action="append", metavar="PREFIX", help="choose among the recordings whose name starts with this (repeatable)")
    s.add_argument("--model-dir", default=MODELS, help="where the model is (default: assets/models)")
    s = add("ingest", cmd_ingest, "a folder of screenshots and clips into recordings to label")
    s.add_argument("folder")
    s.add_argument("--per-clip", type=int, default=8, help="most frames from one short clip")
    s.add_argument("--clip-step", type=float, default=2.0, help="seconds between frames of a short clip")
    s.add_argument("--chunk", type=int, default=1000, help="frames per shots-NNN / clips-NNN recording")
    s.add_argument("--force", action="store_true", help="import recordings that already exist again")
    s = add("import", cmd_import, "merge a labels export")
    s.add_argument("file", nargs="?", help="default: the newest labels-dataset-*.json in ~/Downloads")
    s.add_argument("--if-new", action="store_true", help="only if that export is newer than the labels already merged")
    add("sort", cmd_sort, "sort the frames into first_person, third_person, on_foot, in_vehicle, in_menu")
    add("status", cmd_status, "what is labeled and what is not")
    s = add("train", cmd_train, "train and write the models")
    s.add_argument("--out", default=MODELS, help="where the models go (default: assets/models)")
    s.add_argument("--reexport", action="store_true", help="convert the models to Core ML again")
    s.add_argument("--no-check", action="store_true", help="skip the report and the check")
    for sp in (s, add("eval", cmd_eval, "read recordings with a head that has not seen them")):
        sp.add_argument("--c", type=float, default=HEAD_C, help=f"head regularisation (default {HEAD_C})")
        sp.add_argument("--zs", type=float, default=ZERO_SHOT_WEIGHT, help=f"weight of the zero-shot reading (default {ZERO_SHOT_WEIGHT})")
    sub.choices["eval"].add_argument("--seeds", type=int, default=10, help="sets of simulated presses for the replay")
    sub.choices["eval"].add_argument("--model-dir", default=MODELS, help="where scene-probe.json is (default: assets/models)")
    s = add("check", cmd_check, "the exported models against the cached features")
    s.add_argument("--model-dir", default=MODELS)
    s = add("fixtures", cmd_fixtures, "rewrite the Rust tests' reference files")
    s.add_argument("--model-dir", default=MODELS)
    a = p.parse_args(argv)
    a.fn(a)


if __name__ == "__main__":
    main()
