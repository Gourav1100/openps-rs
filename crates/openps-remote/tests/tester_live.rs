//! Tester A2 (feature `live`): the TLS certificate verifier with its AIA fetcher, and the push WebSocket transport, against hostile
//! LOCALHOST peers only. Nothing here opens a connection to a non-loopback host: `wss://` is never used, `http_fetch` is never called
//! (it would resolve a Sectigo host name), the verifier gets in-process fetchers.
#![cfg(feature = "live")]

use openps_remote::session::{PushConnector, PushSocket};
use openps_remote::tls::*;
use openps_remote::ws::LiveConnector;
use rcgen::{BasicConstraints, CertificateParams, CustomExtension, DnType, IsCa, KeyPair, KeyUsagePurpose};
use rustls::client::danger::ServerCertVerifier;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{CertificateError, Error, RootCertStore};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
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

// ---- PKI helpers (same construction as the crate's own unit tests) -------------------------------------------------------------

const CA_ISSUERS: [u8; 8] = [0x2B, 0x06, 0x01, 0x05, 0x05, 0x07, 0x30, 0x02];

fn aia_ext(url: &str) -> CustomExtension {
    let u = url.as_bytes();
    let mut desc = vec![0x06, 0x08];
    desc.extend(CA_ISSUERS);
    desc.push(0x86);
    desc.push(u.len() as u8);
    desc.extend(u);
    let mut seq = vec![0x30, desc.len() as u8];
    seq.extend(desc);
    let mut outer = vec![0x30, seq.len() as u8];
    outer.extend(seq);
    CustomExtension::from_oid_content(&[1, 3, 6, 1, 5, 5, 7, 1, 1], outer)
}

struct Pki {
    root: rcgen::Certificate,
    intermediate_der: Vec<u8>,
    leaf_der: Vec<u8>,
}

#[derive(Clone, Copy, Default)]
struct Opt {
    /// the intermediate is NOT a CA
    non_ca_intermediate: bool,
    /// the leaf is long expired
    expired_leaf: bool,
    /// the intermediate itself carries an AIA pointing at an allowed URL (to make the verifier walk up)
    intermediate_aia: bool,
}

fn pki(aia: &str, o: Opt) -> Option<Pki> {
    let root_key = KeyPair::generate().ok()?;
    let mut rp = CertificateParams::new(vec![]).ok()?;
    rp.distinguished_name.push(DnType::CommonName, "Test Root");
    rp.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    rp.key_usages = vec![KeyUsagePurpose::KeyCertSign];
    let root = rp.self_signed(&root_key).ok()?;
    let root_issuer = rcgen::Issuer::new(rp, root_key);

    let int_key = KeyPair::generate().ok()?;
    let mut ip = CertificateParams::new(vec![]).ok()?;
    ip.distinguished_name.push(DnType::CommonName, "Test Intermediate");
    ip.is_ca = if o.non_ca_intermediate { IsCa::ExplicitNoCa } else { IsCa::Ca(BasicConstraints::Unconstrained) };
    if !o.non_ca_intermediate {
        ip.key_usages = vec![KeyUsagePurpose::KeyCertSign];
    }
    if o.intermediate_aia {
        ip.custom_extensions.push(aia_ext("http://crt.sectigo.com/Self.crt"));
    }
    let inter = ip.signed_by(&int_key, &root_issuer).ok()?;
    let int_issuer = rcgen::Issuer::new(ip, int_key);

    let leaf_key = KeyPair::generate().ok()?;
    let mut lp = CertificateParams::new(vec!["push.example.net".to_string()]).ok()?;
    lp.custom_extensions.push(aia_ext(aia));
    if o.expired_leaf {
        lp.not_before = rcgen::date_time_ymd(2000, 1, 1);
        lp.not_after = rcgen::date_time_ymd(2001, 1, 1);
    }
    let leaf = lp.signed_by(&leaf_key, &int_issuer).ok()?;
    Some(Pki { root, intermediate_der: inter.der().to_vec(), leaf_der: leaf.der().to_vec() })
}

