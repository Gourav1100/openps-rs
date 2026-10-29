//! Tester A1: malformed/truncated/boundary tests for packet parse/build and sequence generators.
use openps_packets::*;

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
}

#[test]
fn random_bytes_never_panic_and_payload_is_inside_the_buffer() {
    let mut r = Rng(0xDEAD_BEEF);
    for _ in 0..200_000 {
        let n = r.below(120);
        let mut b: Vec<u8> = (0..n).map(|_| r.next() as u8).collect();
        if n > 0 {
            b[0] = (b[0] & 0xF0) | if r.below(2) == 0 { 2 } else { 3 };
        }
        for t20 in [false, true] {
            if let Ok(v) = VideoPacket::parse(&b, t20) {
                assert_eq!(v.payload_offset + v.payload.len(), b.len());
                assert!(v.payload_offset >= if t20 { 29 } else { 21 });
                assert!(v.total_units >= 1);
            }
            if let Ok(a) = AudioPacket::parse(&b, t20) {
                assert_eq!(a.payload_offset + a.payload.len(), b.len());
            }
        }
    }
}

#[test]
fn every_truncation_of_a_valid_packet_is_handled() {
    let mut v = [0u8; 40];
    v[0] = 0x12;
    for t20 in [false, true] {
        for n in 0..v.len() {
            let _ = VideoPacket::parse(&v[..n], t20);
            let _ = AudioPacket::parse(&v[..n], t20);
        }
    }
    // exact minimum lengths
    assert!(VideoPacket::parse(&[2; 21], false).is_ok());
    assert!(VideoPacket::parse(&[2; 20], false).is_err());
    assert!(VideoPacket::parse(&[2; 29], true).is_ok());
    assert!(VideoPacket::parse(&[2; 28], true).is_err());
    // skip-3 flag needs 3 extra bytes
    assert!(VideoPacket::parse(&[0x12; 23], false).is_err());
    assert!(VideoPacket::parse(&[0x12; 24], false).is_ok());
}

#[test]
fn video_field_extremes() {
    let mut b = vec![0u8; 29];
    b[0] = 2;
    b[13..17].copy_from_slice(&0xFFFF_FFFFu32.to_be_bytes());
    let v = VideoPacket::parse(&b, true).unwrap();
    assert_eq!((v.unit_index, v.total_units, v.fec_units), (2047, 2048, 1023));
    assert_eq!(v.source_units(), 1025);
    // fec > total: source_units saturates to 0 instead of underflowing
    b[13..17].copy_from_slice(&1023u32.to_be_bytes());
    let v = VideoPacket::parse(&b, true).unwrap();
    assert_eq!((v.total_units, v.fec_units, v.source_units()), (1, 1023, 0));
    // unit_index may exceed total_units: parser does not validate (consumer must)
    b[13..17].copy_from_slice(&((2000u32 << 21) | (3 << 10)).to_be_bytes());
    let v = VideoPacket::parse(&b, true).unwrap();
    assert!(v.unit_index >= v.total_units);
}

#[test]
fn audio_total_units_255_is_rejected() {
    let mut b = vec![0u8; 28 + 10];
    b[0] = 3;
    b[14] = 255;
    assert_eq!(AudioPacket::parse(&b, true), Err(PacketError::BadUnits));
    let mut l = vec![0u8; 19 + 10];
    l[6] = 255;
    assert_eq!(AudioPacket::parse(&l, false), Err(PacketError::BadUnits));
}

#[test]
fn audio_legacy_zero_unit_size_gives_empty_payload() {
    let mut b = vec![0u8; 19];
    b[0] = 3;
    let a = AudioPacket::parse(&b, false).unwrap();
    assert_eq!((a.unit_size, a.payload.len(), a.payload_offset), (0, 0, 19));
}

#[test]
fn audio_legacy_payload_must_not_overlap_header() {
    // header claims 1 unit of 4 bytes but the datagram is only the 19-byte header
    let mut b = vec![0u8; 19];
    b[0] = 3;
    b[6] = 0; // total = 1
    b[7..9].copy_from_slice(&((4u16 << 8) | 1).to_be_bytes());
    match AudioPacket::parse(&b, false) {
        Err(_) => {}
        Ok(a) => assert!(a.payload_offset >= 19, "payload starts at {} inside the header", a.payload_offset),
    }
}

#[test]
fn audio_legacy_overlap_current_behaviour() {
    let mut b = vec![0u8; 19];
    b[0] = 3;
    b[7..9].copy_from_slice(&((4u16 << 8) | 1).to_be_bytes());
    assert_eq!(AudioPacket::parse(&b, false), Err(PacketError::BadUnits)); // T01-03 fixed
    b.extend_from_slice(&[0; 4]);
    assert_eq!(AudioPacket::parse(&b, false).unwrap().payload_offset, 19);
}

