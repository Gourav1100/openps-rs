//! The real Con2PC engine on its own thread, fed through the hub, with a recording output and a fake cursor.
use openps_input::engine::Hotkey;
use openps_input::overlay::OverlayState;
use openps_input::state::ControllerState;
use openps_input::{load_defaults, Config};
use openps_macinput::{Cursor, InputHub, InputRunner, Output, RunnerConfig};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Default)]
struct Log {
    pads: Vec<ControllerState>,
    overlays: Vec<bool>, // mapping flag of each pushed overlay state
    beeps: Vec<String>,
    cursor: Vec<String>,
    delta: (f64, f64),
}
type Shared = Arc<Mutex<Log>>;

struct Out(Shared);
impl Output for Out {
    fn pad(&mut self, s: &ControllerState) {
        self.0.lock().unwrap().pads.push(s.clone());
    }
    fn overlay(&mut self, s: &OverlayState) {
        self.0.lock().unwrap().overlays.push(s.mapping);
    }
    fn beep(&mut self, s: &str) {
        self.0.lock().unwrap().beeps.push(s.into());
    }
}
struct Cur(Shared);
impl Cursor for Cur {
    fn engage(&mut self, c: Option<(f64, f64)>) {
        self.0.lock().unwrap().cursor.push(format!("engage {c:?}"));
    }
    fn recenter(&mut self, _: (f64, f64)) {}
    fn release(&mut self) {
        self.0.lock().unwrap().cursor.push("release".into());
    }
    fn set_hide(&mut self, h: bool) {
        self.0.lock().unwrap().cursor.push(format!("hide {h}"));
    }
    fn delta(&mut self) -> (f64, f64) {
        std::mem::take(&mut self.0.lock().unwrap().delta)
    }
}

fn cfg() -> Config {
    let mut c = load_defaults().unwrap();
    c.feedback_sounds = false;
    c
}

fn start(enabled: bool) -> (InputRunner, Arc<InputHub>, Shared) {
    let hub = Arc::new(InputHub::new());
    hub.set_focused(true);
    hub.set_bounds(Some((100.0, 100.0, 1000.0, 600.0)));
    let log: Shared = Default::default();
    let r = InputRunner::start(
        hub.clone(),
        RunnerConfig { config: cfg(), config_path: None, enabled },
        Box::new(Cur(log.clone())),
        Box::new(Out(log.clone())),
    )
    .unwrap();
    (r, hub, log)
}

