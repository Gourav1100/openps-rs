//! The engine's `Platform` over the hub (keys and buttons), a `Cursor` (mouse movement) and an `Output` (where the
//! pad state, the overlay state and feedback sounds go).

use crate::cursor::Cursor;
use crate::hub::InputHub;
use openps_input::engine::{Bounds, Platform};
use openps_input::keys::{Chord, Key};
use openps_input::mapping::InputSnapshot;
use openps_input::overlay::OverlayState;
use openps_input::state::ControllerState;
use std::sync::Arc;

pub trait Output: Send {
    fn pad(&mut self, state: &ControllerState);
    fn overlay(&mut self, state: &OverlayState);
    fn beep(&mut self, _sound: &str) {}
    /// F12 (`quit_app == false`, end the stream) or F11 (`true`, close the whole app).
    fn exit(&mut self, _quit_app: bool) {}
}

pub struct HubPlatform {
    hub: Arc<InputHub>,
    cursor: Box<dyn Cursor>,
    out: Box<dyn Output>,
}

impl HubPlatform {
    pub fn new(hub: Arc<InputHub>, cursor: Box<dyn Cursor>, out: Box<dyn Output>) -> Self {
        Self { hub, cursor, out }
    }
}

impl Platform for HubPlatform {
    // The target application is this one: "frontmost" means the stream window has focus.
    fn is_frontmost(&mut self, _app: &str) -> bool {
        self.hub.focused()
    }
    fn window_bounds(&mut self, _app: &str) -> Option<Bounds> {
        self.hub.bounds()
    }
    fn discard_pulses(&mut self) {
        self.hub.clear_pulses();
    }
    fn snapshot(&mut self) -> InputSnapshot {
        let (dx, dy) = self.cursor.delta();
        InputSnapshot::new(self.hub.take_pressed(), dx, dy)
    }
    // Input reaches us through the web view only while we are focused; there is no system-wide tap to configure.
    fn set_exclusive(&mut self, _keys: bool, _mouse: bool) {}
    fn set_hotkey_swallow(&mut self, _on: bool) {}
    fn set_hotkeys(&mut self, _chords: &[Chord]) {}
    fn cursor_engage(&mut self, center: Option<(f64, f64)>) {
        self.cursor.engage(center);
    }
    fn cursor_recenter(&mut self, center: (f64, f64)) {
        self.cursor.recenter(center);
    }
    fn cursor_release(&mut self) {
        self.cursor.release();
    }
    fn cursor_set_hide(&mut self, hide: bool) {
        self.cursor.set_hide(hide);
    }
    fn send_pad(&mut self, state: &ControllerState) {
        self.out.pad(state);
    }
    fn beep(&mut self, sound: &str, _force: bool) {
        self.out.beep(sound);
    }
    // There is no separate game process to kill: the "panic" hotkey just releases everything (the engine does that).
    fn force_quit(&mut self, _app: &str) -> Vec<u32> {
        vec![]
    }
    fn exit_requested(&mut self, quit_app: bool) {
        self.out.exit(quit_app);
    }
    fn overlay_push(&mut self, state: &OverlayState) {
        self.out.overlay(state);
    }
    fn start_key_log(&mut self) {
        self.hub.start_key_log();
    }
    fn stop_key_log(&mut self) {
        self.hub.stop_key_log();
    }
    fn drain_key_log(&mut self) -> Vec<(Key, bool)> {
        self.hub.drain_key_log()
    }
}
