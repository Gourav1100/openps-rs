//! Keyboard and mouse for the stream window (macOS): the web view forwards key and button events while a session is
//! connected, the mouse is read as relative movement from CoreGraphics, and the Con2PC engine turns it all into pad
//! state for the console. No Accessibility permission is needed: nothing observes other applications.

use objc2::msg_send;
use objc2::runtime::{AnyClass, AnyObject};
use openps_core::events::AppEvent;
use openps_core::{Core, SessionState};
use openps_input::engine::{Hotkey, UiEvent};
use openps_input::overlay::OverlayState;
use openps_input::state::ControllerState;
use openps_macinput::cursor::MacCursor;
use openps_macinput::{InputHub, InputRunner, Output, RunnerConfig};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use tauri::{AppHandle, Emitter, Manager};

pub struct InputState {
    pub hub: Arc<InputHub>,
    /// Right-stick values sent to the console, timestamped: the renderer's camera compensation integrates them.
    look: openps_metal::look::SharedLook,
    runner: Mutex<Option<InputRunner>>,
    config_path: PathBuf,
}

impl InputState {
    pub fn new(data_dir: &Path, look: openps_metal::look::SharedLook) -> Self {
        Self {
            hub: Arc::new(InputHub::new()),
            look,
            runner: Mutex::new(None),
            config_path: data_dir.join("input.toml"),
        }
    }
}

struct AppOutput {
    app: AppHandle,
    look: openps_metal::look::SharedLook,
}

/// The on-screen PS button. It is merged into whatever the key map sends, so the two never fight over the pad state.
static PS_HELD: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// The pad state last produced by the key map (demo QA harness only).
#[cfg(feature = "demo")]
pub fn last_pad_debug() -> String {
    format!("{:?}", LAST_PAD.lock().unwrap_or_else(PoisonError::into_inner))
}

static LAST_PAD: Mutex<Option<ControllerState>> = Mutex::new(None);

/// Forget the on-screen PS button and the last pad: nothing may stay "held" across a focus loss or a session.
#[cfg(test)]
static TEST_GUARD: Mutex<()> = Mutex::new(()); // the tests below share the statics above

fn clear_pad_memory() {
    PS_HELD.store(false, std::sync::atomic::Ordering::Relaxed);
    *LAST_PAD.lock().unwrap_or_else(PoisonError::into_inner) = None;
}

/// The window's focus reaching the hub. The hub ignores keys and the wheel until it has been told the window is focused, so this
/// must run at startup (the window may already be key) and on every change. Losing focus also wakes the engine, so the keys the hub
/// just released are seen at once.
fn apply_focus(hub: &InputHub, focused: bool) {
    hub.set_focused(focused);
    if !focused {
        clear_pad_memory();
        hub.send(openps_macinput::Msg::Wake);
    }
}

fn with_ps(mut state: ControllerState) -> ControllerState {
    if PS_HELD.load(std::sync::atomic::Ordering::Relaxed) {
        state.buttons.insert(openps_input::state::Button::PS);
    }
    state
}

impl Output for AppOutput {
    fn pad(&mut self, state: &ControllerState) {
        self.look.push(openps_metal::now(), state.right_x as f32, state.right_y as f32);
        *LAST_PAD.lock().unwrap_or_else(PoisonError::into_inner) = Some(state.clone());
        if let Some(core) = self.app.try_state::<Core>() {
            let _ = core.set_pad(with_ps(state.clone())); // NotConnected between sessions is expected
        }
    }
    fn overlay(&mut self, state: &OverlayState) {
        // the front end shows the mapping status; the full overlay editor is not built yet
        if let Ok(v) = serde_json::to_value(state) {
            let _ = self.app.emit(crate::EVENT, serde_json::json!({ "type": "input", "overlay": v }));
        }
    }
    fn exit(&mut self, quit_app: bool) {
        // Runs on the input thread, which `shutdown` joins: hand the work to another thread.
        let app = self.app.clone();
        std::thread::spawn(move || {
            if quit_app {
                shutdown(&app); // give the mouse back first
                if let Some(core) = app.try_state::<Core>() {
                    core.shutdown(); // tells the console goodbye
                }
                app.exit(0);
            } else if let Some(core) = app.try_state::<Core>() {
                let _ = core.disconnect(); // the session event then stops input as usual
            }
        });
    }
    fn beep(&mut self, sound: &str) {
        // short system sounds ("Pop", "Basso", "Tink") for toggles
        unsafe {
            let Some(class) = AnyClass::get(c"NSSound") else { return };
            let name = crate::video::nsstring(sound);
            let snd: *mut AnyObject = msg_send![class, soundNamed: &*name];
            if !snd.is_null() {
                let _: bool = msg_send![snd, play];
            }
        }
    }
}

