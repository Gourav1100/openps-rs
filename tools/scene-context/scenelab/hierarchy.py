"""The classification as a tree, the way it is asked:

  1. is it a game, a cutscene or a menu? (every frame that is not a menu or a cutscene is a game frame)
  2. if a game: is it 2D or 3D? (game_2d is 2D; on_foot, in_vehicle and game_3d are 3D)
  3. if 3D: on foot, in a vehicle (air, land or water), or a 3D game scene with some other in-game interaction (game_3d: talking, buying, ...)?

A cutscene, a menu and a 2D game are not looked at any further. The six scenes are menu, cutscene, on_foot, in_vehicle, game_3d and game_2d. Each
question has its own head (logistic regression on the same embeddings as the shipped model), trained only on the frames where that question is asked:
the first on every frame, the second on the games, the third on the 3D games. `leaf` reads a frame by going down the tree, taking the likeliest answer
at each step; `leaf_probs` multiplies the three heads' answers into a probability for each scene, for what needs one (a filter over time). `report`
scores the tree against one flat head over the six scenes, on recordings neither has seen. Nothing here is exported yet: the app still reads four
scenes (`scene.py train`).

A frame labeled in_game (a game, not yet told apart as 2D or 3D) teaches the first question only.
"""
import time
from pathlib import Path

import numpy as np
from sklearn.metrics import f1_score
from sklearn.model_selection import GroupKFold

from . import data as scene_data
from . import evaluate as scene_eval
from . import features
from .data import flat_name, frame_number, key, read_labels
from .paths import DATASET_FRAMES, FRAMES, MODELS

TOP = ["game", "cutscene", "menu"]
KIND = ["2d", "3d"]
ACTION = ["on_foot", "in_vehicle", "game_3d"]
LEAVES = ["on_foot", "in_vehicle", "game_3d", "game_2d", "menu", "cutscene"]
THREE_D = [LEAVES.index(a) for a in ACTION]
G2D, MENU, CUT = (LEAVES.index(s) for s in ("game_2d", "menu", "cutscene"))
GAME_ONLY = -1  # in_game: a game, 2D or 3D not told
FOLDS = 10


def top_of(leaf):
    """Index into TOP of a scene (an index into LEAVES, or GAME_ONLY): anything that is not a menu or a cutscene is a game."""
    return TOP.index("menu") if leaf == MENU else TOP.index("cutscene") if leaf == CUT else TOP.index("game")


def human():
    """The human-labeled frames the tree reads: (keys, paths, scene index into LEAVES or GAME_ONLY, recording)."""
    rows = [l for l in read_labels()["labels"]
            if (l["context"] in LEAVES or l["context"] == "in_game")
            and scene_data.keep_frame(l["recording"], frame_number(l["file"]))]
    keys = [key(l["recording"], l["file"]) for l in rows]
    paths = []
    for l in rows:
        flat = flat_name(l["recording"], l["file"])
        paths.append(DATASET_FRAMES / flat if (DATASET_FRAMES / flat).exists() else FRAMES / flat)
    return (keys, paths, np.array([LEAVES.index(l["context"]) if l["context"] in LEAVES else GAME_ONLY for l in rows]),
            np.array([l["recording"] for l in rows]))


def _head(c):
    return scene_eval._head(c)


class Tree:
    """The three heads."""

    def fit(self, X, y, c=0.1):
        self.top = _head(c).fit(X, np.array([top_of(k) for k in y]))
        kind = np.isin(y, [G2D, *THREE_D])  # in_game is left out: it does not say
        self.kind = _head(c).fit(X[kind], (y[kind] != G2D).astype(int)) if len(set(y[kind] == G2D)) == 2 else None
        three = np.isin(y, THREE_D)
        action = np.array([THREE_D.index(k) for k in y[three]])
        self.action = _head(c).fit(X[three], action) if len(action) and len(set(action)) > 1 else None
        self.only_action = int(action[0]) if len(action) and len(set(action)) == 1 else None
        return self

    def answers(self, X):
        """The three heads' probabilities for every frame: (top, kind, action), shaped (n, 3), (n, 2), (n, 3). A head with nothing to learn from
        (no 2D game labeled yet, say) answers with the one answer it knows."""
        Pt = scene_eval._proba(self.top, X, len(TOP))
        Pk = scene_eval._proba(self.kind, X, len(KIND)) if self.kind is not None else np.tile([0.0, 1.0], (len(X), 1))
        if self.action is not None:
            Pa = scene_eval._proba(self.action, X, len(ACTION))
        else:
            Pa = np.zeros((len(X), len(ACTION)))
            Pa[:, self.only_action or 0] = 1.0
        return Pt, Pk, Pa

    def leaf(self, X):
        """The scene of every frame, going down the tree and taking the likeliest answer at each step."""
        Pt, Pk, Pa = self.answers(X)
        out = np.empty(len(X), int)
        for i in range(len(X)):
            t = TOP[Pt[i].argmax()]
            if t != "game":
                out[i] = MENU if t == "menu" else CUT
            elif KIND[Pk[i].argmax()] == "2d":
                out[i] = G2D
            else:
                out[i] = THREE_D[Pa[i].argmax()]
        return out

    def leaf_probs(self, X):
        """A probability for each scene: the answers multiplied down the tree."""
        Pt, Pk, Pa = self.answers(X)
        P = np.zeros((len(X), len(LEAVES)))
        game = Pt[:, TOP.index("game")]
        P[:, G2D] = game * Pk[:, 0]
        for j, k in enumerate(THREE_D):
            P[:, k] = game * Pk[:, 1] * Pa[:, j]
        P[:, MENU], P[:, CUT] = Pt[:, TOP.index("menu")], Pt[:, TOP.index("cutscene")]
        return P


