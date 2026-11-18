//! Thread-safe collection point for input events. The UI thread feeds it (DOM key and button events); the engine
//! thread reads a snapshot from it. It also remembers whether the stream window has focus and where it is.

use crate::domkeys;
use openps_input::engine::{Bounds, Hotkey, UiEvent};
use openps_input::keys::{Input, Key, MouseInput};
use std::collections::{BTreeSet, VecDeque};
use std::sync::mpsc::Sender;
use std::sync::{Mutex, MutexGuard, PoisonError};

/// Messages for the engine thread.
#[derive(Debug)]
pub enum Msg {
    /// A key or button went down (not auto-repeat); carries everything held at that moment (hotkey chords).
    Press(Input, BTreeSet<Input>),
    Ui(UiEvent),
    Hotkey(Hotkey),
    /// Something was released: tick now instead of at the next poll (a release must not be slower than a press).
    Wake,
    Stop,
}

#[derive(Default)]
struct HubState {
    pressed: BTreeSet<Input>,
    /// Momentary inputs (scroll ticks): visible for exactly one snapshot.
    pulses: BTreeSet<Input>,
    focused: bool,
    bounds: Option<Bounds>,
    log: Option<VecDeque<(Key, bool)>>,
}

#[derive(Default)]
pub struct InputHub {
    state: Mutex<HubState>,
    tx: Mutex<Option<Sender<Msg>>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Cap on a single wheel event, so a hostile value cannot do anything but "one tick".
const MAX_LOG: usize = 256;

impl InputHub {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn attach(&self, tx: Sender<Msg>) {
        *lock(&self.tx) = Some(tx);
    }
    pub fn detach(&self) {
        *lock(&self.tx) = None;
    }
    pub fn send(&self, m: Msg) {
        if let Some(tx) = lock(&self.tx).as_ref() {
            let _ = tx.send(m); // the engine thread may already be gone: nothing to do
        }
    }

    fn pressed_input(&self, inp: Input, down: bool, repeat: bool) {
        let press = {
            let mut s = lock(&self.state);
            if down && !s.focused {
                return; // a key-down while unfocused would never get its key-up: it would stay "held"
            }
            if down {
                let fresh = s.pressed.insert(inp);
                if fresh && !repeat {
                    let shift = s.pressed.contains(&Input::Key(Key::LEFT_SHIFT))
                        || s.pressed.contains(&Input::Key(Key::RIGHT_SHIFT));
                    if let (Input::Key(k), Some(log)) = (inp, s.log.as_mut()) {
                        if log.len() < MAX_LOG {
                            log.push_back((k, shift));
                        }
                    }
                }
                (!repeat && fresh).then(|| s.pressed.clone())
            } else {
                let was = s.pressed.remove(&inp);
                if was {
                    drop(s);
                    self.send(Msg::Wake);
                }
                return;
            }
        };
        if let Some(p) = press {
            self.send(Msg::Press(inp, p));
        }
    }

    /// A DOM key event. Unknown codes are ignored.
    pub fn key_event(&self, code: &str, down: bool, repeat: bool) {
        if let Some(k) = domkeys::key_from_dom_code(code) {
            self.pressed_input(Input::Key(k), down, repeat);
        }
    }

    /// A DOM mouse button event.
    pub fn button_event(&self, button: i32, down: bool) {
        if let Some(m) = domkeys::mouse_from_button(button) {
            self.pressed_input(Input::Mouse(m), down, false);
        }
    }

    /// A wheel event: one tick in the dominant direction of each axis, whatever the magnitude.
    pub fn wheel(&self, dx: f64, dy: f64) {
        let mut s = lock(&self.state);
        if !s.focused {
            return;
        }
        if dy.is_finite() && dy != 0.0 {
            s.pulses.insert(Input::Mouse(if dy < 0.0 { MouseInput::SCROLL_UP } else { MouseInput::SCROLL_DOWN }));
        }
        if dx.is_finite() && dx != 0.0 {
            s.pulses.insert(Input::Mouse(if dx < 0.0 { MouseInput::SCROLL_LEFT } else { MouseInput::SCROLL_RIGHT }));
        }
    }

    /// Focus changes. Losing focus releases everything: a key-up that arrives while another window is in front would
    /// otherwise never be seen, leaving the key stuck down.
    pub fn set_focused(&self, focused: bool) {
        let mut s = lock(&self.state);
        s.focused = focused;
        if !focused {
            s.pressed.clear();
            s.pulses.clear();
        }
    }
    pub fn focused(&self) -> bool {
        lock(&self.state).focused
    }

    pub fn set_bounds(&self, b: Option<Bounds>) {
        lock(&self.state).bounds = b.filter(|b| {
            b.0.is_finite() && b.1.is_finite() && b.2 > 0.0 && b.3 > 0.0 && b.2.is_finite() && b.3.is_finite()
        });
    }
    pub fn bounds(&self) -> Option<Bounds> {
        lock(&self.state).bounds
    }

    pub fn release_all(&self) {
        let mut s = lock(&self.state);
        s.pressed.clear();
        s.pulses.clear();
    }

    /// Drop pending scroll ticks.
    pub fn clear_pulses(&self) {
        lock(&self.state).pulses.clear();
    }

