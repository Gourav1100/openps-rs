//! Mock console for the streaming phase: builds encrypted video/audio/control packets and verifies the
//! client's GMAC-protected and encrypted uplink packets using independently derived keys.
use openps_crypto::{Context, Direction};
use openps_takion::chunk::{class, Chunk};
use openps_takion::handshake::{KeyPosCounter, Session};
use openps_takion::proto::*;

pub const HK: [u8; 16] = [0x42; 16];
pub const SECRET: &[u8] = b"shared-secret";
pub const TAG: u32 = 0x1000_0001;

pub fn session(version: i32) -> Session {
    Session {
        remote_tag: TAG,
        next_tsn: 0x4827,
        takion_version: version,
        stream_info: StreamInfoPayload::default(),
        local: Context::new(Direction::Local, &HK, SECRET),
        remote: Context::new(Direction::Remote, &HK, SECRET),
        key_pos: KeyPosCounter::default(),
    }
}

pub struct StreamConsole {
    pub down: Context, // server -> client encryption (tag 3)
    pub up: Context,   // client -> server verification (tag 2)
    pub seq: u32,
    pub pos: u64, // downlink keyPos, 64-bit so tests can cross the u32 wrap
}

impl StreamConsole {
    pub fn new() -> Self {
        Self {
            down: Context::new(Direction::Remote, &HK, SECRET),
            up: Context::new(Direction::Local, &HK, SECRET),
            seq: TAG,
            pos: 0,
        }
    }

    pub fn next_pos(&mut self) -> (u32, u64) {
        let p = self.pos;
        self.pos += 16;
        ((p & 0xFFFF_FFFF) as u32, p)
    }

    pub fn video(&mut self, version: i32, frame: u16, idx: u16, total: u16, fec: u16, unit: &[u8]) -> Vec<u8> {
        let t20 = version >= 20;
        let (kp, full) = self.next_pos();
        let mut payload = unit.to_vec();
        self.down.ctr_xor(full + 16, &mut payload);
        let word: u32 = ((idx as u32) << 21) | (((total - 1) as u32) << 10) | fec as u32;
        let mut p;
        if t20 {
            p = vec![0u8; 29];
            p[0] = 2;
            p[11..13].copy_from_slice(&frame.to_be_bytes());
            p[13..17].copy_from_slice(&word.to_be_bytes());
            p[22..26].copy_from_slice(&kp.to_be_bytes());
        } else {
            p = vec![0u8; 21];
            p[0] = 2;
            p[3..5].copy_from_slice(&frame.to_be_bytes());
            p[5..9].copy_from_slice(&word.to_be_bytes());
            p[14..18].copy_from_slice(&kp.to_be_bytes());
        }
        p.extend(payload);
        p
    }

    pub fn audio(&mut self, frame: u16, units: &[&[u8]], src: u8, red: u8, size: usize) -> Vec<u8> {
        let (kp, full) = self.next_pos();
        let mut payload: Vec<u8> = units.iter().flat_map(|u| u.iter().copied()).collect();
        self.down.ctr_xor(full + 16, &mut payload);
        let mut p = vec![0u8; 19];
        p[0] = 3;
        p[3..5].copy_from_slice(&frame.to_be_bytes());
        p[6] = (src + red) - 1;
        p[7..9].copy_from_slice(&(((size as u16) << 8) | ((red as u16) << 4) | src as u16).to_be_bytes());
        p[14..18].copy_from_slice(&kp.to_be_bytes());
        p.extend(payload);
        p
    }

    pub fn data(&mut self, class: u32, payload: Vec<u8>) -> (u32, Vec<u8>) {
        let s = self.seq;
        self.seq += 1;
        (s, Chunk::data(0xC0FFEE, s, class, payload).encode())
    }
    pub fn message(&mut self, m: &TakionMessage) -> (u32, Vec<u8>) {
        self.data(class::GENERAL, m.to_chunk_payload())
    }

    /// GMAC of a control or congestion packet: gmac and keyPos fields are zero in the input.
    pub fn gmac_ok_zeroed(&mut self, raw: &[u8], gmac_off: usize, kp_off: usize) -> bool {
        let kp = u32::from_be_bytes(raw[kp_off..kp_off + 4].try_into().unwrap());
        let mut z = raw.to_vec();
        z[gmac_off..gmac_off + 4].fill(0);
        z[kp_off..kp_off + 4].fill(0);
        self.up.gmac(kp, &z) == raw[gmac_off..gmac_off + 4]
    }
    pub fn control_ok(&mut self, raw: &[u8]) -> bool {
        self.gmac_ok_zeroed(raw, 5, 9)
    }
}

pub fn chunk_of(raw: &[u8]) -> Chunk {
    Chunk::decode(raw).unwrap()
}
pub fn msg_of(c: &Chunk) -> TakionMessage {
    TakionMessage::parse(&c.payload).unwrap()
}

/// k source units for `data` plus m parity (frame content helper shared with the assembler tests).
pub fn encode_frame(data: &[u8], k: usize, m: usize) -> Vec<Vec<u8>> {
    let per = data.len().div_ceil(k);
    let block = per + 2;
    let mut src = vec![];
    let mut real = vec![];
    for i in 0..k {
        let chunk = &data[(i * per).min(data.len())..((i + 1) * per).min(data.len())];
        let mut u = ((block - chunk.len() - 2) as u16).to_be_bytes().to_vec();
        u.extend(chunk);
        real.push(u.len());
        u.resize(block, 0);
        src.push(u);
    }
    let parity = openps_fec::encode(&src, m).unwrap();
    let mut wire: Vec<Vec<u8>> = src.iter().zip(&real).map(|(u, r)| u[..*r].to_vec()).collect();
    wire.extend(parity);
    wire
}

pub fn frame_data(n: usize, seed: u8) -> Vec<u8> {
    let mut v = vec![0, 0, 0, 1];
    v.extend((0..n).map(|i| (i as u8).wrapping_mul(13).wrapping_add(seed)));
    v
}

pub fn decrypt_feedback(c: &mut StreamConsole, raw: &[u8], t20: bool) -> (u8, u16, Vec<u8>) {
    let (hdr, gmac_off, kp_off, seq_off) = if t20 { (20, 16, 12, 9) } else { (12, 8, 4, 1) };
    let kp = u32::from_be_bytes(raw[kp_off..kp_off + 4].try_into().unwrap());
    let mut z = raw.to_vec();
    z[gmac_off..gmac_off + 4].fill(0); // keyPos stays in the GMAC input for feedback packets
    assert_eq!(c.up.gmac(kp, &z), raw[gmac_off..gmac_off + 4], "feedback GMAC");
    let mut payload = raw[hdr..].to_vec();
    c.up.ctr_xor(kp as u64 + 16, &mut payload);
    (raw[0] & 0x0F, u16::from_be_bytes([raw[seq_off], raw[seq_off + 1]]), payload)
}

impl StreamConsole {
    /// Build from already-negotiated contexts: `down` encrypts server->client traffic (tag 3), `up` verifies and
    /// decrypts client->server traffic (tag 2). `seq` is the next DATA sequence the console will use.
    pub fn from_contexts(down: Context, up: Context, seq: u32) -> Self {
        Self { down, up, seq, pos: 0 }
    }
}

impl Default for StreamConsole {
    fn default() -> Self {
        Self::new()
    }
}
