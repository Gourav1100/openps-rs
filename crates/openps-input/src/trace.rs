//! Opt-in input tracing (`OPENPS_INPUT_TRACE=1`): one on/off switch and one clock shared by every crate on the pad path, so lines
//! logged from different threads can be ordered against each other (the log's own timestamps only have 1 s resolution).

use crate::state::ControllerState;
use std::sync::OnceLock;
use std::time::Instant;

pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("OPENPS_INPUT_TRACE").is_some())
}

/// Microseconds since the first trace call in this process.
pub fn now_us() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_micros() as u64
}

/// The parts of a pad state that matter when hunting a stuck control, on one short line.
pub fn brief(s: &ControllerState) -> String {
    format!(
        "btn={:?} lt={:.3} rt={:.3} ls=({:.2},{:.2}) rs=({:.2},{:.2})",
        s.buttons, s.l2, s.r2, s.left_x, s.left_y, s.right_x, s.right_y
    )
}

#[cfg(test)]
mod tests {
    use super::{brief, now_us};
    use crate::state::{Button, ControllerState};

    #[test]
    fn the_clock_never_goes_backwards() {
        let a = now_us();
        let b = now_us();
        assert!(b >= a);
    }

    #[test]
    fn brief_names_buttons_and_both_triggers() {
        let mut s = ControllerState { l2: 1.0, r2: 0.25, ..Default::default() };
        s.buttons.insert(Button::CROSS);
        let line = brief(&s);
        assert!(line.contains("CROSS") && line.contains("lt=1.000") && line.contains("rt=0.250"), "{line}");
    }
}
