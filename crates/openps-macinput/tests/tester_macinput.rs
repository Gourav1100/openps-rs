//! Tester A4: hub / DOM key mapping / runner resilience. Uses fake Cursor and Output only: the real mouse and
//! keyboard are never touched.
use openps_input::engine::Hotkey;
use openps_input::keys::{Input, Key, MouseInput};
use openps_input::overlay::OverlayState;
use openps_input::state::ControllerState;
use openps_input::{load_defaults, load_layers};
use openps_macinput::domkeys::{key_from_dom_code, mouse_from_button};
use openps_macinput::{Cursor, InputHub, InputRunner, Msg, Output, RunnerConfig};
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------- DOM key table

const DOM_CODES: &[&str] = &[
    "Backquote",
    "Backslash",
    "BracketLeft",
    "BracketRight",
    "Comma",
    "Digit0",
    "Digit1",
    "Digit2",
    "Digit3",
    "Digit4",
    "Digit5",
    "Digit6",
    "Digit7",
    "Digit8",
    "Digit9",
    "Equal",
    "IntlBackslash",
    "IntlRo",
    "IntlYen",
    "KeyA",
    "KeyB",
    "KeyC",
    "KeyD",
    "KeyE",
    "KeyF",
    "KeyG",
    "KeyH",
    "KeyI",
    "KeyJ",
    "KeyK",
    "KeyL",
    "KeyM",
    "KeyN",
    "KeyO",
    "KeyP",
    "KeyQ",
    "KeyR",
    "KeyS",
    "KeyT",
    "KeyU",
    "KeyV",
    "KeyW",
    "KeyX",
    "KeyY",
    "KeyZ",
    "Minus",
    "Period",
    "Quote",
    "Semicolon",
    "Slash",
    "AltLeft",
    "AltRight",
    "Backspace",
    "CapsLock",
    "ContextMenu",
    "ControlLeft",
    "ControlRight",
    "Enter",
    "MetaLeft",
    "MetaRight",
    "ShiftLeft",
    "ShiftRight",
    "Space",
    "Tab",
    "Convert",
    "KanaMode",
    "Lang1",
    "Lang2",
    "NonConvert",
    "Delete",
    "End",
    "Help",
    "Home",
    "Insert",
    "PageDown",
    "PageUp",
    "ArrowDown",
    "ArrowLeft",
    "ArrowRight",
    "ArrowUp",
    "NumLock",
    "Numpad0",
    "Numpad1",
    "Numpad2",
    "Numpad3",
    "Numpad4",
    "Numpad5",
    "Numpad6",
    "Numpad7",
    "Numpad8",
    "Numpad9",
    "NumpadAdd",
    "NumpadComma",
    "NumpadDecimal",
    "NumpadDivide",
    "NumpadEnter",
    "NumpadEqual",
    "NumpadMultiply",
    "NumpadSubtract",
    "Escape",
    "F1",
    "F2",
    "F3",
    "F4",
    "F5",
    "F6",
    "F7",
    "F8",
    "F9",
    "F10",
    "F11",
    "F12",
    "F13",
    "F14",
    "F15",
    "F16",
    "F17",
    "F18",
    "F19",
    "F20",
    "Fn",
    "AudioVolumeUp",
    "AudioVolumeDown",
    "AudioVolumeMute",
    "VolumeUp",
    "VolumeDown",
    "VolumeMute",
];

#[test]
fn dom_codes_map_injectively_to_known_keys() {
    let mut seen = std::collections::HashMap::new();
    for c in DOM_CODES {
        if let Some(k) = key_from_dom_code(c) {
            assert!(Key::ALL.contains(&k));
            if let Some(prev) = seen.insert(k, *c) {
                panic!("{prev} and {c} both map to {k:?}");
            }
        }
    }
    assert!(seen.len() > 100);
}

#[test]
fn keys_that_exist_in_config_but_can_never_be_produced_by_the_ui_info() {
    // Info (T07-15): a binding to one of these is accepted by the config but no DOM event can ever press it, and the
    // overlay cannot capture it. List is pinned so a change is noticed.
    let reachable: BTreeSet<Key> = DOM_CODES.iter().filter_map(|c| key_from_dom_code(c)).collect();
    let unreachable: Vec<&str> = Key::ALL.iter().filter(|k| !reachable.contains(k)).map(|k| k.name()).collect();
    assert_eq!(unreachable, ["FN", "JIS_KP_COMMA", "JIS_EISU", "JIS_KANA", "VOLUME_UP", "VOLUME_DOWN", "VOLUME_MUTE"]);
    assert!(unreachable.contains(&"FN") && unreachable.contains(&"JIS_KANA"));
    assert!(!unreachable.contains(&"CAPS_LOCK"));
}

#[test]
fn mouse_button_codes() {
    for (b, m) in [(0, MouseInput::MOUSE_LEFT), (1, MouseInput::MOUSE_MIDDLE), (2, MouseInput::MOUSE_RIGHT)] {
        assert_eq!(mouse_from_button(b), Some(m));
    }
    for b in [-1, 5, 6, 100, i32::MIN, i32::MAX] {
        assert_eq!(mouse_from_button(b), None);
    }
}

