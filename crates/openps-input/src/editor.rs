//! Pure config editor (spec 04 section 13; port of customise.Editor) and the TOML writer.
//! No capture UI here: the overlay and the Tauri front end drive these methods.

use crate::config::{
    can_carry_action, load_defaults, load_layers, load_path, mouse_move_rows, valid_profile_name, Advanced, Combo,
    ComboOutput, Config,
};
use crate::keys::{chord_to_str, parse_chord, Chord};
use crate::scene::{Action, Layer};
use crate::state::{Axis, Button, Control, Gesture};
use indexmap::IndexMap;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use toml::{Table, Value};

pub const PRIMARY: u8 = 1;
pub const SECONDARY: u8 = 2;
const DEFAULT_PROFILE: &str = "default";

pub fn all_controls() -> Vec<Control> {
    Button::ALL
        .iter()
        .map(|b| Control::Button(*b))
        .chain(Axis::ALL.iter().map(|a| Control::Axis(*a)))
        .chain(Gesture::ALL.iter().map(|g| Control::Gesture(*g)))
        .collect()
}

type Table2 = IndexMap<String, Vec<String>>;

/// Everything editable, for rollback of a change that does not validate.
#[derive(Clone)]
pub struct EditorState {
    profiles: IndexMap<String, Table2>,
    default_name: String,
    deleted: BTreeSet<String>,
    current: String,
    toggle: String,
    quit: String,
    profile_key: String,
    combos: Vec<Value>,
    raw: Table,
    profile_combos: IndexMap<String, Vec<Value>>,
    /// Permanent ids by profile name, for every profile but the base (see `Config::id_of`).
    ids: BTreeMap<String, String>,
    /// The advanced mode of every profile, the base one included.
    advanced: IndexMap<String, Advanced>,
}

pub struct Editor {
    pub path: PathBuf,
    defaults: Config,
    pub notes: Vec<String>,
    pub mouse_look: String,
    pub invert_y: bool,
    s: EditorState,
}

fn strs(v: &[String]) -> Value {
    Value::Array(v.iter().cloned().map(Value::String).collect())
}

fn combo_value(c: &Combo) -> Value {
    let mut t = Table::new();
    t.insert("input".into(), Value::String(c.input.clone()));
    t.insert(
        "output".into(),
        match &c.output {
            ComboOutput::One(s) => Value::String(s.clone()),
            ComboOutput::Many(v) => strs(v),
        },
    );
    Value::Table(t)
}

/// Serialise a table to TOML text (scalars, lists, nested tables, arrays of tables).
pub fn dump_toml(data: &Table) -> String {
    toml::to_string(data).expect("serialisable table")
}

fn advanced_value(a: &Advanced) -> Value {
    let mut t = Table::new();
    t.insert("enabled".into(), Value::Boolean(a.enabled));
    for layer in Layer::ALL {
        let keys: Table = a
            .layer(layer)
            .iter()
            .filter(|(_, chords)| !chords.is_empty())
            .map(|(c, chords)| (c.clone(), strs(chords)))
            .collect();
        if !keys.is_empty() {
            t.insert(layer.name().into(), Value::Table(keys));
        }
    }
    if !a.labels.is_empty() {
        let labels: Table = a.labels.iter().map(|(c, n)| (c.clone(), Value::String(n.clone()))).collect();
        t.insert("labels".into(), Value::Table(labels));
    }
    if !a.disabled_layers.is_empty() {
        t.insert("disabled_layers".into(), strs(&a.disabled_layers));
    }
    Value::Table(t)
}

