//! The Rust filter over real per-frame probabilities (frames the model had not seen) with simulated actions: how often is the scene right, and
//! what happens to on-foot frames that look like a vehicle? Needs `tools/scene-context/cache/sequences.json` (`scene.py train` or `scene.py fixtures` writes it); skipped
//! without it.

use openps_input::scene::Action;
use openps_scene::filter::{Params, SceneFilter};
use serde_json::Value;
use std::path::Path;

fn load() -> Option<Value> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/scene-context/cache/sequences.json");
    serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()
}

fn floats(v: &Value) -> Vec<f32> {
    v.as_array().unwrap().iter().map(|x| x.as_f64().unwrap() as f32).collect()
}

#[derive(Default, Debug)]
struct Score {
    right: usize,
    n: usize,
    per_state: [[usize; 2]; 4],
    /// what the filter said, by what it was (rows: truth, columns: on foot, vehicle, menu, cutscene), and the same for the model alone
    confusion: [[usize; 4]; 4],
    model_confusion: [[usize; 4]; 4],
    /// on-foot frames that the model alone calls a vehicle, and how many of those the filter gets right
    foot_looks_like_vehicle: [usize; 2],
    /// in-vehicle frames the model alone calls on foot, and how many of those the filter gets right
    vehicle_looks_like_foot: [usize; 2],
    switches: usize,
}

fn replay(doc: &Value, params: &Params) -> Score {
    let mut s = Score::default();
    for seed in 0..5 {
        for sess in doc["sessions"].as_array().unwrap() {
            let t = floats(&sess["t"]);
            let truth: Vec<usize> =
                sess["truth"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap() as usize).collect();
            let mut filter = SceneFilter::new(params.clone());
            for a in Action::ALL {
                filter.set_available(a, true);
            }
            let mut actions: Vec<(f64, Action)> = sess["actions"][seed]
                .as_array()
                .unwrap()
                .iter()
                .map(|a| (a[0].as_f64().unwrap(), Action::parse(a[1].as_str().unwrap()).unwrap()))
                .collect();
            actions.sort_by(|a, b| a.0.total_cmp(&b.0));
            let mut next = 0;
            let mut last = usize::MAX;
            for i in 0..t.len() {
                let ti = f64::from(t[i]);
                while next < actions.len() && actions[next].0 <= ti {
                    filter.push_action(actions[next].0, actions[next].1);
                    next += 1;
                }
                let c = floats(&sess["context"][i]);
                let v = floats(&sess["view"][i]);
                let e = filter.update(ti, [c[0], c[1], c[2], c[3]], [v[0], v[1]]);
                let pred = e.context as usize;
                let model = (0..4).fold(0, |b, k| if c[k] > c[b] { k } else { b });
                s.n += 1;
                s.per_state[truth[i]][1] += 1;
                s.confusion[truth[i]][pred] += 1;
                s.model_confusion[truth[i]][model] += 1;
                if pred == truth[i] {
                    s.right += 1;
                    s.per_state[truth[i]][0] += 1;
                }
                if truth[i] == 0 && model == 1 {
                    s.foot_looks_like_vehicle[1] += 1;
                    s.foot_looks_like_vehicle[0] += usize::from(pred == 0);
                }
                if truth[i] == 1 && model == 0 {
                    s.vehicle_looks_like_foot[1] += 1;
                    s.vehicle_looks_like_foot[0] += usize::from(pred == 1);
                }
                if last != usize::MAX && pred != last {
                    s.switches += 1;
                }
                last = pred;
            }
        }
    }
    s
}

