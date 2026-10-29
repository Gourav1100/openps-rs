//! Tester A2: hostile / malformed / stalling console replies against the blocking client over localhost sockets,
//! plus deterministic fuzz loops for the HTTP, header-block and control-frame parsers. Uses all-zero key tables
//! (the derivation functions only need correctly sized tables), so it runs without the real protocol constants.

use openps_crypto::RpCipher;
use openps_discovery::DeviceType;
use openps_rp::client::{open_session, register, ClientError, OpenParams, Timeouts};
use openps_rp::ctrl::{CtrlRx, FrameError, Inbound};
use openps_rp::http::{exchange, parse_response, NoLength, Request};
use openps_rp::kdf::session_cipher;
use openps_rp::regist::{parse_header_block, RegisterParams, Registered};
use openps_rp::session::{ctrl_request, CtrlParams, StreamingType};
use openps_rp::tables::KeyTables;
use rand_core::RngCore;
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

// ---- helpers -------------------------------------------------------------------------------------------------

/// Deterministic xorshift PRNG (so failures reproduce).
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
impl RngCore for Prng {
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

fn fast() -> Timeouts {
    Timeouts {
        connect: Duration::from_millis(500),
        io: Duration::from_millis(300),
        total: Duration::from_millis(1500),
        retry_delay: Duration::from_millis(5),
    }
}

fn reg_params() -> RegisterParams<'static> {
    RegisterParams { version: "1.0", device: DeviceType::Ps5, pin: 12345678, account_id: 7, online_id: "x" }
}

fn registered() -> Registered {
    Registered {
        device: DeviceType::Ps5,
        mac: "AA".into(),
        nickname: "n".into(),
        regist_key: "3a3b3c3d".into(),
        rp_key: "00112233445566778899aabbccddeeff".into(),
        ap_name: "ap".into(),
    }
}

/// Accept connections forever on a loopback port, run `f(stream, connection_index)` on a thread for each.
fn serve<F>(f: F) -> (u16, Arc<AtomicUsize>, Arc<AtomicBool>)
where
    F: Fn(TcpStream, usize) + Send + Sync + 'static,
{
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    l.set_nonblocking(true).unwrap();
    let port = l.local_addr().unwrap().port();
    let count = Arc::new(AtomicUsize::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let (c2, s2) = (count.clone(), stop.clone());
    let f = Arc::new(f);
    std::thread::spawn(move || {
        while !s2.load(Ordering::Relaxed) {
            match l.accept() {
                Ok((s, _)) => {
                    s.set_nonblocking(false).unwrap();
                    let i = c2.fetch_add(1, Ordering::SeqCst);
                    let f = f.clone();
                    std::thread::spawn(move || f(s, i));
                }
                Err(_) => std::thread::sleep(Duration::from_millis(5)),
            }
        }
    });
    (port, count, stop)
}

/// Read until the blank line that ends an HTTP request head (and ignore the body).
fn read_request_head(s: &mut TcpStream) {
    s.set_read_timeout(Some(Duration::from_secs(2))).ok();
    let mut got = vec![];
    let mut b = [0u8; 1024];
    while !got.windows(4).any(|w| w == b"\r\n\r\n") {
        match s.read(&mut b) {
            Ok(0) | Err(_) => return,
            Ok(n) => got.extend_from_slice(&b[..n]),
        }
    }
}

fn do_register(port: u16, t: &Timeouts) -> Result<Registered, ClientError> {
    register(&zero_tables(), "127.0.0.1", port, &reg_params(), &mut Prng(1), t)
}

fn open(port: u16, version: &'static str, t: &Timeouts) -> Result<openps_rp::client::ControlSession, ClientError> {
    let reg = registered();
    let p = OpenParams {
        host: "127.0.0.1",
        port,
        device: DeviceType::Ps5,
        version,
        registration: &reg,
        streaming: StreamingType::Avc,
    };
    open_session(&zero_tables(), &p, &mut Prng(2), t)
}

fn with_deadline<T: Send + 'static>(secs: u64, f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(Duration::from_secs(secs)).ok()
}

// ---- parser fuzz ---------------------------------------------------------------------------------------------

