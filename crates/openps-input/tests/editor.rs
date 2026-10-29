//! Ported from Con2PC tests/test_customise.py and the Editor sections of test_profiles_reserved.py.
use openps_input::config::{load_defaults, load_path, Config};
use openps_input::editor::{dump_toml, Editor, PRIMARY, SECONDARY};
use openps_input::keys::{chord_to_str, Chord, Input, Key, MouseInput};
use openps_input::state::{Axis, Button, Control};
use tempfile::TempDir;

fn chord(items: &[Input]) -> Chord {
    items.iter().copied().collect()
}
fn k(key: Key) -> Input {
    Input::Key(key)
}
fn b(x: Button) -> Control {
    Control::Button(x)
}
fn a(x: Axis) -> Control {
    Control::Axis(x)
}
fn ed() -> (Editor, TempDir) {
    let d = TempDir::new().unwrap();
    (Editor::new(d.path().join("u.toml")).unwrap(), d)
}
fn reload(e: &Editor) -> Config {
    e.save().unwrap();
    load_path(&e.path, false).unwrap()
}

#[test]
fn chord_text_order() {
    assert_eq!(chord_to_str(&chord(&[k(Key::W), k(Key::LEFT_SHIFT)])), "LEFT_SHIFT+W");
    assert_eq!(chord_to_str(&chord(&[Input::Mouse(MouseInput::MOUSE_LEFT), k(Key::A)])), "A+MOUSE_LEFT");
}

#[test]
fn dump_roundtrip() {
    let text = "toggle = \"F9\"\n[mouse]\nsensitivity = 0.1\ninvert_y = true\n[bindings]\nCROSS = [\"SPACE\", \"A+B\"]\n[[combos]]\ninput = \"Q\"\noutput = [\"L1\"]\n";
    let data: toml::Table = text.parse().unwrap();
    let back: toml::Table = dump_toml(&data).parse().unwrap();
    assert_eq!(back, data);
}

#[test]
fn editor_save_and_reload() {
    let (mut e, _d) = ed();
    e.replace(b(Button::CROSS), &chord(&[k(Key::Z)]));
    e.add(a(Axis::L2), &chord(&[k(Key::LEFT_SHIFT), k(Key::X)]));
    e.clear(b(Button::TRIANGLE));
    e.set_toggle("LEFT_CONTROL+F7");
    let cfg = reload(&e);
    assert_eq!(cfg.bindings["CROSS"], vec!["Z"]);
    assert!(
        cfg.bindings["L2"].contains(&"LEFT_SHIFT+X".to_string())
            && cfg.bindings["L2"].contains(&"MOUSE_RIGHT".to_string())
    );
    assert!(cfg.bindings["TRIANGLE"].is_empty() && cfg.toggle == "LEFT_CONTROL+F7");
    e.reset(b(Button::CROSS));
    assert_eq!(e.bindings()["CROSS"], vec!["SPACE"]);
    assert_eq!(e.owners(&chord(&[k(Key::Q)]), None), vec!["SQUARE"]);
}

#[test]
fn primary_secondary_columns() {
    let (mut e, _d) = ed();
    assert_eq!((e.slot(b(Button::L1), PRIMARY), e.slot(b(Button::L1), SECONDARY)), ("1".into(), "SCROLL_UP".into()));
    e.set_slot(b(Button::CROSS), SECONDARY, &chord(&[k(Key::Z)]));
    assert_eq!((e.slot(b(Button::CROSS), 1), e.slot(b(Button::CROSS), 2)), ("SPACE".into(), "Z".into()));
    e.clear_slot(b(Button::CROSS), 1); // secondary moves up
    assert_eq!((e.slot(b(Button::CROSS), 1), e.slot(b(Button::CROSS), 2)), ("Z".into(), "-".into()));
    e.set_slot(a(Axis::L2), 1, &chord(&[k(Key::X)]));
    assert_eq!((e.slot(a(Axis::L2), 1), e.slot(a(Axis::L2), 2)), ("X".into(), "-".into()));
    // primary replaced
}

#[test]
fn mouse_move_rows_shown_and_follow_config() {
    let (mut e, _d) = ed();
    let rows: Vec<_> = e.mouse_move_rows();
    assert_eq!(rows[0], ("MOUSE_MOVE_UP".into(), "RIGHT_STICK_UP".into()));
    assert_eq!(rows[3], ("MOUSE_MOVE_RIGHT".into(), "RIGHT_STICK_RIGHT".into()));
    e.invert_y = true;
    assert_eq!(e.mouse_move_rows()[0].1, "RIGHT_STICK_DOWN");
}

