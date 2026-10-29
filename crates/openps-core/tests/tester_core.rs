//! Independent tester suite for openps-core: state machine under concurrency, event ordering, shutdown while
//! connecting, store corruption / partial writes / permissions, settings and host validation boundaries, import,
//! netlink parsers. Fully offline (fake backend). Tests marked `#[ignore = "BUG T06-xx ..."]` demonstrate a confirmed
//! defect: un-ignore after the fix.
use openps_core::backend::Backend;
use openps_core::control::{ControlCommand, ControlLink};
use openps_core::events::{AppEvent, EventSink, NullMedia, VecSink};
use openps_core::netlink;
use openps_core::store::ConsoleStore;
use openps_core::types::*;
use openps_core::{Core, CoreBuilder, CoreError, PsnService, RemoteRegistrar, RemoteRequest};
use openps_crypto::RpCipher;
use openps_discovery::{Console, DeviceType, Status};
use openps_psn::store::{MemoryStore, SecretStore};
use openps_rp::client::ClientError;
use openps_rp::ctrl::Inbound;
use openps_rp::regist::{RegisterParams, Registered};
use openps_rp::session::{CtrlInfo, StreamingType};
use openps_session::link::{mem_pair, Link};
use openps_takion::senkusha::SenkushaConfig;
use std::io;
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

const MAC: &str = "AABBCCDDEE01";

// ---- fakes ----------------------------------------------------------------------------------------------------------

struct PCtl;
impl ControlLink for PCtl {
    fn version(&self) -> String {
        "1.0".into()
    }
    fn session_id(&self) -> String {
        "s".into()
    }
    fn cipher(&self) -> RpCipher {
        RpCipher::new([1; 16], [2; 16], [3; 16])
    }
    fn info(&self) -> CtrlInfo {
        // the console "does not allow streaming": the session ends at once, deterministically, with `Failed`
        CtrlInfo { server_type: Some(1), full_hd: true, prohibited: true }
    }
    fn take_pending(&mut self) -> Vec<Inbound> {
        vec![]
    }
    fn poll(&mut self, t: Duration) -> io::Result<Vec<Inbound>> {
        std::thread::sleep(t.min(Duration::from_millis(5)));
        Ok(vec![])
    }
    fn send(&mut self, _: ControlCommand) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Default)]
struct FB {
    /// `open_control` blocks until this many milliseconds after the first call have elapsed (0 = no gate).
    gate_ms: AtomicUsize,
    open_calls: AtomicUsize,
    fail_first: AtomicBool,
    wakes: Mutex<Vec<IpAddr>>,
    scan: Mutex<Vec<Console>>,
}
impl Backend for FB {
    fn scan(&self, _: &[IpAddr]) -> io::Result<Vec<Console>> {
        Ok(self.scan.lock().unwrap().clone())
    }
    fn wake(&self, _: DeviceType, ip: IpAddr, _: &str) -> Result<bool, String> {
        self.wakes.lock().unwrap().push(ip);
        Ok(true)
    }
    fn register(&self, _: &str, _: &RegisterParams) -> Result<Registered, ClientError> {
        Err(ClientError::Http("fake".into()))
    }
    fn open_control(
        &self,
        _: &str,
        _: DeviceType,
        _: &str,
        _: &Registered,
        _: StreamingType,
    ) -> Result<Box<dyn ControlLink>, ClientError> {
        let n = self.open_calls.fetch_add(1, Ordering::SeqCst);
        let gate = self.gate_ms.load(Ordering::SeqCst);
        if gate > 0 && n == 0 {
            std::thread::sleep(Duration::from_millis(gate as u64));
        }
        if n == 0 && self.fail_first.load(Ordering::SeqCst) {
            return Err(ClientError::Http("first attempt fails".into()));
        }
        Ok(Box::new(PCtl))
    }
    fn open_links(&self, _: &str) -> io::Result<(Link, Link)> {
        Ok((mem_pair().0, mem_pair().0))
    }
}

struct FPsn;
impl PsnService for FPsn {
    fn authorize_url(&self) -> Result<String, String> {
        Ok("https://a.test/".into())
    }
    fn complete(&self, _: &str) -> Result<PsnAccountInfo, String> {
        Ok(PsnAccountInfo { online_id: "P".into(), account_id: "42".into() })
    }
    fn sign_out(&self) {}
    fn remote_access_token(&self) -> Result<String, String> {
        Ok("tok".into())
    }
}

struct Rig {
    core: Core,
    fb: Arc<FB>,
    events: Arc<VecSink>,
    secrets: Arc<MemoryStore>,
    dir: tempfile::TempDir,
}

fn rec(id: &str, host: Option<&str>) -> ConsoleRecord {
    ConsoleRecord {
        id: id.into(),
        device: Device::Ps5,
        nickname: "n".into(),
        ap_name: "a".into(),
        version: "1.0".into(),
        host: host.map(Into::into),
    }
}

