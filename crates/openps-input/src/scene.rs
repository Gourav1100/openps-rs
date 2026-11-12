//! Scene-aware key maps: the layers a profile can have, the action labels a PS5 control can carry, and the link between the engine
//! (which knows what was pressed and uses the layer) and the scene detector (which sees the picture).

use serde::Serialize;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::Instant;

/// The key map layers of an advanced profile.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Layer {
    #[default]
    OnFoot,
    InVehicle,
    InMenu,
}

impl Layer {
    pub const ALL: [Layer; 3] = [Layer::OnFoot, Layer::InVehicle, Layer::InMenu];

    pub fn name(self) -> &'static str {
        match self {
            Layer::OnFoot => "on_foot",
            Layer::InVehicle => "in_vehicle",
            Layer::InMenu => "in_menu",
        }
    }

    pub fn parse(name: &str) -> Option<Layer> {
        Layer::ALL.into_iter().find(|l| l.name() == name)
    }

    /// The editor slot of this layer (1 and 2 are the primary and secondary keys of a plain profile).
    pub fn slot(self) -> u8 {
        match self {
            Layer::OnFoot => SLOT_ON_FOOT,
            Layer::InVehicle => SLOT_IN_VEHICLE,
            Layer::InMenu => SLOT_IN_MENU,
        }
    }

    pub fn from_slot(slot: u8) -> Option<Layer> {
        Layer::ALL.into_iter().find(|l| l.slot() == slot)
    }
}

pub const SLOT_ON_FOOT: u8 = 3;
pub const SLOT_IN_VEHICLE: u8 = 4;
pub const SLOT_IN_MENU: u8 = 5;

/// What a PS5 control means in the game, whatever keys it is bound to. The scene detector listens for these.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Action {
    EnterOrExitVehicle,
    ChangeCamera,
    PauseGame,
}

impl Action {
    pub const ALL: [Action; 3] = [Action::EnterOrExitVehicle, Action::ChangeCamera, Action::PauseGame];

    pub fn name(self) -> &'static str {
        match self {
            Action::EnterOrExitVehicle => "key_enter_or_exit_vehicle",
            Action::ChangeCamera => "key_change_camera",
            Action::PauseGame => "key_pause_game",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Action::EnterOrExitVehicle => "Enter or exit vehicle",
            Action::ChangeCamera => "Change camera",
            Action::PauseGame => "Pause game",
        }
    }

    pub fn parse(name: &str) -> Option<Action> {
        Action::ALL.into_iter().find(|a| a.name() == name)
    }
}

/// What the detector believes the game is showing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SceneContext {
    OnFoot,
    InVehicle,
    Menu,
    Cutscene,
}

impl SceneContext {
    pub const ALL: [SceneContext; 4] =
        [SceneContext::OnFoot, SceneContext::InVehicle, SceneContext::Menu, SceneContext::Cutscene];

    pub fn name(self) -> &'static str {
        match self {
            SceneContext::OnFoot => "on_foot",
            SceneContext::InVehicle => "in_vehicle",
            SceneContext::Menu => "menu",
            SceneContext::Cutscene => "cutscene",
        }
    }

    /// Cutscenes use the menu layer: what is pressed there is "skip" and "continue", not gameplay.
    pub fn layer(self) -> Layer {
        match self {
            SceneContext::OnFoot => Layer::OnFoot,
            SceneContext::InVehicle => Layer::InVehicle,
            SceneContext::Menu | SceneContext::Cutscene => Layer::InMenu,
        }
    }

    fn index(self) -> u8 {
        self as u8
    }
    fn from_index(i: u8) -> Option<SceneContext> {
        SceneContext::ALL.get(usize::from(i)).copied()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SceneView {
    FirstPerson,
    ThirdPerson,
}

impl SceneView {
    pub fn name(self) -> &'static str {
        match self {
            SceneView::FirstPerson => "first_person",
            SceneView::ThirdPerson => "third_person",
        }
    }
}

/// Where the detector is, for the UI.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DetectorState {
    #[default]
    Off,
    /// Switched on, the model is being prepared.
    Loading,
    Running,
    /// Switched on but it cannot run (the note says why).
    Unavailable,
    /// Switched on, waiting: the keyboard and mouse are not driving the console (a physical controller is), so there is nothing to follow.
    Idle,
}

