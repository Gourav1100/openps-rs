//! Ported from Con2PC tests/test_engine_modes.py (rebinding) and test_features_engine.py
//! (rename, slider, Escape hold, mouse pick, reset).
use openps_input::config::{load_defaults, load_path, Config};
use openps_input::engine::{Bounds, Engine, OverlayState, Platform, UiEvent};
use openps_input::keys::{Chord, Input, Key, MouseInput};
use openps_input::state::{Button, ControllerState};
use openps_input::InputSnapshot;
use std::cell::RefCell;
use std::collections::{BTreeSet, VecDeque};
use std::rc::Rc;
use tempfile::TempDir;

const WINDOW: Bounds = (100.0, 200.0, 800.0, 600.0);

#[derive(Default)]
struct Shared {
    focused: bool,
    script: Vec<BTreeSet<Input>>,
    excl: (bool, bool),
    key_log: Option<VecDeque<(Key, bool)>>,
    overlay: Vec<OverlayState>,
}
type S = Rc<RefCell<Shared>>;
struct Mock(S);

impl Platform for Mock {
    fn is_frontmost(&mut self, _: &str) -> bool {
        self.0.borrow().focused
    }
    fn window_bounds(&mut self, _: &str) -> Option<Bounds> {
        Some(WINDOW)
    }
    fn snapshot(&mut self) -> InputSnapshot {
        let mut s = self.0.borrow_mut();
        let pressed =
            if s.script.len() > 1 { s.script.remove(0) } else { s.script.first().cloned().unwrap_or_default() };
        InputSnapshot::new(pressed, 0.0, 0.0)
    }
    fn set_exclusive(&mut self, k: bool, m: bool) {
        self.0.borrow_mut().excl = (k, m);
    }
    fn set_hotkey_swallow(&mut self, _: bool) {}
    fn set_hotkeys(&mut self, _: &[Chord]) {}
    fn cursor_engage(&mut self, _: Option<(f64, f64)>) {}
    fn cursor_recenter(&mut self, _: (f64, f64)) {}
    fn cursor_release(&mut self) {}
    fn cursor_set_hide(&mut self, _: bool) {}
    fn send_pad(&mut self, _: &ControllerState) {}
    fn beep(&mut self, _: &str, _: bool) {}
    fn force_quit(&mut self, _: &str) -> Vec<u32> {
        vec![]
    }
    fn overlay_push(&mut self, s: &OverlayState) {
        self.0.borrow_mut().overlay.push(s.clone());
    }
    fn start_key_log(&mut self) {
        self.0.borrow_mut().key_log = Some(VecDeque::new());
    }
    fn stop_key_log(&mut self) {
        self.0.borrow_mut().key_log = None;
    }
    fn drain_key_log(&mut self) -> Vec<(Key, bool)> {
        self.0.borrow_mut().key_log.as_mut().map(|q| q.drain(..).collect()).unwrap_or_default()
    }
}

struct Rig {
    e: Engine<Mock>,
    s: S,
    path: std::path::PathBuf,
    t: f64,
    _d: TempDir,
}

impl Rig {
    fn new() -> Self {
        let d = TempDir::new().unwrap();
        let path = d.path().join("user.toml");
        let s: S = Rc::new(RefCell::new(Shared { focused: true, ..Default::default() }));
        let mut cfg = load_defaults().unwrap();
        cfg.feedback_sounds = false;
        let mut e = Engine::new(cfg, Mock(s.clone()), false).unwrap().with_config_path(&path);
        e.start(100.0);
        e.toggle_overlay(); // a click can only happen on a visible overlay
        Rig { e, s, path, t: 100.0, _d: d }
    }
    fn script(&self, frames: &[&[Input]]) {
        self.s.borrow_mut().script = frames.iter().map(|f| f.iter().copied().collect()).collect();
    }
    fn tick(&mut self) {
        self.t += 0.1;
        self.e.poll_capture(self.t);
        self.e.set_time(self.t);
    }
    fn ticks(&mut self, n: usize) {
        for _ in 0..n {
            self.tick();
        }
    }
    fn ui(&mut self, v: serde_json::Value) {
        if let Some(ev) = UiEvent::from_json(&v) {
            self.e.handle_ui_event(ev);
        }
    }
    fn edit(&mut self, control: &str, slot: i64) {
        self.ui(serde_json::json!({"type": "edit", "control": control, "slot": slot}));
    }
    fn feed(&mut self, keys: &[(Key, bool)]) {
        self.s.borrow_mut().key_log.as_mut().unwrap().extend(keys.iter().copied());
        self.t += 0.1;
        self.e.tick(self.t);
    }
    fn saved(&self) -> Config {
        load_path(&self.path, false).unwrap()
    }
    fn notice(&self) -> String {
        self.s.borrow().overlay.last().unwrap().notice.clone()
    }
    fn last(&self) -> OverlayState {
        self.s.borrow().overlay.last().unwrap().clone()
    }
    fn excl(&self) -> (bool, bool) {
        self.s.borrow().excl
    }
    fn bindings(&self, p: &str, c: &str) -> Vec<String> {
        self.saved().profile_bindings(p)[c].clone()
    }
}
fn k(key: Key) -> Input {
    Input::Key(key)
}
fn plain(keys: &[Key]) -> Vec<(Key, bool)> {
    keys.iter().map(|k| (*k, false)).collect()
}