impl Editor {
    pub fn new(path: impl Into<PathBuf>) -> Result<Self, String> {
        let path = path.into();
        let raw: Table = if path.exists() {
            // The same (migrated) text the config was loaded from, so a saved file is fully migrated.
            let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
            crate::config::parse_table(&crate::config::migrated_text(&text), &path.display().to_string())?
        } else {
            Table::new()
        };
        // A file that does not exist yet starts from the shipped defaults only.
        let cfg = if path.exists() { load_path(&path, true)? } else { load_defaults()? };
        let defaults = load_defaults()?;
        let mut profiles = IndexMap::new();
        for name in cfg.profile_names() {
            let mut full: Table2 = all_controls().iter().map(|c| (c.name().to_string(), vec![])).collect();
            for (k, v) in cfg.profile_bindings(&name) {
                full.insert(k, v);
            }
            profiles.insert(name, full);
        }
        let profile_combos = cfg
            .profiles
            .iter()
            .filter(|(_, p)| !p.combos.is_empty())
            .map(|(n, p)| (n.clone(), p.combos.iter().map(combo_value).collect()))
            .collect();
        Ok(Self {
            path,
            notes: cfg.notes.clone(),
            mouse_look: cfg.mouse.look.clone(),
            invert_y: cfg.mouse.invert_y,
            defaults,
            s: EditorState {
                profiles,
                default_name: cfg.default_name.clone(),
                deleted: cfg.deleted_profiles.iter().cloned().collect(),
                current: cfg.default_name.clone(),
                toggle: cfg.toggle.clone(),
                quit: cfg.quit.clone(),
                profile_key: cfg.profile_key.clone(),
                combos: cfg.combos.iter().map(combo_value).collect(),
                raw,
                profile_combos,
                ids: cfg.profiles.keys().filter_map(|n| cfg.id_of(n).map(|i| (n.clone(), i))).collect(),
                advanced: cfg
                    .profile_names()
                    .into_iter()
                    .map(|n| {
                        let a = cfg.advanced_of(&n).cloned().unwrap_or_default();
                        (n, a)
                    })
                    .collect(),
            },
        })
    }

    // ---- views ---------------------------------------------------------------------------
    pub fn current(&self) -> &str {
        &self.s.current
    }
    /// Select the profile to edit. Unknown names are ignored (returns false) so the accessors never see a missing profile.
    pub fn set_current(&mut self, name: &str) -> bool {
        let known = self.s.profiles.contains_key(name);
        if known {
            self.s.current = name.to_string();
        }
        known
    }
    pub fn default_name(&self) -> &str {
        &self.s.default_name
    }
    pub fn deleted(&self) -> &BTreeSet<String> {
        &self.s.deleted
    }
    pub fn profile_names(&self) -> Vec<String> {
        self.s.profiles.keys().cloned().collect()
    }
    pub fn profile(&self, name: &str) -> Option<&Table2> {
        self.s.profiles.get(name)
    }
    pub fn profile_mut(&mut self, name: &str) -> Option<&mut Table2> {
        self.s.profiles.get_mut(name)
    }
    pub fn bindings(&self) -> &Table2 {
        &self.s.profiles[&self.s.current]
    }
    pub fn set_toggle(&mut self, t: &str) {
        self.s.toggle = t.into();
    }
    pub fn set_quit(&mut self, t: &str) {
        self.s.quit = t.into();
    }
    pub fn set_profile_key(&mut self, t: &str) {
        self.s.profile_key = t.into();
    }
    pub fn toggle(&self) -> &str {
        &self.s.toggle
    }
    pub fn combos(&self) -> &[Value] {
        &self.s.combos
    }

    /// Text of the primary (1) or secondary (2) column; extras fold into secondary.
    pub fn slot(&self, control: Control, which: u8) -> String {
        let items = &self.bindings()[control.name()];
        let part: Vec<&String> =
            if which == PRIMARY { items.iter().take(1).collect() } else { items.iter().skip(1).collect() };
        if part.is_empty() {
            "-".into()
        } else {
            part.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
        }
    }

    pub fn mouse_move_rows(&self) -> Vec<(String, String)> {
        mouse_move_rows(&self.mouse_look, self.invert_y)
    }

    // ---- editing (all on the current profile) --------------------------------------------------
    pub fn owners(&self, chord: &Chord, skip: Option<&str>) -> Vec<String> {
        let text = chord_to_str(chord);
        self.bindings()
            .iter()
            .filter(|(c, chords)| {
                Some(c.as_str()) != skip
                    && chords.iter().any(|x| parse_chord(x).map(|p| chord_to_str(&p) == text).unwrap_or(false))
            })
            .map(|(c, _)| c.clone())
            .collect()
    }

