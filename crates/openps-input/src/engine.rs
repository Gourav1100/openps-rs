//! Engine state machine (spec 04 sections 7-8; port of the mode logic in engine.py).
//!
//! Sans-IO: everything OS-specific (focus, window bounds, event tap, cursor, sounds, overlay window,
//! killing the target) sits behind [`Platform`], so the logic is deterministic to test. A real
//! listener thread calls [`Engine::hotkey_for`]/[`Engine::request`] on a press (flags only) and the
//! loop calls [`Engine::tick`] at [`Engine::period`].
//!
//! Includes rebinding capture (mapping OFF, overlay visible), profile rename by typing, overlay UI
//! events and saving through [`Editor`].

use crate::config::{can_carry_action, load_path, Advanced, Config};
use crate::editor::Editor;
use crate::keys::{chord_to_str, Chord, Input, Key, MouseInput};
use crate::mapping::{InputSnapshot, Mapper};
pub use crate::overlay::OverlayState;
use crate::overlay::{build_state, Capturing, Renaming, MAX_PROFILES, SENS_MAX, SENS_MIN};
use crate::scene::{Action, Layer, SceneLink, SceneStatus};
use crate::state::{parse_control, Axis, Control, ControllerState};
use crate::textinput::{apply_key, is_confirm, TextAction};
use std::collections::{BTreeSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;

pub const IDLE_HZ: f64 = 50.0;
pub const FOCUS_POLL_S: f64 = 0.1;
pub const WATCHDOG_S: f64 = 0.5;
pub const NOTICE_S: f64 = 6.0;
pub const CAPTURE_TIMEOUT_S: f64 = 12.0;
pub const MOUSE_SETTLE_S: f64 = 0.15; // wait after a captured mouse click for the overlay to claim it
pub const ESCAPE_HOLD_S: f64 = 0.6; // while capturing: tap Escape = bind it, hold it this long = skip
pub const TEXT_TIMEOUT_S: f64 = 30.0; // renaming gives up after this long without a key

/// x, y, width, height of the target window.
pub type Bounds = (f64, f64, f64, f64);

pub trait Platform {
    fn is_frontmost(&mut self, app: &str) -> bool;
    fn window_bounds(&mut self, app: &str) -> Option<Bounds>;
    /// Current pressed inputs; consumes accumulated mouse deltas.
    fn snapshot(&mut self) -> InputSnapshot;
    fn set_exclusive(&mut self, keys: bool, mouse: bool);
    fn set_hotkey_swallow(&mut self, on: bool);
    fn set_hotkeys(&mut self, chords: &[Chord]);
    /// Re-enable a disabled event tap and resync stuck keys (watchdog).
    fn ensure_enabled(&mut self) {}
    fn cursor_engage(&mut self, center: Option<(f64, f64)>);
    fn cursor_recenter(&mut self, center: (f64, f64));
    fn cursor_release(&mut self);
    fn cursor_set_hide(&mut self, hide: bool);
    fn send_pad(&mut self, state: &ControllerState);
    fn beep(&mut self, sound: &str, force: bool);
    /// SIGTERM then SIGKILL the target app; returns the pids that were killed.
    fn force_quit(&mut self, app: &str) -> Vec<u32>;
    /// The user pressed the quit (`quit_app == false`: end the session) or panic (`true`: close everything) hotkey. Input has
    /// already been released when this is called. Hosts that own the stream act on it; the default does nothing.
    fn exit_requested(&mut self, _quit_app: bool) {}
    fn overlay_push(&mut self, state: &OverlayState);
    fn overlay_close(&mut self) {}
    /// Rename mode: start/stop recording non-repeat key presses as (key, shift held).
    fn start_key_log(&mut self) {}
    fn stop_key_log(&mut self) {}
    /// Forget momentary inputs (scroll ticks) gathered while mapping was off, so they do not fire as a phantom press.
    fn discard_pulses(&mut self) {}
    fn drain_key_log(&mut self) -> Vec<(Key, bool)> {
        vec![]
    }
}

/// Messages from the overlay UI (Tauri front end). Anything malformed is ignored.
#[derive(Clone, Debug, PartialEq)]
pub enum UiEvent {
    OverlayClick,
    Cancel,
    Edit {
        control: String,
        slot: i64,
    },
    Clear {
        control: String,
        slot: i64,
    },
    Pick(String),
    ProfileReset,
    Slider {
        value: f64,
        final_: bool,
    },
    ProfileSelect(String),
    ProfileAdd,
    /// A new profile that starts as a copy of the named one.
    ProfileAddFrom(String),
    ProfileDelete(String),
    ProfileRename(String),
    /// Switch an advanced profile's mode on or off (saved). It also switches scene detection, and works while mapping is on.
    Advanced(bool),
    /// Use a layer of the advanced profile or not (saved), like the advanced switch while mapping is on too. The last layer in use stays.
    LayerEnabled {
        layer: String,
        enabled: bool,
    },
    /// Put an action label on a control, or take it off (`None`) (saved).
    Label {
        control: String,
        label: Option<String>,
    },
}

impl UiEvent {
    pub fn from_json(v: &serde_json::Value) -> Option<UiEvent> {
        let kind = v.get("type")?.as_str()?;
        let control = || v.get("control").and_then(|c| c.as_str()).map(String::from);
        let slot = || v.get("slot").and_then(|c| c.as_i64());
        let name = || v.get("name").and_then(|c| c.as_str()).unwrap_or("").to_string();
        Some(match kind {
            "overlay_click" => UiEvent::OverlayClick,
            "cancel" => UiEvent::Cancel,
            "edit" => UiEvent::Edit { control: control()?, slot: slot()? },
            "clear" => UiEvent::Clear { control: control()?, slot: slot()? },
            "pick" => UiEvent::Pick(v.get("input")?.as_str()?.to_string()),
            "profile_reset" => UiEvent::ProfileReset,
            "slider" => UiEvent::Slider {
                value: v.get("value")?.as_f64()?,
                final_: v.get("final").and_then(|f| f.as_bool()).unwrap_or(false),
            },
            "profile_select" => UiEvent::ProfileSelect(name()),
            "profile_add" => match v.get("source").and_then(|c| c.as_str()) {
                Some(s) if !s.is_empty() => UiEvent::ProfileAddFrom(s.to_string()),
                _ => UiEvent::ProfileAdd,
            },
            "profile_delete" => UiEvent::ProfileDelete(name()),
            "profile_rename" => UiEvent::ProfileRename(name()),
            "advanced" => UiEvent::Advanced(v.get("enabled")?.as_bool()?),
            "layer_enabled" => UiEvent::LayerEnabled {
                layer: v.get("layer")?.as_str()?.to_string(),
                enabled: v.get("enabled")?.as_bool()?,
            },
            "label" => UiEvent::Label {
                control: control()?,
                label: match v.get("label") {
                    None | Some(serde_json::Value::Null) => None,
                    Some(l) => Some(l.as_str()?.to_string()),
                },
            },
            _ => return None,
        })
    }
}

struct Capture {
    control: String,
    slot: u8,
    deadline: f64,
    ignore: BTreeSet<Input>, // held when it began (the opening click)
    peak: Chord,
    esc_since: Option<f64>,
    mouse_seen_at: Option<f64>,
    released_at: Option<f64>,
    clicked_overlay_at: Option<f64>,
}

struct Rename {
    old: String,
    text: String,
    deadline: f64,
    fresh: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hotkey {
    Panic,
    Quit,
    Toggle,
    NextProfile,
    Overlay,
    Slot(usize),
}

/// After the cursor is captured the mouse is ignored for this long.
const MOUSE_HOLD_S: f64 = 0.12;

pub struct Engine<P: Platform> {
    pub cfg: Config,
    platform: P,
    mapper: Mapper,
    profiles: Vec<String>,
    profile_idx: usize,
    enabled: bool,
    focused: bool,
    anchor: Option<Bounds>,
    overlay_wanted: bool,
    notice: String,
    notice_until: f64,
    last_pushed: Option<OverlayState>,
    owns_target: bool,
    config_path: Option<PathBuf>,
    capture: Option<Capture>,
    rename: Option<Rename>,
    ui_events: VecDeque<UiEvent>,
    stop: bool,
    // flags set by hotkeys
    f_toggle: bool,
    f_profile: bool,
    f_overlay: bool,
    slot_request: Option<usize>,
    // timing
    now: f64,
    last_focus: f64,
    last_dog: f64,
    /// Mouse movement is ignored until this time (just after the cursor was captured).
    mouse_hold_until: f64,
    panicking: bool,
    scene: Arc<SceneLink>,
    last_scene: SceneStatus,
    last_scene_poll: f64,
    /// Controls whose action label was held on the last tick (an action is a press, not a hold).
    labeled_down: BTreeSet<Control>,
}

impl<P: Platform> Engine<P> {
    pub fn new(cfg: Config, mut platform: P, owns_target: bool) -> Result<Self, String> {
        let mapper = Mapper::new(cfg.clone(), None)?;
        let profiles = cfg.profile_names();
        let chords: Vec<Chord> = cfg.hotkey_chords()?.into_iter().map(|(_, c)| c).collect();
        platform.set_hotkeys(&chords);
        platform.cursor_set_hide(cfg.mouse.hide_cursor);
        Ok(Self {
            cfg,
            platform,
            mapper,
            profiles,
            profile_idx: 0,
            enabled: false,
            focused: true,
            anchor: None,
            overlay_wanted: false,
            notice: String::new(),
            notice_until: 0.0,
            last_pushed: None,
            owns_target,
            config_path: None,
            capture: None,
            rename: None,
            ui_events: VecDeque::new(),
            stop: false,
            f_toggle: false,
            f_profile: false,
            f_overlay: false,
            slot_request: None,
            now: 0.0,
            last_focus: f64::NEG_INFINITY,
            last_dog: f64::NEG_INFINITY,
            mouse_hold_until: f64::NEG_INFINITY,
            panicking: false,
            scene: Arc::new(SceneLink::new()),
            last_scene: SceneStatus::default(),
            last_scene_poll: f64::NEG_INFINITY,
            labeled_down: BTreeSet::new(),
        })
    }

    /// Share a scene link with the scene detector (the engine makes its own, unused one otherwise).
    pub fn with_scene(mut self, link: Arc<SceneLink>) -> Self {
        self.scene = link;
        self
    }

    pub fn scene(&self) -> &Arc<SceneLink> {
        &self.scene
    }

    /// The file overlay edits are saved to (the one this run was started with).
    pub fn with_config_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.config_path = Some(path.into());
        self
    }

    // ---- state -------------------------------------------------------------------------
    pub fn capturing(&self) -> bool {
        self.capture.is_some()
    }
    pub fn renaming(&self) -> bool {
        self.rename.is_some()
    }
    pub fn enabled(&self) -> bool {
        self.enabled
    }
    /// Mapping is on *and* the target app has focus: the only time input is grabbed.
    pub fn live(&self) -> bool {
        self.enabled && self.focused
    }
    pub fn focused(&self) -> bool {
        self.focused
    }
    pub fn profile(&self) -> &str {
        &self.profiles[self.profile_idx]
    }
    pub fn stopped(&self) -> bool {
        self.stop
    }
    pub fn platform(&mut self) -> &mut P {
        &mut self.platform
    }
    pub fn mapper(&mut self) -> &mut Mapper {
        &mut self.mapper
    }
    pub fn overlay_wanted(&self) -> bool {
        self.overlay_wanted
    }
    /// Set the engine clock (seconds). `tick` does this itself; tests and event handlers can too.
    pub fn set_time(&mut self, now: f64) {
        self.now = now;
    }
    pub fn notice_text(&self) -> &str {
        if self.now < self.notice_until {
            &self.notice
        } else {
            ""
        }
    }

    // ---- hotkeys (listener thread: flags only) -------------------------------------------
    /// Priority: panic, quit, toggle, profile key, overlay, slots. A hotkey fires on a new press of an
    /// input that belongs to the chord while the whole chord is held.
    pub fn hotkey_for(&self, inp: Input, pressed: &BTreeSet<Input>) -> Option<Hotkey> {
        let hit = |c: &Result<Chord, String>| c.as_ref().is_ok_and(|c| c.contains(&inp) && c.is_subset(pressed));
        if hit(&self.cfg.panic_chord()) {
            return Some(Hotkey::Panic);
        }
        if hit(&self.cfg.quit_chord()) {
            return Some(Hotkey::Quit);
        }
        if hit(&self.cfg.toggle_chord()) {
            return Some(Hotkey::Toggle);
        }
        if hit(&self.cfg.profile_key_chord()) {
            return Some(Hotkey::NextProfile);
        }
        if hit(&self.cfg.overlay_chord()) {
            return Some(Hotkey::Overlay);
        }
        self.cfg
            .profile_slot_chords()
            .ok()?
            .into_iter()
            .find(|(_, _, c)| c.contains(&inp) && c.is_subset(pressed))
            .map(|(i, _, _)| Hotkey::Slot(i))
    }

    pub fn request(&mut self, h: Hotkey) {
        match h {
            Hotkey::Panic => {
                self.panic();
                self.platform.exit_requested(true);
            }
            Hotkey::Quit => {
                self.stop = true;
                self.enabled = false;
                self.mapper.reset();
                self.platform.cursor_release();
                self.platform.set_exclusive(false, false);
                self.platform.send_pad(&ControllerState::default()); // no stuck button/stick on the console
                self.platform.exit_requested(false);
            }
            Hotkey::Toggle => self.f_toggle = true,
            Hotkey::NextProfile => self.f_profile = true,
            Hotkey::Overlay => self.f_overlay = true,
            Hotkey::Slot(i) => self.slot_request = Some(i),
        }
    }

    pub fn on_press(&mut self, inp: Input, pressed: &BTreeSet<Input>) -> Option<Hotkey> {
        let h = self.hotkey_for(inp, pressed)?;
        self.request(h);
        Some(h)
    }

    // ---- mode switching ------------------------------------------------------------------
    fn center(&self) -> Option<(f64, f64)> {
        self.anchor.map(|b| (b.0 + b.2 / 2.0, b.1 + b.3 / 2.0))
    }

    /// Set input swallowing for the CURRENT mode.
    fn apply_exclusivity(&mut self) {
        let excl = self.cfg.suppress_input;
        if self.live() {
            self.platform.set_exclusive(excl, excl);
        } else {
            // While capturing a binding / typing a name the keyboard is held back from the app, the mouse is not.
            let typing = self.capture.is_some() || self.rename.is_some();
            self.platform.set_exclusive(excl && typing, false);
        }
        self.platform.set_hotkey_swallow(self.focused || self.cfg.target_app.is_empty());
    }

    /// Forget any capture/rename WITHOUT touching swallowing (callers re-apply it).
    fn drop_text_modes(&mut self) {
        self.capture = None;
        if self.rename.take().is_some() {
            self.platform.stop_key_log();
        }
    }

    fn sync(&mut self) {
        self.mapper.reset();
        self.platform.discard_pulses();
        if self.live() {
            self.drop_text_modes(); // editing is only possible while mapping is off
            self.apply_exclusivity();
            if self.cfg.mouse.capture_cursor {
                let c = self.center();
                self.platform.cursor_engage(c);
                // whatever the pointer was doing a moment ago (and the warp to the centre) must not jerk the camera
                self.mouse_hold_until = self.now + MOUSE_HOLD_S;
            }
        } else {
            self.apply_exclusivity();
            self.platform.cursor_release();
            self.platform.send_pad(&ControllerState::default()); // release everything
        }
        self.push_overlay();
    }

    pub fn apply_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        self.sync();
        self.beep(if enabled { "Pop" } else { "Basso" }, false);
    }

    fn switch_profile(&mut self, idx: usize) {
        self.drop_text_modes(); // a half-typed name / capture belongs to the old profile
        self.apply_exclusivity();
        self.profile_idx = idx % self.profiles.len();
        let name = self.profile().to_string();
        let _ = self.mapper.set_profile(&name);
        self.platform.send_pad(&ControllerState::default()); // release anything held from the old layout
        self.beep("Glass", false);
        self.push_overlay();
    }

    pub fn next_profile(&mut self) {
        self.switch_profile(self.profile_idx + 1);
    }

    /// Command + F<n>: jump straight to the n-th profile.
    pub fn select_slot(&mut self, idx: usize) {
        if idx < self.profiles.len() {
            self.switch_profile(idx);
        } else {
            self.say(format!("No profile in slot {}.", idx.saturating_add(1)));
        }
    }

    pub fn poll_window(&mut self) {
        let name = self.cfg.target_app.clone();
        let focused = self.platform.is_frontmost(&name);
        let anchor = if name.is_empty() { None } else { self.platform.window_bounds(&name) };
        let changed_focus = focused != self.focused;
        let changed_anchor = anchor != self.anchor;
        self.focused = focused;
        self.anchor = anchor;
        if changed_focus {
            if !focused {
                self.drop_text_modes();
            } // the overlay is hidden now: never keep the keyboard
            self.sync();
        } else if changed_anchor {
            self.push_overlay();
        }
        if self.live() && self.cfg.mouse.capture_cursor {
            if let Some(c) = self.center() {
                self.platform.cursor_recenter(c);
            } // window moved or resized
        }
    }

    // ---- overlay ---------------------------------------------------------------------------
    pub fn overlay_visible(&self) -> bool {
        self.overlay_wanted && (self.focused || self.cfg.target_app.is_empty())
    }

    fn overlay_state(&self) -> OverlayState {
        build_state(
            &self.cfg,
            self.profile(),
            self.enabled,
            self.overlay_visible(),
            self.anchor,
            self.capture.as_ref().map(|c| Capturing { control: c.control.clone(), slot: c.slot }),
            if self.now < self.notice_until { self.notice.clone() } else { String::new() },
            self.rename.as_ref().map(|r| Renaming { name: r.old.clone(), text: r.text.clone() }),
            self.mapper.click_modes(),
            self.scene.status(),
        )
    }

    pub fn push_overlay(&mut self) {
        if !self.cfg.overlay {
            return;
        }
        let s = self.overlay_state();
        if self.last_pushed.as_ref() != Some(&s) {
            // never spam the UI with identical frames
            self.platform.overlay_push(&s);
            self.last_pushed = Some(s);
        }
    }

    pub fn say(&mut self, text: String) {
        self.notice = text;
        self.notice_until = self.now + NOTICE_S;
        self.push_overlay();
    }

    pub fn toggle_overlay(&mut self) {
        if !self.cfg.overlay {
            return;
        }
        self.overlay_wanted = !self.overlay_wanted;
        if !self.overlay_wanted {
            self.drop_text_modes(); // nothing to type into any more: release the keyboard
            self.apply_exclusivity();
        }
        self.push_overlay();
    }

    // ---- config hot reload -----------------------------------------------------------------
    /// Swap in a reloaded config AND everything derived from it. `select` names the profile to show.
    pub fn apply_config(&mut self, new: Config, select: Option<&str>) -> Result<(), String> {
        let chords: Vec<Chord> = new.hotkey_chords()?.into_iter().map(|(_, c)| c).collect();
        let wanted = select.map(String::from).unwrap_or_else(|| self.profile().to_string());
        let profiles = new.profile_names();
        let idx = profiles.iter().position(|p| *p == wanted).unwrap_or(0);
        let bumper = self.mapper.bumper();
        let mut mapper = Mapper::new(new.clone(), Some(&profiles[idx]))?;
        mapper.set_bumper(bumper); // RT/RB and LT/LB are a session choice: keep them
        self.cfg = new;
        self.profiles = profiles;
        self.profile_idx = idx;
        self.mapper = mapper;
        self.platform.set_hotkeys(&chords);
        self.platform.cursor_set_hide(self.cfg.mouse.hide_cursor);
        self.apply_exclusivity();
        self.push_overlay();
        Ok(())
    }

    // ---- safety ------------------------------------------------------------------------------
    /// Force-quit the target app, give the cursor back, stop mapping and exit. Single-flight.
    pub fn panic(&mut self) -> Vec<u32> {
        if self.panicking {
            return vec![];
        }
        self.panicking = true;
        self.enabled = false;
        self.platform.cursor_release(); // first: the user must regain the mouse
        self.platform.set_exclusive(false, false);
        self.mapper.reset();
        self.platform.send_pad(&ControllerState::default()); // no stuck button/stick on the console
        let killed = if self.cfg.target_app.is_empty() {
            vec![]
        } else {
            self.platform.force_quit(&self.cfg.target_app.clone())
        };
        self.beep("Sosumi", true);
        self.stop = true;
        killed
    }

    fn beep(&mut self, sound: &str, force: bool) {
        if self.cfg.feedback_sounds {
            self.platform.beep(sound, force);
        }
    }

    pub fn stop(&mut self) {
        self.stop = true;
    }

    // ---- lifecycle ------------------------------------------------------------------------------
    pub fn start(&mut self, now: f64) {
        self.now = now;
        self.focused = self.platform.is_frontmost(&self.cfg.target_app.clone());
        self.anchor = if self.cfg.target_app.is_empty() {
            None
        } else {
            self.platform.window_bounds(&self.cfg.target_app.clone())
        };
        let on = self.cfg.start_enabled;
        self.apply_enabled(on);
        if let Some(first) = self.cfg.notes.first().cloned() {
            self.say(format!("Config updated: {first}"));
        }
    }

    /// Cleanup that Python runs in `finally`: cursor, overlay, target app, neutral pad state.
    pub fn shutdown(&mut self) {
        self.platform.cursor_release();
        if self.owns_target && self.cfg.close_target_on_exit && !self.cfg.target_app.is_empty() {
            self.platform.force_quit(&self.cfg.target_app.clone());
        }
        self.platform.overlay_close();
        self.platform.send_pad(&ControllerState::default());
    }

    /// Seconds the loop should sleep after a tick: 1/poll_hz while live, 1/50 otherwise.
    pub fn period(&self) -> f64 {
        1.0 / if self.live() { self.cfg.poll_hz as f64 } else { IDLE_HZ }
    }

    /// One loop iteration (order from spec 04 section 8). `mono` is monotonic seconds.
    pub fn tick(&mut self, mono: f64) {
        self.now = mono;
        if self.stop {
            // After Quit/Panic nothing may re-enable mapping or re-capture the cursor.
            self.f_toggle = false;
            self.f_profile = false;
            self.f_overlay = false;
            self.slot_request = None;
            return;
        }
        if mono - self.last_focus >= FOCUS_POLL_S {
            self.last_focus = mono;
            self.poll_window();
        }
        if mono - self.last_dog >= WATCHDOG_S {
            self.last_dog = mono;
            self.platform.ensure_enabled();
            if !self.notice.is_empty() && mono >= self.notice_until {
                self.notice.clear();
                self.push_overlay();
            }
        }
        if std::mem::take(&mut self.f_toggle) {
            let on = !self.enabled;
            self.apply_enabled(on);
        }
        if std::mem::take(&mut self.f_profile) {
            self.next_profile();
        }
        if let Some(i) = self.slot_request.take() {
            self.select_slot(i);
        }
        if std::mem::take(&mut self.f_overlay) {
            self.toggle_overlay();
        }
        self.process_ui_events();
        self.poll_capture(mono);
        self.poll_text(mono);
        self.poll_scene(mono);
        if self.live() {
            let mut snap = self.platform.snapshot();
            if mono < self.mouse_hold_until {
                (snap.mouse_dx, snap.mouse_dy) = (0.0, 0.0);
            }
            self.sync_layer();
            let st = self.mapper.map(&snap, mono);
            self.emit_actions(&st);
            self.platform.send_pad(&st);
            for ev in self.mapper.pop_events() {
                self.beep("Tink", false);
                let mut c = ev.chars();
                let text = c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default();
                self.say(text);
            }
        }
    }
}