/// A broken key map in plain words: which file, which line if known, and what happens instead. The parser's own message
/// (with code frames) goes to the log, not to the player.
pub fn friendly_config_error(raw: &str) -> String {
    let line = raw
        .split("line ")
        .nth(1)
        .and_then(|r| r.split(|c: char| !c.is_ascii_digit()).next())
        .and_then(|n| n.parse::<u32>().ok());
    match line {
        Some(n) => format!("Your key map (input.toml) has a mistake on line {n}, so the default keys are in use."),
        None => "Your key map (input.toml) could not be read, so the default keys are in use.".into(),
    }
}

/// The user's key map: defaults, then `input.toml` in the data folder on top. A broken file is reported and ignored,
/// so a typo cannot lock the user out of the stream.
fn load_config(path: &Path, app: &AppHandle) -> openps_input::Config {
    let user = std::fs::read_to_string(path).ok();
    match openps_input::load_layers(user.as_deref(), true) {
        Ok(c) => c,
        Err(e) => {
            log::error!("{}: {e}; using the default key map", path.display());
            let _ = app.emit(
                crate::EVENT,
                AppEvent::Control { kind: "input_config_error".into(), detail: friendly_config_error(&e) },
            );
            openps_input::load_defaults().expect("the shipped key map is valid")
        }
    }
}

fn start(app: &AppHandle) {
    let Some(st) = app.try_state::<InputState>() else { return };
    let mut slot = st.runner.lock().unwrap_or_else(PoisonError::into_inner);
    if slot.is_some() {
        return;
    }
    update_bounds(app);
    let cfg = RunnerConfig {
        config: load_config(&st.config_path, app),
        config_path: Some(st.config_path.clone()),
        enabled: false, // mapping starts off (the specified default); `start_enabled = true` in input.toml turns it on
    };
    match InputRunner::start(
        st.hub.clone(),
        cfg,
        Box::new(MacCursor::new()),
        Box::new(AppOutput { app: app.clone(), look: st.look.clone() }),
    ) {
        Ok(r) => *slot = Some(r),
        Err(e) => {
            log::error!("keyboard/mouse input did not start: {e}");
            let _ = app.emit(crate::EVENT, AppEvent::Control { kind: "input_error".into(), detail: e });
        }
    }
}

fn stop(app: &AppHandle) {
    let Some(st) = app.try_state::<InputState>() else { return };
    let r = st.runner.lock().unwrap_or_else(PoisonError::into_inner).take();
    clear_pad_memory();
    if let Some(r) = r {
        r.stop(); // releases the cursor and sends a neutral pad
    }
}

/// Hook for every core event: input runs exactly while a session is connected.
pub fn on_event(app: &AppHandle, ev: &AppEvent) {
    if let AppEvent::Session(s) = ev {
        match s {
            SessionState::Connected { .. } => start(app),
            SessionState::Closed { .. } | SessionState::Failed { .. } | SessionState::Idle => stop(app),
            _ => {}
        }
    }
}

/// Stop input immediately (app exit).
pub fn shutdown(app: &AppHandle) {
    stop(app);
}