fn rig_with(settle_s: f64, events: Option<Arc<dyn EventSink>>) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let fb = Arc::new(FB::default());
    let vec_events = Arc::new(VecSink::default());
    let secrets = Arc::new(MemoryStore::default());
    let mut store = ConsoleStore::open(dir.path().join("state.json")).unwrap();
    store.upsert(rec(MAC, Some("127.0.0.1"))).unwrap();
    ConsoleStore::put_secrets(&*secrets, MAC, &ConsoleSecrets { regist_key: "aa".into(), rp_key: "bb".into() })
        .unwrap();
    let sink: Arc<dyn EventSink> = events.unwrap_or_else(|| vec_events.clone());
    let core = Core::new(CoreBuilder {
        store,
        secrets: secrets.clone(),
        backend: fb.clone(),
        psn: Some(Arc::new(FPsn)),
        events: sink,
        media: Arc::new(NullMedia),
        settle_s,
        senkusha: SenkushaConfig { io_timeout_s: 0.02, attempts: 2, retry_delay_s: 0.01, ..Default::default() },
    });
    core.set_auto_bitrate_probe(|_| None);
    Rig { core, fb, events: vec_events, secrets, dir }
}
fn rig() -> Rig {
    rig_with(0.0, None)
}

fn wait_idle(core: &Core) {
    assert!(core.wait_idle(Duration::from_secs(10)), "session did not end");
}

// ---- state machine / events ---------------------------------------------------------------------------------------------

#[test]
fn a_session_that_cannot_start_reports_connecting_then_failed_and_frees_the_slot_every_time() {
    let r = rig();
    for round in 1..=5 {
        r.core.connect(MAC, StreamOptions::default()).unwrap();
        wait_idle(&r.core);
        let states = r.events.states();
        assert_eq!(states.len(), round * 2, "{states:?}");
        assert_eq!(states[round * 2 - 2], SessionState::Connecting);
        assert!(matches!(&states[round * 2 - 1], SessionState::Failed { reason } if reason.contains("does not allow")));
        assert!(matches!(r.core.session_state(), SessionState::Failed { .. }));
        assert_eq!(r.core.active_console(), None);
    }
    assert_eq!(r.core.disconnect(), Err(CoreError::NotConnected));
}

#[test]
fn invalid_options_and_unknown_consoles_change_nothing() {
    let r = rig();
    let bad = StreamOptions { bitrate: 7, ..Default::default() };
    assert!(matches!(r.core.connect(MAC, bad), Err(CoreError::Console(_))));
    assert_eq!(r.core.connect("112233445566", StreamOptions::default()), Err(CoreError::UnknownConsole));
    assert_eq!(r.core.connect("", StreamOptions::default()), Err(CoreError::UnknownConsole));
    assert_eq!(r.fb.open_calls.load(Ordering::SeqCst), 0);
    assert!(r.events.states().is_empty());
    assert_eq!(r.core.session_state(), SessionState::Idle);
    assert_eq!(r.core.active_console(), None);
}

#[test]
fn connect_without_stored_keys_or_address_fails_cleanly() {
    let r = rig();
    r.secrets.delete(&format!("console/{MAC}")).unwrap();
    assert!(matches!(r.core.connect(MAC, StreamOptions::default()), Err(CoreError::Store(_))));
    assert_eq!(
        r.core.wake(MAC).unwrap_err().to_string(),
        "stored registration keys are missing; pair the console again"
    );
    assert_eq!(r.core.session_state(), SessionState::Idle);
}

#[test]
fn concurrent_connects_start_exactly_one_session() {
    let r = rig();
    r.fb.gate_ms.store(400, Ordering::SeqCst); // the first open_control blocks, keeping the slot busy
    let barrier = Arc::new(std::sync::Barrier::new(8));
    let hs: Vec<_> = (0..8)
        .map(|_| {
            let (c, b) = (r.core.clone(), barrier.clone());
            std::thread::spawn(move || {
                b.wait();
                c.connect(MAC, StreamOptions::default())
            })
        })
        .collect();
    let res: Vec<_> = hs.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(res.iter().filter(|r| r.is_ok()).count(), 1, "{res:?}");
    assert_eq!(res.iter().filter(|r| **r == Err(CoreError::AlreadyConnected)).count(), 7);
    wait_idle(&r.core);
    assert_eq!(r.fb.open_calls.load(Ordering::SeqCst), 1);
    let connecting = r.events.states().iter().filter(|s| **s == SessionState::Connecting).count();
    assert_eq!(connecting, 1);
}

#[test]
fn every_core_call_stays_responsive_while_a_connect_is_blocked_in_the_backend() {
    let r = rig();
    r.fb.gate_ms.store(600, Ordering::SeqCst);
    r.core.connect(MAC, StreamOptions::default()).unwrap();
    let t = Instant::now();
    let c = r.core.clone();
    let h = std::thread::spawn(move || {
        let _ = c.consoles();
        let _ = c.settings();
        let _ = c.set_settings(StreamOptions::default());
        let _ = c.wake(MAC);
        let _ = c.scan(&[]);
        let _ = c.session_state();
        let _ = c.active_console();
        let _ = c.psn_account();
        let _ = c.control(ControlCommand::MicRequest);
        let _ = c.set_pad(Default::default());
        let _ = c.request_keyframe();
        let _ = c.disconnect();
        let _ = c.forget("112233445566");
    });
    h.join().unwrap();
    assert!(
        t.elapsed() < Duration::from_millis(500),
        "core calls were blocked by the connecting driver: {:?}",
        t.elapsed()
    );
    wait_idle(&r.core);
}

#[test]
fn forgetting_the_console_during_a_connect_does_not_deadlock_or_wedge_the_core() {
    let r = rig();
    r.fb.gate_ms.store(300, Ordering::SeqCst);
    r.core.connect(MAC, StreamOptions::default()).unwrap();
    r.core.forget(MAC).unwrap();
    wait_idle(&r.core);
    assert!(r.core.consoles().is_empty());
    assert_eq!(r.core.connect(MAC, StreamOptions::default()), Err(CoreError::UnknownConsole));
}