// ---- editing through the overlay (mapping OFF only) ------------------------------------------------------
impl<P: Platform> Engine<P> {
    /// The profile in use is in advanced mode.
    fn advanced_on(&self) -> bool {
        self.cfg.advanced_of(self.profile()).is_some_and(|a| a.enabled)
    }

    /// An advanced profile uses the keys of the layer the scene detector reports (on foot while it is off).
    fn sync_layer(&mut self) {
        let layer = if self.advanced_on() { self.scene.layer() } else { Layer::OnFoot };
        if layer != self.mapper.layer() {
            let _ = self.mapper.set_layer(layer);
        }
    }

    /// A press of a labeled control is what the scene detector is told about, whatever keys it was bound to.
    fn emit_actions(&mut self, st: &ControllerState) {
        let labeled: Vec<(Control, Action)> = match self.cfg.advanced_of(self.profile()) {
            Some(a) if a.enabled && !a.labels.is_empty() => a
                .labels
                .keys()
                .filter_map(|c| Some((parse_control(c).ok()?, a.label_of(c)?)))
                .filter(|(c, _)| can_carry_action(*c))
                .collect(),
            _ => vec![],
        };
        if labeled.is_empty() {
            self.labeled_down.clear();
            return;
        }
        let held = |c: Control| match c {
            Control::Button(b) => st.buttons.contains(&b),
            Control::Axis(Axis::L2) => st.l2 > 0.5,
            Control::Axis(Axis::R2) => st.r2 > 0.5,
            _ => false,
        };
        let mut down = BTreeSet::new();
        for (c, action) in labeled {
            if held(c) {
                down.insert(c);
                if !self.labeled_down.contains(&c) {
                    self.scene.push_action(action);
                }
            }
        }
        self.labeled_down = down;
    }