fn verifier(p: &Pki, fetch: Fetcher) -> AiaVerifier {
    let mut roots = RootCertStore::empty();
    roots.add(p.root.der().clone()).unwrap();
    AiaVerifier::new(Arc::new(roots), Arc::new(rustls::crypto::ring::default_provider()), fetch).unwrap()
}

fn check(v: &AiaVerifier, leaf: &[u8], sent: &[Vec<u8>], name: &str, now: UnixTime) -> Result<(), Error> {
    let inter: Vec<CertificateDer<'static>> = sent.iter().map(|d| CertificateDer::from(d.clone())).collect();
    v.verify_server_cert(
        &CertificateDer::from(leaf.to_vec()),
        &inter,
        &ServerName::try_from(name.to_string()).unwrap(),
        &[],
        now,
    )
    .map(|_| ())
}

// ---- AIA verifier --------------------------------------------------------------------------------------------------------------

#[test]
fn allowlist_fuzz_only_ever_yields_the_three_hosts_and_safe_paths() {
    let mut r = Prng(0xA1A);
    let frags = [
        "http://",
        "https://",
        "crt.sectigo.com",
        "crt.comodoca.com",
        "crt.usertrust.com",
        "evil.com",
        "/",
        "@",
        ":80",
        ":",
        ".",
        "..",
        "%2e",
        "%00",
        "a",
        "b.crt",
        "?",
        "#",
        "\\",
        " ",
        "\t",
        "\r\n",
        "\0",
        "CRT.SECTIGO.COM",
        "crt.sectigo.com.",
        "[::1]",
        "x".repeat(300).leak(),
    ];
    for _ in 0..200_000 {
        let n = 1 + r.below(7);
        let url: String = (0..n).map(|_| frags[r.below(frags.len())]).collect();
        if let Some((host, path)) = aia_url_allowed(&url) {
            assert!(ALLOWED_AIA_HOSTS.contains(&host), "{url:?}");
            assert!(url == format!("http://{host}/{path}"), "{url:?}");
            assert!(
                path.len() <= 256 && path.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-/~".contains(&b)),
                "{url:?}"
            );
        }
    }
    // T05-13 (fixed): no dot segments, percent escapes or empty segments
    assert!(aia_url_allowed("http://crt.sectigo.com/../x").is_none());
    assert!(aia_url_allowed("http://crt.sectigo.com/%2e%2e/x").is_none());
    assert!(aia_url_allowed("http://crt.sectigo.com/a//x").is_none());
    assert!(aia_url_allowed("http://crt.sectigo.com/Test_CA.crt").is_some());
}

#[test]
fn ca_issuers_url_fuzz_over_random_and_mutated_certificates_never_panics() {
    let Some(p) = pki("http://crt.sectigo.com/Test_CA.crt", Opt::default()) else { return };
    let mut r = Prng(11);
    for i in 0..20_000 {
        let mut b = if i % 4 == 0 {
            let n = r.below(300);
            r.bytes(n)
        } else {
            p.leaf_der.clone()
        };
        for _ in 0..r.below(8) {
            if b.is_empty() {
                break;
            }
            let at = r.below(b.len());
            match r.below(3) {
                0 => b[at] = r.next() as u8,
                1 => b.truncate(at),
                _ => b.insert(at, r.next() as u8),
            }
        }
        if let Some(u) = ca_issuers_url(&b) {
            assert!(u.is_ascii() && !u.bytes().any(|c| c.is_ascii_control()));
        }
    }
    // an OID at the very end, with every possible length byte
    for len in 0..=255u8 {
        let mut d = CA_ISSUERS.to_vec();
        d.extend([0x86, len]);
        let _ = ca_issuers_url(&d);
        d.extend([0x81, len]);
        let _ = ca_issuers_url(&d);
    }
}

