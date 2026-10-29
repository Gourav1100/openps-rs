//! The whole local pairing and session flow over real TCP against a fake console. The console recovers the
//! registration key by inverting the client's body patch, then answers like the original protocol. It is
//! written from the same spec as the client, so it proves the HTTP framing, retries and key handling fit
//! together; the key derivations themselves are validated separately against the original application.
use base64::Engine as _;
use openps_crypto::RpCipher;
use openps_discovery::DeviceType;
use openps_rp::client::{open_session, register, ClientError, OpenParams, Timeouts};
use openps_rp::ctrl::{ty, Inbound};
use openps_rp::kdf::{regist_cipher, session_cipher, Family};
use openps_rp::regist::{parse_header_block, RegisterParams, Registered};
use openps_rp::session::StreamingType;
use openps_rp::tables::{from_hex, KeyTables};
use rand_core::RngCore;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

fn tables() -> Option<KeyTables> {
    if let Some(r) = KeyTables::from_env() {
        return Some(r.expect("OPENPS_TABLES invalid"));
    }
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../analysis/tables/tables.json");
    match KeyTables::load(&p) {
        Ok(t) => Some(t),
        Err(_) => {
            eprintln!("SKIPPED: key tables not available");
            None
        }
    }
}

struct Seq(u8);
impl RngCore for Seq {
    fn next_u32(&mut self) -> u32 {
        let mut b = [0u8; 4];
        self.fill_bytes(&mut b);
        u32::from_le_bytes(b)
    }
    fn next_u64(&mut self) -> u64 {
        let mut b = [0u8; 8];
        self.fill_bytes(&mut b);
        u64::from_le_bytes(b)
    }
    fn fill_bytes(&mut self, d: &mut [u8]) {
        for x in d {
            self.0 = self.0.wrapping_mul(167).wrapping_add(13);
            *x = self.0;
        }
    }
    fn try_fill_bytes(&mut self, d: &mut [u8]) -> Result<(), rand_core::Error> {
        self.fill_bytes(d);
        Ok(())
    }
}

const PIN: u32 = 12_345_678;
const REGIST_KEY: &str = "3a3b3c3d3e3f4041";
const RP_KEY: &str = "00112233445566778899aabbccddeeff";
const NONCE: [u8; 16] = [0x5A; 16];

fn read_request(s: &mut TcpStream) -> (String, String, Vec<(String, String)>, Vec<u8>) {
    let mut buf = vec![];
    let mut chunk = [0u8; 2048];
    loop {
        let n = s.read(&mut chunk).unwrap();
        assert!(n > 0, "client closed early");
        buf.extend_from_slice(&chunk[..n]);
        if let Some(he) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..he]).to_string();
            let mut lines = head.split("\r\n");
            let mut first = lines.next().unwrap().split(' ');
            let (method, path) = (first.next().unwrap().to_string(), first.next().unwrap().to_string());
            let headers: Vec<(String, String)> = lines
                .filter_map(|l| l.split_once(':').map(|(k, v)| (k.trim().to_string(), v.trim().to_string())))
                .collect();
            let len = headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                .and_then(|(_, v)| v.parse::<usize>().ok())
                .unwrap_or(0);
            while buf.len() < he + 4 + len {
                let n = s.read(&mut chunk).unwrap();
                buf.extend_from_slice(&chunk[..n]);
            }
            return (method, path, headers, buf[he + 4..he + 4 + len].to_vec());
        }
    }
}

fn hdr<'a>(h: &'a [(String, String)], n: &str) -> Option<&'a str> {
    h.iter().find(|(k, _)| k.eq_ignore_ascii_case(n)).map(|(_, v)| v.as_str())
}

fn reply(s: &mut TcpStream, status: &str, headers: &[(&str, String)], body: &[u8]) {
    let mut out = format!("HTTP/1.1 {status}\r\n");
    for (k, v) in headers {
        out += &format!("{k}: {v}\r\n");
    }
    if !body.is_empty() {
        out += &format!("Content-Length: {}\r\n", body.len());
    }
    out += "Connection: close\r\n\r\n";
    s.write_all(out.as_bytes()).unwrap();
    s.write_all(body).unwrap();
}

