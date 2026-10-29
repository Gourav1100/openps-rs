//! Ported from Con2PC tests/test_profiles_reserved.py (config-level sections).
use openps_input::keys::{Input, Key};
use openps_input::state::Button;
use openps_input::{load_defaults, load_layers, InputSnapshot, Mapper};

fn k(key: Key) -> Input {
    Input::Key(key)
}
fn load(text: &str) -> Result<openps_input::Config, String> {
    load_layers(Some(text), false)
}

#[test]
fn special_keys_cannot_be_bound() {
    for key in ["LEFT_COMMAND", "RIGHT_COMMAND", "F1", "F2", "F3", "F4", "F5", "F6", "F8", "F7", "F10", "F11", "F12"] {
        let e = load(&format!("[bindings]\nCROSS = [\"{key}\"]\n")).unwrap_err();
        assert!(e.contains(&format!("{key} is reserved by con2pc")), "{key}: {e}");
    }
}

#[test]
fn chords_containing_a_reserved_key_are_rejected_too() {
    assert!(load("[bindings]\nCROSS = [\"LEFT_SHIFT+LEFT_COMMAND\"]\n")
        .unwrap_err()
        .contains("LEFT_COMMAND is reserved"));
}

#[test]
fn combos_and_profiles_obey_the_same_rule() {
    assert!(load("[[combos]]\ninput = \"LEFT_COMMAND+E\"\noutput = [\"L1\"]\n")
        .unwrap_err()
        .contains("LEFT_COMMAND is reserved"));
    assert!(load("[profiles.x.bindings]\nL1 = [\"F1\"]\n").unwrap_err().contains("F1 is reserved"));
}

#[test]
fn the_error_says_why_and_which_binding() {
    let e = load("[bindings]\nCIRCLE = [\"LEFT_COMMAND\"]\n").unwrap_err();
    assert!(e.contains("Command selects profiles") && e.contains("CIRCLE") && e.contains("\"default\""), "{e}");
}

#[test]
fn a_custom_hotkey_reserves_its_keys_too() {
    let e = load("toggle = \"LEFT_CONTROL+F5\"\n[bindings]\nL3 = [\"LEFT_CONTROL\"]\n").unwrap_err();
    assert!(e.contains("LEFT_CONTROL is reserved"), "{e}");
}

#[test]
fn ordinary_keys_and_the_shipped_defaults_are_fine() {
    let cfg = load_defaults().unwrap();
    let reserved = cfg.reserved_inputs().unwrap();
    for name in cfg.profile_names() {
        for b in cfg.resolve(Some(&name)).unwrap() {
            assert!(!b.inputs.iter().any(|i| reserved.contains_key(i)), "{name}: {b:?}");
        }
    }
    let b = cfg.profile_bindings("default");
    assert!(b["CREATE"] == vec!["B"] && b["OPTIONS"] == vec!["RETURN"]);
}

#[test]
fn reserved_reasons_cover_every_special_key() {
    let r = load_defaults().unwrap().reserved_inputs().unwrap();
    assert!(r.contains_key(&k(Key::LEFT_COMMAND)) && r.contains_key(&k(Key::RIGHT_COMMAND)));
    for key in [Key::F1, Key::F6, Key::F8, Key::F7, Key::F10, Key::F11, Key::F12] {
        assert!(r.contains_key(&k(key)));
    }
    assert!(!r.contains_key(&k(Key::TAB)) && !r.contains_key(&k(Key::ESCAPE)));
}

#[test]
fn six_slots_with_left_and_right_command() {
    let cfg = load_defaults().unwrap();
    let slots = cfg.profile_slot_chords().unwrap();
    assert_eq!(slots.len(), 12);
    assert!(slots.contains(&(0, "profile_slot_1".into(), [k(Key::LEFT_COMMAND), k(Key::F1)].into_iter().collect())));
    assert!(slots.contains(&(
        5,
        "profile_slot_6_right".into(),
        [k(Key::RIGHT_COMMAND), k(Key::F6)].into_iter().collect()
    )));
    let names: Vec<String> = cfg.hotkey_chords().unwrap().into_iter().map(|(n, _)| n).collect();
    assert!(names.contains(&"profile_slot_1".to_string()) && names.contains(&"profile_slot_1_right".to_string()));
}

