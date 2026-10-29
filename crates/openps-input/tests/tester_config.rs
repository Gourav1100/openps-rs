//! Tester A4: config parsing / validation / migration corner cases, key-name round trips.
//! Tests marked `#[ignore = "BUG T07-xx ..."]` demonstrate a confirmed defect that is not fixed.
use openps_input::config::{load_layers, migrate_hotkeys, valid_profile_name};
use openps_input::keys::{chord_to_str, parse_chord, parse_input, Input, Key, MouseInput};
use openps_input::load_defaults;
use std::collections::HashSet;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------- key names

#[test]
fn every_key_and_mouse_name_round_trips_in_any_case() {
    let mut names = HashSet::new();
    let mut vks = HashSet::new();
    for k in Key::ALL {
        assert!(names.insert(k.name()), "duplicate key name {}", k.name());
        assert!(vks.insert(k.vk()), "duplicate vk {:#x}", k.vk());
        assert_eq!(Key::from_vk(k.vk()), Some(*k));
        assert_eq!(parse_input(k.name()), Ok(Input::Key(*k)), "{}", k.name());
        assert_eq!(parse_input(&k.name().to_lowercase()), Ok(Input::Key(*k)));
        assert_eq!(parse_input(&format!("  {}\t", k.name())), Ok(Input::Key(*k)));
    }
    for m in MouseInput::ALL {
        assert!(names.insert(m.name()), "mouse name {} collides with a key", m.name());
        assert_eq!(parse_input(m.name()), Ok(Input::Mouse(*m)));
    }
    assert_eq!(Key::from_vk(0x34), None); // hole in the kVK table
    assert_eq!(Key::from_vk(0xFFFF), None);
}

#[test]
fn chord_text_form_round_trips_for_every_pair_of_keys() {
    for a in Key::ALL {
        for b in Key::ALL {
            let c = parse_chord(&format!("{}+{}", a.name(), b.name())).unwrap();
            assert_eq!(parse_chord(&chord_to_str(&c)).unwrap(), c, "{a:?}+{b:?}");
        }
    }
    // modifiers always come first, so the text is stable regardless of input order
    assert_eq!(chord_to_str(&parse_chord("w+left_shift").unwrap()), "LEFT_SHIFT+W");
}

#[test]
fn malformed_chords_are_errors_not_panics() {
    for bad in [
        "",
        " ",
        "+",
        "W+",
        "+W",
        "W++S",
        "W + + S",
        "W\0",
        "\u{0}",
        "W+\u{202e}S",
        "ＷＡ",
        "KEY_",
        "KEY_10",
        "10",
        "W,S",
        "W S",
        &"W+".repeat(10_000),
    ] {
        let r = parse_chord(bad);
        assert!(r.is_err(), "{bad:?} must be rejected, got {r:?}");
    }
    // many repeated valid keys collapse into a set instead of growing
    assert_eq!(parse_chord("W+".repeat(10_000).trim_end_matches('+')).unwrap().len(), 1);
    // bare digits mean KEY_<digit>; two-digit numbers are not keys
    assert_eq!(parse_input("7"), Ok(Input::Key(Key::KEY_7)));
}

#[test]
fn unicode_case_folding_is_accepted_by_parse_input_info() {
    // Info (T07-21): to_uppercase() folds U+017F (long s) to 'S' and U+0131 (dotless i) to 'I'.
    assert_eq!(parse_input("\u{17f}"), Ok(Input::Key(Key::S)));
    assert_eq!(parse_input("\u{131}"), Ok(Input::Key(Key::I)));
}

#[test]
fn profile_name_rules() {
    for ok in ["a", "a_b9", &"x".repeat(32)] {
        assert!(valid_profile_name(ok), "{ok}");
    }
    for bad in [
        "",
        &"x".repeat(33),
        "A",
        "a-b",
        "_a",
        "a_",
        "a__b",
        "a b",
        "é",
        "日本",
        "a/b",
        "..",
        "a\0",
        "a\n",
        "\u{202e}x",
        "a.b",
        "a+b",
        "[x]",
    ] {
        assert!(!valid_profile_name(bad), "{bad:?}");
    }
}

