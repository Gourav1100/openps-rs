//! Ported from Con2PC tests/test_engine_modes.py, test_panic.py and the hotkey tests.
use openps_input::config::Config;
use openps_input::engine::{Bounds, Engine, Hotkey, OverlayState, Platform};
use openps_input::keys::{Chord, Input, Key};
use openps_input::state::{Button, ControllerState};
use openps_input::{load_defaults, load_layers, InputSnapshot};
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::rc::Rc;

const WINDOW: Bounds = (100.0, 200.0, 800.0, 600.0); // centre (500, 500)

#[derive(Debug, PartialEq, Clone)]
enum Cur {
    Engage(Option<(f64, f64)>),
    Recenter((f64, f64)),
    Release,
    Hide(bool),
}

#[derive(Default)]
struct Shared {
    focused: bool,
    bounds: Option<Bounds>,
    pressed: BTreeSet<Input>,
    excl: (bool, bool),
    hot_swallow: bool,
    hotkeys: Vec<Chord>,
    cursor: Vec<Cur>,
    pads: Vec<ControllerState>,
    beeps: Vec<(String, bool)>,
    killed: Vec<String>,
    exits: Vec<bool>,
    overlay: Vec<OverlayState>,
    overlay_closed: bool,
}
type S = Rc<RefCell<Shared>>;
struct Mock(S);

impl Platform for Mock {
    fn is_frontmost(&mut self, _: &str) -> bool {
        self.0.borrow().focused
    }
    fn window_bounds(&mut self, _: &str) -> Option<Bounds> {
        self.0.borrow().bounds
    }
    fn snapshot(&mut self) -> InputSnapshot {
        InputSnapshot::new(self.0.borrow().pressed.iter().copied(), 0.0, 0.0)
    }
    fn set_exclusive(&mut self, k: bool, m: bool) {
        self.0.borrow_mut().excl = (k, m);
    }
    fn set_hotkey_swallow(&mut self, on: bool) {
        self.0.borrow_mut().hot_swallow = on;
    }
    fn set_hotkeys(&mut self, c: &[Chord]) {
        self.0.borrow_mut().hotkeys = c.to_vec();
    }
    fn cursor_engage(&mut self, c: Option<(f64, f64)>) {
        self.0.borrow_mut().cursor.push(Cur::Engage(c));
    }
    fn cursor_recenter(&mut self, c: (f64, f64)) {
        self.0.borrow_mut().cursor.push(Cur::Recenter(c));
    }
    fn cursor_release(&mut self) {
        self.0.borrow_mut().cursor.push(Cur::Release);
    }
    fn cursor_set_hide(&mut self, h: bool) {
        self.0.borrow_mut().cursor.push(Cur::Hide(h));
    }
    fn send_pad(&mut self, s: &ControllerState) {
        self.0.borrow_mut().pads.push(s.clone());
    }
    fn beep(&mut self, s: &str, f: bool) {
        self.0.borrow_mut().beeps.push((s.into(), f));
    }
    fn exit_requested(&mut self, quit_app: bool) {
        self.0.borrow_mut().exits.push(quit_app);
    }
    fn force_quit(&mut self, app: &str) -> Vec<u32> {
        self.0.borrow_mut().killed.push(app.into());
        vec![1]
    }
    fn overlay_push(&mut self, s: &OverlayState) {
        self.0.borrow_mut().overlay.push(s.clone());
    }
    fn overlay_close(&mut self) {
        self.0.borrow_mut().overlay_closed = true;
    }
}

fn rig_with(cfg: Config, focused: bool) -> (Engine<Mock>, S) {
    let s: S = Rc::new(RefCell::new(Shared { focused, bounds: Some(WINDOW), ..Default::default() }));
    let mut cfg = cfg;
    cfg.feedback_sounds = false;
    let mut e = Engine::new(cfg, Mock(s.clone()), false).unwrap();
    e.start(0.0);
    s.borrow_mut().cursor.clear();
    (e, s)
}
fn rig() -> (Engine<Mock>, S) {
    rig_with(load_defaults().unwrap(), true)
}
fn k(key: Key) -> Input {
    Input::Key(key)
}
fn held(keys: &[Key]) -> BTreeSet<Input> {
    keys.iter().map(|x| k(*x)).collect()
}

