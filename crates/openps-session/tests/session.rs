//! End-to-end runtime tests: the real runner, handshake and stream engines against a mock console (testkit) on
//! the other end of an in-memory link or a loopback UDP socket. As elsewhere, the console is written from the
//! same spec, so this proves the pieces work together, not compatibility with a real PlayStation.
use openps_crypto::RpCipher;
use openps_input::state::{Button, ControllerState};
use openps_session::link::{mem_pair, udp_link, Link};
use openps_session::{connect, ConnectParams, Handle, NetworkInfo, NetworkTest, Notice};
use openps_stream::{Event, StreamConfig};
use openps_takion::chunk::Chunk;
use openps_takion::handshake::{HandshakeConfig, MAX_RESENDS, RESEND_INTERVAL_S};
use openps_takion::launch::LaunchParams;
use openps_takion::proto::{msg_type, DisconnectPayload, TakionMessage};
use openps_takion::senkusha::SenkushaConfig;
use openps_testkit::handshake::{HandshakeConsole, HS_KEY};
use openps_testkit::senkusha::SenkushaConsole;
use openps_testkit::stream::{decrypt_feedback, encode_frame, frame_data, StreamConsole};
use std::net::UdpSocket;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const VERSION: i32 = 14;

fn handshake_cfg(resend: f64, max_resends: u32) -> HandshakeConfig {
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
        max_resends,
    }
}

fn params() -> ConnectParams {
    ConnectParams::new(|_| handshake_cfg(RESEND_INTERVAL_S, MAX_RESENDS), StreamConfig::default())
}

/// Wait for a notice matching `pred`, discarding others. Panics after `limit`.
fn wait_for(h: &Handle, limit: Duration, mut pred: impl FnMut(&Notice) -> bool) -> Notice {
    let end = Instant::now() + limit;
    while Instant::now() < end {
        if let Some(n) = h.recv_timeout(Duration::from_millis(50)) {
            if pred(&n) {
                return n;
            }
        }
    }
    panic!("timed out waiting for a notice");
}

fn wait_terminal(h: &Handle, limit: Duration) -> Notice {
    wait_for(h, limit, |n| n.is_terminal())
}

/// Run the console side: answer the handshake, then hand the negotiated stream console to `script`.
fn console<T: Send + 'static>(
    mut link: Link,
    script: impl FnOnce(StreamConsole, &mut Link) -> T + Send + 'static,
) -> JoinHandle<T> {
    std::thread::spawn(move || {
        let mut con = HandshakeConsole::new(VERSION);
        let end = Instant::now() + Duration::from_secs(5);
        while !con.done() && Instant::now() < end {
            if let Ok(Some(d)) = link.source.recv(Duration::from_millis(50)) {
                for r in con.on_packet(&d) {
                    link.sink.send(&r).unwrap();
                }
            }
        }
        assert!(con.done(), "console never finished the handshake");
        script(con.into_stream_console().expect("keys negotiated"), &mut link)
    })
}

/// Collect packets from the client until `stop` says so or the time limit passes.
fn collect(link: &mut Link, limit: Duration, mut stop: impl FnMut(&[Vec<u8>]) -> bool) -> Vec<Vec<u8>> {
    let end = Instant::now() + limit;
    let mut got = vec![];
    while Instant::now() < end && !stop(&got) {
        if let Ok(Some(d)) = link.source.recv(Duration::from_millis(20)) {
            got.push(d);
        }
    }
    got
}

fn send_frame(con: &mut StreamConsole, link: &mut Link, id: u16, lose: &[usize]) -> Vec<u8> {
    let data = frame_data(900, id as u8);
    for (i, u) in encode_frame(&data, 4, 2).iter().enumerate() {
        if !lose.contains(&i) {
            // the client may already have hung up (test teardown): a closed peer is not a failure of the console under test
            let _ = link.sink.send(&con.video(VERSION, id, i as u16, 6, 2, u));
        }
    }
    data
}