// ---- rebinding from the overlay ----------------------------------------------------------------------
#[test]
fn editing_is_refused_while_mapping_is_on() {
    let mut r = Rig::new();
    r.e.apply_enabled(true);
    r.edit("CROSS", 1);
    assert!(!r.e.capturing() && r.notice().contains("Turn mapping OFF"));
}

#[test]
fn click_starts_capture_with_keys_held_back_but_mouse_free() {
    let mut r = Rig::new();
    r.script(&[&[Input::Mouse(MouseInput::MOUSE_LEFT)]]);
    r.edit("CROSS", 1);
    assert!(r.e.capturing());
    assert_eq!(r.excl(), (true, false));
    let c = r.last().capturing.unwrap();
    assert_eq!((c.control.as_str(), c.slot), ("CROSS", 1));
}

#[test]
fn capture_saves_the_pressed_key_and_applies_it_live() {
    let mut r = Rig::new();
    let l = Input::Mouse(MouseInput::MOUSE_LEFT);
    r.script(&[&[l]]);
    r.edit("CROSS", 1);
    r.script(&[&[l], &[l, k(Key::Z)], &[k(Key::Z)], &[]]);
    r.ticks(4);
    assert!(!r.e.capturing());
    assert_eq!(r.bindings("default", "CROSS")[0], "Z"); // persisted
    let st = r.e.mapper().map(&InputSnapshot::new([k(Key::Z)], 0.0, 0.0), 0.0);
    assert!(st.buttons.contains(&Button::CROSS)); // live
    assert_eq!(r.excl(), (false, false)); // input back to normal
    assert!(r.notice().contains("CROSS (primary) = Z"));
    assert_eq!(r.last().rows.iter().find(|x| x.control == "CROSS").unwrap().primary, "Z");
    // the shipped defaults are unchanged
    let shipped = load_defaults().unwrap();
    assert_eq!(shipped.profile_bindings("default")["CROSS"], vec!["SPACE"]);
}

#[test]
fn capture_records_a_chord_from_its_peak() {
    let mut r = Rig::new();
    r.edit("TRIANGLE", 2);
    r.script(&[&[k(Key::LEFT_SHIFT)], &[k(Key::LEFT_SHIFT), k(Key::Q)], &[k(Key::Q)], &[]]);
    r.ticks(4);
    assert_eq!(r.bindings("default", "TRIANGLE"), vec!["F", "LEFT_SHIFT+Q"]);
}

#[test]
fn secondary_slot_keeps_the_primary() {
    let mut r = Rig::new();
    r.edit("L1", 2);
    r.script(&[&[k(Key::X)], &[]]);
    r.ticks(2);
    assert_eq!(r.bindings("default", "L1"), vec!["1", "X"]);
}

#[test]
fn capture_ignores_hotkeys_and_only_commits_real_keys() {
    let mut r = Rig::new();
    r.edit("CROSS", 1);
    r.script(&[&[k(Key::F8)], &[]]);
    r.ticks(2);
    assert!(r.e.capturing()); // only a hotkey was pressed: still waiting
    assert!(!r.path.exists());
}

#[test]
fn the_complete_hotkey_chord_is_still_kept_out_of_a_capture() {
    let mut r = Rig::new();
    r.edit("CROSS", 1);
    r.script(&[&[k(Key::LEFT_COMMAND), k(Key::F1)], &[]]); // a profile-slot hotkey
    r.ticks(2);
    assert!(r.e.capturing() && !r.path.exists());
}

