//! Typed configuration (spec 04 section 4). Defaults are embedded; user files layer on top.

use crate::keys::{parse_chord, Chord, Input, Key, MouseInput};
use crate::state::{parse_control, Control};
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
    pub bindings: BTreeMap<String, Vec<String>>,
    pub combos: Vec<Combo>,
    pub profiles: IndexMap<String, Profile>,
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
            poll_hz: 500,
            toggle: "F10".into(),
            quit: "F12".into(),
            profile_key: "F11".into(),
            overlay_key: "F8".into(),
            overlay: true,
            panic_key: "F9".into(),
            target_app: "openPS".into(),
            feedback_sounds: true,
            close_target_on_exit: true,
            start_enabled: false,
            suppress_input: true,
            profile_slots: (1..=6).map(|n| format!("LEFT_COMMAND+F{n}")).collect(),
            default_name: "default".into(),
            deleted_profiles: vec![],
            bindings: BTreeMap::new(),
            combos: vec![],
            profiles: IndexMap::new(),
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

pub fn valid_profile_name(n: &str) -> bool {
    (1..=32).contains(&n.len()) && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

impl Config {
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

    pub fn resolve(&self, profile: Option<&str>) -> Result<Vec<Binding>, String> {
        let profile = profile.unwrap_or(&self.default_name);
        if profile != self.default_name && !self.profiles.contains_key(profile) {
            return Err(format!("unknown profile {profile:?}"));
        }
        let mut out = vec![];
        for (control, chords) in self.profile_bindings(profile) {
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
            return Err(format!("bad default_name {:?}: use 1-32 letters, digits, '_' or '-'", self.default_name));
        }
        if self.profiles.contains_key(&self.default_name) {
            return Err(format!("{:?} is the base [bindings]; name the profile otherwise", self.default_name));
        }
        for name in self.profiles.keys() {
            if !valid_profile_name(name) {
                return Err(format!("bad profile name {name:?}: use 1-32 letters, digits, '_' or '-'"));
            }
        }
        for c in &self.profile_slots {
            parse_chord(c)?;
        }
        for name in self.profile_names() {
            self.resolve(Some(&name))?;
        }
        let keys = self.hotkey_chords()?;
        for (i, (a, ca)) in keys.iter().enumerate() {
            for (b, cb) in &keys[i + 1..] {
                if ca.is_subset(cb) || cb.is_subset(ca) {
                    return Err(format!("hotkeys {a} and {b} overlap"));
                }
            }
        }
        let reserved = self.reserved_inputs()?;
        let clicks = [Input::Mouse(MouseInput::MOUSE_LEFT), Input::Mouse(MouseInput::MOUSE_RIGHT)];
        for name in self.profile_names() {
            for b in self.resolve(Some(&name))? {
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

fn normalise(mut data: Table) -> Result<Table, String> {
    if let Some(b) = data.get("bindings") {
        let n = normalise_bindings(b, "bindings")?;
        data.insert("bindings".into(), n);
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

fn parse_table(text: &str, name: &str) -> Result<Table, String> {
    text.parse::<Table>().map_err(|e| format!("{name}: {e}"))
}

pub fn load_defaults() -> Result<Config, String> {
    load_layers(None, false)
}

/// Defaults, then the user layer text on top. `migrate` drops bindings now reserved (implicit user file).
pub fn load_layers(user_text: Option<&str>, migrate: bool) -> Result<Config, String> {
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