    /// Show the overlay and the HUD when what the detector believes changes (at most ten times a second).
    fn poll_scene(&mut self, now: f64) {
        if now - self.last_scene_poll < 0.1 {
            return;
        }
        self.last_scene_poll = now;
        // advanced mapping is what switches scene detection on, and it only matters while the key map drives the console (with mapping
        // off a physical controller does)
        self.scene.set_enabled(self.advanced_on());
        self.scene.set_active(self.enabled);
        self.scene.set_layers(self.cfg.advanced_of(self.profile()).map(Advanced::enabled_layers).unwrap_or_default());
        // only a key map that is live and has labels can say that nothing changed because nothing was pressed
        let labeled: Vec<Action> = match self.cfg.advanced_of(self.profile()) {
            Some(a) if self.live() && a.enabled => a.labels.keys().filter_map(|c| a.label_of(c)).collect(),
            _ => vec![],
        };
        self.scene.set_labeled(labeled);
        let status = self.scene.status();
        if status != self.last_scene {
            self.last_scene = status;
            self.push_overlay();
        }
    }

    pub fn push_ui_event(&mut self, e: UiEvent) {
        self.ui_events.push_back(e);
    }

    fn process_ui_events(&mut self) {
        while let Some(e) = self.ui_events.pop_front() {
            self.handle_ui_event(e);
        }
    }