#[test]
fn audio_t20_unit_size_does_not_truncate() {
    let mut b = vec![0u8; 28 + 300];
    b[0] = 3;
    b[14] = 0; // one unit of 300 bytes
    match AudioPacket::parse(&b, true) {
        Err(_) => {}
        Ok(a) => assert!(a.unit_size == 255 || a.unit_size as usize * a.total_units as usize == a.payload.len()),
    }
}

#[test]
fn audio_t20_unit_size_truncation_current_behaviour() {
    let mut b = vec![0u8; 28 + 300];
    b[0] = 3;
    assert_eq!(AudioPacket::parse(&b, true), Err(PacketError::BadUnits)); // T01-04 fixed
    let mut ok = vec![0u8; 28 + 255];
    ok[0] = 3;
    assert_eq!(AudioPacket::parse(&ok, true).unwrap().unit_size, 255);
}

#[test]
#[ignore = "BUG T01-05 (not fixed): needs a T20 capture to decide whether table or parser is wrong; table is unused at runtime"]
fn video_t20_offsets_agree_with_parser() {
    let mut b = vec![0u8; 29];
    b[0] = 2;
    b[22..26].copy_from_slice(&0xA1B2C3D4u32.to_be_bytes());
    let v = VideoPacket::parse(&b, true).unwrap();
    let (_, kp_off) = gmac_key_pos_offsets(PacketType::Video, true).unwrap();
    assert_eq!(u32::from_be_bytes(b[kp_off..kp_off + 4].try_into().unwrap()), v.key_pos);
}

#[test]
fn offsets_are_inside_the_headers_and_do_not_overlap() {
    for t20 in [false, true] {
        for (t, hdr) in [
            (PacketType::Control, 25usize),
            (PacketType::FeedbackHistory, if t20 { 20 } else { 12 }),
            (PacketType::Feedback, if t20 { 20 } else { 12 }),
            (PacketType::Audio, if t20 { 28 } else { 19 }),
            (PacketType::Congestion, 15),
        ] {
            let (g, k) = gmac_key_pos_offsets(t, t20).unwrap();
            assert!(g + 4 <= hdr && k + 4 <= hdr, "{t:?} t20={t20}");
            assert!(g + 4 <= k || k + 4 <= g, "{t:?} t20={t20} fields overlap");
        }
    }
    assert_eq!(gmac_key_pos_offsets(PacketType::Unknown(4), false), None);
}

#[test]
fn builders_have_expected_sizes_and_never_panic() {
    for kind in 0..=255u8 {
        for t20 in [false, true] {
            for plen in [0usize, 1, 1400] {
                let p = build_feedback(kind, 0xFFFF, t20, &vec![0xEE; plen]);
                assert_eq!(p.len(), if t20 { 20 } else { 12 } + plen);
                assert_eq!(p[0], kind & 0x0F);
            }
        }
    }
    let c = build_congestion(u16::MAX, u16::MAX);
    assert_eq!(&c[3..7], &[0xFF; 4]);
    assert_eq!(&c[7..], &[0u8; 8]);
}

#[test]
fn packet_type_nibble_mapping() {
    for b in 0..=255u8 {
        let t = packet_type(b);
        assert_eq!(t, packet_type(b & 0x0F));
    }
    assert_eq!(packet_type(0xF2), PacketType::Video);
    assert_eq!(packet_type(0x04), PacketType::Unknown(4));
}

#[test]
fn feedback_seq_wraps_and_never_emits_65535() {
    let mut s = FeedbackSeq::default();
    let mut seen_wrap = false;
    let mut prev = s.next_seq();
    assert_eq!(prev, 1537);
    for _ in 0..(256u32 * 66_000) {
        let n = s.next_seq();
        assert_ne!(n, 65535);
        if n < prev {
            assert_eq!(n, 0, "wrap goes 65534 -> 0");
            assert_eq!(prev, 65534);
            seen_wrap = true;
        } else {
            assert!(n - prev <= 1);
        }
        prev = n;
    }
    assert!(seen_wrap);
}

#[test]
fn history_seq_wraps_at_65534() {
    let mut h = HistorySeq::default();
    for expect in 0..=65534u32 {
        assert_eq!(h.next_seq() as u32, expect);
    }
    assert_eq!(h.next_seq(), 0);
    assert_eq!(h.next_seq(), 1);
}
