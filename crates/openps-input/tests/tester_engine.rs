//! Tester A4: engine state machine corner cases (focus loss, stuck input, hotkeys, hostile UI events).
use openps_input::config::Config;
use openps_input::engine::{Bounds, Engine, Hotkey, OverlayState, Platform, UiEvent};
use openps_input::keys::{Chord, Input, Key, MouseInput};
use openps_input::state::ControllerState;
use openps_input::{load_defaults, load_layers, InputSnapshot};
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::rc::Rc;

#[derive(Default)]
struct Shared {
    focused: bool,
    pressed: BTreeSet<Input>,
    snapshots: usize,
    cursor: Vec<String>,
    pads: Vec<ControllerState>,
    exits: Vec<bool>,
    excl: Vec<(bool, bool)>,
    overlays: usize,
    key_log: Vec<(Key, bool)>,
}
type S = Rc<RefCell<Shared>>;
struct Mock(S);
impl Platform for Mock {
    fn is_frontmost(&mut self, _: &str) -> bool {
        self.0.borrow().focused
    }
    fn window_bounds(&mut self, _: &str) -> Option<Bounds> {
        Some((0.0, 0.0, 800.0, 600.0))
    }
    fn snapshot(&mut self) -> InputSnapshot {
        let mut s = self.0.borrow_mut();
        s.snapshots += 1;
        InputSnapshot::new(s.pressed.iter().copied(), 0.0, 0.0)
    }
    fn set_exclusive(&mut self, k: bool, m: bool) {
        self.0.borrow_mut().excl.push((k, m));
    }
    fn set_hotkey_swallow(&mut self, _: bool) {}
    fn set_hotkeys(&mut self, _: &[Chord]) {}
    fn cursor_engage(&mut self, _: Option<(f64, f64)>) {
        self.0.borrow_mut().cursor.push("engage".into());
    }
    fn cursor_recenter(&mut self, _: (f64, f64)) {}
    fn cursor_release(&mut self) {
        self.0.borrow_mut().cursor.push("release".into());
    }
    fn cursor_set_hide(&mut self, _: bool) {}
    fn send_pad(&mut self, st: &ControllerState) {
        self.0.borrow_mut().pads.push(st.clone());
    }
    fn beep(&mut self, _: &str, _: bool) {}
    fn force_quit(&mut self, _: &str) -> Vec<u32> {
        vec![]
    }
    fn exit_requested(&mut self, q: bool) {
        self.0.borrow_mut().exits.push(q);
    }
    fn overlay_push(&mut self, _: &OverlayState) {
        self.0.borrow_mut().overlays += 1;
    }
    fn start_key_log(&mut self) {}
    fn stop_key_log(&mut self) {}
    fn drain_key_log(&mut self) -> Vec<(Key, bool)> {
        std::mem::take(&mut self.0.borrow_mut().key_log)
    }
}

fn k(x: Key) -> Input {
    Input::Key(x)
}
fn engine_with(cfg: Config) -> (Engine<Mock>, S) {
    let s: S = Default::default();
    s.borrow_mut().focused = true;
    let mut e = Engine::new(cfg, Mock(s.clone()), false).unwrap();
    e.start(0.0);
    (e, s)
}
fn live_engine() -> (Engine<Mock>, S) {
    let mut c = load_defaults().unwrap();
    c.start_enabled = true;
    c.feedback_sounds = false;
    engine_with(c)
}
fn neutral(p: &ControllerState) -> bool {
    p.buttons.is_empty()
        && p.left_x == 0.0
        && p.left_y == 0.0
        && p.right_x == 0.0
        && p.right_y == 0.0
        && p.l2 == 0.0
        && p.r2 == 0.0
}