// ---- exclusive input + cursor lock -----------------------------------------------------------
#[test]
fn mapping_on_makes_input_exclusive_and_locks_cursor_to_window_centre() {
    let (mut e, s) = rig();
    e.apply_enabled(true);
    assert_eq!(s.borrow().excl, (true, true));
    assert_eq!(s.borrow().cursor.last(), Some(&Cur::Engage(Some((500.0, 500.0)))));
}

#[test]
fn mapping_off_restores_everything() {
    let (mut e, s) = rig();
    e.apply_enabled(true);
    e.apply_enabled(false);
    assert_eq!(s.borrow().excl, (false, false));
    assert_eq!(s.borrow().cursor.last(), Some(&Cur::Release));
    assert_eq!(s.borrow().pads.last(), Some(&ControllerState::default())); // neutral sent
}

#[test]
fn losing_focus_releases_cursor_and_input_then_regains_it() {
    let (mut e, s) = rig();
    e.apply_enabled(true);
    s.borrow_mut().focused = false;
    e.poll_window();
    assert!(e.enabled() && !e.live());
    assert_eq!(s.borrow().cursor.last(), Some(&Cur::Release));
    assert_eq!(s.borrow().excl, (false, false));
    s.borrow_mut().focused = true;
    e.poll_window();
    let c = s.borrow().cursor.clone();
    assert_eq!(c[c.len() - 2..], [Cur::Engage(Some((500.0, 500.0))), Cur::Recenter((500.0, 500.0))]);
    assert_eq!(s.borrow().excl, (true, true));
}

#[test]
fn cursor_follows_the_window_when_it_moves() {
    let (mut e, s) = rig();
    e.apply_enabled(true);
    s.borrow_mut().bounds = Some((0.0, 0.0, 1000.0, 800.0));
    e.poll_window();
    assert!(s.borrow().cursor.contains(&Cur::Recenter((500.0, 400.0))));
}

#[test]
fn no_cursor_lock_when_disabled_in_config() {
    let mut c = load_defaults().unwrap();
    c.mouse.capture_cursor = false;
    let (mut e, s) = rig_with(c, true);
    e.apply_enabled(true);
    assert!(!s.borrow().cursor.iter().any(|c| matches!(c, Cur::Engage(_))));
    assert_eq!(s.borrow().excl, (true, true)); // input is still exclusive
}

#[test]
fn exclusive_can_be_turned_off_in_config() {
    let mut c = load_defaults().unwrap();
    c.suppress_input = false;
    let (mut e, s) = rig_with(c, true);
    e.apply_enabled(true);
    assert_eq!(s.borrow().excl, (false, false));
}

#[test]
fn mapping_pauses_when_target_app_loses_focus() {
    let (mut e, s) = rig();
    e.apply_enabled(true);
    assert!(e.live());
    s.borrow_mut().focused = false;
    e.poll_window();
    assert!(e.enabled() && !e.live());
    s.borrow_mut().focused = true;
    e.poll_window();
    assert!(e.live());
}

#[test]
fn hotkeys_are_swallowed_only_while_target_is_frontmost() {
    let (mut e, s) = rig();
    assert!(s.borrow().hot_swallow);
    s.borrow_mut().focused = false;
    e.poll_window();
    assert!(!s.borrow().hot_swallow); // F12 still reaches a browser
    let mut c = load_defaults().unwrap();
    c.target_app = String::new();
    let (_e2, s2) = rig_with(c, false);
    assert!(s2.borrow().hot_swallow); // unspecified target: always
}

