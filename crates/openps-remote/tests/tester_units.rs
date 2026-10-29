//! Tester A2: parser fuzz and hostile-input tests for the pure layers of openps-remote (rudp, punch, keys, signaling) over
//! deterministic PRNG input and loopback UDP sockets. No network, no real tables needed (all-zero tables of the right size).
use openps_remote::keys::*;
use openps_remote::punch::{self, Ids, Packet};
use openps_remote::rudp::{self, UdpWire, Wire};
use openps_remote::signaling::*;
use openps_rp::tables::KeyTables;
use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

struct Prng(u64);
impl Prng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.next() as u8).collect()
    }
}

impl rand_core::RngCore for Prng {
    fn next_u32(&mut self) -> u32 {
        self.next() as u32
    }
    fn next_u64(&mut self) -> u64 {
        self.next()
    }
    fn fill_bytes(&mut self, d: &mut [u8]) {
        for x in d {
            *x = self.next() as u8;
        }
    }
    fn try_fill_bytes(&mut self, d: &mut [u8]) -> Result<(), rand_core::Error> {
        self.fill_bytes(d);
        Ok(())
    }
}

fn zero_tables() -> KeyTables {
    KeyTables {
        a: [0; 16],
        k: [0; 16],
        l: [0; 16],
        m: [0; 16],
        o: [0; 16],
        p: [0; 16],
        q: vec![0; 512],
        b: vec![0; 512],
        t: vec![0; 512],
        c: vec![0; 512],
        r: vec![0; 3584],
        s: vec![0; 3584],
        u: vec![0; 3584],
        v: vec![0; 3584],
    }
}

fn mutate(r: &mut Prng, mut b: Vec<u8>) -> Vec<u8> {
    for _ in 0..r.below(6) {
        if b.is_empty() {
            break;
        }
        let p = r.below(b.len());
        match r.below(3) {
            0 => b[p] = r.next() as u8,
            1 => b.truncate(p),
            _ => b.insert(p, r.next() as u8),
        }
    }
    b
}

// ---- rudp ----------------------------------------------------------------------------------------------------

/// A wire that records sends and answers from a script (`None` = silence).
struct Script {
    #[allow(clippy::type_complexity)]
    replies: Box<dyn FnMut(usize, &[u8]) -> Option<Vec<u8>>>,
    sends: usize,
    recvs: usize,
    pending: Vec<Vec<u8>>,
}
impl Wire for Script {
    fn send(&mut self, d: &[u8]) -> io::Result<()> {
        self.sends += 1;
        if let Some(r) = (self.replies)(self.sends, d) {
            self.pending.push(r);
        }
        Ok(())
    }
    fn recv(&mut self, _t: Duration) -> io::Result<Option<Vec<u8>>> {
        self.recvs += 1;
        Ok(if self.pending.is_empty() { None } else { Some(self.pending.remove(0)) })
    }
}

#[test]
fn a_silent_console_costs_exactly_ten_sends_and_fifty_reads_per_phase() {
    let mut w = Script { replies: Box::new(|_, _| None), sends: 0, recvs: 0, pending: vec![] };
    let e = rudp::register(&mut w, b"req", 600, 40000, Duration::from_millis(1)).unwrap_err();
    assert_eq!(e, rudp::RudpError::NoInitReply);
    assert_eq!((w.sends, w.recvs), (10, 50));
}

