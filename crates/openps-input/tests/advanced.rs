//! Advanced mapping: layers per scene, action labels and the scene toggle, through config, editor, mapper and engine.
use openps_input::config::{load_defaults, load_layers, load_path, Config};
use openps_input::editor::Editor;
use openps_input::engine::{Bounds, Engine, OverlayState, Platform, UiEvent};
use openps_input::keys::{parse_chord, Chord, Input, Key, MouseInput};
use openps_input::scene::{Action, Layer, SceneContext, SceneLink};
use openps_input::state::{parse_control, Button, ControllerState};
use openps_input::InputSnapshot;
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::rc::Rc;
use std::sync::Arc;
use tempfile::TempDir;

const WINDOW: Bounds = (100.0, 200.0, 800.0, 600.0);

#[derive(Default)]
struct Shared {
    held: BTreeSet<Input>,
    pads: Vec<ControllerState>,
    overlay: Vec<OverlayState>,
}
type S = Rc<RefCell<Shared>>;
struct Mock(S);

impl Platform for Mock {
    fn is_frontmost(&mut self, _: &str) -> bool {
        true
    }
    fn window_bounds(&mut self, _: &str) -> Option<Bounds> {
        Some(WINDOW)
    }
    fn snapshot(&mut self) -> InputSnapshot {
        InputSnapshot::new(self.0.borrow().held.clone(), 0.0, 0.0)
    }
    fn set_exclusive(&mut self, _: bool, _: bool) {}
    fn set_hotkey_swallow(&mut self, _: bool) {}
    fn set_hotkeys(&mut self, _: &[Chord]) {}
    fn cursor_engage(&mut self, _: Option<(f64, f64)>) {}
    fn cursor_recenter(&mut self, _: (f64, f64)) {}
    fn cursor_release(&mut self) {}
    fn cursor_set_hide(&mut self, _: bool) {}
    fn send_pad(&mut self, st: &ControllerState) {
        self.0.borrow_mut().pads.push(st.clone());
    }
    fn beep(&mut self, _: &str, _: bool) {}
    fn force_quit(&mut self, _: &str) -> Vec<u32> {
        vec![]
    }
    fn overlay_push(&mut self, s: &OverlayState) {
        self.0.borrow_mut().overlay.push(s.clone());
    }
}

struct Rig {
    e: Engine<Mock>,
    s: S,
    link: Arc<SceneLink>,
    path: std::path::PathBuf,
    t: f64,
    _d: TempDir,
}

fn k(key: Key) -> Input {
    Input::Key(key)
}

impl Rig {
    /// An engine whose user file starts as `toml`.
    fn with(toml: &str) -> Self {
        let d = TempDir::new().unwrap();
        let path = d.path().join("user.toml");
        std::fs::write(&path, toml).unwrap();
        let s: S = Rc::new(RefCell::new(Shared::default()));
        let link = Arc::new(SceneLink::new());
        let mut cfg = load_path(&path, false).unwrap();
        cfg.feedback_sounds = false;
        let mut e = Engine::new(cfg, Mock(s.clone()), false).unwrap().with_config_path(&path).with_scene(link.clone());
        e.start(100.0);
        e.toggle_overlay();
        Rig { e, s, link, path, t: 100.0, _d: d }
    }
    fn hold(&self, keys: &[Key]) {
        self.s.borrow_mut().held = keys.iter().map(|x| k(*x)).collect();
    }
    fn tick(&mut self) {
        self.t += 0.01;
        self.e.tick(self.t);
    }
    fn ui(&mut self, v: serde_json::Value) {
        self.e.handle_ui_event(UiEvent::from_json(&v).expect("a known event"));
    }
    fn saved(&self) -> Config {
        load_path(&self.path, false).unwrap()
    }
    fn last_pad(&self) -> ControllerState {
        self.s.borrow().pads.last().cloned().unwrap_or_default()
    }
    fn last_overlay(&self) -> OverlayState {
        self.s.borrow().overlay.last().unwrap().clone()
    }
    /// Hold `keys` for a few ticks and return what the pad shows.
    fn press(&mut self, keys: &[Key]) -> ControllerState {
        self.hold(keys);
        for _ in 0..3 {
            self.tick();
        }
        let st = self.last_pad();
        self.hold(&[]);
        for _ in 0..3 {
            self.tick();
        }
        st
    }
}