#[test]
fn clear_removes_a_secondary_binding() {
    let mut r = Rig::new();
    r.ui(serde_json::json!({"type": "clear", "control": "L1", "slot": 2}));
    assert_eq!(r.bindings("default", "L1"), vec!["1"]);
}

#[test]
fn edits_apply_to_the_active_profile_only() {
    let mut r = Rig::new();
    r.e.next_profile(); // racing
    r.edit("R2", 1);
    r.script(&[&[k(Key::Z)], &[]]);
    r.ticks(2);
    assert_eq!(r.bindings("racing", "R2")[0], "Z");
    assert_eq!(r.bindings("default", "R2"), vec!["MOUSE_LEFT"]);
}

#[test]
fn cancel_timeout_and_mapping_on_all_end_capture() {
    let mut r = Rig::new();
    r.edit("CROSS", 1);
    r.ui(serde_json::json!({"type": "cancel"}));
    assert!(!r.e.capturing());
    r.edit("CROSS", 1);
    r.e.poll_capture(r.t + 999.0);
    assert!(!r.e.capturing() && r.notice().contains("Timed out"));
    r.edit("CROSS", 1);
    r.e.apply_enabled(true); // turning mapping on aborts any pending capture
    assert!(!r.e.capturing());
    assert!(!r.path.exists()); // nothing was written
}

#[test]
fn rejected_edit_leaves_file_and_bindings_untouched() {
    let mut r = Rig::new();
    r.edit("CROSS", 1);
    r.script(&[&[k(Key::TAB), Input::Mouse(MouseInput::MOUSE_LEFT)], &[]]);
    r.ticks(6); // includes the short wait that lets the overlay claim its own clicks
                // Tab + click is spoken for (it toggles RT/RB), so the saved file would not load.
    assert!(r.notice().starts_with("Rejected"), "{}", r.notice());
    assert!(!r.path.exists());
    let st = r.e.mapper().map(&InputSnapshot::new([k(Key::SPACE)], 0.0, 0.0), 0.0);
    assert!(st.buttons.contains(&Button::CROSS));
}

#[test]
fn malformed_ui_messages_are_ignored() {
    let mut r = Rig::new();
    for bad in [
        serde_json::json!({"type": "edit"}),
        serde_json::json!({"type": "edit", "control": "NOPE", "slot": 1}),
        serde_json::json!({"type": "edit", "control": "CROSS", "slot": "x"}),
        serde_json::json!({"type": "edit", "control": "CROSS", "slot": 7}),
        serde_json::json!({"type": "wat"}),
        serde_json::json!({}),
    ] {
        r.ui(bad);
    }
    assert!(!r.e.capturing());
}

#[test]
fn a_held_input_from_before_the_click_is_not_recorded() {
    let mut r = Rig::new();
    r.script(&[&[k(Key::A)]]); // A was already down when the cell was clicked
    r.edit("CROSS", 1);
    r.script(&[&[k(Key::A)], &[k(Key::A), k(Key::B)], &[]]);
    r.ticks(3);
    assert_eq!(r.bindings("default", "CROSS")[0], "B");
}

// ---- hold Escape to skip a capture ---------------------------------------------------------------------
#[test]
fn holding_escape_skips_the_capture_and_changes_nothing() {
    let mut r = Rig::new();
    r.edit("CROSS", 1);
    r.script(&[&[k(Key::ESCAPE)]]);
    r.e.poll_capture(100.0);
    assert!(r.e.capturing());
    r.e.poll_capture(100.3);
    assert!(r.e.capturing()); // not long enough yet
    r.e.poll_capture(100.7);
    assert!(!r.e.capturing());
    assert!(!r.path.exists());
    assert_eq!(r.excl(), (false, false)); // keyboard released
}

#[test]
fn a_quick_escape_tap_binds_escape_alone_or_in_a_chord() {
    let mut r = Rig::new();
    r.edit("CROSS", 1);
    r.script(&[&[k(Key::ESCAPE)], &[]]);
    r.ticks(2);
    assert_eq!(r.bindings("default", "CROSS")[0], "ESCAPE");
    r.edit("R1", 1);
    r.script(&[&[k(Key::LEFT_SHIFT), k(Key::ESCAPE)], &[]]);
    r.ticks(2);
    assert_eq!(r.bindings("default", "R1")[0], "LEFT_SHIFT+ESCAPE");
}