#[test]
fn shutdown_when_idle_is_instant_and_repeatable() {
    let r = rig();
    let t = Instant::now();
    r.core.shutdown();
    r.core.shutdown();
    assert!(t.elapsed() < Duration::from_millis(200));
    assert!(r.core.wait_idle(Duration::ZERO));
}

#[test]
fn shutdown_while_connecting_waits_for_the_backend_to_return_and_leaves_a_clean_state() {
    let r = rig();
    r.fb.gate_ms.store(700, Ordering::SeqCst);
    r.core.connect(MAC, StreamOptions::default()).unwrap();
    let t = Instant::now();
    r.core.shutdown();
    assert!(t.elapsed() >= Duration::from_millis(500), "characterisation: shutdown joins the driver");
    assert_eq!(r.core.active_console(), None);
}

#[test]
fn shutdown_must_return_in_bounded_time_even_if_the_backend_blocks() {
    let r = rig();
    r.fb.gate_ms.store(8000, Ordering::SeqCst);
    r.core.connect(MAC, StreamOptions::default()).unwrap();
    let t = Instant::now();
    r.core.shutdown();
    assert!(t.elapsed() < Duration::from_secs(6), "shutdown took {:?}", t.elapsed());
}

struct ReconnectSink {
    core: OnceLock<Core>,
    outcome: Mutex<Option<Result<(), CoreError>>>,
    inner: VecSink,
}
impl EventSink for ReconnectSink {
    fn emit(&self, e: AppEvent) {
        let terminal = matches!(&e, AppEvent::Session(SessionState::Failed { .. } | SessionState::Closed { .. }));
        self.inner.emit(e);
        if terminal && self.outcome.lock().unwrap().is_none() {
            // what a UI "Retry" handler does the moment it is told the session ended
            let res = self.core.get().unwrap().connect(MAC, StreamOptions::default());
            *self.outcome.lock().unwrap() = Some(res);
        }
    }
}

#[test]
fn a_handler_told_the_session_ended_must_be_able_to_connect_again() {
    let sink = Arc::new(ReconnectSink { core: OnceLock::new(), outcome: Mutex::new(None), inner: VecSink::default() });
    let r = rig_with(0.0, Some(sink.clone()));
    sink.core.set(r.core.clone()).ok();
    r.core.connect(MAC, StreamOptions::default()).unwrap();
    let t = Instant::now();
    while sink.outcome.lock().unwrap().is_none() && t.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(10));
    }
    let got = sink.outcome.lock().unwrap().take().unwrap_or(Err(CoreError::NotConnected));
    assert_eq!(got, Ok(()), "AlreadyConnected: slot still held");
    wait_idle(&r.core);
}

struct CallbackSink {
    core: OnceLock<Core>,
    seen: AtomicBool,
}
impl EventSink for CallbackSink {
    fn emit(&self, e: AppEvent) {
        if matches!(e, AppEvent::Session(SessionState::Connecting)) {
            if let Some(c) = self.core.get() {
                let _ = c.active_console(); // any read-only call back into the core
                self.seen.store(true, Ordering::SeqCst);
            }
        }
    }
}

#[test]
fn an_event_sink_may_call_back_into_the_core_while_connecting() {
    let sink = Arc::new(CallbackSink { core: OnceLock::new(), seen: AtomicBool::new(false) });
    let r = rig_with(0.0, Some(sink.clone()));
    sink.core.set(r.core.clone()).ok();
    let (tx, rx) = std::sync::mpsc::channel();
    let c = r.core.clone();
    std::thread::spawn(move || {
        let _ = tx.send(c.connect(MAC, StreamOptions::default()));
    });
    assert!(rx.recv_timeout(Duration::from_secs(3)).is_ok(), "connect() deadlocked inside the event sink");
}

#[test]
fn a_shutdown_requested_during_the_retry_settle_delay_must_cancel_the_retry() {
    let r = rig_with(0.6, None);
    r.core.set_auto_bitrate_probe(|_| Some(50_000));
    r.fb.fail_first.store(true, Ordering::SeqCst);
    r.core.connect(MAC, StreamOptions { bitrate: -1, ..Default::default() }).unwrap();
    std::thread::sleep(Duration::from_millis(200)); // first attempt failed; the driver is in its settle sleep
    assert_eq!(r.core.disconnect(), Ok(()), "no commander in this window: the request is remembered");
    let c = r.core.clone();
    let h = std::thread::spawn(move || c.shutdown());
    h.join().unwrap();
    let connecting = r.events.states().iter().filter(|s| **s == SessionState::Connecting).count();
    assert_eq!(connecting, 1, "the retry started although the app asked to shut down");
}

#[test]
fn the_auto_bitrate_retry_happens_once_and_ends_in_a_final_state() {
    let r = rig_with(0.05, None);
    r.core.set_auto_bitrate_probe(|_| Some(50_000));
    r.fb.fail_first.store(true, Ordering::SeqCst);
    r.core.connect(MAC, StreamOptions { bitrate: -1, ..Default::default() }).unwrap();
    wait_idle(&r.core);
    assert_eq!(r.fb.open_calls.load(Ordering::SeqCst), 2);
    let s = r.events.states();
    assert_eq!(s.iter().filter(|x| **x == SessionState::Connecting).count(), 2);
    assert!(matches!(s.last(), Some(SessionState::Failed { .. })));
    // a fixed bitrate is never retried
    let r2 = rig_with(0.0, None);
    r2.core.set_auto_bitrate_probe(|_| Some(50_000));
    r2.fb.fail_first.store(true, Ordering::SeqCst);
    r2.core.connect(MAC, StreamOptions { bitrate: 6000, ..Default::default() }).unwrap();
    wait_idle(&r2.core);
    assert_eq!(r2.fb.open_calls.load(Ordering::SeqCst), 1);
}