#[test]
fn rudp_register_survives_random_and_mutated_replies_in_every_phase() {
    let mut r = Prng(0xABCDEF);
    let good_init = rudp::chunk(0xD030, &[0u8; 24]);
    let good_cookie = rudp::chunk(0xA030, &[1, 0, 0, 0]);
    let good_data = rudp::chunk(0x0230, &[0, 5, b'H', b'T', b'T', b'P']);
    let seeds = [good_init, good_cookie, good_data, vec![], vec![0xD0; 8]];
    for i in 0..4000u64 {
        let mut rr = Prng(i + 1);
        let seeds = seeds.clone();
        let mut w = Script {
            replies: Box::new(move |_, _| {
                let s = seeds[rr.below(seeds.len())].clone();
                Some(if rr.below(3) == 0 {
                    let n = rr.below(64);
                    rr.bytes(n)
                } else {
                    mutate(&mut rr, s)
                })
            }),
            sends: 0,
            recvs: 0,
            pending: vec![],
        };
        let n = r.below(50);
        let req = r.bytes(n);
        let (seq, tag) = (r.next() as u16, r.next() as u32);
        let res = rudp::register(&mut w, &req, seq, tag, Duration::from_millis(1));
        assert!(w.sends <= 40, "retry storm: {} sends", w.sends);
        let _ = res;
    }
}

#[test]
fn parse_first_chunk_fuzz_and_boundaries_never_panic() {
    let mut r = Prng(77);
    for _ in 0..50_000 {
        let n = r.below(40);
        let b = r.bytes(n);
        if let Some(c) = rudp::parse_first_chunk(&b) {
            assert!(c.data.len() <= b.len());
        }
        let _ = (rudp::is_init_reply(&b), rudp::is_cookie_reply(&b), rudp::is_data_reply(&b), rudp::is_final_ack(&b));
    }
    // length field smaller than the header, larger than the datagram, and exactly 8
    for len in [0u16, 1, 7, 8, 9, 4095, 0x0FFF] {
        let mut dg = vec![0xC0 | (len >> 8) as u8, len as u8, 0x24, 0x4F, 0x24, 0x4F, 0x20, 0x30];
        dg.extend([1, 2, 3]);
        let c = rudp::parse_first_chunk(&dg).unwrap();
        assert!(c.data.len() <= 3);
    }
}

#[test]
fn an_init_reply_without_a_cookie_and_tiny_replies_are_malformed_errors() {
    let mut w = Script {
        replies: Box::new(|_, _| Some(rudp::chunk(0xD030, &[0u8; 8]))), // data.len() == 8: no cookie
        sends: 0,
        recvs: 0,
        pending: vec![],
    };
    let e = rudp::register(&mut w, b"r", 1, 1, Duration::from_millis(1)).unwrap_err();
    assert!(matches!(e, rudp::RudpError::Malformed(_)), "{e:?}");
}

#[test]
fn udp_wire_must_ignore_datagrams_from_other_senders() {
    let victim = UdpSocket::bind("127.0.0.1:0").unwrap();
    let console = UdpSocket::bind("127.0.0.1:0").unwrap();
    let attacker = UdpSocket::bind("127.0.0.1:0").unwrap();
    let mut wire = UdpWire { socket: victim.try_clone().unwrap(), peer: console.local_addr().unwrap() };
    attacker.send_to(&rudp::chunk(0xD030, &[0u8; 24]), victim.local_addr().unwrap()).unwrap();
    assert!(wire.recv(Duration::from_millis(300)).unwrap().is_none());
}

#[test]
fn rudp_chunk_length_field_documents_the_unchecked_truncation() {
    // Info T05-10: `(8 + len) as u16` and the 0x0FFF mask in parse_first_chunk mean chunks above 4087 data bytes do not round-trip.
    // The register request is about 700 bytes, so this is not reachable today.
    let c = rudp::chunk(0x0230, &vec![7u8; 5000]);
    let parsed = rudp::parse_first_chunk(&c).unwrap();
    assert_ne!(parsed.data.len(), 5000);
    let ok = rudp::chunk(0x0230, &vec![7u8; 600]);
    assert_eq!(rudp::parse_first_chunk(&ok).unwrap().data.len(), 600);
}

#[test]
fn udp_wire_with_a_zero_step_is_an_io_error_not_a_busy_loop() {
    let s = UdpSocket::bind("127.0.0.1:0").unwrap();
    let mut wire = UdpWire { socket: s.try_clone().unwrap(), peer: s.local_addr().unwrap() };
    let t = Instant::now();
    assert!(wire.recv(Duration::ZERO).is_err(), "set_read_timeout(0) is rejected by the OS layer");
    assert!(t.elapsed() < Duration::from_millis(100));
}

