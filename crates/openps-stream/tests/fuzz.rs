//! Deterministic fuzzing of everything that parses untrusted network bytes: random datagrams and
//! structure-aware mutations of valid ones must never panic, hang or grow memory without bound.
use openps_crypto::{Context, Direction};
use openps_stream::{Stream, StreamConfig};
use openps_takion::chunk::{class, Chunk};
use openps_takion::handshake::{KeyPosCounter, Session};
use openps_takion::proto::*;

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
    /// `base` random bytes plus up to `extra` more.
    fn some(&mut self, base: usize, extra: usize) -> Vec<u8> {
        let n = base + self.below(extra);
        self.bytes(n)
    }
}

fn stream(version: i32, ps5: bool) -> Stream {
    let s = Session {
        remote_tag: 1,
        next_tsn: 2,
        takion_version: version,
        stream_info: StreamInfoPayload::default(),
        local: Context::new(Direction::Local, &[7; 16], b"k"),
        remote: Context::new(Direction::Remote, &[7; 16], b"k"),
        key_pos: KeyPosCounter::default(),
    };
    Stream::new(s, StreamConfig { ps5, ..Default::default() })
}

/// A plausible packet of a random kind with random field values, then a few random byte mutations.
fn structured(r: &mut Rng) -> Vec<u8> {
    let mut p = match r.below(4) {
        0 => {
            // video
            let mut v = r.some(21, 1300);
            v[0] = 0x02 | (r.below(2) as u8) << 4;
            v
        }
        1 => {
            // audio
            let mut v = r.some(19, 400);
            v[0] = 0x03;
            v
        }
        2 => {
            // control chunk with random class/flags/payload
            let kinds = [0u8, 7, 9, 11, 0xFF];
            let mut payload = vec![kinds[r.below(kinds.len())]];
            payload.extend(r.some(0, 120));
            Chunk::new(
                r.next() as u32,
                [0, 1, 2, 3, 10, 11][r.below(6)],
                (r.below(2)) as u8,
                r.next() as u32,
                [class::GENERAL, class::CONTROL, 0][r.below(3)],
                payload,
            )
            .encode()
        }
        _ => r.some(0, 64),
    };
    for _ in 0..r.below(4) {
        if !p.is_empty() {
            let i = r.below(p.len());
            p[i] = r.next() as u8;
        }
    }
    if r.below(8) == 0 {
        let n = r.below(p.len() + 1);
        p.truncate(n);
    }
    p
}

#[test]
fn stream_survives_random_and_mutated_datagrams() {
    for (version, ps5) in [(9, false), (12, true), (20, true)] {
        let mut s = stream(version, ps5);
        let mut r = Rng(0x9E37_79B9_7F4A_7C15 ^ version as u64);
        let mut now = 0.0;
        for i in 0..60_000 {
            now += 0.001;
            let pkt = if i % 3 == 0 {
                let n = r.below(1500);
                r.bytes(n)
            } else {
                structured(&mut r)
            };
            let _ = s.handle_datagram(now, &pkt);
            if i % 50 == 0 {
                s.tick(now);
            }
            while s.poll_transmit().is_some() {}
        }
    }
}

#[test]
fn many_distinct_frames_keep_memory_bounded_through_the_stream() {
    let mut s = stream(12, false);
    let mut r = Rng(42);
    for i in 0..20_000u32 {
        let mut v = r.bytes(40);
        v[0] = 0x02;
        v[3..5].copy_from_slice(&((i as u16).wrapping_mul(997)).to_be_bytes()); // distinct frame ids
        v[5..9].copy_from_slice(&((3u32 << 10) | 1).to_be_bytes()); // unit 0 of 4, 1 parity
        let _ = s.handle_datagram(0.0, &v);
    }
}

#[test]
fn protobuf_parser_never_panics_on_garbage() {
    let mut r = Rng(7);
    for _ in 0..50_000 {
        let n = r.below(300);
        let b = r.bytes(n);
        let _ = TakionMessage::parse(&b);
    }
}
