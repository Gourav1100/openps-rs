//! Tester A4: editor / config writer invariants (round trips, profile ops, migration on save, failure modes).
use openps_input::config::{load_path, Config};
use openps_input::editor::{Editor, PRIMARY, SECONDARY};
use openps_input::keys::{Chord, Input, Key};
use openps_input::state::{Axis, Button, Control};
use std::collections::BTreeMap;
use tempfile::TempDir;

fn chord(items: &[Key]) -> Chord {
    items.iter().map(|k| Input::Key(*k)).collect()
}
fn b(x: Button) -> Control {
    Control::Button(x)
}
fn write(d: &TempDir, text: &str) -> std::path::PathBuf {
    let p = d.path().join("u.toml");
    std::fs::write(&p, text).unwrap();
    p
}
fn all_bindings(c: &Config) -> BTreeMap<String, BTreeMap<String, Vec<String>>> {
    c.profile_names().into_iter().map(|n| (n.clone(), c.profile_bindings(&n))).collect()
}

#[test]
fn saving_an_untouched_default_file_changes_nothing_semantically() {
    let d = TempDir::new().unwrap();
    let p = write(&d, "");
    let before = load_path(&p, false).unwrap();
    let e = Editor::new(&p).unwrap();
    e.save().unwrap();
    let after = load_path(&p, false).unwrap();
    assert_eq!(all_bindings(&before), all_bindings(&after));
    assert_eq!(before.profile_names(), after.profile_names());
    // a second save is a fixpoint
    let t1 = std::fs::read_to_string(&p).unwrap();
    Editor::new(&p).unwrap().save().unwrap();
    assert_eq!(t1, std::fs::read_to_string(&p).unwrap());
}

#[test]
fn use_defaults_false_bindings_survive_an_unrelated_edit() {
    let d = TempDir::new().unwrap();
    let p = write(&d, "use_defaults = false\n[bindings]\nCROSS = [\"SPACE\"]\nCIRCLE = [\"E\"]\n");
    let mut e = Editor::new(&p).unwrap();
    e.replace(b(Button::CIRCLE), &chord(&[Key::R]));
    e.save().unwrap();
    let after = load_path(&p, false).unwrap();
    assert_eq!(after.profile_bindings("default")["CROSS"], vec!["SPACE".to_string()]);
}

#[test]
fn saved_file_is_fully_migrated() {
    let d = TempDir::new().unwrap();
    let p = write(&d, "panic_key = \"F9\"\nprofile_key = \"F11\"\n");
    Editor::new(&p).unwrap().save().unwrap();
    let c = load_path(&p, false).unwrap();
    assert_eq!((c.panic_key.as_str(), c.profile_key.as_str()), ("F11", "F7"));
}

#[test]
fn editing_through_the_editor_discards_user_comments_info() {
    // Info (T07-10): the file is rewritten from a parsed table; comments and key order are not preserved.
    let d = TempDir::new().unwrap();
    let p = write(&d, "# my precious notes\npoll_hz = 250 # fast\n");
    let mut e = Editor::new(&p).unwrap();
    e.replace(b(Button::CROSS), &chord(&[Key::Z]));
    e.save().unwrap();
    let t = std::fs::read_to_string(&p).unwrap();
    assert!(!t.contains("precious") && t.contains("poll_hz = 250"), "{t}");
}