#[test]
fn http_parse_response_never_panics_on_random_and_mutated_input() {
    let mut r = Prng(0xDEADBEEF);
    let seeds: [&[u8]; 4] = [
        b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nRP-Nonce: abc\r\n\r\nbody",
        b"HTTP/1.1 403 Forbidden\r\nRP-Application-Reason: 80108b02\r\n\r\n",
        b"HTTP/1.1 200 OK\r\n\r\n\x00\x00\x00\x00\x01\xfe\x00\x00",
        b"HTTP/1.1 200\r\nContent-Length: -1\r\n\r\n",
    ];
    for i in 0..20_000 {
        let mut buf = if i % 3 == 0 {
            let n = r.below(300);
            r.bytes(n)
        } else {
            seeds[r.below(seeds.len())].to_vec()
        };
        for _ in 0..r.below(6) {
            if buf.is_empty() {
                break;
            }
            let p = r.below(buf.len());
            match r.below(3) {
                0 => buf[p] = r.next() as u8,
                1 => buf.truncate(p),
                _ => buf.insert(p, r.next() as u8),
            }
        }
        for nl in [NoLength::Empty, NoLength::UntilClose] {
            for eof in [false, true] {
                if let Ok(Some((resp, used))) = parse_response(&buf, nl, eof) {
                    assert!(used <= buf.len(), "consumed more than was given");
                    assert!(resp.body.len() <= used);
                }
            }
        }
    }
}

#[test]
fn http_parse_is_consistent_for_every_prefix_split() {
    // property: feeding the response in two pieces yields the same parse as feeding it whole
    let full = b"HTTP/1.1 200 OK\r\nA: b\r\ncontent-length: 5\r\n\r\nhelloTRAILING";
    let whole = parse_response(full, NoLength::Empty, false).unwrap().unwrap();
    for cut in 0..full.len() {
        let first = parse_response(&full[..cut], NoLength::Empty, false).unwrap();
        if let Some(done) = first {
            assert_eq!(done, whole, "a prefix of {cut} bytes completed with a different result");
        }
    }
}