    pub fn handle_ui_event(&mut self, e: UiEvent) {
        let is_edit = !matches!(e, UiEvent::OverlayClick | UiEvent::Cancel);
        // a click on the overlay itself must never be recorded as a new binding
        if matches!(e, UiEvent::OverlayClick | UiEvent::Cancel) || is_edit {
            self.ignore_overlay_click();
        }
        match e {
            UiEvent::OverlayClick => return,
            UiEvent::Cancel => {
                self.end_capture("");
                return;
            }
            // a live switch, like the profile keys: it works while mapping is on (the detector starts and stops with it)
            UiEvent::Advanced(on) => {
                if self.overlay_visible() {
                    self.set_advanced(on);
                }
                return;
            }
            UiEvent::LayerEnabled { layer, enabled } => {
                if self.overlay_visible() {
                    self.set_layer_enabled(&layer, enabled);
                }
                return;
            }
            _ => {}
        }
        if self.enabled {
            self.say(format!("Turn mapping OFF ({}) to edit.", self.cfg.toggle));
            return;
        }
        if !self.overlay_visible() {
            return;
        } // a click can only come from a visible overlay; anything else is stale
        let clear = matches!(&e, UiEvent::Clear { .. });
        match e {
            UiEvent::Edit { control, slot } | UiEvent::Clear { control, slot } => {
                let Ok(ctl) = parse_control(&control) else { return };
                let slot = u8::try_from(slot).unwrap_or(0);
                let valid = if self.advanced_on() { Layer::from_slot(slot).is_some() } else { slot == 1 || slot == 2 };
                if !valid {
                    return;
                } // the UI is not trusted to send sane numbers
                if clear {
                    self.save_binding(ctl.name(), slot, None);
                } else {
                    self.begin_capture(ctl.name(), slot);
                }
            }
            UiEvent::Label { control, label } => self.set_label(&control, label.as_deref()),
            UiEvent::Pick(name) => self.pick_mouse_button(&name),
            UiEvent::ProfileReset => self.reset_profile_request(),
            UiEvent::Slider { value, final_ } => self.set_sensitivity(value, final_),
            UiEvent::ProfileSelect(n) => self.profile_request("select", &n),
            UiEvent::ProfileAdd => self.profile_request("add", ""),
            UiEvent::ProfileAddFrom(n) => self.profile_request("add", &n),
            UiEvent::ProfileDelete(n) => self.profile_request("delete", &n),
            UiEvent::ProfileRename(n) => self.profile_request("rename", &n),
            UiEvent::OverlayClick | UiEvent::Cancel | UiEvent::Advanced(_) | UiEvent::LayerEnabled { .. } => {}
        }
    }

