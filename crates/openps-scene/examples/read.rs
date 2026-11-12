//! Reads pictures the way the app does and prints what the scene probe makes of them.
//!
//!     cargo run --release -p openps-scene --example read -- picture.png [more.jpg ...]
//!
//! With several pictures it also prints how close each one's embeddings are to the first one's (cosine, per picture of the layout), which
//! shows how much a different size or compression of the same frame moves the reading.

#[cfg(target_os = "macos")]
fn main() {
    use openps_scene::encoder::{layout, Encoder};
    use openps_scene::probe::Probe;
    use openps_scene::{find_models_dir, PROBE_FILE};

    let paths: Vec<std::path::PathBuf> = std::env::args_os().skip(1).map(Into::into).collect();
    if paths.is_empty() {
        eprintln!("usage: read PICTURE...");
        std::process::exit(2);
    }
    let models = find_models_dir().expect("the scene models (assets/models or OPENPS_MODELS)");
    let cache = std::env::temp_dir().join("openps-scene-read");
    let encoder = Encoder::load(&models, &cache).expect("the encoders load");
    let probe =
        Probe::from_json(&std::fs::read_to_string(models.join(PROBE_FILE)).expect("the probe")).expect("a valid probe");
    let cosine = |a: &[f32], b: &[f32]| {
        let dot: f64 = a.iter().zip(b).map(|(x, y)| f64::from(*x) * f64::from(*y)).sum();
        let norm = |v: &[f32]| v.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>().sqrt();
        dot / (norm(a) * norm(b))
    };
    let mut first: Option<Vec<f32>> = None;
    for path in &paths {
        let e = encoder.embed_file(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let c = probe.context(&e);
        let v = probe.view(&e);
        print!(
            "{}: on foot {:.2} vehicle {:.2} menu {:.2} cutscene {:.2} | first person {:.2}",
            path.display(),
            c[0],
            c[1],
            c[2],
            c[3],
            v[0]
        );
        if let Some(f) = &first {
            let mut at = 0;
            for (name, dim) in layout() {
                print!(" | {name} cosine {:.4}", cosine(&f[at..at + dim], &e[at..at + dim]));
                at += dim;
            }
        } else {
            first = Some(e);
        }
        println!();
    }
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("the scene models run on macOS only");
}
