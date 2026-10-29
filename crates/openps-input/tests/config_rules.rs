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
    for key in ["LEFT_COMMAND", "RIGHT_COMMAND", "F1", "F2", "F3", "F4", "F5", "F6", "F8", "F9", "F10", "F11", "F12"] {
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
    let e = load("toggle = \"LEFT_CONTROL+F7\"\n[bindings]\nL3 = [\"LEFT_CONTROL\"]\n").unwrap_err();
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
    for key in [Key::F1, Key::F6, Key::F8, Key::F9, Key::F10, Key::F11, Key::F12] {
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
    for name in ["", "a b", &"x".repeat(33), "a.b"] {
        assert!(load(&format!("default_name = \"{name}\"\n")).is_err(), "{name:?}");
    }
}

#[test]
fn base_name_cannot_clash_with_another_profile() {
    assert!(load("default_name = \"racing\"\n").unwrap_err().contains("base"));
}

#[test]
fn deleting_and_recreating_a_shipped_profile() {
    assert_eq!(load("deleted_profiles = [\"racing\"]\n").unwrap().profile_names(), vec!["default"]);
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