#[test]
fn profiles_edit_and_save_only_diffs() {
    let (mut e, _d) = ed();
    assert_eq!(e.profile_names(), vec!["default", "racing"]);
    assert_eq!(e.next_profile(), "racing");
    assert_eq!(e.slot(a(Axis::R2), 1), "UP_ARROW");
    e.set_slot(a(Axis::R2), 1, &chord(&[k(Key::Z)])); // change racing only
    e.set_current("default");
    assert_eq!(e.slot(a(Axis::R2), 1), "MOUSE_LEFT");
    e.add_profile("drift", None).unwrap();
    e.set_current("drift");
    e.set_slot(b(Button::CROSS), 1, &chord(&[k(Key::X)]));
    let cfg = reload(&e);
    assert_eq!(cfg.profile_bindings("racing")["R2"][0], "Z");
    assert_eq!(cfg.profile_bindings("drift")["CROSS"], vec!["X"]);
    assert_eq!(cfg.profile_bindings("drift")["CIRCLE"], vec!["E"]); // inherits the base
    assert_eq!(cfg.profiles["drift"].bindings.keys().collect::<Vec<_>>(), vec!["CROSS"]); // diff only
    assert_eq!(cfg.bindings["CROSS"], vec!["SPACE"]);
}

// ---- create / delete / rename ----------------------------------------------------------------------
#[test]
fn add_clones_the_chosen_profile() {
    let (mut e, _d) = ed();
    e.add_profile("night", Some("racing")).unwrap();
    assert_eq!(e.profile("night").unwrap()["R2"][0], "UP_ARROW");
    assert_eq!(reload(&e).profile_bindings("night")["R2"][0], "UP_ARROW");
}

#[test]
fn add_defaults_to_a_copy_of_the_base() {
    let (mut e, _d) = ed();
    e.add_profile("plain", None).unwrap();
    assert_eq!(e.profile("plain").unwrap()["R2"], vec!["MOUSE_LEFT"]);
}

#[test]
fn add_rejects_bad_or_duplicate_names() {
    let (mut e, _d) = ed();
    for name in ["", "has space", "a.b", &"x".repeat(40), "racing", "RACING", "default", "Default"] {
        assert!(e.add_profile(name, None).is_err(), "{name:?}");
    }
}

#[test]
fn unique_name_never_collides() {
    let (mut e, _d) = ed();
    e.add_profile("profile1", None).unwrap();
    e.add_profile("profile2", None).unwrap();
    assert_eq!(e.unique_name("profile"), "profile3");
}

#[test]
fn delete_a_user_profile() {
    let (mut e, _d) = ed();
    e.add_profile("temp", None).unwrap();
    e.delete_profile("temp").unwrap();
    assert!(!reload(&e).profile_names().contains(&"temp".to_string()));
}

#[test]
fn delete_a_shipped_profile_stays_deleted_after_reload() {
    let (mut e, _d) = ed();
    e.delete_profile("racing").unwrap();
    let cfg = reload(&e);
    assert_eq!(cfg.profile_names(), vec!["default"]);
    assert_eq!(cfg.deleted_profiles, vec!["racing"]);
    assert_eq!(Editor::new(&e.path).unwrap().profile_names(), vec!["default"]); // and the editor agrees on re-open
}

#[test]
fn deleting_the_current_profile_falls_back_to_the_base() {
    let (mut e, _d) = ed();
    e.set_current("racing");
    e.delete_profile("racing").unwrap();
    assert_eq!(e.current(), "default");
}

#[test]
fn the_base_profile_cannot_be_deleted_only_renamed() {
    let (mut e, _d) = ed();
    assert!(e.delete_profile("default").unwrap_err().contains("base profile"));
    assert!(e.delete_profile("nope").is_err());
}

#[test]
fn rename_a_user_profile() {
    let (mut e, _d) = ed();
    e.add_profile("temp", None).unwrap();
    e.rename_profile("temp", "stealth").unwrap();
    let names = reload(&e).profile_names();
    assert!(names.contains(&"stealth".to_string()) && !names.contains(&"temp".to_string()));
}

#[test]
fn rename_a_shipped_profile_keeps_its_bindings_and_removes_the_old_name() {
    let (mut e, _d) = ed();
    e.rename_profile("racing", "rally").unwrap();
    let cfg = reload(&e);
    assert_eq!(cfg.profile_names(), vec!["default", "rally"]);
    assert_eq!(cfg.profile_bindings("rally")["R2"][0], "UP_ARROW");
    assert_eq!(cfg.profile_bindings("rally")["RIGHT_STICK_UP"], vec!["W"]);
    assert!(!Editor::new(&e.path).unwrap().profile_names().contains(&"racing".to_string()));
}