// ---- profile mouse picks ------------------------------------------------------------------------------------
#[test]
fn a_pick_button_binds_a_plain_mouse_button_and_held_modifiers_make_a_combo() {
    let mut r = Rig::new();
    r.edit("CROSS", 1);
    r.ui(serde_json::json!({"type": "pick", "input": "MOUSE_MIDDLE"}));
    assert_eq!(r.bindings("default", "CROSS")[0], "MOUSE_MIDDLE");
    r.edit("CIRCLE", 1);
    r.script(&[&[k(Key::LEFT_SHIFT), k(Key::LEFT_OPTION)]]);
    r.ui(serde_json::json!({"type": "pick", "input": "MOUSE_RIGHT"}));
    assert_eq!(r.bindings("default", "CIRCLE")[0], "LEFT_OPTION+LEFT_SHIFT+MOUSE_RIGHT");
}

#[test]
fn a_pick_with_command_held_or_no_capture_or_bad_name_does_nothing() {
    let mut r = Rig::new();
    r.ui(serde_json::json!({"type": "pick", "input": "MOUSE_LEFT"})); // no capture
    assert!(!r.path.exists());
    r.edit("CROSS", 1);
    r.ui(serde_json::json!({"type": "pick", "input": "MOUSE_BUTTON4"})); // not offered
    assert!(r.e.capturing() && !r.path.exists());
    r.script(&[&[k(Key::LEFT_COMMAND)]]); // Command is reserved so it is never added
    r.ui(serde_json::json!({"type": "pick", "input": "MOUSE_LEFT"}));
    assert_eq!(r.bindings("default", "CROSS")[0], "MOUSE_LEFT");
}

// ---- sensitivity slider ----------------------------------------------------------------------------------------
#[test]
fn dragging_the_slider_changes_sensitivity_live_without_writing() {
    let mut r = Rig::new();
    r.ui(serde_json::json!({"type": "slider", "value": 0.12, "final": false}));
    assert_eq!(r.e.cfg.mouse.sensitivity, 0.12);
    assert_eq!(r.e.mapper().config.mouse.sensitivity, 0.12);
    assert!(!r.path.exists()); // a drag is not saved on every pixel
}

#[test]
fn releasing_the_slider_saves_it_and_values_are_clamped() {
    let mut r = Rig::new();
    r.ui(serde_json::json!({"type": "slider", "value": 0.2, "final": true}));
    assert_eq!(r.saved().mouse.sensitivity, 0.2);
    assert_eq!(r.e.cfg.mouse.sensitivity, 0.2);
    assert!(r.notice().contains("Mouse sensitivity 0.200"));
    for (v, want) in [(5.0, 0.3), (-1.0, 0.01), (0.0, 0.01)] {
        r.ui(serde_json::json!({"type": "slider", "value": v, "final": true}));
        assert_eq!(r.saved().mouse.sensitivity, want);
    }
}

#[test]
fn garbage_slider_values_are_ignored() {
    let mut r = Rig::new();
    for bad in [
        serde_json::json!({"type": "slider", "value": "x", "final": true}),
        serde_json::json!({"type": "slider", "final": true}),
    ] {
        r.ui(bad);
    }
    r.e.handle_ui_event(UiEvent::Slider { value: f64::NAN, final_: true });
    assert!(r.e.cfg.mouse.sensitivity == 0.05 && !r.path.exists());
}

#[test]
fn slider_and_binding_edits_do_not_clobber_each_other() {
    let mut r = Rig::new();
    r.ui(serde_json::json!({"type": "slider", "value": 0.15, "final": true}));
    r.ui(serde_json::json!({"type": "clear", "control": "L1", "slot": 2}));
    assert_eq!(r.saved().mouse.sensitivity, 0.15);
    assert_eq!(r.bindings("default", "L1"), vec!["1"]);
}

// ---- profile management -------------------------------------------------------------------------------------------
#[test]
fn adding_a_profile_copies_the_current_one_and_starts_a_rename() {
    let mut r = Rig::new();
    r.ui(serde_json::json!({"type": "profile_add"}));
    assert_eq!(r.e.profile(), "profile1");
    assert!(r.e.renaming());
    assert_eq!(r.saved().profile_names(), vec!["default", "racing", "assassins_creed", "profile1"]);
}

#[test]
fn the_base_profile_cannot_be_deleted() {
    let mut r = Rig::new();
    r.ui(serde_json::json!({"type": "profile_delete", "name": "default"}));
    assert!(r.notice().contains("base profile cannot be deleted"), "{}", r.notice());
}