// ---------------------------------------------------------------- malformed / hostile config text

#[test]
fn garbage_config_text_never_panics_and_errors_carry_context() {
    let cases: Vec<String> = vec![
        "[".into(),
        "[[".into(),
        "= 5".into(),
        "poll_hz".into(),
        "poll_hz = ".into(),
        "poll_hz = \"fast\"".into(),
        "toggle = 5".into(),
        "toggle = \"F10".into(),
        "toggle = \"F10\"\0".into(),
        "\0\0\0".into(),
        "[bindings]\nCROSS = 5".into(),
        "[bindings]\nCROSS = [1,2]".into(),
        "[bindings]\nCROSS = [[\"W\"]]".into(),
        "[bindings]\nCROSSS = \"W\"".into(),
        "bindings = 5".into(),
        "profiles = 5".into(),
        "[profiles]\nx = 5".into(),
        "[profiles.x]\nunknown = 1".into(),
        "combos = 5".into(),
        "[[combos]]\ninput = \"W\"".into(),
        "deleted_profiles = \"racing\"".into(),
        "profile_slots = [\"\"]".into(),
        "mouse = 3".into(),
        "[mouse]\nlook = \"UP\"".into(),
        "a = ".to_string() + &"[".repeat(100), // deeper nesting: see tester_config_deep.rs
        "x = ".to_string() + &"\"a\"".repeat(1000),
    ];
    for c in &cases {
        for migrate in [false, true] {
            let r = load_layers(Some(c), migrate);
            assert!(r.is_err(), "should be rejected (migrate={migrate}): {:?}", &c[..c.len().min(60)]);
            assert!(!r.unwrap_err().is_empty());
        }
    }
}

#[test]
fn combo_with_empty_output_is_accepted_info() {
    // Info: an empty output list is valid (acts as a mask that swallows shorter chords).
    assert!(load_layers(Some("[[combos]]\ninput = \"G+H\"\noutput = []"), false).is_ok());
}

#[test]
fn utf8_bom_is_tolerated() {
    assert!(load_layers(Some("\u{feff}toggle = \"F10\"\n"), true).is_ok());
}

#[test]
fn empty_user_text_equals_defaults() {
    let d = load_defaults().unwrap();
    let u = load_layers(Some(""), true).unwrap();
    assert_eq!(d.profile_names(), u.profile_names());
    assert_eq!(d.hotkey_chords().unwrap(), u.hotkey_chords().unwrap());
    assert!(u.notes.is_empty());
}

#[test]
fn huge_config_files_load_or_fail_quickly() {
    // 8 MB of comments
    let big = "# ".to_string() + &"x".repeat(8 * 1024 * 1024) + "\n";
    let t = Instant::now();
    assert!(load_layers(Some(&big), true).is_ok());
    assert!(t.elapsed() < Duration::from_secs(10), "8 MB of comments took {:?}", t.elapsed());

    // 2000 profiles, each with 20 bindings: validation is quadratic in places (hotkey pairs, reserved checks)
    let mut s = String::new();
    for p in 0..2000 {
        s += &format!("[profiles.p{p}.bindings]\n");
        for c in ["CROSS", "CIRCLE", "SQUARE", "TRIANGLE", "L1", "R1", "L3", "R3", "OPTIONS", "DPAD_UP"] {
            s += &format!("{c} = [\"W\", \"S\"]\n");
        }
    }
    let t = Instant::now();
    let r = load_layers(Some(&s), true);
    assert!(r.is_ok(), "{:?}", r.err());
    assert!(t.elapsed() < Duration::from_secs(20), "2000 profiles took {:?}", t.elapsed());
    // The engine/overlay cap is 12 profiles, but the loader itself does not enforce it
    assert!(r.unwrap().profile_names().len() > 12);

    // 100k combos
    let mut s = String::new();
    for _ in 0..100_000 {
        s += "[[combos]]\ninput = \"G\"\noutput = \"CROSS\"\n";
    }
    let t = Instant::now();
    assert!(load_layers(Some(&s), false).is_ok());
    assert!(t.elapsed() < Duration::from_secs(20), "100k combos took {:?}", t.elapsed());
}

