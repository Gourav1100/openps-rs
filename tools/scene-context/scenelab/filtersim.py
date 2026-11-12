"""The scene filter in Python, and the replay that scores it: per-frame probabilities plus simulated presses in, how often the scene is right out.

Context states: on_foot, in_vehicle (land, air and water), menu, cutscene. View: first_person / third_person.
Actions (what the key map exposes instead of raw buttons):
  key_enter_or_exit_vehicle  makes on_foot <-> in_vehicle likely for a few seconds
  key_pause_game             makes game <-> menu likely for a few seconds (cutscenes start and end without a press)
  key_change_camera          makes first <-> third person likely for a few seconds
With no action the state is held. A forward (HMM) filter takes the frame classifier as the observation.

The captures contain no controller log, so actions are SIMULATED from the label changes (missed actions and unrelated actions mixed in).
The recordings it is replayed on are the sequences of data.py (console captures and continuous gameplay). The Rust filter (crates/openps-scene) is
the one the app runs; tests/reference.rs checks it against this one step for step.
"""
import warnings

import numpy as np
from sklearn.metrics import f1_score, recall_score

from . import data as scene_data

warnings.filterwarnings("ignore")
data = scene_data.load()
STATES, VIEWS = scene_data.STATES, scene_data.VIEWS
FOOT, VEH, MENU, CUT = scene_data.FOOT, scene_data.VEH, scene_data.MENU, scene_data.CUT
SESSIONS = data.names
SEQ = data.sequences
ALPHA = 0.7  # frames are correlated, so the classifier is trusted less than its stated confidence
y, view, sid, tt = data.y, data.view, data.sid, data.t
n = len(data)


def hazard_step(belief, h_all):
    """Move belief by the accumulated hazard h_all[i, j] (rate times seconds) of going i -> j."""
    out = np.zeros(len(belief))
    for i in range(len(belief)):
        h = h_all[i].copy()
        h[i] = 0
        total = h.sum()
        stay = np.exp(-total)
        out[i] += belief[i] * stay
        if total > 0:
            out += belief[i] * (1 - stay) * h / total
    return out


CUE_LEN = {"key_enter_or_exit_vehicle": 5.0, "key_pause_game": 5.0, "key_change_camera": 3.0}


def armed(actions, name, t0, t1):
    """Seconds of [t0, t1] covered by the cue windows that follow `name` actions."""
    iv = sorted((p, p + CUE_LEN[name]) for p, nm in actions if nm == name and p < t1 and p + CUE_LEN[name] > t0)
    total, end = 0.0, t0
    for a, b in iv:
        a, b = max(a, end), min(b, t1)
        if b > a:
            total += b - a
            end = b
    return total


def filter_context(pc, t, actions, base_rate, cue_rate):
    """cue_rate == 0 gives a plain smoother where every change is equally unlikely."""
    belief = np.full(4, 0.25)
    out = np.zeros((len(t), 4))
    for i in range(len(t)):
        if i:
            dt = t[i] - t[i - 1]
            h = np.full((4, 4), base_rate * dt)
            if cue_rate:
                # cutscenes begin and end on their own, so they get their own rates
                for g in (FOOT, VEH, MENU):
                    h[g, CUT] = 0.01 * dt
                    h[CUT, g] = 0.03 * dt
                h[MENU, CUT] = 0.003 * dt
                veh = armed(actions, "key_enter_or_exit_vehicle", t[i - 1], t[i])
                pause = armed(actions, "key_pause_game", t[i - 1], t[i])
                h[FOOT, VEH] += cue_rate * veh
                h[VEH, FOOT] += cue_rate * veh
                for g in (FOOT, VEH):
                    h[g, MENU] += cue_rate * pause
                    h[MENU, g] += cue_rate * pause
            belief = hazard_step(belief, h)
        belief = belief * np.maximum(pc[i], 1e-6) ** ALPHA
        belief /= belief.sum()
        out[i] = belief
    return out


def filter_view(pv, t, actions, ctx_pred, base_rate, cue_rate):
    belief = np.full(2, 0.5)
    out = np.zeros((len(t), 2))
    for i in range(len(t)):
        if i:
            dt = t[i] - t[i - 1]
            h = np.full((2, 2), base_rate * dt)
            if cue_rate:
                cam = armed(actions, "key_change_camera", t[i - 1], t[i])
                veh = armed(actions, "key_enter_or_exit_vehicle", t[i - 1], t[i])
                h += cue_rate * cam + 0.05 * veh  # the game keeps a separate camera for foot and vehicle
            belief = hazard_step(belief, h)
        if ctx_pred[i] in (FOOT, VEH):
            belief = belief * np.maximum(pv[i], 1e-6) ** ALPHA
            belief /= belief.sum()
        out[i] = belief
    return out