    fn ignore_overlay_click(&mut self) {
        if self.capture.is_none() {
            return;
        }
        let buttons: BTreeSet<Input> =
            self.platform.snapshot().pressed.into_iter().filter(|i| matches!(i, Input::Mouse(_))).collect();
        let now = self.now;
        let cap = self.capture.as_mut().unwrap();
        cap.ignore.extend(buttons.iter().copied());
        cap.peak = cap.peak.iter().filter(|i| !buttons.contains(i)).copied().collect();
        cap.clicked_overlay_at = Some(now); // also covers a message that arrives late
    }

    // -- mouse buttons as bindings ---------------------------------------------------------------------
    /// Bind a mouse button (plus any Shift/Ctrl/Option held) to the slot being captured. Command is reserved.
    fn pick_mouse_button(&mut self, name: &str) {
        let button = match name {
            "MOUSE_LEFT" => MouseInput::MOUSE_LEFT,
            "MOUSE_RIGHT" => MouseInput::MOUSE_RIGHT,
            "MOUSE_MIDDLE" => MouseInput::MOUSE_MIDDLE,
            _ => return,
        };
        if self.capture.is_none() {
            return;
        }
        let held = self.platform.snapshot().pressed;
        let mut chord: Chord = [Input::Mouse(button)].into_iter().collect();
        for m in [
            Key::LEFT_SHIFT,
            Key::RIGHT_SHIFT,
            Key::LEFT_CONTROL,
            Key::RIGHT_CONTROL,
            Key::LEFT_OPTION,
            Key::RIGHT_OPTION,
        ] {
            if held.contains(&Input::Key(m)) {
                chord.insert(Input::Key(m));
            }
        }
        let cap = self.capture.take().unwrap();
        self.apply_exclusivity();
        self.commit_chord(cap, chord);
    }