#[test]
fn numeric_bounds_are_enforced_incl_nan_and_inf() {
    let bad = [
        "poll_hz = 29",
        "poll_hz = 1001",
        "poll_hz = 0",
        "poll_hz = -1",
        "poll_hz = 4294967296",
        "overlay_opacity = 0.09",
        "overlay_opacity = 1.01",
        "overlay_opacity = nan",
        "[mouse]\nsensitivity = 0.0",
        "[mouse]\nsensitivity = -0.1",
        "[mouse]\nsensitivity = nan",
        "[mouse]\nsensitivity = inf",
        "[mouse]\nsensitivity = -inf",
        "[mouse]\nsmoothing = 1.0",
        "[mouse]\nsmoothing = nan",
        "[mouse]\ndeadzone = 1.0",
        "[mouse]\ndeadzone = -0.5",
        "[mouse]\nmove_threshold = -1.0",
        "[mouse]\nrecenter_ms = -1.0",
        "[mouse]\nrecenter_ms = nan",
        "[output]\nmin_press_ms = 200.5",
        "[output]\nmin_press_ms = -1.0",
        "[adaptive_triggers]\nsweep_ms = 0.0",
        "[touchpad]\nswipe_ms = 0.0",
        "[touchpad]\ntap_ms = 0.0",
        "[touchpad]\nswipe_length = 1.5",
        "[touchpad]\nswipe_length = 0.0",
        "[keyboard_stick]\nleft_ramp_ms = -1.0",
        "[keyboard_stick]\nrelease_ms = nan",
    ];
    for b in bad {
        assert!(load_layers(Some(b), false).is_err(), "{b:?} accepted");
        assert!(load_layers(Some(b), true).is_err(), "{b:?} accepted (migrate)");
    }
    let good = [
        "poll_hz = 30",
        "poll_hz = 1000",
        "overlay_opacity = 0.1",
        "overlay_opacity = 1.0",
        "[mouse]\nsensitivity = 5e-324",
        "[mouse]\nsensitivity = 1e300",
        "[mouse]\nsmoothing = 0.999999",
        "[mouse]\ndeadzone = 0.0",
        "[output]\nmin_press_ms = 0.0",
        "[output]\nmin_press_ms = 200.0",
        "[keyboard_stick]\nleft_ramp_ms = 0.0",
    ];
    for g in good {
        assert!(load_layers(Some(g), false).is_ok(), "{g:?} rejected: {:?}", load_layers(Some(g), false).err());
    }
}

#[test]
fn integer_literals_for_float_options_are_accepted() {
    // A user writes `sensitivity = 1` (no decimal point): must not be a type error.
    let r = load_layers(Some("[mouse]\nsensitivity = 1\nrecenter_ms = 40"), false);
    assert!(r.is_ok(), "{:?}", r.err());
}

#[test]
fn unknown_keys_are_rejected_with_the_key_name() {
    let e = load_layers(Some("poll_rate = 100"), false).unwrap_err();
    assert!(e.contains("poll_rate"), "{e}");
    let e = load_layers(Some("[mouse]\nsens = 1.0"), false).unwrap_err();
    assert!(e.contains("sens"), "{e}");
}

#[test]
fn duplicate_control_aliases_resolve_deterministically_info() {
    // Info (T07-18): SHARE is an alias of CREATE and names are case-insensitive. Both spellings in one table
    // are accepted without complaint and one silently wins (BTreeMap order: upper-case, then "SHARE", then lower-case).
    let c = load_layers(Some("[bindings]\nCREATE = [\"G\"]\nSHARE = [\"H\"]\n"), false).unwrap();
    let b = c.profile_bindings("default");
    assert_eq!(b["CREATE"], vec!["H".to_string()]); // SHARE overrode CREATE
}

// ---------------------------------------------------------------- hotkeys / reserved inputs

fn err(text: &str) -> String {
    load_layers(Some(text), false).unwrap_err()
}

