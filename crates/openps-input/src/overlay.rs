//! Everything the overlay shows, as plain serialisable data (port of overlay.build_state).
//! The Tauri front end renders it as HTML; Con2PC's pixel layout / hit-testing are not ported.

use crate::config::{can_carry_action, mouse_move_rows, Config};
use crate::keys::{chord_to_str, parse_chord, Key};
use crate::scene::{Action, Layer, SceneStatus};
use crate::state::{parse_control, Axis, Button, Gesture};
use serde::Serialize;

pub const SENS_MIN: f64 = 0.01;
pub const SENS_MAX: f64 = 0.30;
pub const SENS_DEFAULT: f64 = 0.05;
pub const MAX_PROFILES: usize = 12;

pub type Bounds = (f64, f64, f64, f64);

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Row {
    pub control: String,
    pub primary: String,
    pub secondary: String,
    /// The keys in each layer of an advanced profile ("-" for none; all "-" in a plain profile).
    pub on_foot: String,
    pub in_vehicle: String,
    pub in_menu: String,
    /// The action label this control carries (`key_enter_or_exit_vehicle`, ...).
    pub label: Option<String>,
    /// Buttons and the triggers can carry an action; sticks and touchpad gestures cannot.
    pub can_label: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ActionOption {
    pub name: &'static str,
    pub title: &'static str,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ProfileTab {
    pub name: String,
    pub slot: Option<usize>,
    pub active: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Capturing {
    pub control: String,
    pub slot: u8,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Renaming {
    pub name: String,
    pub text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct LayerFlags {
    pub on_foot: bool,
    pub in_vehicle: bool,
    pub in_menu: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Sensitivity {
    pub value: f64,
    pub min: f64,
    pub max: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct OverlayState {
    pub visible: bool,
    pub mapping: bool,
    pub profile: String,
    pub profiles: Vec<String>,
    pub profile_tabs: Vec<ProfileTab>,
    pub renaming: Option<Renaming>,
    pub sensitivity: Sensitivity,
    /// ("RT"|"RB" for the left click, "LT"|"LB" for the right click)
    pub click_modes: (&'static str, &'static str),
    pub opacity: f64,
    pub rows: Vec<Row>,
    /// The profile is in advanced mode: the keys come in three layers and controls can carry action labels.
    pub advanced: bool,
    /// Which layers the profile uses (a layer that is off is skipped by the scene detector and the key map).
    pub layers: LayerFlags,
    /// The action labels a control can carry.
    pub actions: Vec<ActionOption>,
    /// The scene detector, whose scene picks the layer.
    pub scene: SceneStatus,
    pub mouse: Vec<(String, String)>,
    pub hotkeys: Vec<(String, String)>,
    pub anchor: Option<Bounds>,
    pub capturing: Option<Capturing>,
    pub notice: String,
}

fn display(chords: &[String]) -> Vec<String> {
    chords.iter().map(|c| parse_chord(c).map(|p| chord_to_str(&p)).unwrap_or_else(|_| c.clone())).collect()
}

/// e.g. "Cmd+F1-F6" for the default Command+F1..F6 profile slots.
fn slots_label(cfg: &Config) -> String {
    let chords: Vec<_> = cfg.profile_slots.iter().filter_map(|c| parse_chord(c).ok()).collect();
    if chords.is_empty() {
        return "-".into();
    }
    let lc = crate::keys::Input::Key(Key::LEFT_COMMAND);
    let plain: Vec<Vec<_>> = chords
        .iter()
        .map(|c| {
            let mut v: Vec<_> = c.iter().filter(|i| **i != lc).collect();
            v.sort_by_key(|i| i.name());
            v
        })
        .collect();
    if chords.iter().zip(&plain).all(|(c, p)| c.contains(&lc) && p.len() == 1) {
        return format!("Cmd+{}-{}", plain[0][0].name(), plain[plain.len() - 1][0].name());
    }
    format!("{} profile keys", chords.len())
}

#[allow(clippy::too_many_arguments)]
pub fn build_state(
    cfg: &Config,
    profile: &str,
    mapping: bool,
    visible: bool,
    anchor: Option<Bounds>,
    capturing: Option<Capturing>,
    notice: String,
    renaming: Option<Renaming>,
    click_modes: (&'static str, &'static str),
    scene: SceneStatus,
) -> OverlayState {
    let bindings = cfg.profile_bindings(profile);
    let adv = cfg.advanced_of(profile).cloned().unwrap_or_default();
    let controls = Button::ALL
        .iter()
        .map(|b| b.name())
        .chain(Axis::ALL.iter().map(|a| a.name()))
        .chain(Gesture::ALL.iter().map(|g| g.name()));
    let rows = controls
        .map(|c| {
            let keys = display(bindings.get(c).map(Vec::as_slice).unwrap_or(&[]));
            let layer = |l: Layer| {
                display(adv.layer(l).get(c).map(Vec::as_slice).unwrap_or(&[]))
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "-".into())
            };
            Row {
                control: c.into(),
                primary: keys.first().cloned().unwrap_or_else(|| "-".into()),
                secondary: if keys.len() > 1 { keys[1..].join(", ") } else { "-".into() },
                on_foot: layer(Layer::OnFoot),
                in_vehicle: layer(Layer::InVehicle),
                in_menu: layer(Layer::InMenu),
                label: adv.label_of(c).map(|a| a.name().to_string()),
                can_label: parse_control(c).is_ok_and(can_carry_action),
            }
        })
        .collect();
    let app = if cfg.target_app.is_empty() { "app".to_string() } else { cfg.target_app.clone() };
    let slots = cfg.profile_slots.len();
    let names = cfg.profile_names();
    OverlayState {
        visible,
        mapping,
        profile: profile.into(),
        profiles: names.clone(),
        profile_tabs: names
            .iter()
            .enumerate()
            .map(|(i, n)| ProfileTab {
                name: n.clone(),
                slot: if i < slots { Some(i + 1) } else { None },
                active: n == profile,
            })
            .collect(),
        renaming,
        sensitivity: Sensitivity { value: cfg.mouse.sensitivity, min: SENS_MIN, max: SENS_MAX },
        click_modes,
        opacity: cfg.overlay_opacity,
        rows,
        advanced: adv.enabled,
        layers: LayerFlags {
            on_foot: adv.layer_enabled(Layer::OnFoot),
            in_vehicle: adv.layer_enabled(Layer::InVehicle),
            in_menu: adv.layer_enabled(Layer::InMenu),
        },
        actions: Action::ALL.iter().map(|a| ActionOption { name: a.name(), title: a.title() }).collect(),
        scene,
        mouse: mouse_move_rows(&cfg.mouse.look, cfg.mouse.invert_y),
        hotkeys: vec![
            (cfg.overlay_key.clone(), "overlay".into()),
            (cfg.toggle.clone(), "mapping on/off".into()),
            (cfg.profile_key.clone(), "next profile".into()),
            (slots_label(cfg), "pick profile".into()),
            (cfg.panic_key.clone(), format!("PANIC: kill {app}")),
            (cfg.quit.clone(), "quit".into()),
        ],
        anchor,
        capturing,
        notice,
    }
}

pub fn sens_to_fraction(v: f64) -> f64 {
    ((v - SENS_MIN) / (SENS_MAX - SENS_MIN)).clamp(0.0, 1.0)
}
pub fn fraction_to_sens(f: f64) -> f64 {
    ((SENS_MIN + f.clamp(0.0, 1.0) * (SENS_MAX - SENS_MIN)) * 1e4).round() / 1e4
}
