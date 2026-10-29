//! Translation of raw input snapshots into `ControllerState` (spec 04 section 6; port of mapping.py).
//! Stateful only in time: stick ramping, mouse smoothing, touchpad strokes, trigger/click modes.

use crate::config::{Binding, Config};
use crate::keys::{Input, Key, MouseInput};
use crate::state::{Axis, Button, Control, ControllerState, Gesture};
use std::collections::{BTreeSet, HashMap, HashSet};

/// The 250 Hz tick `mouse.sensitivity` / `mouse.smoothing` were tuned at.
pub const REF_DT: f64 = 0.004;
const CLICK_TOGGLE_KEY: Key = Key::TAB;

pub fn limit_to_circle(x: f64, y: f64) -> (f64, f64) {
    let mag = x.hypot(y);
    if mag > 1.0 {
        (x / mag, y / mag)
    } else {
        (x, y)
    }
}

pub fn smoothing_tau(smoothing: f64) -> f64 {
    let s = smoothing.clamp(0.0, 0.99);
    if s <= 0.0 {
        0.0
    } else {
        -REF_DT / s.ln()
    }
}

#[derive(Clone, Debug, Default)]
pub struct InputSnapshot {
    pub pressed: BTreeSet<Input>,
    pub mouse_dx: f64,
    pub mouse_dy: f64,
}

impl InputSnapshot {
    pub fn new(pressed: impl IntoIterator<Item = Input>, dx: f64, dy: f64) -> Self {
        Self { pressed: pressed.into_iter().collect(), mouse_dx: dx, mouse_dy: dy }
    }
}

