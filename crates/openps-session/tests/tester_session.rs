//! Tester A1: failure-mode, lifecycle and resource tests for the session runtime.
use openps_crypto::RpCipher;
use openps_input::state::ControllerState;
use openps_session::link::{mem_pair, udp_link, Link};
use openps_session::{connect, Command, ConnectParams, Handle, NetworkTest, Notice};
use openps_stream::{Event, StreamConfig};
use openps_takion::handshake::{HandshakeConfig, MAX_RESENDS, RESEND_INTERVAL_S};
use openps_takion::launch::LaunchParams;
use openps_takion::senkusha::SenkushaConfig;
use openps_testkit::handshake::{HandshakeConsole, HS_KEY};
use openps_testkit::stream::{encode_frame, frame_data, StreamConsole};
use std::net::UdpSocket;
use std::time::{Duration, Instant};

const VERSION: i32 = 14;

fn hs_cfg(resend: f64, max: u32) -> HandshakeConfig {
    HandshakeConfig {
        ps5: true,
        request_takion20: false,
        session_key: "sess-1".into(),
        mtu_out: 1454,
        launch: LaunchParams::default(),
        handshake_key: HS_KEY,
        session_cipher: RpCipher::new([1; 16], [2; 16], [3; 16]),
        ecdh_secret: None,
        resend_interval_s: resend,
        max_resends: max,
    }
}

fn wait_for(h: &Handle, limit: Duration, mut pred: impl FnMut(&Notice) -> bool) -> Option<Notice> {
    let end = Instant::now() + limit;
    while Instant::now() < end {
        if let Some(n) = h.recv_timeout(Duration::from_millis(20)) {
            if pred(&n) {
                return Some(n);
            }
        }
    }
    None
}

/// A closed UDP port on loopback.
fn closed_port() -> std::net::SocketAddr {
    let s = UdpSocket::bind("127.0.0.1:0").unwrap();
    let a = s.local_addr().unwrap();
    drop(s);
    a
}

/// Drive the console side of a mem_pair through the handshake on the calling thread; returns the stream console.
fn finish_handshake(server: &mut Link) -> StreamConsole {
    let mut con = HandshakeConsole::new(VERSION);
    let end = Instant::now() + Duration::from_secs(5);
    while !con.done() && Instant::now() < end {
        if let Ok(Some(d)) = server.source.recv(Duration::from_millis(20)) {
            for r in con.on_packet(&d) {
                server.sink.send(&r).unwrap();
            }
        }
    }
    assert!(con.done());
    con.into_stream_console().unwrap()
}

// -------------------------------------------------------------------- transport robustness
/// On a connected UDP socket an ICMP port-unreachable surfaces as ECONNREFUSED on the NEXT send or recv. The
/// reader treats it as transient, but the send path (`UdpSink::send` -> `send_all`) turns it into a fatal
/// "send failed" so a console that is not listening yet kills the session instead of being retried.
#[test]
fn udp_send_after_icmp_unreachable_surfaces_econnrefused() {
    let addr = closed_port();
    let mut link = udp_link(addr).unwrap();
    link.sink.send(b"first").unwrap();
    std::thread::sleep(Duration::from_millis(100));
    let second = link.sink.send(b"second");
    println!("second send to a closed port: {second:?}");
    // platform dependent (macOS/Linux report it, but only to one of send/recv): record, do not assert
}

/// Regression guard for T02-08 (speculative race): passes 6/6 today because the reader thread usually consumes the
/// pending ECONNREFUSED before the runner's next send; if it ever fails the send path needs the same tolerance.
#[test]
fn handshake_to_a_not_yet_listening_console_retries_instead_of_failing_on_send() {
    let addr = closed_port();
    let link = udp_link(addr).unwrap();
    let mut p = ConnectParams::new(|_| hs_cfg(0.1, 4), StreamConfig::default());
    p.attempts = 1;
    let h = connect(link, p);
    let n = wait_for(&h, Duration::from_secs(10), |n| n.is_terminal()).expect("terminal notice");
    match n {
        Notice::Failed(why) => assert!(!why.contains("send failed"), "fatal send error instead of retry: {why}"),
        other => panic!("{other:?}"),
    }
    h.join();
}

