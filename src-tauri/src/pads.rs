//! Physical DualSense / DualShock 4 pads: events from the macOS GameController source go through the arbiter (who drives the
//! console right now) to the session. The Controls button hands the console to the key map and back.

use openps_core::events::MediaSink;
use openps_core::Core;
use openps_gamepad::hid::HidOut;
use openps_gamepad::mac::MacPads;
use openps_gamepad::map::DEFAULT_DEADZONE;
use openps_gamepad::mode::{Arbiter, PadCmd};
use openps_stream::padblob::PadOutput;
use serde::Serialize;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use tauri::{AppHandle, Emitter, Manager};

static APP: OnceLock<AppHandle> = OnceLock::new();

pub struct PadState {
    arbiter: Mutex<Arbiter>,
    hid: HidOut,
    look: openps_metal::look::SharedLook,
    _source: Mutex<Option<MacPads>>,
    deadzone: Mutex<f64>,
}

impl PadState {
    pub fn new(look: openps_metal::look::SharedLook) -> Self {
        Self {
            arbiter: Mutex::new(Arbiter::default()),
            hid: HidOut::start(),
            look,
            _source: Mutex::new(None),
            deadzone: Mutex::new(DEFAULT_DEADZONE),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PadStatus {
    slot: usize,
    kind: &'static str,
    name: String,
    battery_level: Option<f32>,
    charging: Option<bool>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ControllerStatus {
    controller_active: bool,
    keyboard_mode: bool,
    deadzone: f64,
    pads: Vec<PadStatus>,
}

/// Start listening for pads. Must run on the main thread.
pub fn init(app: &AppHandle) {
    let _ = APP.set(app.clone());
    let (source, rx) = MacPads::start();
    if let Some(st) = app.try_state::<PadState>() {
        *st._source.lock().unwrap_or_else(PoisonError::into_inner) = Some(source);
    }
    let app = app.clone();
    std::thread::Builder::new()
        .name("pads".into())
        .spawn(move || {
            while let Ok(ev) = rx.recv() {
                let Some(st) = app.try_state::<PadState>() else { continue };
                let changed = matches!(
                    &ev,
                    openps_gamepad::PadEvent::Connected { .. }
                        | openps_gamepad::PadEvent::Disconnected { .. }
                        | openps_gamepad::PadEvent::Battery { .. }
                );
                let cmds = st.arbiter.lock().unwrap_or_else(PoisonError::into_inner).on_event(ev);
                apply(&app, &cmds);
                sync_quiet(&app);
                if changed {
                    if let Err(e) = app.emit("openps://pads", ()) {
                        log::warn!("could not notify the UI about controller changes: {e}");
                    }
                }
            }
        })
        .expect("pad thread");
}

fn with<R>(app: &AppHandle, f: impl FnOnce(&mut Arbiter) -> R) -> Option<R> {
    let st = app.try_state::<PadState>()?;
    let mut a = st.arbiter.lock().unwrap_or_else(PoisonError::into_inner);
    Some(f(&mut a))
}

fn apply(app: &AppHandle, cmds: &[PadCmd]) {
    if cmds.is_empty() {
        return;
    }
    let Some(core) = app.try_state::<Core>() else { return };
    let st = app.try_state::<PadState>();
    for c in cmds {
        // NotConnected between sessions is expected
        let _ = match c {
            PadCmd::Connected { slot, connected } => core.set_pad_connected(*slot, *connected),
            PadCmd::State { slot, state } => {
                log_state(*slot, state);
                if *slot == 0 {
                    if let Some(st) = &st {
                        st.look.push(openps_metal::now(), state.right_x as f32, state.right_y as f32);
                    }
                }
                core.set_pad_slot(*slot, state.clone())
            }
            PadCmd::Sensors { slot, sensors } => {
                core.set_pad_sensors(*slot, sensors.gyro, sensors.accel, sensors.orientation)
            }
        };
    }
}

/// True while the physical pad owns the console, so the key map must not write to it.
pub fn controller_active(app: &AppHandle) -> bool {
    with(app, |a| a.controller_active()).unwrap_or(false)
}

pub fn set_keyboard_mode(app: &AppHandle, on: bool) {
    if let Some(cmds) = with(app, |a| a.set_keyboard_mode(on)) {
        apply(app, &cmds);
    }
    sync_quiet(app);
}

pub fn set_focus(app: &AppHandle, focused: bool) {
    if let Some(cmds) = with(app, |a| a.set_focused(focused)) {
        apply(app, &cmds);
    }
    sync_quiet(app);
}

#[tauri::command]
pub fn controller_status(app: AppHandle) -> Result<ControllerStatus, String> {
    let st = app.try_state::<PadState>().ok_or("controller support is not initialized")?;
    let a = st.arbiter.lock().unwrap_or_else(PoisonError::into_inner);
    let pads = a
        .pads()
        .into_iter()
        .map(|p| {
            let (battery_level, charging) =
                p.battery.map_or((None, None), |(level, charging)| (Some(level.clamp(0.0, 1.0)), Some(charging)));
            PadStatus {
                slot: p.slot,
                kind: match p.kind {
                    openps_gamepad::PadKind::DualSense => "DualSense",
                    openps_gamepad::PadKind::DualShock4 => "DualShock 4",
                },
                name: p.name,
                battery_level,
                charging,
            }
        })
        .collect();
    let deadzone = *st.deadzone.lock().unwrap_or_else(PoisonError::into_inner);
    Ok(ControllerStatus { controller_active: a.controller_active(), keyboard_mode: a.keyboard_mode(), deadzone, pads })
}

#[tauri::command]
pub fn controller_set_deadzone(app: AppHandle, deadzone: f64) -> Result<f64, String> {
    let deadzone = validate_deadzone(deadzone)?;
    let st = app.try_state::<PadState>().ok_or("controller support is not initialized")?;
    let source = st._source.lock().unwrap_or_else(PoisonError::into_inner);
    let source = source.as_ref().ok_or("controller source is not initialized")?;
    source.set_deadzone(deadzone);
    *st.deadzone.lock().unwrap_or_else(PoisonError::into_inner) = deadzone;
    Ok(deadzone)
}

fn validate_deadzone(deadzone: f64) -> Result<f64, String> {
    if !deadzone.is_finite() || !(0.0..=0.5).contains(&deadzone) {
        return Err("deadzone must be between 0 and 0.5".into());
    }
    Ok(deadzone)
}

/// Rumble and trigger effects only play while the pad is the one in play; the console's last request is kept and comes back with it.
fn sync_quiet(app: &AppHandle) {
    let Some(st) = app.try_state::<PadState>() else { return };
    let (live, ranks) = {
        let a = st.arbiter.lock().unwrap_or_else(PoisonError::into_inner);
        (a.is_live(), a.dualsense_ranks())
    };
    for (_, rank) in ranks {
        st.hid.quiet(rank, !live);
    }
}

/// The session ended: forget what the console asked of the pads.
pub fn session_ended(app: &AppHandle) {
    let Some(st) = app.try_state::<PadState>() else { return };
    let ranks = st.arbiter.lock().unwrap_or_else(PoisonError::into_inner).dualsense_ranks();
    for (_, rank) in ranks {
        st.hid.clear(rank);
    }
}

/// What the console asks of a pad (rumble, lightbar, adaptive triggers). Only a DualSense in play is touched.
fn output(out: PadOutput) {
    let Some(app) = APP.get() else { return };
    let Some(st) = app.try_state::<PadState>() else { return };
    let rank = st.arbiter.lock().unwrap_or_else(PoisonError::into_inner).dualsense_ranks();
    match &out {
        PadOutput::Intensity { .. } => {
            for (_, rank) in rank {
                st.hid.apply(rank, &out);
            }
        }
        PadOutput::Rumble { pad, .. } | PadOutput::Led { pad, .. } | PadOutput::AdaptiveTriggers { pad, .. } => {
            if let Some((_, rank)) = rank.into_iter().find(|(slot, _)| *slot == *pad as usize) {
                st.hid.apply(rank, &out);
            }
        }
    }
}

fn output_haptic(frame: [u8; 120]) {
    let Some(app) = APP.get() else { return };
    let Some(st) = app.try_state::<PadState>() else { return };
    let a = st.arbiter.lock().unwrap_or_else(PoisonError::into_inner);
    if !a.is_live() {
        return;
    }
    if let Some((_, rank)) = a.dualsense_ranks().into_iter().find(|(slot, _)| *slot == 0) {
        st.hid.haptic(rank, frame);
    }
}

/// Wraps the media sink: everything goes through unchanged, and the console's pad output also reaches the physical pad.
pub struct WithPads(pub Arc<dyn MediaSink>);

impl MediaSink for WithPads {
    fn stream_info(&self, info: &openps_takion::proto::StreamInfoPayload) {
        self.0.stream_info(info);
    }
    fn take_keyframe_request(&self) -> bool {
        self.0.take_keyframe_request()
    }
    fn video(&self, frame: openps_stream::video::Frame) {
        self.0.video(frame);
    }
    fn audio(&self, frames: Vec<openps_stream::audio::OpusFrame>) {
        self.0.audio(frames);
    }
    fn haptic(&self, frame: [u8; 120]) {
        output_haptic(frame);
        self.0.haptic(frame);
    }
    fn pad_output(&self, out: PadOutput) {
        output(out.clone());
        self.0.pad_output(out);
    }
}

/// A session just connected: mapping starts off, and the console has to be told about the pads again.
pub fn session_started(app: &AppHandle) {
    if let Some(cmds) = with(app, |a| {
        let mut c = a.set_keyboard_mode(false);
        c.extend(a.resync());
        c
    }) {
        apply(app, &cmds);
    }
    sync_quiet(app);
}

/// `OPENPS_PADLOG=1`: every pad state sent to the console is appended to `$TMPDIR/openps-pad.csv` (a spike aid, not a feature).
fn log_state(slot: usize, s: &openps_input::state::ControllerState) {
    use std::io::Write;
    static FILE: OnceLock<Option<Mutex<std::fs::File>>> = OnceLock::new();
    let f = FILE.get_or_init(|| {
        std::env::var_os("OPENPS_PADLOG")?;
        let path = std::env::temp_dir().join("openps-pad.csv");
        std::fs::File::create(path).ok().map(Mutex::new)
    });
    if let Some(f) = f {
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64());
        let _ = writeln!(
            f.lock().unwrap_or_else(PoisonError::into_inner),
            "{t:.4},{slot},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3}",
            s.left_x,
            s.left_y,
            s.right_x,
            s.right_y,
            s.l2,
            s.r2
        );
    }
}

#[cfg(test)]
mod tests {
    use super::validate_deadzone;

    #[test]
    fn deadzone_accepts_only_finite_values_in_the_supported_range() {
        assert_eq!(validate_deadzone(0.0), Ok(0.0));
        assert_eq!(validate_deadzone(0.08), Ok(0.08));
        assert_eq!(validate_deadzone(0.5), Ok(0.5));
        assert!(validate_deadzone(-0.01).is_err());
        assert!(validate_deadzone(0.51).is_err());
        assert!(validate_deadzone(f64::NAN).is_err());
        assert!(validate_deadzone(f64::INFINITY).is_err());
    }
}