/// Recover the PS5 registration cipher from a request body by inverting the nonce patch (as the console would).
fn console_regist_cipher(t: &KeyTables, body: &[u8]) -> RpCipher {
    let idx = (body[397] & 31) as usize;
    let idx_hi = (body[0] >> 3) as usize;
    let mut patch = [0u8; 16];
    patch[..8].copy_from_slice(&body[401..409]);
    patch[8..].copy_from_slice(&body[199..207]);
    let mut nonce = [0u8; 16];
    for i in 0..16 {
        nonce[i] = patch[i].wrapping_add(45).wrapping_sub(i as u8) ^ t.c[(i << 5) + idx_hi];
    }
    regist_cipher(t, Family::Ps5, &nonce, PIN, idx)
}

/// Serve `n` connections with `handler`; returns the port and a join handle.
fn serve<F: FnMut(TcpStream) + Send + 'static>(n: usize, mut handler: F) -> (u16, std::thread::JoinHandle<()>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    (
        port,
        std::thread::spawn(move || {
            for _ in 0..n {
                let (s, _) = l.accept().unwrap();
                s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                handler(s);
            }
        }),
    )
}

fn fast() -> Timeouts {
    Timeouts {
        connect: Duration::from_secs(2),
        io: Duration::from_secs(5),
        total: Duration::from_secs(10),
        retry_delay: Duration::from_millis(10),
    }
}

fn registered() -> Registered {
    Registered {
        device: DeviceType::Ps5,
        mac: "AABBCC".into(),
        nickname: "PS5".into(),
        regist_key: REGIST_KEY.into(),
        rp_key: RP_KEY.into(),
        ap_name: "x".into(),
    }
}

#[test]
fn registration_over_tcp_recovers_the_console_record() {
    let Some(t) = tables() else { return };
    let t2 = t.clone();
    let (port, srv) = serve(1, move |mut s| {
        let (m, path, headers, body) = read_request(&mut s);
        assert_eq!((m.as_str(), path.as_str()), ("POST", "/sie/ps5/rp/sess/rgst"));
        assert_eq!(hdr(&headers, "RP-Version"), Some("1.0"));
        assert_eq!(hdr(&headers, "User-Agent"), Some("remoteplay Windows"));
        let c = console_regist_cipher(&t2, &body);
        let mut enc = body[480..].to_vec();
        c.decrypt(&mut enc, 0);
        let h = parse_header_block(&enc);
        assert!(
            h.contains_key("Np-AccountId") && h.contains_key("Client-Type"),
            "console could read the client's headers: {h:?}"
        );
        let mut resp = format!("PS5-Mac: 001122334455\r\nPS5-Nickname: Den\r\nPS5-RegistKey: {REGIST_KEY}\r\nRP-Key: {RP_KEY}\r\nAP-Name: ps5-9\r\n").into_bytes();
        c.encrypt(&mut resp, 0);
        reply(&mut s, "200 OK", &[], &resp);
    });
    let p = RegisterParams { version: "1.0", device: DeviceType::Ps5, pin: PIN, account_id: 77, online_id: "" };
    let r = register(&t, "127.0.0.1", port, &p, &mut Seq(1), &fast()).unwrap();
    srv.join().unwrap();
    assert_eq!(
        (r.device, r.nickname.as_str(), r.regist_key.as_str(), r.rp_key.as_str()),
        (DeviceType::Ps5, "Den", REGIST_KEY, RP_KEY)
    );
}

#[test]
fn a_wrong_pin_is_reported_with_the_consoles_reason() {
    let Some(t) = tables() else { return };
    let (port, srv) = serve(1, |mut s| {
        read_request(&mut s);
        reply(&mut s, "403 Forbidden", &[("RP-Application-Reason", "80108bff".into())], b"");
    });
    let p = RegisterParams { version: "1.0", device: DeviceType::Ps5, pin: 1, account_id: 1, online_id: "" };
    let e = register(&t, "127.0.0.1", port, &p, &mut Seq(1), &fast()).unwrap_err();
    srv.join().unwrap();
    assert!(matches!(&e, ClientError::Register(_)), "{e}");
    assert!(e.to_string().contains("wrong credentials"), "{e}");
}