/// Where the window is, in screen points: the engine centres the cursor in it.
pub fn update_bounds(app: &AppHandle) {
    let (Some(st), Some(w)) = (app.try_state::<InputState>(), app.get_webview_window("main")) else { return };
    let scale = w.scale_factor().unwrap_or(1.0);
    if let (Ok(pos), Ok(size)) = (w.inner_position(), w.inner_size()) {
        st.hub.set_bounds(Some((
            f64::from(pos.x) / scale,
            f64::from(pos.y) / scale,
            f64::from(size.width) / scale,
            f64::from(size.height) / scale,
        )));
    }
}

pub fn set_focus(app: &AppHandle, focused: bool) {
    if let Some(st) = app.try_state::<InputState>() {
        apply_focus(&st.hub, focused);
    }
    if focused {
        update_bounds(app);
    }
}

// ---- commands ----------------------------------------------------------------------------------------------------------------
#[tauri::command]
pub fn input_key(st: tauri::State<'_, InputState>, code: String, down: bool, repeat: Option<bool>) {
    if code.len() <= 32 {
        st.hub.key_event(&code, down, repeat.unwrap_or(false));
    }
}

/// Press (`down`) or release the console's PS button. Press and release are separate so a long press works too.
#[tauri::command]
pub fn ps_button(core: tauri::State<'_, Core>, down: bool) -> Result<(), String> {
    PS_HELD.store(down, std::sync::atomic::Ordering::Relaxed);
    log::info!("PS button {}", if down { "pressed" } else { "released" });
    let base = LAST_PAD.lock().unwrap_or_else(PoisonError::into_inner).clone().unwrap_or_default();
    core.set_pad(with_ps(base)).map_err(|e| e.to_string())
}

/// The key-map profile names (for the per-game choice), whether or not a session is running.
#[tauri::command]
pub fn input_profile_names(st: tauri::State<'_, InputState>) -> Vec<String> {
    let user = std::fs::read_to_string(&st.config_path).ok();
    openps_input::load_layers(user.as_deref(), true)
        .or_else(|_| openps_input::load_defaults())
        .map(|c| c.profile_names())
        .unwrap_or_default()
}

/// The key map profiles as `{id, name}` (the base profile has no id: a game that has no choice uses it). Games remember a profile by id,
/// so renaming one never breaks the link.
#[tauri::command]
pub fn input_profiles(st: tauri::State<'_, InputState>) -> Vec<serde_json::Value> {
    let user = std::fs::read_to_string(&st.config_path).ok();
    let Ok(cfg) = openps_input::load_layers(user.as_deref(), true).or_else(|_| openps_input::load_defaults()) else {
        return vec![];
    };
    cfg.profile_names()
        .into_iter()
        .filter_map(|n| cfg.id_of(&n).map(|id| serde_json::json!({ "id": id, "name": n })))
        .collect()
}

/// Games that were saved with a profile *name* (before ids existed) are moved to the profile's id. Anything that matches no profile is kept
/// as it is (the tile then shows that the key map is gone).
#[tauri::command]
pub fn migrate_game_maps(st: tauri::State<'_, InputState>, dir: tauri::State<'_, crate::DataDir>) {
    let (st, data) = (&*st, dir.0.as_path());
    let user = std::fs::read_to_string(&st.config_path).ok();
    let Ok(cfg) = openps_input::load_layers(user.as_deref(), true).or_else(|_| openps_input::load_defaults()) else {
        return;
    };
    migrate_with(&cfg, data);
}

fn migrate_with(cfg: &openps_input::Config, data: &std::path::Path) {
    for (game, value) in crate::gamemaps::load(data) {
        if openps_input::config::valid_profile_id(&value) {
            continue;
        }
        let name = openps_input::config::snake_name(&value);
        if let Some(id) = cfg.id_of(&name) {
            let _ = crate::gamemaps::set(data, &game, Some(&id));
        }
    }
}