    /// Set a control's chords in the current profile. Editing the BASE also updates every profile that
    /// still inherits that control (its value equals the old base value).
    fn assign(&mut self, control: Control, items: Vec<String>) {
        let key = control.name().to_string();
        let cur = self.s.current.clone();
        let before = self.s.profiles[&cur][&key].clone();
        self.s.profiles.get_mut(&cur).unwrap().insert(key.clone(), items.clone());
        if cur == self.s.default_name && before != items {
            for (name, table) in self.s.profiles.iter_mut() {
                if *name != self.s.default_name && table[&key] == before {
                    table.insert(key.clone(), items.clone());
                }
            }
        }
    }

    /// Primary replaces the first chord; secondary replaces all others.
    pub fn set_slot(&mut self, control: Control, which: u8, chord: &Chord) {
        let mut items = self.bindings()[control.name()].clone();
        let text = chord_to_str(chord);
        if which == PRIMARY || items.is_empty() {
            if items.is_empty() {
                items.push(text);
            } else {
                items[0] = text;
            }
        } else {
            let first = items[0].clone();
            items.truncate(1);
            if text != first {
                items.push(text);
            }
        }
        self.assign(control, items);
    }

    /// Delete the primary (the secondary is promoted) or the secondary.
    pub fn clear_slot(&mut self, control: Control, which: u8) {
        let mut items = self.bindings()[control.name()].clone();
        if which == PRIMARY {
            if !items.is_empty() {
                items.remove(0);
            }
        } else {
            items.truncate(1);
        }
        self.assign(control, items);
    }

    pub fn add(&mut self, control: Control, chord: &Chord) {
        let mut items = self.bindings()[control.name()].clone();
        let text = chord_to_str(chord);
        if !items.contains(&text) {
            items.push(text);
            self.assign(control, items);
        }
    }
    pub fn replace(&mut self, control: Control, chord: &Chord) {
        self.assign(control, vec![chord_to_str(chord)]);
    }
    pub fn clear(&mut self, control: Control) {
        self.assign(control, vec![]);
    }

    pub fn reset(&mut self, control: Control) {
        let name = if self.defaults.profile_names().contains(&self.s.current) {
            self.s.current.clone()
        } else {
            self.defaults.default_name.clone()
        };
        let v = self.defaults.profile_bindings(&name).get(control.name()).cloned().unwrap_or_default();
        self.assign(control, v);
    }

    pub fn remove_at(&mut self, control: Control, index: usize) {
        let mut items = self.bindings()[control.name()].clone();
        if index < items.len() {
            items.remove(index);
        }
        self.assign(control, items);
    }

    // ---- advanced mode (on the current profile) --------------------------------------------------------
    pub fn advanced(&self) -> &Advanced {
        &self.s.advanced[&self.s.current]
    }

    fn advanced_mut(&mut self) -> &mut Advanced {
        let cur = self.s.current.clone();
        self.s.advanced.entry(cur).or_default()
    }

    /// Switch advanced mode on or off. The first time it is switched on the three layers start as copies of the primary keys, so nothing
    /// stops working; what was in primary and secondary stays in the file for when it is switched off.
    pub fn set_advanced(&mut self, on: bool) {
        let primary: BTreeMap<String, Vec<String>> = self
            .bindings()
            .iter()
            .filter_map(|(c, chords)| chords.first().map(|f| (c.clone(), vec![f.clone()])))
            .collect();
        let a = self.advanced_mut();
        a.enabled = on;
        if on && Layer::ALL.iter().all(|l| a.layer(*l).is_empty()) {
            for layer in Layer::ALL {
                *a.layer_mut(layer) = primary.clone();
            }
        }
    }

    /// Turn a layer of the current profile on or off. `false` when it is the last one in use.
    pub fn set_layer_enabled(&mut self, layer: Layer, on: bool) -> bool {
        self.advanced_mut().set_layer_enabled(layer, on)
    }

    /// The key of a control in a layer ("-" when there is none).
    pub fn layer_slot(&self, control: Control, layer: Layer) -> String {
        self.advanced().layer(layer).get(control.name()).and_then(|c| c.first().cloned()).unwrap_or_else(|| "-".into())
    }