#[test]
fn cert_response_parser_boundaries_redirects_and_garbage() {
    let mk = |status: &str, extra: &str, body: &[u8]| {
        let mut r = format!("HTTP/1.1 {status}\r\n{extra}\r\n").into_bytes();
        r.extend(body);
        r
    };
    let der = |n: usize| {
        let mut d = vec![0x30u8];
        d.resize(n, 0);
        d
    };
    assert!(parse_cert_response(&mk("200 OK", "", &der(16))).is_some());
    assert!(parse_cert_response(&mk("200 OK", "", &der(15))).is_none());
    assert!(parse_cert_response(&mk("200 OK", "", &der(64 * 1024))).is_some());
    assert!(parse_cert_response(&mk("200 OK", "", &der(64 * 1024 + 1))).is_none());
    // redirects are never followed: a 3xx, even one that carries a plausible body, is refused
    for st in ["301 Moved", "302 Found", "307 Temporary Redirect", "308 Permanent Redirect"] {
        assert!(parse_cert_response(&mk(st, "Location: http://evil.example/x.crt\r\n", &der(100))).is_none(), "{st}");
    }
    // a status number hidden in a later header or reason phrase does not count
    assert!(parse_cert_response(&mk("404 200", "X: 200\r\n", &der(100))).is_none());
    // chunked bodies (the request is HTTP/1.1) start with a hex length and are refused rather than mis-parsed
    assert!(parse_cert_response(&mk("200 OK", "Transfer-Encoding: chunked\r\n", b"64\r\n0000")).is_none());
    // Info T05-14: Content-Length is not compared with the body, a truncated download is passed on (webpki then rejects it)
    assert!(parse_cert_response(&mk("200 OK", "Content-Length: 5000\r\n", &der(100))).is_some());
    let mut r = Prng(3);
    for _ in 0..20_000 {
        let n = r.below(200);
        let _ = parse_cert_response(&r.bytes(n));
    }
}

#[test]
fn an_expired_leaf_is_refused_even_when_the_missing_intermediate_is_fetched() {
    let Some(p) = pki("http://crt.sectigo.com/Test_CA.crt", Opt { expired_leaf: true, ..Default::default() }) else {
        return;
    };
    let der = p.intermediate_der.clone();
    let v = verifier(&p, Arc::new(move |_| Some(der.clone())));
    let e = check(&v, &p.leaf_der, &[], "push.example.net", UnixTime::now()).unwrap_err();
    assert!(
        matches!(e, Error::InvalidCertificate(CertificateError::Expired | CertificateError::ExpiredContext { .. })),
        "{e:?}"
    );
}

#[test]
fn a_fetched_intermediate_that_is_not_a_ca_is_refused() {
    let Some(p) = pki("http://crt.sectigo.com/Test_CA.crt", Opt { non_ca_intermediate: true, ..Default::default() })
    else {
        eprintln!("rcgen refused to build a non-CA issuer: skipped");
        return;
    };
    let der = p.intermediate_der.clone();
    let v = verifier(&p, Arc::new(move |_| Some(der.clone())));
    assert!(check(&v, &p.leaf_der, &[], "push.example.net", UnixTime::now()).is_err());
}

#[test]
fn the_host_name_is_checked_after_the_chain_was_completed_by_fetching() {
    let Some(p) = pki("http://crt.sectigo.com/Test_CA.crt", Opt::default()) else { return };
    let der = p.intermediate_der.clone();
    let v = verifier(&p, Arc::new(move |_| Some(der.clone())));
    assert!(check(&v, &p.leaf_der, &[], "push.example.net", UnixTime::now()).is_ok());
    for wrong in ["evil.example.net", "push.example.net.evil.com", "example.net", "sub.push.example.net"] {
        assert!(check(&v, &p.leaf_der, &[], wrong, UnixTime::now()).is_err(), "{wrong} accepted");
    }
}

#[test]
fn a_chain_that_never_reaches_a_trusted_root_stops_after_the_depth_limit() {
    // the fetcher keeps answering with an intermediate (signed by an unrelated root) that again points at an allowed URL
    let Some(good) = pki("http://crt.sectigo.com/A.crt", Opt::default()) else { return };
    let Some(other) = pki("http://crt.sectigo.com/B.crt", Opt { intermediate_aia: true, ..Default::default() }) else {
        return;
    };
    let calls = Arc::new(AtomicUsize::new(0));
    let (c2, der) = (calls.clone(), other.intermediate_der.clone());
    let v = verifier(
        &good,
        Arc::new(move |_| {
            c2.fetch_add(1, Ordering::SeqCst);
            Some(der.clone())
        }),
    );
    let t = Instant::now();
    let e = check(&v, &good.leaf_der, &[], "push.example.net", UnixTime::now()).unwrap_err();
    assert!(matches!(e, Error::InvalidCertificate(_)), "{e:?}");
    assert!(calls.load(Ordering::SeqCst) <= 3, "{} fetches", calls.load(Ordering::SeqCst));
    assert!(t.elapsed() < Duration::from_secs(2));
}