#[test]
fn exchange_survives_one_byte_reads_and_returns_the_trailing_bytes() {
    struct OneByte(Vec<u8>, usize);
    impl Read for OneByte {
        fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
            if self.1 >= self.0.len() {
                return Ok(0);
            }
            b[0] = self.0[self.1];
            self.1 += 1;
            Ok(1)
        }
    }
    impl Write for OneByte {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut s = OneByte(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nX: y\r\n\r\nabc\x01\x02".to_vec(), 0);
    let (r, rest) = exchange(&mut s, &Request::new("GET", "/"), NoLength::Empty).unwrap();
    // with 1-byte reads the response completes before the trailing bytes are read: they stay in the stream
    assert_eq!((r.body.as_slice(), rest), (&b"abc"[..], vec![]));
    assert_eq!(s.1, s.0.len() - 2);
}

#[test]
fn header_block_parser_never_panics_and_is_bounded_by_its_input() {
    let mut r = Prng(7);
    for _ in 0..20_000 {
        let n = r.below(400);
        let raw = r.bytes(n);
        let h = parse_header_block(&raw);
        let total: usize = h.iter().map(|(k, v)| k.len() + v.len()).sum();
        // lossy UTF-8 may expand invalid bytes to 3-byte replacement characters
        assert!(total <= raw.len() * 3 + 8);
    }
}

#[test]
fn control_frame_reassembly_fuzz_never_panics_and_never_buffers_more_than_a_frame_limit() {
    let cipher = RpCipher::new([1; 16], [2; 16], [3; 16]);
    let mut r = Prng(99);
    for _ in 0..300 {
        let mut rx = CtrlRx::new(cipher.clone(), r.below(5) as u32);
        let mut frames = 0;
        for _ in 0..40 {
            let n = r.below(120);
            rx.feed(&r.bytes(n));
            loop {
                match rx.next_message() {
                    Ok(Some(_)) => frames += 1,
                    Ok(None) => break,
                    Err(FrameError::TooLarge(n)) => {
                        assert!(n > 64 * 1024);
                        break;
                    }
                    Err(FrameError::CounterExhausted) => break,
                }
                assert!(frames < 100_000);
            }
        }
    }
}

#[test]
fn control_frames_split_into_single_bytes_decode_identically() {
    let c = RpCipher::new([9; 16], [8; 16], [7; 16]);
    let mut payload = b"sess-1\0".to_vec();
    c.encrypt(&mut payload, 0);
    let mut f = (7u32).to_be_bytes().to_vec();
    f.extend([0, 51, 0, 0]);
    f.extend(&payload);
    let mut rx = CtrlRx::new(c, 0);
    let mut got = vec![];
    for b in &f {
        rx.feed(&[*b]);
        while let Some(m) = rx.next_message().unwrap() {
            got.push(m);
        }
    }
    assert_eq!(got, vec![Inbound::SessionId("sess-1".into())]);
}

// ---- hostile servers -----------------------------------------------------------------------------------------

#[test]
fn a_console_that_accepts_and_never_answers_gets_an_io_timeout_not_a_hang() {
    let (port, _, stop) = serve(|mut s, _| {
        read_request_head(&mut s);
        std::thread::sleep(Duration::from_secs(3)); // never answers
    });
    let t0 = Instant::now();
    let e = do_register(port, &fast()).expect_err("must fail");
    assert!(matches!(e, ClientError::Io(_)), "{e}");
    assert!(t0.elapsed() < Duration::from_secs(2), "took {:?}", t0.elapsed());
    stop.store(true, Ordering::Relaxed);
}

#[test]
fn endless_headers_are_cut_off_after_the_header_limit() {
    let sent = Arc::new(AtomicUsize::new(0));
    let s2 = sent.clone();
    let (port, _, stop) = serve(move |mut s, _| {
        read_request_head(&mut s);
        let chunk = vec![b'a'; 4096];
        let _ = s.write_all(b"HTTP/1.1 200 OK\r\nX: ");
        while s.write_all(&chunk).is_ok() {
            if s2.fetch_add(4096, Ordering::Relaxed) > 64 * 1024 * 1024 {
                break;
            }
        }
    });
    let t0 = Instant::now();
    let e = do_register(port, &fast()).expect_err("must fail");
    assert!(matches!(e, ClientError::Http(ref m) if m.contains("HeadersTooLarge")), "{e}");
    assert!(t0.elapsed() < Duration::from_secs(2));
    stop.store(true, Ordering::Relaxed);
}

#[test]
fn a_declared_body_over_the_limit_is_refused_without_reading_it() {
    let (port, _, stop) = serve(|mut s, _| {
        read_request_head(&mut s);
        let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4294967296\r\n\r\n");
        std::thread::sleep(Duration::from_millis(500));
    });
    let e = do_register(port, &fast()).expect_err("must fail");
    assert!(matches!(e, ClientError::Http(ref m) if m.contains("BodyTooLarge")), "{e}");
    stop.store(true, Ordering::Relaxed);
}

#[test]
fn a_body_without_length_that_never_ends_is_cut_off_at_one_megabyte() {
    let (port, _, stop) = serve(|mut s, _| {
        read_request_head(&mut s);
        let _ = s.write_all(b"HTTP/1.1 200 OK\r\n\r\n");
        let chunk = vec![0u8; 64 * 1024];
        for _ in 0..200 {
            if s.write_all(&chunk).is_err() {
                return;
            }
        }
    });
    let e = do_register(port, &fast()).expect_err("must fail");
    assert!(matches!(e, ClientError::Http(ref m) if m.contains("BodyTooLarge")), "{e}");
    stop.store(true, Ordering::Relaxed);
}

#[test]
fn truncated_response_is_an_unexpected_eof() {
    let (port, _, stop) = serve(|mut s, _| {
        read_request_head(&mut s);
        let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\nshort");
    });
    let e = do_register(port, &fast()).expect_err("must fail");
    match e {
        ClientError::Io(ioe) => assert_eq!(ioe.kind(), io::ErrorKind::UnexpectedEof),
        other => panic!("{other}"),
    }
    stop.store(true, Ordering::Relaxed);
}

#[test]
fn a_registration_answer_of_pure_garbage_is_an_error_not_a_panic() {
    let (port, _, stop) = serve(|mut s, i| {
        read_request_head(&mut s);
        let mut r = Prng(100 + i as u64);
        let mut body = r.bytes(200);
        let mut resp = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len()).into_bytes();
        resp.append(&mut body);
        let _ = s.write_all(&resp);
    });
    for _ in 0..5 {
        let e = do_register(port, &fast()).expect_err("garbage must not register");
        assert!(matches!(e, ClientError::Register(_)), "{e}");
    }
    stop.store(true, Ordering::Relaxed);
}