    fn commit_chord(&mut self, cap: Capture, chord: Chord) {
        let reserved = self.cfg.reserved_inputs().unwrap_or_default();
        let mut sorted: Vec<&Input> = chord.iter().collect();
        sorted.sort_by_key(|i| i.name());
        if let Some(bad) = sorted.into_iter().find(|i| reserved.contains_key(i)) {
            self.say(format!("Rejected: {} is reserved - {}", bad.name(), reserved[bad]));
            return;
        }
        self.save_binding(&cap.control, cap.slot, Some(chord));
    }

    // -- reset / sensitivity / profiles ------------------------------------------------------------------
    fn reset_profile_request(&mut self) {
        let name = self.profile().to_string();
        if self.edit_config(&mut |ed| ed.reset_profile(Some(&name)).map(|_| None)) {
            self.beep("Tink", false);
            self.say(format!("Profile {name} reset to its defaults"));
        }
    }

    fn set_sensitivity(&mut self, value: f64, final_: bool) {
        if value.is_nan() {
            return;
        }
        let value = (value.clamp(SENS_MIN, SENS_MAX) * 1e4).round() / 1e4;
        let previous = self.cfg.mouse.sensitivity;
        self.cfg.mouse.sensitivity = value; // live: the mapper reads it every tick
        self.mapper.config.mouse.sensitivity = value;
        if !final_ {
            return;
        }
        if self.edit_config(&mut |ed| {
            ed.set_option("mouse", "sensitivity", toml::Value::Float(value));
            Ok(None)
        }) {
            self.say(format!("Mouse sensitivity {value:.3}"));
        } else {
            self.cfg.mouse.sensitivity = previous; // not saved: do not pretend it was
            self.mapper.config.mouse.sensitivity = previous;
        }
    }