#[test]
fn focus_loss_with_keys_held_sends_neutral_releases_cursor_and_stops_reading_input() {
    let (mut e, s) = live_engine();
    s.borrow_mut().pressed = [k(Key::W), k(Key::SPACE), Input::Mouse(MouseInput::MOUSE_LEFT)].into_iter().collect();
    let mut t = 0.0;
    for _ in 0..20 {
        t += 0.01;
        e.tick(t);
    }
    assert!(!neutral(s.borrow().pads.last().unwrap()), "mapping is live and driving the pad");
    // focus is lost while the keys are still (as far as the OS is concerned) held down
    s.borrow_mut().focused = false;
    t += 0.2;
    e.tick(t);
    let snaps = s.borrow().snapshots;
    let pads_after_loss = s.borrow().pads.len();
    assert!(neutral(s.borrow().pads.last().unwrap()), "last pad state must be neutral after focus loss");
    assert_eq!(s.borrow().cursor.last().map(String::as_str), Some("release"));
    for _ in 0..50 {
        t += 0.02;
        e.tick(t);
    }
    assert_eq!(s.borrow().snapshots, snaps, "input must not be read (grabbed) while another app is in front");
    assert_eq!(s.borrow().pads.len(), pads_after_loss, "no pad traffic while unfocused");
    // focus returns with the key still down: mapping resumes from a clean state (no stale ramp)
    s.borrow_mut().focused = true;
    t += 0.2;
    e.tick(t);
    assert_eq!(s.borrow().cursor.last().map(String::as_str), Some("engage"));
}

#[test]
fn rapid_focus_flapping_keeps_cursor_calls_balanced() {
    let (mut e, s) = live_engine();
    let mut t = 0.0;
    for i in 0..200 {
        s.borrow_mut().focused = i % 2 == 0;
        t += 0.11;
        e.tick(t);
    }
    let c = s.borrow().cursor.clone();
    let engages = c.iter().filter(|x| *x == "engage").count();
    let releases = c.iter().filter(|x| *x == "release").count();
    assert!(releases >= engages, "every engage needs a release ({engages} vs {releases})");
    // the last state decides: 199 is odd => unfocused => released
    assert_eq!(c.last().map(String::as_str), Some("release"));
}

#[test]
fn quit_and_panic_hotkeys_release_input_and_notify_once() {
    let (mut e, s) = live_engine();
    e.request(Hotkey::Panic);
    e.request(Hotkey::Panic); // double press: single flight for the kill, exit requested per press
    assert!(e.stopped() && !e.enabled());
    assert!(s.borrow().cursor.iter().any(|c| c == "release"));
    assert_eq!(s.borrow().excl.last(), Some(&(false, false)), "keyboard/mouse grab dropped");
    assert_eq!(s.borrow().exits, vec![true, true]);
    let (mut e, s) = live_engine();
    e.request(Hotkey::Quit);
    assert!(e.stopped() && !e.enabled());
    assert_eq!(s.borrow().exits, vec![false]);
}

#[test]
fn quit_and_panic_send_a_neutral_pad_immediately() {
    for hk in [Hotkey::Quit, Hotkey::Panic] {
        let (mut e, s) = live_engine();
        s.borrow_mut().pressed = [k(Key::W), k(Key::SPACE)].into_iter().collect();
        let mut t = 0.0;
        for _ in 0..10 {
            t += 0.01;
            e.tick(t);
        }
        e.request(hk);
        t += 0.01;
        e.tick(t);
        assert!(neutral(s.borrow().pads.last().unwrap()), "{hk:?}");
    }
}

#[test]
fn toggling_off_with_keys_held_sends_neutral() {
    let (mut e, s) = live_engine();
    s.borrow_mut().pressed = [k(Key::W)].into_iter().collect();
    e.tick(0.01);
    e.tick(0.02);
    e.request(Hotkey::Toggle);
    e.tick(0.03);
    assert!(!e.enabled());
    assert!(neutral(s.borrow().pads.last().unwrap()));
    assert_eq!(s.borrow().cursor.last().map(String::as_str), Some("release"));
}

