//! Tester A2: hostile UDP answers, parser fuzz and timing behaviour of discovery / wake, all on loopback.
use openps_discovery::*;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const LO: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

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

/// A UDP "console" that answers every datagram with `reply(request_index)` datagrams, from `answer_from` (or the same socket).
fn responder(
    replies: impl Fn(usize) -> Vec<Vec<u8>> + Send + 'static,
    other_source: bool,
) -> (u16, Arc<AtomicBool>, std::thread::JoinHandle<usize>) {
    let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
    sock.set_read_timeout(Some(Duration::from_millis(20))).unwrap();
    let port = sock.local_addr().unwrap().port();
    let stop = Arc::new(AtomicBool::new(false));
    let s2 = stop.clone();
    let h = std::thread::spawn(move || {
        let spoof = UdpSocket::bind("127.0.0.1:0").unwrap();
        let mut n = 0;
        let mut buf = [0u8; 2048];
        while !s2.load(Ordering::Relaxed) {
            if let Ok((_, from)) = sock.recv_from(&mut buf) {
                for r in replies(n) {
                    let _ = if other_source { spoof.send_to(&r, from) } else { sock.send_to(&r, from) };
                }
                n += 1;
            }
        }
        n
    });
    (port, stop, h)
}

fn opts(port: u16, listen_ms: u64) -> ScanOptions {
    ScanOptions {
        targets: vec![LO],
        devices: vec![DeviceType::Ps5],
        listen: Duration::from_millis(listen_ms),
        sends: 1,
        send_interval: Duration::from_millis(10),
        port_override: Some(port),
    }
}

fn ok_reply(id: &str) -> Vec<u8> {
    format!("HTTP/1.1 200 Ok\r\nhost-id:{id}\r\nhost-type:PS5\r\nhost-name:N {id}\r\n\r\n").into_bytes()
}

#[test]
fn parse_response_fuzz_never_panics() {
    let mut r = Prng(0xC0FFEE);
    let seeds: [&[u8]; 3] = [
        b"HTTP/1.1 200 Ok\r\nhost-id:AA\r\nhost-type:PS5\r\nhost-name:x\r\n\r\n",
        b"HTTP/1.1 620 Server Standby\nhost-id:1\nhost-type:ps4\n",
        "HTTP/1.1 200 \u{e9}\u{4e2d}\r\nhost-id:\u{1F600}\r\n".as_bytes(),
    ];
    for i in 0..50_000 {
        let mut b = if i % 4 == 0 {
            let n = r.below(200);
            r.bytes(n)
        } else {
            seeds[r.below(3)].to_vec()
        };
        for _ in 0..r.below(5) {
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
        if let Some(resp) = parse_response(&b) {
            let _ = console_from_response(&resp, LO);
        }
    }
}

#[test]
fn registration_key_and_wake_packet_fuzz_never_panics() {
    let mut r = Prng(5);
    for _ in 0..20_000 {
        let n = r.below(40);
        let s: String = (0..n).map(|_| char::from_u32(r.below(0x300) as u32).unwrap_or('x')).collect();
        let _ = user_credential(&s);
        let _ = wake_packet(DeviceType::Ps5, &s);
        let hex: String =
            (0..r.below(40)).map(|_| "0123456789abcdefABCDEFgz+- ".chars().nth(r.below(27)).unwrap()).collect();
        let _ = user_credential(&hex);
    }
    // multi-byte characters are rejected, not sliced in the middle
    assert!(user_credential("é1").is_err());
    assert!(user_credential("\u{1F600}\u{1F600}").is_err());
}

#[test]
fn credential_edge_cases() {
    let enc = |t: &str| -> String { t.bytes().map(|b| format!("{b:02x}")).collect() };
    assert_eq!(user_credential(&enc("7fffffffffffffff")), Ok(i64::MAX));
    assert!(user_credential(&enc("8000000000000000")).is_err());
    assert_eq!(user_credential(&enc("-1")), Ok(-1)); // same as Java's Long.parseLong; a negative credential is sent as is
    assert!(user_credential(&enc("+1")).is_err());
    assert!(user_credential(&enc("")).is_err());
    assert!(user_credential(&enc(" 1")).is_err());
    assert!(user_credential(&enc("1 ")).is_err());
    assert!(user_credential(&enc("0x1")).is_err());
}

#[test]
fn a_garbage_flood_and_oversized_datagrams_do_not_break_a_scan() {
    let (port, stop, h) = responder(
        |_| {
            let mut v = vec![vec![0xFF; 1500], vec![], b"HTTP/1.1".to_vec(), vec![b'a'; 2000]];
            // a datagram longer than the 2048-byte receive buffer is truncated by the OS: must still parse or be ignored
            let mut big = ok_reply("BIG");
            big.extend(std::iter::repeat_n(b'x', 5000));
            v.push(big);
            v.push(ok_reply("GOOD"));
            v
        },
        false,
    );
    let found = scan(&opts(port, 300)).unwrap();
    stop.store(true, Ordering::Relaxed);
    h.join().unwrap();
    assert!(found.iter().any(|c| c.host_id == "GOOD"));
}

#[test]
fn a_flood_of_distinct_console_ids_is_is_capped() {
    // Info T03-10: `scan` keeps one entry per distinct host-id with no limit; any LAN host can fill the list.
    let (port, stop, h) = responder(|_| (0..2000).map(|i| ok_reply(&format!("ID{i}"))).collect(), false);
    let t0 = Instant::now();
    let found = scan(&opts(port, 300)).unwrap();
    stop.store(true, Ordering::Relaxed);
    h.join().unwrap();
    assert!(t0.elapsed() < Duration::from_secs(2));
    assert_eq!(found.len(), openps_discovery::MAX_CONSOLES);
}

#[test]
fn control_characters_and_long_names_are_sanitised() {
    // Info T03-11: host-name is shown to the user as received (<= ~2 KiB, control characters allowed).
    let name = format!("{}\x1b[31m", "N".repeat(1500));
    let n2 = name.clone();
    let (port, stop, h) = responder(
        move |_| vec![format!("HTTP/1.1 200 Ok\r\nhost-id:X\r\nhost-type:PS5\r\nhost-name:{n2}\r\n\r\n").into_bytes()],
        false,
    );
    let found = scan(&opts(port, 200)).unwrap();
    stop.store(true, Ordering::Relaxed);
    h.join().unwrap();
    assert_eq!(found[0].host_name, "N".repeat(64));
}

#[test]
fn answers_from_a_socket_that_was_never_probed_are_accepted_documenting_no_source_check() {
    // Low T03-09: replies are matched by content only; the sender port (and, for a unicast probe, the sender address) is not
    // compared with the target, so any host that can reach the scanner's ephemeral port can inject a console / a "Ready" answer.
    let (port, stop, h) = responder(|_| vec![ok_reply("SPOOFED")], true);
    let found = scan(&opts(port, 300)).unwrap();
    stop.store(true, Ordering::Relaxed);
    h.join().unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].host_id, "SPOOFED");
}