/// Console for the session flow. `init_failures` initial `80108b10` rejections exercise the retry rule.
fn session_console(
    t: KeyTables,
    init_failures: u32,
    frames: Vec<(u16, Vec<u8>)>,
) -> (u16, std::thread::JoinHandle<()>, Arc<AtomicU32>) {
    let inits = Arc::new(AtomicU32::new(0));
    let i2 = inits.clone();
    let (port, h) = serve(init_failures as usize + 2, move |mut s| {
        let (m, path, headers, _) = read_request(&mut s);
        assert_eq!(m, "GET");
        if path.ends_with("/init") {
            assert_eq!(hdr(&headers, "RP-Registkey"), Some(REGIST_KEY));
            assert_eq!(hdr(&headers, "Content-Length"), Some("0"));
            if i2.fetch_add(1, Ordering::SeqCst) < init_failures {
                reply(&mut s, "403 Forbidden", &[("RP-Application-Reason", "80108b10".into())], b"");
            } else {
                reply(&mut s, "200 OK", &[("RP-Nonce", b64().encode(NONCE))], b"");
            }
        } else {
            assert_eq!(path, "/sie/ps5/rp/sess/ctrl");
            let rp_key: [u8; 16] = from_hex(RP_KEY).unwrap().try_into().unwrap();
            let c = session_cipher(&t, "1.0", &rp_key, &NONCE);
            // the console can decrypt RP-Auth (counter 0) and sees the registration key
            let mut auth = b64().decode(hdr(&headers, "RP-Auth").unwrap()).unwrap();
            c.decrypt(&mut auth, 0);
            assert_eq!(&auth[..8], &from_hex(REGIST_KEY).unwrap()[..]);
            assert_eq!(hdr(&headers, "RP-ControllerType"), Some("0"));
            let mut st = vec![1u8, 0];
            c.encrypt(&mut st, 0);
            // response without Content-Length, then binary frames right behind the headers
            let mut out = format!("HTTP/1.1 200 OK\r\nRP-Server-Type: {}\r\nRP-Prohibit: 0\r\n\r\n", b64().encode(st))
                .into_bytes();
            let mut ctr = 1u32;
            for (ty, payload) in &frames {
                let mut p = payload.clone();
                if !p.is_empty() {
                    c.encrypt(&mut p, ctr);
                    ctr += 1;
                }
                out.extend((payload.len() as u32).to_be_bytes());
                out.extend(ty.to_be_bytes());
                out.extend([0, 0]);
                out.extend(p);
            }
            s.write_all(&out).unwrap();
            // read frames until the client disconnects: keepalive replies are raw type 0x01fe, pad joins are 0x0008
            let mut hdr8 = [0u8; 8];
            while s.read_exact(&mut hdr8).is_ok() {
                let len = u32::from_be_bytes([hdr8[0], hdr8[1], hdr8[2], hdr8[3]]) as usize;
                let mut payload = vec![0u8; len];
                s.read_exact(&mut payload).unwrap();
                match u16::from_be_bytes([hdr8[4], hdr8[5]]) {
                    0x0008 => {
                        c.decrypt(&mut payload, 5); // outbound counters continue after the five request headers
                        assert_eq!(payload, vec![1, 3]);
                    }
                    0x01FE => assert!(payload.is_empty()),
                    other => panic!("unexpected frame {other:#x}"),
                }
            }
        }
    });
    (port, h, inits)
}

