//! Platform-neutral controller state: what the mapper outputs.

use std::collections::BTreeSet;

macro_rules! names {
    ($ty:ident: $($n:ident),* $(,)?) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum $ty { $($n),* }
        impl $ty {
            pub const ALL: &'static [$ty] = &[$($ty::$n),*];
            pub fn name(self) -> &'static str { match self { $($ty::$n => stringify!($n)),* } }
        }
    };
}
names!(Button: CROSS, CIRCLE, SQUARE, TRIANGLE, L1, R1, L3, R3, CREATE, OPTIONS, PS, TOUCHPAD, MUTE,
    DPAD_UP, DPAD_DOWN, DPAD_LEFT, DPAD_RIGHT);
names!(Axis: L2, R2, LEFT_STICK_UP, LEFT_STICK_DOWN, LEFT_STICK_LEFT, LEFT_STICK_RIGHT,
    RIGHT_STICK_UP, RIGHT_STICK_DOWN, RIGHT_STICK_LEFT, RIGHT_STICK_RIGHT);
names!(Gesture: TOUCHPAD_TAP, TOUCHPAD_SWIPE_UP, TOUCHPAD_SWIPE_DOWN, TOUCHPAD_SWIPE_LEFT, TOUCHPAD_SWIPE_RIGHT);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Control {
    Button(Button),
    Axis(Axis),
    Gesture(Gesture),
}

impl Control {
    pub fn name(self) -> &'static str {
        match self {
            Control::Button(b) => b.name(),
            Control::Axis(a) => a.name(),
            Control::Gesture(g) => g.name(),
        }
    }
}

/// "SHARE" is an alias of CREATE.
pub fn parse_control(name: &str) -> Result<Control, String> {
    let mut t = name.trim().to_uppercase();
    if t == "SHARE" {
        t = "CREATE".into();
    }
    if let Some(b) = Button::ALL.iter().find(|b| b.name() == t) {
        return Ok(Control::Button(*b));
    }
    if let Some(a) = Axis::ALL.iter().find(|a| a.name() == t) {
        return Ok(Control::Axis(*a));
    }
    if let Some(g) = Gesture::ALL.iter().find(|g| g.name() == t) {
        return Ok(Control::Gesture(*g));
    }
    Err(format!("unknown PS5 control {name:?}"))
}

/// Sticks in [-1, 1] (y positive = down, like HID), triggers in [0, 1], touch in [0, 1]^2.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ControllerState {
    pub buttons: BTreeSet<Button>,
    pub left_x: f64,
    pub left_y: f64,
    pub right_x: f64,
    pub right_y: f64,
    pub l2: f64,
    pub r2: f64,
    pub touch: Option<(f64, f64)>,
    /// The second finger on the touchpad (a physical pad only; the keyboard mapper never uses it).
    pub touch2: Option<(f64, f64)>,
}

impl ControllerState {
    pub fn clamped(&self) -> Self {
        // NaN (never expected, but this feeds the network) is treated as neutral instead of propagating
        let c = |v: f64, lo: f64, hi: f64| if v.is_nan() { 0.0_f64.clamp(lo, hi) } else { v.clamp(lo, hi) };
        Self {
            buttons: self.buttons.clone(),
            left_x: c(self.left_x, -1.0, 1.0),
            left_y: c(self.left_y, -1.0, 1.0),
            right_x: c(self.right_x, -1.0, 1.0),
            right_y: c(self.right_y, -1.0, 1.0),
            l2: c(self.l2, 0.0, 1.0),
            r2: c(self.r2, 0.0, 1.0),
            touch: self.touch.map(|(x, y)| (c(x, 0.0, 1.0), c(y, 0.0, 1.0))),
            touch2: self.touch2.map(|(x, y)| (c(x, 0.0, 1.0), c(y, 0.0, 1.0))),
        }
    }
}