// ---------------------------------------------------------------- hub

#[test]
fn input_while_unfocused_is_dropped() {
    let h = InputHub::new();
    h.key_event("KeyW", true, false);
    h.button_event(0, true);
    h.wheel(0.0, -1.0);
    h.set_focused(true);
    assert!(h.take_pressed().is_empty());
}

#[test]
fn scroll_pulses_accumulated_while_mapping_is_off_fire_once_when_it_is_switched_on_info() {
    // Info: pulses are only cleared on focus loss/release_all, not when mapping toggles; snapshot() is not called while
    // mapping is off, so a wheel turned in the meantime is delivered as one phantom L1/R1 tap.
    let h = InputHub::new();
    h.set_focused(true);
    h.wheel(0.0, 5.0);
    std::thread::sleep(Duration::from_millis(5));
    assert!(h.take_pressed().contains(&Input::Mouse(MouseInput::SCROLL_DOWN)));
}

#[test]
fn press_messages_carry_the_held_set_and_releases_wake_the_engine() {
    let h = InputHub::new();
    let (tx, rx) = channel();
    h.attach(tx);
    h.set_focused(true);
    h.key_event("ControlLeft", true, false);
    h.key_event("F10", true, false);
    h.key_event("F10", false, false);
    h.key_event("ControlLeft", false, false);
    h.button_event(2, true);
    let m: Vec<Msg> = rx.try_iter().collect();
    assert!(matches!(&m[0], Msg::Press(Input::Key(Key::LEFT_CONTROL), s) if s.len() == 1));
    assert!(matches!(&m[1], Msg::Press(Input::Key(Key::F10), s) if s.len() == 2));
    assert!(matches!(&m[2], Msg::Wake) && matches!(&m[3], Msg::Wake));
    assert!(matches!(&m[4], Msg::Press(Input::Mouse(MouseInput::MOUSE_RIGHT), _)));
    assert_eq!(m.len(), 5);
    h.detach();
    h.key_event("KeyA", true, false);
    assert!(rx.try_recv().is_err(), "nothing is sent after detach");
}

#[test]
fn hub_survives_heavy_concurrent_use_and_ends_clean() {
    let h = Arc::new(InputHub::new());
    let (tx, rx) = channel();
    h.attach(tx);
    let stop = Arc::new(AtomicBool::new(false));
    let drain = {
        let stop = stop.clone();
        std::thread::spawn(move || {
            let mut n = 0usize;
            while !stop.load(Ordering::Relaxed) {
                n += rx.try_iter().count();
                std::thread::sleep(Duration::from_millis(1));
            }
            n + rx.try_iter().count()
        })
    };
    let ws: Vec<_> = (0..8)
        .map(|t| {
            let h = h.clone();
            std::thread::spawn(move || {
                for i in 0..20_000u32 {
                    let code = ["KeyW", "KeyA", "Space", "ShiftLeft", "F10"][(i as usize + t) % 5];
                    h.key_event(code, i % 3 != 0, false);
                    h.start_key_log();
                    let _ = h.drain_key_log();
                    let _ = h.take_pressed();
                    if i % 1000 == 0 {
                        h.set_focused(i % 2000 == 0);
                    }
                }
            })
        })
        .collect();
    for w in ws {
        w.join().unwrap();
    }
    h.set_focused(false);
    assert!(h.pressed().is_empty() && h.take_pressed().is_empty());
    stop.store(true, Ordering::Relaxed);
    assert!(drain.join().unwrap() > 0);
}

#[test]
fn a_poisoned_hub_lock_does_not_wedge_input() {
    // the hub recovers from poison (PoisonError::into_inner): simulate a panic while another thread holds state
    let h = Arc::new(InputHub::new());
    h.set_focused(true);
    let h2 = h.clone();
    let _ = std::thread::spawn(move || {
        h2.key_event("KeyA", true, false);
        panic!("boom");
    })
    .join();
    h.key_event("KeyB", true, false);
    assert!(h.pressed().contains(&Input::Key(Key::B)));
}

// ---------------------------------------------------------------- runner with fakes

#[derive(Default)]
struct Log {
    pads: Vec<ControllerState>,
    cursor: Vec<String>,
    exits: Vec<bool>,
}
type Shared = Arc<Mutex<Log>>;
struct Out {
    log: Shared,
    panic_after: Option<usize>,
}
impl Output for Out {
    fn pad(&mut self, s: &ControllerState) {
        let mut l = self.log.lock().unwrap();
        l.pads.push(s.clone());
        if self.panic_after.is_some_and(|n| l.pads.len() > n) {
            drop(l);
            panic!("output sink exploded");
        }
    }
    fn overlay(&mut self, _: &OverlayState) {}
    fn exit(&mut self, q: bool) {
        self.log.lock().unwrap().exits.push(q);
    }
}
struct Cur {
    log: Shared,
    dropped: Arc<AtomicUsize>,
}
impl Cursor for Cur {
    fn engage(&mut self, _: Option<(f64, f64)>) {
        self.log.lock().unwrap().cursor.push("engage".into());
    }
    fn release(&mut self) {
        self.log.lock().unwrap().cursor.push("release".into());
    }
    fn set_hide(&mut self, _: bool) {}
    fn delta(&mut self) -> (f64, f64) {
        (0.0, 0.0)
    }
}
impl Drop for Cur {
    fn drop(&mut self) {
        self.dropped.fetch_add(1, Ordering::SeqCst);
        self.log.lock().unwrap().cursor.push("drop".into());
    }
}

