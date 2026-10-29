//! Tester A2 (copy of the flow.rs harness with extra hostile behaviours). Original header: the whole remote registration against a FAKE PSN (HTTP + push WebSocket) and a FAKE console (UDP on loopback).
//!
//! Honest scope: the fakes are written from the same spec as the client (analysis/spec/09b), so this proves the pieces fit together,
//! that every step is taken in the right order with the right bodies, and that failures end cleanly. What was checked against the
//! ORIGINAL application's own code is in `oracle.rs` (keys, the register request, the UDP datagrams). Whether Sony's servers and a
//! real PS5 behave as the spec says can only be seen live.
use base64::Engine as _;
use openps_crypto::RpCipher;
use openps_psn::http::{HttpClient, HttpRequest, HttpResponse};
use openps_remote::keys::register_cipher;
use openps_remote::punch::{self, Packet};
use openps_remote::rudp;
use openps_remote::session::{register, NoProgress, Params, PushConnector, PushSocket, RemoteError};
use openps_remote::signaling::*;
use openps_rp::regist::{parse_header_block, CLIENT_TYPE};
use openps_rp::tables::KeyTables;
use rand_core::RngCore;
use std::collections::VecDeque;
use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const ACCOUNT: u64 = 4382906891241957369;
const DUID: &str = "0000000700410080a0112233445566778899aabbccddeeff";
const CONSOLE_L: [u8; 16] = [0x42; 16];
const EVIL_SID: &str = "a\"b/../c?d#e";
const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