fn wait(limit: Duration, f: impl Fn() -> bool) -> bool {
    let end = Instant::now() + limit;
    while Instant::now() < end {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    false
}

fn last_pad(l: &Shared) -> Option<ControllerState> {
    l.lock().unwrap().pads.last().cloned()
}
fn deflected(s: &ControllerState) -> bool {
    s.left_y.abs() > 0.5 || s.left_x.abs() > 0.5
}

#[test]
fn keys_drive_the_left_stick_and_releasing_recentres_it() {
    let (r, hub, log) = start(true);
    assert!(
        wait(Duration::from_secs(2), || log.lock().unwrap().cursor.iter().any(|c| c.starts_with("engage"))),
        "mapping engages the cursor"
    );
    hub.key_event("KeyW", true, false);
    assert!(
        wait(Duration::from_secs(2), || last_pad(&log).is_some_and(|s| deflected(&s))),
        "W moves the stick: {:?}",
        last_pad(&log)
    );
    hub.key_event("KeyW", false, false);
    assert!(wait(Duration::from_secs(2), || last_pad(&log).is_some_and(|s| !deflected(&s))), "release recentres");
    r.stop();
}

#[test]
fn the_cursor_is_engaged_at_the_window_centre_and_always_released_on_stop() {
    let (r, _hub, log) = start(true);
    assert!(wait(Duration::from_secs(2), || log.lock().unwrap().cursor.iter().any(|c| c.starts_with("engage"))));
    {
        let l = log.lock().unwrap();
        assert!(
            l.cursor.iter().any(|c| c == "engage Some((600.0, 400.0))"),
            "centre of (100,100,1000,600): {:?}",
            l.cursor
        );
    }
    r.stop(); // joins the engine thread
    let l = log.lock().unwrap();
    assert_eq!(l.cursor.last().map(String::as_str), Some("release"), "{:?}", l.cursor);
    let last = l.pads.last().unwrap();
    assert!(!deflected(last) && last.buttons.is_empty(), "a neutral pad is sent on the way out");
}

#[test]
fn mouse_movement_drives_the_right_stick() {
    let (r, _hub, log) = start(true);
    assert!(wait(Duration::from_secs(2), || log.lock().unwrap().cursor.iter().any(|c| c.starts_with("engage"))));
    // the mouse is ignored for a moment after capture (see the engine), so keep moving it for longer than that
    for _ in 0..80 {
        log.lock().unwrap().delta = (80.0, 0.0);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        wait(Duration::from_secs(2), || log.lock().unwrap().pads.iter().any(|s| s.right_x.abs() > 0.05)),
        "{:?}",
        last_pad(&log)
    );
    r.stop();
}

#[test]
fn the_toggle_hotkey_turns_mapping_off_and_releases_the_cursor() {
    let (r, hub, log) = start(true);
    assert!(wait(Duration::from_secs(2), || log.lock().unwrap().cursor.iter().any(|c| c.starts_with("engage"))));
    hub.key_event("F10", true, false); // default toggle key
    hub.key_event("F10", false, false);
    assert!(
        wait(Duration::from_secs(2), || log.lock().unwrap().cursor.last().is_some_and(|c| c == "release")),
        "{:?}",
        log.lock().unwrap().cursor
    );
    log.lock().unwrap().pads.clear();
    hub.key_event("KeyW", true, false);
    std::thread::sleep(Duration::from_millis(150));
    assert!(log.lock().unwrap().pads.iter().all(|s| !deflected(s)), "keys do nothing while mapping is off");
    // and back on
    hub.key_event("KeyW", false, false);
    hub.key_event("F10", true, false);
    hub.key_event("F10", false, false);
    hub.key_event("KeyW", true, false);
    assert!(wait(Duration::from_secs(2), || last_pad(&log).is_some_and(|s| deflected(&s))));
    r.stop();
}

#[test]
fn losing_focus_stops_mapping_and_frees_the_mouse_until_focus_returns() {
    let (r, hub, log) = start(true);
    hub.key_event("KeyW", true, false);
    assert!(wait(Duration::from_secs(2), || last_pad(&log).is_some_and(|s| deflected(&s))));
    hub.set_focused(false); // clicked another window
    assert!(
        wait(Duration::from_secs(2), || log.lock().unwrap().cursor.last().is_some_and(|c| c == "release")),
        "{:?}",
        log.lock().unwrap().cursor
    );
    assert!(
        wait(Duration::from_secs(2), || last_pad(&log).is_some_and(|s| !deflected(&s))),
        "the stuck W is released with the focus"
    );
    hub.set_focused(true);
    assert!(
        wait(Duration::from_secs(2), || log.lock().unwrap().cursor.last().is_some_and(|c| c.starts_with("engage"))),
        "mapping resumes"
    );
    r.stop();
}

#[test]
fn overlay_state_and_hotkey_requests_reach_their_destinations() {
    let (r, _hub, log) = start(false);
    assert!(wait(Duration::from_secs(2), || !log.lock().unwrap().overlays.is_empty()));
    assert_eq!(log.lock().unwrap().overlays.last(), Some(&false), "mapping starts off here");
    r.hotkey(Hotkey::Toggle);
    assert!(wait(Duration::from_secs(2), || log.lock().unwrap().overlays.last() == Some(&true)));
    r.stop();
}

#[test]
fn a_hostile_flood_from_many_threads_never_panics_or_wedges_the_engine() {
    let (r, hub, log) = start(true);
    let codes = ["KeyW", "KeyA", "F10", "F11", "F12", "F9", "F8", "Escape", "Unidentified", "", "MetaLeft", "Space"];
    let hs: Vec<_> = (0..4)
        .map(|t| {
            let hub = hub.clone();
            std::thread::spawn(move || {
                let mut x = 0x9E37_79B9_7F4A_7C15u64 ^ t;
                for _ in 0..20_000 {
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    let c = codes[(x % codes.len() as u64) as usize];
                    hub.key_event(c, x & 1 == 0, x & 2 == 0);
                    hub.button_event((x % 9) as i32 - 2, x & 4 == 0);
                    hub.wheel((x % 7) as f64 - 3.0, f64::from_bits(x));
                    if x.is_multiple_of(97) {
                        hub.set_focused(x & 8 == 0);
                    }
                }
            })
        })
        .collect();
    for h in hs {
        h.join().unwrap();
    }
    hub.set_focused(true);
    hub.release_all();
    // the engine is alive and still obeys: stop returns promptly with the cursor released
    let t0 = Instant::now();
    r.stop();
    assert!(t0.elapsed() < Duration::from_secs(3));
    assert_eq!(log.lock().unwrap().cursor.last().map(String::as_str), Some("release"));
}

#[test]
fn dropping_the_runner_without_stop_still_releases_everything() {
    let (r, _hub, log) = start(true);
    assert!(wait(Duration::from_secs(2), || log.lock().unwrap().cursor.iter().any(|c| c.starts_with("engage"))));
    drop(r);
    assert_eq!(log.lock().unwrap().cursor.last().map(String::as_str), Some("release"));
}

#[test]
fn an_invalid_config_fails_to_start_instead_of_hanging() {
    let hub = Arc::new(InputHub::new());
    let mut c = cfg();
    c.toggle = "NOT_A_KEY".into();
    let log: Shared = Default::default();
    let r = InputRunner::start(
        hub,
        RunnerConfig { config: c, config_path: None, enabled: true },
        Box::new(Cur(log.clone())),
        Box::new(Out(log)),
    );
    assert!(r.is_err());
}

// ---- latency: key event -> pad state out of the engine -----------------------------------------------------------------------
#[test]
fn key_presses_and_releases_reach_the_pad_output_in_about_a_millisecond() {
    struct Timed(Arc<Mutex<Vec<(Instant, bool)>>>);
    impl Output for Timed {
        fn pad(&mut self, s: &ControllerState) {
            self.0.lock().unwrap().push((Instant::now(), s.left_y.abs() > 0.5));
        }
        fn overlay(&mut self, _: &OverlayState) {}
    }
    let hub = Arc::new(InputHub::new());
    hub.set_focused(true);
    hub.set_bounds(Some((0.0, 0.0, 1000.0, 600.0)));
    let seen: Arc<Mutex<Vec<(Instant, bool)>>> = Default::default();
    let log: Shared = Default::default();
    let r = InputRunner::start(
        hub.clone(),
        RunnerConfig { config: cfg(), config_path: None, enabled: true },
        Box::new(Cur(log)),
        Box::new(Timed(seen.clone())),
    )
    .unwrap();
    std::thread::sleep(Duration::from_millis(200));
    let (mut press, mut release) = (vec![], vec![]);
    for _ in 0..100 {
        let t = Instant::now();
        hub.key_event("KeyW", true, false);
        let deadline = t + Duration::from_millis(100);
        let got_press = loop {
            let found = seen.lock().unwrap().iter().find(|(at, on)| *at >= t && *on).map(|(at, _)| *at);
            if found.is_some() || Instant::now() > deadline {
                break found;
            }
            std::thread::sleep(Duration::from_micros(100));
        };
        press.push(got_press.expect("press never reached the pad").duration_since(t).as_secs_f64() * 1e3);
        std::thread::sleep(Duration::from_millis(30));
        let t = Instant::now();
        hub.key_event("KeyW", false, false);
        let deadline = t + Duration::from_millis(100);
        let got_release = loop {
            let found = seen.lock().unwrap().iter().find(|(at, on)| *at >= t && !*on).map(|(at, _)| *at);
            if found.is_some() || Instant::now() > deadline {
                break found;
            }
            std::thread::sleep(Duration::from_micros(100));
        };
        release.push(got_release.expect("release never reached the pad").duration_since(t).as_secs_f64() * 1e3);
        std::thread::sleep(Duration::from_millis(30));
        seen.lock().unwrap().clear();
    }
    r.stop();
    let pct = |v: &mut Vec<f64>, p: f64| {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v[((v.len() - 1) as f64 * p) as usize]
    };
    let (p50, p99) = (pct(&mut press, 0.5), pct(&mut press, 0.99));
    let (r50, r99) = (pct(&mut release, 0.5), pct(&mut release, 0.99));
    eprintln!("key -> pad: press p50 {p50:.2} ms p99 {p99:.2} ms | release p50 {r50:.2} ms p99 {r99:.2} ms");
    assert!(p50 < 1.5 && p99 < 6.0, "press {p50:.2} / {p99:.2} ms");
    assert!(r50 < 1.5 && r99 < 6.0, "release {r50:.2} / {r99:.2} ms");
}

// ---- the specified startup default: mapping OFF unless the user's config says otherwise ------------------------------------
#[test]
fn mapping_starts_off_and_the_cursor_is_left_alone_until_the_user_turns_it_on() {
    let (r, hub, log) = start(false); // the app passes enabled = false; the shipped config has start_enabled = false
    std::thread::sleep(Duration::from_millis(400));
    assert!(
        log.lock().unwrap().cursor.iter().all(|c| !c.starts_with("engage")),
        "the cursor must not be captured at startup"
    );
    hub.key_event("KeyW", true, false); // typing while off does nothing to the pad
    std::thread::sleep(Duration::from_millis(150));
    assert!(last_pad(&log).is_none_or(|p| !deflected(&p)), "keys must not drive the pad while mapping is off");
    // F10 turns it on
    r.hotkey(openps_input::engine::Hotkey::Toggle);
    assert!(wait(Duration::from_secs(2), || log.lock().unwrap().cursor.iter().any(|c| c.starts_with("engage"))));
}

#[test]
fn start_enabled_in_the_users_config_turns_mapping_on_at_startup() {
    let hub = Arc::new(InputHub::new());
    hub.set_focused(true);
    hub.set_bounds(Some((100.0, 100.0, 1000.0, 600.0)));
    let log: Shared = Default::default();
    let mut c = cfg();
    c.start_enabled = true;
    let _r = InputRunner::start(
        hub,
        RunnerConfig { config: c, config_path: None, enabled: false },
        Box::new(Cur(log.clone())),
        Box::new(Out(log.clone())),
    )
    .unwrap();
    assert!(wait(Duration::from_secs(2), || log.lock().unwrap().cursor.iter().any(|c| c.starts_with("engage"))));
}