// ---- panic / quit -------------------------------------------------------------------------------
#[test]
fn panic_gives_the_cursor_back_first_and_kills_target() {
    let (mut e, s) = rig();
    e.apply_enabled(true);
    s.borrow_mut().cursor.clear();
    e.panic();
    assert_eq!(s.borrow().cursor.first(), Some(&Cur::Release));
    assert!(e.stopped() && !e.enabled());
    assert_eq!(s.borrow().excl, (false, false));
    assert_eq!(s.borrow().killed, vec!["OpenPS"]);
    assert!(e.panic().is_empty()); // single-flight
    assert_eq!(s.borrow().killed.len(), 1);
}

#[test]
fn panic_with_no_target_kills_nothing() {
    let mut c = load_defaults().unwrap();
    c.target_app = String::new();
    let (mut e, s) = rig_with(c, true);
    e.apply_enabled(true);
    e.panic();
    assert!(s.borrow().killed.is_empty() && e.stopped());
}

#[test]
fn panic_hotkey_stops_and_disables() {
    let (mut e, _s) = rig();
    e.apply_enabled(true);
    assert_eq!(e.on_press(k(Key::F11), &held(&[Key::F11])), Some(Hotkey::Panic));
    assert!(e.stopped() && !e.enabled());
}

#[test]
fn f9_asks_the_host_to_close_everything_and_f12_to_end_the_stream() {
    let (mut e, s) = rig();
    e.apply_enabled(true);
    s.borrow_mut().cursor.clear();
    assert_eq!(e.on_press(k(Key::F12), &held(&[Key::F12])), Some(Hotkey::Quit));
    assert!(
        !e.enabled() && s.borrow().cursor.first() == Some(&Cur::Release),
        "input is released before the host is told"
    );
    assert_eq!(s.borrow().exits, vec![false]);
    assert!(s.borrow().killed.is_empty(), "F12 never kills anything");
    let (mut e, s) = rig();
    e.apply_enabled(true);
    assert_eq!(e.on_press(k(Key::F11), &held(&[Key::F11])), Some(Hotkey::Panic));
    assert_eq!(s.borrow().exits, vec![true]);
}

#[test]
fn shutdown_cleans_up_and_closes_target_only_if_owned() {
    let (mut e, s) = rig();
    e.shutdown();
    assert_eq!(s.borrow().cursor.last(), Some(&Cur::Release));
    assert!(s.borrow().overlay_closed && s.borrow().killed.is_empty()); // not owned: never closed
    assert_eq!(s.borrow().pads.last(), Some(&ControllerState::default()));
    let s2: S = Rc::new(RefCell::new(Shared { focused: true, ..Default::default() }));
    let mut e2 = Engine::new(load_defaults().unwrap(), Mock(s2.clone()), true).unwrap();
    e2.shutdown();
    assert_eq!(s2.borrow().killed, vec!["OpenPS"]);
}

// ---- hotkeys ---------------------------------------------------------------------------------------
#[test]
fn all_hotkeys_are_registered_for_swallowing() {
    let (_e, s) = rig();
    let hk = s.borrow().hotkeys.clone();
    for key in [Key::F8, Key::F7, Key::F10, Key::F11, Key::F12] {
        assert!(hk.contains(&[k(key)].into_iter().collect()), "{key:?}");
    }
    assert_eq!(hk.len(), 5 + 12); // 6 slots + 6 right-Command twins
}