    pub fn set_layer_slot(&mut self, control: Control, layer: Layer, chord: &Chord) {
        self.advanced_mut().layer_mut(layer).insert(control.name().into(), vec![chord_to_str(chord)]);
    }

    pub fn clear_layer_slot(&mut self, control: Control, layer: Layer) {
        self.advanced_mut().layer_mut(layer).remove(control.name());
    }

    /// Put an action label on a control, or take it off (`None`). A control carries at most one; an action can be on many controls.
    pub fn set_label(&mut self, control: Control, action: Option<Action>) -> Result<(), String> {
        if !can_carry_action(control) {
            return Err(format!("{} cannot carry an action (only buttons and the triggers can)", control.name()));
        }
        let labels = &mut self.advanced_mut().labels;
        match action {
            Some(a) => labels.insert(control.name().into(), a.name().into()),
            None => labels.remove(control.name()),
        };
        Ok(())
    }

    // ---- profiles ----------------------------------------------------------------------------------
    /// Err unless `name` is a usable, unused profile name.
    pub fn check_name(&self, name: &str, ignore: Option<&str>) -> Result<(), String> {
        if !valid_profile_name(name) {
            return Err("profile names are 1-32 letters, digits, '_' or '-'".into());
        }
        let lower = name.to_lowercase();
        if self.s.profiles.keys().any(|n| Some(n.as_str()) != ignore && n.to_lowercase() == lower) {
            return Err(format!("a profile named {name:?} already exists"));
        }
        Ok(())
    }

    pub fn unique_name(&self, stem: &str) -> String {
        let taken: BTreeSet<String> = self.s.profiles.keys().map(|n| n.to_lowercase()).collect();
        (1..).map(|n| format!("{stem}{n}")).find(|c| !taken.contains(&c.to_lowercase())).unwrap()
    }

    /// New profile: a copy of `source` (default: the base profile).
    pub fn add_profile(&mut self, name: &str, source: Option<&str>) -> Result<(), String> {
        if name.trim().is_empty() {
            return Err("type a name".into());
        }
        let name = &crate::config::snake_name(name);
        self.check_name(name, None)?;
        let src = source.unwrap_or(&self.s.default_name);
        let table = self.s.profiles.get(src).ok_or_else(|| format!("no profile named {src:?}"))?.clone();
        self.s.profiles.insert(name.to_string(), table);
        let adv = self.s.advanced.get(src).cloned().unwrap_or_default();
        self.s.advanced.insert(name.to_string(), adv);
        let id = self.fresh_id();
        self.s.ids.insert(name.to_string(), id);
        Ok(())
    }

    /// The permanent id of a profile (`None` for the base profile or an unknown name).
    pub fn profile_id(&self, name: &str) -> Option<String> {
        if name == self.s.default_name || !self.s.profiles.contains_key(name) {
            return None;
        }
        Some(self.s.ids.get(name).cloned().unwrap_or_else(|| crate::config::derive_id(name)))
    }

    /// A new random id that no profile (including the shipped ones) uses.
    fn fresh_id(&self) -> String {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        loop {
            let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
            let n = SEQ.fetch_add(1, Ordering::Relaxed);
            let id = crate::config::derive_id(&format!("{t}-{n}-{}", std::process::id()));
            let used = self.s.ids.values().chain(self.defaults.profile_ids.values()).any(|x| *x == id);
            if !used {
                return id;
            }
        }
    }

    /// The base profile cannot be deleted (rename it instead).
    pub fn delete_profile(&mut self, name: &str) -> Result<(), String> {
        if name == self.s.default_name {
            return Err("the base profile cannot be deleted (it can be renamed)".into());
        }
        if !self.s.profiles.contains_key(name) {
            return Err(format!("no profile named {name:?}"));
        }
        if self.defaults.profiles.contains_key(name) {
            self.s.deleted.insert(name.to_string());
        } // or it would come back
        self.s.profiles.shift_remove(name);
        self.s.advanced.shift_remove(name);
        self.s.profile_combos.shift_remove(name);
        self.s.ids.remove(name);
        if self.s.current == name {
            self.s.current = self.s.default_name.clone();
        }
        Ok(())
    }