// ---- punch ---------------------------------------------------------------------------------------------------

#[test]
fn punch_packet_parse_fuzz_and_roundtrip() {
    let mut r = Prng(31337);
    for _ in 0..50_000 {
        let n = r.below(120);
        let b = r.bytes(n);
        if let Some(p) = Packet::parse(&b) {
            assert!(b.len() >= 88);
            let e = p.encode();
            assert_eq!(Packet::parse(&e), Some(p));
        }
    }
}

fn ids() -> Ids {
    Ids { own_id: [1; 20], peer_id: [2; 20], own_sid: 5123, peer_sid: 9001 }
}
fn cand(addr: &SocketAddr) -> Candidate {
    Candidate {
        kind: "LOCAL".into(),
        addr: addr.ip().to_string(),
        mapped_addr: "0.0.0.0".into(),
        port: i64::from(addr.port()),
        mapped_port: 0,
    }
}

/// An off-path "attacker": sends an ACK with a wrong nonce and wrong ids, from a socket that is not any candidate, to our socket.
fn spoof_ack(target: SocketAddr) -> (std::thread::JoinHandle<()>, std::sync::Arc<AtomicBool>, u16) {
    let s = UdpSocket::bind("127.0.0.1:0").unwrap();
    let port = s.local_addr().unwrap().port();
    let stop = std::sync::Arc::new(AtomicBool::new(false));
    let st = stop.clone();
    let ack = Packet {
        ty: punch::T_ACK,
        own_id: [0xEE; 20],
        peer_id: [0xDD; 20],
        own_sid: 1,
        peer_sid: 2,
        nonce: 0xDEAD_BEEF, // not ours
        mapped_ip: 0x0102_0304,
        mapped_port: 4242,
    }
    .encode();
    let h = std::thread::spawn(move || {
        for _ in 0..40 {
            if st.load(std::sync::atomic::Ordering::Relaxed) {
                break;
            }
            let _ = s.send_to(&ack, target);
            std::thread::sleep(Duration::from_millis(10));
        }
    });
    (h, stop, port)
}

#[test]
fn an_ack_that_does_not_echo_our_nonce_must_not_complete_the_punch() {
    let ours = UdpSocket::bind("127.0.0.1:0").unwrap();
    let silent = UdpSocket::bind("127.0.0.1:0").unwrap();
    let (h, stop, _) = spoof_ack(ours.local_addr().unwrap());
    let res = punch::hole_punch(
        &ours,
        &[cand(&silent.local_addr().unwrap())],
        &ids(),
        42,
        &AtomicBool::new(false),
        Duration::from_millis(300),
        true,
    );
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    h.join().unwrap();
    assert!(res.is_none());
}

#[test]
fn candidate_vetting_survives_hostile_values() {
    let mk = |addr: &str, port: i64, maddr: &str, mport: i64| Candidate {
        kind: String::new(),
        addr: addr.into(),
        mapped_addr: maddr.into(),
        port,
        mapped_port: mport,
    };
    let mut r = Prng(4);
    let weird = [
        "",
        " ",
        "1.2.3",
        "1.2.3.4.5",
        "256.1.1.1",
        "1.2.3.4 ",
        "０.0.0.0",
        "::1",
        "1.2.3.4\0",
        "01.02.03.04",
        "8.8.8.8",
    ];
    let mut v = vec![];
    for _ in 0..200 {
        v.push(mk(weird[r.below(weird.len())], r.next() as i64, weird[r.below(weird.len())], (r.next() as i64) >> 3));
    }
    let t = punch::probe_targets(&v, false);
    assert!(t.len() <= 16);
    assert!(t
        .iter()
        .all(|a| a.port() != 0 && !a.ip().is_loopback() && !a.ip().is_unspecified() && !a.ip().is_multicast()));
    let _ = punch::probe_targets(&[mk("8.8.8.8", i64::MAX, "8.8.4.4", i64::MIN)], false);
}