impl DetectorState {
    pub fn name(self) -> &'static str {
        match self {
            DetectorState::Off => "off",
            DetectorState::Loading => "loading",
            DetectorState::Running => "running",
            DetectorState::Unavailable => "unavailable",
            DetectorState::Idle => "idle",
        }
    }
    fn index(self) -> u8 {
        self as u8
    }
    fn from_index(i: u8) -> DetectorState {
        [
            DetectorState::Off,
            DetectorState::Loading,
            DetectorState::Running,
            DetectorState::Unavailable,
            DetectorState::Idle,
        ]
        .get(usize::from(i))
        .copied()
        .unwrap_or_default()
    }
}

/// A copy of the detector's state, as the overlay shows it.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct SceneStatus {
    pub enabled: bool,
    pub state: &'static str,
    pub context: Option<&'static str>,
    pub view: Option<&'static str>,
    /// 0 to 100.
    pub confidence: u8,
    /// The layer the key map is using because of it.
    pub layer: &'static str,
    pub note: String,
}

const UNKNOWN: u8 = u8::MAX;
const ALL_LAYERS: u8 = 0b111;
const MAX_QUEUED: usize = 256;

/// Shared between the input engine (presses in, layer out) and the scene detector (picture in, scene out). All fields are cheap to read from
/// the 1 kHz input loop: atomics, plus one short lock for the actions.
pub struct SceneLink {
    epoch: Instant,
    enabled: AtomicBool,
    /// The keyboard and mouse are driving the console. The scene only matters then; with a physical controller the detector sleeps.
    active: AtomicBool,
    state: AtomicU8,
    context: AtomicU8,
    view: AtomicU8,
    confidence: AtomicU32,
    note: Mutex<String>,
    actions: Mutex<VecDeque<(f64, Action)>>,
    labeled: AtomicU8,
    /// Bit per [`Layer`] the profile uses (all three until told otherwise).
    layers: AtomicU8,
}

impl Default for SceneLink {
    fn default() -> Self {
        Self::new()
    }
}

impl SceneLink {
    pub fn new() -> Self {
        Self {
            epoch: Instant::now(),
            enabled: AtomicBool::new(false),
            active: AtomicBool::new(true),
            state: AtomicU8::new(DetectorState::Off.index()),
            context: AtomicU8::new(UNKNOWN),
            view: AtomicU8::new(UNKNOWN),
            confidence: AtomicU32::new(0),
            note: Mutex::new(String::new()),
            actions: Mutex::new(VecDeque::new()),
            labeled: AtomicU8::new(0),
            layers: AtomicU8::new(ALL_LAYERS),
        }
    }

    /// Seconds on the clock both sides stamp their data with.
    pub fn now(&self) -> f64 {
        self.epoch.elapsed().as_secs_f64()
    }

    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// Switched on and the keyboard and mouse are in use: the only time the detector looks at pictures.
    pub fn running(&self) -> bool {
        self.enabled() && self.active.load(Ordering::Relaxed)
    }

    /// Tell the detector whether the keyboard and mouse are driving the console. While they are not (a physical controller is), it waits and
    /// forgets the scene; it starts again when they are.
    pub fn set_active(&self, on: bool) {
        if self.active.swap(on, Ordering::Relaxed) == on || !self.enabled() {
            return;
        }
        self.actions.lock().unwrap_or_else(PoisonError::into_inner).clear();
        self.clear_scene();
        if !on {
            self.publish_state(DetectorState::Idle, "");
        } else if DetectorState::from_index(self.state.load(Ordering::Relaxed)) == DetectorState::Idle {
            self.publish_state(DetectorState::Loading, "");
        }
    }

    /// Switch the detector on or off. Switching it off forgets what it saw: the layer goes back to `on_foot`.
    pub fn set_enabled(&self, on: bool) {
        if self.enabled.swap(on, Ordering::Relaxed) == on {
            return;
        }
        self.actions.lock().unwrap_or_else(PoisonError::into_inner).clear();
        let state = match (on, self.active.load(Ordering::Relaxed)) {
            (false, _) => DetectorState::Off,
            (true, true) => DetectorState::Loading,
            (true, false) => DetectorState::Idle,
        };
        self.publish_state(state, "");
        self.clear_scene();
    }

    pub fn publish_state(&self, state: DetectorState, note: &str) {
        self.state.store(state.index(), Ordering::Relaxed);
        *self.note.lock().unwrap_or_else(PoisonError::into_inner) = note.to_string();
    }