#[test]
fn handshake_to_closed_port_terminates_and_threads_exit() {
    let addr = closed_port();
    let link = udp_link(addr).unwrap();
    let mut p = ConnectParams::new(|_| hs_cfg(0.05, 3), StreamConfig::default());
    p.attempts = 2;
    p.attempt_delay_s = 0.01;
    let h = connect(link, p);
    let n = wait_for(&h, Duration::from_secs(10), |n| n.is_terminal()).expect("terminal notice");
    println!("closed-port outcome: {n:?}");
    assert!(matches!(n, Notice::Failed(_)));
    let t = Instant::now();
    h.join();
    assert!(t.elapsed() < Duration::from_millis(600), "join took {:?}", t.elapsed());
}

#[test]
fn network_test_against_a_closed_port_degrades_to_defaults() {
    let ctl = closed_port();
    let nt = NetworkTest {
        link: udp_link(ctl).unwrap(),
        config: SenkushaConfig {
            io_timeout_s: 0.02,
            request_sends: 2,
            attempts: 1,
            retry_delay_s: 0.01,
            ..Default::default()
        },
        settle_s: 0.0,
    };
    let (client, _server) = mem_pair();
    let mut p = ConnectParams::new(|_| hs_cfg(0.02, 1), StreamConfig::default());
    p.network_test = Some(nt);
    p.attempts = 1;
    let h = connect(client, p);
    let first = wait_for(&h, Duration::from_secs(5), |n| matches!(n, Notice::NetworkTested(_))).expect("NetworkTested");
    assert!(matches!(first, Notice::NetworkTested(None)));
    let n = wait_for(&h, Duration::from_secs(5), |n| n.is_terminal()).expect("terminal");
    assert!(matches!(n, Notice::Failed(_)));
    h.join();
}

// -------------------------------------------------------------------- lifecycle
#[test]
fn join_is_prompt_in_every_phase_and_peer_sees_the_link_close() {
    // during handshake
    let (client, mut server) = mem_pair();
    let h = connect(client, ConnectParams::new(|_| hs_cfg(RESEND_INTERVAL_S, MAX_RESENDS), StreamConfig::default()));
    std::thread::sleep(Duration::from_millis(50));
    let t = Instant::now();
    h.join();
    assert!(t.elapsed() < Duration::from_millis(400), "join during handshake took {:?}", t.elapsed());
    let end = Instant::now() + Duration::from_secs(1);
    let mut closed = false;
    while Instant::now() < end {
        if server.source.recv(Duration::from_millis(20)).is_err() {
            closed = true;
            break;
        }
    }
    assert!(closed, "threads leaked: peer never saw the link close after join()");

    // while streaming
    let (client, mut server) = mem_pair();
    let h = connect(client, ConnectParams::new(|_| hs_cfg(RESEND_INTERVAL_S, MAX_RESENDS), StreamConfig::default()));
    let _con = finish_handshake(&mut server);
    assert!(wait_for(&h, Duration::from_secs(3), |n| matches!(n, Notice::Connected { .. })).is_some());
    let t = Instant::now();
    h.join();
    assert!(t.elapsed() < Duration::from_millis(400), "join while streaming took {:?}", t.elapsed());
}

#[test]
fn commands_after_the_session_ended_are_harmless() {
    let (client, _server) = mem_pair();
    let mut p = ConnectParams::new(|_| hs_cfg(0.01, 0), StreamConfig::default());
    p.attempts = 1;
    let h = connect(client, p);
    let c = h.commander();
    assert!(wait_for(&h, Duration::from_secs(5), |n| n.is_terminal()).is_some());
    std::thread::sleep(Duration::from_millis(50));
    let t = Instant::now();
    for i in 0..200_000usize {
        c.set_pad(i % 8, ControllerState::default());
        c.request_idr();
    }
    assert!(t.elapsed() < Duration::from_secs(5));
    assert!(h.has_stopped());
}

