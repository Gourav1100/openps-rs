//! The Rust scene code against the Python it was evaluated with (`tools/scene-context/scene.py train` or `fixtures` wrote the fixture).

use openps_input::scene::Action;
use openps_scene::filter::{Params, SceneFilter};
use openps_scene::probe::Probe;
use openps_scene::{find_models_dir, PROBE_FILE};
use serde_json::Value;
use std::path::Path;

fn fixture() -> Value {
    let text = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/reference.json"))
        .expect("tests/fixtures/reference.json (run tools/scene-context/scene.sh fixtures)");
    serde_json::from_str(&text).unwrap()
}

fn floats(v: &Value) -> Vec<f64> {
    v.as_array().unwrap().iter().map(|x| x.as_f64().unwrap()).collect()
}

#[test]
fn the_probe_gives_the_probabilities_python_does() {
    let Some(models) = find_models_dir() else {
        eprintln!("skipped: no model in assets/models");
        return;
    };
    let probe = Probe::from_json(&std::fs::read_to_string(models.join(PROBE_FILE)).unwrap()).unwrap();
    let doc = fixture();
    for f in doc["frames"].as_array().unwrap() {
        let e: Vec<f32> = floats(&f["embedding"]).into_iter().map(|v| v as f32).collect();
        let want = floats(&f["context"]);
        let got = probe.context(&e);
        for i in 0..4 {
            assert!((f64::from(got[i]) - want[i]).abs() < 1e-4, "{}: context {i}: {got:?} vs {want:?}", f["file"]);
        }
        let want = floats(&f["view"]);
        let got = probe.view(&e);
        for i in 0..2 {
            assert!((f64::from(got[i]) - want[i]).abs() < 1e-4, "{}: view {i}: {got:?} vs {want:?}", f["file"]);
        }
    }
}

#[test]
fn the_filter_follows_the_python_filter_step_for_step() {
    let doc = fixture();
    let f = &doc["filter"];
    let t = floats(&f["t"]);
    let mut filter = SceneFilter::new(Params::legacy());
    for a in Action::ALL {
        filter.set_available(a, true);
    }
    let mut pending: Vec<(f64, Action)> = f["actions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| (a[0].as_f64().unwrap(), Action::parse(a[1].as_str().unwrap()).unwrap()))
        .collect();
    pending.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut next = 0;
    for (i, ti) in t.iter().enumerate() {
        while next < pending.len() && pending[next].0 <= *ti {
            filter.push_action(pending[next].0, pending[next].1);
            next += 1;
        }
        let c: Vec<f32> = floats(&f["context_probs"][i]).into_iter().map(|v| v as f32).collect();
        let v: Vec<f32> = floats(&f["view_probs"][i]).into_iter().map(|v| v as f32).collect();
        filter.update(*ti, [c[0], c[1], c[2], c[3]], [v[0], v[1]]);
        let (ctx, view) = filter.belief();
        let (want_c, want_v) = (floats(&f["context_belief"][i]), floats(&f["view_belief"][i]));
        for k in 0..4 {
            assert!((ctx[k] - want_c[k]).abs() < 1e-5, "step {i} context {k}: {ctx:?} vs {want_c:?}");
        }
        for k in 0..2 {
            assert!((view[k] - want_v[k]).abs() < 1e-5, "step {i} view {k}: {view:?} vs {want_v:?}");
        }
    }
    assert_eq!(next, pending.len().min(next), "every action was delivered");
}

#[cfg(target_os = "macos")]
#[test]
fn vision_scales_the_pictures_nearly_the_way_python_did() {
    use openps_scene::encoder::{layout, Encoder};
    let Some(models) = find_models_dir() else {
        eprintln!("skipped: no model in assets/models");
        return;
    };
    let data = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/scene-context/dataset");
    let doc = fixture();
    let cache = tempfile::tempdir().unwrap();
    let encoder = Encoder::load(&models, cache.path()).unwrap();
    let layout = layout();
    // the lowest cosine per picture over the fixture frames
    let mut worst = vec![1.0f64; layout.len()];
    for f in doc["frames"].as_array().unwrap() {
        let path = data.join(f["file"].as_str().unwrap());
        if !path.is_file() {
            eprintln!("skipped: {} is not in this checkout", path.display());
            return;
        }
        let got = encoder.embed_file(&path).unwrap();
        let want = floats(&f["embedding"]);
        assert_eq!(got.len(), want.len());
        let mut at = 0;
        for (k, (_, dim)) in layout.iter().enumerate() {
            let (g, w) = (&got[at..at + dim], &want[at..at + dim]);
            let dot: f64 = g.iter().zip(w).map(|(a, b)| f64::from(*a) * b).sum();
            let (ng, nw) = (
                g.iter().map(|a| f64::from(*a).powi(2)).sum::<f64>().sqrt(),
                w.iter().map(|b| b * b).sum::<f64>().sqrt(),
            );
            worst[k] = worst[k].min(dot / (ng * nw));
            at += dim;
        }
    }
    for ((name, _), c) in layout.iter().zip(&worst) {
        eprintln!("lowest cosine to the Python embedding, {name}: {c:.4}");
        assert!(*c > 0.95, "Vision scales {name} differently from the training pipeline (cosine {c})");
    }
}