#[test]
fn cancelling_stops_a_running_punch_within_a_round_even_with_a_long_round_wait() {
    let ours = UdpSocket::bind("127.0.0.1:0").unwrap();
    let silent = UdpSocket::bind("127.0.0.1:0").unwrap();
    let cancel = std::sync::Arc::new(AtomicBool::new(false));
    let c2 = cancel.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        c2.store(true, std::sync::atomic::Ordering::Relaxed);
    });
    let t = Instant::now();
    let r = punch::hole_punch(
        &ours,
        &[cand(&silent.local_addr().unwrap())],
        &ids(),
        1,
        &cancel,
        Duration::from_secs(30),
        true,
    );
    assert!(r.is_none());
    assert!(t.elapsed() < Duration::from_secs(1), "{:?}", t.elapsed());
}

// ---- keys ----------------------------------------------------------------------------------------------------

#[test]
fn custom_data_fuzz_never_panics_and_errors_do_not_echo_the_input() {
    let t = zero_tables();
    let (i, j) = ([b'A'; 16], [b'B'; 16]);
    let mut r = Prng(2024);
    for _ in 0..20_000 {
        let n = r.below(80);
        let s: String = (0..n).map(|_| (b"ABCxyz019+/= \n\t$\xc3"[r.below(17)]) as char).collect();
        if let Err(e) = decode_custom_data(&t, &i, &j, &s) {
            let text = e.to_string();
            assert!(!s.trim().is_empty() || text.contains("0 characters") || text.contains("bytes"), "{text}");
            if s.trim().len() > 24 {
                assert!(!text.contains(s.trim()), "the error text repeats the key material: {text}");
            }
        }
    }
    // exact boundaries: 15 bytes fail, 16 and more pass (the original only reads the first 16)
    use base64::Engine as _;
    let b = base64::engine::general_purpose::STANDARD;
    for (len, ok) in [(0usize, false), (15, false), (16, true), (17, true), (4096, true)] {
        let inner = b.encode(vec![7u8; len]);
        assert_eq!(decode_custom_data(&t, &i, &j, &b.encode(inner)).is_ok(), ok, "{len}");
    }
}

#[test]
fn custom_data_base64_errors_do_not_name_the_offending_symbol() {
    // T05-08 (fixed): the error text no longer carries the base64 crate's message ("Invalid symbol 36, offset 3").
    let t = zero_tables();
    let e = decode_custom_data(&t, &[b'A'; 16], &[b'B'; 16], "AAA$AAAA").unwrap_err().to_string();
    assert!(!e.contains("symbol") && !e.contains("offset") && !e.contains("36"), "{e}");
}

#[test]
fn register_response_fuzz_never_panics() {
    let t = zero_tables();
    let cipher = openps_crypto::RpCipher::new([1; 16], [2; 16], t.k);
    let mut r = Prng(555);
    let seed = b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\nPS5-Mac: 1\r\n".to_vec();
    for i in 0..30_000 {
        let b = if i % 3 == 0 {
            let n = r.below(200);
            r.bytes(n)
        } else {
            mutate(&mut r, seed.clone())
        };
        let _ = parse_register_response(&cipher, &b);
    }
    // statuses
    assert!(parse_register_response(&cipher, b"HTTP/1.1 403 Forbidden\r\n\r\n").is_err());
    assert!(parse_register_response(&cipher, b"HTTP/1.1 2000\r\n\r\n").is_err());
    assert!(parse_register_response(&cipher, "HTTP/1.1 \u{e9}\u{e9}\u{e9}\r\n\r\n".as_bytes()).is_err());
}

