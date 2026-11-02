//! Raw pad values -> `ControllerState`.

use openps_input::state::Button;
use openps_input::state::ControllerState;
use std::collections::BTreeSet;

/// What a pad API reports: sticks with y up (positive) and each axis in [-1, 1], triggers in [0, 1].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RawPad {
    pub buttons: BTreeSet<Button>,
    pub left: (f64, f64),
    pub right: (f64, f64),
    pub l2: f64,
    pub r2: f64,
    pub touch: Option<(f64, f64)>,
    pub touch2: Option<(f64, f64)>,
}

pub const DEFAULT_DEADZONE: f64 = 0.08;

/// Radial dead zone: inside it the stick is neutral, outside it the range is rescaled so it starts from 0.
fn stick(x: f64, y: f64, deadzone: f64) -> (f64, f64) {
    let (x, y) = (if x.is_nan() { 0.0 } else { x }, if y.is_nan() { 0.0 } else { y });
    let m = x.hypot(y);
    if m <= deadzone || deadzone >= 1.0 {
        return (0.0, 0.0);
    }
    let k = ((m.min(1.0) - deadzone) / (1.0 - deadzone)) / m;
    (x * k, y * k)
}

pub fn to_state(raw: &RawPad, deadzone: f64) -> ControllerState {
    let dz = deadzone.clamp(0.0, 0.9);
    let (lx, ly) = stick(raw.left.0, raw.left.1, dz);
    let (rx, ry) = stick(raw.right.0, raw.right.1, dz);
    ControllerState {
        buttons: raw.buttons.clone(),
        left_x: lx,
        left_y: -ly,
        right_x: rx,
        right_y: -ry,
        l2: raw.l2,
        r2: raw.r2,
        touch: raw.touch,
        touch2: raw.touch2,
    }
    .clamped()
}