#[test]
fn profile_add_rename_delete_invariants() {
    let d = TempDir::new().unwrap();
    let p = write(&d, "");
    let mut e = Editor::new(&p).unwrap();
    let n0 = e.profile_names().len();
    // names: empty, or a duplicate once converted to snake_case (anything else typed is converted, never refused)
    for bad in ["", "   ", "DEFAULT", "Racing", "racing"] {
        assert!(e.add_profile(bad, None).is_err(), "{bad:?}");
    }
    assert_eq!(e.profile_names().len(), n0);
    for odd in ["é", "x\0", "../../etc", "a b"] {
        let n = e.profile_names().len();
        e.add_profile(odd, None).unwrap();
        assert_eq!(e.profile_names().len(), n + 1);
        let added = e.profile_names().pop().unwrap();
        assert!(openps_input::config::valid_profile_name(&added), "{odd:?} -> {added:?}");
        e.delete_profile(&added).unwrap();
    }
    e.add_profile("mine", Some("racing")).unwrap();
    assert!(e.add_profile("MINE", None).is_err());
    assert!(e.add_profile("other", Some("nope")).is_err());
    // rename keeps order and contents, and refuses collisions
    let before = e.profile("mine").unwrap().clone();
    e.rename_profile("mine", "mine2").unwrap();
    assert_eq!(e.profile("mine2"), Some(&before));
    assert!(e.profile("mine").is_none());
    assert!(e.rename_profile("mine2", "racing").is_err());
    assert!(e.rename_profile("ghost", "x").is_err());
    e.rename_profile("mine2", "mine2").unwrap(); // no-op
                                                 // rename the base profile, then name something "default" again
    e.rename_profile("default", "base").unwrap();
    assert_eq!(e.default_name(), "base");
    e.add_profile("default", None).unwrap();
    // the base cannot be deleted; everything else can
    assert!(e.delete_profile("base").is_err());
    e.delete_profile("default").unwrap();
    e.delete_profile("mine2").unwrap();
    assert!(e.delete_profile("mine2").is_err());
    // shipped profile deleted: stays deleted after save+reload
    e.delete_profile("racing").unwrap();
    e.save().unwrap();
    let c = load_path(&p, false).unwrap();
    assert_eq!(c.default_name, "base");
    assert!(!c.profile_names().contains(&"racing".to_string()));
    assert!(c.deleted_profiles.contains(&"racing".to_string()));
    // and a re-created user profile with the deleted shipped name is a plain user profile
    let mut e = Editor::new(&p).unwrap();
    e.add_profile("racing", None).unwrap();
    e.save().unwrap();
    let c = load_path(&p, false).unwrap();
    assert_eq!(c.profile_bindings("racing"), c.profile_bindings("base"));
}

#[test]
fn every_profile_survives_save_and_reload_unchanged() {
    let d = TempDir::new().unwrap();
    let p = write(&d, "");
    let mut e = Editor::new(&p).unwrap();
    e.add_profile("p1", None).unwrap();
    e.set_current("p1");
    e.set_slot(b(Button::CROSS).into_ctl(), PRIMARY, &chord(&[Key::Z]));
    e.set_slot(Control::Axis(Axis::R2), SECONDARY, &chord(&[Key::X]));
    e.clear_slot(b(Button::CIRCLE), PRIMARY);
    e.set_current("default");
    e.set_slot(b(Button::TRIANGLE), PRIMARY, &chord(&[Key::LEFT_SHIFT, Key::T]));
    e.save().unwrap();
    let c = load_path(&p, false).unwrap();
    let e2 = Editor::new(&p).unwrap();
    for n in c.profile_names() {
        let want = c.profile_bindings(&n);
        let got = e2.profile(&n).unwrap();
        for (ctl, v) in want {
            assert_eq!(got.get(&ctl), Some(&v), "{n}/{ctl}");
        }
    }
    // base edits propagate to profiles that inherit the control, but not to profiles that override it
    assert_eq!(c.profile_bindings("p1")["TRIANGLE"], vec!["LEFT_SHIFT+T".to_string()]);
    assert_eq!(c.profile_bindings("p1")["CROSS"], vec!["Z".to_string()]);
}

trait IntoCtl {
    fn into_ctl(self) -> Control;
}
impl IntoCtl for Control {
    fn into_ctl(self) -> Control {
        self
    }
}

#[test]
fn unknown_current_profile_does_not_panic() {
    let d = TempDir::new().unwrap();
    let mut e = Editor::new(d.path().join("u.toml")).unwrap();
    e.set_current("ghost");
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = e.bindings();
    }));
    assert!(r.is_ok());
}