#[test]
fn slow_loris_console_must_not_hold_registration_beyond_a_total_deadline() {
    let halt = Arc::new(AtomicBool::new(false));
    let h2 = halt.clone();
    let (port, _, stop) = serve(move |mut s, _| {
        read_request_head(&mut s);
        let _ = s.write_all(b"HTTP/1.1 200 OK\r\nX: ");
        while !h2.load(Ordering::Relaxed) {
            if s.write_all(b"a").is_err() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100)); // always inside the 300 ms per-read timeout
        }
    });
    let done = with_deadline(3, move || do_register(port, &fast()).is_err());
    halt.store(true, Ordering::Relaxed);
    stop.store(true, Ordering::Relaxed);
    assert_eq!(done, Some(true), "register() did not give up within 3 s of a 300 ms io timeout");
}

// ---- retry behaviour -----------------------------------------------------------------------------------------

#[test]
fn init_io_failures_are_retried_a_bounded_number_of_times() {
    let (port, count, stop) = serve(|s, _| drop(s)); // closes immediately every time
    let e = open(port, "1.0", &fast()).err().expect("must fail");
    assert!(matches!(e, ClientError::Io(_)), "{e}");
    assert_eq!(count.load(Ordering::SeqCst), 6, "1 attempt + 5 retries, then give up");
    stop.store(true, Ordering::Relaxed);
}

#[test]
fn console_busy_is_retried_exactly_twice_and_a_downgrade_chain_stops_at_8_0() {
    let (port, count, stop) = serve(|mut s, _| {
        read_request_head(&mut s);
        let _ = s.write_all(b"HTTP/1.1 403 Forbidden\r\nRP-Application-Reason: 80108b10\r\nContent-Length: 0\r\n\r\n");
    });
    let e = open(port, "1.0", &fast()).err().expect("must fail");
    assert!(matches!(e, ClientError::Init(_)), "{e}");
    assert_eq!(count.load(Ordering::SeqCst), 3);
    stop.store(true, Ordering::Relaxed);

    // a console that always says "update me, I speak 8.0" -> 10.0, 9.0, 8.0 and then it gives up (no endless loop)
    let (port, count, stop) = serve(|mut s, _| {
        read_request_head(&mut s);
        let _ = s.write_all(
            b"HTTP/1.1 403 Forbidden\r\nRP-Application-Reason: 80108b11\r\nRP-Version: 8.0\r\nContent-Length: 0\r\n\r\n",
        );
    });
    let e = open(port, "10.0", &fast()).err().expect("must fail");
    assert!(matches!(e, ClientError::Init(_)), "{e}");
    assert_eq!(count.load(Ordering::SeqCst), 3);
    stop.store(true, Ordering::Relaxed);
}

#[test]
fn nonce_of_wrong_length_or_encoding_is_rejected() {
    for nonce in ["", "!!!", "AAAA", "AAAAAAAAAAA=", "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"] {
        let (port, _, stop) = serve(move |mut s, _| {
            read_request_head(&mut s);
            let r = format!("HTTP/1.1 200 OK\r\nRP-Nonce: {nonce}\r\nContent-Length: 0\r\n\r\n");
            let _ = s.write_all(r.as_bytes());
        });
        let e = open(port, "1.0", &fast()).err().expect("must fail");
        assert!(matches!(e, ClientError::Init(_)), "{nonce:?}: {e}");
        stop.store(true, Ordering::Relaxed);
    }
}

// ---- control channel after a hostile ctrl answer -------------------------------------------------------------

