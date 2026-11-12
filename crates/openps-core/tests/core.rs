//! The application core against a fake backend. Sessions run on the REAL runtime, handshake and stream engines
//! over in-memory links to testkit consoles; only the network edges (discovery, HTTP pairing, sockets) are faked.
use openps_core::backend::Backend;
use openps_core::control::{ControlCommand, ControlLink};
use openps_core::events::{AppEvent, MediaSink, VecSink};
use openps_core::store::ConsoleStore;
use openps_core::types::*;
use openps_core::{Core, CoreBuilder, CoreError, PsnService, RemoteRegistrar, RemoteRequest};
use openps_crypto::RpCipher;
use openps_discovery::{Console, DeviceType, Status};
use openps_psn::store::{MemoryStore, SecretStore};
use openps_rp::client::ClientError;
use openps_rp::ctrl::Inbound;
use openps_rp::regist::{RegisterError, RegisterParams, Registered};
use openps_rp::session::{CtrlInfo, StreamingType};
use openps_session::link::{mem_pair, Link};
use openps_stream::video::Frame;
use openps_takion::senkusha::SenkushaConfig;
use openps_testkit::handshake::HandshakeConsole;
use openps_testkit::senkusha::SenkushaConsole;
use openps_testkit::stream::{encode_frame, frame_data};
use std::io;
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const MAC: &str = "AA:BB:CC:DD:EE:01";

// ---- fakes ---------------------------------------------------------------------------------------------------------------
struct FakeControl {
    prohibited: bool,
    version: &'static str,
}

impl ControlLink for FakeControl {
    fn version(&self) -> String {
        self.version.into()
    }
    fn session_id(&self) -> String {
        "fake-session".into()
    }
    fn cipher(&self) -> RpCipher {
        RpCipher::new([1; 16], [2; 16], [3; 16]) // the testkit console decrypts launch specs with this
    }
    fn info(&self) -> CtrlInfo {
        CtrlInfo { server_type: Some(1), full_hd: true, prohibited: self.prohibited }
    }
    fn take_pending(&mut self) -> Vec<Inbound> {
        vec![Inbound::DisplayStatus { can_display: true }]
    }
    fn poll(&mut self, timeout: Duration) -> io::Result<Vec<Inbound>> {
        std::thread::sleep(timeout.min(Duration::from_millis(10)));
        Ok(vec![])
    }
    fn send(&mut self, _: ControlCommand) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Default)]
struct Fake {
    scan: Mutex<Vec<Console>>,
    register: Mutex<Option<Result<Registered, ClientError>>>,
    register_calls: Mutex<Vec<(String, u32, u64, String)>>, // host, pin, account id, version
    wakes: Mutex<Vec<(DeviceType, IpAddr, String)>>,
    control_error: Mutex<Option<ClientError>>,
    control_calls: std::sync::atomic::AtomicUsize,
    prohibited: AtomicBool,
    control_version: Mutex<Option<&'static str>>,
    /// Consoles serving the next `open_links` call.
    serve: AtomicBool,
    frames_to_send: Mutex<u16>,
    console_threads: Mutex<Vec<std::thread::JoinHandle<()>>>,
    stop: Arc<AtomicBool>,
    /// `open_links` returns a stream link whose sink panics (a runtime thread dies).
    dead_links: AtomicBool,
    /// The streaming type of every `open_control` call, in order.
    streaming_calls: Mutex<Vec<StreamingType>>,
}

impl Backend for Fake {
    fn scan(&self, _: &[IpAddr]) -> io::Result<Vec<Console>> {
        Ok(self.scan.lock().unwrap().clone())
    }
    fn wake(&self, d: DeviceType, ip: IpAddr, key: &str) -> Result<bool, String> {
        self.wakes.lock().unwrap().push((d, ip, key.into()));
        Ok(true)
    }
    fn register(&self, host: &str, p: &RegisterParams) -> Result<Registered, ClientError> {
        self.register_calls.lock().unwrap().push((host.into(), p.pin, p.account_id, p.version.into()));
        self.register.lock().unwrap().take().expect("test set a registration result")
    }
    fn open_control(
        &self,
        _: &str,
        _: DeviceType,
        version: &str,
        _: &Registered,
        streaming: StreamingType,
    ) -> Result<Box<dyn ControlLink>, ClientError> {
        self.control_calls.fetch_add(1, Ordering::Relaxed);
        self.streaming_calls.lock().unwrap().push(streaming);
        if let Some(e) = self.control_error.lock().unwrap().take() {
            return Err(e);
        }
        let v = self.control_version.lock().unwrap().unwrap_or(if version == "1.0" { "1.0" } else { "10.0" });
        Ok(Box::new(FakeControl { prohibited: self.prohibited.load(Ordering::Relaxed), version: v }))
    }
    fn open_links(&self, _: &str) -> io::Result<(Link, Link)> {
        let (mut stream_client, stream_server) = mem_pair();
        let (test_client, test_server) = mem_pair();
        if self.dead_links.load(Ordering::Relaxed) {
            struct PanicSink;
            impl openps_session::link::PacketSink for PanicSink {
                fn send(&mut self, _: &[u8]) -> io::Result<()> {
                    panic!("simulated runtime bug");
                }
            }
            stream_client.sink = Box::new(PanicSink);
        }
        if self.serve.load(Ordering::Relaxed) {
            let frames = *self.frames_to_send.lock().unwrap();
            let stop = self.stop.clone();
            let mut t = self.console_threads.lock().unwrap();
            t.push(std::thread::spawn(move || serve_stream(stream_server, frames, stop)));
            let stop = self.stop.clone();
            t.push(std::thread::spawn(move || serve_senkusha(test_server, stop)));
        }
        Ok((stream_client, test_client))
    }
}

