//! Physical pads. A `PadSource` reports what the pad does, a `PadSink` applies what the console asks of it.
//! Everything here is platform-neutral; the macOS implementation lives behind `cfg(target_os = "macos")`.

pub mod haptics;
pub mod hid;
#[cfg(target_os = "macos")]
pub mod mac;
pub mod map;
pub mod mock;
pub mod mode;
pub mod orient;
pub mod report;

use openps_input::state::ControllerState;
use openps_stream::padblob::PadOutput;

pub const MAX_PADS: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PadKind {
    DualSense,
    DualShock4,
}

/// Gyro in rad/s, accelerometer in g, orientation as a unit quaternion (x, y, z, w).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Sensors {
    pub gyro: [f32; 3],
    pub accel: [f32; 3],
    pub orientation: [f32; 4],
}

impl Sensors {
    /// A pad lying level and still: no rotation, 1 g of gravity on the vertical axis, identity orientation. This is what the console
    /// is given when a pad goes away (chiaki-ng `chiaki_controller_state_set_idle`).
    pub fn idle() -> Self {
        Self { gyro: [0.0; 3], accel: [0.0, 1.0, 0.0], orientation: [0.0, 0.0, 0.0, 1.0] }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum PadEvent {
    Connected { id: u32, kind: PadKind, name: String },
    State { id: u32, state: ControllerState },
    Sensors { id: u32, sensors: Sensors },
    Battery { id: u32, level: f32, charging: bool },
    Disconnected { id: u32 },
}

pub trait PadSource: Send {
    /// Everything that happened since the last call, oldest first. Never blocks.
    fn poll(&mut self) -> Vec<PadEvent>;
}

pub trait PadSink: Send + Sync {
    fn apply(&self, id: u32, out: &PadOutput);
    fn haptic(&self, id: u32, pcm: &[u8; 120]);
}