/// A console whose init gives a valid nonce and whose ctrl answer then runs `after` on the same socket.
fn session_console(after: impl Fn(&mut TcpStream, &RpCipher) + Send + Sync + 'static) -> (u16, Arc<AtomicBool>) {
    let nonce = [5u8; 16];
    let rp_key: [u8; 16] =
        [0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff];
    let cipher = session_cipher(&zero_tables(), "1.0", &rp_key, &nonce);
    let (port, _, stop) = serve(move |mut s, _| {
        s.set_read_timeout(Some(Duration::from_secs(2))).ok();
        let mut got = vec![];
        let mut b = [0u8; 1024];
        while !got.windows(4).any(|w| w == b"\r\n\r\n") {
            match s.read(&mut b) {
                Ok(0) | Err(_) => return,
                Ok(n) => got.extend_from_slice(&b[..n]),
            }
        }
        let head = String::from_utf8_lossy(&got).to_string();
        if head.contains("/init") {
            use base64::Engine as _;
            let n = base64::engine::general_purpose::STANDARD.encode(nonce);
            let _ = s.write_all(format!("HTTP/1.1 200 OK\r\nRP-Nonce: {n}\r\nContent-Length: 0\r\n\r\n").as_bytes());
        } else {
            let _ = s.write_all(b"HTTP/1.1 200 OK\r\nRP-Prohibit: 0\r\n\r\n");
            after(&mut s, &cipher);
        }
    });
    (port, stop)
}

fn frame(c: &RpCipher, ty: u16, payload: &[u8], ctr: u32) -> Vec<u8> {
    let mut p = payload.to_vec();
    if !p.is_empty() {
        c.encrypt(&mut p, ctr);
    }
    let mut f = (payload.len() as u32).to_be_bytes().to_vec();
    f.extend(ty.to_be_bytes());
    f.extend([0, 0]);
    f.extend(p);
    f
}

#[test]
fn session_id_arrives_in_dribbles_and_keepalives_are_answered() {
    let (port, stop) = session_console(|s, c| {
        let mut bytes = frame(c, 254, &[], 0);
        bytes.extend(frame(c, 51, b"SESSION-XYZ\0", 0));
        for b in bytes {
            let _ = s.write_all(&[b]);
            std::thread::sleep(Duration::from_millis(5));
        }
        // read the keepalive reply
        let mut r = [0u8; 8];
        s.set_read_timeout(Some(Duration::from_secs(2))).ok();
        assert!(s.read_exact(&mut r).is_ok());
        assert_eq!(r, openps_rp::ctrl::KEEPALIVE_REPLY);
    });
    let mut cs = open(port, "1.0", &fast()).map_err(|e| e.to_string()).unwrap();
    let id = cs.wait_for_session_id(Duration::from_secs(3), || "FALLBACK".into()).map_err(|e| e.to_string()).unwrap();
    assert_eq!(id, "SESSION-XYZ");
    stop.store(true, Ordering::Relaxed);
}

#[test]
fn a_silent_control_channel_falls_back_at_the_deadline() {
    let (port, stop) = session_console(|_s, _c| std::thread::sleep(Duration::from_secs(2)));
    let mut cs = open(port, "1.0", &fast()).map_err(|e| e.to_string()).unwrap();
    let t0 = Instant::now();
    let id =
        cs.wait_for_session_id(Duration::from_millis(400), || "FALLBACK".into()).map_err(|e| e.to_string()).unwrap();
    assert_eq!(id, "FALLBACK");
    assert!(t0.elapsed() < Duration::from_millis(1500));
    stop.store(true, Ordering::Relaxed);
}

#[test]
fn a_hostile_frame_length_on_the_control_channel_is_an_error() {
    let (port, stop) = session_console(|s, _c| {
        let _ = s.write_all(&[0xFF, 0xFF, 0xFF, 0xFF, 0, 51, 0, 0]);
        std::thread::sleep(Duration::from_millis(500));
    });
    let mut cs = open(port, "1.0", &fast()).map_err(|e| e.to_string()).unwrap();
    let e = cs.wait_for_session_id(Duration::from_secs(2), || "F".into()).expect_err("must fail");
    assert!(matches!(e, ClientError::Frame(FrameError::TooLarge(_))), "{e}");
    stop.store(true, Ordering::Relaxed);
}

#[test]
fn the_console_closing_the_control_channel_is_reported() {
    let (port, stop) = session_console(|_s, _c| {});
    let mut cs = open(port, "1.0", &fast()).map_err(|e| e.to_string()).unwrap();
    let e = cs.wait_for_session_id(Duration::from_secs(2), || "F".into()).expect_err("must fail");
    assert!(matches!(e, ClientError::Io(ref i) if i.kind() == io::ErrorKind::UnexpectedEof), "{e}");
    stop.store(true, Ordering::Relaxed);
}

