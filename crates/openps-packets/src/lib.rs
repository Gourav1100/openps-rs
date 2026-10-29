//! Big-endian Takion packet parse/build. "T20" = Takion version >= 20, else "legacy".

#[derive(Debug, PartialEq, Eq)]
pub enum PacketError {
    TooShort,
    BadUnits,
}

/// Takion packet type = byte0 low nibble.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PacketType {
    Control,
    FeedbackHistory,
    Video,
    Audio,
    Congestion,
    Feedback,
    Unknown(u8),
}

pub fn packet_type(b0: u8) -> PacketType {
    match b0 & 0x0F {
        0 => PacketType::Control,
        1 => PacketType::FeedbackHistory,
        2 => PacketType::Video,
        3 => PacketType::Audio,
        5 => PacketType::Congestion,
        6 => PacketType::Feedback,
        n => PacketType::Unknown(n),
    }
}

fn u16be(b: &[u8], o: usize) -> u16 {
    u16::from_be_bytes([b[o], b[o + 1]])
}
fn u32be(b: &[u8], o: usize) -> u32 {
    u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

/// Byte offsets of (gmac, keyPos) per packet type (spec 6.5).
pub fn gmac_key_pos_offsets(t: PacketType, t20: bool) -> Option<(usize, usize)> {
    Some(match (t, t20) {
        (PacketType::Control, _) => (5, 9),
        (PacketType::FeedbackHistory | PacketType::Feedback, false) => (8, 4),
        (PacketType::FeedbackHistory | PacketType::Feedback, true) => (16, 12),
        (PacketType::Video, _) => (10, 14),
        (PacketType::Audio, false) => (10, 14),
        (PacketType::Audio, true) => (18, 22),
        (PacketType::Congestion, _) => (7, 11),
        _ => return None,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoPacket<'a> {
    pub packet_id: u32,
    pub frame_id: u16,
    pub unit_index: u16,
    pub total_units: u16,
    pub fec_units: u16,
    pub codec: u8,
    pub key_pos: u32,
    pub adaptive_stream_index: u8,
    /// Encrypted unit payload (AES-CTR at keyPos + 2^32*wrap + 16 with the remote context).
    pub payload: &'a [u8],
    /// Byte index where the payload begins (for in-place decrypt on the original buffer).
    pub payload_offset: usize,
}

impl<'a> VideoPacket<'a> {
    pub fn parse(b: &'a [u8], t20: bool) -> Result<Self, PacketError> {
        let min = if t20 { 29 } else { 21 };
        if b.len() < min {
            return Err(PacketError::TooShort);
        }
        let (packet_id, frame_id, word, codec, key_pos, adaptive) = if t20 {
            (u32be(b, 5), u16be(b, 11), u32be(b, 13), b[17], u32be(b, 22), (b[28] >> 5) & 7)
        } else {
            (u16be(b, 1) as u32, u16be(b, 3), u32be(b, 5), b[9], u32be(b, 14), b[20] >> 5)
        };
        let mut off = min;
        if b[0] & 0x10 != 0 {
            off += 3;
        }
        if b.len() < off {
            return Err(PacketError::TooShort);
        }
        Ok(Self {
            packet_id,
            frame_id,
            unit_index: (word >> 21) as u16,
            total_units: ((word >> 10) & 0x7FF) as u16 + 1,
            fec_units: (word & 0x3FF) as u16,
            codec,
            key_pos,
            adaptive_stream_index: adaptive,
            payload: &b[off..],
            payload_offset: off,
        })
    }
    pub fn source_units(&self) -> u16 {
        self.total_units - self.fec_units.min(self.total_units)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioPacket<'a> {
    pub packet_id: u32,
    pub frame_id: u16,
    pub unit_index: u8,
    pub total_units: u8,
    pub unit_size: u8,
    pub redundant_units: u8,
    pub source_units: u8,
    pub codec: u8,
    pub key_pos: u32,
    pub payload: &'a [u8],
    pub payload_offset: usize,
}

impl<'a> AudioPacket<'a> {
    pub fn parse(b: &'a [u8], t20: bool) -> Result<Self, PacketError> {
        let hdr = if t20 { 28 } else { 19 };
        if b.len() < hdr {
            return Err(PacketError::TooShort);
        }
        if t20 {
            let total = b[14].wrapping_add(1);
            if total == 0 {
                return Err(PacketError::BadUnits);
            }
            let payload = &b[28..];
            let unit_size = (payload.len() / total as usize) as u8;
            let src = total.min(1);
            return Ok(Self {
                packet_id: u32be(b, 5),
                frame_id: u16be(b, 11),
                unit_index: b[13],
                total_units: total,
                unit_size,
                redundant_units: total - src,
                source_units: src,
                codec: b[17],
                key_pos: u32be(b, 22),
                payload,
                payload_offset: 28,
            });
        }
        let total = b[6].wrapping_add(1);
        let fec = u16be(b, 7);
        let unit_size = (fec >> 8) as u8;
        let need = unit_size as usize * total as usize;
        if total == 0 || b.len() < need {
            return Err(PacketError::BadUnits);
        }
        let off = b.len() - need; // leading extra bytes are dropped
        Ok(Self {
            packet_id: u16be(b, 1) as u32,
            frame_id: u16be(b, 3),
            unit_index: b[5],
            total_units: total,
            unit_size,
            redundant_units: ((fec >> 4) & 0xF) as u8,
            source_units: (fec & 0xF) as u8,
            codec: b[9],
            key_pos: u32be(b, 14),
            payload: &b[off..],
            payload_offset: off,
        })
    }
    /// PS5 haptic packets ride the audio stream with unitSize > 80.
    pub fn is_haptic(&self, ps5_haptics_enabled: bool) -> bool {
        ps5_haptics_enabled && self.unit_size > 80
    }
}

/// Congestion packet (client -> console, 15 B). GMAC@7 and keyPos@11 are filled by the caller (crypto).
pub fn build_congestion(units_received: u16, units_lost_estimate: u16) -> [u8; 15] {
    let mut p = [0u8; 15];
    p[0] = 0x05;
    p[3..5].copy_from_slice(&units_received.to_be_bytes());
    p[5..7].copy_from_slice(&units_lost_estimate.to_be_bytes());
    p
}

/// Feedback / feedback-history header followed by `payload`. GMAC and keyPos fields are filled by the caller.
/// `kind` is 6 (feedback) or 1 (feedback history).
pub fn build_feedback(kind: u8, seq: u16, t20: bool, payload: &[u8]) -> Vec<u8> {
    let hdr = if t20 { 20 } else { 12 };
    let mut p = vec![0u8; hdr + payload.len()];
    p[0] = kind & 0x0F;
    let seq_off = if t20 { 9 } else { 1 };
    p[seq_off..seq_off + 2].copy_from_slice(&seq.to_be_bytes());
    p[hdr..].copy_from_slice(payload);
    p
}

/// Feedback sequence generator: first packet 1537, counter starts at 256 so it bumps after the very first
/// packet, then every 256 packets; wraps > 65534 -> 0 (spec R5).
#[derive(Debug)]
pub struct FeedbackSeq {
    seq: u32,
    counter: u32,
}
impl Default for FeedbackSeq {
    fn default() -> Self {
        Self { seq: 1537, counter: 256 }
    }
}
impl FeedbackSeq {
    pub fn next_seq(&mut self) -> u16 {
        let out = self.seq as u16;
        self.counter += 1;
        if self.counter >= 256 {
            self.counter = 0;
            self.seq += 1;
            if self.seq > 65534 {
                self.seq = 0;
            }
        }
        out
    }
}

/// Feedback-history sequence: starts 0, +1 per packet, wraps > 65534 -> 0.
#[derive(Debug, Default)]
pub struct HistorySeq(u32);
impl HistorySeq {
    pub fn next_seq(&mut self) -> u16 {
        let o = self.0 as u16;
        self.0 += 1;
        if self.0 > 65534 {
            self.0 = 0;
        }
        o
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_legacy_parse() {
        let mut b = vec![0u8; 21 + 4];
        b[0] = 0x02;
        b[1..3].copy_from_slice(&7u16.to_be_bytes());
        b[3..5].copy_from_slice(&300u16.to_be_bytes());
        let word: u32 = (5 << 21) | (9 << 10) | 3; // unit 5, total 10, fec 3
        b[5..9].copy_from_slice(&word.to_be_bytes());
        b[9] = 2;
        b[14..18].copy_from_slice(&0xDEADBEF0u32.to_be_bytes());
        b[20] = 0b0110_0000;
        b[21..].copy_from_slice(&[1, 2, 3, 4]);
        let v = VideoPacket::parse(&b, false).unwrap();
        assert_eq!((v.packet_id, v.frame_id, v.unit_index, v.total_units, v.fec_units), (7, 300, 5, 10, 3));
        assert_eq!((v.codec, v.key_pos, v.adaptive_stream_index), (2, 0xDEADBEF0, 3));
        assert_eq!(v.payload, &[1, 2, 3, 4]);
        assert_eq!(v.source_units(), 7);
        assert_eq!(packet_type(b[0]), PacketType::Video);
    }

    #[test]
    fn video_t20_with_skipped_bytes() {
        let mut b = vec![0u8; 29 + 3 + 2];
        b[0] = 0x12; // bit4 set => 3 skipped bytes
        b[5..9].copy_from_slice(&0x01020304u32.to_be_bytes());
        b[11..13].copy_from_slice(&42u16.to_be_bytes());
        b[22..26].copy_from_slice(&64u32.to_be_bytes());
        b[28] = 0b1010_0000;
        let v = VideoPacket::parse(&b, true).unwrap();
        assert_eq!((v.packet_id, v.frame_id, v.key_pos, v.adaptive_stream_index), (0x01020304, 42, 64, 5));
        assert_eq!(v.payload_offset, 32);
        assert_eq!(v.payload.len(), 2);
    }

    #[test]
    fn short_packets_rejected() {
        assert_eq!(VideoPacket::parse(&[2; 10], false), Err(PacketError::TooShort));
        assert_eq!(AudioPacket::parse(&[3; 10], false), Err(PacketError::TooShort));
    }

    #[test]
    fn audio_legacy_takes_trailing_units() {
        let mut b = vec![0u8; 19 + 2 + 3 * 4]; // 2 stray bytes then 3 units of 4 B
        b[0] = 0x03;
        b[3..5].copy_from_slice(&9u16.to_be_bytes());
        b[5] = 1;
        b[6] = 2; // total = 3
        b[7..9].copy_from_slice(&((4u16 << 8) | (1 << 4) | 2).to_be_bytes());
        b[14..18].copy_from_slice(&160u32.to_be_bytes());
        let n = b.len();
        b[n - 12..].copy_from_slice(&[9; 12]);
        let a = AudioPacket::parse(&b, false).unwrap();
        assert_eq!((a.frame_id, a.unit_index, a.total_units, a.unit_size), (9, 1, 3, 4));
        assert_eq!((a.redundant_units, a.source_units, a.key_pos), (1, 2, 160));
        assert_eq!(a.payload, &[9; 12]);
        assert!(!a.is_haptic(true));
    }

    #[test]
    fn audio_t20_synthesises_fec_word() {
        let mut b = vec![0u8; 28 + 120];
        b[0] = 0x03;
        b[14] = 0; // total = 1
        let a = AudioPacket::parse(&b, true).unwrap();
        assert_eq!((a.total_units, a.unit_size, a.source_units, a.redundant_units), (1, 120, 1, 0));
        assert!(a.is_haptic(true) && !a.is_haptic(false));
    }

    #[test]
    fn congestion_layout() {
        let p = build_congestion(0x0102, 0x0304);
        assert_eq!(&p[..7], &[5, 0, 0, 1, 2, 3, 4]);
        assert_eq!(p.len(), 15);
    }

    #[test]
    fn feedback_layout_and_offsets() {
        let l = build_feedback(6, 0x0601, false, &[0xAA; 3]);
        assert_eq!(l.len(), 15);
        assert_eq!(&l[..3], &[6, 6, 1]);
        let n = build_feedback(1, 2, true, &[]);
        assert_eq!((n.len(), n[9], n[10]), (20, 0, 2));
        assert_eq!(gmac_key_pos_offsets(PacketType::Feedback, false), Some((8, 4)));
        assert_eq!(gmac_key_pos_offsets(PacketType::Audio, true), Some((18, 22)));
        assert_eq!(gmac_key_pos_offsets(PacketType::Unknown(9), true), None);
    }

    #[test]
    fn feedback_sequence_matches_spec() {
        let mut s = FeedbackSeq::default();
        assert_eq!(s.next_seq(), 1537);
        for _ in 0..256 {
            assert_eq!(s.next_seq(), 1538);
        } // packets #2..#257
        assert_eq!(s.next_seq(), 1539);
        let mut h = HistorySeq::default();
        assert_eq!((h.next_seq(), h.next_seq()), (0, 1));
    }
}