/// Switch the running key map to the named profile (the same as the profile hotkeys). Errors if the profile does not exist or no
/// session is running.
#[tauri::command]
pub fn input_select_profile(st: tauri::State<'_, InputState>, name: String) -> Result<(), String> {
    let user = std::fs::read_to_string(&st.config_path).ok();
    let names = openps_input::load_layers(user.as_deref(), true)
        .or_else(|_| openps_input::load_defaults())
        .map(|c| c.profile_names())
        .map_err(|e| e.to_string())?;
    if name.len() > 256 {
        return Err("that key map does not exist".into());
    }
    let idx = names.iter().position(|n| *n == name).ok_or("that key map does not exist")?;
    match st.runner.lock().unwrap_or_else(PoisonError::into_inner).as_ref() {
        Some(r) => {
            r.hotkey(Hotkey::Slot(idx));
            Ok(())
        }
        None => Err("keyboard and mouse control is not running yet".into()),
    }
}

#[tauri::command]
pub fn input_button(st: tauri::State<'_, InputState>, button: i32, down: bool) {
    st.hub.button_event(button, down);
}

#[tauri::command]
pub fn input_wheel(st: tauri::State<'_, InputState>, dx: f64, dy: f64) {
    st.hub.wheel(dx, dy);
}

/// Toggle mapping on/off (the HUD button; the F10 hotkey does the same).
#[tauri::command]
pub fn input_toggle(st: tauri::State<'_, InputState>) {
    if let Some(r) = st.runner.lock().unwrap_or_else(PoisonError::into_inner).as_ref() {
        r.hotkey(Hotkey::Toggle);
    }
}

/// Show or hide the key-map editor (same as the overlay hotkey).
#[tauri::command]
pub fn input_overlay(st: tauri::State<'_, InputState>) {
    if let Some(r) = st.runner.lock().unwrap_or_else(PoisonError::into_inner).as_ref() {
        r.hotkey(Hotkey::Overlay);
    }
}

/// An event from the overlay UI (same JSON the engine's UI protocol defines; anything malformed is ignored).
#[tauri::command]
pub fn input_ui(st: tauri::State<'_, InputState>, event: serde_json::Value) {
    if let (Some(e), Some(r)) =
        (UiEvent::from_json(&event), st.runner.lock().unwrap_or_else(PoisonError::into_inner).as_ref())
    {
        r.ui_event(e);
    }
}

#[derive(serde::Serialize)]
pub struct KeyMapFile {
    pub path: String,
    pub exists: bool,
}

/// Where the user's key map lives and whether they have customised it.
#[tauri::command]
pub fn input_config_info(st: tauri::State<'_, InputState>) -> KeyMapFile {
    KeyMapFile { path: st.config_path.display().to_string(), exists: st.config_path.exists() }
}

/// Show the key map file in Finder (or the folder that will hold it). Fixed arguments, our own path: no user text reaches a shell.
#[tauri::command]
pub fn input_config_reveal(st: tauri::State<'_, InputState>) -> Result<(), String> {
    let mut cmd = std::process::Command::new("/usr/bin/open");
    // absolute, so a path starting with '-' can never be read as an option
    let path = std::path::absolute(&st.config_path).unwrap_or_else(|_| st.config_path.clone());
    if path.exists() {
        cmd.arg("-R").arg(&path);
    } else if let Some(dir) = path.parent() {
        cmd.arg(dir);
    }
    cmd.stdin(std::process::Stdio::null());
    let mut child = cmd.spawn().map_err(|e| format!("could not open Finder: {e}"))?;
    std::thread::spawn(move || {
        let _ = child.wait(); // no zombie per click
    });
    Ok(())
}