    fn profile_request(&mut self, kind: &str, name: &str) {
        if kind == "add" {
            if self.profiles.len() >= MAX_PROFILES {
                self.say(format!("Up to {MAX_PROFILES} profiles."));
                return;
            }
            let src =
                if self.profiles.iter().any(|p| p == name) { name.to_string() } else { self.profile().to_string() };
            let mut created = String::new();
            let ok = self.edit_config(&mut |ed| {
                created = ed.unique_name("profile");
                ed.add_profile(&created, Some(&src))?;
                Ok(Some(created.clone()))
            });
            if ok {
                self.begin_rename(&created);
            } // name it right away (Enter keeps the default)
            return;
        }
        if !self.profiles.iter().any(|p| p == name) {
            return;
        }
        match kind {
            "select" => {
                let i = self.profiles.iter().position(|p| p == name).unwrap();
                self.switch_profile(i);
            }
            "rename" => self.begin_rename(name),
            "delete" => {
                let n = name.to_string();
                if self.edit_config(&mut |ed| ed.delete_profile(&n).map(|_| None)) {
                    self.say(format!("Profile {name} deleted"));
                }
            }
            _ => {}
        }
    }

    fn begin_rename(&mut self, name: &str) {
        self.capture = None;
        self.rename =
            Some(Rename { old: name.into(), text: name.into(), deadline: self.now + TEXT_TIMEOUT_S, fresh: true });
        self.platform.start_key_log();
        self.apply_exclusivity();
        self.push_overlay();
    }

    fn end_rename(&mut self, notice: &str) {
        if self.rename.take().is_none() {
            return;
        }
        self.platform.stop_key_log();
        self.apply_exclusivity();
        if notice.is_empty() {
            self.push_overlay();
        } else {
            self.say(notice.into());
        }
    }

    fn poll_text(&mut self, now: f64) {
        let Some(r) = self.rename.as_mut() else { return };
        if !(self.focused || self.cfg.target_app.is_empty()) || !self.overlay_wanted || now > r.deadline {
            let visible = self.overlay_visible();
            self.end_rename(if visible { "Rename cancelled." } else { "" });
            return;
        }
        let before = r.text.clone();
        let keys = self.platform.drain_key_log();
        let r = self.rename.as_mut().unwrap();
        for (key, shift) in keys {
            if r.fresh && key != Key::ESCAPE && !is_confirm(key) {
                r.text.clear();
                r.fresh = false;
            } // replace, do not append
            let (text, action) = apply_key(&r.text, key, shift);
            r.text = text;
            r.deadline = now + TEXT_TIMEOUT_S;
            match action {
                Some(TextAction::Cancel) => {
                    self.end_rename("Rename cancelled.");
                    return;
                }
                Some(TextAction::Confirm) => {
                    let (old, new) = (r.old.clone(), r.text.clone());
                    self.end_rename("");
                    self.commit_rename(&old, &new);
                    return;
                }
                None => {}
            }
        }
        if self.rename.as_ref().is_some_and(|r| r.text != before) {
            self.push_overlay();
        }
    }

    fn commit_rename(&mut self, old: &str, new: &str) {
        if new == old {
            self.push_overlay();
            return;
        }
        let keep_current = self.profile() == old;
        let (o, n) = (old.to_string(), new.to_string());
        if self
            .edit_config(&mut |ed| ed.rename_profile(&o, &n).map(|_| if keep_current { Some(n.clone()) } else { None }))
        {
            self.say(format!("Profile {old} is now {new}"));
        }
    }

    // -- rebinding from the overlay --------------------------------------------------------------------------
    fn begin_capture(&mut self, control: &str, slot: u8) {
        self.end_rename("");
        let held = self.platform.snapshot().pressed.into_iter().collect(); // the click that opened this
        self.capture = Some(Capture {
            control: control.into(),
            slot,
            deadline: self.now + CAPTURE_TIMEOUT_S,
            ignore: held,
            peak: Chord::new(),
            esc_since: None,
            mouse_seen_at: None,
            released_at: None,
            clicked_overlay_at: None,
        });
        self.apply_exclusivity();
        self.push_overlay();
    }

    fn end_capture(&mut self, notice: &str) {
        self.capture = None;
        self.apply_exclusivity(); // NOT "everything off": mapping may be live by now
        if notice.is_empty() {
            self.push_overlay();
        } else {
            self.say(notice.into());
        }
    }

    pub fn poll_capture(&mut self, now: f64) {
        if self.capture.is_none() {
            return;
        }
        if !self.overlay_visible() {
            self.end_capture("");
            return;
        } // hidden or app left: stop swallowing the keyboard
        if now > self.capture.as_ref().unwrap().deadline {
            self.end_capture("Timed out - nothing changed.");
            return;
        }
        let pressed = self.platform.snapshot().pressed;
        let hot: Vec<Chord> =
            self.cfg.hotkey_chords().map(|v| v.into_iter().map(|(_, c)| c).collect()).unwrap_or_default();
        let cap = self.capture.as_mut().unwrap();
        cap.ignore = cap.ignore.intersection(&pressed).copied().collect(); // an input stops being ignored once released
        let mut current: Chord = pressed.difference(&cap.ignore).copied().collect();
        // HOLDING Escape skips the capture; a quick tap of it is an ordinary key to bind.
        if current.contains(&Input::Key(Key::ESCAPE)) {
            let since = *cap.esc_since.get_or_insert(now);
            if now - since >= ESCAPE_HOLD_S {
                self.end_capture("Skipped - nothing changed.");
                return;
            }
        } else {
            cap.esc_since = None;
        }
        for chord in &hot {
            if chord.is_subset(&pressed) {
                for i in chord {
                    current.remove(i);
                }
            }
        } // a hotkey is not part of the binding
        if current.len() >= cap.peak.len() && !current.is_empty() {
            cap.peak = current.clone();
        }
        let has_mouse = |c: &Chord| c.iter().any(|i| matches!(i, Input::Mouse(_)));
        if has_mouse(&current) {
            cap.mouse_seen_at.get_or_insert(now);
        }
        if !current.is_empty() {
            cap.released_at = None;
        }
        if !cap.peak.is_empty() && current.is_empty() {
            // everything released: the chord is complete
            if has_mouse(&cap.peak) {
                // The overlay reports its own clicks a few ms AFTER the global listener saw them: wait
                // briefly so a late report can still keep that click out of the binding.
                let rel = *cap.released_at.get_or_insert(now);
                if now - rel < MOUSE_SETTLE_S {
                    return;
                }
                let seen = cap.mouse_seen_at;
                if let Some(c) = cap.clicked_overlay_at {
                    if seen.is_none_or(|s| c >= s - 0.25) {
                        cap.peak = cap.peak.iter().filter(|i| !matches!(i, Input::Mouse(_))).copied().collect();
                        cap.released_at = None;
                        cap.mouse_seen_at = None;
                        if cap.peak.is_empty() {
                            return;
                        } // it was only a click on the overlay: keep waiting
                    }
                }
            }
            let cap = self.capture.take().unwrap();
            let chord = cap.peak.clone();
            self.apply_exclusivity();
            self.commit_chord(cap, chord);
        }
    }