#[test]
fn hotkey_priority_and_chord_rules() {
    let (e, _s) = rig();
    assert_eq!(e.hotkey_for(k(Key::F12), &held(&[Key::F12])), Some(Hotkey::Quit));
    assert_eq!(e.hotkey_for(k(Key::F10), &held(&[Key::F10])), Some(Hotkey::Toggle));
    assert_eq!(e.hotkey_for(k(Key::F7), &held(&[Key::F7])), Some(Hotkey::NextProfile));
    assert_eq!(e.hotkey_for(k(Key::F8), &held(&[Key::F8])), Some(Hotkey::Overlay));
    assert_eq!(e.hotkey_for(k(Key::F2), &held(&[Key::LEFT_COMMAND, Key::F2])), Some(Hotkey::Slot(1)));
    assert_eq!(e.hotkey_for(k(Key::F2), &held(&[Key::RIGHT_COMMAND, Key::F2])), Some(Hotkey::Slot(1)));
    assert_eq!(e.hotkey_for(k(Key::F2), &held(&[Key::F2])), None); // Command not held
    assert_eq!(e.hotkey_for(k(Key::W), &held(&[Key::W])), None);
}

#[test]
fn toggle_hotkey_flips_mapping_on_the_next_tick_with_sounds() {
    let mut c = load_defaults().unwrap();
    c.feedback_sounds = true;
    let s: S = Rc::new(RefCell::new(Shared { focused: true, bounds: Some(WINDOW), ..Default::default() }));
    let mut e = Engine::new(c, Mock(s.clone()), false).unwrap();
    e.start(0.0);
    e.on_press(k(Key::F10), &held(&[Key::F10]));
    assert!(!e.enabled()); // flags only until the loop runs
    e.tick(1.0);
    assert!(e.enabled());
    e.on_press(k(Key::F10), &held(&[Key::F10]));
    e.tick(2.0);
    assert!(!e.enabled());
    let b: Vec<String> = s.borrow().beeps.iter().map(|b| b.0.clone()).collect();
    assert_eq!(b, vec!["Basso", "Pop", "Basso"]); // start(off), on, off
}

#[test]
fn profile_hotkeys_cycle_and_slots_select() {
    let (mut e, s) = rig();
    s.borrow_mut().pads.clear();
    e.on_press(k(Key::F7), &held(&[Key::F7]));
    e.tick(1.0);
    assert_eq!(e.profile(), "racing");
    assert_eq!(s.borrow().pads.last(), Some(&ControllerState::default())); // old layout released
    e.on_press(k(Key::F7), &held(&[Key::F7]));
    e.tick(2.0);
    assert_eq!(e.profile(), "assassins_creed"); // the shipped profile order is default, racing, assassins_creed
    e.on_press(k(Key::F7), &held(&[Key::F7]));
    e.tick(3.0);
    assert_eq!(e.profile(), "default"); // wraps
    e.on_press(k(Key::F2), &held(&[Key::LEFT_COMMAND, Key::F2]));
    e.tick(4.0);
    assert_eq!(e.profile(), "racing");
    e.on_press(k(Key::F3), &held(&[Key::LEFT_COMMAND, Key::F3]));
    e.tick(5.0);
    assert_eq!(e.profile(), "assassins_creed");
    e.on_press(k(Key::F5), &held(&[Key::LEFT_COMMAND, Key::F5]));
    e.tick(6.0);
    assert_eq!(e.profile(), "assassins_creed"); // no 5th profile: unchanged
}

#[test]
fn empty_slot_shows_a_notice() {
    let mut c = load_defaults().unwrap();
    c.overlay = true;
    let (mut e, s) = rig_with(c, true);
    e.on_press(k(Key::F5), &held(&[Key::LEFT_COMMAND, Key::F5]));
    e.tick(1.0);
    assert_eq!(s.borrow().overlay.last().unwrap().notice, "No profile in slot 5.");
    e.tick(8.0); // notice expires after 6 s (watchdog clears it)
    assert_eq!(s.borrow().overlay.last().unwrap().notice, "");
}

#[test]
fn live_tick_drives_the_pad_and_idle_tick_does_not() {
    let (mut e, s) = rig();
    s.borrow_mut().pressed = held(&[Key::SPACE]);
    s.borrow_mut().pads.clear();
    e.tick(1.0);
    assert!(s.borrow().pads.is_empty()); // OFF: nothing sent
    assert!((e.period() - 1.0 / 50.0).abs() < 1e-12);
    e.apply_enabled(true);
    s.borrow_mut().pads.clear();
    e.tick(2.0);
    assert!(s.borrow().pads.last().unwrap().buttons.contains(&Button::CROSS));
    assert!((e.period() - 1.0 / 1000.0).abs() < 1e-12);
}

