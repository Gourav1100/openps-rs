//! macOS pads through Apple's GameController framework. The framework calls our handler on a private high-priority queue
//! whenever any control changes; the handler snapshots the pad and sends one `PadEvent` down a channel. Nothing polls.

use crate::map::{to_state, RawPad, DEFAULT_DEADZONE};
use crate::orient::Orient;
use crate::{PadEvent, PadKind, Sensors};
use block2::RcBlock;
use dispatch2::{DispatchQueue, DispatchQueueAttr, DispatchRetained};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::ClassType;
use objc2_foundation::{NSNotification, NSNotificationCenter, NSObjectProtocol};
use objc2_game_controller::{
    GCController, GCControllerButtonInput, GCControllerDidConnectNotification, GCControllerDidDisconnectNotification,
    GCDevice, GCDualSenseGamepad, GCDualShockGamepad, GCExtendedGamepad, GCMotion, GCPhysicalInputProfile,
    GCSystemGestureState,
};
use openps_input::state::Button;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

struct Entry {
    id: u32,
    ctrl: Retained<GCController>,
}

/// GameController objects may be used from any thread; the wrapper only exists to say so.
struct Ship<T>(T);
unsafe impl<T> Send for Ship<T> {}
unsafe impl<T> Sync for Ship<T> {}

struct Shared {
    tx: Sender<PadEvent>,
    pads: Mutex<Vec<Ship<Entry>>>,
    next_id: AtomicU32,
    queue: Ship<DispatchRetained<DispatchQueue>>,
    deadzone: Mutex<f64>,
}

pub struct MacPads {
    shared: Arc<Shared>,
    _observers: Ship<Vec<Retained<ProtocolObject<dyn NSObjectProtocol>>>>,
}

impl MacPads {
    /// Start listening. Call from the main thread; pads already connected are reported first.
    pub fn start() -> (Self, Receiver<PadEvent>) {
        let (tx, rx) = channel();
        let queue = DispatchQueue::new("openps.gamepad", DispatchQueueAttr::SERIAL);
        let shared = Arc::new(Shared {
            tx,
            pads: Mutex::new(vec![]),
            next_id: AtomicU32::new(1),
            queue: Ship(queue),
            deadzone: Mutex::new(DEFAULT_DEADZONE),
        });
        unsafe {
            GCController::setShouldMonitorBackgroundEvents(false);
            let center = NSNotificationCenter::defaultCenter();
            let s = shared.clone();
            let on_connect = RcBlock::new(move |n: NonNull<NSNotification>| {
                if let Some(c) = controller_of(n) {
                    attach(&s, c);
                }
            });
            let s = shared.clone();
            let on_disconnect = RcBlock::new(move |n: NonNull<NSNotification>| {
                if let Some(c) = controller_of(n) {
                    detach(&s, &c);
                }
            });
            let observers = vec![
                center.addObserverForName_object_queue_usingBlock(
                    Some(GCControllerDidConnectNotification),
                    None,
                    None,
                    &on_connect,
                ),
                center.addObserverForName_object_queue_usingBlock(
                    Some(GCControllerDidDisconnectNotification),
                    None,
                    None,
                    &on_disconnect,
                ),
            ];
            for c in GCController::controllers().iter() {
                attach(&shared, c);
            }
            (Self { shared, _observers: Ship(observers) }, rx)
        }
    }

    pub fn set_deadzone(&self, dz: f64) {
        *self.shared.deadzone.lock().unwrap_or_else(PoisonError::into_inner) = dz.clamp(0.0, 0.5);
    }

    /// Read every pad again and report its state. GameController delivers nothing while the app is in the background, so a button
    /// let go meanwhile would otherwise stay "held" in the arbiter's memory until the pad next changes; call this when the app comes
    /// to the front. It runs on the queue GameController uses for its handlers, so this read can never be queued behind a newer one.
    pub fn resnapshot(&self) {
        let pads: Vec<(u32, Retained<GCController>)> = {
            let pads = self.shared.pads.lock().unwrap_or_else(PoisonError::into_inner);
            pads.iter().map(|p| (p.0.id, p.0.ctrl.clone())).collect()
        };
        let job = Ship((self.shared.clone(), pads));
        self.shared.queue.0.exec_async(move || {
            let job = job; // capture the whole wrapper (it is `Send`), not its fields
            let (shared, pads) = &job.0;
            for (id, ctrl) in pads {
                send_state(shared, *id, ctrl);
            }
        });
    }
}