#[test]
fn slots_are_configurable_and_validated() {
    let cfg = load("profile_slots = [\"LEFT_COMMAND+F13\", \"LEFT_COMMAND+F14\"]\n").unwrap();
    let idx: Vec<usize> = cfg.profile_slot_chords().unwrap().iter().map(|s| s.0).collect();
    assert_eq!(idx, vec![0, 0, 1, 1]);
    assert!(load("profile_slots = [\"NOPE+F1\"]\n").is_err());
    assert!(load("toggle = \"LEFT_COMMAND+F1\"\n").unwrap_err().contains("overlap"));
    // collides with slot 1
}

#[test]
fn slot_keys_cannot_be_bound() {
    assert!(load("[bindings]\nSQUARE = [\"F3\"]\n").unwrap_err().contains("F3 is reserved"));
}

#[test]
fn base_profile_can_have_a_custom_name() {
    let cfg = load("default_name = \"fps\"\n").unwrap();
    assert_eq!(&cfg.profile_names()[..2], ["fps", "racing"]);
    let mut m = Mapper::new(cfg.clone(), None).unwrap();
    assert_eq!(m.profile, "fps");
    assert_eq!(cfg.profile_bindings("fps")["CROSS"], vec!["SPACE"]);
    assert!(m.map(&InputSnapshot::new([k(Key::SPACE)], 0.0, 0.0), 0.0).buttons.contains(&Button::CROSS));
}

#[test]
fn bad_base_names_are_rejected() {
    for name in ["", "   "] {
        assert!(load(&format!("default_name = \"{name}\"\n")).is_err(), "{name:?}");
    }
}

#[test]
fn names_in_a_file_are_read_as_lowercase_snake_case() {
    let c = load("default_name = \"My Base\"\n[profiles.\"Night Drive-2\".bindings]\nCROSS = [\"Z\"]\n[profiles.Night_Drive_2.bindings]\nCROSS = [\"X\"]\n").unwrap();
    assert_eq!(c.default_name, "my_base");
    let names = c.profile_names();
    assert!(names.contains(&"night_drive_2".to_string()), "{names:?}");
    assert!(
        names.contains(&"night_drive_2_2".to_string()),
        "a second name that converts to the same one is numbered: {names:?}"
    );
}

#[test]
fn every_profile_has_a_permanent_id_and_the_shipped_ones_keep_theirs() {
    let c = load("").unwrap();
    assert_eq!(c.id_of("racing").as_deref(), Some("p_3a1f0c01"));
    assert_eq!(c.id_of("assassins_creed").as_deref(), Some("p_3a1f0c02"));
    assert_eq!(c.id_of("default"), None, "the base profile has no id: a game without a choice uses it");
    assert_eq!(c.name_of_id("p_3a1f0c02").as_deref(), Some("assassins_creed"));
    let u = load("[profiles.mine.bindings]\nCROSS = [\"Z\"]\n").unwrap();
    let id = u.id_of("mine").unwrap();
    assert!(openps_input::config::valid_profile_id(&id), "{id}");
    assert_eq!(
        load("[profiles.mine.bindings]\nCROSS = [\"Z\"]\n").unwrap().id_of("mine").unwrap(),
        id,
        "stable between loads"
    );
    assert!(
        load("[profile_ids]\nmine = \"p_3a1f0c01\"\n[profiles.mine.bindings]\nCROSS = [\"Z\"]\n").is_err(),
        "two profiles with one id"
    );
}

#[test]
fn base_name_cannot_clash_with_another_profile() {
    assert!(load("default_name = \"racing\"\n").unwrap_err().contains("base"));
}