#[test]
fn garbage_and_oversized_fetch_results_fail_closed_without_panicking() {
    let Some(p) = pki("http://crt.sectigo.com/Test_CA.crt", Opt::default()) else { return };
    let mut r = Prng(5);
    let outputs: Vec<Option<Vec<u8>>> = vec![
        None,
        Some(vec![]),
        Some(vec![0x30]),
        Some(vec![0x30; 100]),
        Some(r.bytes(70_000)),
        Some(p.leaf_der.clone()), // the leaf itself
    ];
    for out in outputs {
        let v = verifier(&p, Arc::new(move |_| out.clone()));
        assert!(check(&v, &p.leaf_der, &[], "push.example.net", UnixTime::now()).is_err());
    }
}

#[test]
fn a_replaceable_fetcher_is_never_asked_for_a_url_outside_the_allowlist() {
    let Some(p) = pki("http://evil.example/x.crt", Opt::default()) else { return };
    let seen = Arc::new(Mutex::new(vec![]));
    let (s2, der) = (seen.clone(), p.intermediate_der.clone());
    let v = verifier(
        &p,
        Arc::new(move |u| {
            s2.lock().unwrap().push(u.to_string());
            Some(der.clone())
        }),
    );
    assert!(check(&v, &p.leaf_der, &[], "push.example.net", UnixTime::now()).is_err());
    assert!(seen.lock().unwrap().is_empty(), "{:?}", seen.lock().unwrap());
}

// ---- WebSocket transport against hostile localhost servers -----------------------------------------------------------------------

fn hdr(k: &str, v: &str) -> (String, String) {
    (k.to_string(), v.to_string())
}

/// Accept one connection, read the upgrade request, answer 101 with a correct accept key, then run `after`.
fn ws_server(after: impl FnOnce(TcpStream) + Send + 'static) -> (u16, std::thread::JoinHandle<()>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let h = std::thread::spawn(move || {
        let (mut s, _) = l.accept().unwrap();
        s.set_read_timeout(Some(Duration::from_secs(3))).ok();
        let mut req = vec![];
        let mut b = [0u8; 1];
        while !req.ends_with(b"\r\n\r\n") {
            if s.read(&mut b).unwrap_or(0) == 0 {
                return;
            }
            req.push(b[0]);
        }
        let text = String::from_utf8_lossy(&req).to_string();
        let key = text
            .lines()
            .find_map(|l| l.strip_prefix("Sec-WebSocket-Key: "))
            .expect("client sent a key")
            .trim()
            .to_string();
        let accept = tungstenite::handshake::derive_accept_key(key.as_bytes());
        let _ = s.write_all(
            format!("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n")
                .as_bytes(),
        );
        after(s);
    });
    (port, h)
}

fn connect(port: u16) -> std::io::Result<Box<dyn PushSocket>> {
    LiveConnector.connect(&format!("ws://127.0.0.1:{port}/np/pushNotification"), &[hdr("Authorization", "Bearer T")])
}

fn text_frame(payload: &[u8]) -> Vec<u8> {
    let mut f = vec![0x81];
    match payload.len() {
        n if n < 126 => f.push(n as u8),
        n if n < 65536 => {
            f.push(126);
            f.extend((n as u16).to_be_bytes());
        }
        n => {
            f.push(127);
            f.extend((n as u64).to_be_bytes());
        }
    }
    f.extend(payload);
    f
}

fn recv_until(s: &mut Box<dyn PushSocket>, secs: u64) -> std::io::Result<Option<String>> {
    let end = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < end {
        if let Some(m) = s.recv(Duration::from_millis(100))? {
            return Ok(Some(m));
        }
    }
    Ok(None)
}