#[test]
#[ignore = "a parameter sweep that prints a table"]
fn sweep_the_vehicle_parameters() {
    let Some(doc) = load() else { return };
    let pct = |a: usize, b: usize| 100.0 * a as f64 / b.max(1) as f64;
    let show = |name: String, s: &Score| {
        eprintln!(
            "{name:46} right {:5.1} | foot {:5.1} veh {:5.1} menu {:5.1} cut {:5.1} | foot-looks-veh {:5.1} veh-looks-foot {:5.1} | switches {}",
            pct(s.right, s.n),
            pct(s.per_state[0][0], s.per_state[0][1]),
            pct(s.per_state[1][0], s.per_state[1][1]),
            pct(s.per_state[2][0], s.per_state[2][1]),
            pct(s.per_state[3][0], s.per_state[3][1]),
            pct(s.foot_looks_like_vehicle[0], s.foot_looks_like_vehicle[1]),
            pct(s.vehicle_looks_like_foot[0], s.vehicle_looks_like_foot[1]),
            s.switches
        );
    };
    show("legacy".into(), &replay(&doc, &Params::legacy()));
    for fv in [1e-15, 1e-6, 1e-5, 1e-4] {
        for lock in [6.0, 12.0, 25.0] {
            for enter in [0.3, 0.5, 0.8] {
                let p =
                    Params { foot_to_vehicle_free: fv, enter_toggle: enter, exit_lock_s: lock, ..Params::default() };
                show(format!("fv {fv:e} lock {lock} enter {enter}"), &replay(&doc, &p));
            }
        }
    }
}

#[test]
fn the_press_gated_filter_beats_the_old_one_on_vehicles_that_look_like_the_wrong_thing() {
    let Some(doc) = load() else {
        eprintln!("skipped: no tools/scene-context/cache/sequences.json (run tools/scene-context/scene.sh fixtures)");
        return;
    };
    let old = replay(&doc, &Params::legacy());
    let new = replay(&doc, &Params::default());
    let pct = |a: usize, b: usize| 100.0 * a as f64 / b.max(1) as f64;
    for (name, s) in [("old", &old), ("new", &new)] {
        eprintln!(
            "{name}: scene right {:.1}% | on foot {:.1}% vehicle {:.1}% menu {:.1}% cutscene {:.1}% | on-foot frames that look like a vehicle: {:.1}% right ({} of them) | vehicle frames that look on foot: {:.1}% right ({}) | {} switches",
            pct(s.right, s.n),
            pct(s.per_state[0][0], s.per_state[0][1]),
            pct(s.per_state[1][0], s.per_state[1][1]),
            pct(s.per_state[2][0], s.per_state[2][1]),
            pct(s.per_state[3][0], s.per_state[3][1]),
            pct(s.foot_looks_like_vehicle[0], s.foot_looks_like_vehicle[1]),
            s.foot_looks_like_vehicle[1],
            pct(s.vehicle_looks_like_foot[0], s.vehicle_looks_like_foot[1]),
            s.vehicle_looks_like_foot[1],
            s.switches,
        );
    }
    for (name, m) in [("the model alone", &new.model_confusion), ("the press-gated filter", &new.confusion)] {
        eprintln!("{name}, rows: truth, columns: read as (on foot, vehicle, menu, cutscene)");
        for (k, row) in m.iter().enumerate() {
            eprintln!("  {:8} {row:?}", ["on foot", "vehicle", "menu", "cutscene"][k]);
        }
    }
    assert!(new.right >= old.right, "the press-gated filter must not be worse overall");
    assert!(
        pct(new.foot_looks_like_vehicle[0], new.foot_looks_like_vehicle[1])
            > pct(old.foot_looks_like_vehicle[0], old.foot_looks_like_vehicle[1]),
        "and it must read the on-foot frames that look like a vehicle better"
    );
}