    /// Rename any profile, including the base one and shipped ones.
    pub fn rename_profile(&mut self, old: &str, new: &str) -> Result<(), String> {
        if !self.s.profiles.contains_key(old) {
            return Err(format!("no profile named {old:?}"));
        }
        if new.trim().is_empty() {
            return Err("type a name".into());
        }
        let old_id = self.profile_id(old);
        // whatever was typed is converted to lowercase snake_case (the id stays, so games using this profile keep it)
        let new = &crate::config::snake_name(new);
        if new == old {
            return Ok(());
        }
        self.check_name(new, Some(old))?;
        if old != self.s.default_name && self.defaults.profiles.contains_key(old) {
            self.s.deleted.insert(old.to_string());
        }
        // `new` is never taken out of `deleted`: a profile reusing a deleted shipped name is a plain user profile.
        self.s.profiles = std::mem::take(&mut self.s.profiles)
            .into_iter()
            .map(|(n, t)| (if n == old { new.to_string() } else { n }, t))
            .collect();
        self.s.advanced = std::mem::take(&mut self.s.advanced)
            .into_iter()
            .map(|(n, a)| (if n == old { new.to_string() } else { n }, a))
            .collect();
        if let Some(c) = self.s.profile_combos.shift_remove(old) {
            self.s.profile_combos.insert(new.to_string(), c);
        }
        if let Some(id) = old_id {
            self.s.ids.remove(old);
            self.s.ids.insert(new.to_string(), id);
        }
        if old == self.s.default_name {
            self.s.default_name = new.to_string();
        }
        if self.s.current == old {
            self.s.current = new.to_string();
        }
        Ok(())
    }

    /// Put a profile back to its shipped layout (base -> shipped base; shipped profile -> its layout;
    /// a user profile -> the shipped base bindings). Per-profile combos are cleared.
    pub fn reset_profile(&mut self, name: Option<&str>) -> Result<(), String> {
        let name = name.unwrap_or(&self.s.current).to_string();
        if !self.s.profiles.contains_key(&name) {
            return Err(format!("no profile named {name:?}"));
        }
        let d = self.defaults.clone();
        let shipped = name != self.s.default_name && d.profiles.contains_key(&name) && !self.s.deleted.contains(&name);
        let target = d.profile_bindings(if shipped { &name } else { &d.default_name });
        if name == self.s.default_name {
            let keep = std::mem::replace(&mut self.s.current, self.s.default_name.clone());
            for c in all_controls() {
                self.assign(c, target.get(c.name()).cloned().unwrap_or_default());
            }
            self.s.current = keep;
        } else {
            let t: Table2 = all_controls()
                .iter()
                .map(|c| (c.name().to_string(), target.get(c.name()).cloned().unwrap_or_default()))
                .collect();
            self.s.profiles.insert(name.clone(), t);
        }
        self.s.profile_combos.shift_remove(&name);
        self.s.advanced.insert(name, Advanced::default());
        Ok(())
    }

    /// Set `[section] key = value` in the user's file (kept when everything is saved).
    pub fn set_option(&mut self, section: &str, key: &str, value: Value) {
        let e = self.s.raw.entry(section.to_string()).or_insert_with(|| Value::Table(Table::new()));
        if let Value::Table(t) = e {
            t.insert(key.to_string(), value);
        }
    }

    pub fn next_profile(&mut self) -> String {
        let names = self.profile_names();
        let i = names.iter().position(|n| *n == self.s.current).unwrap_or(0);
        self.s.current = names[(i + 1) % names.len()].clone();
        self.s.current.clone()
    }

    // ---- rollback ---------------------------------------------------------------------------------------
    pub fn snapshot(&self) -> EditorState {
        self.s.clone()
    }
    pub fn restore(&mut self, snap: EditorState) {
        self.s = snap;
    }

    /// Err if the current state would not load (nothing is written).
    pub fn validate(&self) -> Result<(), String> {
        load_layers(Some(&dump_toml(&self.to_toml())), false).map(|_| ())
    }