#[test]
fn invalid_frames_after_a_good_handshake_are_errors_not_panics_or_hangs() {
    let bad: Vec<Vec<u8>> = vec![
        vec![0xFF, 0xFF, 0xFF, 0xFF],                // reserved bits, bad opcode
        vec![0x81, 0x85, 1, 2, 3, 4, 5, 6, 7, 8, 9], // a MASKED frame from the server
        vec![0x01, 0x03, b'a', b'b', b'c'],          // fragment start, never finished, then...
        vec![0x82, 0x7F, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF], // 2^64-1 length
        text_frame(&[0xFF, 0xFE, 0xFD]),             // text that is not UTF-8
        vec![0x88, 0x7E, 0x00, 0x01, 0x00],          // close with an illegal 1-byte payload
    ];
    for (i, frame) in bad.into_iter().enumerate() {
        let (port, h) = ws_server(move |mut s| {
            let _ = s.write_all(&frame);
            std::thread::sleep(Duration::from_millis(400));
        });
        let mut ws = connect(port).unwrap();
        let t = Instant::now();
        let r = recv_until(&mut ws, 3);
        // either an error or "nothing yet" (the unfinished fragment), but never a message, a panic or a hang
        assert!(matches!(r, Err(_) | Ok(None)), "case {i}: {r:?}");
        assert!(t.elapsed() < Duration::from_secs(4), "case {i}");
        ws.close();
        h.join().unwrap();
    }
}

#[test]
fn a_connection_cut_in_the_middle_of_a_frame_is_an_error() {
    let (port, h) = ws_server(|mut s| {
        let _ = s.write_all(&[0x81, 100, b'x', b'y']); // announces 100 bytes, sends 2, closes
    });
    let mut ws = connect(port).unwrap();
    let r = recv_until(&mut ws, 3);
    assert!(r.is_err(), "{r:?}");
    h.join().unwrap();
}

#[test]
fn a_text_message_survives_arriving_one_byte_at_a_time() {
    let (port, h) = ws_server(|mut s| {
        for b in text_frame(b"{\"k\":1}") {
            let _ = s.write_all(&[b]);
            std::thread::sleep(Duration::from_millis(15));
        }
        std::thread::sleep(Duration::from_millis(200));
    });
    let mut ws = connect(port).unwrap();
    assert_eq!(recv_until(&mut ws, 3).unwrap().as_deref(), Some("{\"k\":1}"));
    h.join().unwrap();
}

#[test]
fn a_ping_flood_does_not_stop_a_following_message_and_is_answered_with_pongs() {
    let (port, h) = ws_server(|mut s| {
        let mut all = vec![];
        for _ in 0..3000 {
            all.extend([0x89, 0x00]);
        }
        all.extend(text_frame(b"after-the-flood"));
        let _ = s.write_all(&all);
        std::thread::sleep(Duration::from_millis(300));
    });
    let mut ws = connect(port).unwrap();
    assert_eq!(recv_until(&mut ws, 5).unwrap().as_deref(), Some("after-the-flood"));
    h.join().unwrap();
}

#[test]
fn a_text_message_over_a_sane_limit_must_be_refused() {
    let big = vec![b'a'; 3 * 1024 * 1024];
    let (port, h) = ws_server(move |mut s| {
        let _ = s.write_all(&text_frame(&big));
        std::thread::sleep(Duration::from_millis(500));
    });
    let mut ws = connect(port).unwrap();
    let r = recv_until(&mut ws, 10);
    assert!(r.is_err(), "a 3 MiB push message was delivered");
    drop(ws); // the server may still be writing: closing our side lets it finish
    h.join().unwrap();
}

#[test]
fn redirects_are_not_followed_and_report_the_status() {
    let target = TcpListener::bind("127.0.0.1:0").unwrap();
    target.set_nonblocking(true).unwrap();
    let tport = target.local_addr().unwrap().port();
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let h = std::thread::spawn(move || {
        let (mut s, _) = l.accept().unwrap();
        let mut b = [0u8; 2048];
        let _ = s.read(&mut b);
        let _ = s.write_all(
            format!("HTTP/1.1 301 Moved\r\nLocation: ws://127.0.0.1:{tport}/x\r\nContent-Length: 0\r\n\r\n").as_bytes(),
        );
    });
    let e = connect(port).err().expect("must fail");
    assert!(e.to_string().contains("HTTP 301"), "{e}");
    h.join().unwrap();
    std::thread::sleep(Duration::from_millis(200));
    assert!(target.accept().is_err(), "the client followed the redirect");
}

