//! Tester A1: adversarial tests for openps-takion (chunk framing, ECDH inputs, launch spec, protobuf, Senkusha,
//! handshake resource behaviour). Deterministic PRNG only.
use openps_crypto::RpCipher;
use openps_takion::chunk::{chunk_type, class, Chunk};
use openps_takion::ecdh::{curve_for, java_to_byte_array, normalise_secret, Curve, Keypair};
use openps_takion::handshake::{Handshake, HandshakeConfig, KeyPosCounter, MAX_RESENDS, RESEND_INTERVAL_S};
use openps_takion::launch::LaunchParams;
use openps_takion::proto::*;
use openps_takion::senkusha::*;
use openps_testkit::handshake::{HandshakeConsole, HS_KEY};
use openps_testkit::senkusha::SenkushaConsole;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.next() as u8).collect()
    }
}

// ------------------------------------------------------------------ chunk
#[test]
fn chunk_decode_never_panics_on_random_and_truncated_input() {
    let mut r = Rng(11);
    for _ in 0..100_000 {
        let n = r.below(80);
        let mut b = r.bytes(n);
        if n > 0 && r.below(3) > 0 {
            b[0] &= 0xF0;
        }
        if n > 16 && r.below(2) == 0 {
            let size = [0u16, 4, 7, 8, 12, 16, 0xFFFF][r.below(7)];
            b[15..17].copy_from_slice(&size.to_be_bytes());
        }
        let _ = Chunk::decode(&b);
    }
    let full = Chunk::data(1, 2, class::GENERAL, vec![0, 1, 2, 3, 4, 5]).encode();
    for n in 0..full.len() {
        let _ = Chunk::decode(&full[..n]);
    }
}

#[test]
fn chunk_roundtrip_property() {
    let mut r = Rng(12);
    for _ in 0..2_000 {
        let len = r.below(300);
        let c = Chunk::data(r.next() as u32, r.next() as u32, r.next() as u32, r.bytes(len));
        assert_eq!(Chunk::decode(&c.encode()).unwrap(), c);
    }
}

#[test]
fn chunk_decode_ignores_the_declared_chunk_size() {
    // Declared size says 12 + 3 but the datagram carries 10 more bytes: all are taken as payload.
    let mut b = Chunk::data(1, 2, class::GENERAL, vec![0, 8, 3]).encode();
    b.extend([0xEE; 10]);
    let c = Chunk::decode(&b).unwrap();
    assert_eq!(c.payload.len(), 2 + 10);
    // Declared size smaller than 8 means "no sequence/class": payload starts at 17
    let mut small = vec![0u8; 20];
    small[13] = chunk_type::DATA_ACK;
    small[15..17].copy_from_slice(&4u16.to_be_bytes());
    let c = Chunk::decode(&small).unwrap();
    assert_eq!((c.sequence, c.class, c.payload.len()), (0, 0, 3));
}

#[test]
fn chunk_size_field_truncates_for_payloads_over_65k() {
    let c = Chunk::data(1, 2, class::GENERAL, vec![0; 70_000]);
    let enc = c.encode();
    let declared = u16::from_be_bytes([enc[15], enc[16]]) as usize;
    assert_ne!(declared, c.chunk_size(), "u16 truncation of the size field");
}

// ------------------------------------------------------------------ ecdh
#[test]
fn ecdh_rejects_invalid_peer_keys_without_panicking() {
    let mut r = Rng(13);
    for (curve, plen) in [(Curve::K256, 65usize), (Curve::P521, 133)] {
        let kp = Keypair::generate(curve);
        let version = if curve == Curve::K256 { 9 } else { 14 };
        assert!(kp.shared_secret(&[], version).is_err());
        assert!(kp.shared_secret(&[0], version).is_err()); // point at infinity
        let mut offcurve = vec![4u8];
        offcurve.extend(r.bytes(plen - 1));
        assert!(kp.shared_secret(&offcurve, version).is_err());
        let mut x00 = vec![4u8];
        x00.extend(vec![0u8; plen - 1]);
        assert!(kp.shared_secret(&x00, version).is_err());
        for _ in 0..200 {
            let n = r.below(200);
            let junk = r.bytes(n);
            let _ = kp.shared_secret(&junk, version);
        }
        // a valid key of the wrong curve
        let other = Keypair::generate(if curve == Curve::K256 { Curve::P521 } else { Curve::K256 });
        assert!(kp.shared_secret(&other.public_bytes(), version).is_err());
    }
}