#[derive(Clone, Copy, Debug)]
struct Adaptive {
    value: f64,
    direction: i32,
    sweeping: bool,
    suppress: bool,
}
impl Default for Adaptive {
    fn default() -> Self {
        Self { value: 0.0, direction: 1, sweeping: false, suppress: false }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Stick {
    LeftX,
    LeftY,
    RightX,
    RightY,
}

fn stick_key(a: Axis) -> Option<(Stick, f64)> {
    Some(match a {
        Axis::LEFT_STICK_UP => (Stick::LeftY, -1.0),
        Axis::LEFT_STICK_DOWN => (Stick::LeftY, 1.0),
        Axis::LEFT_STICK_LEFT => (Stick::LeftX, -1.0),
        Axis::LEFT_STICK_RIGHT => (Stick::LeftX, 1.0),
        Axis::RIGHT_STICK_UP => (Stick::RightY, -1.0),
        Axis::RIGHT_STICK_DOWN => (Stick::RightY, 1.0),
        Axis::RIGHT_STICK_LEFT => (Stick::RightX, -1.0),
        Axis::RIGHT_STICK_RIGHT => (Stick::RightX, 1.0),
        _ => return None,
    })
}

/// (start, end) finger positions per gesture as offsets from the pad centre, in units of swipe_length.
fn stroke(g: Gesture) -> ((f64, f64), (f64, f64)) {
    match g {
        Gesture::TOUCHPAD_SWIPE_RIGHT => ((-0.5, 0.0), (0.5, 0.0)),
        Gesture::TOUCHPAD_SWIPE_LEFT => ((0.5, 0.0), (-0.5, 0.0)),
        Gesture::TOUCHPAD_SWIPE_UP => ((0.0, 0.5), (0.0, -0.5)),
        Gesture::TOUCHPAD_SWIPE_DOWN => ((0.0, -0.5), (0.0, 0.5)),
        Gesture::TOUCHPAD_TAP => ((0.0, 0.0), (0.0, 0.0)),
    }
}

fn is_cmd(i: &Input) -> bool {
    matches!(i, Input::Key(Key::LEFT_COMMAND) | Input::Key(Key::RIGHT_COMMAND))
}

pub struct Mapper {
    pub config: Config,
    pub profile: String,
    bindings: Vec<Binding>,
    mx: f64,
    my: f64,
    last_move: f64,
    last_t: Option<f64>,
    stick: HashMap<Stick, f64>,
    held: HashSet<Gesture>,
    strokes: Vec<(f64, Gesture)>,
    /// true = the click acts as the bumper (RB / LB) instead of the trigger
    bumper_left: bool,
    bumper_right: bool,
    prev_pressed: BTreeSet<Input>,
    consumed: BTreeSet<Input>,
    adaptive_l2: Adaptive,
    adaptive_r2: Adaptive,
    on: HashMap<Control, f64>,
    events: Vec<String>,
}

impl Mapper {
    pub fn new(config: Config, profile: Option<&str>) -> Result<Self, String> {
        let profile = profile.unwrap_or(&config.default_name).to_string();
        let bindings = config.resolve(Some(&profile))?;
        Ok(Self {
            config,
            profile,
            bindings,
            mx: 0.0,
            my: 0.0,
            last_move: 0.0,
            last_t: None,
            stick: HashMap::new(),
            held: HashSet::new(),
            strokes: vec![],
            bumper_left: false,
            bumper_right: false,
            prev_pressed: BTreeSet::new(),
            consumed: BTreeSet::new(),
            adaptive_l2: Adaptive::default(),
            adaptive_r2: Adaptive::default(),
            on: HashMap::new(),
            events: vec![],
        })
    }

    pub fn set_profile(&mut self, name: &str) -> Result<(), String> {
        self.bindings = self.config.resolve(Some(name))?;
        self.profile = name.to_string();
        self.reset();
        Ok(())
    }

    /// Drops held strokes and ramped sticks. `held` gestures and click modes are kept on purpose.
    pub fn reset(&mut self) {
        self.mx = 0.0;
        self.my = 0.0;
        self.last_t = None;
        self.stick.clear();
        self.strokes.clear();
        self.on.clear();
        self.consumed.clear();
        self.prev_pressed.clear();
        for a in [&mut self.adaptive_l2, &mut self.adaptive_r2] {
            a.value = 0.0;
            a.sweeping = false;
            a.suppress = false;
        }
    }

    /// (left click is bumper, right click is bumper): a session choice that survives config reloads.
    pub fn bumper(&self) -> (bool, bool) {
        (self.bumper_left, self.bumper_right)
    }
    pub fn set_bumper(&mut self, (left, right): (bool, bool)) {
        self.bumper_left = left;
        self.bumper_right = right;
    }

    pub fn pop_events(&mut self) -> Vec<String> {
        std::mem::take(&mut self.events)
    }

    /// (left, right) = ("RT"|"RB", "LT"|"LB")
    pub fn click_modes(&self) -> (&'static str, &'static str) {
        (if self.bumper_left { "RB" } else { "RT" }, if self.bumper_right { "LB" } else { "LT" })
    }

    fn adaptive_mut(&mut self, axis: Axis) -> &mut Adaptive {
        if axis == Axis::L2 {
            &mut self.adaptive_l2
        } else {
            &mut self.adaptive_r2
        }
    }

    pub fn map(&mut self, snap: &InputSnapshot, now: f64) -> ControllerState {
        // A non-finite mouse delta (another `Cursor` impl) must not poison the smoothing filter.
        let clean;
        let snap = if snap.mouse_dx.is_finite() && snap.mouse_dy.is_finite() {
            snap
        } else {
            let fin = |v: f64| if v.is_finite() { v } else { 0.0 };
            clean = InputSnapshot {
                pressed: snap.pressed.clone(),
                mouse_dx: fin(snap.mouse_dx),
                mouse_dy: fin(snap.mouse_dy),
            };
            &clean
        };
        let dt = self.last_t.map_or(0.0, |t| (now - t).max(0.0));
        self.last_t = Some(now);

        let mut raw: BTreeSet<Input> = snap.pressed.clone();
        raw.extend(self.mouse_moves(snap));
        self.toggle_click_modes(&raw);
        let adaptive = self.adaptive_clicks(&raw);
        let pressed: BTreeSet<Input> = raw.difference(&self.consumed).copied().collect();
        let active: Vec<&Binding> = self.bindings.iter().filter(|b| b.inputs.is_subset(&pressed)).collect();
        // Longest chord wins: drop bindings whose chord is a strict subset of another active one.
        let active: Vec<Binding> = active
            .iter()
            .filter(|b| !active.iter().any(|o| b.inputs.is_subset(&o.inputs) && b.inputs != o.inputs))
            .map(|b| (*b).clone())
            .collect();

        let mut st = ControllerState::default();
        let mut targets: HashMap<Stick, f64> =
            [(Stick::LeftX, 0.0), (Stick::LeftY, 0.0), (Stick::RightX, 0.0), (Stick::RightY, 0.0)].into();
        let mut gestures: HashSet<Gesture> = HashSet::new();
        for b in &active {
            for ctl in &b.outputs {
                let ctl = self.swap_click(b, *ctl);
                Self::apply(&mut st, &mut targets, &mut gestures, ctl);
            }
        }

        self.finish_digital(&mut st, adaptive, now, dt);

        for g in gestures.difference(&self.held) {
            self.strokes.push((now, *g));
        } // rising edge only
        self.held = gestures;
        st.touch = self.touch(now);

        for (name, target) in [(Stick::LeftX, 0), (Stick::LeftY, 0), (Stick::RightX, 0), (Stick::RightY, 0)]
            .map(|(s, _)| (s, targets[&s]))
        {
            let v = self.ramp(name, target.clamp(-1.0, 1.0), dt);
            match name {
                Stick::LeftX => st.left_x = v,
                Stick::LeftY => st.left_y = v,
                Stick::RightX => st.right_x = v,
                Stick::RightY => st.right_y = v,
            }
        }
        (st.left_x, st.left_y) = limit_to_circle(st.left_x, st.left_y);
        (st.right_x, st.right_y) = limit_to_circle(st.right_x, st.right_y);
        self.apply_mouse_look(&mut st, snap, now, dt);
        (st.left_x, st.left_y) = limit_to_circle(st.left_x, st.left_y);
        (st.right_x, st.right_y) = limit_to_circle(st.right_x, st.right_y);
        st.clamped()
    }

    /// Command held: the physical clicks drive the analog triggers. Returns (L2 click, R2 click) or None.
    fn adaptive_clicks(&mut self, raw: &BTreeSet<Input>) -> Option<(bool, bool)> {
        if !self.config.adaptive_triggers.enabled || !raw.iter().any(is_cmd) {
            return None;
        }
        let (mut l2, mut r2) = (false, false);
        // Fixed pairing: physical left click -> LT, right click -> RT.
        let left = Input::Mouse(MouseInput::MOUSE_LEFT);
        let right = Input::Mouse(MouseInput::MOUSE_RIGHT);
        if raw.contains(&left) {
            self.consumed.insert(left);
            l2 = true;
        }
        if raw.contains(&right) {
            self.consumed.insert(right);
            r2 = true;
        }
        Some((l2, r2))
    }

    /// Tab + left click toggles RT/RB, Tab + right click toggles LT/LB (on the click edge).
    fn toggle_click_modes(&mut self, raw: &BTreeSet<Input>) {
        let newly: BTreeSet<Input> = raw.difference(&self.prev_pressed).copied().collect();
        if raw.contains(&Input::Key(CLICK_TOGGLE_KEY)) {
            let left = Input::Mouse(MouseInput::MOUSE_LEFT);
            let right = Input::Mouse(MouseInput::MOUSE_RIGHT);
            if newly.contains(&left) {
                self.bumper_left = !self.bumper_left;
                self.consumed.insert(left);
                self.events.push(format!("left click now acts as {}", self.click_modes().0));
            }
            if newly.contains(&right) {
                self.bumper_right = !self.bumper_right;
                self.consumed.insert(right);
                self.events.push(format!("right click now acts as {}", self.click_modes().1));
            }
        }
        self.consumed = self.consumed.intersection(raw).copied().collect();
        self.prev_pressed = raw.clone();
    }

    fn swap_click(&self, b: &Binding, ctl: Control) -> Control {
        if b.inputs.len() == 1 {
            let only = *b.inputs.iter().next().unwrap();
            if only == Input::Mouse(MouseInput::MOUSE_LEFT) && ctl == Control::Axis(Axis::R2) && self.bumper_left {
                return Control::Button(Button::R1);
            }
            if only == Input::Mouse(MouseInput::MOUSE_RIGHT) && ctl == Control::Axis(Axis::L2) && self.bumper_right {
                return Control::Button(Button::L1);
            }
        }
        ctl
    }

    fn finish_digital(&mut self, st: &mut ControllerState, adaptive: Option<(bool, bool)>, now: f64, dt: f64) {
        let min_s = self.config.output.min_press_ms / 1000.0;
        // buttons: a tap lasts at least min_press_ms
        let wanted: BTreeSet<Button> = st.buttons.clone();
        let on_buttons: Vec<(Button, f64)> = self
            .on
            .iter()
            .filter_map(|(c, t)| if let Control::Button(b) = c { Some((*b, *t)) } else { None })
            .collect();
        for (btn, t) in &on_buttons {
            if !wanted.contains(btn) && (0.0..min_s).contains(&(now - t)) {
                st.buttons.insert(*btn);
            }
        }
        for btn in &wanted {
            self.on.entry(Control::Button(*btn)).or_insert(now);
        }
        let stale: Vec<Control> =
            self.on.keys().filter(|c| matches!(c, Control::Button(b) if !st.buttons.contains(b))).copied().collect();
        for c in stale {
            self.on.remove(&c);
        }
        // triggers
        let modifier = adaptive.is_some();
        for axis in [Axis::L2, Axis::R2] {
            let digital_in = if axis == Axis::L2 { st.l2 } else { st.r2 };
            let value;
            if let Some((l_click, r_click)) = adaptive {
                self.on.remove(&Control::Axis(axis));
                let click = if axis == Axis::L2 { l_click } else { r_click };
                value = self.trigger_value(axis, if click { 1.0 } else { 0.0 }, click, true, dt);
            } else {
                let mut digital = digital_in;
                let held = digital > 0.0;
                let key = Control::Axis(axis);
                if held {
                    self.on.entry(key).or_insert(now);
                } else if self.on.get(&key).is_some_and(|t| (0.0..min_s).contains(&(now - t))) {
                    digital = 1.0; // a quick click still registers as a real press
                } else {
                    self.on.remove(&key);
                }
                value = self.trigger_value(axis, digital, held, modifier, dt);
            }
            if axis == Axis::L2 {
                st.l2 = value
            } else {
                st.r2 = value
            }
        }
    }

    fn trigger_value(&mut self, axis: Axis, digital: f64, held: bool, modifier: bool, dt: f64) -> f64 {
        let sweep_ms = self.config.adaptive_triggers.sweep_ms;
        let a = self.adaptive_mut(axis);
        if modifier {
            if held && !a.suppress {
                if !a.sweeping {
                    // a new click starts a sweep from the locked value
                    a.sweeping = true;
                    a.direction = if a.value < 1.0 { 1 } else { -1 };
                }
                let sweep_s = (sweep_ms / 1000.0).max(1e-3);
                a.value += a.direction as f64 * dt / sweep_s;
                if a.value >= 1.0 {
                    a.value = 1.0;
                    a.direction = -1;
                } else if a.value <= 0.0 {
                    a.value = 0.0;
                    a.direction = 1;
                }
                return a.value;
            }
            a.sweeping = false;
            if !held {
                a.suppress = false;
            }
            return a.value; // click released: value stays LOCKED until Command is released
        }
        if a.value > 0.0 || a.sweeping {
            // Command released: reset
            a.value = 0.0;
            a.sweeping = false;
            a.suppress = held; // a click still down must be released before it fires again
        }
        if a.suppress {
            if !held {
                a.suppress = false;
            }
            return 0.0;
        }
        digital
    }

    fn mouse_moves(&self, snap: &InputSnapshot) -> BTreeSet<Input> {
        let t = self.config.mouse.move_threshold;
        let mut out = BTreeSet::new();
        if snap.mouse_dx > t {
            out.insert(Input::Mouse(MouseInput::MOUSE_MOVE_RIGHT));
        } else if snap.mouse_dx < -t {
            out.insert(Input::Mouse(MouseInput::MOUSE_MOVE_LEFT));
        }
        if snap.mouse_dy > t {
            out.insert(Input::Mouse(MouseInput::MOUSE_MOVE_DOWN));
        } else if snap.mouse_dy < -t {
            out.insert(Input::Mouse(MouseInput::MOUSE_MOVE_UP));
        }
        out
    }

    fn apply(
        st: &mut ControllerState,
        targets: &mut HashMap<Stick, f64>,
        gestures: &mut HashSet<Gesture>,
        ctl: Control,
    ) {
        match ctl {
            Control::Button(b) => {
                st.buttons.insert(b);
            }
            Control::Gesture(g) => {
                gestures.insert(g);
            }
            Control::Axis(Axis::L2) => st.l2 = 1.0,
            Control::Axis(Axis::R2) => st.r2 = 1.0,
            Control::Axis(a) => {
                let (name, sign) = stick_key(a).expect("stick axis");
                *targets.get_mut(&name).unwrap() += sign;
            }
        }
    }

    fn ramp(&mut self, name: Stick, target: f64, dt: f64) -> f64 {
        let k = &self.config.keyboard_stick;
        let cur = *self.stick.get(&name).unwrap_or(&0.0);
        let growing = target.abs() > cur.abs() && (cur == 0.0 || (cur > 0.0) == (target > 0.0));
        let left = matches!(name, Stick::LeftX | Stick::LeftY);
        let ms = if growing {
            if left {
                k.left_ramp_ms
            } else {
                k.right_ramp_ms
            }
        } else if left {
            k.left_release_ms
        } else {
            k.release_ms
        };
        let cur = if ms <= 0.0 {
            target
        } else {
            let step = dt * 1000.0 / ms;
            if target > cur {
                target.min(cur + step)
            } else {
                target.max(cur - step)
            }
        };
        self.stick.insert(name, cur);
        cur
    }

    fn apply_mouse_look(&mut self, st: &mut ControllerState, snap: &InputSnapshot, now: f64, dt: f64) {
        let m = self.config.mouse.clone();
        if m.look == "NONE" {
            return;
        }
        let moving = snap.mouse_dx != 0.0 || snap.mouse_dy != 0.0;
        if moving {
            self.last_move = now;
        } else if now - self.last_move >= m.recenter_ms / 1000.0 {
            self.mx = 0.0;
            self.my = 0.0;
        }
        let (mdx, dy) = (snap.mouse_dx, if m.invert_y { -snap.mouse_dy } else { snap.mouse_dy });
        let step = if dt > 0.0 { dt } else { REF_DT }; // first tick after a reset: one reference tick
        let scale = REF_DT / step * m.sensitivity * 10.0; // sensitivity keeps its meaning at any loop rate
        let (tx, ty) = (mdx * scale, dy * scale);
        let tau = smoothing_tau(m.smoothing);
        let alpha = if tau <= 0.0 { 1.0 } else { 1.0 - (-step / tau).exp() };
        self.mx += (tx - self.mx) * alpha;
        self.my += (ty - self.my) * alpha;
        let (mut x, mut y) = (self.mx, self.my);
        let mag = x.hypot(y);
        if mag < 1e-3 {
            x = 0.0;
            y = 0.0;
        } else if m.deadzone != 0.0 {
            let s2 = m.deadzone + (1.0 - m.deadzone) * mag.min(1.0);
            x = x / mag * s2;
            y = y / mag * s2;
        }
        if m.look == "RIGHT_STICK" {
            st.right_x += x;
            st.right_y += y;
        } else {
            st.left_x += x;
            st.left_y += y;
        }
    }

    fn touch(&mut self, now: f64) -> Option<(f64, f64)> {
        let t = self.config.touchpad.clone();
        let mut live = vec![];
        let mut pos = None;
        for (start, g) in &self.strokes {
            let dur = (if *g == Gesture::TOUCHPAD_TAP { t.tap_ms } else { t.swipe_ms }) / 1000.0;
            let p = (now - start) / dur;
            if p >= 1.0 {
                continue;
            } // finger lifts
            live.push((*start, *g));
            let ((x0, y0), (x1, y1)) = stroke(*g);
            pos = Some((0.5 + t.swipe_length * (x0 + (x1 - x0) * p), 0.5 + t.swipe_length * (y0 + (y1 - y0) * p)));
        }
        self.strokes = live;
        pos
    }
}