#[test]
fn rename_the_base_profile() {
    let (mut e, _d) = ed();
    e.rename_profile("default", "fps").unwrap();
    e.replace(b(Button::CROSS), &chord(&[k(Key::X)])); // edits the base (now called fps)
    let cfg = reload(&e);
    assert_eq!(cfg.default_name, "fps");
    assert_eq!(cfg.profile_bindings("fps")["CROSS"], vec!["X"]);
    assert_eq!(Editor::new(&e.path).unwrap().current(), "fps");
    assert_eq!(cfg.profile_bindings("racing")["CROSS"], vec!["X"]); // other profiles still inherit it
}

#[test]
fn renaming_the_base_back_to_default_cleans_the_file() {
    let (mut e, _d) = ed();
    e.rename_profile("default", "fps").unwrap();
    e.save().unwrap();
    e.rename_profile("fps", "default").unwrap();
    e.save().unwrap();
    assert!(!std::fs::read_to_string(&e.path).unwrap().contains("default_name"));
}

#[test]
fn rename_keeps_the_current_selection_and_order() {
    let (mut e, _d) = ed();
    e.add_profile("temp", None).unwrap();
    e.set_current("racing");
    e.rename_profile("racing", "rally").unwrap();
    assert_eq!(e.current(), "rally");
    assert_eq!(e.profile_names(), vec!["default", "rally", "temp"]);
}

#[test]
fn rename_validates_the_new_name() {
    let (mut e, _d) = ed();
    e.add_profile("temp", None).unwrap();
    for new in ["", "bad name", "default", "racing"] {
        assert!(e.rename_profile("temp", new).is_err(), "{new:?}");
    }
}

#[test]
fn rename_to_the_same_name_is_a_no_op() {
    let (mut e, _d) = ed();
    e.rename_profile("racing", "racing").unwrap();
    assert_eq!(e.profile_names(), vec!["default", "racing"]);
    assert!(e.deleted().is_empty());
}

#[test]
fn a_shipped_name_can_be_reused_after_it_was_renamed_away() {
    let (mut e, _d) = ed();
    e.rename_profile("racing", "rally").unwrap();
    e.add_profile("racing", Some("default")).unwrap(); // a brand new "racing", not the shipped one
    let cfg = reload(&e);
    assert_eq!(cfg.profile_bindings("racing")["R2"], vec!["MOUSE_LEFT"]);
    assert_eq!(cfg.profile_bindings("rally")["R2"][0], "UP_ARROW");
}

#[test]
fn a_complex_session_round_trips() {
    let (mut e, _d) = ed();
    e.rename_profile("default", "fps").unwrap();
    e.rename_profile("racing", "rally").unwrap();
    e.add_profile("night", Some("rally")).unwrap();
    e.profile_mut("night").unwrap().insert("CROSS".into(), vec!["X".into()]);
    e.add_profile("temp", None).unwrap();
    e.delete_profile("temp").unwrap();
    let cfg = reload(&e);
    assert_eq!(cfg.profile_names(), vec!["fps", "rally", "night"]);
    let again = Editor::new(&e.path).unwrap();
    assert_eq!(again.profile_names(), vec!["fps", "rally", "night"]);
    assert_eq!(again.profile("night").unwrap()["CROSS"], vec!["X"]);
    assert_eq!(again.profile("night").unwrap()["R2"][0], "UP_ARROW");
}

#[test]
fn set_option_persists_and_survives_other_edits() {
    let (mut e, _d) = ed();
    e.set_option("mouse", "sensitivity", toml::Value::Float(0.12));
    e.profile_mut("default").unwrap().insert("CROSS".into(), vec!["X".into()]);
    let cfg = reload(&e);
    assert!(cfg.mouse.sensitivity == 0.12 && cfg.mouse.smoothing == 0.6); // other fields untouched
    let mut e2 = Editor::new(&e.path).unwrap();
    e2.set_option("mouse", "sensitivity", toml::Value::Float(0.2));
    assert_eq!(reload(&e2).mouse.sensitivity, 0.2);
}

#[test]
fn a_bad_option_is_rejected_without_touching_the_file() {
    let (mut e, _d) = ed();
    e.save().unwrap();
    let good = std::fs::read_to_string(&e.path).unwrap();
    e.set_option("mouse", "sensitivity", toml::Value::Integer(-5));
    assert!(e.save().is_err());
    assert_eq!(std::fs::read_to_string(&e.path).unwrap(), good);
}