unsafe fn controller_of(n: NonNull<NSNotification>) -> Option<Retained<GCController>> {
    let obj = n.as_ref().object()?;
    obj.downcast::<GCController>().ok()
}

fn kind_of(c: &GCController) -> Option<PadKind> {
    let gp = unsafe { c.extendedGamepad()? };
    if gp.isKindOfClass(GCDualSenseGamepad::class()) {
        Some(PadKind::DualSense)
    } else if gp.isKindOfClass(GCDualShockGamepad::class()) {
        Some(PadKind::DualShock4)
    } else {
        None
    }
}

fn attach(s: &Arc<Shared>, c: Retained<GCController>) {
    let Some(kind) = kind_of(&c) else { return };
    let mut pads = s.pads.lock().unwrap_or_else(PoisonError::into_inner);
    if pads.iter().any(|p| p.0.ctrl == c) {
        return;
    }
    let id = s.next_id.fetch_add(1, Ordering::Relaxed);
    let name = unsafe { c.vendorName() }.map_or_else(|| "Wireless Controller".to_string(), |n| n.to_string());
    unsafe {
        c.setHandlerQueue(&s.queue.0);
        let profile = c.physicalInputProfile();
        let (s2, c2) = (s.clone(), c.clone());
        let handler = RcBlock::new(
            move |_p: NonNull<GCPhysicalInputProfile>, _e: NonNull<objc2_game_controller::GCControllerElement>| {
                send_state(&s2, id, &c2);
            },
        );
        profile.setValueDidChangeHandler(Some(&handler));
        // macOS would otherwise keep the PS (Home) button for itself
        for b in profile.buttons().allValues().iter() {
            b.setPreferredSystemGestureState(GCSystemGestureState::Disabled);
        }
    }
    unsafe { start_motion(s, id, &c) };
    let _ = s.tx.send(PadEvent::Connected { id, kind, name });
    pads.push(Ship(Entry { id, ctrl: c.clone() }));
    drop(pads);
    send_battery(s, id, &c);
    send_state(s, id, &c);
}

/// The sensors are off until asked for. Updates are limited to 125 Hz (one console packet each) and carry an orientation worked out
/// here, since the framework reports none for the DualSense.
unsafe fn start_motion(s: &Arc<Shared>, id: u32, c: &GCController) {
    let Some(m) = c.motion() else { return };
    if m.sensorsRequireManualActivation() {
        m.setSensorsActive(true);
    }
    let tx = s.tx.clone();
    let st = Mutex::new((Instant::now(), Orient::default()));
    let handler = RcBlock::new(move |m: NonNull<GCMotion>| {
        let mut g = st.lock().unwrap_or_else(PoisonError::into_inner);
        let now = Instant::now();
        let dt = now.duration_since(g.0);
        if dt < Duration::from_millis(8) {
            return;
        }
        let m = m.as_ref();
        let (r, a) = (m.rotationRate(), m.acceleration());
        let (gyro, accel) = ([r.x as f32, r.y as f32, r.z as f32], [a.x as f32, a.y as f32, a.z as f32]);
        let orientation = g.1.update(gyro, accel, dt.as_secs_f32().min(0.1));
        g.0 = now;
        let _ = tx.send(PadEvent::Sensors { id, sensors: Sensors { gyro, accel, orientation } });
    });
    let raw: *const block2::DynBlock<dyn Fn(NonNull<GCMotion>)> = &*handler;
    m.setValueChangedHandler(raw as *mut _);
}