struct Boom;
impl RemoteRegistrar for Boom {
    fn register(&self, _: RemoteRequest, _: Arc<AtomicBool>, _: &dyn Fn(&str)) -> Result<Registered, String> {
        panic!("registrar bug");
    }
}
struct Fail;
impl RemoteRegistrar for Fail {
    fn register(&self, _: RemoteRequest, _: Arc<AtomicBool>, _: &dyn Fn(&str)) -> Result<Registered, String> {
        Err("nope".into())
    }
}

#[test]
fn remote_registration_validates_its_input_and_releases_the_slot_after_an_error() {
    let r = rig();
    r.core.set_remote_registrar(Arc::new(Fail));
    assert_eq!(r.core.register_remote("d").unwrap_err(), CoreError::NeedsPsnLogin);
    r.core.psn_complete("x?code=1").unwrap();
    for bad in ["", &"a".repeat(129), "has space", "tab\t", "né", "nl\n"] {
        assert!(matches!(r.core.register_remote(bad), Err(CoreError::Console(_))), "{bad:?}");
    }
    for _ in 0..3 {
        assert_eq!(r.core.register_remote("duid-1").unwrap_err(), CoreError::Console("nope".into()));
    }
}

#[test]
fn a_panicking_remote_registrar_must_not_block_later_registrations() {
    let r = rig();
    r.core.psn_complete("x?code=1").unwrap();
    r.core.set_remote_registrar(Arc::new(Boom));
    let c = r.core.clone();
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| c.register_remote("duid-1")));
    r.core.set_remote_registrar(Arc::new(Fail));
    assert_eq!(r.core.register_remote("duid-1").unwrap_err(), CoreError::Console("nope".into()), "slot is stuck");
}

// ---- settings / host / pin validation -------------------------------------------------------------------------------------

#[test]
fn stream_option_validation_never_panics_and_is_exact_at_the_boundaries() {
    let ok = StreamOptions::default();
    for h in [i32::MIN, -1, 0, 1, 359, 360, 361, 539, 540, 719, 720, 721, 1079, 1080, 1081, i32::MAX] {
        for w in [i32::MIN, -1, 0, h, h.wrapping_mul(16) / 9, i32::MAX] {
            let v = StreamOptions { width: w, height: h, ..ok.clone() };
            let good = matches!(h, 360 | 540 | 720 | 1080) && w == h * 16 / 9;
            assert_eq!(v.validate().is_ok(), good, "{w}x{h}");
        }
    }
    for fps in [i32::MIN, -1, 0, 29, 30, 31, 59, 60, 61, i32::MAX] {
        assert_eq!(StreamOptions { fps, ..ok.clone() }.validate().is_ok(), matches!(fps, 30 | 60), "{fps}");
    }
    for b in [i32::MIN, -2, -1, 0, 1, 1999, 2000, 15000, 15001, i32::MAX] {
        assert_eq!(
            StreamOptions { bitrate: b, ..ok.clone() }.validate().is_ok(),
            matches!(b, -1 | 2000 | 6000 | 10000 | 15000),
            "{b}"
        );
    }
    for (s, v) in [(0u8, true), (200, true), (201, false), (255, false)] {
        assert_eq!(StreamOptions { sharpness: s, ..ok.clone() }.validate().is_ok(), v);
        assert_eq!(StreamOptions { saturation: s, ..ok.clone() }.validate().is_ok(), v);
    }
    for v in [0u8, 100, 101, 255] {
        assert_eq!(StreamOptions { volume: v, ..ok.clone() }.validate().is_ok(), v <= 100);
    }
    for f in [0u16, 29, 30, 60, 75, 90, 120, 144, 145, u16::MAX] {
        assert_eq!(
            StreamOptions { display_fps: f, ..ok.clone() }.validate().is_ok(),
            matches!(f, 30 | 60 | 75 | 120 | 144)
        );
    }
    for s in ["Off", "OFF", " off", "off\0", "1440P", "\u{ff4f}ff"] {
        assert!(StreamOptions { upscale: s.into(), ..ok.clone() }.validate().is_err(), "{s:?}");
    }
    assert!(StreamOptions { hevc: false, hdr: true, ..ok.clone() }.validate().is_err());
}

#[test]
fn set_settings_rejects_garbage_without_touching_the_stored_value() {
    let r = rig();
    assert_eq!(r.core.settings(), None);
    r.core.set_settings(StreamOptions { fps: 30, ..Default::default() }).unwrap();
    assert!(r.core.set_settings(StreamOptions { fps: 31, ..Default::default() }).is_err());
    // the stream always runs at 60: a saved 30 comes back as 60
    assert_eq!(r.core.settings().unwrap().fps, 60);
    let reopened = ConsoleStore::open(r.dir.path().join("state.json")).unwrap();
    assert_eq!(reopened.settings().unwrap().fps, 60);
}

#[test]
fn settings_read_from_a_hand_edited_file_are_validated_and_dropped_if_bad() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("state.json");
    std::fs::write(&p, r#"{"settings":{"width":1,"height":1234,"fps":999,"bitrate":-7,"hevc":false,"hdr":true,"volume":250,"upscale":"x","sharpness":255}}"#).unwrap();
    let s = ConsoleStore::open(&p).unwrap();
    assert!(s.settings().is_none(), "an invalid saved setting must not be handed out");
}