#[test]
fn quit_hotkey_stops() {
    let (mut e, _s) = rig();
    e.on_press(k(Key::F12), &held(&[Key::F12]));
    assert!(e.stopped());
}

#[test]
fn mapping_starts_off_by_default_and_start_enabled_is_honoured() {
    let (e, _) = rig();
    assert!(!e.enabled());
    let mut c = load_defaults().unwrap();
    c.start_enabled = true;
    let (e, s) = rig_with(c, true);
    assert!(e.live());
    assert_eq!(s.borrow().excl, (true, true));
}

#[test]
fn click_mode_toggle_is_announced() {
    let (mut e, s) = rig();
    e.apply_enabled(true);
    let tab = k(Key::TAB);
    let left = Input::Mouse(openps_input::keys::MouseInput::MOUSE_LEFT);
    s.borrow_mut().pressed = [tab].into_iter().collect();
    e.tick(1.0);
    s.borrow_mut().pressed = [tab, left].into_iter().collect();
    e.tick(1.01);
    assert_eq!(s.borrow().overlay.last().unwrap().notice, "Left click now acts as RB");
    assert_eq!(s.borrow().overlay.last().unwrap().click_modes, ("RB", "LT"));
}

// ---- overlay -----------------------------------------------------------------------------------------
#[test]
fn f8_hotkey_sets_the_overlay_flag() {
    let (mut e, _s) = rig();
    assert_eq!(e.on_press(k(Key::F8), &held(&[Key::F8])), Some(Hotkey::Overlay));
    assert!(!e.overlay_wanted());
    e.tick(1.0);
    assert!(e.overlay_wanted());
}

#[test]
fn overlay_toggles_visible_and_hidden() {
    let (mut e, s) = rig();
    e.toggle_overlay();
    assert!(s.borrow().overlay.last().unwrap().visible);
    assert_eq!(s.borrow().overlay.last().unwrap().anchor, Some(WINDOW));
    e.toggle_overlay();
    assert!(!s.borrow().overlay.last().unwrap().visible);
}

#[test]
fn overlay_hides_while_target_app_is_not_frontmost() {
    let (mut e, s) = rig();
    e.toggle_overlay();
    s.borrow_mut().focused = false;
    e.poll_window();
    assert!(!s.borrow().overlay.last().unwrap().visible); // wanted, but the app is not in front
    s.borrow_mut().focused = true;
    e.poll_window();
    assert!(s.borrow().overlay.last().unwrap().visible);
}

#[test]
fn overlay_shows_mapping_state_and_profile() {
    let (mut e, s) = rig();
    e.toggle_overlay();
    assert!(!s.borrow().overlay.last().unwrap().mapping);
    e.apply_enabled(true);
    assert!(s.borrow().overlay.last().unwrap().mapping);
    e.next_profile();
    assert_eq!(s.borrow().overlay.last().unwrap().profile, "racing");
}

#[test]
fn identical_states_are_not_resent() {
    let (mut e, s) = rig();
    e.toggle_overlay();
    let n = s.borrow().overlay.len();
    e.push_overlay();
    e.push_overlay();
    assert_eq!(s.borrow().overlay.len(), n);
}

#[test]
fn overlay_follows_the_window() {
    let (mut e, s) = rig();
    e.toggle_overlay();
    s.borrow_mut().bounds = Some((10.0, 20.0, 300.0, 200.0));
    e.poll_window();
    assert_eq!(s.borrow().overlay.last().unwrap().anchor, Some((10.0, 20.0, 300.0, 200.0)));
}