    pub fn publish_scene(&self, context: SceneContext, view: Option<SceneView>, confidence: f32) {
        self.context.store(context.index(), Ordering::Relaxed);
        self.view.store(view.map_or(UNKNOWN, |v| v as u8), Ordering::Relaxed);
        self.confidence.store(confidence.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    pub fn clear_scene(&self) {
        self.context.store(UNKNOWN, Ordering::Relaxed);
        self.view.store(UNKNOWN, Ordering::Relaxed);
        self.confidence.store(0, Ordering::Relaxed);
    }

    pub fn context(&self) -> Option<SceneContext> {
        SceneContext::from_index(self.context.load(Ordering::Relaxed))
    }

    /// Which layers the profile uses. A layer that is off is never chosen and its scenes are never reported (see
    /// [`SceneLink::allowed_contexts`]); with none given all three are used.
    pub fn set_layers(&self, layers: impl IntoIterator<Item = Layer>) {
        let mask = layers.into_iter().fold(0u8, |m, l| m | (1 << l as u8));
        self.layers.store(if mask == 0 { ALL_LAYERS } else { mask }, Ordering::Relaxed);
    }

    pub fn layer_enabled(&self, layer: Layer) -> bool {
        self.layers.load(Ordering::Relaxed) & (1 << layer as u8) != 0
    }

    /// The scenes the detector may report: those whose layer is in use.
    pub fn allowed_contexts(&self) -> [bool; 4] {
        SceneContext::ALL.map(|c| self.layer_enabled(c.layer()))
    }

    /// The layer an advanced key map should use right now: the detected scene's while detection is on, otherwise `on_foot`; a layer that
    /// is off is replaced by the first one in use (on foot, in a vehicle, in a menu).
    pub fn layer(&self) -> Layer {
        let wanted =
            if self.running() { self.context().map_or(Layer::OnFoot, SceneContext::layer) } else { Layer::OnFoot };
        if self.layer_enabled(wanted) {
            return wanted;
        }
        Layer::ALL.into_iter().find(|l| self.layer_enabled(*l)).unwrap_or(Layer::OnFoot)
    }

    /// Which actions some control carries right now (the key map in use is advanced and live). The detector only waits for a press of an
    /// action that can happen.
    pub fn set_labeled(&self, actions: impl IntoIterator<Item = Action>) {
        let mask =
            actions.into_iter().fold(0u8, |m, a| m | (1 << Action::ALL.iter().position(|x| *x == a).unwrap_or(0)));
        self.labeled.store(mask, Ordering::Relaxed);
    }

    pub fn is_labeled(&self, action: Action) -> bool {
        let bit = Action::ALL.iter().position(|x| *x == action).unwrap_or(0);
        self.labeled.load(Ordering::Relaxed) & (1 << bit) != 0
    }

    /// An action happened (a labeled control was pressed). Ignored unless the detector is running.
    pub fn push_action(&self, action: Action) {
        if !self.running() {
            return;
        }
        let t = self.now();
        let mut q = self.actions.lock().unwrap_or_else(PoisonError::into_inner);
        if q.len() >= MAX_QUEUED {
            q.pop_front();
        }
        q.push_back((t, action));
    }

    /// Everything pushed since the last call, oldest first.
    pub fn drain_actions(&self) -> Vec<(f64, Action)> {
        self.actions.lock().unwrap_or_else(PoisonError::into_inner).drain(..).collect()
    }

    pub fn status(&self) -> SceneStatus {
        let view = match self.view.load(Ordering::Relaxed) {
            0 => Some(SceneView::FirstPerson.name()),
            1 => Some(SceneView::ThirdPerson.name()),
            _ => None,
        };
        SceneStatus {
            enabled: self.enabled(),
            state: DetectorState::from_index(self.state.load(Ordering::Relaxed)).name(),
            context: self.context().map(SceneContext::name),
            view,
            confidence: (f32::from_bits(self.confidence.load(Ordering::Relaxed)) * 100.0).round() as u8,
            layer: self.layer().name(),
            note: self.note.lock().unwrap_or_else(PoisonError::into_inner).clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        for l in Layer::ALL {
            assert_eq!(Layer::parse(l.name()), Some(l));
            assert_eq!(Layer::from_slot(l.slot()), Some(l));
        }
        for a in Action::ALL {
            assert_eq!(Action::parse(a.name()), Some(a));
            assert!(a.name().starts_with("key_"));
        }
        assert_eq!(Layer::from_slot(1), None);
        assert_eq!(Layer::from_slot(2), None);
        assert_eq!(Action::parse("key_jump"), None);
    }

    #[test]
    fn cutscenes_use_the_menu_layer() {
        assert_eq!(SceneContext::OnFoot.layer(), Layer::OnFoot);
        assert_eq!(SceneContext::InVehicle.layer(), Layer::InVehicle);
        assert_eq!(SceneContext::Menu.layer(), Layer::InMenu);
        assert_eq!(SceneContext::Cutscene.layer(), Layer::InMenu);
    }

    #[test]
    fn the_layer_is_on_foot_until_detection_is_on_and_has_seen_something() {
        let link = SceneLink::new();
        link.publish_scene(SceneContext::InVehicle, None, 0.9);
        assert_eq!(link.layer(), Layer::OnFoot, "detection is off");
        link.set_enabled(true);
        assert_eq!(link.layer(), Layer::OnFoot, "nothing seen yet (switching on forgets the old scene)");
        link.publish_scene(SceneContext::InVehicle, Some(SceneView::FirstPerson), 0.9);
        assert_eq!(link.layer(), Layer::InVehicle);
        link.set_enabled(false);
        assert_eq!(link.layer(), Layer::OnFoot);
        assert_eq!(link.status().context, None);
    }

    #[test]
    fn a_layer_that_is_off_is_replaced_by_the_first_one_in_use() {
        let link = SceneLink::new();
        link.set_enabled(true);
        link.publish_scene(SceneContext::InVehicle, None, 0.9);
        assert_eq!(link.allowed_contexts(), [true; 4]);
        link.set_layers([Layer::OnFoot, Layer::InMenu]);
        assert_eq!(link.layer(), Layer::OnFoot, "no vehicle layer: the vehicle keys are never used");
        assert_eq!(link.allowed_contexts(), [true, false, true, true]);
        link.publish_scene(SceneContext::Cutscene, None, 0.9);
        assert_eq!(link.layer(), Layer::InMenu);
        link.set_layers([Layer::InVehicle, Layer::InMenu]);
        assert_eq!(link.layer(), Layer::InMenu);
        link.publish_scene(SceneContext::OnFoot, None, 0.9);
        assert_eq!(link.layer(), Layer::InVehicle, "no on-foot layer: the first one in use stands in");
        link.set_enabled(false);
        assert_eq!(link.layer(), Layer::InVehicle, "also while detection is off");
        link.set_layers([]);
        assert_eq!(link.allowed_contexts(), [true; 4], "an empty set means all three");
    }

    #[test]
    fn actions_are_kept_in_order_only_while_detection_is_on() {
        let link = SceneLink::new();
        link.push_action(Action::PauseGame);
        assert!(link.drain_actions().is_empty());
        link.set_enabled(true);
        link.push_action(Action::ChangeCamera);
        link.push_action(Action::EnterOrExitVehicle);
        let got = link.drain_actions();
        assert_eq!(got.iter().map(|(_, a)| *a).collect::<Vec<_>>(), [Action::ChangeCamera, Action::EnterOrExitVehicle]);
        assert!(got[0].0 <= got[1].0);
        assert!(link.drain_actions().is_empty());
    }

    #[test]
    fn the_queue_is_bounded() {
        let link = SceneLink::new();
        link.set_enabled(true);
        for _ in 0..1000 {
            link.push_action(Action::PauseGame);
        }
        assert_eq!(link.drain_actions().len(), MAX_QUEUED);
    }

    #[test]
    fn it_waits_while_a_controller_drives_and_starts_again_with_the_keyboard() {
        let link = SceneLink::new();
        link.set_enabled(true);
        link.publish_scene(SceneContext::InVehicle, None, 0.9);
        assert!(link.running());
        link.set_active(false);
        let s = link.status();
        assert_eq!((s.enabled, s.state, s.context, s.layer), (true, "idle", None, "on_foot"));
        assert!(!link.running());
        link.push_action(Action::PauseGame);
        assert!(link.drain_actions().is_empty(), "nothing is pressed through the key map while a controller drives");
        link.set_active(false); // no change: nothing is lost or restarted
        assert_eq!(link.status().state, "idle");
        link.set_active(true);
        assert_eq!(link.status().state, "loading");
        assert!(link.running());
        // switching on while a controller drives starts idle
        link.set_enabled(false);
        link.set_active(false);
        link.set_enabled(true);
        assert_eq!(link.status().state, "idle");
    }

    #[test]
    fn status_reports_what_the_ui_needs() {
        let link = SceneLink::new();
        assert_eq!(link.status().state, "off");
        link.set_enabled(true);
        assert_eq!(link.status().state, "loading");
        link.publish_state(DetectorState::Running, "");
        link.publish_scene(SceneContext::Cutscene, Some(SceneView::ThirdPerson), 0.876);
        let s = link.status();
        assert_eq!((s.context, s.view, s.confidence, s.layer), (Some("cutscene"), Some("third_person"), 88, "in_menu"));
        link.publish_state(DetectorState::Unavailable, "the model is missing");
        assert_eq!(link.status().note, "the model is missing");
    }
}