/// Forget the user's key map: the shipped defaults apply from the next session.
#[tauri::command]
pub fn input_config_reset(st: tauri::State<'_, InputState>) -> Result<bool, String> {
    // keep the old file as input.toml.bak (replacing an older backup) rather than deleting the user's work
    match std::fs::rename(&st.config_path, st.config_path.with_extension("toml.bak")) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(format!("could not reset the key map: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::friendly_config_error;

    #[test]
    fn config_errors_are_plain_and_name_the_line() {
        let raw = "user config: TOML parse error at line 3, column 5\n  |\n3 | not toml at all\n  |     ^\nexpected `.`, `=`\n";
        let m = friendly_config_error(raw);
        assert_eq!(m, "Your key map (input.toml) has a mistake on line 3, so the default keys are in use.");
        assert!(!m.contains('|') && !m.contains("TOML"));
        assert!(friendly_config_error("something else").contains("could not be read"));
        assert!(friendly_config_error("line x").contains("could not be read"));
    }
}

#[cfg(test)]
mod ps_tests {
    use super::*;
    use openps_input::state::Button;

    #[test]
    fn ps_button_merges_into_pad_state_without_touching_the_rest() {
        let _g = TEST_GUARD.lock().unwrap_or_else(PoisonError::into_inner);
        let mut base = ControllerState::default();
        base.buttons.insert(Button::CROSS);
        base.left_x = 0.5;
        PS_HELD.store(false, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(with_ps(base.clone()), base);
        PS_HELD.store(true, std::sync::atomic::Ordering::Relaxed);
        let held = with_ps(base.clone());
        PS_HELD.store(false, std::sync::atomic::Ordering::Relaxed);
        assert!(held.buttons.contains(&Button::PS) && held.buttons.contains(&Button::CROSS));
        assert_eq!(held.left_x, 0.5);
    }
}

#[cfg(test)]
mod focus_tests {
    use super::*;
    use openps_input::state::Button;

    #[test]
    fn the_hub_ignores_keys_until_the_shell_reports_focus_and_blur_clears_the_ps_button() {
        let _g = TEST_GUARD.lock().unwrap_or_else(PoisonError::into_inner);
        let hub = InputHub::new();
        hub.key_event("KeyW", true, false);
        assert!(hub.pressed().is_empty(), "an unfocused hub must ignore key-downs");
        apply_focus(&hub, true);
        hub.key_event("KeyW", true, false);
        assert_eq!(hub.pressed().len(), 1);
        PS_HELD.store(true, std::sync::atomic::Ordering::Relaxed);
        *LAST_PAD.lock().unwrap() = Some(ControllerState::default());
        apply_focus(&hub, false);
        assert!(hub.pressed().is_empty());
        assert!(!with_ps(ControllerState::default()).buttons.contains(&Button::PS));
        assert!(LAST_PAD.lock().unwrap().is_none());
    }

    #[test]
    fn a_session_ending_clears_the_ps_button() {
        let _g = TEST_GUARD.lock().unwrap_or_else(PoisonError::into_inner);
        PS_HELD.store(true, std::sync::atomic::Ordering::Relaxed);
        clear_pad_memory();
        assert!(!with_ps(ControllerState::default()).buttons.contains(&Button::PS));
    }
}

#[cfg(test)]
mod migrate_tests {
    #[test]
    fn names_become_ids_and_the_rest_is_left_alone() {
        let cfg = openps_input::load_defaults().unwrap();
        let d = tempfile::tempdir().unwrap();
        crate::gamemaps::set(d.path(), "PPSA1", Some("AssassinsCreed")).unwrap(); // an old name, in its old spelling
        crate::gamemaps::set(d.path(), "PPSA2", Some("racing")).unwrap();
        crate::gamemaps::set(d.path(), "PPSA3", Some("p_3a1f0c02")).unwrap(); // already an id
        crate::gamemaps::set(d.path(), "PPSA4", Some("long_gone")).unwrap(); // a profile that no longer exists
        super::migrate_with(&cfg, d.path());
        let m = crate::gamemaps::load(d.path());
        assert_eq!(m["PPSA1"], "p_3a1f0c02");
        assert_eq!(m["PPSA2"], "p_3a1f0c01");
        assert_eq!(m["PPSA3"], "p_3a1f0c02");
        assert_eq!(m["PPSA4"], "long_gone");
    }
}