fn serve_stream(mut link: Link, frames: u16, stop: Arc<AtomicBool>) {
    let mut con = HandshakeConsole::new(14);
    con.expect_session_key = Some("fake-session".into());
    let end = Instant::now() + Duration::from_secs(10);
    while !con.done() && Instant::now() < end && !stop.load(Ordering::Relaxed) {
        if let Ok(Some(d)) = link.source.recv(Duration::from_millis(20)) {
            for r in con.on_packet(&d) {
                if link.sink.send(&r).is_err() {
                    return;
                }
            }
        }
    }
    let Some(mut sc) = con.into_stream_console() else { return };
    for id in 0..frames {
        let data = frame_data(500, id as u8);
        for (i, u) in encode_frame(&data, 3, 1).iter().enumerate() {
            if link.sink.send(&sc.video(14, id, i as u16, 4, 1, u)).is_err() {
                return;
            }
        }
    }
    // keep the link open, absorbing client traffic, until told to stop or the client leaves
    while !stop.load(Ordering::Relaxed) {
        if link.source.recv(Duration::from_millis(20)).is_err() {
            return;
        }
    }
}

fn serve_senkusha(mut link: Link, stop: Arc<AtomicBool>) {
    let mut con = SenkushaConsole::new(true, 1400, 1300);
    while !stop.load(Ordering::Relaxed) {
        match link.source.recv(Duration::from_millis(20)) {
            Ok(Some(d)) => {
                for r in con.on_packet(&d) {
                    if link.sink.send(&r).is_err() {
                        return;
                    }
                }
            }
            Ok(None) => {}
            Err(_) => return,
        }
    }
}

struct FakePsn {
    fail: AtomicBool,
    signed_out: AtomicBool,
}

impl PsnService for FakePsn {
    fn authorize_url(&self) -> Result<String, String> {
        Ok("https://auth.example.test/authorize?x=1".into())
    }
    fn complete(&self, url: &str) -> Result<PsnAccountInfo, String> {
        if self.fail.load(Ordering::Relaxed) || !url.contains("code=") {
            return Err("sign-in was cancelled".into());
        }
        Ok(PsnAccountInfo { online_id: "Player_1".into(), account_id: "18446744073709551615".into() })
    }
    fn sign_out(&self) {
        self.signed_out.store(true, Ordering::Relaxed);
    }
    fn remote_access_token(&self) -> Result<String, String> {
        if self.fail.load(Ordering::Relaxed) {
            Err("your PlayStation sign-in has expired: sign in again".into())
        } else {
            Ok("REMOTE-TOKEN".into())
        }
    }
}

#[derive(Default)]
struct Frames(Mutex<Vec<Frame>>);
impl MediaSink for Frames {
    fn video(&self, f: Frame) {
        self.0.lock().unwrap().push(f);
    }
}

struct Rig {
    core: Core,
    fake: Arc<Fake>,
    events: Arc<VecSink>,
    secrets: Arc<MemoryStore>,
    media: Arc<Frames>,
    dir: tempfile::TempDir,
    psn: Arc<FakePsn>,
}

fn rig() -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let fake = Arc::new(Fake::default());
    let events = Arc::new(VecSink::default());
    let secrets = Arc::new(MemoryStore::default());
    let media = Arc::new(Frames::default());
    let psn = Arc::new(FakePsn { fail: AtomicBool::new(false), signed_out: AtomicBool::new(false) });
    let core = build(
        &dir,
        fake.clone(),
        events.clone(),
        secrets.clone(),
        media.clone(),
        Some(psn.clone() as Arc<dyn PsnService>),
    );
    Rig { core, fake, events, secrets, media, dir, psn }
}

fn build(
    dir: &tempfile::TempDir,
    fake: Arc<Fake>,
    events: Arc<VecSink>,
    secrets: Arc<MemoryStore>,
    media: Arc<Frames>,
    psn: Option<Arc<dyn PsnService>>,
) -> Core {
    let core = Core::new(CoreBuilder {
        store: ConsoleStore::open(dir.path().join("state.json")).unwrap(),
        secrets,
        backend: fake,
        psn,
        events,
        media,
        settle_s: 0.0,
        senkusha: SenkushaConfig { io_timeout_s: 0.02, attempts: 2, retry_delay_s: 0.01, ..Default::default() },
    });
    // tests must not depend on the network of the machine they run on: Auto bitrate knows nothing unless a test says otherwise
    core.set_auto_bitrate_probe(|_| None);
    core
}

fn registered(device: DeviceType) -> Registered {
    Registered {
        device,
        mac: MAC.into(),
        nickname: "Den".into(),
        regist_key: "regkey".into(),
        rp_key: "rpkey".into(),
        ap_name: "ap".into(),
    }
}

/// Pair a PS5 so connect tests have something to connect to.
fn paired(r: &Rig) -> ConsoleRecord {
    r.core.psn_complete("https://x/redirect?code=1").unwrap();
    *r.fake.register.lock().unwrap() = Some(Ok(registered(DeviceType::Ps5)));
    r.core.register("10.0.0.9", Device::Ps5, "12345678", None).unwrap()
}