#[test]
fn ecdh_bad_secret_inputs() {
    assert!(Keypair::from_secret(Curve::K256, &[]).is_err());
    assert!(Keypair::from_secret(Curve::K256, &[0u8; 32]).is_err()); // zero scalar
    assert!(Keypair::from_secret(Curve::K256, &[0xFF; 32]).is_err()); // >= group order
    assert!(Keypair::from_secret(Curve::P521, &[0u8; 66]).is_err());
}

#[test]
fn both_sides_always_derive_the_same_normalised_secret() {
    // The "short value is not zero padded" quirk (1 in 256) must still agree on both sides.
    for (curve, version) in [(Curve::K256, 9), (Curve::P521, 14)] {
        for _ in 0..300 {
            let a = Keypair::generate(curve);
            let b = Keypair::generate(curve);
            assert_eq!(
                a.shared_secret(&b.public_bytes(), version).unwrap(),
                b.shared_secret(&a.public_bytes(), version).unwrap()
            );
        }
    }
}

#[test]
fn normalise_secret_shape_properties() {
    let mut r = Rng(14);
    for _ in 0..2_000 {
        let n = if r.below(2) == 0 { 32 } else { 66 };
        let mut x = r.bytes(n);
        let zeros = r.below(4);
        for b in x.iter_mut().take(zeros) {
            *b = 0;
        }
        for v in [9, 14] {
            let s = normalise_secret(&x, v);
            assert!(!s.is_empty() && s.len() <= 67);
        }
    }
    assert_eq!(java_to_byte_array(&[0, 0, 0]), vec![0]);
    assert_eq!(java_to_byte_array(&[0, 0x80]), vec![0, 0x80]);
    assert_eq!(java_to_byte_array(&[]), vec![0]);
    assert_eq!(curve_for(i32::MIN), Curve::K256);
    assert_eq!(curve_for(i32::MAX), Curve::P521);
}

// ------------------------------------------------------------------ launch
#[test]
fn launch_params_extreme_values() {
    for v in [i32::MIN, -2, -1, 0, 1, 1999, 2000, 2001, 99_999, 100_000, 100_001, i32::MAX] {
        for auto in [None, Some(i32::MIN), Some(0), Some(i32::MAX)] {
            let p = LaunchParams {
                width: v,
                height: v,
                fps: v,
                bitrate: v,
                auto_kbps: auto,
                mtu_in: v,
                rtt_ms: v,
                ..Default::default()
            };
            let b = p.eff_bitrate();
            assert!(b >= 2000, "bitrate {b} for {v} {auto:?}");
            assert!(b <= 100_000 || b == 15000);
            assert!((576..=1454).contains(&p.eff_mtu()));
            let s = p.build("QUJD");
            assert!(s.ends_with('\u{0}') && s.starts_with('{'));
        }
    }
    // JSON validity of the template is checked structurally: balanced braces
    let s = LaunchParams::default().build("QUJD");
    assert_eq!(s.matches('{').count(), s.matches('}').count());
}

// ------------------------------------------------------------------ proto
#[test]
fn protobuf_parse_random_bytes_never_panics() {
    let mut r = Rng(15);
    for _ in 0..50_000 {
        let n = r.below(120);
        let b = r.bytes(n);
        let _ = TakionMessage::parse(&b);
    }
    // a length-prefixed field claiming 4 GiB
    let _ = TakionMessage::parse(&[0x12, 0xFF, 0xFF, 0xFF, 0xFF, 0x0F, 1, 2, 3]);
    // deeply nested unknown group tags
    let _ = TakionMessage::parse(&vec![0x0B; 10_000]);
}

// ------------------------------------------------------------------ senkusha
#[test]
fn rtt_ms_does_not_overflow_with_large_samples() {
    let s: Vec<i64> = (0..10).map(|i| if i % 2 == 0 { 0 } else { 3_000_000_000 }).collect();
    let r = std::panic::catch_unwind(|| rtt_ms(&s));
    assert!(r.is_ok(), "overflow panic");
}

#[test]
fn rtt_ms_basic_robustness() {
    assert_eq!(rtt_ms(&[-5_000_000, 5_000_000, 5_000_000]), Some(5)); // negative sample tolerated, extremes trimmed
    for n in 1..30usize {
        let s: Vec<i64> = (0..n as i64).map(|i| (i + 1) * 1_000_000).collect();
        assert!(rtt_ms(&s).is_some());
    }
}