#[test]
fn hotkey_chords_fire_regardless_of_press_order_but_not_for_partial_chords() {
    let mut c = load_layers(Some("toggle = \"LEFT_CONTROL+F10\""), false).unwrap();
    c.feedback_sounds = false;
    let (e, _s) = engine_with(c);
    let ctl = k(Key::LEFT_CONTROL);
    let f10 = k(Key::F10);
    let held = |v: &[Input]| v.iter().copied().collect::<BTreeSet<_>>();
    assert_eq!(e.hotkey_for(f10, &held(&[f10])), None, "F10 alone");
    assert_eq!(e.hotkey_for(ctl, &held(&[ctl])), None, "Ctrl alone");
    assert_eq!(e.hotkey_for(f10, &held(&[ctl, f10])), Some(Hotkey::Toggle), "ctrl first");
    assert_eq!(e.hotkey_for(ctl, &held(&[ctl, f10])), Some(Hotkey::Toggle), "F10 first");
    // an unrelated key pressed while the chord is held does not re-fire
    assert_eq!(e.hotkey_for(k(Key::W), &held(&[ctl, f10, k(Key::W)])), None);
    // shipped slots: Command+F1 ... and the right-Command twin
    let (e, _s) = live_engine();
    let f1 = k(Key::F1);
    for cmd in [Key::LEFT_COMMAND, Key::RIGHT_COMMAND] {
        assert_eq!(e.hotkey_for(f1, &held(&[k(cmd), f1])), Some(Hotkey::Slot(0)), "{cmd:?}");
    }
    assert_eq!(e.hotkey_for(f1, &held(&[f1])), None);
    // priority: panic wins over everything else on the same press
    assert_eq!(e.hotkey_for(k(Key::F11), &held(&[k(Key::F11)])), Some(Hotkey::Panic));
}

#[test]
fn invalid_hotkeys_are_rejected_without_changing_the_running_engine() {
    let (mut e, _s) = live_engine();
    let mut bad = e.cfg.clone();
    bad.toggle = "NOT_A_KEY".into();
    assert!(e.apply_config(bad.clone(), None).is_err());
    assert_eq!(e.cfg.toggle, "F10");
    let s: S = Default::default();
    assert!(Engine::new(bad, Mock(s), false).is_err());
    let mut e2 = e.cfg.clone();
    e2.profile_slots = vec!["".into()];
    assert!(e.apply_config(e2, None).is_err());
}

#[test]
fn select_slot_out_of_range_and_profile_cycling_wrap() {
    let (mut e, s) = live_engine();
    let n = e.cfg.profile_names().len();
    e.select_slot(5);
    assert_eq!(e.profile(), "default");
    for _ in 0..n * 3 + 1 {
        e.next_profile();
    }
    assert_eq!(e.profile(), e.cfg.profile_names()[1 % n]);
    assert!(neutral(s.borrow().pads.last().unwrap()), "profile switch releases everything");
}

#[test]
fn hostile_ui_events_never_panic() {
    use serde_json::json;
    let (mut e, _s) = live_engine();
    let blobs = [
        json!(null),
        json!([]),
        json!("edit"),
        json!({"type": 5}),
        json!({"type": "edit"}),
        json!({"type": "edit", "control": 5, "slot": 1}),
        json!({"type": "edit", "control": "CROSS", "slot": "1"}),
        json!({"type": "edit", "control": "CROSS", "slot": i64::MAX}),
        json!({"type": "edit", "control": "CROSS", "slot": i64::MIN}),
        json!({"type": "edit", "control": "CROSS", "slot": 0}),
        json!({"type": "edit", "control": "x".repeat(100_000), "slot": 1}),
        json!({"type": "clear", "control": "\u{0}", "slot": 2}),
        json!({"type": "pick", "input": 5}),
        json!({"type": "pick", "input": "MOUSE_LEFT"}),
        json!({"type": "slider", "value": "fast"}),
        json!({"type": "slider", "value": 1e308}),
        json!({"type": "slider", "value": -1e308, "final": true}),
        json!({"type": "profile_delete", "name": "default"}),
        json!({"type": "profile_rename", "name": {"a": 1}}),
        json!({"type": "profile_select", "name": "\u{202e}"}),
        json!({"type": "profile_add"}),
        json!({"type": "profile_reset"}),
        json!({"type": "unknown"}),
    ];
    for (i, b) in blobs.iter().enumerate() {
        if let Some(ev) = UiEvent::from_json(b) {
            e.push_ui_event(ev);
        }
        e.tick(1.0 + i as f64);
    }
    assert!(!e.renaming() && !e.capturing(), "mapping is ON, so every edit must be refused");
    assert_eq!(UiEvent::from_json(&json!({"type": "edit", "control": "CROSS", "slot": 1.5})), None);
}