#[test]
fn conflicting_hotkeys_are_rejected() {
    assert!(err("toggle = \"F9\"\nquit = \"F9\"").contains("overlap"));
    assert!(err("toggle = \"F12\"").contains("overlap")); // same as quit
    assert!(err("toggle = \"LEFT_COMMAND+F1\"").contains("overlap")); // identical to a profile slot
    assert!(err("toggle = \"F1\"").contains("overlap")); // subset of slot LEFT_COMMAND+F1
    assert!(err("panic_key = \"F7\"").contains("overlap")); // profile_key
    assert!(err("overlay_key = \"F11\"").contains("overlap")); // panic
                                                               // disjoint chords are fine; modifier+key chord distinct from bare key
    assert!(load_layers(Some("toggle = \"LEFT_CONTROL+F10\""), false).is_ok());
    // a hotkey that is a bare modifier collides with the slot chords that contain it
    assert!(err("toggle = \"LEFT_COMMAND\"").contains("overlap"));
}

#[test]
fn bindings_on_hotkeys_or_command_are_rejected() {
    for k in ["F10", "F12", "F7", "F11", "F8", "LEFT_COMMAND", "RIGHT_COMMAND", "F3"] {
        let t = format!("[bindings]\nCROSS = [\"{k}\"]");
        let e = err(&t);
        assert!(e.contains("reserved"), "{k}: {e}");
    }
    // only TAB + click is refused, TAB alone and click alone are fine
    assert!(load_layers(Some("[bindings]\nCROSS = [\"TAB+MOUSE_LEFT\"]"), false).is_err());
    assert!(load_layers(Some("[bindings]\nCROSS = [\"TAB\"]"), false).is_ok());
    assert!(load_layers(Some("[bindings]\nCROSS = [\"MOUSE_LEFT\"]"), false).is_ok());
    // reserved check applies to profile bindings and combos as well
    assert!(err("[profiles.x.bindings]\nCROSS = [\"F10\"]").contains("reserved"));
    assert!(err("[[combos]]\ninput = \"F10+W\"\noutput = \"CROSS\"").contains("reserved"));
}

#[test]
fn too_many_profile_slots_or_bad_default_name_are_rejected() {
    let slots: Vec<String> = (1..=13).map(|n| format!("\"LEFT_OPTION+F{}\"", n.min(12))).collect();
    assert!(load_layers(Some(&format!("profile_slots = [{}]", slots.join(","))), false).is_err());
    assert!(err("default_name = \"\"").contains("default_name"));
    assert!(err("default_name = \"racing\"").contains("racing")); // collides with shipped profile
    assert!(err("[profiles.\"\".bindings]\nCROSS = [\"W\"]").contains("profile name"));
}

#[test]
fn hotkeys_on_scroll_or_mouse_move_are_rejected() {
    for field in ["panic_key", "quit", "toggle", "overlay_key", "profile_key"] {
        for k in ["SCROLL_UP", "MOUSE_MOVE_LEFT"] {
            let t = format!("use_defaults = false\n{field} = \"{k}\"");
            let r = load_layers(Some(&t), false);
            assert!(r.as_ref().is_err_and(|e| e.contains(field) || e.contains("hotkey")), "{field}={k}: {r:?}");
        }
    }
}

// ---------------------------------------------------------------- migration

#[test]
fn migrate_hotkeys_rewrites_only_the_old_pair() {
    assert_eq!(migrate_hotkeys("panic_key = \"F9\"\nquit = \"F12\""), "panic_key = \"F11\"\nquit = \"F12\"");
    assert_eq!(migrate_hotkeys("profile_key = \"F11\""), "profile_key = \"F7\"");
    // trailing comment kept as long as the code part matches
    assert_eq!(migrate_hotkeys("panic_key=\"F9\" # old"), "panic_key = \"F11\"");
    // already-new files are untouched byte for byte (incl. trailing newline)
    let new = "panic_key = \"F11\"\nprofile_key = \"F7\"\n";
    assert_eq!(migrate_hotkeys(new), new);
    // user who chose profile_key = F11 *and* has a panic_key is not touched
    let both = "profile_key = \"F11\"\npanic_key = \"F9\"\n";
    assert_eq!(migrate_hotkeys(both), "profile_key = \"F7\"\npanic_key = \"F11\"");
    // CRLF input
    assert!(migrate_hotkeys("panic_key = \"F9\"\r\nquit = \"F12\"\r\n").contains("panic_key = \"F11\""));
}