def simulate_actions(ctx, vw, t, rng, distract):
    ev = []
    for i in range(1, len(t)):
        lo, hi = t[i - 1], t[i]
        dt = hi - lo
        when = lambda: hi - rng.uniform(0.3, min(dt, 2.5))
        a, b = ctx[i - 1], ctx[i]
        if a != b:
            if {a, b} == {FOOT, VEH}:
                if rng.random() < 0.9:
                    ev.append((when(), "key_enter_or_exit_vehicle"))
            elif MENU in (a, b) and CUT not in (a, b):
                if rng.random() < (0.9 if b == MENU else 0.7):  # some menus close with another button
                    ev.append((when(), "key_pause_game"))
        if vw[i] >= 0 and vw[i - 1] >= 0 and vw[i] != vw[i - 1]:
            if rng.random() < (0.9 if b == a else 0.4):
                ev.append((when(), "key_change_camera"))
        game = b in (FOOT, VEH)
        # the enter/exit key does nothing but leave in a vehicle; unrelated presses of it happen on foot and in menus
        rates = [("key_enter_or_exit_vehicle", 1 / 40 if b == FOOT else 1 / 6 if b == MENU else 0.0),
                 ("key_pause_game", 1 / 300 if game else 1 / 6 if b == MENU else 1 / 30),
                 ("key_change_camera", 1 / 90 if game else 0.0)]
        for name, rate in rates:
            for _ in range(rng.poisson(rate * distract * dt)):
                ev.append((rng.uniform(lo, hi), name))
    return sorted(ev)


def switches(a):
    return int((a[1:] != a[:-1]).sum())


def delays(truth, pred):
    """Frames until the prediction follows each true change (capped at the next change)."""
    out = []
    for k in np.flatnonzero(truth[1:] != truth[:-1]) + 1:
        end = k + 1
        while end < len(truth) and truth[end] == truth[k]:
            end += 1
        hit = np.flatnonzero(pred[k:end] == truth[k])
        out.append(hit[0] if len(hit) else end - k)
    return out


def rec(truth, pred):
    r = recall_score(truth, pred, labels=range(4), average=None, zero_division=0)
    return " ".join(f"{nm[:6]}={v:.2f}" for nm, v in zip(STATES, r))


def evaluate(oof, distract=1.0, seeds=20, base_keys=0.003):
    pc, pv = oof
    names = ["frame", "smooth", "keys"]
    res = {m: {"acc": [], "f1": [], "view": [], "sw": [], "dl": [], "t": [], "p": []} for m in names}
    for seed in range(seeds):
        rng = np.random.default_rng(seed)
        parts = {m: {"t": [], "p": [], "vt": [], "vp": [], "sw": 0, "n": 0, "dl": []} for m in names}
        for s in SEQ:
            m_ = sid == s
            ctx, vw, t, c_, v_ = y[m_], view[m_], tt[m_], pc[m_], pv[m_]
            acts = simulate_actions(ctx, vw, t, rng, distract)
            fs = filter_context(c_, t, [], 0.02, 0)
            fk = filter_context(c_, t, acts, base_keys, 0.6)
            variants = {"frame": (c_.argmax(1), v_.argmax(1)),
                        "smooth": (fs.argmax(1), filter_view(v_, t, [], fs.argmax(1), 0.02, 0).argmax(1)),
                        "keys": (fk.argmax(1), filter_view(v_, t, acts, fk.argmax(1), 0.002, 1.0).argmax(1))}
            vm = vw >= 0
            for name, (cp, vpred) in variants.items():
                r = parts[name]
                r["t"].append(ctx); r["p"].append(cp)
                r["vt"].append(vw[vm]); r["vp"].append(vpred[vm])
                r["sw"] += switches(cp); r["n"] += len(cp); r["dl"].extend(delays(ctx, cp))
        for name, r in parts.items():
            ct, cp = np.concatenate(r["t"]), np.concatenate(r["p"])
            res[name]["acc"].append((ct == cp).mean())
            res[name]["f1"].append(f1_score(ct, cp, labels=range(4), average="macro", zero_division=0))
            res[name]["view"].append((np.concatenate(r["vt"]) == np.concatenate(r["vp"])).mean())
            res[name]["sw"].append(r["sw"] / r["n"] * 100)
            res[name]["dl"].append(np.mean(r["dl"]))
            res[name]["t"].append(ct); res[name]["p"].append(cp)
    return res


def show(title, res, true_sw):
    print(f"\n  {title}  (true context changes per 100 frames: {true_sw:.1f})")
    print(f"  {'':8}{'ctx acc':>9}{'ctx F1':>8}{'view acc':>10}{'switches/100':>14}{'delay(frames)':>15}   recall")
    for name, r in res.items():
        print(f"  {name:8}{np.mean(r['acc']):9.3f}{np.mean(r['f1']):8.3f}{np.mean(r['view']):10.3f}"
              f"{np.mean(r['sw']):14.1f}{np.mean(r['dl']):15.2f}   {rec(np.concatenate(r['t']), np.concatenate(r['p']))}")