#[test]
fn deleting_and_recreating_a_shipped_profile() {
    assert_eq!(load("deleted_profiles = [\"racing\"]\n").unwrap().profile_names(), vec!["default", "assassins_creed"]);
    let cfg = load("deleted_profiles = [\"racing\"]\n[profiles.racing.bindings]\nCROSS = [\"Z\"]\n").unwrap();
    assert_eq!(cfg.profile_bindings("racing")["R2"], vec!["MOUSE_LEFT"]); // shipped override is gone
    assert_eq!(cfg.profile_bindings("racing")["CROSS"], vec!["Z"]);
    assert!(load("deleted_profiles = \"racing\"\n").unwrap_err().contains("deleted_profiles"));
}

#[test]
fn tab_and_escape_can_be_bound_alone_or_in_chords_and_drive_the_pad() {
    for key in ["TAB", "ESCAPE"] {
        let cfg = load(&format!("[bindings]\nCROSS = [\"{key}\"]\nCIRCLE = [\"LEFT_SHIFT+{key}\"]\n")).unwrap();
        let b = cfg.profile_bindings("default");
        assert!(b["CROSS"] == vec![key.to_string()] && b["CIRCLE"] == vec![format!("LEFT_SHIFT+{key}")]);
    }
    let mut cfg = load("[bindings]\nCROSS = [\"TAB\"]\nCIRCLE = [\"ESCAPE\"]\n").unwrap();
    cfg.output.min_press_ms = 0.0;
    let st = Mapper::new(cfg, None).unwrap().map(&InputSnapshot::new([k(Key::TAB), k(Key::ESCAPE)], 0.0, 0.0), 0.0);
    assert!(st.buttons.contains(&Button::CROSS) && st.buttons.contains(&Button::CIRCLE));
}

#[test]
fn tab_plus_click_cannot_be_a_binding_but_middle_click_can() {
    for click in ["MOUSE_LEFT", "MOUSE_RIGHT"] {
        assert!(load(&format!("[bindings]\nCROSS = [\"TAB+{click}\"]\n"))
            .unwrap_err()
            .contains("toggles RT/RB and LT/LB"));
        assert!(load(&format!("[bindings]\nCROSS = [\"LEFT_SHIFT+TAB+{click}\"]\n"))
            .unwrap_err()
            .contains("toggles RT/RB and LT/LB"));
    }
    let cfg = load("[bindings]\nCROSS = [\"TAB+MOUSE_MIDDLE\"]\nCIRCLE = [\"TAB+E\"]\n").unwrap();
    assert_eq!(cfg.profile_bindings("default")["CROSS"], vec!["TAB+MOUSE_MIDDLE"]);
}

#[test]
fn tab_click_still_toggles_even_when_tab_itself_is_bound() {
    let mut cfg = load("[bindings]\nCROSS = [\"TAB\"]\n").unwrap();
    cfg.output.min_press_ms = 0.0;
    let mut m = Mapper::new(cfg, None).unwrap();
    assert!(m.map(&InputSnapshot::new([k(Key::TAB)], 0.0, 0.0), 0.0).buttons.contains(&Button::CROSS));
    m.map(&InputSnapshot::new([k(Key::TAB), Input::Mouse(openps_input::keys::MouseInput::MOUSE_LEFT)], 0.0, 0.0), 0.01);
    assert_eq!(m.click_modes().0, "RB");
}

#[test]
fn an_old_config_that_binds_tab_or_escape_loads_unchanged() {
    let cfg = load_layers(Some("[bindings]\nCREATE = [\"TAB\"]\nOPTIONS = [\"ESCAPE\", \"RETURN\"]\n"), true).unwrap();
    assert!(cfg.notes.is_empty());
    assert_eq!(cfg.profile_bindings("default")["OPTIONS"], vec!["ESCAPE", "RETURN"]);
}