fn wait_state(r: &Rig, limit: Duration, pred: impl Fn(&SessionState) -> bool) -> bool {
    let end = Instant::now() + limit;
    while Instant::now() < end {
        if r.events.states().iter().any(&pred) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

// ---- pairing ---------------------------------------------------------------------------------------------------------------
#[test]
fn ps5_registration_needs_a_psn_login_and_a_valid_pin() {
    let r = rig();
    assert_eq!(r.core.register("10.0.0.9", Device::Ps5, "12345678", None), Err(CoreError::NeedsPsnLogin));
    assert!(r.fake.register_calls.lock().unwrap().is_empty()); // never reached the console
    r.core.psn_complete("https://x/redirect?code=1").unwrap();
    for bad in ["1234567", "123456789", "abcdefgh", ""] {
        assert!(matches!(r.core.register("10.0.0.9", Device::Ps5, bad, None), Err(CoreError::Console(_))), "{bad}");
    }
    assert!(r.fake.register_calls.lock().unwrap().is_empty());
}

#[test]
fn successful_registration_persists_the_record_and_keeps_secrets_in_the_secret_store() {
    let r = rig();
    let rec = paired(&r);
    assert_eq!(
        (rec.id.as_str(), rec.device, rec.version.as_str(), rec.host.as_deref()),
        ("AABBCCDDEE01", Device::Ps5, "1.0", Some("10.0.0.9"))
    );
    // the PSN account id (full u64) went into the request
    assert_eq!(r.fake.register_calls.lock().unwrap()[0], ("10.0.0.9".into(), 12_345_678, u64::MAX, "1.0".into()));
    let file = std::fs::read_to_string(r.dir.path().join("state.json")).unwrap();
    assert!(file.contains("AABBCCDDEE01") && !file.contains("regkey") && !file.contains("rpkey"), "{file}");
    assert!(r.secrets.get("console/AABBCCDDEE01").unwrap().contains("regkey"));
    assert!(r.events.events().contains(&AppEvent::ConsolesChanged));
    // a new Core over the same files sees the pairing (restart)
    let again = build(&r.dir, r.fake.clone(), r.events.clone(), r.secrets.clone(), r.media.clone(), None);
    assert_eq!(again.consoles(), vec![rec]);
}

#[test]
fn a_rejected_registration_reports_the_reason_and_stores_nothing() {
    let r = rig();
    r.core.psn_complete("https://x/redirect?code=1").unwrap();
    *r.fake.register.lock().unwrap() =
        Some(Err(ClientError::Register(RegisterError::Rejected { status: 403, reason: "80108bff".into() })));
    let e = r.core.register("10.0.0.9", Device::Ps5, "12345678", None).unwrap_err();
    assert!(matches!(&e, CoreError::Console(m) if m.contains("wrong credentials")), "{e}");
    assert!(r.core.consoles().is_empty() && r.secrets.get("console/AABBCCDDEE01").is_none());
}

#[test]
fn legacy_registration_uses_the_online_id_and_ps4_defaults_to_version_10() {
    let r = rig();
    r.core.psn_complete("https://x/redirect?code=1").unwrap();
    *r.fake.register.lock().unwrap() = Some(Ok(registered(DeviceType::Ps4)));
    r.core.register("10.0.0.5", Device::Ps4, "00000001", None).unwrap();
    assert_eq!(r.fake.register_calls.lock().unwrap()[0].3, "10.0");
    *r.fake.register.lock().unwrap() = Some(Ok(registered(DeviceType::Ps4)));
    r.core.register("10.0.0.5", Device::Ps4, "00000001", Some("8.0")).unwrap();
    assert_eq!(r.fake.register_calls.lock().unwrap()[1].3, "8.0");
}

// ---- psn -------------------------------------------------------------------------------------------------------------------
#[test]
fn psn_sign_in_and_out() {
    let r = rig();
    assert!(r.core.psn_authorize_url().unwrap().starts_with("https://auth.example.test/"));
    assert_eq!(r.core.psn_account(), None);
    r.psn.fail.store(true, Ordering::Relaxed);
    assert!(
        matches!(r.core.psn_complete("https://x/redirect?error=access_denied"), Err(CoreError::Console(m)) if m.contains("cancelled"))
    );
    r.psn.fail.store(false, Ordering::Relaxed);
    let a = r.core.psn_complete("https://x/redirect?code=abc").unwrap();
    assert_eq!(a.account_id, "18446744073709551615");
    assert_eq!(r.core.psn_account(), Some(a));
    assert!(r.events.events().contains(&AppEvent::PsnChanged { signed_in: true }));
    r.core.psn_sign_out().unwrap();
    assert!(r.psn.signed_out.load(Ordering::Relaxed) && r.core.psn_account().is_none());
    assert!(r.events.events().contains(&AppEvent::PsnChanged { signed_in: false }));
}

#[test]
fn psn_is_reported_as_unconfigured_when_there_is_no_service() {
    let dir = tempfile::tempdir().unwrap();
    let core = build(
        &dir,
        Arc::new(Fake::default()),
        Arc::new(VecSink::default()),
        Arc::new(MemoryStore::default()),
        Arc::new(Frames::default()),
        None,
    );
    assert_eq!(core.psn_authorize_url(), Err(CoreError::PsnNotConfigured));
    assert_eq!(core.psn_complete("x"), Err(CoreError::PsnNotConfigured));
}

// ---- discovery / wake / forget -------------------------------------------------------------------------------------------------
#[test]
fn scan_marks_paired_consoles_and_refreshes_their_address() {
    let r = rig();
    paired(&r);
    *r.fake.scan.lock().unwrap() = vec![
        Console {
            device: DeviceType::Ps5,
            host_id: "aabbccddee01".into(),
            host_name: "Den".into(),
            status: Status::Standby,
            addr: "10.0.0.77".parse().unwrap(),
        },
        Console {
            device: DeviceType::Ps4,
            host_id: "112233445566".into(),
            host_name: "Other".into(),
            status: Status::Ready,
            addr: "10.0.0.8".parse().unwrap(),
        },
    ];
    let found = r.core.scan(&[]).unwrap();
    assert_eq!(found[0].registered_id.as_deref(), Some("AABBCCDDEE01"));
    assert_eq!((found[0].status.as_str(), found[1].registered_id.clone()), ("standby", None));
    assert_eq!(r.core.consoles()[0].host.as_deref(), Some("10.0.0.77")); // moved on the network: address follows
}

#[test]
fn wake_uses_the_stored_key_and_address() {
    let r = rig();
    let rec = paired(&r);
    assert!(r.core.wake(&rec.id).unwrap());
    assert_eq!(r.fake.wakes.lock().unwrap()[0], (DeviceType::Ps5, "10.0.0.9".parse().unwrap(), "regkey".to_string()));
    assert_eq!(r.core.wake("nope"), Err(CoreError::UnknownConsole));
}

#[test]
fn forget_removes_the_record_and_its_secrets() {
    let r = rig();
    let rec = paired(&r);
    r.core.forget(&rec.id).unwrap();
    assert!(r.core.consoles().is_empty() && r.secrets.get("console/AABBCCDDEE01").is_none());
    assert_eq!(r.core.forget(&rec.id), Err(CoreError::UnknownConsole));
}

// ---- sessions --------------------------------------------------------------------------------------------------------------------
#[test]
fn a_full_session_runs_end_to_end_and_shuts_down_cleanly() {
    let r = rig();
    let rec = paired(&r);
    r.fake.serve.store(true, Ordering::Relaxed);
    *r.fake.frames_to_send.lock().unwrap() = 2;
    r.core.connect(&rec.id, StreamOptions::default()).unwrap();
    assert!(matches!(r.core.connect(&rec.id, StreamOptions::default()), Err(CoreError::AlreadyConnected)));
    assert!(
        wait_state(&r, Duration::from_secs(15), |s| matches!(s, SessionState::Connected { .. })),
        "{:?}",
        r.events.states()
    );
    // video flows to the media sink; the UI controls work while connected
    let end = Instant::now() + Duration::from_secs(5);
    while r.media.0.lock().unwrap().len() < 2 && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(10));
    }
    let ids: Vec<u16> = r.media.0.lock().unwrap().iter().map(|f| f.id).collect();
    assert_eq!(ids, vec![0, 1]);
    let mut pad = openps_input::state::ControllerState::default();
    pad.buttons.insert(openps_input::state::Button::CROSS);
    r.core.set_pad(pad).unwrap();
    r.core.request_keyframe().unwrap();
    r.core.control(ControlCommand::MicRequest).unwrap();
    r.core.disconnect().unwrap();
    assert!(r.core.wait_idle(Duration::from_secs(10)));
    r.fake.stop.store(true, Ordering::Relaxed);
    let states = r.events.states();
    let pos = |p: fn(&SessionState) -> bool| {
        states.iter().position(p).unwrap_or_else(|| panic!("missing state in {states:?}"))
    };
    assert!(pos(|s| matches!(s, SessionState::Connecting)) < pos(|s| matches!(s, SessionState::TestingNetwork)));
    assert!(
        pos(|s| matches!(s, SessionState::TestingNetwork)) < pos(|s| matches!(s, SessionState::Handshaking { .. }))
    );
    assert!(
        pos(|s| matches!(s, SessionState::Handshaking { .. })) < pos(|s| matches!(s, SessionState::Connected { .. }))
    );
    assert_eq!(states.last(), Some(&SessionState::Closed { reason: None }));
    assert_eq!(r.core.session_state(), SessionState::Closed { reason: None });
    // control messages the console sent while we waited were surfaced
    assert!(r.events.events().iter().any(|e| matches!(e, AppEvent::Control { kind, .. } if kind == "display_status")));
    assert_eq!(r.core.set_pad(Default::default()), Err(CoreError::NotConnected));
    r.core.shutdown();
}