fn run_with_console(
    s: &mut Senkusha,
    con: &mut SenkushaConsole,
    rtt_s: f64,
    mut inject: impl FnMut(&mut Senkusha, f64),
) -> f64 {
    let mut now = 0.0;
    for _ in 0..200_000 {
        let mut progressed = false;
        while let Some(p) = s.poll_transmit(now) {
            progressed = true;
            let replies = con.on_packet(&p);
            if !replies.is_empty() {
                now += rtt_s;
                for r in replies {
                    s.handle_datagram(now, &r);
                }
            }
            inject(s, now);
        }
        if !matches!(s.state(), State::Running) {
            return now;
        }
        if !progressed {
            match s.poll_timeout() {
                Some(t) => {
                    now = now.max(t);
                    s.handle_timeout(now);
                }
                None => return now,
            }
        }
    }
    panic!("did not settle");
}

fn senkusha(ps5: bool) -> Senkusha {
    let mut n = 7u32;
    Senkusha::new(SenkushaConfig { ps5, ..Default::default() }, move || {
        n = n.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        n
    })
}

#[test]
fn senkusha_survives_garbage_injected_after_every_packet() {
    let mut r = Rng(21);
    for ps5 in [true, false] {
        let mut s = senkusha(ps5);
        let mut con = SenkushaConsole::new(ps5, 1350, 1300);
        run_with_console(&mut s, &mut con, 0.003, |s, now| {
            for _ in 0..3 {
                let n = r.below(100);
                let mut g = r.bytes(n);
                if !g.is_empty() && r.below(2) == 0 {
                    g[0] = [0u8, 2, 3][r.below(3)];
                }
                s.handle_datagram(now, &g);
            }
        });
        // The run must end (Complete or Failed), never hang or panic. Garbage is allowed to disturb the result.
        assert!(!matches!(s.state(), State::Running), "ps5={ps5}");
    }
}

#[test]
fn senkusha_gives_up_after_three_attempts_with_a_silent_console() {
    let mut s = senkusha(true);
    let mut now = 0.0;
    let mut sent = 0;
    for _ in 0..1000 {
        while s.poll_transmit(now).is_some() {
            sent += 1;
        }
        match s.poll_timeout() {
            Some(t) => {
                now = now.max(t);
                s.handle_timeout(now);
            }
            None => break,
        }
    }
    assert!(matches!(s.state(), State::Failed(_)), "{:?}", s.state());
    assert_eq!(sent, 3 * 4, "3 attempts x 4 INIT sends");
    assert!(now < 3.0, "virtual time used: {now}");
}

#[test]
fn senkusha_minimum_path_mtu_gives_a_sane_result() {
    // A path that only just carries the 548 B RTT probes (576 B IP packets).
    let mut s = senkusha(true);
    let mut con = SenkushaConsole::new(true, 576, 576);
    run_with_console(&mut s, &mut con, 0.002, |_, _| {});
    match s.state() {
        State::Complete(r) => {
            println!("min-path result: {r:?}");
            assert!((575..=1454).contains(&r.mtu_in) && (575..=1454).contains(&r.mtu_out));
        }
        other => panic!("{other:?}"),
    }
}

/// Reply identity is never checked: every datagram whose first byte is 3 is accepted as the answer to the
/// outstanding RTT probe (stale, duplicated or foreign audio packets included).
#[test]
#[ignore = "BUG T02-04 (NOT FIXED on purpose): the real console's echo format is unconfirmed (no capture), so checking nonce/index could fail every real Senkusha run; needs a capture"]
fn senkusha_duplicate_echo_is_not_counted_twice() {
    let mut s = senkusha(true);
    let mut con = SenkushaConsole::new(true, 1454, 1454);
    let mut now = 0.0;
    // run the handshake part until the first RTT probe (548 B, type 3) appears
    let mut first_probe = None;
    for _ in 0..10_000 {
        while let Some(p) = s.poll_transmit(now) {
            if p[0] & 0x0F == 3 && p.len() == RTT_PROBE_LEN {
                first_probe = Some(p);
                break;
            }
            for r in con.on_packet(&p) {
                s.handle_datagram(now, &r);
            }
        }
        if first_probe.is_some() {
            break;
        }
        if let Some(t) = s.poll_timeout() {
            now = now.max(t);
            s.handle_timeout(now);
        }
    }
    let probe = first_probe.expect("reached RTT phase");
    now += 0.001;
    s.handle_datagram(now, &probe); // genuine echo
    s.handle_datagram(now, &probe); // duplicate of the same echo
    let mut idx = vec![];
    while let Some(p) = s.poll_transmit(now) {
        if p[0] & 0x0F == 3 {
            idx.push(u32::from_be_bytes([p[5], p[6], p[7], p[8]]) >> 21);
        }
    }
    assert_eq!(idx, vec![1], "only probe #1 may be outstanding after one real reply, got {idx:?}");
}