#[test]
fn editing_the_base_updates_inheriting_profiles_but_not_overridden_ones() {
    let (mut e, _d) = ed();
    e.replace(b(Button::CROSS), &chord(&[k(Key::X)])); // racing inherits CROSS, overrides R2
    assert_eq!(e.profile("racing").unwrap()["CROSS"], vec!["X"]);
    e.set_current("racing");
    e.replace(b(Button::SQUARE), &chord(&[k(Key::Y)])); // racing-only override
    e.set_current("default");
    e.replace(b(Button::SQUARE), &chord(&[k(Key::Z)])); // base changes SQUARE: racing keeps its own
    assert_eq!(e.profile("racing").unwrap()["SQUARE"], vec!["Y"]);
    let cfg = reload(&e);
    assert_eq!(cfg.profile_bindings("racing")["CROSS"], vec!["X"]);
    assert_eq!(cfg.profile_bindings("racing")["SQUARE"], vec!["Y"]);
    assert!(!cfg.profiles["racing"].bindings.contains_key("CROSS")); // still inherited, not frozen
}

#[test]
fn clearing_a_base_binding_reaches_inheriting_profiles() {
    let (mut e, _d) = ed();
    e.clear_slot(b(Button::L1), 2);
    assert_eq!(e.profile("racing").unwrap()["L1"], vec!["1"]);
}

#[test]
fn deleting_the_primary_promotes_the_secondary() {
    let (mut e, _d) = ed();
    e.clear_slot(b(Button::L1), 1);
    assert_eq!(e.bindings()["L1"], vec!["SCROLL_UP"]);
    assert_eq!((e.slot(b(Button::L1), 1), e.slot(b(Button::L1), 2)), ("SCROLL_UP".into(), "-".into()));
}

#[test]
fn deleting_the_primary_with_no_secondary_leaves_it_unbound() {
    let (mut e, _d) = ed();
    e.clear_slot(b(Button::CROSS), 1);
    assert!(e.bindings()["CROSS"].is_empty());
    assert_eq!(e.slot(b(Button::CROSS), 1), "-");
}

#[test]
fn deleting_the_secondary_keeps_the_primary_and_twice_empties() {
    let (mut e, _d) = ed();
    e.clear_slot(b(Button::R1), 2);
    assert_eq!(e.bindings()["R1"], vec!["2"]);
    e.clear_slot(b(Button::L1), 1);
    e.clear_slot(b(Button::L1), 1);
    assert!(e.bindings()["L1"].is_empty());
}

#[test]
fn reset_profile_restores_shipped_layouts() {
    let (mut e, _d) = ed();
    e.set_current("racing");
    e.replace(a(Axis::R2), &chord(&[k(Key::Z)]));
    e.reset_profile(None).unwrap();
    assert_eq!(e.profile("racing").unwrap()["R2"][0], "UP_ARROW");
    e.add_profile("mine", None).unwrap();
    e.profile_mut("mine").unwrap().insert("CROSS".into(), vec!["Q".into()]);
    e.reset_profile(Some("mine")).unwrap();
    assert_eq!(e.profile("mine").unwrap()["CROSS"], vec!["SPACE"]); // a user profile gets the shipped base
    e.set_current("default");
    e.replace(b(Button::CROSS), &chord(&[k(Key::X)]));
    e.reset_profile(Some("default")).unwrap();
    assert_eq!(e.bindings()["CROSS"], vec!["SPACE"]);
    assert!(e.reset_profile(Some("nope")).is_err());
}

#[test]
fn snapshot_and_restore_undo_a_bad_change() {
    let (mut e, _d) = ed();
    let good = e.snapshot();
    e.set_toggle("F11"); // overlaps profile_key
    assert!(e.validate().is_err());
    e.restore(good);
    assert!(e.validate().is_ok());
    assert_eq!(e.toggle(), "F10");
}

#[test]
fn combos_are_written_and_reloaded() {
    let d = TempDir::new().unwrap();
    let path = d.path().join("u.toml");
    std::fs::write(&path, "[[combos]]\ninput = \"LEFT_OPTION+E\"\noutput = [\"L1\", \"CIRCLE\"]\n").unwrap();
    let e = Editor::new(&path).unwrap();
    assert_eq!(e.combos().len(), 1);
    let cfg = reload(&e);
    assert_eq!(cfg.combos.len(), 1);
    assert!(load_defaults().unwrap().combos.is_empty());
}

#[test]
fn old_files_with_reserved_bindings_are_migrated_with_notes_and_not_written_back() {
    let d = TempDir::new().unwrap();
    let path = d.path().join("u.toml");
    std::fs::write(&path, "[bindings]\nCROSS = [\"F10\", \"Z\"]\n").unwrap();
    let e = Editor::new(&path).unwrap();
    assert_eq!(e.notes.len(), 1);
    e.save().unwrap();
    assert_eq!(load_path(&path, false).unwrap().bindings["CROSS"], vec!["Z"]); // strict load now passes
}