#[test]
fn a_session_can_be_started_again_after_it_ends() {
    let r = rig();
    let rec = paired(&r);
    *r.fake.control_error.lock().unwrap() = Some(ClientError::Http("boom".into()));
    r.core.connect(&rec.id, StreamOptions::default()).unwrap();
    assert!(r.core.wait_idle(Duration::from_secs(5)));
    r.fake.serve.store(true, Ordering::Relaxed);
    r.core.connect(&rec.id, StreamOptions::default()).unwrap(); // not stuck on "already connected"
    assert!(wait_state(&r, Duration::from_secs(15), |s| matches!(s, SessionState::Connected { .. })));
    r.core.shutdown();
    r.fake.stop.store(true, Ordering::Relaxed);
}

#[test]
fn an_instant_failure_cannot_leave_a_stale_active_entry() {
    // Regression for a race: if the driver fails before `connect` returns, its cleanup must not run before the
    // entry is registered, or `connect` would store an entry nobody ever clears.
    for _ in 0..30 {
        let r = rig();
        let rec = paired(&r);
        *r.fake.control_error.lock().unwrap() = Some(ClientError::Http("instant".into()));
        r.core.connect(&rec.id, StreamOptions::default()).unwrap();
        assert!(r.core.wait_idle(Duration::from_secs(5)), "stuck");
        assert!(matches!(r.core.session_state(), SessionState::Failed { .. }));
        r.core.shutdown();
    }
}

#[test]
fn setup_failures_become_failed_states_with_a_reason() {
    let r = rig();
    let rec = paired(&r);
    *r.fake.control_error.lock().unwrap() = Some(ClientError::Http("console unreachable".into()));
    r.core.connect(&rec.id, StreamOptions::default()).unwrap();
    assert!(r.core.wait_idle(Duration::from_secs(5)));
    assert!(
        matches!(r.events.states().last(), Some(SessionState::Failed { reason }) if reason.contains("console unreachable"))
    );

    r.fake.prohibited.store(true, Ordering::Relaxed);
    r.core.connect(&rec.id, StreamOptions::default()).unwrap();
    assert!(r.core.wait_idle(Duration::from_secs(5)));
    assert!(
        matches!(r.events.states().last(), Some(SessionState::Failed { reason }) if reason.contains("does not allow streaming"))
    );
    r.core.shutdown();
}

#[test]
fn connect_validates_before_starting_anything() {
    let r = rig();
    assert_eq!(r.core.connect("nope", StreamOptions::default()), Err(CoreError::UnknownConsole));
    let rec = paired(&r);
    // no known address
    let mut store = ConsoleStore::open(r.dir.path().join("state.json")).unwrap();
    store.upsert(ConsoleRecord { host: None, ..rec.clone() }).unwrap();
    let core = build(&r.dir, r.fake.clone(), r.events.clone(), r.secrets.clone(), r.media.clone(), None);
    assert_eq!(core.connect(&rec.id, StreamOptions::default()), Err(CoreError::NoAddress));
    assert!(r.events.states().is_empty(), "nothing was announced for a request that never started");
}

#[test]
fn a_protocol_version_downgrade_found_during_setup_is_remembered() {
    let r = rig();
    *r.fake.register.lock().unwrap() = Some(Ok(registered(DeviceType::Ps4)));
    r.core.psn_complete("https://x/redirect?code=1").unwrap();
    let rec = r.core.register("10.0.0.5", Device::Ps4, "00000001", None).unwrap();
    assert_eq!(rec.version, "10.0");
    *r.fake.control_version.lock().unwrap() = Some("9.0"); // init had to downgrade
    r.fake.serve.store(true, Ordering::Relaxed);
    r.core.connect(&rec.id, StreamOptions::default()).unwrap();
    assert!(wait_state(&r, Duration::from_secs(20), |s| !matches!(s, SessionState::Connecting)));
    r.core.shutdown();
    r.fake.stop.store(true, Ordering::Relaxed);
    assert_eq!(r.core.consoles()[0].version, "9.0");
}

