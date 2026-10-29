//! Con2PC input system (keyboard + mouse -> virtual PS5 pad). See analysis/spec/04-con2pc-input-system.md.
#![allow(non_camel_case_types, non_upper_case_globals, clippy::upper_case_acronyms)]
pub mod config;
pub mod editor;
pub mod engine;
pub mod keys;
pub mod mapping;
pub mod overlay;
pub mod state;
pub mod textinput;

pub use config::{load_defaults, load_layers, Config};
pub use mapping::{InputSnapshot, Mapper};