#[test]
fn an_oversized_handshake_answer_is_refused() {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let h = std::thread::spawn(move || {
        let (mut s, _) = l.accept().unwrap();
        let mut b = [0u8; 2048];
        let _ = s.read(&mut b);
        let _ = s.write_all(b"HTTP/1.1 101 Switching Protocols\r\nX: ");
        let junk = vec![b'a'; 4096];
        for _ in 0..8 {
            if s.write_all(&junk).is_err() {
                return;
            }
        }
    });
    let t = Instant::now();
    let e = connect(port).err().expect("must fail");
    assert!(e.to_string().contains("too long"), "{e}");
    assert!(t.elapsed() < Duration::from_secs(3));
    h.join().unwrap();
}

#[test]
fn the_host_header_cannot_be_overridden_and_the_token_is_sent_once() {
    let seen = Arc::new(Mutex::new(String::new()));
    let s2 = seen.clone();
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let h = std::thread::spawn(move || {
        let (mut s, _) = l.accept().unwrap();
        let mut got = vec![];
        let mut b = [0u8; 1];
        while !got.ends_with(b"\r\n\r\n") {
            if s.read(&mut b).unwrap_or(0) == 0 {
                break;
            }
            got.push(b[0]);
        }
        *s2.lock().unwrap() = String::from_utf8_lossy(&got).to_string();
    });
    let _ = LiveConnector.connect(
        &format!("ws://127.0.0.1:{port}/x"),
        &[hdr("Host", "evil.example"), hdr("Authorization", "Bearer SECRET")],
    );
    h.join().unwrap();
    let t = seen.lock().unwrap().clone();
    assert!(!t.contains("evil.example"), "{t}");
    assert_eq!(t.matches("Authorization").count(), 1);
}

#[test]
fn bad_urls_fail_before_any_name_lookup_or_connection() {
    // URL forms rejected locally (no resolver call); the header cases fail on the refused loopback port 1 or on the CR/LF check
    for u in [
        "",
        "ws://",
        "wss",
        "ftp://x/y",
        "http://x",
        "ws://127.0.0.1:99999/x",
        "ws://127.0.0.1:abc/x",
        "ws://127.0.0.1:-1/x",
    ] {
        assert!(LiveConnector.connect(u, &[]).is_err(), "{u:?}");
    }
    assert!(LiveConnector.connect("ws://127.0.0.1:1/x", &[hdr("A", "b\r\nC: d")]).is_err());
    assert!(LiveConnector.connect("ws://127.0.0.1:1/x", &[hdr("A\nB", "c")]).is_err());
    assert!(LiveConnector.connect("ws://127.0.0.1:1/x", &[hdr("", "c")]).is_err());
}

#[test]
fn a_server_that_accepts_but_never_answers_the_upgrade_fails_after_the_handshake_timeout() {
    // takes about 10 s: the read timeout of the handshake is a fixed 10 s per read
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let h = std::thread::spawn(move || {
        let (s, _) = l.accept().unwrap();
        std::thread::sleep(Duration::from_secs(12));
        drop(s);
    });
    let t = Instant::now();
    let e = connect(port).err().expect("must fail");
    let took = t.elapsed();
    assert!(took >= Duration::from_secs(9) && took < Duration::from_secs(12), "{took:?}: {e}");
    h.join().unwrap();
}

#[test]
fn a_slow_drip_handshake_must_fail_within_the_handshake_timeout() {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let h = std::thread::spawn(move || {
        let (mut s, _) = l.accept().unwrap();
        for _ in 0..4 {
            if s.write_all(b"H").is_err() {
                return;
            }
            std::thread::sleep(Duration::from_secs(4)); // always inside the 10 s per-read timeout
        }
    });
    let t = Instant::now();
    let _ = connect(port);
    let took = t.elapsed();
    let _ = h.join();
    assert!(took < Duration::from_secs(12), "connect() was held for {took:?} by a 4-byte drip");
}