// ---- settings --------------------------------------------------------------------------------------------------------------
#[test]
fn settings_are_validated_persisted_and_survive_restart() {
    let r = rig();
    assert_eq!(r.core.settings(), None, "nothing saved until the user changes something");
    let o = StreamOptions {
        width: 1920,
        height: 1080,
        fps: 60,
        bitrate: 15000,
        hevc: true,
        hdr: false,
        volume: 35,
        upscale: "1.33x".into(),
        display_fps: 144,
        frame_gen: 20,
        framegen_on: true,
        linear_light: false,
        sharpness_on: false,
        rcas_on: true,
        saturation_on: false,
        frame_mode: "smooth".into(),
        camera_comp: true,
        engine: "fsr".into(),
        interpolator: "dynamic".into(),
        sharpness: 50,
        rcas: 100,
        saturation: 100,
        immersion: true,
        debug: false,
        rumble_pct: 120,
        trigger_pct: 80,
        settings_version: openps_core::types::SETTINGS_VERSION,
    };
    r.core.set_settings(o.clone()).unwrap();
    assert_eq!(r.core.settings(), Some(o.clone()));
    // rejected values change nothing
    for bad in [
        StreamOptions { fps: 45, ..o.clone() },
        StreamOptions { height: 999, ..o.clone() },
        StreamOptions { hevc: false, hdr: true, ..o.clone() },
    ] {
        assert!(matches!(r.core.set_settings(bad), Err(CoreError::Console(_))));
    }
    assert_eq!(r.core.settings(), Some(o.clone()));
    // restart over the same files
    let again = build(&r.dir, r.fake.clone(), r.events.clone(), r.secrets.clone(), r.media.clone(), None);
    assert_eq!(again.settings(), Some(o));
}

#[test]
fn retired_mapped_prompt_setting_is_ignored_and_not_persisted() {
    let mut legacy = serde_json::to_value(StreamOptions::default()).unwrap();
    legacy["show_prompts"] = serde_json::Value::Bool(true);

    let options: StreamOptions = serde_json::from_value(legacy).unwrap();
    let saved = serde_json::to_value(options).unwrap();

    assert!(saved.get("show_prompts").is_none());
}

#[test]
fn connect_refuses_invalid_options_before_touching_anything() {
    let r = rig();
    let rec = paired(&r);
    let bad = StreamOptions { fps: 1000, ..Default::default() };
    assert!(matches!(r.core.connect(&rec.id, bad), Err(CoreError::Console(_))));
    assert_eq!(r.core.session_state(), SessionState::Idle);
    assert!(r.events.states().is_empty());
}

// ---- panic safety ------------------------------------------------------------------------------------------------------
/// A decoder/renderer/audio bug must not leave the UI believing a dead session is still running.
struct Bomb {
    armed: AtomicBool,
}
impl MediaSink for Bomb {
    fn video(&self, _f: Frame) {
        if self.armed.swap(false, Ordering::SeqCst) {
            panic!("simulated decoder bug");
        }
    }
}

#[test]
fn a_panicking_media_sink_ends_the_session_cleanly_and_allows_reconnecting() {
    let dir = tempfile::tempdir().unwrap();
    let fake = Arc::new(Fake::default());
    let events = Arc::new(VecSink::default());
    let bomb = Arc::new(Bomb { armed: AtomicBool::new(true) });
    let core = Core::new(CoreBuilder {
        store: ConsoleStore::open(dir.path().join("state.json")).unwrap(),
        secrets: Arc::new(MemoryStore::default()),
        backend: fake.clone(),
        psn: Some(Arc::new(FakePsn { fail: AtomicBool::new(false), signed_out: AtomicBool::new(false) })),
        events: events.clone(),
        media: bomb.clone(),
        settle_s: 0.0,
        senkusha: SenkushaConfig { io_timeout_s: 0.02, attempts: 2, retry_delay_s: 0.01, ..Default::default() },
    });
    core.psn_complete("https://x/redirect?code=1").unwrap();
    *fake.register.lock().unwrap() = Some(Ok(registered(DeviceType::Ps5)));
    let rec = core.register("10.0.0.9", Device::Ps5, "12345678", None).unwrap();
    fake.serve.store(true, Ordering::Relaxed);
    *fake.frames_to_send.lock().unwrap() = 3;
    core.connect(&rec.id, StreamOptions::default()).unwrap();
    // the first video frame blows up; the core must notice, report a failure and tear everything down
    assert!(core.wait_idle(Duration::from_secs(15)), "session never ended: {:?}", events.states());
    let last = events.states().last().cloned();
    assert!(matches!(last, Some(SessionState::Failed { .. })), "{last:?}");
    assert!(matches!(core.session_state(), SessionState::Failed { .. }));
    // and the app is usable again: a second session (the bomb is spent) runs normally
    core.connect(&rec.id, StreamOptions::default()).unwrap();
    assert!(
        {
            let end = Instant::now() + Duration::from_secs(15);
            while Instant::now() < end && !matches!(core.session_state(), SessionState::Connected { .. }) {
                std::thread::sleep(Duration::from_millis(10));
            }
            matches!(core.session_state(), SessionState::Connected { .. })
        },
        "{:?}",
        events.states()
    );
    core.disconnect().unwrap();
    assert!(core.wait_idle(Duration::from_secs(10)));
    fake.stop.store(true, Ordering::Relaxed);
}