#[test]
fn session_flow_gets_the_session_id_and_sends_frames_with_continued_counters() {
    let Some(t) = tables() else { return };
    let frames = vec![
        (ty::DISPLAY_STATUS, vec![0]),
        (ty::SESSION_ID, b"abc-session\0".to_vec()),
        (ty::PAD_JOIN_RESULT, vec![1, 0]),
    ];
    let (port, srv, _) = session_console(t.clone(), 0, frames);
    let reg = registered();
    let p = OpenParams {
        host: "127.0.0.1",
        port,
        device: DeviceType::Ps5,
        version: "1.0",
        registration: &reg,
        streaming: StreamingType::Avc,
    };
    let mut s = open_session(&t, &p, &mut Seq(3), &fast()).unwrap();
    assert_eq!(
        (s.version.as_str(), s.info.server_type, s.info.prohibited, s.info.full_hd),
        ("1.0", Some(1), false, true)
    );
    let id = s.wait_for_session_id(Duration::from_secs(2), || "fallback".into()).unwrap().to_string();
    assert_eq!(id, "abc-session");
    // frames that arrived alongside the session id are kept, in order
    assert_eq!(
        s.pending,
        vec![Inbound::DisplayStatus { can_display: true }, Inbound::PadJoinResult { pad: 1, accepted: true }]
    );
    s.send(|tx| tx.pad_join(1, 6)).unwrap(); // the console asserts payload [1, 3] at counter 5
    drop(s);
    srv.join().unwrap();
}

#[test]
fn another_user_connected_is_retried_then_succeeds() {
    let Some(t) = tables() else { return };
    let (port, srv, inits) = session_console(t.clone(), 2, vec![(ty::SESSION_ID, b"s\0".to_vec())]);
    let reg = registered();
    let p = OpenParams {
        host: "127.0.0.1",
        port,
        device: DeviceType::Ps5,
        version: "1.0",
        registration: &reg,
        streaming: StreamingType::Avc,
    };
    let mut s = open_session(&t, &p, &mut Seq(3), &fast()).unwrap();
    assert_eq!(inits.load(Ordering::SeqCst), 3); // two refusals, then the nonce
    assert_eq!(s.wait_for_session_id(Duration::from_secs(2), || "x".into()).unwrap(), "s");
    s.stream.shutdown(std::net::Shutdown::Both).ok();
    drop(s);
    srv.join().unwrap();
}

#[test]
fn retries_are_bounded() {
    let Some(t) = tables() else { return };
    // three refusals exceed the two retries the app allows; the third error is returned
    let (port, srv) = serve(3, |mut s| {
        read_request(&mut s);
        reply(&mut s, "403 Forbidden", &[("RP-Application-Reason", "80108b10".into())], b"");
    });
    let reg = registered();
    let p = OpenParams {
        host: "127.0.0.1",
        port,
        device: DeviceType::Ps5,
        version: "1.0",
        registration: &reg,
        streaming: StreamingType::Avc,
    };
    let e = open_session(&t, &p, &mut Seq(3), &fast()).err().expect("must fail");
    srv.join().unwrap();
    assert!(matches!(e, ClientError::Init(_)), "{e}");
}

#[test]
fn a_missing_session_id_falls_back_after_the_timeout() {
    let Some(t) = tables() else { return };
    let (port, srv, _) = session_console(t.clone(), 0, vec![(ty::KEEPALIVE, vec![])]);
    let reg = registered();
    let p = OpenParams {
        host: "127.0.0.1",
        port,
        device: DeviceType::Ps5,
        version: "1.0",
        registration: &reg,
        streaming: StreamingType::Avc,
    };
    let mut s = open_session(&t, &p, &mut Seq(3), &fast()).unwrap();
    let id = s.wait_for_session_id(Duration::from_millis(150), || "1700000000-random".into()).unwrap().to_string();
    assert_eq!(id, "1700000000-random");
    s.stream.shutdown(std::net::Shutdown::Both).ok();
    drop(s);
    srv.join().unwrap();
}

#[test]
fn unreachable_console_is_an_io_error_after_bounded_retries() {
    let Some(t) = tables() else { return };
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    drop(l); // nothing listens
    let reg = registered();
    let p = OpenParams {
        host: "127.0.0.1",
        port,
        device: DeviceType::Ps5,
        version: "1.0",
        registration: &reg,
        streaming: StreamingType::Avc,
    };
    let e = open_session(&t, &p, &mut Seq(3), &fast()).err().expect("must fail");
    assert!(matches!(e, ClientError::Io(_)), "{e}");
}
