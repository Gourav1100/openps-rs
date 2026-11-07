//! OpenPS application core: console store, discovery/pairing/connect flows, control channel and event
//! forwarding, with no UI dependency (the Tauri crate is a thin adapter over this).
pub mod backend;
pub mod capture;
pub mod control;
pub mod core;
pub mod events;
pub mod netlink;
pub mod picture;
pub mod psn;
pub mod store;
pub mod sync;
pub mod types;

pub use crate::core::{Core, CoreBuilder, CoreError, PsnService, RemoteRegistrar, RemoteRequest};
pub use events::{AppEvent, EventSink, MediaSink, NullMedia, StreamStats};
pub use openps_psn::api::PsnConsole;
pub use types::*;