const LAYERED: &str = r#"
[advanced]
enabled = true
[advanced.on_foot]
CROSS = ["Z"]
TRIANGLE = ["F"]
[advanced.in_vehicle]
CROSS = ["X"]
TRIANGLE = ["F"]
[advanced.in_menu]
CROSS = ["C"]
[advanced.labels]
TRIANGLE = "key_enter_or_exit_vehicle"
L1 = "key_change_camera"
OPTIONS = "key_pause_game"
"#;

// ---- config -------------------------------------------------------------------------------------------
#[test]
fn layers_and_labels_load_and_the_layer_picks_the_bindings() {
    let cfg = load_layers(Some(LAYERED), false).unwrap();
    let adv = cfg.advanced_of("default").unwrap();
    assert!(adv.enabled);
    assert_eq!(adv.label_of("TRIANGLE"), Some(Action::EnterOrExitVehicle));
    let keys_for = |layer| {
        let b = cfg.resolve_layer(None, layer).unwrap();
        b.iter().find(|b| b.outputs == [parse_control("CROSS").unwrap()]).map(|b| b.inputs.clone())
    };
    assert_eq!(keys_for(Layer::OnFoot), Some(parse_chord("Z").unwrap()));
    assert_eq!(keys_for(Layer::InVehicle), Some(parse_chord("X").unwrap()));
    assert_eq!(keys_for(Layer::InMenu), Some(parse_chord("C").unwrap()));
}

#[test]
fn a_plain_profile_has_the_same_keys_in_every_layer() {
    let cfg = load_defaults().unwrap();
    let a = cfg.resolve_layer(None, Layer::OnFoot).unwrap();
    assert_eq!(a, cfg.resolve(None).unwrap());
    assert_eq!(a, cfg.resolve_layer(None, Layer::InVehicle).unwrap());
}

#[test]
fn switching_advanced_off_brings_the_primary_keys_back() {
    let text = LAYERED.replace("enabled = true", "enabled = false");
    let cfg = load_layers(Some(&text), false).unwrap();
    assert_eq!(cfg.resolve_layer(None, Layer::InVehicle).unwrap(), cfg.resolve(None).unwrap());
}

#[test]
fn control_names_in_layers_and_labels_are_normalised() {
    let cfg = load_layers(Some("[advanced]\nenabled = true\n[advanced.on_foot]\ncross = \"Z\"\n[advanced.labels]\ntriangle = \"key_change_camera\"\n"), false)
        .unwrap();
    let adv = cfg.advanced_of("default").unwrap();
    assert_eq!(adv.on_foot["CROSS"], vec!["Z"]);
    assert_eq!(adv.label_of("TRIANGLE"), Some(Action::ChangeCamera));
}

#[test]
fn bad_labels_and_layer_keys_are_refused() {
    let err = |t: &str| load_layers(Some(t), false).unwrap_err();
    assert!(err("[advanced.labels]\nTRIANGLE = \"key_jump\"\n").contains("unknown action"));
    assert!(err("[advanced.labels]\nLEFT_STICK_UP = \"key_pause_game\"\n").contains("cannot carry an action"));
    assert!(err("[advanced.labels]\nNOPE = \"key_pause_game\"\n").contains("unknown PS5 control"));
    assert!(err("[advanced.in_vehicle]\nCROSS = \"LEFT_COMMAND\"\n").contains("reserved"));
    assert!(err("[advanced.on_foot]\nCROSS = \"NOT_A_KEY\"\n").len() > 3);
    assert!(
        err("[advanced]\nbogus = 1\n").contains("bogus") || err("[advanced]\nbogus = 1\n").contains("unknown field")
    );
}

#[test]
fn a_profile_has_its_own_advanced_mode() {
    let cfg = load_layers(
        Some("[profiles.gta.advanced]\nenabled = true\n[profiles.gta.advanced.in_vehicle]\nCROSS = \"X\"\n"),
        false,
    )
    .unwrap();
    assert!(cfg.advanced_of("gta").unwrap().enabled);
    assert!(!cfg.advanced_of("default").unwrap().enabled);
    let b = cfg.resolve_layer(Some("gta"), Layer::InVehicle).unwrap();
    assert!(b.iter().any(|b| b.inputs == parse_chord("X").unwrap()));
    assert!(cfg.advanced_of("nope").is_none());
}

