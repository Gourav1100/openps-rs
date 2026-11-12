//! Typed configuration (spec 04 section 4). Defaults are embedded; user files layer on top.

use crate::keys::{parse_chord, Chord, Input, Key, MouseInput};
use crate::scene::{Action, Layer};
use crate::state::{parse_control, Axis, Control};
use indexmap::IndexMap;
use serde::Deserialize;
use std::collections::BTreeMap;
use toml::{Table, Value};

pub const DEFAULT_TOML: &str = include_str!("../default.toml");

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    pub inputs: Chord,
    pub outputs: Vec<Control>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct MouseConfig {
    pub look: String,
    pub sensitivity: f64,
    pub smoothing: f64,
    pub deadzone: f64,
    pub invert_y: bool,
    pub capture_cursor: bool,
    pub hide_cursor: bool,
    pub move_threshold: f64,
    pub recenter_ms: f64,
}
impl Default for MouseConfig {
    fn default() -> Self {
        Self {
            look: "RIGHT_STICK".into(),
            sensitivity: 0.05,
            smoothing: 0.6,
            deadzone: 0.0,
            invert_y: false,
            capture_cursor: true,
            hide_cursor: true,
            move_threshold: 3.0,
            recenter_ms: 40.0,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct KeyboardStickConfig {
    pub left_ramp_ms: f64,
    pub right_ramp_ms: f64,
    pub release_ms: f64,
    pub left_release_ms: f64,
}
impl Default for KeyboardStickConfig {
    fn default() -> Self {
        Self { left_ramp_ms: 0.0, right_ramp_ms: 100.0, release_ms: 40.0, left_release_ms: 0.0 }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct OutputConfig {
    pub min_press_ms: f64,
}
impl Default for OutputConfig {
    fn default() -> Self {
        Self { min_press_ms: 35.0 }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct AdaptiveTriggerConfig {
    pub enabled: bool,
    pub sweep_ms: f64,
}
impl Default for AdaptiveTriggerConfig {
    fn default() -> Self {
        Self { enabled: true, sweep_ms: 1200.0 }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct TouchpadConfig {
    pub swipe_ms: f64,
    pub swipe_length: f64,
    pub tap_ms: f64,
}
impl Default for TouchpadConfig {
    fn default() -> Self {
        Self { swipe_ms: 150.0, swipe_length: 0.6, tap_ms: 60.0 }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum ComboOutput {
    One(String),
    Many(Vec<String>),
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Combo {
    pub input: String,
    pub output: ComboOutput,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Profile {
    pub description: String,
    pub bindings: BTreeMap<String, Vec<String>>,
    pub combos: Vec<Combo>,
    pub advanced: Advanced,
}

/// A profile's advanced mode: while `enabled`, the keys come from the layer the game is in (on foot, in a vehicle, in a menu) instead of
/// the primary and secondary keys, and PS5 controls can carry an action label for the scene detector. Each profile (the base too) has its own.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Advanced {
    pub enabled: bool,
    pub on_foot: BTreeMap<String, Vec<String>>,
    pub in_vehicle: BTreeMap<String, Vec<String>>,
    pub in_menu: BTreeMap<String, Vec<String>>,
    /// PS5 control -> action name (`key_enter_or_exit_vehicle`, ...). A control has at most one; an action can be on many controls.
    pub labels: BTreeMap<String, String>,
    /// Layers the game has no use for (`on_foot`, `in_vehicle`, `in_menu`): the key map never switches to them and the scene detector
    /// never reports their scenes. At least one layer stays on.
    pub disabled_layers: Vec<String>,
}

impl Advanced {
    pub fn layer(&self, layer: Layer) -> &BTreeMap<String, Vec<String>> {
        match layer {
            Layer::OnFoot => &self.on_foot,
            Layer::InVehicle => &self.in_vehicle,
            Layer::InMenu => &self.in_menu,
        }
    }

    pub fn layer_mut(&mut self, layer: Layer) -> &mut BTreeMap<String, Vec<String>> {
        match layer {
            Layer::OnFoot => &mut self.on_foot,
            Layer::InVehicle => &mut self.in_vehicle,
            Layer::InMenu => &mut self.in_menu,
        }
    }

    /// Nothing worth saving: switched off, no keys, no labels.
    pub fn is_empty(&self) -> bool {
        *self == Advanced::default()
    }

    /// The action a control carries.
    pub fn label_of(&self, control: &str) -> Option<Action> {
        self.labels.get(control).and_then(|n| Action::parse(n))
    }

    pub fn layer_enabled(&self, layer: Layer) -> bool {
        !self.disabled_layers.iter().any(|n| n == layer.name())
    }

    /// The layers in use, in the order on foot, in a vehicle, in a menu.
    pub fn enabled_layers(&self) -> Vec<Layer> {
        Layer::ALL.into_iter().filter(|l| self.layer_enabled(*l)).collect()
    }

    /// Turn a layer on or off. The last layer in use cannot be turned off (`false` then, nothing changes).
    pub fn set_layer_enabled(&mut self, layer: Layer, on: bool) -> bool {
        if !on && self.enabled_layers() == [layer] {
            return false;
        }
        self.disabled_layers.retain(|n| n != layer.name());
        if !on {
            self.disabled_layers.push(layer.name().into());
            self.disabled_layers.sort_by_key(|n| Layer::parse(n).map(|l| l as u8));
        }
        true
    }
}

/// Only buttons and the two triggers can carry an action: they are what a press is read from.
pub fn can_carry_action(control: Control) -> bool {
    matches!(control, Control::Button(_) | Control::Axis(Axis::L2) | Control::Axis(Axis::R2))
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    pub use_defaults: bool,
    pub poll_hz: u32,
    pub toggle: String,
    pub quit: String,
    pub profile_key: String,
    pub overlay_key: String,
    pub overlay: bool,
    pub panic_key: String,
    pub target_app: String,
    pub feedback_sounds: bool,
    pub close_target_on_exit: bool,
    pub start_enabled: bool,
    pub suppress_input: bool,
    pub profile_slots: Vec<String>,
    pub default_name: String,
    pub deleted_profiles: Vec<String>,
    /// Permanent ids of profiles by name (see `id_of`).
    pub profile_ids: BTreeMap<String, String>,
    pub bindings: BTreeMap<String, Vec<String>>,
    pub combos: Vec<Combo>,
    pub profiles: IndexMap<String, Profile>,
    /// The base profile's advanced mode (every other profile has its own in `profiles`).
    pub advanced: Advanced,
    pub mouse: MouseConfig,
    pub output: OutputConfig,
    pub adaptive_triggers: AdaptiveTriggerConfig,
    pub overlay_opacity: f64,
    pub keyboard_stick: KeyboardStickConfig,
    pub touchpad: TouchpadConfig,
    #[serde(skip)]
    pub notes: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            use_defaults: true,
            poll_hz: 1000,
            toggle: "F10".into(),
            quit: "F12".into(),
            profile_key: "F7".into(),
            overlay_key: "F8".into(),
            overlay: true,
            panic_key: "F11".into(),
            target_app: "OpenPS".into(),
            feedback_sounds: true,
            close_target_on_exit: true,
            start_enabled: false,
            suppress_input: true,
            profile_slots: (1..=6).map(|n| format!("LEFT_COMMAND+F{n}")).collect(),
            default_name: "default".into(),
            deleted_profiles: vec![],
            profile_ids: BTreeMap::new(),
            bindings: BTreeMap::new(),
            combos: vec![],
            profiles: IndexMap::new(),
            advanced: Default::default(),
            mouse: Default::default(),
            output: Default::default(),
            adaptive_triggers: Default::default(),
            overlay_opacity: 0.5,
            keyboard_stick: Default::default(),
            touchpad: Default::default(),
            notes: vec![],
        }
    }
}

/// Profile names are lowercase snake_case: letters a-z, digits and single underscores, 1 to 32 characters.
pub fn valid_profile_name(n: &str) -> bool {
    (1..=32).contains(&n.len())
        && n.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && !n.starts_with('_')
        && !n.ends_with('_')
        && !n.contains("__")
}

/// Whatever was typed, as a valid profile name: `AssassinsCreed` and `Assassin's Creed 2` become `assassins_creed` and `assassin_s_creed_2`.
pub fn snake_name(raw: &str) -> String {
    let mut out = String::new();
    let mut prev_lower = false;
    for ch in raw.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            if ch.is_ascii_uppercase() && prev_lower {
                out.push('_'); // a camelCase boundary
            }
            out.push(ch.to_ascii_lowercase());
            prev_lower = ch.is_ascii_lowercase() || ch.is_ascii_digit();
        } else if !out.ends_with('_') {
            out.push('_');
            prev_lower = false;
        }
    }
    let t = out.trim_matches('_');
    let mut t: String = t.chars().take(32).collect();
    while t.ends_with('_') {
        t.pop();
    }
    if t.is_empty() {
        "profile".into()
    } else {
        t
    }
}

/// An id for a profile that has none written: stable for a given name (so a plain file keeps working), `p_` and 8 hex digits.
pub fn derive_id(name: &str) -> String {
    let mut h: u32 = 0x811c_9dc5;
    for b in name.bytes() {
        h = (h ^ u32::from(b)).wrapping_mul(0x0100_0193);
    }
    format!("p_{h:08x}")
}

pub fn valid_profile_id(id: &str) -> bool {
    id.len() == 10 && id.starts_with("p_") && id[2..].chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

impl Config {
    /// The permanent id of a profile (derived from its name when none was written). The base profile has none: games without a choice use it.
    pub fn id_of(&self, name: &str) -> Option<String> {
        if name == self.default_name || !self.profiles.contains_key(name) {
            return None;
        }
        Some(self.profile_ids.get(name).cloned().unwrap_or_else(|| derive_id(name)))
    }

    /// The profile that has this id, if it still exists.
    pub fn name_of_id(&self, id: &str) -> Option<String> {
        self.profiles.keys().find(|n| self.id_of(n).as_deref() == Some(id)).cloned()
    }

    pub fn profile_names(&self) -> Vec<String> {
        std::iter::once(self.default_name.clone()).chain(self.profiles.keys().cloned()).collect()
    }

    pub fn profile_bindings(&self, name: &str) -> BTreeMap<String, Vec<String>> {
        let mut out = self.bindings.clone();
        if name != self.default_name {
            if let Some(p) = self.profiles.get(name) {
                out.extend(p.bindings.clone());
            }
        }
        out
    }

    pub fn toggle_chord(&self) -> Result<Chord, String> {
        parse_chord(&self.toggle)
    }
    pub fn quit_chord(&self) -> Result<Chord, String> {
        parse_chord(&self.quit)
    }
    pub fn profile_key_chord(&self) -> Result<Chord, String> {
        parse_chord(&self.profile_key)
    }
    pub fn panic_chord(&self) -> Result<Chord, String> {
        parse_chord(&self.panic_key)
    }
    pub fn overlay_chord(&self) -> Result<Chord, String> {
        parse_chord(&self.overlay_key)
    }

    /// (profile index, hotkey name, chord) per slot, RIGHT_COMMAND twins included.
    pub fn profile_slot_chords(&self) -> Result<Vec<(usize, String, Chord)>, String> {
        let mut out = vec![];
        for (idx, text) in self.profile_slots.iter().enumerate() {
            let chord = parse_chord(text)?;
            out.push((idx, format!("profile_slot_{}", idx + 1), chord.clone()));
            let lc = Input::Key(Key::LEFT_COMMAND);
            if chord.contains(&lc) {
                let mut twin = chord.clone();
                twin.remove(&lc);
                twin.insert(Input::Key(Key::RIGHT_COMMAND));
                out.push((idx, format!("profile_slot_{}_right", idx + 1), twin));
            }
        }
        Ok(out)
    }

    pub fn hotkey_chords(&self) -> Result<Vec<(String, Chord)>, String> {
        let mut v = vec![
            ("toggle".to_string(), self.toggle_chord()?),
            ("quit".into(), self.quit_chord()?),
            ("profile_key".into(), self.profile_key_chord()?),
            ("panic_key".into(), self.panic_chord()?),
            ("overlay_key".into(), self.overlay_chord()?),
        ];
        for (_, n, c) in self.profile_slot_chords()? {
            v.push((n, c));
        }
        Ok(v)
    }

    /// Inputs con2pc itself uses; none can be bound to a PS5 control.
    pub fn reserved_inputs(&self) -> Result<BTreeMap<Input, String>, String> {
        let mut out = BTreeMap::new();
        let why = "Command selects profiles and drives adaptive triggers".to_string();
        out.insert(Input::Key(Key::LEFT_COMMAND), why.clone());
        out.insert(Input::Key(Key::RIGHT_COMMAND), why);
        for (name, chord) in self.hotkey_chords()? {
            for i in chord {
                out.entry(i).or_insert_with(|| format!("part of the {} hotkey", name.replace('_', " ")));
            }
        }
        Ok(out)
    }

    /// A profile's advanced mode (the base profile's lives in the top-level `[advanced]`). `None` for a profile that does not exist.
    pub fn advanced_of(&self, profile: &str) -> Option<&Advanced> {
        if profile == self.default_name {
            Some(&self.advanced)
        } else {
            self.profiles.get(profile).map(|p| &p.advanced)
        }
    }

    /// The profile's bindings: the primary and secondary keys.
    pub fn resolve(&self, profile: Option<&str>) -> Result<Vec<Binding>, String> {
        self.resolve_map(profile, None)
    }

    /// The bindings to use while the game is in `layer`: the layer's keys when the profile is in advanced mode, otherwise the same as `resolve`.
    pub fn resolve_layer(&self, profile: Option<&str>, layer: Layer) -> Result<Vec<Binding>, String> {
        let name = profile.unwrap_or(&self.default_name);
        let advanced = self.advanced_of(name).is_some_and(|a| a.enabled);
        self.resolve_map(profile, advanced.then_some(layer))
    }

    /// Every set of bindings the profile can ever use (its plain keys and all three layers), for validation.
    fn all_bindings(&self, profile: &str) -> Result<Vec<Binding>, String> {
        let mut out = self.resolve_map(Some(profile), None)?;
        if self.advanced_of(profile).is_some_and(|a| !a.is_empty()) {
            for layer in Layer::ALL {
                out.extend(self.resolve_map(Some(profile), Some(layer))?);
            }
        }
        Ok(out)
    }

    fn resolve_map(&self, profile: Option<&str>, layer: Option<Layer>) -> Result<Vec<Binding>, String> {
        let profile = profile.unwrap_or(&self.default_name);
        if profile != self.default_name && !self.profiles.contains_key(profile) {
            return Err(format!("unknown profile {profile:?}"));
        }
        let mut out = vec![];
        let map = match (layer, self.advanced_of(profile)) {
            (Some(l), Some(a)) => a.layer(l).clone(),
            _ => self.profile_bindings(profile),
        };
        for (control, chords) in map {
            let ctl = parse_control(&control)?;
            for c in chords {
                out.push(Binding { inputs: parse_chord(&c)?, outputs: vec![ctl] });
            }
        }
        let mut combos = self.combos.clone();
        if profile != self.default_name {
            combos.extend(self.profiles[profile].combos.clone());
        }
        for combo in combos {
            let outs = match &combo.output {
                ComboOutput::One(s) => vec![s.clone()],
                ComboOutput::Many(v) => v.clone(),
            };
            out.push(Binding {
                inputs: parse_chord(&combo.input)?,
                outputs: outs.iter().map(|o| parse_control(o)).collect::<Result<_, _>>()?,
            });
        }
        Ok(out)
    }

    pub fn validate(&self) -> Result<(), String> {
        let range = |name: &str, v: f64, lo: f64, hi: f64, lo_incl: bool| -> Result<(), String> {
            let ok = v <= hi && if lo_incl { v >= lo } else { v > lo };
            if ok {
                Ok(())
            } else {
                Err(format!("{name} = {v} is out of range"))
            }
        };
        if !(30..=1000).contains(&self.poll_hz) {
            return Err("poll_hz must be 30..=1000".into());
        }
        if !["RIGHT_STICK", "LEFT_STICK", "NONE"].contains(&self.mouse.look.as_str()) {
            return Err(format!("mouse.look {:?} must be RIGHT_STICK, LEFT_STICK or NONE", self.mouse.look));
        }
        range("mouse.sensitivity", self.mouse.sensitivity, 0.0, f64::MAX, false)?;
        range("mouse.smoothing", self.mouse.smoothing, 0.0, 0.999_999, true)?;
        range("mouse.deadzone", self.mouse.deadzone, 0.0, 0.999_999, true)?;
        range("mouse.move_threshold", self.mouse.move_threshold, 0.0, f64::MAX, true)?;
        range("mouse.recenter_ms", self.mouse.recenter_ms, 0.0, f64::MAX, true)?;
        range("output.min_press_ms", self.output.min_press_ms, 0.0, 200.0, true)?;
        range("adaptive_triggers.sweep_ms", self.adaptive_triggers.sweep_ms, 0.0, f64::MAX, false)?;
        range("touchpad.swipe_ms", self.touchpad.swipe_ms, 0.0, f64::MAX, false)?;
        range("touchpad.tap_ms", self.touchpad.tap_ms, 0.0, f64::MAX, false)?;
        range("touchpad.swipe_length", self.touchpad.swipe_length, 0.0, 1.0, false)?;
        range("overlay_opacity", self.overlay_opacity, 0.1, 1.0, true)?;
        for (n, v) in [
            ("left_ramp_ms", self.keyboard_stick.left_ramp_ms),
            ("right_ramp_ms", self.keyboard_stick.right_ramp_ms),
            ("release_ms", self.keyboard_stick.release_ms),
            ("left_release_ms", self.keyboard_stick.left_release_ms),
        ] {
            range(&format!("keyboard_stick.{n}"), v, 0.0, f64::MAX, true)?;
        }
        if self.profile_slots.len() > 12 {
            return Err("profile_slots has at most 12 entries".into());
        }
        if !valid_profile_name(&self.default_name) {
            return Err(format!(
                "bad default_name {:?}: use 1-32 lowercase letters, digits and '_'",
                self.default_name
            ));
        }
        if self.profiles.contains_key(&self.default_name) {
            return Err(format!("{:?} is the base [bindings]; name the profile otherwise", self.default_name));
        }
        for name in self.profiles.keys() {
            if !valid_profile_name(name) {
                return Err(format!("bad profile name {name:?}: use 1-32 lowercase letters, digits and '_'"));
            }
        }
        let mut seen = BTreeMap::new();
        for name in self.profiles.keys() {
            let id = self.id_of(name).unwrap_or_default();
            if !valid_profile_id(&id) {
                return Err(format!("bad id {id:?} for profile {name:?}"));
            }
            if let Some(other) = seen.insert(id.clone(), name.clone()) {
                return Err(format!("profiles {other:?} and {name:?} have the same id {id}"));
            }
        }
        for c in &self.profile_slots {
            parse_chord(c)?;
        }
        for name in self.profile_names() {
            self.all_bindings(&name)?;
            let advanced = self.advanced_of(&name).cloned().unwrap_or_default();
            for (control, action) in &advanced.labels {
                let ctl = parse_control(control)?;
                if !can_carry_action(ctl) {
                    return Err(format!(
                        "profile {name:?}: {control} cannot carry an action (only buttons and the triggers can)"
                    ));
                }
                if Action::parse(action).is_none() {
                    return Err(format!("profile {name:?}: unknown action {action:?} on {control}"));
                }
            }
            if let Some(bad) = advanced.disabled_layers.iter().find(|n| Layer::parse(n).is_none()) {
                return Err(format!(
                    "profile {name:?}: unknown layer {bad:?} in disabled_layers (on_foot, in_vehicle or in_menu)"
                ));
            }
            if advanced.enabled_layers().is_empty() {
                return Err(format!("profile {name:?}: disabled_layers cannot switch off every layer"));
            }
        }
        let keys = self.hotkey_chords()?;
        for (i, (a, ca)) in keys.iter().enumerate() {
            for (b, cb) in &keys[i + 1..] {
                if ca.is_subset(cb) || cb.is_subset(ca) {
                    return Err(format!("hotkeys {a} and {b} overlap"));
                }
            }
        }
        for (name, chord) in &keys {
            if let Some(i) = chord.iter().find(|i| matches!(i, Input::Mouse(m) if m.name().starts_with("SCROLL_") || m.name().starts_with("MOUSE_MOVE_"))) {
                return Err(format!(
                    "{} cannot be used as the {} hotkey: scrolling and mouse movement cannot trigger a hotkey (use a key or a mouse button)",
                    i.name(),
                    name.replace('_', " ")
                ));
            }
        }
        let reserved = self.reserved_inputs()?;
        let clicks = [Input::Mouse(MouseInput::MOUSE_LEFT), Input::Mouse(MouseInput::MOUSE_RIGHT)];
        for name in self.profile_names() {
            for b in self.all_bindings(&name)? {
                let outs = b.outputs.iter().map(|c| c.name()).collect::<Vec<_>>().join(", ");
                if b.inputs.contains(&Input::Key(Key::TAB)) && clicks.iter().any(|c| b.inputs.contains(c)) {
                    return Err(format!("Tab + left/right click toggles RT/RB and LT/LB, so a binding cannot use that combination (binding for {outs} in profile {name:?})"));
                }
                for i in &b.inputs {
                    if let Some(why) = reserved.get(i) {
                        return Err(format!("{} is reserved by con2pc ({why}) and cannot be bound (binding for {outs} in profile {name:?})", i.name()));
                    }
                }
            }
        }
        Ok(())
    }
}

// ---- loading ----------------------------------------------------------------------------

fn normalise_bindings(raw: &Value, where_: &str) -> Result<Value, String> {
    let t = raw.as_table().ok_or_else(|| format!("[{where_}] must be a table of CONTROL = [\"KEY\", ...]"))?;
    let mut out = Table::new();
    for (k, v) in t {
        let chords: Vec<Value> = match v {
            Value::String(_) => vec![v.clone()],
            Value::Array(a) if a.iter().all(|x| x.is_str()) => a.clone(),
            _ => return Err(format!("{where_}.{k} must be a string or a list of strings")),
        };
        out.insert(parse_control(k)?.name().to_string(), Value::Array(chords));
    }
    Ok(Value::Table(out))
}

/// Rename the keys of a table to snake_case profile names; a second name that collides gets `_2`, `_3`, ... Returns old -> new.
fn snake_keys(t: &mut Table) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    let mut out = Table::new();
    for (k, v) in std::mem::take(t) {
        if k.trim().is_empty() {
            out.insert(k, v); // left as it is: validation rejects it
            continue;
        }
        let mut n = snake_name(&k);
        let mut i = 2;
        while out.contains_key(&n) {
            let suffix = format!("_{i}");
            n = format!("{}{suffix}", snake_name(&k).chars().take(32 - suffix.len()).collect::<String>());
            i += 1;
        }
        map.insert(k, n.clone());
        out.insert(n, v);
    }
    *t = out;
    map
}

/// `[advanced]`: each layer table like a bindings table, and `labels` as CONTROL = "key_..." with canonical control names.
fn normalise_advanced(raw: &Value, where_: &str) -> Result<Value, String> {
    let t = raw.as_table().ok_or_else(|| format!("[{where_}] must be a table"))?;
    let mut out = t.clone();
    for layer in Layer::ALL {
        if let Some(b) = t.get(layer.name()) {
            out.insert(layer.name().into(), normalise_bindings(b, &format!("{where_}.{}", layer.name()))?);
        }
    }
    if let Some(l) = t.get("labels") {
        let lt = l.as_table().ok_or_else(|| format!("[{where_}.labels] must be a table of CONTROL = \"key_...\""))?;
        let mut labels = Table::new();
        for (k, v) in lt {
            let name = v.as_str().ok_or_else(|| format!("{where_}.labels.{k} must be a string"))?;
            labels.insert(parse_control(k)?.name().to_string(), Value::String(name.to_string()));
        }
        out.insert("labels".into(), Value::Table(labels));
    }
    Ok(Value::Table(out))
}

fn normalise(mut data: Table) -> Result<Table, String> {
    // profile names are lowercase snake_case: older files with other spellings are read as the converted names
    let mut renamed = BTreeMap::new();
    if let Some(Value::Table(p)) = data.get_mut("profiles") {
        renamed = snake_keys(p);
    }
    if let Some(Value::Table(ids)) = data.get_mut("profile_ids") {
        for (k, v) in std::mem::take(ids) {
            ids.insert(renamed.get(&k).cloned().unwrap_or_else(|| snake_name(&k)), v);
        }
    }
    if let Some(Value::Array(a)) = data.get_mut("deleted_profiles") {
        for x in a.iter_mut() {
            if let Some(s) = x.as_str() {
                *x = Value::String(snake_name(s));
            }
        }
    }
    if let Some(Value::String(s)) = data.get_mut("default_name") {
        if !s.trim().is_empty() {
            *s = snake_name(s);
        }
    }
    if let Some(b) = data.get("bindings") {
        let n = normalise_bindings(b, "bindings")?;
        data.insert("bindings".into(), n);
    }
    if let Some(a) = data.get("advanced") {
        let n = normalise_advanced(a, "advanced")?;
        data.insert("advanced".into(), n);
    }
    if let Some(p) = data.get_mut("profiles") {
        let profs = p.as_table_mut().ok_or("[profiles] must be a table of [profiles.<name>] sections")?;
        let names: Vec<String> = profs.keys().cloned().collect();
        for name in names {
            let prof =
                profs.get_mut(&name).unwrap().as_table_mut().ok_or(format!("[profiles.{name}] must be a table"))?;
            if let Some(b) = prof.get("bindings") {
                let n = normalise_bindings(b, &format!("profiles.{name}.bindings"))?;
                prof.insert("bindings".into(), n);
            }
            if let Some(a) = prof.get("advanced") {
                let n = normalise_advanced(a, &format!("profiles.{name}.advanced"))?;
                prof.insert("advanced".into(), n);
            }
        }
    }
    Ok(data)
}

/// Recursive merge; arrays (a control's chords) are replaced, not appended.
fn merge(base: &Table, over: &Table) -> Table {
    let mut out = base.clone();
    for (k, v) in over {
        let merged = match (out.get(k), v) {
            (Some(Value::Table(b)), Value::Table(o)) => Value::Table(merge(b, o)),
            _ => v.clone(),
        };
        out.insert(k.clone(), merged);
    }
    out
}

/// Deepest `[`/`{` nesting outside strings and comments (cheap lexical scan).
fn bracket_depth(text: &str) -> usize {
    let (mut depth, mut max) = (0usize, 0usize);
    let (mut quote, mut escaped, mut comment) = (None::<char>, false, false);
    for c in text.chars() {
        if comment {
            comment = c != '\n';
            continue;
        }
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if c == '\\' && q == '"' {
                escaped = true;
            } else if c == q || c == '\n' {
                quote = None;
            }
            continue;
        }
        match c {
            '#' => comment = true,
            '"' | '\'' => quote = Some(c),
            '[' | '{' => {
                depth += 1;
                max = max.max(depth);
            }
            ']' | '}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    max
}

/// Deeper nesting than any real config needs is refused up front: the TOML parser recurses and a few thousand
/// brackets overflow a small thread stack (an uncatchable abort in unoptimised builds).
const MAX_NESTING: usize = 64;

pub(crate) fn parse_table(text: &str, name: &str) -> Result<Table, String> {
    if bracket_depth(text) > MAX_NESTING {
        return Err(format!("{name}: brackets are nested more than {MAX_NESTING} levels deep"));
    }
    text.parse::<Table>().map_err(|e| format!("{name}: {e}"))
}

pub fn load_defaults() -> Result<Config, String> {
    load_layers(None, false)
}

/// Defaults, then the user layer text on top. `migrate` drops bindings now reserved (implicit user file).
/// Older versions closed the app with F9 (which macOS's Metal HUD also uses) and cycled profiles with F11. The app is now closed with
/// F11 and profiles cycle with F7. A user file that still has the old pair is read as the new one (only for the implicit user file).
pub fn migrate_hotkeys(text: &str) -> String {
    let code_of = |l: &str| l.split('#').next().unwrap_or("").replace(' ', "");
    let line = |k: &str, v: &str| text.lines().any(|l| code_of(l) == format!("{k}=\"{v}\""));
    // A target key that another line of the file already uses is never taken: the migration must not create an overlap.
    let uses = |key: &str, except: &[&str]| {
        text.lines().any(|l| {
            let c = code_of(l);
            c.ends_with(&format!("=\"{key}\"")) && !except.iter().any(|e| c.starts_with(e))
        })
    };
    let profile_to_f7 = line("profile_key", "F11")
        && (!text.contains("panic_key") || line("panic_key", "F9"))
        && !uses("F7", &["profile_key="]);
    let except: &[&str] = if profile_to_f7 { &["panic_key=", "profile_key="] } else { &["panic_key="] };
    let panic_to_f11 = line("panic_key", "F9") && !uses("F11", except);
    if panic_to_f11 || profile_to_f7 {
        text.lines()
            .map(|l| {
                let code = code_of(l);
                if panic_to_f11 && code == "panic_key=\"F9\"" {
                    "panic_key = \"F11\"".to_string()
                } else if profile_to_f7 && code == "profile_key=\"F11\"" {
                    "profile_key = \"F7\"".to_string()
                } else {
                    l.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        text.to_string()
    }
}

/// The text the implicit user file is really loaded from: the migrated one, unless the migration would make a
/// file that loads today fail (then the old text is kept, so the app still starts).
pub fn migrated_text(text: &str) -> String {
    let m = migrate_hotkeys(text);
    let loads = |t: &str| load_layers_inner(Some(t), true).is_ok();
    if loads(&m) {
        return m;
    }
    if m != text && loads(text) {
        return text.to_string(); // the old file is fine as it is
    }
    // The old profile key (F11) is now the quit key's neighbour: move it to the first free F-key instead of failing.
    let old_profile = |l: &str| l.split('#').next().unwrap_or("").replace(' ', "") == "profile_key=\"F11\"";
    if m.lines().any(old_profile) {
        for n in [7, 8, 10, 9, 6, 5, 4, 3, 2, 1] {
            let cand = m
                .lines()
                .map(|l| if old_profile(l) { format!("profile_key = \"F{n}\"") } else { l.to_string() })
                .collect::<Vec<_>>()
                .join("\n");
            if loads(&cand) {
                return cand;
            }
        }
    }
    m
}

pub fn load_layers(user_text: Option<&str>, migrate: bool) -> Result<Config, String> {
    let migrated = user_text.filter(|_| migrate).map(migrated_text);
    load_layers_inner(migrated.as_deref().or(user_text), migrate)
}

fn load_layers_inner(user_text: Option<&str>, migrate: bool) -> Result<Config, String> {
    let user = match user_text {
        Some(t) => normalise(parse_table(t, "user config")?)?,
        None => Table::new(),
    };
    let shipped = normalise(parse_table(DEFAULT_TOML, "default.toml")?)?;
    let use_defaults = user.get("use_defaults").and_then(Value::as_bool).unwrap_or(true);
    let mut data = if use_defaults { shipped.clone() } else { Table::new() };
    let deleted: Vec<String> = match user.get("deleted_profiles") {
        None => vec![],
        Some(Value::Array(a)) if a.iter().all(|x| x.is_str()) => {
            a.iter().map(|x| x.as_str().unwrap().to_string()).collect()
        }
        _ => return Err("deleted_profiles must be a list of profile names".into()),
    };
    for gone in &deleted {
        if let Some(Value::Table(p)) = data.get_mut("profiles") {
            p.remove(gone);
        }
    }
    for (k, v) in &user {
        if k == "combos" {
            let mut c = data.get("combos").and_then(Value::as_array).cloned().unwrap_or_default();
            c.extend(v.as_array().cloned().ok_or("combos must be an array of tables")?);
            data.insert(k.clone(), Value::Array(c));
        } else if let (Value::Table(vt), Some(Value::Table(dt))) = (v, data.get(k)) {
            let m = merge(dt, vt);
            data.insert(k.clone(), Value::Table(m));
        } else {
            data.insert(k.clone(), v.clone());
        }
    }
    let notes = if migrate { migrate_table(&mut data, &shipped)? } else { vec![] };
    let mut cfg: Config = Value::Table(data).try_into().map_err(|e| format!("config: {e}"))?;
    cfg.validate()?;
    cfg.notes = notes;
    Ok(cfg)
}

fn migrate_table(data: &mut Table, shipped: &Table) -> Result<Vec<String>, String> {
    let mut notes = vec![];
    let mut probe = data.clone();
    for k in ["bindings", "profiles", "combos"] {
        probe.remove(k);
    }
    let probe_cfg: Config = Value::Table(probe).try_into().map_err(|e| format!("config: {e}"))?;
    let reserved = probe_cfg.reserved_inputs()?;
    let bad = |chord: &str| -> Option<Input> {
        parse_chord(chord).ok().and_then(|c| c.into_iter().find(|i| reserved.contains_key(i)))
    };
    let empty = Table::new();
    let base_shipped = shipped.get("bindings").and_then(Value::as_table).unwrap_or(&empty).clone();

    let clean = |table: &mut Table, fallback: &Table, prefix: &str, notes: &mut Vec<String>| {
        let controls: Vec<String> = table.keys().cloned().collect();
        for control in controls {
            let chords: Vec<String> = table[&control]
                .as_array()
                .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                .unwrap_or_default();
            let mut keep: Vec<String> = vec![];
            for c in &chords {
                match bad(c) {
                    None => keep.push(c.clone()),
                    Some(b) => notes.push(format!(
                        "{prefix}{control}: {c} dropped ({} is reserved: {})",
                        b.name(),
                        reserved[&b]
                    )),
                }
            }
            if !chords.is_empty() && keep.is_empty() {
                let fb: Vec<String> = fallback
                    .get(&control)
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter().filter_map(|x| x.as_str().map(String::from)).filter(|c| bad(c).is_none()).collect()
                    })
                    .unwrap_or_default();
                if !fb.is_empty() {
                    if let Some(n) = notes.last_mut() {
                        n.push_str(&format!("; using {}", fb.join(", ")));
                    }
                    keep = fb;
                }
            }
            table.insert(control, Value::Array(keep.into_iter().map(Value::String).collect()));
        }
    };
    let filter_combos = |v: &Value| -> Vec<Value> {
        v.as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|c| c.get("input").and_then(Value::as_str).map(|i| bad(i).is_none()).unwrap_or(true))
            .collect()
    };
    if let Some(Value::Table(b)) = data.get_mut("bindings") {
        clean(b, &base_shipped, "", &mut notes);
    }
    if let Some(Value::Table(profs)) = data.get_mut("profiles") {
        for (name, prof) in profs.iter_mut() {
            let fb = shipped
                .get("profiles")
                .and_then(|p| p.get(name))
                .and_then(|p| p.get("bindings"))
                .and_then(Value::as_table)
                .cloned()
                .unwrap_or_else(|| base_shipped.clone());
            let prof = prof.as_table_mut().ok_or("profile must be a table")?;
            if let Some(Value::Table(b)) = prof.get_mut("bindings") {
                clean(b, &fb, &format!("[{name}] "), &mut notes);
            }
            if let Some(c) = prof.get("combos") {
                let kept = filter_combos(c);
                if kept.len() != c.as_array().map(Vec::len).unwrap_or(0) {
                    notes.push(format!("[{name}] a combo using a reserved key was dropped"));
                    prof.insert("combos".into(), Value::Array(kept));
                }
            }
        }
    }
    if let Some(c) = data.get("combos") {
        let kept = filter_combos(c);
        if kept.len() != c.as_array().map(Vec::len).unwrap_or(0) {
            notes.push("a combo using a reserved key was dropped".into());
            data.insert("combos".into(), Value::Array(kept));
        }
    }
    Ok(notes)
}

/// Load a TOML file on top of the defaults. An explicit file is validated strictly (`migrate = false`);
/// the implicit user file is migrated so it can never stop the app from starting.
pub fn load_path(path: &std::path::Path, migrate: bool) -> Result<Config, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    load_layers(Some(&text), migrate).map_err(|e| format!("{}: {e}", path.display()))
}

/// The fixed mouse-movement -> stick mapping, as (input, target) text rows.
pub fn mouse_move_rows(look: &str, invert_y: bool) -> Vec<(String, String)> {
    let names = ["MOUSE_MOVE_UP", "MOUSE_MOVE_DOWN", "MOUSE_MOVE_LEFT", "MOUSE_MOVE_RIGHT"];
    if look == "NONE" {
        return names.iter().map(|n| (n.to_string(), "(disabled: mouse.look = NONE)".to_string())).collect();
    }
    let side = look.trim_end_matches("_STICK");
    let (up, down) = if invert_y { ("DOWN", "UP") } else { ("UP", "DOWN") };
    names.iter().zip([up, down, "LEFT", "RIGHT"]).map(|(n, t)| (n.to_string(), format!("{side}_STICK_{t}"))).collect()
}