/// The defaults the developer specified, written out once so that nobody changes them by accident.
#[test]
fn shipped_defaults_match_the_specified_table() {
    let c = load_defaults().unwrap();
    // startup and hotkeys
    assert!(!c.start_enabled, "mapping starts OFF");
    assert_eq!(
        (c.overlay_key.as_str(), c.panic_key.as_str(), c.toggle.as_str(), c.profile_key.as_str(), c.quit.as_str()),
        ("F8", "F11", "F10", "F7", "F12")
    );
    assert_eq!(c.profile_slots, (1..=6).map(|n| format!("LEFT_COMMAND+F{n}")).collect::<Vec<_>>());
    // cursor / input suppression / overlay
    assert!(c.mouse.capture_cursor && c.mouse.hide_cursor && c.suppress_input);
    assert!((c.overlay_opacity - 0.5).abs() < 1e-9);
    assert_eq!(c.mouse.look, "RIGHT_STICK");
    // numbers
    assert!((c.mouse.sensitivity - 0.05).abs() < 1e-9 && (c.mouse.smoothing - 0.6).abs() < 1e-9);
    assert_eq!(c.poll_hz, 1000);
    assert!((c.output.min_press_ms - 35.0).abs() < 1e-9 && (c.adaptive_triggers.sweep_ms - 1200.0).abs() < 1e-9);
    assert!((c.keyboard_stick.right_ramp_ms - 100.0).abs() < 1e-9 && (c.mouse.recenter_ms - 40.0).abs() < 1e-9);
    assert_eq!(c.default_name, "default");
    assert!(c.feedback_sounds && c.adaptive_triggers.enabled);
    // bindings: movement, buttons, shoulders, menu, touchpad
    let b = c.profile_bindings("default");
    let get = |n: &str| b.get(n).cloned().unwrap_or_default();
    for (control, keys) in [
        ("LEFT_STICK_UP", vec!["W"]),
        ("LEFT_STICK_DOWN", vec!["S"]),
        ("LEFT_STICK_LEFT", vec!["A"]),
        ("LEFT_STICK_RIGHT", vec!["D"]),
        ("RIGHT_STICK_UP", vec!["I", "UP_ARROW"]),
        ("RIGHT_STICK_DOWN", vec!["K", "DOWN_ARROW"]),
        ("RIGHT_STICK_LEFT", vec!["J", "LEFT_ARROW"]),
        ("RIGHT_STICK_RIGHT", vec!["L", "RIGHT_ARROW"]),
        ("CROSS", vec!["SPACE"]),
        ("CIRCLE", vec!["E"]),
        ("SQUARE", vec!["Q"]),
        ("TRIANGLE", vec!["F"]),
        ("L1", vec!["1", "SCROLL_UP"]),
        ("R1", vec!["2", "SCROLL_DOWN"]),
        ("L3", vec!["LEFT_SHIFT"]),
        ("R3", vec!["C", "MOUSE_MIDDLE"]),
        ("R2", vec!["MOUSE_LEFT"]),
        ("L2", vec!["MOUSE_RIGHT"]),
        ("CREATE", vec!["B"]),
        ("OPTIONS", vec!["RETURN"]),
        ("PS", vec!["GRAVE"]),
        ("TOUCHPAD", vec!["M"]),
        ("MUTE", vec!["T"]),
        ("TOUCHPAD_SWIPE_LEFT", vec!["KEY_3"]),
        ("TOUCHPAD_SWIPE_RIGHT", vec!["KEY_4"]),
        ("TOUCHPAD_SWIPE_UP", vec!["KEY_5"]),
        ("TOUCHPAD_SWIPE_DOWN", vec!["KEY_6"]),
        ("TOUCHPAD_TAP", vec!["KEY_7"]),
        ("DPAD_UP", vec!["LEFT_OPTION+UP_ARROW"]),
        ("DPAD_DOWN", vec!["LEFT_OPTION+DOWN_ARROW"]),
        ("DPAD_LEFT", vec!["LEFT_OPTION+LEFT_ARROW"]),
        ("DPAD_RIGHT", vec!["LEFT_OPTION+RIGHT_ARROW"]),
    ] {
        assert_eq!(get(control), keys, "{control}");
    }
    // racing profile
    let r = c.profile_bindings("racing");
    let get = |n: &str| r.get(n).cloned().unwrap_or_default();
    assert_eq!(get("R2")[0], "UP_ARROW");
    assert_eq!(get("L2")[0], "DOWN_ARROW");
    assert_eq!(
        (get("LEFT_STICK_LEFT"), get("LEFT_STICK_RIGHT")),
        (vec!["LEFT_ARROW".to_string()], vec!["RIGHT_ARROW".to_string()])
    );
    assert_eq!(
        (get("RIGHT_STICK_UP"), get("RIGHT_STICK_DOWN"), get("RIGHT_STICK_LEFT"), get("RIGHT_STICK_RIGHT")),
        (
            vec!["LEFT_OPTION+UP_ARROW".to_string()],
            vec!["LEFT_OPTION+DOWN_ARROW".to_string(), "Z".to_string()],
            vec!["LEFT_OPTION+LEFT_ARROW".to_string()],
            vec!["LEFT_OPTION+RIGHT_ARROW".to_string()]
        )
    );
    // W/A/S/D is the D-pad in racing, and the toggle hotkey F10 is bound to nothing in any shipped profile
    assert_eq!(get("DPAD_UP"), vec!["W".to_string()]);
    for profile in c.profile_names() {
        let b = c.profile_bindings(&profile);
        assert!(!b.values().flatten().any(|chord| chord.contains("F10")), "F10 is the mapping toggle: {profile}");
    }
}