// ---- editor -------------------------------------------------------------------------------------------
#[test]
fn switching_advanced_on_copies_the_primary_keys_into_every_layer_once() {
    let d = TempDir::new().unwrap();
    let path = d.path().join("user.toml");
    let mut ed = Editor::new(&path).unwrap();
    let cross = parse_control("CROSS").unwrap();
    ed.set_advanced(true);
    for l in Layer::ALL {
        assert_eq!(ed.layer_slot(cross, l), "SPACE");
    }
    ed.set_layer_slot(cross, Layer::InVehicle, &parse_chord("X").unwrap());
    ed.set_advanced(false);
    ed.set_advanced(true); // the layers already exist: they are not overwritten
    assert_eq!(ed.layer_slot(cross, Layer::InVehicle), "X");
    ed.save().unwrap();
    let cfg = load_path(&path, false).unwrap();
    assert_eq!(cfg.advanced.in_vehicle["CROSS"], vec!["X"]);
    assert_eq!(cfg.advanced.on_foot["CROSS"], vec!["SPACE"]);
    assert!(cfg.advanced.enabled);
    // the primary and secondary keys are still in the file for when advanced mode is switched off
    assert_eq!(cfg.profile_bindings("default")["CROSS"], vec!["SPACE"]);
}

#[test]
fn a_control_has_one_label_and_a_label_can_be_on_many_controls() {
    let d = TempDir::new().unwrap();
    let path = d.path().join("user.toml");
    let mut ed = Editor::new(&path).unwrap();
    ed.set_advanced(true);
    let t = |n: &str| parse_control(n).unwrap();
    ed.set_label(t("TRIANGLE"), Some(Action::EnterOrExitVehicle)).unwrap();
    ed.set_label(t("SQUARE"), Some(Action::EnterOrExitVehicle)).unwrap();
    ed.set_label(t("TRIANGLE"), Some(Action::PauseGame)).unwrap(); // replaces, never adds a second
    ed.set_label(t("R2"), Some(Action::ChangeCamera)).unwrap();
    assert!(ed.set_label(t("LEFT_STICK_UP"), Some(Action::PauseGame)).is_err());
    assert!(ed.set_label(t("TOUCHPAD_TAP"), Some(Action::PauseGame)).is_err());
    ed.save().unwrap();
    let adv = load_path(&path, false).unwrap().advanced;
    assert_eq!(adv.labels.len(), 3);
    assert_eq!(adv.label_of("TRIANGLE"), Some(Action::PauseGame));
    assert_eq!(adv.label_of("SQUARE"), Some(Action::EnterOrExitVehicle));
    assert_eq!(adv.label_of("R2"), Some(Action::ChangeCamera));
    let mut ed = Editor::new(&path).unwrap();
    ed.set_label(t("TRIANGLE"), None).unwrap();
    ed.save().unwrap();
    assert_eq!(load_path(&path, false).unwrap().advanced.label_of("TRIANGLE"), None);
}

#[test]
fn advanced_data_follows_its_profile() {
    let d = TempDir::new().unwrap();
    let path = d.path().join("user.toml");
    let mut ed = Editor::new(&path).unwrap();
    ed.set_advanced(true);
    ed.set_label(parse_control("TRIANGLE").unwrap(), Some(Action::PauseGame)).unwrap();
    ed.add_profile("gta", None).unwrap();
    ed.set_current("gta");
    assert!(ed.advanced().enabled, "a copy keeps the advanced mode of its source");
    ed.rename_profile("gta", "gta_five").unwrap();
    ed.set_current("gta_five");
    ed.set_advanced(false);
    ed.save().unwrap();
    let cfg = load_path(&path, false).unwrap();
    assert!(cfg.advanced_of("default").unwrap().enabled);
    assert!(!cfg.advanced_of("gta_five").unwrap().enabled);
    assert_eq!(cfg.advanced_of("gta_five").unwrap().label_of("TRIANGLE"), Some(Action::PauseGame));
    let mut ed = Editor::new(&path).unwrap();
    ed.reset_profile(Some("gta_five")).unwrap();
    ed.delete_profile("gta_five").unwrap();
    ed.save().unwrap();
    assert!(load_path(&path, false).unwrap().advanced_of("gta_five").is_none());
}