#[test]
fn a_bad_edit_never_clobbers_the_file() {
    let d = TempDir::new().unwrap();
    let p = write(&d, "poll_hz = 250\n");
    let mut e = Editor::new(&p).unwrap();
    // reserved hotkey as a binding: save must refuse and leave the old file byte for byte
    let before = std::fs::read(&p).unwrap();
    e.replace(b(Button::CROSS), &chord(&[Key::F10]));
    assert!(e.save().is_err());
    assert_eq!(std::fs::read(&p).unwrap(), before);
    assert!(e.validate().is_err());
    // hotkey conflict via set_toggle
    let mut e = Editor::new(&p).unwrap();
    e.set_toggle("F12");
    assert!(e.save().is_err());
    assert_eq!(std::fs::read(&p).unwrap(), before);
    // no temp file left behind
    let names: Vec<_> = std::fs::read_dir(d.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(names.len(), 1, "{names:?}");
}

#[cfg(unix)]
#[test]
fn unwritable_directory_is_an_error_not_a_panic_and_leaves_the_file_intact() {
    use std::os::unix::fs::PermissionsExt;
    let d = TempDir::new().unwrap();
    let p = write(&d, "poll_hz = 250\n");
    let mut e = Editor::new(&p).unwrap();
    e.replace(b(Button::CROSS), &chord(&[Key::Z]));
    std::fs::set_permissions(d.path(), std::fs::Permissions::from_mode(0o555)).unwrap();
    let r = e.save();
    std::fs::set_permissions(d.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    if r.is_ok() {
        return; // running as root: nothing to assert
    }
    assert_eq!(std::fs::read_to_string(&p).unwrap(), "poll_hz = 250\n");
}

#[test]
fn corrupt_or_huge_existing_files_are_reported_not_overwritten_silently() {
    let d = TempDir::new().unwrap();
    let p = write(&d, "[bindings\nCROSS = ");
    let e = Editor::new(&p);
    assert!(e.is_err(), "a corrupt file must not be replaced by defaults");
    assert_eq!(std::fs::read_to_string(&p).unwrap(), "[bindings\nCROSS = ");
    // non-UTF-8
    std::fs::write(&p, [0xFF, 0xFE, 0x00, 0x41]).unwrap();
    assert!(Editor::new(&p).is_err());
    // 6 MB of comments is fine and saves quickly
    std::fs::write(&p, format!("# {}\n", "y".repeat(6 << 20))).unwrap();
    let t = std::time::Instant::now();
    let e = Editor::new(&p).unwrap();
    e.save().unwrap();
    assert!(t.elapsed().as_secs() < 10);
}

#[test]
fn set_slot_secondary_semantics_and_duplicates() {
    let d = TempDir::new().unwrap();
    let mut e = Editor::new(d.path().join("u.toml")).unwrap();
    let ctl = b(Button::CROSS);
    e.set_slot(ctl, SECONDARY, &chord(&[Key::SPACE])); // same as primary: not duplicated
    assert_eq!(e.slot(ctl, SECONDARY), "-");
    e.set_slot(ctl, SECONDARY, &chord(&[Key::Z]));
    assert_eq!(e.slot(ctl, SECONDARY), "Z");
    e.clear_slot(ctl, PRIMARY); // the secondary is promoted
    assert_eq!(e.slot(ctl, PRIMARY), "Z");
    e.clear_slot(ctl, PRIMARY);
    e.clear_slot(ctl, PRIMARY); // clearing an empty slot is harmless
    assert_eq!(e.slot(ctl, PRIMARY), "-");
    e.remove_at(ctl, 99);
    let owners = e.owners(&chord(&[Key::SPACE]), None);
    assert!(!owners.contains(&"CROSS".to_string()));
}

#[test]
fn set_current_ignores_unknown_names() {
    let d = TempDir::new().unwrap();
    let mut e = Editor::new(d.path().join("u.toml")).unwrap();
    let before = e.current().to_string();
    assert!(!e.set_current("ghost"));
    assert_eq!(e.current(), before);
    assert!(e.set_current("racing"));
}