#[test]
fn a_flood_of_keepalives_cannot_extend_the_session_id_wait_past_its_deadline() {
    let (port, stop) = session_console(|s, _c| {
        let ka = [0u8, 0, 0, 0, 0, 254, 0, 0];
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_secs(3) {
            if s.write_all(&ka.repeat(64)).is_err() {
                return;
            }
        }
    });
    let mut cs = open(port, "1.0", &fast()).map_err(|e| e.to_string()).unwrap();
    let t0 = Instant::now();
    let r = cs.wait_for_session_id(Duration::from_millis(500), || "FALLBACK".into());
    assert!(t0.elapsed() < Duration::from_secs(2), "took {:?}", t0.elapsed());
    assert!(r.is_ok() || matches!(r, Err(ClientError::Io(_))));
    stop.store(true, Ordering::Relaxed);
}

// ---- information leaks / error text ---------------------------------------------------------------------------

#[test]
fn error_text_for_a_rejected_registration_never_contains_pin_or_keys() {
    let (port, _, stop) = serve(|mut s, _| {
        read_request_head(&mut s);
        let _ = s.write_all(b"HTTP/1.1 403 Forbidden\r\nRP-Application-Reason: 80108bff\r\nContent-Length: 0\r\n\r\n");
    });
    let e = do_register(port, &fast()).expect_err("403");
    for text in [e.to_string(), format!("{e:?}")] {
        assert!(!text.contains("12345678"), "{text}");
    }
    let e = open(port, "1.0", &fast()).err().expect("403");
    for text in [e.to_string(), format!("{e:?}")] {
        assert!(!text.contains("3a3b3c3d") && !text.contains("00112233445566778899aabbccddeeff"), "{text}");
    }
    stop.store(true, Ordering::Relaxed);
}

#[test]
fn error_text_must_not_echo_unbounded_console_controlled_strings() {
    let reason = format!("{}\x1b[2J", "A".repeat(10_000));
    let (port, _, stop) = serve(move |mut s, _| {
        read_request_head(&mut s);
        let _ = s.write_all(
            format!("HTTP/1.1 403 Forbidden\r\nRP-Application-Reason: {reason}\r\nContent-Length: 0\r\n\r\n")
                .as_bytes(),
        );
    });
    let e = do_register(port, &fast()).expect_err("403").to_string();
    assert!(e.len() < 1000 && !e.contains('\x1b'), "error text is {} bytes", e.len());
    stop.store(true, Ordering::Relaxed);
}

#[test]
fn crlf_in_the_host_must_not_inject_headers() {
    let req = openps_rp::session::init_request("1.2.3.4\r\nX-Injected: 1", "aa", "1.0");
    let text = String::from_utf8(req.to_bytes()).unwrap();
    assert!(!text.contains("\r\nX-Injected:"), "injected header present:\n{text}");
}

// ---- state machine / key handling -----------------------------------------------------------------------------

#[test]
fn ctrl_request_rejects_bad_key_material_but_accepts_an_empty_registration_key() {
    let t = zero_tables();
    let mk = |reg: &str, rp: &str| {
        let p = CtrlParams {
            host: "h",
            version: "1.0",
            device: DeviceType::Ps5,
            regist_key_hex: reg,
            rp_key_hex: rp,
            streaming: StreamingType::Avc,
        };
        ctrl_request(&t, &p, &[0; 16], &mut Prng(1)).is_ok()
    };
    let good = "00112233445566778899aabbccddeeff";
    assert!(mk("aa", good));
    assert!(!mk("a", good), "odd length");
    assert!(!mk("zz", good));
    assert!(!mk("aa", &good[..30]), "rp key too short");
    assert!(!mk("aa", &format!("{good}00")), "rp key too long");
    assert!(!mk("aé", good), "non-ascii must not panic");
    // documented laxness (Info T03-08): an empty registration key is accepted and produces a request
    assert!(mk("", good));
}