#[test]
fn migrate_hotkeys_info_limits() {
    // Info (T07-20): literal-string quoting is not recognised, and a trailing newline is dropped when it rewrites.
    let lit = "panic_key = 'F9'\n";
    assert_eq!(migrate_hotkeys(lit), lit);
    assert!(!migrate_hotkeys("panic_key = \"F9\"\n").ends_with('\n'));
    // inside a TOML multi-line string the same text is rewritten too (text-level, not TOML-aware)
    let t = "description = \"\"\"\npanic_key = \"F9\"\n\"\"\"\n";
    assert_ne!(migrate_hotkeys(t), t);
}

#[test]
fn migration_drops_reserved_bindings_and_reports_notes() {
    let t = "[bindings]\nCROSS = [\"F10\", \"X\"]\nCIRCLE = [\"LEFT_COMMAND\"]\n";
    assert!(load_layers(Some(t), false).is_err()); // strict
    let c = load_layers(Some(t), true).unwrap(); // implicit file: tolerant
    let b = c.profile_bindings("default");
    assert_eq!(b["CROSS"], vec!["X".to_string()]);
    assert!(!c.notes.is_empty());
    // CIRCLE falls back to the shipped value
    assert!(!b["CIRCLE"].is_empty() && !b["CIRCLE"].contains(&"LEFT_COMMAND".to_string()));
}

#[test]
fn migration_never_turns_a_valid_file_into_an_invalid_one() {
    for old in ["toggle = \"F7\"\nquit = \"F12\"\nprofile_key = \"F11\"\n", "quit = \"F11\"\npanic_key = \"F9\"\n"] {
        let r = load_layers(Some(old), true);
        assert!(r.is_ok(), "{old:?}: {:?}", r.err());
    }
}

#[test]
fn deleted_profiles_and_use_defaults_false() {
    let c = load_layers(Some("deleted_profiles = [\"racing\"]"), false).unwrap();
    assert!(!c.profile_names().contains(&"racing".to_string()));
    // use_defaults = false starts from nothing
    let c = load_layers(Some("use_defaults = false"), false).unwrap();
    assert_eq!(c.profile_names(), vec!["default".to_string()]);
    assert!(c.profile_bindings("default").is_empty());
    // deleting a profile that does not exist is harmless
    assert!(load_layers(Some("deleted_profiles = [\"nope\", \"\\u0000\"]"), false).is_ok());
}

#[test]
fn unknown_profile_in_resolve_is_an_error() {
    let c = load_defaults().unwrap();
    assert!(c.resolve(Some("nope")).is_err());
    assert!(c.resolve(None).is_ok());
}

#[test]
fn migration_is_idempotent_and_keeps_the_trailing_state_stable() {
    for old in [
        "toggle = \"F7\"\nquit = \"F12\"\nprofile_key = \"F11\"\n",
        "quit = \"F11\"\npanic_key = \"F9\"\n",
        "panic_key = \"F9\"\nprofile_key = \"F11\"\n",
        "profile_key = \"F11\"\n",
    ] {
        let once = migrate_hotkeys(old);
        assert_eq!(migrate_hotkeys(&once), once, "{old:?}");
    }
}

#[test]
fn nesting_deeper_than_64_is_refused_without_parsing() {
    let e = load_layers(Some(&"[".repeat(500)), false).unwrap_err();
    assert!(e.contains("nested"), "{e}");
    let ok = format!("# {}\npoll_hz = 250\ns = \"{}\"", "[".repeat(500), "{".repeat(500));
    assert!(load_layers(Some(&ok), false).is_err()); // unknown key s, but not for nesting
    assert!(!load_layers(Some(&ok), false).unwrap_err().contains("nested"));
}