// ---- engine -------------------------------------------------------------------------------------------
#[test]
fn the_overlay_edits_advanced_mode_layer_keys_and_labels() {
    let mut r = Rig::with("");
    assert!(!r.last_overlay().advanced);
    // a plain profile only has the primary and secondary slots
    r.ui(serde_json::json!({"type": "edit", "control": "CROSS", "slot": 4}));
    assert!(!r.e.capturing());
    r.ui(serde_json::json!({"type": "advanced", "enabled": true}));
    let o = r.last_overlay();
    assert!(o.advanced);
    let row = o.rows.iter().find(|x| x.control == "CROSS").unwrap();
    assert_eq!((row.on_foot.as_str(), row.in_vehicle.as_str(), row.in_menu.as_str()), ("SPACE", "SPACE", "SPACE"));
    // and only the layer slots now
    r.ui(serde_json::json!({"type": "edit", "control": "CROSS", "slot": 1}));
    assert!(!r.e.capturing());
    r.ui(serde_json::json!({"type": "edit", "control": "CROSS", "slot": 4}));
    assert!(r.e.capturing());
    r.hold(&[Key::X]);
    r.tick();
    r.hold(&[]);
    r.tick();
    r.t += 0.5;
    r.tick();
    assert_eq!(r.saved().advanced.in_vehicle["CROSS"], vec!["X"]);
    assert_eq!(r.saved().advanced.on_foot["CROSS"], vec!["SPACE"]);
    // labels
    r.ui(serde_json::json!({"type": "label", "control": "TRIANGLE", "label": "key_enter_or_exit_vehicle"}));
    r.ui(serde_json::json!({"type": "label", "control": "SQUARE", "label": "key_enter_or_exit_vehicle"}));
    r.ui(serde_json::json!({"type": "label", "control": "LEFT_STICK_UP", "label": "key_pause_game"}));
    r.ui(serde_json::json!({"type": "label", "control": "CIRCLE", "label": "key_nonsense"}));
    let o = r.last_overlay();
    let label = |c: &str| o.rows.iter().find(|x| x.control == c).unwrap().label.clone();
    assert_eq!(label("TRIANGLE").as_deref(), Some("key_enter_or_exit_vehicle"));
    assert_eq!(label("SQUARE").as_deref(), Some("key_enter_or_exit_vehicle"));
    assert_eq!(label("LEFT_STICK_UP"), None);
    assert_eq!(label("CIRCLE"), None);
    assert!(o.rows.iter().find(|x| x.control == "CROSS").unwrap().can_label);
    assert!(!o.rows.iter().find(|x| x.control == "LEFT_STICK_UP").unwrap().can_label);
    assert_eq!(o.actions.len(), 3);
    r.ui(serde_json::json!({"type": "label", "control": "SQUARE", "label": null}));
    assert_eq!(r.last_overlay().rows.iter().find(|x| x.control == "SQUARE").unwrap().label, None);
    r.ui(serde_json::json!({"type": "advanced", "enabled": false}));
    assert!(!r.last_overlay().advanced);
}

#[test]
fn a_layer_the_game_has_no_use_for_can_be_switched_off_and_is_never_followed() {
    let mut r = Rig::with(LAYERED);
    r.e.apply_enabled(true);
    r.link.set_enabled(true);
    assert!(r.last_overlay().layers.in_vehicle);
    r.e.toggle_overlay();
    r.e.toggle_overlay(); // the overlay is visible with mapping on
    r.ui(serde_json::json!({"type": "layer_enabled", "layer": "in_vehicle", "enabled": false}));
    assert_eq!(r.saved().advanced_of("default").unwrap().disabled_layers, ["in_vehicle"]);
    assert!(r.saved().advanced.on_foot.contains_key("CROSS"), "the keys of a switched-off layer are kept");
    r.t += 1.0;
    r.tick();
    let o = r.last_overlay();
    assert_eq!((o.layers.on_foot, o.layers.in_vehicle, o.layers.in_menu), (true, false, true));
    // the detector is told, and a vehicle scene keeps the on-foot keys
    assert_eq!(r.link.allowed_contexts(), [true, false, true, true]);
    r.link.publish_scene(SceneContext::InVehicle, None, 0.9);
    assert!(r.press(&[Key::Z]).buttons.contains(&Button::CROSS));
    assert!(!r.press(&[Key::X]).buttons.contains(&Button::CROSS));
    r.link.publish_scene(SceneContext::Menu, None, 0.9);
    assert!(r.press(&[Key::C]).buttons.contains(&Button::CROSS), "the other layers still work");
    // and it comes back
    r.ui(serde_json::json!({"type": "layer_enabled", "layer": "in_vehicle", "enabled": true}));
    assert!(r.saved().advanced.disabled_layers.is_empty());
    r.link.publish_scene(SceneContext::InVehicle, None, 0.9);
    assert!(r.press(&[Key::X]).buttons.contains(&Button::CROSS));
}