// ---- tests --------------------------------------------------------------------------------------------------------
#[test]
fn full_session_video_input_and_clean_shutdown() {
    let (client, server) = mem_pair();
    let console = console(server, |mut con, link| {
        let sent = send_frame(&mut con, link, 0, &[1, 2]); // two source units lost: needs FEC
                                                           // wait for the app's feedback (state + history) and later its 3 disconnect messages
        let pkts = collect(link, Duration::from_secs(5), |p| {
            p.iter()
                .filter(|d| d[0] & 0x0F == 0 && msg_of_safe(d).is_some_and(|m| m.r#type == Some(msg_type::DISCONNECT)))
                .count()
                >= 3
        });
        let mut hist = None;
        for p in &pkts {
            if p[0] & 0x0F == 1 {
                hist = Some(decrypt_feedback(&mut con, p, false).2);
            }
        }
        let disconnects = pkts
            .iter()
            .filter(|d| d[0] & 0x0F == 0 && msg_of_safe(d).is_some_and(|m| m.r#type == Some(msg_type::DISCONNECT)))
            .count();
        (sent, hist, disconnects)
    });

    let h = connect(client, params());
    assert!(matches!(
        wait_for(&h, Duration::from_secs(5), |n| matches!(n, Notice::Connected { .. })),
        Notice::Connected { takion_version: VERSION, .. }
    ));
    let n = wait_for(&h, Duration::from_secs(5), |n| matches!(n, Notice::Event(Event::Video(_))));
    let Notice::Event(Event::Video(frame)) = n else { unreachable!() };
    let mut st = ControllerState::default();
    st.buttons.insert(Button::CROSS);
    h.set_pad(0, st);
    std::thread::sleep(Duration::from_millis(150));
    h.disconnect();
    assert!(matches!(wait_terminal(&h, Duration::from_secs(5)), Notice::Closed(None)));
    let stats = h.stats();
    h.join();

    let (sent, hist, disconnects) = console.join().unwrap();
    assert_eq!(frame.data, sent, "frame recovered through FEC");
    assert!(hist.expect("console received history").starts_with(&[0x80, 0x88, 0xFF]), "CROSS press arrived encrypted");
    assert_eq!(disconnects, 3);
    assert_eq!((stats.video_frames, stats.video_frames_dropped), (1, 0));
    assert!(stats.datagrams_in > 5 && stats.datagrams_out > 5);
}

fn msg_of_safe(d: &[u8]) -> Option<TakionMessage> {
    let c = Chunk::decode(d)?;
    (c.flags == 1).then(|| TakionMessage::parse(&c.payload)).flatten()
}

#[test]
fn handshake_failure_after_the_configured_attempts() {
    let (client, _silent_server) = mem_pair(); // alive but never answers
    let mut p = ConnectParams::new(|_| handshake_cfg(0.02, 2), StreamConfig::default());
    p.attempts = 2;
    p.attempt_delay_s = 0.01;
    let h = connect(client, p);
    let start = Instant::now();
    match wait_terminal(&h, Duration::from_secs(5)) {
        Notice::Failed(why) => assert!(why.contains("could not connect after 2 attempts"), "{why}"),
        n => panic!("unexpected {n:?}"),
    }
    assert!(start.elapsed() < Duration::from_secs(2));
    h.join();
}

#[test]
fn each_attempt_is_announced_and_uses_a_fresh_handshake_config() {
    let (client, _server) = mem_pair();
    let built = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let b = built.clone();
    let mut p = ConnectParams::new(
        move |_| {
            b.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            handshake_cfg(0.01, 1)
        },
        StreamConfig::default(),
    );
    p.attempts = 3;
    p.attempt_delay_s = 0.01;
    let h = connect(client, p);
    let mut attempts = vec![];
    loop {
        match h.recv_timeout(Duration::from_secs(5)) {
            Some(Notice::Handshaking { attempt }) => attempts.push(attempt),
            Some(n) if n.is_terminal() => break,
            Some(_) => {}
            None => panic!("timed out"),
        }
    }
    assert_eq!(attempts, vec![1, 2, 3]);
    assert_eq!(built.load(std::sync::atomic::Ordering::Relaxed), 3);
    h.join();
}

#[test]
fn a_closed_link_during_the_handshake_fails_fast() {
    let (client, server) = mem_pair();
    let h = connect(client, params());
    drop(server);
    match wait_terminal(&h, Duration::from_secs(5)) {
        Notice::Failed(why) => assert!(why.contains("link closed") || why.contains("send failed"), "{why}"),
        n => panic!("unexpected {n:?}"),
    }
    h.join();
}

#[test]
fn a_console_initiated_disconnect_closes_the_session_with_its_reason() {
    let (client, server) = mem_pair();
    let console = console(server, |mut con, link| {
        let mut m = TakionMessage::of_type(msg_type::DISCONNECT);
        m.disconnect = Some(DisconnectPayload { reason: Some("session ended".into()), extended_info: None });
        link.sink.send(&con.message(&m).1).unwrap();
        collect(link, Duration::from_millis(300), |_| false).len()
    });
    let h = connect(client, params());
    let n = wait_terminal(&h, Duration::from_secs(5));
    assert!(matches!(&n, Notice::Closed(Some(r)) if r == "session ended"), "{n:?}");
    h.join();
    console.join().unwrap();
}

#[test]
fn a_silent_console_is_declared_dead_after_the_idle_timeout() {
    let (client, server) = mem_pair();
    let console = console(server, |_con, link| collect(link, Duration::from_millis(900), |_| false).len());
    let mut p = params();
    p.idle_timeout_s = 0.3;
    let h = connect(client, p);
    match wait_terminal(&h, Duration::from_secs(5)) {
        Notice::Failed(why) => assert!(why.contains("stopped responding"), "{why}"),
        n => panic!("unexpected {n:?}"),
    }
    h.join();
    console.join().unwrap();
}

#[test]
fn a_slow_consumer_gets_bounded_media_and_the_console_is_asked_for_a_keyframe() {
    let (client, server) = mem_pair();
    let console = console(server, |mut con, link| {
        for id in 0..60u16 {
            send_frame(&mut con, link, id, &[]);
        }
        // stay connected until the client says goodbye (3 DISCONNECT messages), remembering whether an IDR request came
        let is_type = |d: &Vec<u8>, t: i32| d[0] & 0x0F == 0 && msg_of_safe(d).is_some_and(|m| m.r#type == Some(t));
        let pkts = collect(link, Duration::from_secs(5), |p| {
            p.iter().filter(|d| is_type(d, msg_type::DISCONNECT)).count() >= 3
        });
        pkts.iter().any(|d| is_type(d, msg_type::IDR_REQUEST))
    });
    let mut p = params();
    p.notice_capacity = 12; // tiny queue, and the app does not read while frames arrive
    let h = connect(client, p);
    std::thread::sleep(Duration::from_millis(700)); // let everything arrive unread
    let stats = h.stats();
    assert!(stats.video_frames_dropped > 0, "{stats:?}");
    assert!(stats.video_frames + stats.video_frames_dropped >= 50, "{stats:?}");
    h.disconnect();
    // lifecycle notices survive a full media queue: we still see Connected ... and the terminal Closed
    let mut saw_connected = false;
    let mut terminal = None;
    let end = Instant::now() + Duration::from_secs(5);
    while Instant::now() < end && terminal.is_none() {
        match h.recv_timeout(Duration::from_millis(50)) {
            Some(Notice::Connected { .. }) => saw_connected = true,
            Some(n) if n.is_terminal() => terminal = Some(n),
            _ => {}
        }
    }
    assert!(saw_connected && matches!(terminal, Some(Notice::Closed(None))), "{terminal:?}");
    h.join();
    assert!(console.join().unwrap(), "console received an IDR request after frames were dropped");
}

#[test]
fn works_over_a_real_udp_socket_on_loopback() {
    let server_sock = UdpSocket::bind("127.0.0.1:0").unwrap();
    let addr = server_sock.local_addr().unwrap();
    let console = std::thread::spawn(move || {
        server_sock.set_read_timeout(Some(Duration::from_millis(50))).unwrap();
        let mut con = HandshakeConsole::new(VERSION);
        let mut peer = None;
        let mut buf = vec![0u8; 65535];
        let end = Instant::now() + Duration::from_secs(5);
        while !con.done() && Instant::now() < end {
            if let Ok((n, from)) = server_sock.recv_from(&mut buf) {
                peer = Some(from);
                for r in con.on_packet(&buf[..n]) {
                    server_sock.send_to(&r, from).unwrap();
                }
            }
        }
        assert!(con.done());
        let mut sc = con.into_stream_console().unwrap();
        let peer = peer.unwrap();
        let data = frame_data(600, 7);
        for (i, u) in encode_frame(&data, 3, 1).iter().enumerate() {
            server_sock.send_to(&sc.video(VERSION, 0, i as u16, 4, 1, u), peer).unwrap();
        }
        // wait for the client's disconnect messages so we know it shut down properly
        let mut disconnects = 0;
        let end = Instant::now() + Duration::from_secs(5);
        while disconnects < 3 && Instant::now() < end {
            if let Ok((n, _)) = server_sock.recv_from(&mut buf) {
                if msg_of_safe(&buf[..n]).is_some_and(|m| m.r#type == Some(msg_type::DISCONNECT)) {
                    disconnects += 1;
                }
            }
        }
        (data, disconnects)
    });
    let h = connect(udp_link(addr).unwrap(), params());
    let n = wait_for(&h, Duration::from_secs(5), |n| matches!(n, Notice::Event(Event::Video(_))));
    let Notice::Event(Event::Video(f)) = n else { unreachable!() };
    h.disconnect();
    assert!(matches!(wait_terminal(&h, Duration::from_secs(5)), Notice::Closed(None)));
    h.join();
    let (data, disconnects) = console.join().unwrap();
    assert_eq!(f.data, data);
    assert_eq!(disconnects, 3);
}

#[test]
fn dropping_the_handle_ends_the_threads() {
    let (client, mut server) = mem_pair();
    let h = connect(client, params());
    drop(h);
    // the runner must stop on its own: the console end sees the link close
    let end = Instant::now() + Duration::from_secs(3);
    let mut closed = false;
    while Instant::now() < end {
        if server.source.recv(Duration::from_millis(50)).is_err() {
            closed = true;
            break;
        }
    }
    assert!(closed, "runner thread did not exit after the handle was dropped");
}

// ---- Senkusha network test before the handshake ------------------------------------------------------------------
fn fast_senkusha() -> SenkushaConfig {
    SenkushaConfig { ps5: true, io_timeout_s: 0.02, request_sends: 4, attempts: 2, retry_delay_s: 0.01 }
}

/// Answer the network test on `link` until the client's link closes or `stop` is set.
fn senkusha_console(
    mut link: Link,
    mut con: SenkushaConsole,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> JoinHandle<SenkushaConsole> {
    std::thread::spawn(move || {
        while !stop.load(std::sync::atomic::Ordering::Relaxed) {
            match link.source.recv(Duration::from_millis(10)) {
                Ok(Some(d)) => {
                    for r in con.on_packet(&d) {
                        if link.sink.send(&r).is_err() {
                            return con;
                        }
                    }
                }
                Ok(None) => {}
                Err(_) => break,
            }
        }
        con
    })
}

fn params_with_test(link: Link, seen: std::sync::Arc<std::sync::Mutex<Vec<NetworkInfo>>>) -> ConnectParams {
    let mut p = ConnectParams::new(
        move |info| {
            seen.lock().unwrap().push(*info);
            let mut cfg = handshake_cfg(RESEND_INTERVAL_S, MAX_RESENDS);
            // what the application does with the result: fragment to the uplink MTU, tell the console the downlink MTU and RTT
            cfg.mtu_out = info.mtu_out as usize;
            cfg.launch.mtu_in = info.mtu_in;
            cfg.launch.rtt_ms = info.rtt_ms;
            cfg
        },
        StreamConfig::default(),
    );
    p.network_test = Some(NetworkTest { link, config: fast_senkusha(), settle_s: 0.0 });
    p
}

#[test]
fn the_network_test_result_reaches_the_handshake_configuration() {
    let (client, server) = mem_pair();
    let (test_client, test_server) = mem_pair();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let tester = senkusha_console(test_server, SenkushaConsole::new(true, 1300, 1200), stop.clone());
    let console = console(server, |_con, link| {
        collect(link, Duration::from_secs(5), |p| {
            p.iter()
                .filter(|d| d[0] & 0x0F == 0 && msg_of_safe(d).is_some_and(|m| m.r#type == Some(msg_type::DISCONNECT)))
                .count()
                >= 3
        })
        .len()
    });
    let seen = std::sync::Arc::new(std::sync::Mutex::new(vec![]));
    let h = connect(client, params_with_test(test_client, seen.clone()));

    let mut order = vec![];
    let end = Instant::now() + Duration::from_secs(10);
    while Instant::now() < end {
        match h.recv_timeout(Duration::from_millis(50)) {
            Some(Notice::NetworkTested(Some(info))) => {
                assert!((1298..=1300).contains(&info.mtu_in) && (1198..=1200).contains(&info.mtu_out), "{info:?}");
                order.push("tested");
            }
            Some(Notice::Handshaking { .. }) => order.push("handshaking"),
            Some(Notice::Connected { .. }) => {
                order.push("connected");
                break;
            }
            Some(n) if n.is_terminal() => panic!("{n:?}"),
            _ => {}
        }
    }
    assert_eq!(order, vec!["tested", "handshaking", "connected"], "the test runs first");
    let infos = seen.lock().unwrap().clone();
    assert_eq!(infos.len(), 1);
    assert!((1198..=1200).contains(&infos[0].mtu_out));
    h.disconnect();
    wait_terminal(&h, Duration::from_secs(5));
    h.join();
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let con = tester.join().unwrap();
    assert!(con.disconnected, "the network test said goodbye to the console");
    console.join().unwrap();
}

#[test]
fn a_failed_network_test_falls_back_to_defaults_and_the_session_still_starts() {
    let (client, server) = mem_pair();
    let (test_client, _silent_test_server) = mem_pair(); // alive, never answers
    let console = console(server, |_con, link| {
        collect(link, Duration::from_secs(5), |p| {
            p.iter()
                .filter(|d| d[0] & 0x0F == 0 && msg_of_safe(d).is_some_and(|m| m.r#type == Some(msg_type::DISCONNECT)))
                .count()
                >= 3
        })
        .len()
    });
    let seen = std::sync::Arc::new(std::sync::Mutex::new(vec![]));
    let h = connect(client, params_with_test(test_client, seen.clone()));
    let first = wait_for(&h, Duration::from_secs(10), |n| matches!(n, Notice::NetworkTested(_)));
    assert!(matches!(first, Notice::NetworkTested(None)), "{first:?}");
    wait_for(&h, Duration::from_secs(10), |n| matches!(n, Notice::Connected { .. }));
    assert_eq!(seen.lock().unwrap().as_slice(), &[NetworkInfo::default()]);
    h.disconnect();
    wait_terminal(&h, Duration::from_secs(5));
    h.join();
    console.join().unwrap();
}

#[test]
fn disconnecting_during_the_network_test_ends_the_session() {
    let (client, _server) = mem_pair();
    let (test_client, _silent) = mem_pair();
    let seen = std::sync::Arc::new(std::sync::Mutex::new(vec![]));
    let mut p = params_with_test(test_client, seen.clone());
    p.network_test.as_mut().unwrap().config = SenkushaConfig { io_timeout_s: 0.5, attempts: 5, ..fast_senkusha() }; // would take seconds
    let h = connect(client, p);
    std::thread::sleep(Duration::from_millis(100));
    let start = Instant::now();
    h.disconnect();
    assert!(matches!(wait_terminal(&h, Duration::from_secs(3)), Notice::Closed(None)));
    assert!(start.elapsed() < Duration::from_secs(1), "stopping must not wait for the test to finish");
    assert!(seen.lock().unwrap().is_empty(), "no handshake was attempted");
    h.join();
}

// ---- a dead session thread must be detectable -------------------------------------------------------------------------
struct PanicSink;
impl openps_session::link::PacketSink for PanicSink {
    fn send(&mut self, _: &[u8]) -> std::io::Result<()> {
        panic!("simulated bug in the runtime thread");
    }
}

#[test]
fn a_session_thread_that_dies_is_detectable_instead_of_silent() {
    let (client, _server) = mem_pair();
    let link = openps_session::link::Link { sink: Box::new(PanicSink), source: client.source };
    let h = connect(link, params());
    let end = Instant::now() + Duration::from_secs(5);
    while !h.has_stopped() && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(h.has_stopped(), "the runtime thread panicked but the handle still claims to be running");
    // no terminal notice was ever produced: this is exactly the case the caller has to detect via `has_stopped`
    assert!(h.recv_timeout(Duration::from_millis(50)).is_none_or(|n| !n.is_terminal()));
    h.join(); // must not hang
}

// ---- input latency ---------------------------------------------------------------------------------------------------------
/// Time from `set_pad` to the datagram's arrival at the console, for stick changes sent 20 ms apart.
fn measure_input_latency(load_threads: usize, video_flood: bool) -> Vec<f64> {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    let (client, server) = mem_pair();
    let arrivals: Arc<Mutex<Vec<Instant>>> = Default::default();
    let stop = Arc::new(AtomicBool::new(false));
    let (a2, s2) = (arrivals.clone(), stop.clone());
    let console = console(server, move |mut con, link| {
        let mut id = 0u16;
        let mut next_frame = Instant::now();
        while !s2.load(Ordering::Relaxed) {
            if video_flood && Instant::now() >= next_frame {
                send_frame(&mut con, link, id, &[]);
                id = id.wrapping_add(1);
                next_frame += Duration::from_micros(6944); // 144 frames per second of 900-byte frames
            }
            if let Ok(Some(d)) = link.source.recv(Duration::from_micros(500)) {
                if d[0] & 0x0F == 6 {
                    a2.lock().unwrap().push(Instant::now());
                }
            }
        }
    });
    let h = connect(client, params());
    wait_for(&h, Duration::from_secs(5), |n| matches!(n, Notice::Connected { .. }));
    // CPU pressure: threads spinning flat out, like a busy renderer and decoder
    let spin = Arc::new(AtomicBool::new(true));
    let burners: Vec<_> = (0..load_threads)
        .map(|_| {
            let s = spin.clone();
            std::thread::spawn(move || {
                let mut x = 1u64;
                while s.load(Ordering::Relaxed) {
                    x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                    std::hint::black_box(x);
                }
            })
        })
        .collect();
    std::thread::sleep(Duration::from_millis(300));
    arrivals.lock().unwrap().clear();
    let mut sent = vec![];
    for i in 0..150 {
        let st = ControllerState { left_x: if i % 2 == 0 { 0.8 } else { -0.8 }, ..Default::default() };
        sent.push(Instant::now());
        h.set_pad(0, st);
        std::thread::sleep(Duration::from_millis(20));
    }
    std::thread::sleep(Duration::from_millis(100));
    spin.store(false, Ordering::Relaxed);
    for b in burners {
        b.join().unwrap();
    }
    stop.store(true, Ordering::Relaxed);
    h.disconnect();
    wait_terminal(&h, Duration::from_secs(5));
    h.join();
    console.join().unwrap();
    let arr = arrivals.lock().unwrap().clone();
    // first arrival after each send
    sent.iter()
        .filter_map(|t| arr.iter().find(|a| **a >= *t).map(|a| a.duration_since(*t).as_secs_f64() * 1e3))
        .collect()
}

fn pct(v: &mut [f64], p: f64) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[((v.len() - 1) as f64 * p) as usize]
}

#[test]
fn pad_input_reaches_the_wire_within_milliseconds_even_under_heavy_load() {
    let cores = std::thread::available_parallelism().map_or(4, |n| n.get());
    for (label, threads, flood, p50_max, p99_max) in [
        ("idle", 0, false, 2.0, 6.0),
        ("video flood at 144 fps", 0, true, 2.0, 8.0),
        // the tail with every core busy is the OS scheduler's, and depends on what else the machine runs (measured 5.9 ms on a
        // quiet machine, 38-52 ms next to a busy WindowServer); the median stays strict, the tail only has to stay sane
        ("every core saturated + video flood", cores, true, 3.0, 120.0),
    ] {
        let mut v = measure_input_latency(threads, flood);
        assert!(v.len() >= 120, "{label}: only {} of 150 changes reached the console", v.len());
        let (p50, p99, max) = (pct(&mut v, 0.5), pct(&mut v, 0.99), v.iter().cloned().fold(0.0, f64::max));
        eprintln!("input latency [{label}]: p50 {p50:.2} ms, p99 {p99:.2} ms, max {max:.2} ms ({} samples)", v.len());
        assert!(p50 < p50_max, "{label}: median {p50:.2} ms");
        assert!(p99 < p99_max, "{label}: p99 {p99:.2} ms");
    }
}