    /// What is held right now, plus this tick's pulses (which are consumed).
    pub fn take_pressed(&self) -> BTreeSet<Input> {
        let mut s = lock(&self.state);
        let mut p = s.pressed.clone();
        p.extend(std::mem::take(&mut s.pulses));
        p
    }
    pub fn pressed(&self) -> BTreeSet<Input> {
        lock(&self.state).pressed.clone()
    }

    pub fn start_key_log(&self) {
        lock(&self.state).log = Some(VecDeque::new());
    }
    pub fn stop_key_log(&self) {
        lock(&self.state).log = None;
    }
    pub fn drain_key_log(&self) -> Vec<(Key, bool)> {
        lock(&self.state).log.as_mut().map(|q| q.drain(..).collect()).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;

    fn k(k: Key) -> Input {
        Input::Key(k)
    }

    #[test]
    fn press_hold_release_and_repeat() {
        let h = InputHub::new();
        h.set_focused(true);
        let (tx, rx) = channel();
        h.attach(tx);
        h.key_event("KeyW", true, false);
        h.key_event("KeyW", true, true); // auto-repeat: still held, not a new press
        h.key_event("ShiftLeft", true, false);
        assert_eq!(h.pressed(), [k(Key::W), k(Key::LEFT_SHIFT)].into_iter().collect());
        let msgs: Vec<_> = rx.try_iter().collect();
        assert_eq!(msgs.len(), 2, "one press message per physical press: {msgs:?}");
        assert!(
            matches!(&msgs[1], Msg::Press(i, held) if *i == k(Key::LEFT_SHIFT) && held.contains(&k(Key::W))),
            "{msgs:?}"
        );
        h.key_event("KeyW", false, false);
        assert_eq!(h.pressed(), [k(Key::LEFT_SHIFT)].into_iter().collect());
        h.key_event("KeyW", false, false); // duplicate release is harmless
        h.key_event("Unidentified", true, false); // unknown code ignored
        assert_eq!(h.pressed().len(), 1);
    }

    #[test]
    fn mouse_buttons_and_pulses() {
        let h = InputHub::new();
        h.set_focused(true);
        h.button_event(0, true);
        h.button_event(9, true); // unknown
        assert!(h.pressed().contains(&Input::Mouse(MouseInput::MOUSE_LEFT)));
        h.button_event(0, false);
        assert!(h.pressed().is_empty());
        h.wheel(0.0, -3.0);
        h.wheel(f64::NAN, f64::INFINITY * 0.0);
        h.wheel(5.0, 1e300);
        let p = h.take_pressed();
        assert!(p.contains(&Input::Mouse(MouseInput::SCROLL_UP)) && p.contains(&Input::Mouse(MouseInput::SCROLL_DOWN)));
        assert!(p.contains(&Input::Mouse(MouseInput::SCROLL_RIGHT)));
        assert!(h.take_pressed().is_empty(), "a scroll tick is visible for exactly one snapshot");
    }

    #[test]
    fn losing_focus_releases_everything() {
        let h = InputHub::new();
        h.set_focused(true);
        h.key_event("KeyA", true, false);
        h.button_event(2, true);
        h.wheel(0.0, 1.0);
        h.set_focused(false);
        assert!(h.pressed().is_empty() && h.take_pressed().is_empty() && !h.focused());
    }

    #[test]
    fn bounds_must_be_sane() {
        let h = InputHub::new();
        h.set_bounds(Some((10.0, 20.0, 800.0, 600.0)));
        assert_eq!(h.bounds(), Some((10.0, 20.0, 800.0, 600.0)));
        for bad in
            [(0.0, 0.0, 0.0, 10.0), (0.0, 0.0, -1.0, 10.0), (f64::NAN, 0.0, 1.0, 1.0), (0.0, 0.0, f64::INFINITY, 1.0)]
        {
            h.set_bounds(Some(bad));
            assert_eq!(h.bounds(), None, "{bad:?}");
        }
    }

    #[test]
    fn key_log_records_fresh_presses_with_shift_state_and_is_bounded() {
        let h = InputHub::new();
        h.set_focused(true);
        h.key_event("KeyA", true, false); // before logging starts: not recorded
        h.start_key_log();
        h.key_event("ShiftLeft", true, false);
        h.key_event("KeyB", true, false);
        h.key_event("KeyB", true, true);
        h.key_event("ShiftLeft", false, false);
        h.key_event("KeyC", true, false);
        let l = h.drain_key_log();
        assert_eq!(l, vec![(Key::LEFT_SHIFT, true), (Key::B, true), (Key::C, false)]);
        for _ in 0..1000 {
            h.key_event("KeyD", true, false);
            h.key_event("KeyD", false, false);
        }
        assert!(h.drain_key_log().len() <= MAX_LOG);
        h.stop_key_log();
        assert!(h.drain_key_log().is_empty());
    }

    #[test]
    fn a_dead_engine_thread_does_not_break_the_ui_thread() {
        let h = InputHub::new();
        let (tx, rx) = channel();
        h.attach(tx);
        drop(rx);
        h.key_event("KeyA", true, false); // send fails silently
        h.send(Msg::Stop);
    }
}