#[test]
fn secrets_are_redacted_from_debug_and_the_attempt_secrets_are_well_formed() {
    let r = RemoteRegistered {
        mac: "m".into(),
        nickname: "n".into(),
        regist_key: "REGKEY123".into(),
        rp_key: "RPKEY456".into(),
    };
    let d = format!("{r:?} {r:#?}");
    assert!(!d.contains("REGKEY") && !d.contains("RPKEY"));
    let mut rng = Prng(9);
    let mut seen = std::collections::HashSet::new();
    for _ in 0..200 {
        let (i, j) = attempt_secrets(&mut rng);
        assert!(i.iter().chain(j.iter()).all(|c| b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567".contains(c)));
        assert!(seen.insert(i) && seen.insert(j), "repeated secret from a fresh PRNG state");
    }
}

// ---- signaling -----------------------------------------------------------------------------------------------

#[test]
fn websocket_message_and_payload_parsers_survive_fuzz_and_deep_nesting() {
    let mut r = Prng(8);
    let seeds = [
        r#"{"dataType":"x","body":{"data":{"sessionId":"s","customData1":"c","sessionMessage":{"payload":"p"}}},"contexts":[{"contextId":"a"}]}"#.to_string(),
        r#"ver=1.0, type=text, body={"action":"OFFER","reqId":1,"connRequest":{"sid":1,"candidate":[{"type":"LOCAL","addr":"1.2.3.4","port":5}]}}"#.to_string(),
    ];
    for i in 0..20_000 {
        let b = mutate(&mut r, seeds[i % 2].clone().into_bytes());
        let s = String::from_utf8_lossy(&b).to_string();
        if let Some(m) = WsMessage::parse(&s) {
            let _ = (m.is_for_us("", "a"), m.payload(), m.data_type(), m.custom_data1());
        }
        let _ = EndpointPayload::parse(&s);
        let _ = redact(&s);
    }
    for depth in [10, 127, 200, 5000, 100_000] {
        let deep = format!("{}{}", "[".repeat(depth), "]".repeat(depth));
        assert!(WsMessage::parse(&deep).is_none());
        let deep_obj = format!("{}1{}", "{\"a\":".repeat(depth.min(20_000)), "}".repeat(depth.min(20_000)));
        let _ = EndpointPayload::parse(&deep_obj); // must not overflow the stack
    }
}

#[test]
fn lenient_json_stays_linear_on_pathological_input() {
    let t = Instant::now();
    let nasty = format!("{{\"action\":{}", ":,".repeat(200_000));
    let _ = EndpointPayload::parse(&nasty);
    let nasty2 = format!("{{\"action\":\"{}", "\\".repeat(200_000));
    let _ = EndpointPayload::parse(&nasty2);
    assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
}

#[test]
fn out_of_range_numbers_in_a_payload_make_the_whole_message_unreadable() {
    // Info T05-11: one oversized/negative field (e.g. sid 1e30, port as a string) turns the message into `None`, which the session
    // reports as Unreadable and aborts: a console (or anyone who can send into the session) can end the registration with one message.
    assert!(EndpointPayload::parse(r#"{"action":"OFFER","connRequest":{"sid":1e30}}"#).is_none());
    assert!(EndpointPayload::parse(r#"{"action":"OFFER","connRequest":{"candidate":[{"port":"9304"}]}}"#).is_none());
    assert!(EndpointPayload::parse(r#"{"action":"OFFER","connRequest":{"sid":-5}}"#).is_some());
}

#[test]
fn redaction_hides_tokens_of_twelve_or_more_characters() {
    // T05-12 (fixed): the attempt secrets are exactly 16 characters and are now redacted.
    assert_eq!(redact("ABCDEFGHIJKLMNOP"), "<x>");
    assert_eq!(redact("ABCDEFGHIJK"), "ABCDEFGHIJK");
    assert_eq!(redact("ABCDEFGHIJKLMNOPQ"), "<x>");
    let long = redact(&"z".repeat(100_000));
    assert!(long.len() <= 800 * 3);
}

#[test]
fn websocket_url_must_only_accept_sony_hosts() {
    for host in ["evil.example.com", "localhost", "10.0.0.5", "127.0.0.1"] {
        assert!(websocket_url(host).is_none(), "{host} accepted");
    }
    assert!(websocket_url("mobile-pushcl.np.communication.playstation.net").is_some());
}

#[test]
fn accept_payload_must_escape_console_supplied_strings() {
    let res = PunchResult {
        kind: "LOCAL\",\"injected\":\"1".into(),
        console_ip: "1.2.3.4".into(),
        console_port: 1,
        mapped_ip: "5.6.7.8".into(),
        mapped_port: 2,
    };
    let acc = accept_payload(2, 1, 2, 0, &res, 1, AcceptStyle::Valid);
    let json: serde_json::Value = serde_json::from_str(acc.strip_prefix(PAYLOAD_PREFIX).unwrap()).unwrap();
    assert!(json["connRequest"]["candidate"][0].get("injected").is_none());
}

// ---- added with the fixes ----------------------------------------------------------------------------------

#[test]
fn hostile_values_are_escaped_in_every_json_template_and_url() {
    let t = join_command_body("d\"uid", 1, "s\"id", &[b'A'; 16], &[b'B'; 16]);
    let v: serde_json::Value = serde_json::from_str(t.trim_end()).expect("valid JSON");
    assert_eq!(v["commandDetail"]["duid"], "d\"uid");
    let inner: serde_json::Value =
        serde_json::from_str(v["commandDetail"]["parameters"]["initialParams"].as_str().unwrap()).unwrap();
    assert_eq!(inner["sessionId"], "s\"id");
    let u = session_message_url("a\"b/../c?d#e");
    assert!(!u.contains("..") && !u.contains('?') && !u.contains('#') && !u.contains('"'), "{u}");
    let c = local_candidate("1\",\"x\":\"", 1);
    assert!(serde_json::from_str::<serde_json::Value>(&c).is_ok());
    assert!(!valid_session_id("a/b") && !valid_session_id("") && !valid_session_id(&"a".repeat(129)));
    assert!(valid_session_id("1b2f-C_9"));
}

#[test]
fn a_udp_wire_drops_other_senders_but_still_hears_the_peer() {
    let victim = UdpSocket::bind("127.0.0.1:0").unwrap();
    let console = UdpSocket::bind("127.0.0.1:0").unwrap();
    let attacker = UdpSocket::bind("127.0.0.1:0").unwrap();
    let mut wire = UdpWire { socket: victim.try_clone().unwrap(), peer: console.local_addr().unwrap() };
    attacker.send_to(&rudp::chunk(0xD030, &[0u8; 24]), victim.local_addr().unwrap()).unwrap();
    console.send_to(&rudp::chunk(0xA030, &[1u8; 24]), victim.local_addr().unwrap()).unwrap();
    let got = wire.recv(Duration::from_millis(300)).unwrap().expect("the console's datagram");
    assert_eq!(got[6], 0xA0);
}

#[test]
fn an_oversized_chunk_is_cut_to_what_its_length_field_can_say() {
    let c = rudp::chunk(0x0230, &vec![7u8; 5000]);
    assert_eq!(rudp::parse_first_chunk(&c).unwrap().data.len(), rudp::MAX_CHUNK_DATA);
}

#[test]
fn rudp_register_stops_on_cancel_and_on_deadline() {
    struct Silent;
    impl rudp::Wire for Silent {
        fn send(&mut self, _: &[u8]) -> std::io::Result<()> {
            Ok(())
        }
        fn recv(&mut self, t: Duration) -> std::io::Result<Option<Vec<u8>>> {
            std::thread::sleep(t);
            Ok(None)
        }
    }
    let cancel = AtomicBool::new(true);
    let lim = rudp::Limits { cancel: Some(&cancel), deadline: None };
    let e = rudp::register_until(&mut Silent, b"r", 1, 1, Duration::from_millis(20), &lim).unwrap_err();
    assert_eq!(e, rudp::RudpError::Cancelled);
    let lim = rudp::Limits { cancel: None, deadline: Some(Instant::now() + Duration::from_millis(100)) };
    let t = Instant::now();
    let e = rudp::register_until(&mut Silent, b"r", 1, 1, Duration::from_millis(20), &lim).unwrap_err();
    assert_eq!(e, rudp::RudpError::TimedOut);
    assert!(t.elapsed() < Duration::from_secs(1));
}