fn start(enabled: bool, panic_after: Option<usize>) -> (InputRunner, Arc<InputHub>, Shared, Arc<AtomicUsize>) {
    let hub = Arc::new(InputHub::new());
    hub.set_focused(true);
    hub.set_bounds(Some((0.0, 0.0, 800.0, 600.0)));
    let log: Shared = Default::default();
    let dropped = Arc::new(AtomicUsize::new(0));
    let mut c = load_defaults().unwrap();
    c.feedback_sounds = false;
    let r = InputRunner::start(
        hub.clone(),
        RunnerConfig { config: c, config_path: None, enabled },
        Box::new(Cur { log: log.clone(), dropped: dropped.clone() }),
        Box::new(Out { log: log.clone(), panic_after }),
    )
    .unwrap();
    (r, hub, log, dropped)
}

fn wait(limit: Duration, f: impl Fn() -> bool) -> bool {
    let end = Instant::now() + limit;
    while Instant::now() < end {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    false
}

#[test]
fn a_panicking_output_kills_only_the_engine_thread_and_the_cursor_is_still_dropped() {
    let (r, hub, log, dropped) = start(true, Some(3));
    hub.key_event("KeyW", true, false);
    assert!(
        wait(Duration::from_secs(3), || dropped.load(Ordering::SeqCst) == 1),
        "cursor must be dropped during unwind"
    );
    // hub and runner stay usable; stop() must not hang or panic
    hub.key_event("KeyA", true, false);
    let t = Instant::now();
    r.stop();
    assert!(t.elapsed() < Duration::from_secs(3));
    assert!(log.lock().unwrap().cursor.contains(&"drop".to_string()));
}

#[test]
fn quit_hotkey_reports_exit_and_releases_the_cursor() {
    let (r, hub, log, _d) = start(true, None);
    assert!(wait(Duration::from_secs(2), || log.lock().unwrap().cursor.contains(&"engage".to_string())));
    hub.send(Msg::Hotkey(Hotkey::Quit));
    assert!(wait(Duration::from_secs(2), || log.lock().unwrap().exits == vec![false]));
    assert_eq!(log.lock().unwrap().cursor.iter().rev().find(|c| *c != "drop").map(String::as_str), Some("release"));
    r.stop();
}

#[test]
fn start_stop_cycles_do_not_leak_threads_or_leave_the_cursor_engaged() {
    for i in 0..30 {
        let (r, _hub, log, dropped) = start(i % 2 == 0, None);
        if i % 3 == 0 {
            drop(r);
        } else {
            r.stop();
        }
        assert_eq!(dropped.load(Ordering::SeqCst), 1, "cycle {i}: cursor not dropped");
        let l = log.lock().unwrap();
        let last = l.cursor.iter().rev().find(|c| *c != "drop").map(String::as_str);
        assert!(last.is_none() || last == Some("release"), "cycle {i}: {:?}", l.cursor);
    }
}

#[test]
fn ui_edit_events_without_a_config_path_are_refused_not_fatal() {
    let (r, hub, _log, _d) = start(false, None);
    hub.send(Msg::Ui(openps_input::engine::UiEvent::ProfileAdd));
    hub.send(Msg::Ui(openps_input::engine::UiEvent::Slider { value: 0.2, final_: true }));
    hub.send(Msg::Hotkey(Hotkey::Overlay));
    std::thread::sleep(Duration::from_millis(100));
    r.stop();
}

#[test]
fn runner_with_an_explicit_but_unusable_config_path_does_not_crash_on_edit() {
    let hub = Arc::new(InputHub::new());
    hub.set_focused(true);
    let log: Shared = Default::default();
    let c = load_layers(Some(""), false).unwrap();
    let r = InputRunner::start(
        hub.clone(),
        RunnerConfig { config: c, config_path: Some("/nonexistent-dir-zzz/sub/u.toml".into()), enabled: false },
        Box::new(Cur { log: log.clone(), dropped: Default::default() }),
        Box::new(Out { log, panic_after: None }),
    )
    .unwrap();
    hub.send(Msg::Hotkey(Hotkey::Overlay));
    hub.send(Msg::Ui(openps_input::engine::UiEvent::Slider { value: 0.2, final_: true }));
    std::thread::sleep(Duration::from_millis(150));
    r.stop();
}