#[test]
fn a_dead_session_thread_fails_the_session_instead_of_hanging_on_connecting() {
    let r = rig();
    let rec = paired(&r);
    r.fake.serve.store(true, Ordering::Relaxed); // keep the console end alive: only the panic can end this session
    r.fake.dead_links.store(true, Ordering::Relaxed);
    r.core.connect(&rec.id, StreamOptions::default()).unwrap();
    assert!(r.core.wait_idle(Duration::from_secs(15)), "never ended: {:?}", r.events.states());
    assert!(
        matches!(r.core.session_state(), SessionState::Failed { ref reason } if reason.contains("stopped unexpectedly")),
        "{:?}",
        r.core.session_state()
    );
    // the slot is free again
    r.fake.dead_links.store(false, Ordering::Relaxed);
    *r.fake.frames_to_send.lock().unwrap() = 1;
    r.core.connect(&rec.id, StreamOptions::default()).expect("the failed session released its slot");
    assert!(
        wait_state(&r, Duration::from_secs(15), |s| matches!(s, SessionState::Connected { .. })),
        "{:?}",
        r.events.states()
    );
    r.core.disconnect().unwrap();
    assert!(r.core.wait_idle(Duration::from_secs(10)));
    r.fake.stop.store(true, Ordering::Relaxed);
}

// ---- input hardening ---------------------------------------------------------------------------------------------------
#[test]
fn pins_must_be_exactly_eight_ascii_digits() {
    let r = rig();
    r.core.psn_complete("https://x/redirect?code=1").unwrap();
    for bad in [
        "+1234567",
        "-1234567",
        "1234567 ",
        "12 45678",
        "１２３４５６７８", /* full-width digits */
        "1234567a",
        "0x123456",
        "",
        "١٢٣٤٥٦٧٨", /* arabic-indic */
    ] {
        let e = r.core.register("10.0.0.9", Device::Ps5, bad, None).unwrap_err();
        assert!(matches!(e, CoreError::Console(_)), "{bad:?} -> {e:?}");
    }
    assert!(r.fake.register_calls.lock().unwrap().is_empty(), "nothing reached the network");
    *r.fake.register.lock().unwrap() = Some(Ok(registered(DeviceType::Ps5)));
    assert!(r.core.register("10.0.0.9", Device::Ps5, "00000042", None).is_ok(), "leading zeros are fine");
    assert_eq!(r.fake.register_calls.lock().unwrap()[0].1, 42);
}

#[test]
fn hosts_and_versions_are_validated_before_any_network_use() {
    let r = rig();
    r.core.psn_complete("https://x/redirect?code=1").unwrap();
    for bad in [
        "",
        " ",
        "10.0.0.9\r\nX-Evil: 1",
        "10.0.0.9\n",
        "host name",
        "a/b",
        "http://10.0.0.9",
        "10.0.0.9:9295",
        "evil.test/../x",
        "[::1",
        "-bad.test",
        "bad-.test",
        "a..b",
        &"a".repeat(64),
        &format!("{}.test", "a".repeat(64)),
        &"a.".repeat(130),
        "10.0.0.256.1",
        "\u{202e}evil",
        "ex ample.test",
        "user@10.0.0.9",
        "10.0.0.9#frag",
        "10.0.0.9?x=1",
    ] {
        let e = r.core.register(bad, Device::Ps5, "12345678", None).unwrap_err();
        assert!(matches!(e, CoreError::Console(_)), "{bad:?} -> {e:?}");
    }
    for bad_version in ["", "1.0\r\nX: y", "2.0", "1.0 ", "10", "../1.0", "1.0;"] {
        let e = r.core.register("10.0.0.9", Device::Ps5, "12345678", Some(bad_version)).unwrap_err();
        assert!(matches!(e, CoreError::Console(_)), "{bad_version:?} -> {e:?}");
    }
    assert!(r.fake.register_calls.lock().unwrap().is_empty(), "nothing reached the network");
    for good in ["10.0.0.9", "192.168.1.20", "::1", "[::1]", "fe80::1", "ps5.local", "my-ps5.example.test", "PS5"] {
        *r.fake.register.lock().unwrap() = Some(Ok(registered(DeviceType::Ps5)));
        assert!(r.core.register(good, Device::Ps5, "12345678", None).is_ok(), "{good}");
    }
    for v in ["1.0", "10.0", "9.0", "8.0"] {
        *r.fake.register.lock().unwrap() = Some(Ok(registered(DeviceType::Ps5)));
        assert!(r.core.register("10.0.0.9", Device::Ps4, "12345678", Some(v)).is_ok(), "{v}");
    }
}

#[test]
fn text_from_the_network_is_cleaned_and_bounded_before_it_is_stored_or_shown() {
    let r = rig();
    r.core.psn_complete("https://x/redirect?code=1").unwrap();
    let mut reg = registered(DeviceType::Ps5);
    reg.nickname = format!("  My\u{0}PS5\r\n\u{202e}{}\u{7}  ", "x".repeat(500));
    *r.fake.register.lock().unwrap() = Some(Ok(reg));
    let rec = r.core.register("10.0.0.9", Device::Ps5, "12345678", None).unwrap();
    assert!(rec.nickname.chars().count() <= 64, "{}", rec.nickname.len());
    assert!(!rec.nickname.chars().any(|c| c.is_control() || c == '\u{202e}'), "{:?}", rec.nickname);
    assert!(rec.nickname.starts_with("MyPS5"), "{:?}", rec.nickname);
    // discovery results from any device on the LAN get the same treatment
    *r.fake.scan.lock().unwrap() = vec![Console {
        device: DeviceType::Ps5,
        host_id: "aabbccddee09".into(),
        host_name: format!("<img src=x onerror=alert(1)>\u{0}{}", "y".repeat(400)),
        status: Status::Ready,
        addr: "10.0.0.77".parse().unwrap(),
    }];
    let found = r.core.scan(&[]).unwrap();
    assert!(found[0].name.chars().count() <= 64 && !found[0].name.chars().any(char::is_control), "{:?}", found[0].name);
}