#[test]
fn deleting_a_profile_and_selecting_one() {
    let mut r = Rig::new();
    r.ui(serde_json::json!({"type": "profile_select", "name": "racing"}));
    assert_eq!(r.e.profile(), "racing");
    r.ui(serde_json::json!({"type": "profile_delete", "name": "racing"}));
    assert_eq!(r.e.profile(), "default");
    assert!(r.notice().contains("Profile racing deleted"));
}

#[test]
fn profile_actions_are_refused_while_mapping_is_on() {
    let mut r = Rig::new();
    r.e.apply_enabled(true);
    for kind in ["profile_add", "profile_delete", "profile_rename", "slider"] {
        r.ui(serde_json::json!({"type": kind, "name": "racing", "value": 0.1}));
    }
    assert_eq!(r.e.profile(), "default");
    assert!(!r.e.renaming() && r.notice().contains("Turn mapping OFF"));
}

// ---- renaming -------------------------------------------------------------------------------------------------------
fn rename(r: &mut Rig, name: &str) {
    r.ui(serde_json::json!({"type": "profile_rename", "name": name}));
}

#[test]
fn typing_replaces_the_old_name_and_enter_saves() {
    let mut r = Rig::new();
    rename(&mut r, "racing");
    assert!(r.e.renaming() && r.last().renaming.unwrap().text == "racing");
    r.feed(&plain(&[Key::R, Key::A, Key::L, Key::L, Key::Y]));
    assert_eq!(r.last().renaming.unwrap().text, "rally"); // not "racingrally"
    r.feed(&plain(&[Key::RETURN]));
    assert!(!r.e.renaming());
    assert_eq!(r.saved().profile_names(), vec!["default", "assassins_creed", "rally"]);
    assert_eq!(r.saved().profile_bindings("rally")["R2"][0], "UP_ARROW"); // bindings came along
}

#[test]
fn renaming_the_active_profile_keeps_it_selected_but_another_does_not_change_selection() {
    let mut r = Rig::new();
    r.e.select_slot(1);
    rename(&mut r, "racing");
    r.feed(&plain(&[Key::K, Key::A, Key::R, Key::T, Key::RETURN]));
    assert_eq!(r.e.profile(), "kart");
    assert_eq!(r.e.mapper().profile, "kart");
    let mut r = Rig::new();
    rename(&mut r, "racing");
    r.feed(&plain(&[Key::X, Key::RETURN]));
    assert_eq!(r.e.profile(), "default");
}

#[test]
fn the_base_profile_can_be_renamed() {
    let mut r = Rig::new();
    rename(&mut r, "default");
    r.feed(&plain(&[Key::F, Key::P, Key::S, Key::RETURN]));
    assert_eq!(r.e.profile(), "fps");
    assert_eq!(r.saved().default_name, "fps");
}

#[test]
fn editing_keys_backspace_shift_digits_dash() {
    let mut r = Rig::new();
    rename(&mut r, "racing");
    r.feed(&plain(&[Key::BACKSPACE])); // first key clears the selected old name
    assert_eq!(r.last().renaming.unwrap().text, "");
    r.feed(&[
        (Key::A, true),
        (Key::KEY_2, false),
        (Key::MINUS, false),
        (Key::MINUS, true),
        (Key::B, false),
        (Key::BACKSPACE, false),
    ]);
    assert_eq!(r.last().renaming.unwrap().text, "A2-_");
    r.feed(&[(Key::SPACE, false), (Key::COMMA, false), (Key::KEY_1, true)]); // not allowed in names: ignored
    assert_eq!(r.last().renaming.unwrap().text, "A2-_");
}

#[test]
fn name_length_is_limited() {
    let mut r = Rig::new();
    rename(&mut r, "racing");
    r.feed(&plain(&[Key::A; 50]));
    assert_eq!(r.last().renaming.unwrap().text.len(), 32);
}

#[test]
fn escape_cancels_a_rename_without_saving_and_enter_without_typing_keeps_the_name() {
    let mut r = Rig::new();
    rename(&mut r, "racing");
    r.feed(&plain(&[Key::X, Key::ESCAPE]));
    assert!(!r.e.renaming() && !r.path.exists());
    assert!(r.notice().contains("Rename cancelled"));
    rename(&mut r, "racing");
    r.feed(&plain(&[Key::RETURN]));
    assert!(!r.e.renaming() && r.e.cfg.profile_names() == vec!["default", "racing", "assassins_creed"]);
}