#[test]
fn control_counter_near_u32_max_never_wraps_or_panics() {
    // T03-07: counters use checked arithmetic. Inbound reports CounterExhausted; outbound refuses to reuse keystream.
    let c = RpCipher::new([1; 16], [2; 16], [3; 16]);
    let mut p = vec![9u8]; // PadDropped(9): implausible pad index -> forces the resync search
    c.encrypt(&mut p, 0);
    let mut f = 1u32.to_be_bytes().to_vec();
    f.extend([0, 10, 0, 0]);
    f.extend(p);
    let mut rx = CtrlRx::new(RpCipher::new([1; 16], [2; 16], [3; 16]), u32::MAX - 3);
    rx.feed(&f);
    assert!(rx.next_message().is_ok());
    let mut rx = CtrlRx::new(RpCipher::new([1; 16], [2; 16], [3; 16]), u32::MAX);
    rx.feed(&f);
    assert_eq!(rx.next_message(), Err(openps_rp::ctrl::FrameError::CounterExhausted));
    let mut tx = openps_rp::ctrl::CtrlTx::new(RpCipher::new([1; 16], [2; 16], [3; 16]), u32::MAX);
    assert!(tx.frame(10, &[1]).is_empty());
    assert_eq!(tx.counter(), u32::MAX);
}

// ---- table / index robustness and resync cost -------------------------------------------------------------------

#[test]
fn derivation_functions_panic_on_out_of_range_indices_or_short_tables_documenting_a_validation_gap() {
    // Info T03-03: `regist_cipher(idx)` / `regist_patch(idx_hi)` index the tables with unchecked caller values, and `KeyTables` has
    // public fields so `validate()` can be bypassed. All in-tree callers mask the index (`& 0x1F`, `>> 3`) and load via `from_json`,
    // so this is not reachable from network bytes today; it is a trap for future callers.
    use openps_rp::kdf::{regist_cipher, regist_patch, Family};
    let t = zero_tables();
    assert!(std::panic::catch_unwind(|| regist_cipher(&zero_tables(), Family::Ps5, &[0; 16], 1, 32)).is_err());
    assert!(std::panic::catch_unwind(|| regist_patch(&zero_tables(), Family::Ps5, true, &[0; 16], 32)).is_err());
    let _ = regist_cipher(&t, Family::Ps5, &[0; 16], 1, 31); // the largest valid index works
    let mut short = zero_tables();
    short.t.truncate(100);
    assert!(std::panic::catch_unwind(move || regist_cipher(&short, Family::Ps5, &[0; 16], 1, 0)).is_err());
    // from_json refuses such tables
    let mut bad = zero_tables();
    bad.t.truncate(100);
    assert!(KeyTables::from_json(&bad.to_json()).is_err());
}

#[test]
fn a_hostile_console_can_make_each_control_frame_cost_a_few_dozen_decryptions_documenting_the_resync_amplification() {
    // Low T03-04: a frame whose decode looks implausible (pad index >= 4) triggers up to 64 extra full-payload decryptions.
    // Frames are capped at 64 KiB, so the cost is bounded but 65x the normal one; measured here, not asserted tightly.
    let c = RpCipher::new([1; 16], [2; 16], [3; 16]);
    let mut rx = CtrlRx::new(c.clone(), 1000);
    let mut payload = vec![0x55u8; 64 * 1024];
    c.encrypt(&mut payload, 0); // decrypts to a first byte of 0x55 (>= 4) at counter 0, i.e. never plausible at 1000 +- 32
    let mut f = (payload.len() as u32).to_be_bytes().to_vec();
    f.extend([0, 10, 0, 0]);
    f.extend(&payload);
    let t0 = Instant::now();
    for _ in 0..20 {
        rx.feed(&f);
        assert!(rx.next_message().unwrap().is_some());
    }
    let per_frame = t0.elapsed() / 20;
    eprintln!("resync search: {per_frame:?} per 64 KiB frame (debug build)");
    assert!(per_frame < Duration::from_secs(2));
}

#[test]
fn drip_feeding_console_hits_the_total_deadline_on_open_session_too() {
    let halt = Arc::new(AtomicBool::new(false));
    let h2 = halt.clone();
    let (port, _, stop) = serve(move |mut s, _| {
        read_request_head(&mut s);
        let _ = s.write_all(b"HTTP/1.1 200 OK\r\nX: ");
        while !h2.load(Ordering::Relaxed) {
            if s.write_all(b"a").is_err() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    });
    let done = with_deadline(8, move || open(port, "1.0", &fast()).is_err());
    halt.store(true, Ordering::Relaxed);
    stop.store(true, Ordering::Relaxed);
    assert_eq!(done, Some(true));
}