// ---- Auto bitrate: ask for what the link carries, retry once with the default if that fails before connecting ---------------------
#[test]
fn a_failed_attempt_with_a_large_auto_request_is_retried_once_with_the_default() {
    let r = rig();
    let rec = paired(&r);
    let asked = Arc::new(Mutex::new(vec![]));
    let a2 = asked.clone();
    r.core.set_auto_bitrate_probe(move |host| {
        a2.lock().unwrap().push(host.to_string());
        Some(800_000)
    });
    r.fake.serve.store(true, Ordering::Relaxed);
    *r.fake.frames_to_send.lock().unwrap() = 1;
    *r.fake.control_error.lock().unwrap() = Some(ClientError::Http("first attempt refused".into())); // only the first call fails
    r.core.connect(&rec.id, StreamOptions { bitrate: -1, ..Default::default() }).unwrap();
    assert!(
        wait_state(&r, Duration::from_secs(15), |s| matches!(s, SessionState::Connected { .. })),
        "{:?}",
        r.events.states()
    );
    assert_eq!(r.fake.control_calls.load(Ordering::Relaxed), 2, "one retry");
    assert_eq!(
        *asked.lock().unwrap(),
        vec!["10.0.0.9".to_string()],
        "the link was looked at once, for the console's address"
    );
    assert!(
        !r.events.states().iter().any(|s| matches!(s, SessionState::Failed { .. })),
        "the user must not see the first failure: {:?}",
        r.events.states()
    );
    r.core.disconnect().unwrap();
    assert!(r.core.wait_idle(Duration::from_secs(10)));
    r.fake.stop.store(true, Ordering::Relaxed);
    r.core.shutdown();
}

#[test]
fn no_retry_when_auto_asked_for_nothing_special_or_the_bitrate_was_chosen() {
    // auto but nothing known about the link: the default was already used, a second try would change nothing
    let r = rig();
    let rec = paired(&r);
    *r.fake.control_error.lock().unwrap() = Some(ClientError::Http("down".into()));
    r.core.connect(&rec.id, StreamOptions { bitrate: -1, ..Default::default() }).unwrap();
    assert!(r.core.wait_idle(Duration::from_secs(5)));
    assert_eq!(r.fake.control_calls.load(Ordering::Relaxed), 1);
    assert!(matches!(r.core.session_state(), SessionState::Failed { .. }));
    // an explicit bitrate is never second-guessed, even if a probe would have a number
    let r = rig();
    let rec = paired(&r);
    let probed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let p2 = probed.clone();
    r.core.set_auto_bitrate_probe(move |_| {
        p2.store(true, Ordering::Relaxed);
        Some(800_000)
    });
    *r.fake.control_error.lock().unwrap() = Some(ClientError::Http("down".into()));
    r.core.connect(&rec.id, StreamOptions { bitrate: 6000, ..Default::default() }).unwrap();
    assert!(r.core.wait_idle(Duration::from_secs(5)));
    assert_eq!(r.fake.control_calls.load(Ordering::Relaxed), 1);
    assert!(!probed.load(Ordering::Relaxed), "an explicit bitrate does not even look at the link");
    assert!(matches!(r.core.session_state(), SessionState::Failed { .. }));
}

// ---- registering over the internet (spec 09) -----------------------------------------------------------------------------------------
struct FakeRegistrar {
    seen: Mutex<Vec<(String, u64, String)>>,
    outcome: Mutex<Result<Registered, String>>,
    /// Block until cancelled (to test cancel and overlap).
    wait_for_cancel: AtomicBool,
    started: AtomicBool,
}

impl RemoteRegistrar for FakeRegistrar {
    fn register(
        &self,
        req: RemoteRequest,
        cancel: Arc<AtomicBool>,
        progress: &dyn Fn(&str),
    ) -> Result<Registered, String> {
        self.seen.lock().unwrap().push((req.access_token, req.account_id, req.duid));
        progress("Connecting to PlayStation Network");
        self.started.store(true, Ordering::Relaxed);
        if self.wait_for_cancel.load(Ordering::Relaxed) {
            let end = Instant::now() + Duration::from_secs(5);
            while !cancel.load(Ordering::Relaxed) && Instant::now() < end {
                std::thread::sleep(Duration::from_millis(5));
            }
            return Err("cancelled".into());
        }
        self.outcome.lock().unwrap().clone()
    }
}

fn registrar(outcome: Result<Registered, String>) -> Arc<FakeRegistrar> {
    Arc::new(FakeRegistrar {
        seen: Mutex::default(),
        outcome: Mutex::new(outcome),
        wait_for_cancel: AtomicBool::new(false),
        started: AtomicBool::new(false),
    })
}

fn signed_in(r: &Rig) {
    r.core.psn_complete("https://x/redirect?code=1").unwrap();
}

#[test]
fn a_remote_registration_is_saved_like_a_local_one_with_the_keys_only_in_the_secret_store() {
    let r = rig();
    signed_in(&r);
    let reg = registrar(Ok(Registered {
        device: DeviceType::Ps5,
        mac: "98fa2e70c0e7".into(),
        nickname: "PS5-591".into(),
        regist_key: "3a3b3c3d3e3f4041".into(),
        rp_key: "00112233445566778899aabbccddeeff".into(),
        ap_name: "PS5".into(),
    }));
    r.core.set_remote_registrar(reg.clone());
    assert!(r.core.remote_available());
    let rec = r.core.register_remote("00000007aabb").unwrap();
    assert_eq!(
        (rec.id.as_str(), rec.nickname.as_str(), rec.version.as_str(), rec.host.clone()),
        ("98FA2E70C0E7", "PS5-591", "1.0", None)
    );
    // the registrar got the token, the account id (unsigned 64-bit) and the console id
    assert_eq!(*reg.seen.lock().unwrap(), vec![("REMOTE-TOKEN".to_string(), u64::MAX, "00000007aabb".to_string())]);
    let k = ConsoleStore::get_secrets(&*r.secrets, "98FA2E70C0E7").expect("keys stored");
    assert_eq!((k.regist_key.as_str(), k.rp_key.len()), ("3a3b3c3d3e3f4041", 32));
    assert!(
        !std::fs::read_to_string(r.dir.path().join("state.json")).unwrap().contains("3a3b3c3d"),
        "keys never reach the state file"
    );
    assert_eq!(r.core.consoles().len(), 1);
    assert!(r.events.events().iter().any(|e| matches!(e, AppEvent::Control { kind, detail } if kind == "remote_step" && detail.contains("PlayStation Network"))));
    assert!(r.events.events().iter().any(|e| matches!(e, AppEvent::ConsolesChanged)));
}