#[test]
fn host_validation_boundaries() {
    for good in [
        "192.168.1.20",
        "::1",
        "[::1]",
        "fe80::1",
        "ps5.local",
        "a",
        "a-b.c",
        &"a".repeat(63),
        "ps5.example.com",
        "0.0.0.0",
        "255.255.255.255",
        "127.0.0.1",
    ] {
        assert!(validate_host(good).is_ok(), "{good}");
    }
    for bad in [
        "",
        " ",
        "a b",
        "a\n",
        "a\r\nHost: x",
        "host:9295",
        "http://h",
        "h/x",
        "u@h",
        "-a",
        "a-",
        "a..b",
        ".a",
        "a.",
        "1.2.3",
        "300.1.1.1",
        "ps5_local",
        "[::1",
        "::1]",
        "fe80::1%en0",
        "é.local",
        &"a".repeat(64),
        &format!("{}.{}", "a".repeat(63), "b".repeat(200)),
        "\0",
        "a\0b",
        "[1.2.3.4]x",
    ] {
        assert!(validate_host(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn pin_and_version_validation() {
    assert_eq!(parse_pin("12345678"), Ok(12_345_678));
    assert_eq!(parse_pin(" 00000001 "), Ok(1));
    for bad in [
        "",
        "1234567",
        "123456789",
        "+1234567",
        "-1234567",
        "1234 678",
        "１２３４５６７８",
        "0x123456",
        "12345678\n9",
        "1e7000000",
    ] {
        assert!(parse_pin(bad).is_err(), "{bad:?}");
    }
    for v in ["1.0", "9.0", "8.0", "10.0"] {
        assert!(validate_version(v).is_ok());
    }
    for v in ["", "1.0\r\nX: y", "11.0", "1", "1.0 "] {
        assert!(validate_version(v).is_err());
    }
}

#[test]
fn registration_input_is_validated_before_the_backend_is_called() {
    let r = rig();
    r.core.psn_complete("x?code=1").unwrap();
    assert!(r.core.register("not a host", Device::Ps5, "12345678", None).is_err());
    assert!(r.core.register("10.0.0.2", Device::Ps5, "1234", None).is_err());
    assert!(r.core.register("10.0.0.2", Device::Ps5, "12345678", Some("2.0")).is_err());
    // the fake backend answers every registration with an error: the store stays empty of new consoles
    assert!(r.core.register("10.0.0.2", Device::Ps5, "12345678", None).is_err());
    assert_eq!(r.core.consoles().len(), 1);
}

#[test]
fn a_garbage_stored_psn_account_id_is_an_error_not_a_panic() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("state.json");
    std::fs::write(&p, r#"{"psn_account":{"online_id":"x","account_id":"18446744073709551616"}}"#).unwrap();
    let fb = Arc::new(FB::default());
    let core = Core::new(CoreBuilder {
        store: ConsoleStore::open(&p).unwrap(),
        secrets: Arc::new(MemoryStore::default()),
        backend: fb,
        psn: Some(Arc::new(FPsn)),
        events: Arc::new(VecSink::default()),
        media: Arc::new(NullMedia),
        settle_s: 0.0,
        senkusha: SenkushaConfig::default(),
    });
    assert!(matches!(core.register("10.0.0.2", Device::Ps5, "12345678", None), Err(CoreError::Store(_))));
    core.set_remote_registrar(Arc::new(Fail));
    assert!(matches!(core.register_remote("d"), Err(CoreError::Store(_))));
}

#[test]
fn a_host_accepted_by_validate_host_must_be_usable_for_wake() {
    let r = rig();
    // a record with a bracketed address passes the store's sanitising on load
    let mut s = ConsoleStore::open(r.dir.path().join("state2.json")).unwrap();
    s.upsert(rec("AABBCCDDEE02", Some("[::1]"))).unwrap();
    assert_eq!(ConsoleStore::open(r.dir.path().join("state2.json")).unwrap().consoles().len(), 1);
    ConsoleStore::put_secrets(
        &*r.secrets,
        "AABBCCDDEE02",
        &ConsoleSecrets { regist_key: "aa".into(), rp_key: "bb".into() },
    )
    .unwrap();
    let core = Core::new(CoreBuilder {
        store: ConsoleStore::open(r.dir.path().join("state2.json")).unwrap(),
        secrets: r.secrets.clone(),
        backend: r.fb.clone(),
        psn: None,
        events: Arc::new(VecSink::default()),
        media: Arc::new(NullMedia),
        settle_s: 0.0,
        senkusha: SenkushaConfig::default(),
    });
    assert_eq!(core.wake("AABBCCDDEE02"), Ok(true), "wake gave {:?}", core.wake("AABBCCDDEE02"));
}

// ---- scan / forget -----------------------------------------------------------------------------------------------------------

#[test]
fn scan_updates_known_addresses_cleans_names_and_reports_once() {
    let r = rig();
    *r.fb.scan.lock().unwrap() = vec![
        Console {
            device: DeviceType::Ps5,
            host_id: "aa:bb:cc:dd:ee:01".into(),
            host_name: "Den\u{202e}\u{0}\n".repeat(40),
            status: Status::Standby,
            addr: "10.1.2.3".parse().unwrap(),
        },
        Console {
            device: DeviceType::Ps4,
            host_id: "112233445566".into(),
            host_name: "Other".into(),
            status: Status::Other(9),
            addr: "10.1.2.4".parse().unwrap(),
        },
    ];
    let found = r.core.scan(&[]).unwrap();
    assert_eq!(found.len(), 2);
    assert_eq!(found[0].registered_id.as_deref(), Some(MAC));
    assert_eq!(found[1].registered_id, None);
    assert!(found[0].name.chars().count() <= MAX_NAME_CHARS && !found[0].name.contains('\u{202e}'));
    assert_eq!(r.core.consoles()[0].host.as_deref(), Some("10.1.2.3"));
    assert_eq!(r.events.events().iter().filter(|e| **e == AppEvent::ConsolesChanged).count(), 1);
}

#[test]
fn forget_removes_record_and_secrets_and_unknown_ids_are_reported() {
    let r = rig();
    assert_eq!(r.core.forget("000000000000"), Err(CoreError::UnknownConsole));
    r.core.forget("aa:bb:cc:dd:ee:01").unwrap();
    assert!(ConsoleStore::get_secrets(&*r.secrets, MAC).is_none());
    assert_eq!(r.core.forget(MAC), Err(CoreError::UnknownConsole));
}

// ---- store: corruption, partial writes, permissions -------------------------------------------------------------------------

fn read_only_dir() -> Option<(tempfile::TempDir, std::path::PathBuf)> {
    use std::os::unix::fs::PermissionsExt;
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("state.json");
    std::fs::set_permissions(d.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
    if std::fs::write(d.path().join("probe"), b"x").is_ok() {
        std::fs::set_permissions(d.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        return None; // running as root: permissions do not apply
    }
    Some((d, p))
}
fn restore(d: &tempfile::TempDir) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(d.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
}

#[test]
fn a_failed_save_is_an_error_and_leaves_no_temp_file() {
    let Some((d, p)) = read_only_dir() else { return };
    let mut s = ConsoleStore::open(&p).unwrap();
    assert!(s.upsert(rec(MAC, None)).is_err());
    restore(&d);
    assert!(!d.path().join("state.json.tmp").exists());
    assert!(!p.exists());
}

#[test]
fn a_failed_save_must_not_leave_the_change_in_memory() {
    let Some((d, p)) = read_only_dir() else { panic!("cannot make a read-only dir (root?)") };
    let mut s = ConsoleStore::open(&p).unwrap();
    assert!(s.upsert(rec(MAC, None)).is_err());
    assert!(s.set_psn_account(Some(PsnAccountInfo { online_id: "x".into(), account_id: "1".into() })).is_err());
    restore(&d);
    assert!(s.consoles().is_empty(), "memory says paired, disk says not");
    assert!(s.psn_account().is_none());
}

#[test]
fn a_truncated_or_hostile_state_file_is_an_error_and_is_left_for_recovery() {
    let d = tempfile::tempdir().unwrap();
    let full = r#"{"consoles":[{"id":"AABBCCDDEE01","device":"ps5","nickname":"n","ap_name":"a","version":"1.0","host":null}]}"#;
    for cut in [1, 10, 40, full.len() - 1] {
        let p = d.path().join(format!("t{cut}.json"));
        std::fs::write(&p, &full[..cut]).unwrap();
        assert!(ConsoleStore::open(&p).is_err(), "cut at {cut}");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), &full[..cut]);
    }
    for (i, bad) in [
        &b"\xff\xfe\x00"[..],
        b"null",
        b"{\"consoles\":5}",
        b"{\"consoles\":[{\"id\":1}]}",
        b"{\"consoles\":[{\"id\":\"A\",\"device\":\"ps3\"}]}",
    ]
    .iter()
    .enumerate()
    {
        let p = d.path().join(format!("h{i}.json"));
        std::fs::write(&p, bad).unwrap();
        assert!(ConsoleStore::open(&p).is_err(), "{bad:?}");
    }
    // a JSON array is not a state file either (serde would accept it for the struct)
    for text in ["[]", "[[],null,null]"] {
        let arr = d.path().join("arr.json");
        std::fs::write(&arr, text).unwrap();
        assert!(ConsoleStore::open(&arr).is_err(), "{text}");
        assert_eq!(std::fs::read_to_string(&arr).unwrap(), text);
    }
    let deep = d.path().join("deep.json");
    std::fs::write(&deep, "[".repeat(300_000)).unwrap();
    assert!(ConsoleStore::open(&deep).is_err());
    assert!(ConsoleStore::open(d.path()).is_err(), "a directory");
    // an empty file is also an error (not silently an empty store)
    let e = d.path().join("empty.json");
    std::fs::write(&e, "").unwrap();
    assert!(ConsoleStore::open(&e).is_err());
    // unknown fields from a newer version are tolerated and unknown optional sections default
    let n = d.path().join("new.json");
    std::fs::write(&n, r#"{"future":1}"#).unwrap();
    assert!(ConsoleStore::open(&n).unwrap().consoles().is_empty());
}

#[test]
fn a_new_state_file_is_private_and_saves_are_visible_to_a_second_reader() {
    use std::os::unix::fs::PermissionsExt;
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("deep/er/state.json");
    let mut s = ConsoleStore::open(&p).unwrap();
    s.upsert(rec(MAC, Some("10.0.0.1"))).unwrap();
    assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
    assert_eq!(ConsoleStore::open(&p).unwrap().consoles().len(), 1);
}

#[test]
fn a_stale_loose_temp_file_must_not_loosen_the_state_file() {
    use std::os::unix::fs::PermissionsExt;
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("state.json");
    let tmp = d.path().join("state.json.tmp");
    std::fs::write(&tmp, b"junk").unwrap();
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o644)).unwrap();
    let mut s = ConsoleStore::open(&p).unwrap();
    s.upsert(rec(MAC, None)).unwrap();
    assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
}

#[test]
fn a_hand_edited_non_canonical_id_must_be_normalised_on_load() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("state.json");
    std::fs::write(&p, r#"{"consoles":[{"id":"aa:bb:cc:dd:ee:01","device":"ps5","nickname":"n","ap_name":"a","version":"1.0","host":"10.0.0.1"}]}"#).unwrap();
    let s = ConsoleStore::open(&p).unwrap();
    assert_eq!(s.consoles().len(), 1, "listed...");
    assert!(s.get("aa:bb:cc:dd:ee:01").is_some(), "...but not addressable");
}

#[test]
fn duplicate_console_records_must_collapse_on_load() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("state.json");
    let one = |n: &str| {
        format!(r#"{{"id":"AABBCCDDEE01","device":"ps5","nickname":"{n}","ap_name":"a","version":"1.0","host":null}}"#)
    };
    std::fs::write(&p, format!(r#"{{"consoles":[{},{}]}}"#, one("a"), one("b"))).unwrap();
    assert_eq!(ConsoleStore::open(&p).unwrap().consoles().len(), 1);
}

#[test]
fn a_hand_edited_protocol_version_must_be_validated_on_load() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("state.json");
    std::fs::write(&p, r#"{"consoles":[{"id":"AABBCCDDEE01","device":"ps5","nickname":"n","ap_name":"a","version":"1.0\r\nX-Evil: 1","host":null}]}"#).unwrap();
    let s = ConsoleStore::open(&p).unwrap();
    assert!(s.consoles().iter().all(|c| validate_version(&c.version).is_ok()), "{:?}", s.consoles());
}

#[test]
fn two_store_instances_must_not_overwrite_each_others_consoles() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("state.json");
    let mut app = ConsoleStore::open(&p).unwrap();
    let mut tool = ConsoleStore::open(&p).unwrap();
    tool.upsert(rec("AABBCCDDEE01", None)).unwrap();
    app.upsert(rec("AABBCCDDEE02", None)).unwrap();
    assert_eq!(ConsoleStore::open(&p).unwrap().consoles().len(), 2);
}

#[test]
fn clean_text_basics() {
    assert_eq!(clean_text("  a\u{202e}b\u{200b}c\0\n ", 10), "abc");
    assert_eq!(clean_text(&"é".repeat(100), 5).chars().count(), 5);
    assert_eq!(clean_text("", 5), "");
    assert_eq!(clean_text("abc", 0), "");
}

#[test]
fn clean_text_must_drop_line_separators_and_other_invisible_formatting() {
    for c in ['\u{2028}', '\u{2029}', '\u{061C}', '\u{180E}', '\u{E0041}'] {
        let s = clean_text(&format!("a{c}b"), 10);
        assert_eq!(s, "ab", "U+{:04X} survived", c as u32);
    }
}

// ---- netlink ---------------------------------------------------------------------------------------------------------------------

#[test]
fn netlink_parsers_survive_garbage() {
    let junk = [
        "",
        "\0",
        "media:",
        "media: (",
        "media: ()",
        "media: autoselect (Gbase",
        "media: autoselect (base)",
        "media: autoselect (-5baseT)",
        "media: autoselect (0baseT)",
        "interface:",
        "interface: \u{e9}0",
        "gateway: x\ninterface: en0",
        "{",
        "{\"_name\":\"en0\"}",
        "[[[[[[",
        "Device:",
        "Hardware Port:\nDevice: en0",
    ];
    for j in junk {
        let _ = netlink::parse_media_mbps(j);
        let _ = netlink::parse_route(j);
        let _ = netlink::parse_wifi_rate(j, "en0");
        let _ = netlink::is_wifi_port(j, "en0");
    }
    assert!(netlink::parse_wifi_rate(&"[".repeat(300_000), "en0").is_none());
    assert_eq!(netlink::parse_media_mbps("media: autoselect (0baseT)"), None);
    assert_eq!(netlink::parse_route("gateway: 1.2.3.4\n interface: en0\n"), Some(("en0".into(), true)));
    assert_eq!(netlink::parse_route("interface: en0 \n"), Some(("en0".into(), false)));
    assert_eq!(
        netlink::parse_wifi_rate(
            r#"{"_name":"en0","spairport_current_network_information":{"spairport_network_rate":0}}"#,
            "en0"
        ),
        None
    );
    // Auto policy never exceeds the cap, whatever the link claims
    for m in [0, 1, 99, 100, 125, 126, 1000, u32::MAX] {
        for medium in [netlink::Medium::Wired, netlink::Medium::WiFi] {
            let k = netlink::auto_kbps(&netlink::Link { interface: "en0".into(), medium, mbps: Some(m), direct: true });
            assert!(k.is_none_or(|k| (0..=netlink::MAX_AUTO_KBPS).contains(&k)), "{m}: {k:?}");
        }
    }
    // a zero Mbit/s wired link keeps the default instead of asking for 0 kbit/s
    assert_eq!(
        netlink::auto_kbps(&netlink::Link {
            interface: "e".into(),
            medium: netlink::Medium::Wired,
            mbps: Some(0),
            direct: true
        }),
        None
    );
}

#[test]
fn a_silly_media_line_must_not_panic() {
    assert_eq!(netlink::parse_media_mbps("media: autoselect (5000000Gbase-T <full-duplex>)"), None);
    assert_eq!(netlink::parse_media_mbps("media: autoselect (10Gbase-T <full-duplex>)"), Some(10_000));
}

// ---- PSN adapter ---------------------------------------------------------------------------------------------------------------------

mod adapter {
    use super::*;
    use openps_core::psn::PsnAdapter;
    use openps_psn::config::PsnConfig;
    use openps_psn::http::{HttpClient, HttpRequest, HttpResponse};
    use openps_psn::PsnClient;

    struct Net(Arc<Mutex<Vec<HttpRequest>>>, u16);
    impl HttpClient for Net {
        fn execute(&self, r: &HttpRequest) -> io::Result<HttpResponse> {
            self.0.lock().unwrap().push(r.clone());
            Ok(HttpResponse { status: self.1, headers: vec![], body: b"{}".to_vec() })
        }
    }
    fn adapter(status: u16) -> (PsnAdapter<Net, MemoryStore>, Arc<Mutex<Vec<HttpRequest>>>) {
        let cfg = PsnConfig {
            client_id: "c".into(),
            client_secret: "SECRET-CLIENT".into(),
            token_url: "https://auth.example.test/2.0/oauth/token".into(),
            redirect_uri: "https://example.test/redirect".into(),
            scope: "a".into(),
            api: None,
        };
        let log = Arc::new(Mutex::new(vec![]));
        (PsnAdapter::new(PsnClient::new(cfg, Net(log.clone(), status), MemoryStore::default())), log)
    }

    #[test]
    fn errors_never_contain_the_authorisation_code_or_client_secret() {
        for status in [400, 401, 500, 200] {
            let (a, _) = adapter(status);
            let e = a.complete("https://example.test/redirect?code=SECRETCODE").unwrap_err();
            assert!(!e.contains("SECRETCODE") && !e.contains("SECRET-CLIENT"), "{status}: {e}");
        }
        let (a, log) = adapter(200);
        assert_eq!(
            a.complete("https://example.test/redirect?error=access_denied").unwrap_err(),
            "sign-in was cancelled"
        );
        assert!(a.complete("https://evil.test/redirect?code=1").is_err());
        assert!(log.lock().unwrap().is_empty(), "a foreign redirect never triggers a token exchange");
        assert!(
            a.is_redirect("https://example.test/redirect?code=1")
                && !a.is_redirect("https://example.test.evil/redirect")
        );
        assert!(
            !a.api_available()
                && a.api_consoles().is_err()
                && a.api_art("https://image.api.playstation.com/a.png").is_err()
        );
    }

    #[test]
    fn the_authorize_url_carries_no_secret_and_a_fresh_device_id_each_time() {
        let (a, _) = adapter(200);
        let (u1, u2) = (a.authorize_url().unwrap(), a.authorize_url().unwrap());
        assert!(!u1.contains("SECRET-CLIENT"));
        assert_ne!(u1, u2, "duid is random");
    }
}

// ---- added with the fixes ---------------------------------------------------------------------------------------------------------

#[test]
fn store_updates_merge_with_the_file_and_set_hosts_writes_once() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("state.json");
    let mut a = ConsoleStore::open(&p).unwrap();
    let mut b = ConsoleStore::open(&p).unwrap();
    a.upsert(rec("AABBCCDDEE01", None)).unwrap();
    b.upsert(rec("AABBCCDDEE02", None)).unwrap();
    a.set_hosts(&[("aabbccddee01".into(), "10.0.0.1".into()), ("AABBCCDDEE02".into(), "[::1]".into())]).unwrap();
    let again = ConsoleStore::open(&p).unwrap();
    assert_eq!(again.get("AABBCCDDEE01").unwrap().host.as_deref(), Some("10.0.0.1"));
    assert_eq!(again.get("AABBCCDDEE02").unwrap().host.as_deref(), Some("::1"), "brackets are stripped");
    // a failed update (corrupt file) changes nothing in memory
    std::fs::write(&p, "{ broken").unwrap();
    assert!(a.upsert(rec("AABBCCDDEE03", None)).is_err());
    assert!(a.get("AABBCCDDEE03").is_none());
    assert_eq!(std::fs::read_to_string(&p).unwrap(), "{ broken");
}

struct FailingSignOut;
impl PsnService for FailingSignOut {
    fn authorize_url(&self) -> Result<String, String> {
        Ok(String::new())
    }
    fn complete(&self, _: &str) -> Result<PsnAccountInfo, String> {
        Ok(PsnAccountInfo { online_id: "me".into(), account_id: "1".into() })
    }
    fn sign_out(&self) {}
    fn try_sign_out(&self) -> Result<(), String> {
        Err("keychain locked".into())
    }
}

#[test]
fn a_failed_psn_sign_out_is_reported_and_keeps_the_account() {
    let d = tempfile::tempdir().unwrap();
    let core = Core::new(CoreBuilder {
        store: ConsoleStore::open(d.path().join("state.json")).unwrap(),
        secrets: Arc::new(MemoryStore::default()),
        backend: Arc::new(FB::default()),
        psn: Some(Arc::new(FailingSignOut)),
        events: Arc::new(VecSink::default()),
        media: Arc::new(NullMedia),
        settle_s: 0.0,
        senkusha: SenkushaConfig::default(),
    });
    core.psn_complete("x?code=1").unwrap();
    assert!(matches!(core.psn_sign_out(), Err(CoreError::Console(m)) if m.contains("keychain locked")));
    assert!(core.psn_account().is_some());
}