#[test]
fn clashing_or_empty_names_are_rejected_with_a_reason() {
    for keys in [vec![Key::D, Key::E, Key::F, Key::A, Key::U, Key::L, Key::T], vec![]] {
        let mut r = Rig::new();
        rename(&mut r, "racing");
        if keys.is_empty() {
            r.feed(&plain(&[Key::BACKSPACE]));
        } else {
            r.feed(&plain(&keys));
        }
        r.feed(&plain(&[Key::RETURN]));
        assert_eq!(r.e.cfg.profile_names(), vec!["default", "racing", "assassins_creed"]);
        assert!(r.notice().starts_with("Rejected"), "{}", r.notice());
    }
}

#[test]
fn typing_is_held_back_from_the_app_but_the_mouse_is_not() {
    let mut r = Rig::new();
    rename(&mut r, "racing");
    assert_eq!(r.excl(), (true, false));
    assert!(r.s.borrow().key_log.is_some()); // recording only while renaming
}

#[test]
fn a_rename_ends_and_frees_the_keyboard() {
    for how in ["hide", "focus", "profile", "mapping", "timeout", "capture"] {
        let mut r = Rig::new();
        rename(&mut r, "racing");
        match how {
            "hide" => r.e.toggle_overlay(),
            "focus" => {
                r.s.borrow_mut().focused = false;
                r.e.poll_window();
            }
            "profile" => r.e.select_slot(1),
            "mapping" => r.e.apply_enabled(true),
            "timeout" => r.e.tick(r.t + 999.0),
            _ => {
                r.edit("CROSS", 1);
                assert!(r.e.capturing());
            }
        }
        assert!(!r.e.renaming() && r.s.borrow().key_log.is_none(), "{how}");
        if how == "capture" {
            assert!(r.excl().0);
        }
        // now waiting for a binding instead
        else if how != "mapping" {
            assert!(!r.excl().0, "{how}");
        }
    }
}

// ---- reset -------------------------------------------------------------------------------------------------------------
#[test]
fn reset_restores_the_base_and_shipped_profiles_and_is_refused_while_live() {
    let mut r = Rig::new();
    r.ui(serde_json::json!({"type": "clear", "control": "CROSS", "slot": 1}));
    assert!(r.bindings("default", "CROSS").is_empty());
    r.ui(serde_json::json!({"type": "profile_reset"}));
    assert_eq!(r.bindings("default", "CROSS"), vec!["SPACE"]);
    assert!(r.notice().contains("reset to its defaults"));
    r.e.next_profile();
    r.ui(serde_json::json!({"type": "clear", "control": "R2", "slot": 1}));
    r.ui(serde_json::json!({"type": "profile_reset"}));
    assert_eq!(r.bindings("racing", "R2")[0], "UP_ARROW");
    r.e.apply_enabled(true);
    r.ui(serde_json::json!({"type": "clear", "control": "R2", "slot": 1}));
    assert!(r.notice().contains("Turn mapping OFF"));
}

// ---- overlay model -------------------------------------------------------------------------------------------------------
#[test]
fn overlay_state_carries_opacity_slots_rows_and_hotkeys() {
    let mut r = Rig::new();
    r.e.push_overlay();
    let s = r.last();
    assert_eq!(s.opacity, 0.5);
    assert_eq!(s.sensitivity.value, 0.05);
    assert_eq!(s.profile_tabs[0].slot, Some(1));
    assert_eq!(
        s.rows.iter().find(|x| x.control == "L1").map(|x| (x.primary.as_str(), x.secondary.as_str())),
        Some(("KEY_1", "SCROLL_UP"))
    );
    assert_eq!(s.rows.len(), 17 + 10 + 5);
    assert!(s.hotkeys.iter().any(|(k, t)| k == "Cmd+F1-F6" && t == "pick profile"));
    assert!(s.hotkeys.iter().any(|(_, t)| t == "PANIC: kill OpenPS"));
    assert_eq!(s.mouse[0], ("MOUSE_MOVE_UP".to_string(), "RIGHT_STICK_UP".to_string()));
}

#[test]
fn a_new_profile_can_start_as_a_copy_of_any_existing_one() {
    let mut r = Rig::new();
    r.ui(serde_json::json!({ "type": "profile_add", "source": "racing" }));
    assert!(r.e.renaming(), "naming starts right away");
    let racing = r.e.cfg.profile_bindings("racing");
    let created = r.e.cfg.profile_names().pop().unwrap();
    assert_eq!(r.e.cfg.profile_bindings(&created), racing, "a copy of racing, not of the profile that was selected");
}