#[test]
fn a_failed_or_unusable_remote_registration_stores_nothing() {
    let r = rig();
    signed_in(&r);
    // the registrar fails
    r.core.set_remote_registrar(registrar(Err("could not reach your console".into())));
    let e = r.core.register_remote("00000007aabb").unwrap_err();
    assert!(e.to_string().contains("could not reach your console"), "{e}");
    assert!(r.core.consoles().is_empty());
    // it "succeeds" but the console reported no usable id
    let bad = Registered {
        device: DeviceType::Ps5,
        mac: "".into(),
        nickname: "x".into(),
        regist_key: "aa".into(),
        rp_key: "bb".into(),
        ap_name: "".into(),
    };
    r.core.set_remote_registrar(registrar(Ok(bad)));
    assert!(r.core.register_remote("00000007aabb").unwrap_err().to_string().contains("usable id"));
    assert!(r.core.consoles().is_empty());
    // and the next attempt is allowed (the running marker was cleared)
    r.core.set_remote_registrar(registrar(Err("again".into())));
    assert!(r.core.register_remote("00000007aabb").is_err());
}

#[test]
fn remote_registration_needs_a_sign_in_a_valid_id_and_a_registrar() {
    let r = rig();
    assert!(!r.core.remote_available());
    assert!(r.core.register_remote("abc").unwrap_err().to_string().contains("not available"));
    r.core.set_remote_registrar(registrar(Err("x".into())));
    assert!(matches!(r.core.register_remote("abc"), Err(CoreError::NeedsPsnLogin)), "not signed in to PSN");
    signed_in(&r);
    for bad in ["", &"a".repeat(200), "has space", "new\nline", "é"] {
        assert!(r.core.register_remote(bad).unwrap_err().to_string().contains("not a console id"), "{bad:.10?}");
    }
    r.psn.fail.store(true, Ordering::Relaxed); // the token cannot be refreshed
    assert!(r.core.register_remote("abc").unwrap_err().to_string().contains("sign in again"));
}

#[test]
fn only_one_remote_registration_runs_at_a_time_and_cancel_reaches_it() {
    let r = rig();
    signed_in(&r);
    let reg = registrar(Err("unused".into()));
    reg.wait_for_cancel.store(true, Ordering::Relaxed);
    r.core.set_remote_registrar(reg.clone());
    let core = r.core.clone();
    let t = std::thread::spawn(move || core.register_remote("00000007aabb"));
    let end = Instant::now() + Duration::from_secs(3);
    while !reg.started.load(Ordering::Relaxed) && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(r.core.register_remote("00000007aabb").unwrap_err().to_string().contains("already running"));
    r.core.cancel_remote();
    let e = t.join().unwrap().unwrap_err();
    assert!(e.to_string().contains("cancelled"), "{e}");
    r.core.cancel_remote(); // nothing running: harmless
}

// ---- first-picture watchdog ----------------------------------------------------------------------------------------------
/// Receives video but never shows a picture: a decoder that cannot cope with the stream.
struct Blind;
impl MediaSink for Blind {
    fn pictures_shown(&self) -> Option<u64> {
        Some(0)
    }
}

fn blind_core(fake: &Arc<Fake>, events: &Arc<VecSink>, dir: &tempfile::TempDir) -> (Core, ConsoleRecord) {
    let core = Core::new(CoreBuilder {
        store: ConsoleStore::open(dir.path().join("state.json")).unwrap(),
        secrets: Arc::new(MemoryStore::default()),
        backend: fake.clone(),
        psn: Some(Arc::new(FakePsn { fail: AtomicBool::new(false), signed_out: AtomicBool::new(false) })),
        events: events.clone(),
        media: Arc::new(Blind),
        settle_s: 0.0,
        senkusha: SenkushaConfig { io_timeout_s: 0.02, attempts: 2, retry_delay_s: 0.01, ..Default::default() },
    });
    core.set_auto_bitrate_probe(|_| None);
    core.set_first_picture_timeout(Duration::from_millis(400));
    core.psn_complete("https://x/redirect?code=1").unwrap();
    *fake.register.lock().unwrap() = Some(Ok(registered(DeviceType::Ps5)));
    let rec = core.register("10.0.0.9", Device::Ps5, "12345678", None).unwrap();
    fake.serve.store(true, Ordering::Relaxed);
    *fake.frames_to_send.lock().unwrap() = 3;
    (core, rec)
}

#[test]
fn a_session_that_never_shows_a_picture_fails_with_a_clear_reason_instead_of_staying_connected() {
    let (fake, events, dir) = (Arc::new(Fake::default()), Arc::new(VecSink::default()), tempfile::tempdir().unwrap());
    let (core, rec) = blind_core(&fake, &events, &dir);
    core.connect(&rec.id, StreamOptions { hevc: false, ..Default::default() }).unwrap();
    assert!(core.wait_idle(Duration::from_secs(15)), "never ended: {:?}", events.states());
    assert!(
        matches!(core.session_state(), SessionState::Failed { ref reason } if reason.contains("no picture could be decoded")),
        "{:?}",
        core.session_state()
    );
    assert_eq!(fake.control_calls.load(Ordering::Relaxed), 1, "H.264 was already in use: nothing else to try");
    fake.stop.store(true, Ordering::Relaxed);
}

#[test]
fn a_decoder_that_shows_nothing_for_hevc_is_retried_once_with_h264() {
    let (fake, events, dir) = (Arc::new(Fake::default()), Arc::new(VecSink::default()), tempfile::tempdir().unwrap());
    let (core, rec) = blind_core(&fake, &events, &dir);
    core.connect(&rec.id, StreamOptions { hevc: true, hdr: true, ..Default::default() }).unwrap();
    assert!(core.wait_idle(Duration::from_secs(20)), "never ended: {:?}", events.states());
    assert_eq!(
        *fake.streaming_calls.lock().unwrap(),
        vec![StreamingType::HevcHdr, StreamingType::Avc],
        "the second attempt gives up HEVC and HDR"
    );
    assert!(matches!(core.session_state(), SessionState::Failed { .. }), "the blind sink fails H.264 too");
    fake.stop.store(true, Ordering::Relaxed);
}