#[test]
fn senkusha_duplicate_echo_current_behaviour() {
    // Same scenario as the ignored test, asserting what happens today so the effect is on record.
    let mut s = senkusha(true);
    let mut con = SenkushaConsole::new(true, 1454, 1454);
    let mut now = 0.0;
    let mut probe = None;
    'outer: for _ in 0..10_000 {
        while let Some(p) = s.poll_transmit(now) {
            if p[0] & 0x0F == 3 && p.len() == RTT_PROBE_LEN {
                probe = Some(p);
                break 'outer;
            }
            for r in con.on_packet(&p) {
                s.handle_datagram(now, &r);
            }
        }
        if let Some(t) = s.poll_timeout() {
            now = now.max(t);
            s.handle_timeout(now);
        }
    }
    let probe = probe.unwrap();
    s.handle_datagram(now + 0.001, &probe);
    s.handle_datagram(now + 0.001, &probe);
    let mut idx = vec![];
    while let Some(p) = s.poll_transmit(now) {
        if p[0] & 0x0F == 3 {
            idx.push(u32::from_be_bytes([p[5], p[6], p[7], p[8]]) >> 21);
        }
    }
    assert_eq!(idx, vec![1, 2]);
}

// ------------------------------------------------------------------ handshake
fn hs_cfg(mtu_out: usize) -> HandshakeConfig {
    HandshakeConfig {
        ps5: true,
        request_takion20: false,
        session_key: "sess-1".into(),
        mtu_out,
        launch: LaunchParams::default(),
        handshake_key: HS_KEY,
        session_cipher: RpCipher::new([1; 16], [2; 16], [3; 16]),
        ecdh_secret: None,
        resend_interval_s: RESEND_INTERVAL_S,
        max_resends: MAX_RESENDS,
    }
}

/// Count the datagrams needed to carry Big for a given outbound MTU (pathological values included).
fn big_datagrams(mtu_out: usize) -> usize {
    let mut h = Handshake::new(hs_cfg(mtu_out));
    let mut con = HandshakeConsole::new(14);
    let mut now = 0.0;
    let mut total = 0;
    for _ in 0..500 {
        let mut any = false;
        while let Some(p) = h.poll_transmit(now) {
            any = true;
            total += 1;
            for r in con.on_packet(&p) {
                h.handle_datagram(now, &r);
            }
        }
        if !any {
            break;
        }
        now += 0.01;
    }
    total
}

#[test]
fn mtu_out_edge_values_do_not_panic_and_fragment_count_is_bounded_by_payload() {
    let normal = big_datagrams(1454);
    let m200 = big_datagrams(200);
    let m60 = big_datagrams(60);
    let m0 = big_datagrams(0);
    println!("datagrams: mtu1454={normal} mtu200={m200} mtu60={m60} mtu0={m0}");
    assert!(m200 > normal);
    assert!(m0 >= m60);
}

#[test]
fn silent_console_fails_after_exactly_max_resends_and_stops_sending() {
    let mut h = Handshake::new(hs_cfg(1454));
    let mut now = 0.0;
    let mut sent = 0;
    for _ in 0..50 {
        while h.poll_transmit(now).is_some() {
            sent += 1;
        }
        match h.poll_timeout() {
            Some(t) => {
                now = t;
                h.handle_timeout(now);
            }
            None => break,
        }
    }
    assert!(h.failed().is_some());
    assert_eq!(sent, 1 + MAX_RESENDS as usize);
    now += 100.0;
    h.handle_timeout(now);
    assert!(h.poll_transmit(now).is_none(), "a failed handshake must stay quiet");
}

#[test]
fn zero_resend_interval_cannot_spin() {
    let mut cfg = hs_cfg(1454);
    cfg.resend_interval_s = 0.0;
    let mut h = Handshake::new(cfg);
    let mut sent = 0;
    for _ in 0..100 {
        while h.poll_transmit(0.0).is_some() {
            sent += 1;
        }
        h.handle_timeout(0.0);
    }
    assert!(h.failed().is_some());
    assert_eq!(sent, 1 + MAX_RESENDS as usize);
}

#[test]
fn key_pos_counter_wraps_exactly_at_2_pow_32() {
    let mut c = KeyPosCounter::default();
    let total = (1u64 << 32) / 16; // allocations before the wrap
    let mut last = 0;
    for i in 0..total {
        let v = c.allocate();
        assert_eq!(v as u64, i * 16);
        last = v;
    }
    assert_eq!(last, 0xFFFF_FFF0);
    assert_eq!(c.allocate(), 0, "wraps to 0");
    assert_eq!(c.allocate(), 16);
}