#[test]
#[ignore = "prints, for the first set of presses, where the filter is wrong: truth / model alone / filter, one letter per picture"]
fn dump_where_the_filter_is_wrong() {
    let Some(doc) = load() else { return };
    let letter = |k: usize| ['F', 'V', 'M', 'C'][k];
    for sess in doc["sessions"].as_array().unwrap() {
        let t = floats(&sess["t"]);
        let truth: Vec<usize> =
            sess["truth"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap() as usize).collect();
        let mut filter = SceneFilter::new(Params::default());
        for a in Action::ALL {
            filter.set_available(a, true);
        }
        let mut actions: Vec<(f64, Action)> = sess["actions"][0]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| (a[0].as_f64().unwrap(), Action::parse(a[1].as_str().unwrap()).unwrap()))
            .collect();
        actions.sort_by(|a, b| a.0.total_cmp(&b.0));
        let (mut tr, mut mo, mut fi, mut pr) = (String::new(), String::new(), String::new(), String::new());
        let mut next = 0;
        for i in 0..t.len() {
            let ti = f64::from(t[i]);
            let mut mark = ' ';
            while next < actions.len() && actions[next].0 <= ti {
                filter.push_action(actions[next].0, actions[next].1);
                mark = match actions[next].1 {
                    Action::EnterOrExitVehicle => 'E',
                    Action::PauseGame => 'P',
                    Action::ChangeCamera => {
                        if mark == ' ' {
                            'c'
                        } else {
                            mark
                        }
                    }
                };
                next += 1;
            }
            let c = floats(&sess["context"][i]);
            let e = filter.update(ti, [c[0], c[1], c[2], c[3]], [0.5, 0.5]);
            tr.push(letter(truth[i]));
            mo.push(letter((0..4).fold(0, |b, k| if c[k] > c[b] { k } else { b })));
            fi.push(letter(e.context as usize));
            pr.push(mark);
        }
        eprintln!(
            "== {} ({} pictures, {:.0} s apart)",
            sess["name"],
            t.len(),
            (t[t.len() - 1] - t[0]) / t.len() as f32
        );
        for start in (0..t.len()).step_by(110) {
            let end = (start + 110).min(t.len());
            eprintln!(
                "press {}\ntruth {}\nmodel {}\nfilter {}\n",
                &pr[start..end],
                &tr[start..end],
                &mo[start..end],
                &fi[start..end]
            );
        }
    }
}

#[test]
#[ignore = "a parameter sweep that prints a table"]
fn sweep_the_other_parameters_one_at_a_time() {
    let Some(doc) = load() else { return };
    let pct = |a: usize, b: usize| 100.0 * a as f64 / b.max(1) as f64;
    let show = |name: String, s: &Score| {
        eprintln!(
            "{name:34} right {:5.2} | foot {:5.1} veh {:5.1} menu {:5.1} cut {:5.1} | switches {}",
            pct(s.right, s.n),
            pct(s.per_state[0][0], s.per_state[0][1]),
            pct(s.per_state[1][0], s.per_state[1][1]),
            pct(s.per_state[2][0], s.per_state[2][1]),
            pct(s.per_state[3][0], s.per_state[3][1]),
            s.switches
        );
    };
    show("default".into(), &replay(&doc, &Params::default()));
    let d = Params::default;
    for v in [0.1, 0.2, 0.35, 0.5, 0.7, 1.0] {
        show(format!("alpha_per_s {v}"), &replay(&doc, &Params { alpha_per_s: v, ..d() }));
    }
    for v in [0.4, 0.7, 1.0] {
        show(format!("alpha_first {v}"), &replay(&doc, &Params { alpha_first: v, ..d() }));
    }
    for v in [0.003, 0.01, 0.03, 0.1] {
        show(format!("cutscene_in {v}"), &replay(&doc, &Params { cutscene_in: v, ..d() }));
    }
    for v in [0.01, 0.03, 0.1, 0.3] {
        show(format!("cutscene_out {v}"), &replay(&doc, &Params { cutscene_out: v, ..d() }));
    }
    for v in [0.001, 0.003, 0.01, 0.03] {
        show(format!("base_cued {v}"), &replay(&doc, &Params { base_cued: v, ..d() }));
    }
    for v in [0.2, 0.6, 1.5] {
        show(format!("cue_rate {v}"), &replay(&doc, &Params { cue_rate: v, ..d() }));
    }
    for v in [0.3, 0.5, 0.7, 0.9] {
        show(format!("enter_toggle {v}"), &replay(&doc, &Params { enter_toggle: v, ..d() }));
    }
    for v in [0.0, 3.0, 6.0, 12.0] {
        show(format!("exit_lock_s {v}"), &replay(&doc, &Params { exit_lock_s: v, ..d() }));
    }
    for v in [1e-5, 1e-4, 1e-3] {
        show(format!("foot_to_vehicle_free {v:e}"), &replay(&doc, &Params { foot_to_vehicle_free: v, ..d() }));
    }
}