/// The profile imported from Con2PC (`assassins_creed`) loads, is the third profile (Command+F3) and binds what the original did.
#[test]
fn the_assassins_creed_profile_from_con2pc_is_shipped_and_valid() {
    let c = load_defaults().unwrap();
    assert_eq!(c.profile_names(), vec!["default", "racing", "assassins_creed"]);
    let b = c.profile_bindings("assassins_creed");
    let get = |n: &str| b.get(n).cloned().unwrap_or_default();
    for (control, keys) in [
        ("CROSS", vec!["SPACE", "EQUAL"]),
        ("CIRCLE", vec!["LEFT_OPTION", "MINUS"]),
        ("SQUARE", vec!["C"]),
        ("TRIANGLE", vec!["E"]),
        ("L1", vec!["Q"]),
        ("R1", vec!["MOUSE_LEFT"]),
        ("R3", vec!["MOUSE_MIDDLE"]),
        ("CREATE", vec!["TAB"]),
        ("OPTIONS", vec!["ESCAPE"]),
        ("PS", vec!["RIGHT_OPTION"]),
        ("DPAD_UP", vec!["H"]),
        ("DPAD_DOWN", vec!["V"]),
        ("DPAD_LEFT", vec!["X"]),
        ("DPAD_RIGHT", vec!["B"]),
        ("R2", vec!["LEFT_SHIFT+MOUSE_LEFT", "Z"]),
    ] {
        assert_eq!(get(control), keys, "{control}");
    }
    // everything it does not mention keeps the default key
    assert_eq!(get("LEFT_STICK_UP"), vec!["W"]);
    assert_eq!(get("L2"), vec!["MOUSE_RIGHT"]);
    // it really drives the pad: Space presses Cross
    let mut m = Mapper::new(c, Some("assassins_creed")).unwrap();
    assert!(m.map(&InputSnapshot::new([k(Key::SPACE)], 0.0, 0.0), 0.0).buttons.contains(&Button::CROSS));
    assert!(m.map(&InputSnapshot::new([k(Key::H)], 0.0, 0.0), 0.1).buttons.contains(&Button::DPAD_UP));
}

#[test]
fn an_old_user_file_with_f9_and_f11_is_read_as_f11_and_f7() {
    let old = "toggle = \"F10\"\nprofile_key = \"F11\"\n[bindings]\nCROSS = [\"SPACE\"]\n";
    let c = openps_input::load_layers(Some(old), true).unwrap();
    assert_eq!((c.panic_key.as_str(), c.profile_key.as_str()), ("F11", "F7"));
    let older = "panic_key = \"F9\"\n";
    assert_eq!(openps_input::load_layers(Some(older), true).unwrap().panic_key, "F11");
    // an explicit file is never rewritten
    assert!(
        openps_input::load_layers(Some(old), false).is_err()
            || openps_input::load_layers(Some(old), false).unwrap().panic_key == "F11"
    );
}
