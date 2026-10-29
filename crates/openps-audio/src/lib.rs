#[cfg(target_os = "macos")]
mod ffi;
pub mod header;
#[cfg(target_os = "macos")]
pub mod media;
#[cfg(target_os = "macos")]
pub mod opus;
#[cfg(target_os = "macos")]
pub mod player;