def report(model_dir=MODELS, c=0.1):
    keys, paths, y, rec = human()
    X = features.vectors(list(zip(keys, paths)), model_dir)
    names = [*LEAVES, "in_game"]
    print(f"{len(y)} labeled frames in {len(set(rec))} recordings: "
          + ", ".join(f"{names[i]} {(y == i).sum()}" for i in [*range(len(LEAVES)), GAME_ONLY] if (y == i).any() or i >= 0))
    started = time.time()
    tree_pred, prod_pred, flat_pred = (np.zeros(len(y), int) for _ in range(3))
    top_ok, kind_p, action_p = np.zeros(len(y), bool), np.zeros(len(y), int), np.zeros(len(y), int)
    sure = y != GAME_ONLY  # the flat head learns from, and is scored on, frames that name a scene
    for n, (train, test) in enumerate(GroupKFold(FOLDS).split(X, y, rec), 1):
        tree = Tree().fit(X[train], y[train], c)
        tree_pred[test] = tree.leaf(X[test])
        prod_pred[test] = tree.leaf_probs(X[test]).argmax(1)
        Pt, Pk, Pa = tree.answers(X[test])
        top_ok[test] = Pt.argmax(1) == np.array([top_of(k) for k in y[test]])
        kind_p[test], action_p[test] = Pk.argmax(1), Pa.argmax(1)
        flat = _head(c).fit(X[train][sure[train]], y[train][sure[train]])
        flat_pred[test] = scene_eval._proba(flat, X[test], len(LEAVES)).argmax(1)
        print(f"  fold {n}/{FOLDS} [{time.time() - started:.0f} s]", flush=True)

    print("\nEach question on its own (recordings unseen):")
    print(f"  game / cutscene / menu          {top_ok.mean():.1%} of all {len(y)} frames")
    kind = np.isin(y, [G2D, *THREE_D])
    print(f"  2D or 3D (of the games)         {(kind_p[kind] == (y[kind] != G2D)).mean():.1%} of {kind.sum()} frames"
          + ("" if (y == G2D).any() else "  (no game_2d labeled yet: every game reads as 3D)"))
    three = np.isin(y, THREE_D)
    truth = np.array([THREE_D.index(k) for k in y[three]])
    print(f"  on foot / vehicle / game_3d (of the 3D games)  {(action_p[three] == truth).mean():.1%} of {three.sum()} frames"
          + ("" if (y == LEAVES.index("game_3d")).any() else "  (no game_3d labeled yet)"))

    print("\nThe six scenes, end to end (frames that name a scene):")
    print(f"  {'':28}{'accuracy':>9}{'macro-F1':>9}   recall: " + " ".join(s[:8].rjust(8) for s in LEAVES))
    present = [i for i in range(len(LEAVES)) if (y == i).any()]
    for name, p in (("tree, down the answers", tree_pred), ("tree, answers multiplied", prod_pred), ("one flat head", flat_pred)):
        rec_by = " ".join((f"{(p[y == i] == i).mean():8.2f}" if (y == i).any() else "       -") for i in range(len(LEAVES)))
        print(f"  {name:28}{(p[sure] == y[sure]).mean():9.3f}{f1_score(y[sure], p[sure], labels=present, average='macro'):9.3f}   {rec_by}")
    shown = scene_data.display_names()
    ui = np.array([shown.get(r, r).startswith(("shots", "clips")) for r in rec])
    print("\nBy kind of recording (accuracy, tree down the answers / flat):")
    for tag, m in (("gameplay and captures", ~ui & sure), ("UI screenshots and clips", ui & sure)):
        print(f"  {tag:28}{(tree_pred[m] == y[m]).mean():.3f} / {(flat_pred[m] == y[m]).mean():.3f}   ({m.sum()} frames)")
    if (y == GAME_ONLY).any():
        print(f"\n{(y == GAME_ONLY).sum()} frames are labeled in_game: they teach 'game' but not 2D or 3D; remap them to 5 (game_2d), 6 (game_3d) or "
              "on foot / in a vehicle (show: in_game: 2D or 3D?).")
