//! Independent tester checks of the built-in pretend console. Fully offline.
use openps_core::backend::Backend;
use openps_core::{PsnService, RemoteRegistrar, RemoteRequest};
use openps_demo::{DemoBackend, DemoConfig, DemoPsn, DemoRegistrar, MAC};
use openps_discovery::DeviceType;
use openps_rp::regist::RegisterParams;
use openps_video::nal::{self, Codec};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// DemoPsn keeps its API sign-in state in a process-global static: tests that touch it must not interleave.
static API_STATE: Mutex<()> = Mutex::new(());

fn params(pin: u32) -> RegisterParams<'static> {
    RegisterParams { version: "1.0", device: DeviceType::Ps5, pin, account_id: 1, online_id: "x" }
}

#[test]
fn the_demo_console_scans_registers_and_rejects_pin_zero() {
    let b = DemoBackend::new(DemoConfig::default());
    let found = b.scan(&[]).unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].addr.to_string(), "127.0.0.1");
    assert!(b.wake(DeviceType::Ps5, "127.0.0.1".parse().unwrap(), "k").unwrap());
    assert!(b.register("h", &params(0)).is_err());
    let r = b.register("h", &params(1)).unwrap();
    assert_eq!(r.mac, MAC);
    // the demo registration id is what core.save_registration normalises: 12 hex digits
    assert_eq!(r.mac.replace(':', "").len(), 12);
}

#[test]
fn demo_config_env_parsing_rejects_nonsense_values() {
    // env vars are process-global: everything in ONE test so nothing races
    for k in ["OPENPS_DEMO", "OPENPS_DEMO_FPS", "OPENPS_DEMO_LOSS", "OPENPS_DEMO_AUDIO"] {
        std::env::remove_var(k);
    }
    assert!(DemoConfig::from_env().is_none());
    std::env::set_var("OPENPS_DEMO", "H264");
    for (v, want) in [
        ("0", 30),
        ("121", 30),
        ("-5", 30),
        ("abc", 30),
        ("", 30),
        ("1", 1),
        ("120", 120),
        ("99999999999999999999", 30),
    ] {
        std::env::set_var("OPENPS_DEMO_FPS", v);
        let c = DemoConfig::from_env().unwrap();
        assert_eq!((c.codec, c.fps), (Codec::H264, want), "{v:?}");
    }
    std::env::set_var("OPENPS_DEMO_LOSS", "yes");
    assert_eq!(DemoConfig::from_env().unwrap().recoverable_loss_every, 0, "only `1` turns loss on");
    std::env::set_var("OPENPS_DEMO_LOSS", "1");
    let c = DemoConfig::from_env().unwrap();
    assert_eq!((c.recoverable_loss_every, c.fatal_loss_every), (7, 97));
    std::env::set_var("OPENPS_DEMO_AUDIO", "0");
    assert!(!DemoConfig::from_env().unwrap().audio);
    for k in ["OPENPS_DEMO", "OPENPS_DEMO_FPS", "OPENPS_DEMO_LOSS", "OPENPS_DEMO_AUDIO"] {
        std::env::remove_var(k);
    }
}

#[test]
fn the_demo_psn_never_touches_a_network_and_its_art_is_generated() {
    let _g = API_STATE.lock().unwrap_or_else(|e| e.into_inner());
    let p = DemoPsn;
    assert!(p.api_available());
    assert!(p.api_sign_in("short").is_err());
    assert!(p.api_sign_in(&"n".repeat(64)).is_ok());
    assert!(p.api_signed_in());
    p.api_sign_out();
    assert!(!p.api_signed_in());
    let lib = p.api_library().unwrap();
    assert!(lib.games.len() >= 10 && lib.summary.is_some());
    for g in &lib.games {
        assert!(
            openps_psn::library::art_url_allowed(g.image_url.as_deref().unwrap()),
            "demo art URLs follow the real allow-list"
        );
        assert!(g.trophies.as_ref().is_none_or(|t| t.progress <= 100));
    }
    let (bytes, mime) = p.api_art("https://image.api.playstation.com/demo/3.png").unwrap();
    assert!(!bytes.is_empty() && mime == "image/svg+xml");
    // hostile url: no panic, no reflection of the text into the SVG
    let (bytes, _) = p.api_art("https://x/<script>alert(1)</script>/9999999999999999999999.png").unwrap();
    assert!(!String::from_utf8(bytes).unwrap().contains("<script>"));
    assert_eq!(p.api_launch("d", "").unwrap_err(), "no title");
    assert_eq!(p.api_consoles().unwrap().len(), 1);
}

#[test]
fn characterisation_demo_sign_in_accepts_any_url_containing_code_and_api_state_is_process_global() {
    let _g = API_STATE.lock().unwrap_or_else(|e| e.into_inner());
    let p = DemoPsn;
    assert!(p.complete("https://evil.test/?x=code=1").is_ok());
    assert!(p.complete("nothing").is_err());
    // DEMO_API_SIGNED_IN is a `static`: a second DemoPsn (another Core in the same process, e.g. parallel tests) shares it
    let a = DemoPsn;
    let b = DemoPsn;
    a.api_sign_in(&"n".repeat(64)).unwrap();
    assert!(b.api_signed_in());
    a.api_sign_out();
}

#[test]
fn the_demo_registrar_reports_steps_cancels_quickly_and_fails_on_request() {
    let req = |d: &str| RemoteRequest { access_token: "t".into(), account_id: 1, duid: d.into() };
    let steps = Mutex::new(vec![]);
    let r = DemoRegistrar
        .register(req("fail-please"), Arc::new(AtomicBool::new(false)), &|s| steps.lock().unwrap().push(s.to_string()));
    assert!(r.is_err());
    assert_eq!(steps.lock().unwrap().len(), 4);
    // cancelled before it starts: returns within one poll interval
    let cancel = Arc::new(AtomicBool::new(true));
    let t = Instant::now();
    assert_eq!(DemoRegistrar.register(req("d"), cancel, &|_| {}).unwrap_err(), "cancelled");
    assert!(t.elapsed() < Duration::from_millis(300));
    // cancelled mid-way
    let cancel = Arc::new(AtomicBool::new(false));
    let c2 = cancel.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        c2.store(true, Ordering::Relaxed);
    });
    let t = Instant::now();
    assert_eq!(DemoRegistrar.register(req("d"), cancel, &|_| {}).unwrap_err(), "cancelled");
    assert!(t.elapsed() < Duration::from_millis(600));
}

#[test]
fn dropping_the_backend_with_live_links_does_not_hang() {
    let t = Instant::now();
    let b = DemoBackend::new(DemoConfig::default());
    let mut links = vec![];
    for _ in 0..5 {
        links.push(b.open_links("127.0.0.1").unwrap()); // five "sessions" that never send a byte
    }
    drop(b); // stops and joins every serving thread while the client halves are still alive
    assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
    drop(links);
}

#[test]
fn precondition_of_the_empty_clip_hazard_a_garbage_clip_yields_no_access_units() {
    // serve_stream does `units[n % units.len()]` and `n.div_ceil(units.len())`: with OPENPS_DEMO_CLIP pointing at a
    // non-empty file that is not Annex-B video this is a division by zero on the server thread (a silent stall in the UI).
    for garbage in [&b"hello"[..], b"\x00\x00\x01", b"\xff\xff\xff\xff"] {
        let _ = nal::access_units(Codec::H264, garbage);
    }
    assert!(nal::access_units(Codec::H264, b"hello world, not a video").is_empty());
}