    // -- saving ----------------------------------------------------------------------------------------------
    /// Apply `change` to the config file in use, validate, save, hot-reload. The closure may return the name
    /// of the profile to switch to afterwards. Returns false (and tells the user why) if rejected.
    fn edit_config(&mut self, change: &mut dyn FnMut(&mut Editor) -> Result<Option<String>, String>) -> bool {
        let Some(path) = self.config_path.clone() else {
            self.say("Rejected: no config file to save to".into());
            return false;
        };
        let profile = self.profile().to_string();
        let result = (|| -> Result<(Config, String), String> {
            let mut ed = Editor::new(&path)?;
            ed.set_current(&if ed.profile(&profile).is_some() {
                profile.clone()
            } else {
                ed.default_name().to_string()
            });
            let selected = change(&mut ed)?;
            ed.save()?; // validated before it replaces the file
            let new = load_path(&path, false)?;
            let target = selected.unwrap_or_else(|| {
                if ed.profile(&profile).is_some() {
                    profile.clone()
                } else {
                    ed.default_name().to_string()
                }
            });
            Ok((new, target))
        })();
        match result {
            Ok((new, target)) => self.apply_config(new, Some(&target)).is_ok(),
            Err(e) => {
                self.say(format!("Rejected: {}", short_error(&e)));
                false
            }
        }
    }

    fn save_binding(&mut self, control: &str, slot: u8, chord: Option<Chord>) {
        let Ok(ctl) = parse_control(control) else { return };
        let c2 = chord.clone();
        if !self.edit_config(&mut |ed| {
            match (Layer::from_slot(slot), &c2) {
                (Some(l), None) => ed.clear_layer_slot(ctl, l),
                (Some(l), Some(c)) => ed.set_layer_slot(ctl, l, c),
                (None, None) => ed.clear_slot(ctl, slot),
                (None, Some(c)) => ed.set_slot(ctl, slot, c),
            }
            Ok(None)
        }) {
            return;
        }
        let slot_name = match (Layer::from_slot(slot), slot) {
            (Some(Layer::OnFoot), _) => "on foot",
            (Some(Layer::InVehicle), _) => "in vehicle",
            (Some(Layer::InMenu), _) => "in menu",
            (None, 1) => "primary",
            (None, _) => "secondary",
        };
        self.beep("Tink", false);
        match chord {
            Some(c) => self.say(format!("{control} ({slot_name}) = {}", chord_to_str(&c))),
            None => self.say(format!("{control} {slot_name} deleted")),
        }
    }

    fn set_advanced(&mut self, on: bool) {
        if self.advanced_on() == on {
            return;
        }
        if self.edit_config(&mut |ed| {
            ed.set_advanced(on);
            Ok(None)
        }) {
            self.beep("Tink", false);
            self.say(if on {
                "Advanced mapping on: the keys follow the scene.".into()
            } else {
                "Advanced mapping off: primary and secondary keys.".into()
            });
        }
    }

    fn set_layer_enabled(&mut self, layer: &str, on: bool) {
        let Some(l) = Layer::parse(layer) else { return };
        if self.cfg.advanced_of(self.profile()).is_some_and(|a| a.layer_enabled(l) == on) {
            return;
        }
        let mut kept = true;
        if self.edit_config(&mut |ed| {
            kept = ed.set_layer_enabled(l, on);
            Ok(None)
        }) {
            let name = layer.replace('_', " ");
            if kept {
                self.beep("Tink", false);
                self.say(format!("The {name} keys are {}.", if on { "in use" } else { "not used" }));
            } else {
                self.say("At least one layer has to stay on.".into());
            }
        }
    }

    fn set_label(&mut self, control: &str, label: Option<&str>) {
        let Ok(ctl) = parse_control(control) else { return };
        let action = match label {
            None | Some("") => None,
            Some(name) => match Action::parse(name) {
                Some(a) => Some(a),
                None => return,
            },
        };
        if self.edit_config(&mut |ed| ed.set_label(ctl, action).map(|_| None)) {
            self.beep("Tink", false);
            match action {
                Some(a) => self.say(format!("{control} is now {}", a.title().to_lowercase())),
                None => self.say(format!("{control} has no action")),
            }
        }
    }
}

/// One readable line from a possibly multi-line error.
fn short_error(e: &str) -> String {
    let first = e.trim().lines().next().unwrap_or("error");
    if first.chars().count() <= 110 {
        first.to_string()
    } else {
        format!("{}\u{2026}", first.chars().take(109).collect::<String>())
    }
}

/// Human text for a chord (used by banners and the overlay).
pub fn describe(chord: &Chord) -> String {
    chord_to_str(chord)
}