#[test]
fn slider_bounds_nan_and_inf() {
    let d = tempfile::TempDir::new().unwrap();
    let p = d.path().join("u.toml");
    std::fs::write(&p, "").unwrap();
    let s: S = Default::default();
    s.borrow_mut().focused = true;
    let mut c = load_layers(Some(""), false).unwrap();
    c.feedback_sounds = false;
    let mut e = Engine::new(c, Mock(s.clone()), false).unwrap().with_config_path(&p);
    e.start(0.0);
    e.toggle_overlay();
    e.tick(0.2); // overlay visible, mapping off
    for (v, want) in [
        (f64::NAN, 0.05),
        (f64::INFINITY, 0.3),
        (f64::NEG_INFINITY, 0.01),
        (1e300, 0.3),
        (-5.0, 0.01),
        (0.1234567, 0.1235),
    ] {
        e.handle_ui_event(UiEvent::Slider { value: v, final_: false });
        assert!((e.cfg.mouse.sensitivity - want).abs() < 1e-9, "{v} -> {}", e.cfg.mouse.sensitivity);
        assert!((e.mapper().config.mouse.sensitivity - want).abs() < 1e-9);
    }
    e.handle_ui_event(UiEvent::Slider { value: 0.2, final_: true });
    let saved = openps_input::config::load_path(&p, false).unwrap();
    assert!((saved.mouse.sensitivity - 0.2).abs() < 1e-9);
}

#[test]
fn rename_typing_filters_characters_and_caps_length() {
    use openps_input::textinput::{apply_key, key_to_char, MAX_LEN};
    // all keys: only [A-Za-z0-9_-] can ever be typed
    for key in Key::ALL {
        for shift in [false, true] {
            if let Some(c) = key_to_char(*key, shift) {
                assert!(c.is_ascii_alphanumeric() || c == '_' || c == '-', "{key:?} {shift} -> {c:?}");
            }
        }
    }
    let mut t = String::new();
    for _ in 0..100 {
        t = apply_key(&t, Key::A, false).0;
    }
    assert_eq!(t.len(), MAX_LEN);
    assert_eq!(apply_key("", Key::BACKSPACE, false).0, "");
    assert_eq!(apply_key("ab", Key::KEY_1, true).0, "ab"); // shifted digit is a symbol
    assert_eq!(apply_key("ab", Key::MINUS, true).0, "ab_");
    // the typed text always passes the name validator once non-empty
    assert!(openps_input::config::valid_profile_name(&t));
}

#[test]
fn many_queued_ui_events_are_drained_in_one_tick() {
    let (mut e, _s) = live_engine();
    for _ in 0..200_000 {
        e.push_ui_event(UiEvent::OverlayClick);
    }
    let t = std::time::Instant::now();
    e.tick(1.0);
    assert!(t.elapsed().as_secs() < 5);
}

#[test]
fn select_slot_with_an_absurd_index_does_not_panic() {
    let (mut e, _s) = live_engine();
    e.select_slot(usize::MAX);
    assert_eq!(e.profile(), "default");
}

#[test]
fn cursor_stays_captured_until_the_next_focus_poll_after_focus_loss_info() {
    // Info (T07-17): focus is polled every FOCUS_POLL_S (100 ms). After Cmd+Tab the cursor stays frozen/hidden and the
    // pad keeps being driven by mouse deltas for up to 100 ms. (Keys are already cleared by the hub on blur.)
    use openps_input::engine::FOCUS_POLL_S;
    let (mut e, s) = live_engine();
    e.tick(1.0);
    s.borrow_mut().focused = false;
    e.tick(1.0 + FOCUS_POLL_S * 0.2);
    assert_eq!(s.borrow().cursor.last().map(String::as_str), Some("engage"), "still captured 20 ms after blur");
    e.tick(1.0 + FOCUS_POLL_S * 1.1);
    assert_eq!(s.borrow().cursor.last().map(String::as_str), Some("release"));
}

#[test]
fn after_quit_the_toggle_hotkey_cannot_re_enable_mapping() {
    let (mut e, s) = live_engine();
    e.request(Hotkey::Quit);
    s.borrow_mut().cursor.clear();
    e.request(Hotkey::Toggle);
    e.tick(1.0);
    assert!(!e.enabled() && e.stopped());
    assert!(!s.borrow().cursor.iter().any(|c| c == "engage"));
}