#[test]
fn the_last_layer_in_use_cannot_be_switched_off_and_bad_layers_are_ignored() {
    let mut r = Rig::with(LAYERED);
    r.ui(serde_json::json!({"type": "layer_enabled", "layer": "in_vehicle", "enabled": false}));
    r.ui(serde_json::json!({"type": "layer_enabled", "layer": "in_menu", "enabled": false}));
    r.ui(serde_json::json!({"type": "layer_enabled", "layer": "on_foot", "enabled": false}));
    assert_eq!(r.saved().advanced.enabled_layers(), [Layer::OnFoot]);
    r.ui(serde_json::json!({"type": "layer_enabled", "layer": "in_the_air", "enabled": false}));
    assert_eq!(r.saved().advanced.enabled_layers(), [Layer::OnFoot]);
    assert!(UiEvent::from_json(&serde_json::json!({"type": "layer_enabled", "layer": "on_foot"})).is_none());
    // a file that switches every layer off is refused
    let err = load_layers(Some("[advanced]\ndisabled_layers = [\"on_foot\", \"in_vehicle\", \"in_menu\"]\n"), false)
        .unwrap_err();
    assert!(err.contains("every layer"), "{err}");
    let err = load_layers(Some("[advanced]\ndisabled_layers = [\"flying\"]\n"), false).unwrap_err();
    assert!(err.contains("unknown layer"), "{err}");
}

#[test]
fn each_profile_keeps_its_own_layers() {
    let r = Rig::with("[profiles.racing.advanced]\nenabled = true\ndisabled_layers = [\"on_foot\"]\n");
    let cfg = r.saved();
    assert_eq!(cfg.advanced_of("racing").unwrap().enabled_layers(), [Layer::InVehicle, Layer::InMenu]);
    assert_eq!(cfg.advanced_of("default").unwrap().enabled_layers(), Layer::ALL);
}

#[test]
fn the_keys_follow_the_detected_scene() {
    let mut r = Rig::with(LAYERED);
    r.e.apply_enabled(true);
    r.link.set_enabled(true);
    // nothing detected yet: on foot
    assert!(r.press(&[Key::Z]).buttons.contains(&Button::CROSS));
    assert!(!r.press(&[Key::X]).buttons.contains(&Button::CROSS));
    r.link.publish_scene(SceneContext::InVehicle, None, 0.9);
    assert!(r.press(&[Key::X]).buttons.contains(&Button::CROSS));
    assert!(!r.press(&[Key::Z]).buttons.contains(&Button::CROSS));
    r.link.publish_scene(SceneContext::Menu, None, 0.9);
    assert!(r.press(&[Key::C]).buttons.contains(&Button::CROSS));
    r.link.publish_scene(SceneContext::Cutscene, None, 0.9);
    assert!(r.press(&[Key::C]).buttons.contains(&Button::CROSS), "a cutscene uses the menu layer");
    r.link.publish_scene(SceneContext::OnFoot, None, 0.9);
    assert!(r.press(&[Key::Z]).buttons.contains(&Button::CROSS));
}

#[test]
fn a_scene_seen_before_detection_started_is_forgotten() {
    let mut r = Rig::with(LAYERED);
    r.e.apply_enabled(true);
    r.link.publish_scene(SceneContext::InVehicle, None, 0.9); // nothing was looking when this was written
    assert!(r.press(&[Key::Z]).buttons.contains(&Button::CROSS));
    assert!(!r.press(&[Key::X]).buttons.contains(&Button::CROSS));
}

#[test]
fn a_plain_profile_ignores_the_scene() {
    let mut r = Rig::with("[advanced]\nenabled = false\n");
    r.e.apply_enabled(true);
    r.link.set_enabled(true);
    r.link.publish_scene(SceneContext::InVehicle, None, 0.9);
    assert!(r.press(&[Key::SPACE]).buttons.contains(&Button::CROSS));
}

#[test]
fn labeled_presses_become_actions_once_per_press() {
    let mut r = Rig::with(LAYERED);
    r.e.apply_enabled(true);
    r.link.set_enabled(true);
    r.hold(&[Key::F]);
    for _ in 0..20 {
        r.tick(); // held: one press
    }
    r.hold(&[]);
    r.tick();
    r.hold(&[Key::F]);
    r.tick();
    r.hold(&[]);
    r.tick();
    let got: Vec<Action> = r.link.drain_actions().into_iter().map(|(_, a)| a).collect();
    assert_eq!(got, [Action::EnterOrExitVehicle, Action::EnterOrExitVehicle]);
    // an unlabeled control says nothing
    r.press(&[Key::Z]);
    assert!(r.link.drain_actions().is_empty());
}