#[test]
fn wait_until_ready_overshoots_its_timeout_by_at_most_one_poll() {
    let silent = UdpSocket::bind("127.0.0.1:0").unwrap();
    let t0 = Instant::now();
    let ready =
        wait_until_ready(DeviceType::Ps5, LO, Duration::from_millis(900), Some(silent.local_addr().unwrap().port()))
            .unwrap();
    let e = t0.elapsed();
    assert!(!ready);
    assert!(e >= Duration::from_millis(900) && e < Duration::from_millis(1700), "{e:?}");
    // a zero timeout returns at once without sending anything
    let t0 = Instant::now();
    assert!(!wait_until_ready(DeviceType::Ps5, LO, Duration::ZERO, Some(9)).unwrap());
    assert!(t0.elapsed() < Duration::from_millis(100));
}

#[test]
fn wake_sends_exactly_count_packets_and_respects_the_interval() {
    let l = UdpSocket::bind("127.0.0.1:0").unwrap();
    l.set_read_timeout(Some(Duration::from_millis(300))).unwrap();
    let key: String = "00000064".bytes().map(|b| format!("{b:02x}")).collect();
    let t0 = Instant::now();
    wake(DeviceType::Ps4, LO, &key, 4, Duration::from_millis(50), Some(l.local_addr().unwrap().port())).unwrap();
    let e = t0.elapsed();
    assert!(e >= Duration::from_millis(150) && e < Duration::from_millis(600), "{e:?}"); // 3 gaps, none after the last
    let mut n = 0;
    let mut b = [0u8; 512];
    while l.recv_from(&mut b).is_ok() {
        n += 1;
    }
    assert_eq!(n, 4);
    // count 0 sends nothing and succeeds
    wake(DeviceType::Ps4, LO, &key, 0, Duration::ZERO, Some(9)).unwrap();
}

#[test]
fn ipv6_targets_are_an_error_for_wake_and_an_error_for_scan() {
    // Info T03-12: the socket is IPv4-only; scan swallows the send error, wake reports it.
    let v6: IpAddr = "::1".parse().unwrap();
    let key: String = "00000064".bytes().map(|b| format!("{b:02x}")).collect();
    assert!(wake(DeviceType::Ps5, v6, &key, 1, Duration::ZERO, Some(9)).is_err());
    let o = ScanOptions { targets: vec![v6], listen: Duration::from_millis(50), sends: 1, ..ScanOptions::default() };
    assert!(scan(&o).is_err(), "all sends failed must be reported");
}

#[test]
fn probe_registration_returns_the_sender_of_any_datagram_even_garbage() {
    // Info: only the sender matters by design; a stray datagram from anyone ends the probe successfully.
    let (port, stop, h) = responder(|_| vec![vec![0xFF, 0x00, 0x01]], true);
    let r = probe_registration(DeviceType::Ps5, LO, 1, Duration::from_millis(100), Duration::from_millis(500), port)
        .unwrap();
    stop.store(true, Ordering::Relaxed);
    h.join().unwrap();
    assert!(r.is_some());
    assert_ne!(r.map(|a: SocketAddr| a.port()), Some(port), "answer came from another socket and was still accepted");
}

#[test]
fn scan_with_zero_sends_still_sends_once_and_does_not_loop() {
    let (port, stop, h) = responder(|_| vec![ok_reply("A")], false);
    let mut o = opts(port, 150);
    o.sends = 0;
    let found = scan(&o).unwrap();
    stop.store(true, Ordering::Relaxed);
    assert_eq!(h.join().unwrap(), 1);
    assert_eq!(found.len(), 1);
}
