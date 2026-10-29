//! Tester A1: adversarial/boundary/property tests for openps-crypto.
use openps_crypto::{derive, Context, Direction, KeyPosTracker, RpCipher};
use std::time::Instant;

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

fn ctx() -> Context {
    Context::new(Direction::Remote, &[0x42; 16], b"tester-secret")
}

#[test]
fn ctr_chunked_equals_whole_for_random_offsets() {
    let c = ctx();
    let mut r = Rng(1);
    for _ in 0..300 {
        let off = r.next() >> r.below(40); // anything from small to ~2^64
        let len = r.below(700);
        let plain = r.bytes(len);
        let mut whole = plain.clone();
        c.ctr_xor(off, &mut whole);
        // split at a random point; second half encrypted at off + split
        let split = r.below(len + 1);
        let mut a = plain[..split].to_vec();
        let mut b = plain[split..].to_vec();
        c.ctr_xor(off, &mut a);
        c.ctr_xor(off.wrapping_add(split as u64), &mut b);
        a.extend(b);
        assert_eq!(a, whole, "off={off} len={len} split={split}");
    }
}

#[test]
fn ctr_extreme_offsets_do_not_panic() {
    let c = ctx();
    let mut buf = vec![0u8; 1000];
    for off in [0u64, 1, 15, 16, u32::MAX as u64, (1 << 32) + 16, u64::MAX / 16 * 16, u64::MAX - 1, u64::MAX] {
        c.ctr_xor(off, &mut buf);
    }
    c.ctr_xor(7, &mut []); // empty buffer
}

#[test]
fn ctr_is_an_involution_and_is_not_identity() {
    let c = ctx();
    let mut r = Rng(9);
    let plain = r.bytes(1500);
    let mut x = plain.clone();
    c.ctr_xor(1 << 33, &mut x);
    assert_ne!(x, plain);
    c.ctr_xor(1 << 33, &mut x);
    assert_eq!(x, plain);
}

#[test]
fn gmac_boundary_inputs_do_not_panic() {
    let mut c = Context::new(Direction::Local, &[1; 16], b"s");
    let _ = c.gmac(0, &[]);
    let _ = c.gmac(u32::MAX, &[]);
    let _ = c.gmac(u32::MAX, &vec![0xAA; 65_535]);
    let _ = c.gmac(0, &[1]); // key_pos went "backwards" across ~95k epochs
}

#[test]
fn gmac_is_deterministic_for_monotonic_key_pos() {
    // verifier and signer walking the same monotonic keyPos sequence must agree, incl. across epochs.
    let mut a = Context::new(Direction::Local, &[1; 16], b"s");
    let mut b = Context::new(Direction::Local, &[1; 16], b"s");
    let p = [7u8; 40];
    for kp in (0u32..).step_by(16 * 997).take(2000) {
        assert_eq!(a.gmac(kp, &p), b.gmac(kp, &p));
    }
    // epoch edge: (kp - 1) / 45000
    let mut a = Context::new(Direction::Local, &[1; 16], b"s");
    let t_edge = a.gmac(45_000, &p);
    let mut fresh = Context::new(Direction::Local, &[1; 16], b"s");
    assert_eq!(fresh.gmac(45_000, &p), t_edge);
}

#[test]
fn gmac_depends_on_every_input() {
    let mut c = Context::new(Direction::Local, &[1; 16], b"s");
    let p = vec![5u8; 64];
    let base = c.gmac(32, &p);
    let mut q = p.clone();
    q[63] ^= 1;
    assert_ne!(c.gmac(32, &q), base); // 1 in 2^32 false-failure chance is accepted
    assert_ne!(c.gmac(48, &p), base);
    let mut d = Context::new(Direction::Remote, &[1; 16], b"s");
    assert_ne!(d.gmac(32, &p), base);
}

/// After the outbound keyPos wraps to 0, epoch 0 is MAC'd with whatever key is cached from the highest epoch
/// reached. A fresh context (which is what a peer that never saw the previous epochs derives) disagrees.
#[test]
fn gmac_statefulness_after_epoch_regression() {
    let p = [3u8; 24];
    let mut used = Context::new(Direction::Local, &[9; 16], b"s");
    let _ = used.gmac(0xFFFF_FFF0, &p);
    let after_wrap = used.gmac(16, &p);
    let mut fresh = Context::new(Direction::Local, &[9; 16], b"s");
    let ref_tag = fresh.gmac(16, &p);
    println!("after wrap {after_wrap:02x?} vs fresh {ref_tag:02x?}");
    assert_ne!(after_wrap, ref_tag, "documents spec-literal behaviour: result depends on history");
}

#[test]
fn derive_handles_huge_epochs() {
    let _ = derive(&[1; 16], &[0xFF; 16], u64::MAX); // epoch * 44910 must not overflow
    assert_ne!(derive(&[1; 16], &[2; 16], 0), derive(&[1; 16], &[2; 16], 1));
}