#[test]
fn overlay_disabled_in_config_is_harmless() {
    let mut c = load_defaults().unwrap();
    c.overlay = false;
    let (mut e, s) = rig_with(c, true);
    e.toggle_overlay();
    assert!(!e.overlay_wanted() && s.borrow().overlay.is_empty());
}

// ---- config hot reload -------------------------------------------------------------------------------
#[test]
fn reload_keeps_profile_click_modes_and_updates_hotkeys() {
    let (mut e, s) = rig();
    e.next_profile(); // racing
    let tab = k(Key::TAB);
    let left = Input::Mouse(openps_input::keys::MouseInput::MOUSE_LEFT);
    e.mapper().map(&InputSnapshot::new([tab], 0.0, 0.0), 0.0);
    e.mapper().map(&InputSnapshot::new([tab, left], 0.0, 0.0), 0.01);
    assert_eq!(e.mapper().click_modes().0, "RB");
    let new = load_layers(Some("toggle = \"LEFT_CONTROL+F5\"\n"), false).unwrap();
    e.apply_config(new, None).unwrap();
    assert_eq!(e.profile(), "racing");
    assert_eq!(e.mapper().click_modes().0, "RB"); // session choice kept
    assert!(s.borrow().hotkeys.contains(&[k(Key::LEFT_CONTROL), k(Key::F5)].into_iter().collect()));
    assert_eq!(e.hotkey_for(k(Key::F5), &held(&[Key::LEFT_CONTROL, Key::F5])), Some(Hotkey::Toggle));
    // a removed profile falls back to the first one; `select` picks a named one
    let gone = load_layers(Some("deleted_profiles = [\"racing\"]\n"), false).unwrap();
    e.apply_config(gone, None).unwrap();
    assert_eq!(e.profile(), "default");
    e.apply_config(load_defaults().unwrap(), Some("racing")).unwrap();
    assert_eq!(e.profile(), "racing");
}

#[test]
fn the_mouse_is_ignored_for_a_moment_after_the_cursor_is_captured() {
    // a platform whose mouse delta is huge on every read
    struct Lurch(S);
    impl Platform for Lurch {
        fn is_frontmost(&mut self, _: &str) -> bool {
            true
        }
        fn window_bounds(&mut self, _: &str) -> Option<Bounds> {
            Some(WINDOW)
        }
        fn snapshot(&mut self) -> InputSnapshot {
            InputSnapshot::new(std::iter::empty(), 400.0, 0.0)
        }
        fn set_exclusive(&mut self, _: bool, _: bool) {}
        fn set_hotkey_swallow(&mut self, _: bool) {}
        fn set_hotkeys(&mut self, _: &[Chord]) {}
        fn cursor_engage(&mut self, _: Option<(f64, f64)>) {}
        fn cursor_recenter(&mut self, _: (f64, f64)) {}
        fn cursor_release(&mut self) {}
        fn cursor_set_hide(&mut self, _: bool) {}
        fn send_pad(&mut self, s: &ControllerState) {
            self.0.borrow_mut().pads.push(s.clone());
        }
        fn beep(&mut self, _: &str, _: bool) {}
        fn exit_requested(&mut self, _: bool) {}
        fn force_quit(&mut self, _: &str) -> Vec<u32> {
            vec![]
        }
        fn overlay_push(&mut self, _: &OverlayState) {}
        fn overlay_close(&mut self) {}
    }
    let s: S = Rc::new(RefCell::new(Shared::default()));
    let mut cfg = load_defaults().unwrap();
    cfg.feedback_sounds = false;
    let mut e = Engine::new(cfg, Lurch(s.clone()), false).unwrap();
    e.start(0.0);
    e.tick(1.0);
    e.apply_enabled(true);
    s.borrow_mut().pads.clear();
    e.tick(1.01);
    assert!(s.borrow().pads.iter().all(|p| p.right_x == 0.0), "no camera movement in the first moments");
    e.tick(1.5);
    assert!(s.borrow().pads.last().unwrap().right_x != 0.0, "the mouse works again afterwards");
}