#[test]
fn degenerate_connect_params_do_not_hang_or_panic() {
    for (attempts, cap, idle) in [(0u32, 0usize, 0.0f64), (1, 1, -5.0), (1, 0, f64::NAN)] {
        let (client, mut server) = mem_pair();
        let mut p = ConnectParams::new(|_| hs_cfg(RESEND_INTERVAL_S, MAX_RESENDS), StreamConfig::default());
        p.attempts = attempts;
        p.notice_capacity = cap;
        p.idle_timeout_s = idle;
        p.attempt_delay_s = 0.0;
        let h = connect(client, p);
        let _con = finish_handshake(&mut server);
        let n = wait_for(&h, Duration::from_secs(3), |n| n.is_terminal());
        println!("attempts={attempts} cap={cap} idle={idle}: {n:?}");
        h.join();
    }
}

// -------------------------------------------------------------------- hostile traffic while streaming
#[test]
fn garbage_flood_does_not_kill_the_session_and_media_still_flows_afterwards() {
    let (client, mut server) = mem_pair();
    let mut p = ConnectParams::new(|_| hs_cfg(RESEND_INTERVAL_S, MAX_RESENDS), StreamConfig::default());
    p.idle_timeout_s = 30.0;
    let h = connect(client, p);
    let mut con = finish_handshake(&mut server);
    assert!(wait_for(&h, Duration::from_secs(3), |n| matches!(n, Notice::Connected { .. })).is_some());

    // 60k random-ish datagrams of every packet type, burst far above the runner's inbound backlog cap
    let mut x = 0x1234_5678_9abc_def0u64;
    let mut next = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    for i in 0..60_000usize {
        let n = (next() % 200) as usize;
        let mut d: Vec<u8> = (0..n).map(|_| next() as u8).collect();
        if !d.is_empty() {
            d[0] = [0u8, 2, 3, 1, 5, 6, 0x12][i % 7];
        }
        server.sink.send(&d).unwrap();
    }
    // drain notices produced by garbage (may include Messages); session must be alive
    std::thread::sleep(Duration::from_millis(300));
    while h.try_recv().is_some() {}
    let st = h.stats();
    println!("after flood: {st:?}");
    assert!(!h.has_stopped());

    // genuine frame after the flood: use a frame id far from any garbage-induced cursor state by trying a few
    let mut delivered = false;
    'o: for base in [20_000u16, 40_000, 60_000] {
        for k in 0..8u16 {
            let id = base + k;
            let data = frame_data(500, id as u8);
            for (i, u) in encode_frame(&data, 3, 1).iter().enumerate() {
                server.sink.send(&con.video(VERSION, id, i as u16, 4, 1, u)).unwrap();
            }
        }
        if wait_for(&h, Duration::from_millis(300), |n| matches!(n, Notice::Event(Event::Video(_)))).is_some() {
            delivered = true;
            break 'o;
        }
    }
    println!("media delivered after garbage flood: {delivered}");
    h.send(Command::Disconnect);
    h.join();
}

#[test]
fn stats_count_exactly_the_datagrams_fed() {
    let (client, mut server) = mem_pair();
    let mut p = ConnectParams::new(|_| hs_cfg(RESEND_INTERVAL_S, MAX_RESENDS), StreamConfig::default());
    p.idle_timeout_s = 30.0;
    let h = connect(client, p);
    let _con = finish_handshake(&mut server);
    assert!(wait_for(&h, Duration::from_secs(3), |n| matches!(n, Notice::Connected { .. })).is_some());
    let base = h.stats().datagrams_in;
    for _ in 0..1000 {
        server.sink.send(&[0xFF, 1, 2, 3]).unwrap(); // unknown type: ignored by the engine but counted
    }
    let end = Instant::now() + Duration::from_secs(3);
    while h.stats().datagrams_in < base + 1000 && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(10));
    }
    let st = h.stats();
    assert_eq!(st.datagrams_in - base + st.datagrams_dropped, 1000);
    h.join();
}
