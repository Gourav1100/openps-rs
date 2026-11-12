//! Scene detection for the key map: a small vision model reads the stream picture, a filter combines it with the player's actions.
//!
//! - [`probe`]: embedding -> probabilities (what the picture says)
//! - [`filter`]: probabilities + actions -> a steady scene (what is really going on)
//! - [`encoder`] (macOS): the Core ML image encoders
//! - [`service`] (macOS): the thread that ties them to the stream and publishes the scene on a [`openps_input::scene::SceneLink`]

pub mod filter;
pub mod probe;

#[cfg(target_os = "macos")]
pub mod encoder;
#[cfg(target_os = "macos")]
pub mod service;

use std::path::{Path, PathBuf};

pub const ENCODER_PACKAGE: &str = "scene-encoder.mlpackage";
pub const BACKBONE_PACKAGE: &str = "scene-backbone.mlpackage";
pub const PROBE_FILE: &str = "scene-probe.json";

/// The folder with the model: `OPENPS_MODELS` if set, the app bundle's `Resources/models`, or `assets/models` of a source checkout.
pub fn find_models_dir() -> Option<PathBuf> {
    let has = |d: &Path| {
        d.join(PROBE_FILE).is_file() && d.join(ENCODER_PACKAGE).exists() && d.join(BACKBONE_PACKAGE).exists()
    };
    let mut candidates: Vec<PathBuf> = vec![];
    if let Some(d) = std::env::var_os("OPENPS_MODELS") {
        candidates.push(d.into());
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(contents) = exe.parent().and_then(Path::parent) {
            candidates.push(contents.join("Resources").join("models"));
        }
    }
    candidates.push(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/models"));
    candidates.into_iter().find(|d| has(d))
}