fn detach(s: &Arc<Shared>, c: &Retained<GCController>) {
    let mut pads = s.pads.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(i) = pads.iter().position(|p| &p.0.ctrl == c) {
        let e = pads.remove(i);
        unsafe { c.physicalInputProfile().setValueDidChangeHandler(None) };
        let _ = s.tx.send(PadEvent::Disconnected { id: e.0.id });
    }
}

fn send_battery(s: &Shared, id: u32, c: &GCController) {
    if let Some(b) = unsafe { c.battery() } {
        let (level, state) = unsafe { (b.batteryLevel(), b.batteryState()) };
        let charging = state == objc2_game_controller::GCDeviceBatteryState::Charging;
        let _ = s.tx.send(PadEvent::Battery { id, level, charging });
    }
}

fn send_state(s: &Shared, id: u32, c: &GCController) {
    let Some(gp) = (unsafe { c.extendedGamepad() }) else { return };
    let dz = *s.deadzone.lock().unwrap_or_else(PoisonError::into_inner);
    let state = to_state(&read(&gp), dz);
    if openps_input::trace::enabled() {
        log::info!(
            "input_trace t={} stage=source_emit pad={id} {}",
            openps_input::trace::now_us(),
            openps_input::trace::brief(&state)
        );
    }
    let _ = s.tx.send(PadEvent::State { id, state });
}

fn read(gp: &GCExtendedGamepad) -> RawPad {
    let mut raw = RawPad::default();
    unsafe {
        let mut b = |btn: &GCControllerButtonInput, to: Button| {
            if btn.isPressed() {
                raw.buttons.insert(to);
            }
        };
        b(&gp.buttonA(), Button::CROSS);
        b(&gp.buttonB(), Button::CIRCLE);
        b(&gp.buttonX(), Button::SQUARE);
        b(&gp.buttonY(), Button::TRIANGLE);
        b(&gp.leftShoulder(), Button::L1);
        b(&gp.rightShoulder(), Button::R1);
        b(&gp.buttonMenu(), Button::OPTIONS);
        let d = gp.dpad();
        b(&d.up(), Button::DPAD_UP);
        b(&d.down(), Button::DPAD_DOWN);
        b(&d.left(), Button::DPAD_LEFT);
        b(&d.right(), Button::DPAD_RIGHT);
        if let Some(x) = gp.buttonOptions() {
            b(&x, Button::CREATE);
        }
        if let Some(x) = gp.buttonHome() {
            b(&x, Button::PS);
        }
        if let Some(x) = gp.leftThumbstickButton() {
            b(&x, Button::L3);
        }
        if let Some(x) = gp.rightThumbstickButton() {
            b(&x, Button::R3);
        }
        let (l, r) = (gp.leftThumbstick(), gp.rightThumbstick());
        raw.left = (f64::from(l.xAxis().value()), f64::from(l.yAxis().value()));
        raw.right = (f64::from(r.xAxis().value()), f64::from(r.yAxis().value()));
        if let Some(ds) = gp.downcast_ref::<GCDualSenseGamepad>() {
            b(&ds.touchpadButton(), Button::TOUCHPAD);
            raw.touch = finger(&ds.touchpadPrimary());
            raw.touch2 = finger(&ds.touchpadSecondary());
        } else if let Some(ds) = gp.downcast_ref::<GCDualShockGamepad>() {
            if let Some(x) = ds.touchpadButton() {
                b(&x, Button::TOUCHPAD);
            }
            raw.touch = ds.touchpadPrimary().and_then(|p| finger(&p));
            raw.touch2 = ds.touchpadSecondary().and_then(|p| finger(&p));
        }
        raw.l2 = f64::from(gp.leftTrigger().value());
        raw.r2 = f64::from(gp.rightTrigger().value());
    }
    raw
}

/// A touch surface reports (0, 0) with no finger on it; the finger's position is -1..1 with y up.
unsafe fn finger(p: &objc2_game_controller::GCControllerDirectionPad) -> Option<(f64, f64)> {
    let (x, y) = (f64::from(p.xAxis().value()), f64::from(p.yAxis().value()));
    (x != 0.0 || y != 0.0).then(|| ((x + 1.0) / 2.0, (1.0 - y) / 2.0))
}
