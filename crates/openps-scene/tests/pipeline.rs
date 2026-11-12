//! The whole model path on real frames: Core ML encoder through Vision, then the probe. Skipped when the model or the labeled frames are
//! not in the checkout (`assets/models`, `tools/scene-context/dataset`).
#![cfg(target_os = "macos")]

use openps_scene::encoder::Encoder;
use openps_scene::probe::Probe;
use openps_scene::{find_models_dir, PROBE_FILE};
use std::path::{Path, PathBuf};

fn data_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/scene-context/dataset")
}

/// (image, state) for every `stride`-th labeled frame: on foot 0, in vehicle 1, menu 2, cutscene 3.
fn sample(stride: usize) -> Vec<(PathBuf, usize)> {
    let mut out = vec![];
    let Ok(text) = std::fs::read_to_string(data_dir().join("labels.json")) else { return out };
    let doc: serde_json::Value = serde_json::from_str(&text).unwrap();
    for (i, l) in doc["labels"].as_array().unwrap().iter().enumerate() {
        if i % stride != 0 {
            continue;
        }
        let state = match l["context"].as_str().unwrap() {
            "on_foot" => 0,
            "in_vehicle" => 1,
            "menu" => 2,
            "cutscene" => 3,
            _ => continue,
        };
        let file = Path::new(l["file"].as_str().unwrap()).file_name().unwrap().to_str().unwrap();
        out.push((data_dir().join("frames").join(format!("{}__{file}", l["recording"].as_str().unwrap())), state));
    }
    out
}

#[test]
fn the_model_reads_labeled_frames_the_way_the_probe_was_trained() {
    let Some(models) = find_models_dir() else {
        eprintln!("skipped: no model in assets/models");
        return;
    };
    let frames = sample(30);
    if frames.len() < 50 {
        eprintln!("skipped: no labeled frames in tools/scene-context/dataset");
        return;
    }
    let cache = tempfile::tempdir().unwrap();
    let started = std::time::Instant::now();
    let encoder = Encoder::load(&models, cache.path()).expect("the encoder loads");
    eprintln!("load (with compiling): {:?}", started.elapsed());
    let probe = Probe::from_json(&std::fs::read_to_string(models.join(PROBE_FILE)).unwrap()).unwrap();

    let started = std::time::Instant::now();
    let (mut right, mut per_class) = (0, [[0usize; 2]; 4]);
    for (path, state) in &frames {
        let e = encoder.embed_file(path).expect("a frame is embedded");
        assert_eq!(e.len(), probe.dim());
        let p = probe.context(&e);
        let pred = (0..4).fold(0, |b, i| if p[i] > p[b] { i } else { b });
        per_class[*state][1] += 1;
        if pred == *state {
            right += 1;
            per_class[*state][0] += 1;
        }
    }
    let n = frames.len();
    eprintln!("{right}/{n} right, {:?} per frame; per state (right/seen) {per_class:?}", started.elapsed() / n as u32);
    // The probe was trained on these frames and mixed with a zero-shot reading, so not all of them agree with their labels; a much lower
    // number would mean the preprocessing differs from training.
    assert!(right as f32 / n as f32 > 0.8, "only {right} of {n} frames read right");

    // a second load finds the compiled model and is quick
    let started = std::time::Instant::now();
    Encoder::load(&models, cache.path()).expect("loads again");
    eprintln!("load (compiled): {:?}", started.elapsed());
}
