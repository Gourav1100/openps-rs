//! Backend-independent render policy shared by native presentation hosts.
pub mod camera;
pub mod ladder;
pub mod look;
pub mod timeline;

mod policy;
pub use policy::*;

use std::sync::OnceLock;
use std::time::Instant;

/// Seconds on a monotonic clock shared by the renderer and the input thread (camera compensation compares their timestamps).
pub fn now() -> f64 {
    static T0: OnceLock<Instant> = OnceLock::new();
    T0.get_or_init(Instant::now).elapsed().as_secs_f64()
}