    // ---- persistence -----------------------------------------------------------------------------------
    /// Only what differs from the shipped defaults is written, so later improvements to the defaults
    /// still reach this user.
    pub fn to_toml(&self) -> Table {
        let s = &self.s;
        let mut data: Table = s
            .raw
            .iter()
            .filter(|(k, _)| !["bindings", "combos", "profiles", "profile_ids", "advanced"].contains(&k.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        data.insert("toggle".into(), Value::String(s.toggle.clone()));
        data.insert("quit".into(), Value::String(s.quit.clone()));
        data.insert("profile_key".into(), Value::String(s.profile_key.clone()));
        if s.default_name != DEFAULT_PROFILE {
            data.insert("default_name".into(), Value::String(s.default_name.clone()));
        } else {
            data.remove("default_name");
        }
        if s.deleted.is_empty() {
            data.remove("deleted_profiles");
        } else {
            data.insert(
                "deleted_profiles".into(),
                Value::Array(s.deleted.iter().cloned().map(Value::String).collect()),
            );
        }
        let base = &s.profiles[&s.default_name];
        // With use_defaults = false the shipped bindings are not underneath: write everything that is set.
        let use_defaults = s.raw.get("use_defaults").and_then(Value::as_bool).unwrap_or(true);
        let shipped_base =
            if use_defaults { self.defaults.profile_bindings(&self.defaults.default_name) } else { Default::default() };
        let changed: Table = all_controls()
            .iter()
            .filter(|c| base[c.name()] != shipped_base.get(c.name()).cloned().unwrap_or_default())
            .map(|c| (c.name().to_string(), strs(&base[c.name()])))
            .collect();
        if changed.is_empty() {
            data.remove("bindings");
        } else {
            data.insert("bindings".into(), Value::Table(changed));
        }
        if !s.combos.is_empty() {
            data.insert("combos".into(), Value::Array(s.combos.clone()));
        }
        if let Some(a) = s.advanced.get(&s.default_name).filter(|a| !a.is_empty()) {
            data.insert("advanced".into(), advanced_value(a));
        }
        let mut profiles = Table::new();
        for (name, table) in &s.profiles {
            if *name == s.default_name {
                continue;
            }
            let shipped: Table2 = if use_defaults && !s.deleted.contains(name) {
                self.defaults
                    .profiles
                    .get(name)
                    .map(|p| p.bindings.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                    .unwrap_or_default()
            } else {
                Table2::new()
            };
            let keep: Table = all_controls()
                .iter()
                .filter(|c| table[c.name()] != base[c.name()] || shipped.contains_key(c.name()))
                .map(|c| (c.name().to_string(), strs(&table[c.name()])))
                .collect();
            let mut prof =
                s.raw.get("profiles").and_then(|p| p.get(name)).and_then(Value::as_table).cloned().unwrap_or_default();
            prof.insert("bindings".into(), Value::Table(keep));
            prof.remove("advanced");
            if let Some(a) = s.advanced.get(name).filter(|a| !a.is_empty()) {
                prof.insert("advanced".into(), advanced_value(a));
            }
            match s.profile_combos.get(name) {
                Some(c) if !c.is_empty() => {
                    prof.insert("combos".into(), Value::Array(c.clone()));
                }
                _ => {
                    prof.remove("combos");
                }
            }
            profiles.insert(name.clone(), Value::Table(prof));
        }
        if !profiles.is_empty() {
            data.insert("profiles".into(), Value::Table(profiles));
        }
        // ids of the profiles that exist (the base has none)
        let ids: Table = s
            .profiles
            .keys()
            .filter(|n| **n != s.default_name)
            .filter_map(|n| s.ids.get(n).map(|i| (n.clone(), Value::String(i.clone()))))
            .collect();
        if !ids.is_empty() {
            data.insert("profile_ids".into(), Value::Table(ids));
        } else {
            data.remove("profile_ids");
        }
        data
    }

    /// Validate BEFORE replacing: a bad edit never clobbers the file. Written atomically.
    pub fn save(&self) -> Result<(), String> {
        let text = dump_toml(&self.to_toml());
        load_layers(Some(&text), false)?;
        let dir = self.path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let tmp = self.path.with_extension("toml.tmp");
        std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &self.path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            e.to_string()
        })
    }
}