fn tables() -> Option<KeyTables> {
    if let Some(r) = KeyTables::from_env() {
        return Some(r.expect("OPENPS_TABLES invalid"));
    }
    KeyTables::load(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../analysis/tables/tables.json")).ok()
}

#[derive(Clone, Copy, Default)]
struct Behaviour {
    /// POST .../commands answers 401.
    refuse_join: bool,
    /// The console never answers the join (no offer).
    silent_console: bool,
    /// The console answers our offer with TERMINATE.
    terminate: bool,
    /// The console's UDP side stays silent (hole punch fails).
    udp_silent: bool,
    /// customData1 is sent only after the console's RESULT for our ACCEPT.
    late_custom_data: bool,
    /// The console answers the register request with 403.
    refuse_register: bool,
    /// Garbage and foreign messages are mixed into the push stream.
    noise: bool,
    /// (tester) PSN never sends the "session created" notification, although POST remotePlaySessions succeeded.
    no_created: bool,
    /// (tester) the "created" notification carries a hostile session id.
    evil_sid: bool,
    /// (tester) the console answers the hole punch but never the reliable-UDP INIT.
    rudp_silent: bool,
}

struct World {
    tables: KeyTables,
    b: Behaviour,
    console_udp: Mutex<Option<SocketAddr>>,
    queue: Mutex<VecDeque<String>>,
    seen: Mutex<Vec<String>>, // "METHOD url" in order
    bodies: Mutex<Vec<(String, String)>>,
    context: Mutex<String>,
    secrets: Mutex<Option<([u8; 16], [u8; 16])>>,
    ws_closed: AtomicBool,
    udp_stop: AtomicBool,
    script: Mutex<Vec<(&'static str, String)>>,
}

impl World {
    fn push(&self, ty: &str, data: String) {
        let ctx = self.context.lock().unwrap().clone();
        let msg = format!(
            r#"{{"dataType":"{ty}","body":{{"data":{data}}},"contexts":[{{"contextId":"{ctx}","sequenceNumber":1}}]}}"#
        );
        self.queue.lock().unwrap().push_back(msg);
    }
    fn push_session_message(&self, payload: &str) {
        let p = serde_json::to_string(payload).unwrap();
        self.push(
            N_MESSAGE,
            format!(r#"{{"sessionId":"sess-1","sessionMessage":{{"channel":"remote_play:1","payload":{p}}}}}"#),
        );
    }
    fn push_custom_data(&self) {
        let (i, j) = self.secrets.lock().unwrap().expect("join command seen");
        // the console computes the same cipher we do and applies it to its key
        let mut raw = CONSOLE_L;
        RpCipher::new(i, j, self.tables.k).encrypt(&mut raw, 0);
        let cd = B64.encode(B64.encode(raw));
        self.push(N_CUSTOM_DATA, format!(r#"{{"sessionId":"sess-1","customData1":"{cd}"}}"#));
    }
    fn console_offer(&self) -> String {
        let a = self.console_udp.lock().unwrap().expect("console running");
        format!(
            "{PAYLOAD_PREFIX}{{\"action\":\"OFFER\",\"reqId\":12,\"error\":0,\"connRequest\":{{\"sid\":9001,\"peerSid\":0,\"skey\":\"AAAAAAAAAAAAAAAAAAAAAA==\",\"natType\":2,\"candidate\":[{{\"type\":\"LOCAL\",\"addr\":\"{}\",\"mappedAddr\":\"0.0.0.0\",\"port\":{},\"mappedPort\":0}}],\"localHashedId\":\"{}\"}}}}",
            a.ip(),
            a.port(),
            B64.encode([0x55u8; 20])
        )
    }
}

/// A console reads the action out of our message even when the rest is not valid JSON (the original's ACCEPT is not).
fn action_of(payload: &str) -> String {
    match EndpointPayload::parse(payload) {
        Some(p) => p.action().to_string(),
        None => ["OFFER", "ACCEPT", "RESULT", "TERMINATE"]
            .into_iter()
            .find(|a| payload.contains(&format!("\"action\":\"{a}\"")))
            .unwrap_or("")
            .to_string(),
    }
}

struct FakeHttp(Arc<World>);

fn resp(status: u16, body: &str) -> io::Result<HttpResponse> {
    Ok(HttpResponse { status, headers: vec![], body: body.as_bytes().to_vec() })
}

impl HttpClient for FakeHttp {
    fn execute(&self, req: &HttpRequest) -> io::Result<HttpResponse> {
        let w = &self.0;
        w.seen.lock().unwrap().push(format!("{} {}", req.method, req.url.split('?').next().unwrap()));
        let body = String::from_utf8_lossy(&req.body).to_string();
        w.bodies.lock().unwrap().push((req.url.clone(), body.clone()));
        assert_eq!(req.header("Authorization"), Some("Bearer TOKEN"), "every PSN call carries the bearer token");
        if req.url.starts_with("https://mobile-pushcl") {
            return resp(
                200,
                r#"{"fqdn":"push.example.playstation.net","keepAliveStatus":{},"retryIntervalMax":60,"retryIntervalMin":10}"#,
            );
        }
        if req.url == CREATE_SESSION_URL && req.method == "POST" {
            let v: serde_json::Value = serde_json::from_str(&body).unwrap();
            let ctx = v["remotePlaySessions"][0]["members"][0]["pushContexts"][0]["pushContextId"]
                .as_str()
                .unwrap()
                .to_string();
            *w.context.lock().unwrap() = ctx;
            if w.b.noise {
                for junk in [
                    "not json",
                    "{}",
                    r#"{"dataType":"x","body":{"data":{"sessionId":"other"}},"contexts":[{"contextId":"someone-else"}]}"#,
                ] {
                    w.queue.lock().unwrap().push_back(junk.into());
                }
            }
            if !w.b.no_created {
                let sid = if w.b.evil_sid { EVIL_SID } else { "sess-1" };
                w.push(N_CREATED, serde_json::json!({ "sessionId": sid }).to_string());
            }
            for (ty, data) in w.script.lock().unwrap().drain(..) {
                if ty == "MSG" {
                    w.push_session_message(&data);
                } else {
                    w.push(ty, data);
                }
            }
            return resp(200, r#"{"remotePlaySessions":[{"sessionId":"sess-1","members":[]}]}"#);
        }
        if req.url == COMMANDS_URL {
            if w.b.evil_sid {
                return resp(200, "");
            }
            if w.b.refuse_join {
                return resp(401, "");
            }
            let v: serde_json::Value = serde_json::from_str(body.trim_end()).unwrap();
            let params: serde_json::Value =
                serde_json::from_str(v["commandDetail"]["parameters"]["initialParams"].as_str().unwrap()).unwrap();
            assert_eq!(params["accountId"].as_u64(), Some(ACCOUNT));
            assert_eq!(v["commandDetail"]["duid"], DUID);
            let i: [u8; 16] = B64.decode(params["data1"].as_str().unwrap()).unwrap().try_into().unwrap();
            let j: [u8; 16] = B64.decode(params["data2"].as_str().unwrap()).unwrap().try_into().unwrap();
            *w.secrets.lock().unwrap() = Some((i, j));
            if !w.b.silent_console {
                if !w.b.late_custom_data {
                    w.push_custom_data();
                }
                w.push_session_message(&w.console_offer());
            }
            return resp(200, "");
        }
        if req.url.ends_with("/sessionMessage") {
            let v: serde_json::Value = serde_json::from_str(&body).unwrap();
            let payload = v["payload"].as_str().unwrap();
            let action = action_of(payload);
            match action.as_str() {
                "OFFER" => {
                    if w.b.terminate {
                        w.push_session_message(&format!(
                            "{PAYLOAD_PREFIX}{{\"action\":\"TERMINATE\",\"reqId\":13,\"error\":0}}"
                        ));
                    } else {
                        w.push_session_message(&result_payload(1));
                    }
                }
                "ACCEPT" => {
                    w.push_session_message(&result_payload(2));
                    if w.b.late_custom_data {
                        w.push_custom_data();
                    }
                }
                _ => {}
            }
            return resp(200, "");
        }
        if req.method == "DELETE" {
            return resp(200, "");
        }
        resp(404, "")
    }
}

struct FakePush(Arc<World>);
struct FakeSocket(Arc<World>);

impl PushConnector for FakePush {
    fn connect(&self, url: &str, headers: &[(String, String)]) -> io::Result<Box<dyn PushSocket>> {
        assert_eq!(url, "wss://push.example.playstation.net/np/pushNotification");
        let h = |k: &str| headers.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
        assert_eq!(h("Sec-WebSocket-Protocol"), Some("np-pushpacket"));
        assert_eq!(h("X-PSN-APP-TYPE"), Some("REMOTE_PLAY"));
        assert_eq!(h("Authorization"), Some("Bearer TOKEN"));
        self.0.seen.lock().unwrap().push("WS open".into());
        Ok(Box::new(FakeSocket(self.0.clone())))
    }
}
impl PushSocket for FakeSocket {
    fn recv(&mut self, timeout: Duration) -> io::Result<Option<String>> {
        let end = Instant::now() + timeout;
        while Instant::now() < end {
            if let Some(m) = self.0.queue.lock().unwrap().pop_front() {
                return Ok(Some(m));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        Ok(None)
    }
    fn close(&mut self) {
        self.0.ws_closed.store(true, Ordering::Relaxed);
    }
}

/// The fake console on loopback: answers hole-punch REQUESTs, speaks the reliable-UDP layer, decodes the register request like a console.
fn console(w: Arc<World>) -> std::thread::JoinHandle<()> {
    let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
    *w.console_udp.lock().unwrap() = Some(sock.local_addr().unwrap());
    sock.set_read_timeout(Some(Duration::from_millis(50))).unwrap();
    std::thread::spawn(move || {
        let mut buf = vec![0u8; 4096];
        while !w.udp_stop.load(Ordering::Relaxed) {
            let Ok((n, from)) = sock.recv_from(&mut buf) else { continue };
            let dg = &buf[..n];
            if w.b.udp_silent {
                continue;
            }
            if let Some(p) = Packet::parse(dg).filter(|p| p.ty == punch::T_REQUEST) {
                if let SocketAddr::V4(f) = from {
                    let ack = punch::ack_for(&p, [0x55; 20], 9001, p.own_sid, (*f.ip(), f.port()));
                    sock.send_to(&ack.encode(), from).unwrap();
                }
                continue;
            }
            let Some(c) = rudp::parse_first_chunk(dg) else { continue };
            match c.ty {
                rudp::T_INIT => {
                    if w.b.rudp_silent {
                        continue;
                    }
                    let mut d = vec![0u8; 8];
                    d.extend([0xC0, 0x0C, 0x1E, 0xFF, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]); // 8 unknown bytes, then the cookie
                    sock.send_to(&rudp::chunk(0xD030, &d), from).unwrap();
                }
                rudp::T_COOKIE => {
                    assert!(
                        c.data.ends_with(&[0xC0, 0x0C, 0x1E, 0xFF, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]),
                        "the cookie is echoed"
                    );
                    sock.send_to(&rudp::chunk(0xA030, &[0x01, 0x00, 0, 0]), from).unwrap();
                }
                rudp::T_HEAD => {
                    // second chunk starts after the first (8 + 4 bytes)
                    let second = rudp::parse_first_chunk(&dg[12..]).expect("second chunk");
                    assert_eq!(second.ty, rudp::T_DATA);
                    let http = &second.data[2..];
                    let split = http.windows(4).position(|x| x == b"\r\n\r\n").unwrap();
                    let body = &http[split + 4..];
                    // decode like a console
                    let nidx = (body[0] >> 3) as usize;
                    let mut obf = [0u8; 16];
                    obf[..8].copy_from_slice(&body[401..409]);
                    obf[8..].copy_from_slice(&body[199..207]);
                    let mut nonce = [0u8; 16];
                    for i in 0..16 {
                        nonce[i] = (obf[i] ^ w.tables.c[(i << 5) + nidx]).wrapping_add(i as u8).wrapping_sub(43);
                    }
                    let ck = register_cipher(&w.tables, &CONSOLE_L, (body[397] & 0x1F) as usize, nonce);
                    let mut hdr = body[480..].to_vec();
                    ck.decrypt(&mut hdr, 0);
                    let h = parse_header_block(&hdr);
                    assert_eq!(
                        h.get("Client-Type").map(String::as_str),
                        Some(CLIENT_TYPE),
                        "the console could read the request"
                    );
                    assert_eq!(h.get("Np-AccountId"), Some(&B64.encode(ACCOUNT.to_le_bytes())));
                    let reply = if w.b.refuse_register {
                        b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n".to_vec()
                    } else {
                        let mut plain = b"PS5-Mac: 98fa2e70c0e7\r\nPS5-Nickname: PS5-591\r\nPS5-RegistKey: 3a3b3c3d3e3f4041\r\nRP-Key: 00112233445566778899aabbccddeeff\r\n".to_vec();
                        ck.encrypt(&mut plain, 0);
                        let mut r = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", plain.len()).into_bytes();
                        r.extend(plain);
                        r
                    };
                    let mut d = vec![0x00, 0x05];
                    d.extend(reply);
                    sock.send_to(&rudp::chunk(0x0230, &d), from).unwrap();
                }
                rudp::T_FINAL => {
                    sock.send_to(&rudp::chunk(0xC000, &[0, 0, 0, 0]), from).unwrap();
                }
                _ => {}
            }
        }
    })
}

struct Fixed(u64);
impl RngCore for Fixed {
    fn next_u32(&mut self) -> u32 {
        self.next_u64() as u32
    }
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 11
    }
    fn fill_bytes(&mut self, d: &mut [u8]) {
        for x in d {
            *x = (self.next_u64() >> 3) as u8;
        }
    }
    fn try_fill_bytes(&mut self, d: &mut [u8]) -> Result<(), rand_core::Error> {
        self.fill_bytes(d);
        Ok(())
    }
}

struct Rig {
    world: Arc<World>,
    t: std::thread::JoinHandle<()>,
    cancel: Arc<AtomicBool>,
}

fn rig(b: Behaviour) -> Option<Rig> {
    let tables = tables().or_else(|| {
        eprintln!("SKIPPED: key tables not available");
        None
    })?;
    let world = Arc::new(World {
        tables,
        b,
        console_udp: Mutex::new(None),
        queue: Mutex::default(),
        seen: Mutex::default(),
        bodies: Mutex::default(),
        context: Mutex::default(),
        secrets: Mutex::default(),
        ws_closed: AtomicBool::new(false),
        udp_stop: AtomicBool::new(false),
        script: Mutex::default(),
    });
    let t = console(world.clone());
    Some(Rig { world, t, cancel: Arc::new(AtomicBool::new(false)) })
}

fn run(r: &Rig, timeout: Duration) -> Result<openps_remote::keys::RemoteRegistered, RemoteError> {
    run_p(r, timeout, &NoProgress)
}

fn run_p(
    r: &Rig,
    timeout: Duration,
    progress: &dyn openps_remote::session::Progress,
) -> Result<openps_remote::keys::RemoteRegistered, RemoteError> {
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    let p = Params {
        tables: &r.world.tables,
        access_token: "TOKEN".into(),
        account_id: ACCOUNT,
        duid: DUID.into(),
        local_ips: vec!["127.0.0.1".parse().unwrap()],
        public_ip: Some("203.0.113.5".into()),
        socket,
        accept_style: AcceptStyle::Original,
        allow_loopback: true,
        timeout,
        cancel: r.cancel.clone(),
        round_wait: Duration::from_millis(300),
        step_wait: Duration::from_millis(100),
    };
    register(&p, &FakeHttp(r.world.clone()), &FakePush(r.world.clone()), &mut Fixed(7), progress)
}

fn finish(r: Rig) -> Arc<World> {
    r.world.udp_stop.store(true, Ordering::Relaxed);
    r.t.join().unwrap();
    r.world
}

// =================================================================================================================
// tester A2 tests
// =================================================================================================================

fn posts(w: &World, suffix: &str) -> Vec<String> {
    w.bodies.lock().unwrap().iter().filter(|(u, _)| u.ends_with(suffix)).map(|(_, b)| b.clone()).collect()
}
fn count_action(w: &World, action: &str) -> usize {
    posts(w, "/sessionMessage")
        .iter()
        .filter(|b| {
            action_of(serde_json::from_str::<serde_json::Value>(b).unwrap()["payload"].as_str().unwrap()) == action
        })
        .count()
}

#[test]
fn tokens_never_appear_in_any_error_text() {
    for b in [
        Behaviour { refuse_join: true, ..Default::default() },
        Behaviour { terminate: true, ..Default::default() },
        Behaviour { udp_silent: true, ..Default::default() },
        Behaviour { refuse_register: true, ..Default::default() },
        Behaviour { silent_console: true, ..Default::default() },
    ] {
        let Some(r) = rig(b) else { return };
        let e = run(&r, Duration::from_millis(1200)).unwrap_err();
        for text in [e.to_string(), format!("{e:?}")] {
            assert!(!text.contains("TOKEN") && !text.contains("Bearer"), "{text}");
        }
        finish(r);
    }
}

#[test]
fn acceptance_messages_in_the_wrong_state_are_ignored() {
    let Some(r) = rig(Behaviour { silent_console: true, ..Default::default() }) else { return };
    // an ACCEPT before any OFFER, a RESULT for an unknown request, and a second "created" for another session
    let accept = format!("{PAYLOAD_PREFIX}{{\"action\":\"ACCEPT\",\"reqId\":2,\"error\":0,\"connRequest\":{{}}}}");
    r.world.script.lock().unwrap().extend([
        ("MSG", accept),
        ("MSG", result_payload(2)),
        (N_CREATED, r#"{"sessionId":"sess-2"}"#.to_string()),
    ]);
    assert_eq!(run(&r, Duration::from_millis(800)), Err(RemoteError::TimedOut));
    let w = finish(r);
    assert!(posts(&w, "/sessionMessage").is_empty(), "no reply to an ACCEPT that came before the offer");
    assert_eq!(posts(&w, "/commands").len(), 1, "the second created-notification must not trigger a second join");
}

#[test]
fn an_error_result_and_a_terminate_end_the_exchange_and_a_deleted_session_too() {
    let cases: [(&str, String, &str); 3] = [
        ("MSG", format!("{PAYLOAD_PREFIX}{{\"action\":\"RESULT\",\"reqId\":1,\"error\":5}}"), "refused"),
        ("MSG", format!("{PAYLOAD_PREFIX}{{\"action\":\"TERMINATE\"}}"), "ended"),
        (N_SESSION_DELETED, r#"{"sessionId":"sess-1"}"#.to_string(), "closed the session"),
    ];
    for (ty, data, want) in cases {
        let Some(r) = rig(Behaviour { silent_console: true, ..Default::default() }) else { return };
        r.world.script.lock().unwrap().push((ty, data));
        let e = run(&r, Duration::from_secs(5)).unwrap_err().to_string();
        assert!(e.contains(want), "{e}");
        let w = finish(r);
        assert!(w.seen.lock().unwrap().iter().any(|s| s.starts_with("DELETE ")), "the session is left");
    }
}

#[test]
fn unreadable_console_message_is_an_error_whose_display_hides_the_content() {
    let Some(r) = rig(Behaviour { silent_console: true, ..Default::default() }) else { return };
    r.world.script.lock().unwrap().push(("MSG", format!("{PAYLOAD_PREFIX}SECRET-CONTENT-%%%")));
    let e = run(&r, Duration::from_secs(5)).unwrap_err();
    assert!(matches!(e, RemoteError::Unreadable(_)), "{e:?}");
    assert!(!e.to_string().contains("SECRET"));
    finish(r);
}

#[test]
fn bad_custom_data_is_a_key_error_that_does_not_echo_it() {
    let Some(r) = rig(Behaviour { silent_console: true, ..Default::default() }) else { return };
    r.world
        .script
        .lock()
        .unwrap()
        .push((N_CUSTOM_DATA, r#"{"sessionId":"sess-1","customData1":"TOPSECRETTOPSECRET$$"}"#.to_string()));
    let e = run(&r, Duration::from_secs(5)).unwrap_err();
    assert!(matches!(e, RemoteError::Key(_)), "{e:?}");
    assert!(!e.to_string().contains("TOPSECRET") && !format!("{e:?}").contains("TOPSECRET"));
    finish(r);
}

#[test]
fn a_flood_of_offers_is_answered_one_post_each_but_only_one_offer_and_one_punch_happen() {
    // Info T05-17: every duplicate OFFER costs one HTTPS POST (RESULT); there is no rate limit. The single-offer logic holds.
    let Some(r) = rig(Behaviour { silent_console: true, ..Default::default() }) else { return };
    let offer = r.world.console_offer();
    r.world.script.lock().unwrap().extend((0..30).map(|_| ("MSG", offer.clone())));
    assert_eq!(run(&r, Duration::from_millis(1500)), Err(RemoteError::TimedOut));
    let w = finish(r);
    assert_eq!(count_action(&w, "RESULT"), 30);
    assert_eq!(count_action(&w, "OFFER"), 1);
    assert_eq!(count_action(&w, "ACCEPT"), 1);
}

#[test]
fn a_hostile_session_id_is_refused_and_never_reaches_a_url_or_json() {
    let Some(r) = rig(Behaviour { evil_sid: true, ..Default::default() }) else { return };
    let _ = run(&r, Duration::from_millis(900));
    let w = finish(r);
    for (u, b) in w.bodies.lock().unwrap().iter() {
        if u.contains("remotePlaySessions/") {
            assert!(!u.contains("..") && !u.contains('?') && !u.contains('#') && !u.contains('"'), "unsafe URL {u}");
        }
        if u == COMMANDS_URL {
            assert!(serde_json::from_str::<serde_json::Value>(b.trim_end()).is_ok(), "join body is not JSON: {b}");
        }
    }
}

#[test]
fn the_session_from_the_create_answer_must_be_left_on_timeout() {
    let Some(r) = rig(Behaviour { no_created: true, ..Default::default() }) else { return };
    let _ = run(&r, Duration::from_millis(700));
    let w = finish(r);
    assert!(w.seen.lock().unwrap().iter().any(|s| s.starts_with("DELETE ")), "session sess-1 leaked");
}

struct CancelAtRegistering {
    cancel: Arc<AtomicBool>,
    at: Mutex<Option<Instant>>,
}
impl openps_remote::session::Progress for CancelAtRegistering {
    fn step(&self, s: openps_remote::session::Step) {
        if s == openps_remote::session::Step::Registering {
            *self.at.lock().unwrap() = Some(Instant::now());
            let c = self.cancel.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(150));
                c.store(true, Ordering::Relaxed);
            });
        }
    }
}

#[test]
fn cancel_must_stop_the_register_exchange_promptly() {
    let Some(r) = rig(Behaviour { rudp_silent: true, ..Default::default() }) else { return };
    let prog = CancelAtRegistering { cancel: r.cancel.clone(), at: Mutex::new(None) };
    let res = run_p(&r, Duration::from_secs(60), &prog);
    let started = prog.at.lock().unwrap().expect("reached the register step");
    let took = started.elapsed();
    finish(r);
    assert_eq!(res, Err(RemoteError::Cancelled), "got {res:?}");
    assert!(took < Duration::from_secs(1), "cancel took {took:?} to take effect");
}

#[test]
fn a_silent_console_in_the_register_step_ends_with_a_plain_error_after_the_datagram_retries() {
    let Some(r) = rig(Behaviour { rudp_silent: true, ..Default::default() }) else { return };
    let t = Instant::now();
    let e = run(&r, Duration::from_secs(60)).unwrap_err();
    let took = t.elapsed();
    finish(r);
    assert!(matches!(&e, RemoteError::Console(m) if m.contains("did not answer the connection request")), "{e}");
    assert!(took > Duration::from_secs(4) && took < Duration::from_secs(9), "{took:?}");
    // 10 sends x 5 x 100 ms, ignoring cancel/timeout
}