#[test]
fn keypos_tracker_in_order_across_wrap() {
    let mut t = KeyPosTracker::default();
    let mut expect_wrap = 0u64;
    let mut kp: u64 = 0xFFFF_FF00;
    for _ in 0..100 {
        let k32 = (kp & 0xFFFF_FFFF) as u32;
        if kp >> 32 > expect_wrap {
            expect_wrap = kp >> 32;
        }
        assert_eq!(t.offset(k32), kp + 16);
        kp += 16;
    }
}

/// A packet delayed across the 2^32 boundary is decrypted with the wrong offset AND permanently shifts the wrap
/// counter: every later packet is then decrypted at the wrong offset.
#[test]
fn keypos_tracker_survives_reordering_at_wrap() {
    let mut t = KeyPosTracker::default();
    assert_eq!(t.offset(0xFFFF_FFE0), 0xFFFF_FFE0 + 16);
    assert_eq!(t.offset(0x0000_0010), (1u64 << 32) + 0x10 + 16); // wrapped: ok
                                                                 // late packet from before the wrap
    assert_eq!(t.offset(0xFFFF_FFF0), 0xFFFF_FFF0 + 16, "late pre-wrap packet must use wrap 0");
    // and the stream must continue at wrap 1
    assert_eq!(t.offset(0x0000_0020), (1u64 << 32) + 0x20 + 16, "wrap counter permanently advanced");
}

#[test]
fn keypos_tracker_current_behaviour_on_reordering_is_wrong_but_bounded() {
    let mut t = KeyPosTracker::default();
    t.offset(0xFFFF_FFE0);
    t.offset(0x10);
    let late = t.offset(0xFFFF_FFF0);
    assert_eq!(late, 0xFFFF_FFF0 + 16); // previous epoch applied (T01-02 fixed)
    let next = t.offset(0x20);
    assert_eq!(next, (1u64 << 32) + 0x20 + 16); // stream continues in epoch 1
}

#[test]
fn keypos_tracker_forward_jump_never_wraps_and_backward_small_never_wraps() {
    let mut t = KeyPosTracker::default();
    t.offset(100);
    assert_eq!(t.offset(4_000_000_100), 4_000_000_100 + 16); // +4e9 forward: no wrap
    let mut t = KeyPosTracker::default();
    t.offset(3_999_999_999);
    assert_eq!(t.offset(0), 16); // delta exactly -3_999_999_999 (>= -4e9): no wrap
    let mut t = KeyPosTracker::default();
    t.offset(4_000_000_001);
    assert_eq!(t.offset(0), (1u64 << 32) + 16); // delta < -4e9: wrap
}

#[test]
fn rp_cipher_roundtrip_property_and_counter_wrap() {
    let mut r = Rng(3);
    let enc = RpCipher::new([1; 16], [2; 16], [3; 16]);
    let mut dec = enc.clone();
    for i in 0..200u32 {
        let l = r.below(200);
        let msg = r.bytes(l);
        let mut m = msg.clone();
        enc.encrypt(&mut m, i);
        if msg.len() > 8 {
            assert_ne!(m, msg);
        }
        dec.decrypt_next(&mut m);
        assert_eq!(m, msg);
    }
    // counter wraps at u32::MAX without panicking
    let c = RpCipher::new([1; 16], [2; 16], [3; 16]);
    let mut d = vec![1u8; 5];
    c.encrypt(&mut d, u32::MAX);
    c.decrypt(&mut d, u32::MAX);
    assert_eq!(d, vec![1u8; 5]);
}

#[test]
fn rp_cipher_decrypt_next_wraps() {
    let mut c = RpCipher::new([1; 16], [2; 16], [3; 16]);
    let mut d = vec![0u8; 3];
    for _ in 0..3 {
        c.decrypt_next(&mut d);
    }
    // internal counter is private; just ensure many calls do not panic
    for _ in 0..10_000 {
        c.decrypt_next(&mut d);
    }
}

/// Throughput probe (prints; run with --release). Video needs ~12.5 MB/s at 100 Mbit/s.
#[test]
fn perf_ctr_and_gmac() {
    let c = ctx();
    let mut buf = vec![0u8; 1400];
    let n = if cfg!(debug_assertions) { 2_000 } else { 100_000 };
    let t = Instant::now();
    for i in 0..n {
        c.ctr_xor(i as u64 * 16 + 16, &mut buf);
    }
    let e = t.elapsed();
    let mbps = (n as f64 * 1400.0) / e.as_secs_f64() / 1e6;
    println!("ctr_xor 1400 B: {:?}/pkt = {mbps:.1} MB/s", e / n as u32);
    let mut g = Context::new(Direction::Local, &[1; 16], b"s");
    let pkt = vec![0u8; 60];
    let t = Instant::now();
    for i in 0..n {
        let _ = g.gmac(i as u32 * 16, &pkt);
    }
    let e = t.elapsed();
    println!("gmac 60 B: {:?}/pkt = {:.0} pkt/s", e / n as u32, n as f64 / e.as_secs_f64());
    if !cfg!(debug_assertions) {
        assert!(mbps > 12.5 * 4.0, "ctr too slow for 100 Mbit/s video with margin: {mbps} MB/s");
    }
}