#[test]
fn actions_are_not_collected_for_a_plain_key_map() {
    let mut r = Rig::with(&LAYERED.replace("enabled = true", "enabled = false"));
    r.e.apply_enabled(true);
    r.press(&[Key::F]);
    assert!(!r.link.enabled());
    assert!(r.link.drain_actions().is_empty());
}

#[test]
fn advanced_mapping_is_the_one_switch_and_it_works_while_mapping_is_on() {
    let mut r = Rig::with("");
    r.e.apply_enabled(true);
    r.t += 1.0;
    r.tick();
    assert!(!r.link.enabled(), "a plain key map does not look at the picture");
    r.ui(serde_json::json!({"type": "advanced", "enabled": true}));
    assert!(r.saved().advanced.enabled);
    r.t += 1.0;
    r.tick();
    assert!(r.link.running(), "detection follows advanced mapping");
    assert!(r.last_overlay().scene.enabled);
    r.ui(serde_json::json!({"type": "advanced", "enabled": false}));
    r.t += 1.0;
    r.tick();
    assert!(!r.link.enabled());
    // there is no separate scene event any more
    assert!(UiEvent::from_json(&serde_json::json!({"type": "scene", "enabled": true})).is_none());
}

#[test]
fn the_advanced_switch_needs_a_visible_editor() {
    let mut r = Rig::with("");
    r.e.toggle_overlay(); // hidden
    r.ui(serde_json::json!({"type": "advanced", "enabled": true}));
    assert!(!r.saved().advanced.enabled);
}

#[test]
fn a_changed_scene_reaches_the_overlay() {
    let mut r = Rig::with(LAYERED);
    r.e.apply_enabled(true);
    r.link.set_enabled(true);
    r.link.publish_scene(SceneContext::InVehicle, None, 0.87);
    r.t += 1.0;
    r.tick();
    let s = r.last_overlay().scene;
    assert_eq!((s.context, s.confidence, s.layer), (Some("in_vehicle"), 87, "in_vehicle"));
}

#[test]
fn detection_waits_while_mapping_is_off_because_a_controller_is_driving() {
    let mut r = Rig::with(LAYERED);
    r.link.set_enabled(true);
    r.t += 1.0;
    r.tick();
    let s = r.last_overlay().scene;
    assert_eq!((s.enabled, s.state), (true, "idle"));
    assert!(!r.link.running());
    // keyboard and mouse take over: it starts
    r.e.apply_enabled(true);
    r.t += 1.0;
    r.tick();
    assert!(r.link.running());
    r.link.publish_scene(SceneContext::InVehicle, None, 0.9);
    assert!(r.press(&[Key::X]).buttons.contains(&Button::CROSS));
    // and the controller takes the console back: the scene is forgotten and the switch stays on
    r.e.apply_enabled(false);
    r.t += 1.0;
    r.tick();
    let s = r.last_overlay().scene;
    assert_eq!((s.enabled, s.state, s.context), (true, "idle", None));
}

#[test]
fn the_detector_is_told_which_actions_can_happen() {
    let mut r = Rig::with(LAYERED);
    r.t += 1.0;
    r.tick();
    assert!(!r.link.is_labeled(Action::PauseGame), "mapping is off: nothing is pressed through the key map");
    r.e.apply_enabled(true);
    r.t += 1.0;
    r.tick();
    for a in Action::ALL {
        assert!(r.link.is_labeled(a), "{a:?}");
    }
    r.ui(serde_json::json!({"type": "label", "control": "OPTIONS", "label": null})); // refused while mapping is on
    r.e.apply_enabled(false);
    r.ui(serde_json::json!({"type": "label", "control": "OPTIONS", "label": null}));
    r.e.apply_enabled(true);
    r.t += 1.0;
    r.tick();
    assert!(!r.link.is_labeled(Action::PauseGame));
    assert!(r.link.is_labeled(Action::ChangeCamera));
}

#[test]
fn malformed_scene_events_are_ignored() {
    assert!(UiEvent::from_json(&serde_json::json!({"type": "scene"})).is_none());
    assert!(UiEvent::from_json(&serde_json::json!({"type": "advanced", "enabled": "yes"})).is_none());
    assert!(UiEvent::from_json(&serde_json::json!({"type": "label"})).is_none());
    assert!(UiEvent::from_json(&serde_json::json!({"type": "label", "control": "CROSS", "label": 5})).is_none());
    let _ = MouseInput::MOUSE_LEFT;
}
