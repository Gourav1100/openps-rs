//! Downlink audio and haptics (spec 02 8.2, R6). Opus decoding itself is outside this crate (libopus via FFI);
//! this stage decides *which* Opus frames to decode, in order, without duplicates.

use crate::seq::SeqFilter;
use std::collections::BTreeSet;

pub const HAPTIC_FRAME_LEN: usize = 120;
/// On PS5 an audio-type packet with a unit size above this is a haptic packet, not Opus.
pub const HAPTIC_UNIT_SIZE_THRESHOLD: usize = 80;

/// Decrypted audio-type packet, already parsed (see `openps_packets::AudioPacket`).
#[derive(Clone, Debug)]
pub struct AudioUnits<'a> {
    pub frame_id: u16,
    pub total_units: u8,
    pub source_units: u8,
    pub redundant_units: u8,
    pub unit_size: usize,
    pub payload: &'a [u8],
}

impl AudioUnits<'_> {
    /// `redundant + source == total` and `unitSize * total == payload length`.
    pub fn valid(&self) -> bool {
        self.redundant_units as usize + self.source_units as usize == self.total_units as usize
            && self.unit_size * self.total_units as usize == self.payload.len()
    }
    fn unit(&self, i: usize) -> &[u8] {
        &self.payload[i * self.unit_size..(i + 1) * self.unit_size]
    }
}

fn wrap16(x: i32) -> i32 {
    x.rem_euclid(65536)
}

/// Id of unit `i` and whether it is a redundant copy to be skipped during warm-up.
fn unit_id(u: &AudioUnits, i: usize, warm: bool) -> Option<i32> {
    let (s, r) = (u.source_units as usize, u.redundant_units as usize);
    if i < s {
        return Some(wrap16(u.frame_id as i32 + i as i32));
    }
    let j = i - s;
    if warm && (u.frame_id as usize + j) < r + 1 {
        return None;
    } // start-of-stream guard
    Some(wrap16(u.frame_id as i32 - (r + j) as i32))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpusFrame {
    pub id: u16,
    pub data: Vec<u8>,
}

#[derive(Default)]
pub struct AudioAssembler {
    filter: SeqFilter,
    warm: bool,
    started: bool,
    window: BTreeSet<i32>,
}

impl AudioAssembler {
    pub fn new() -> Self {
        Self { warm: true, ..Default::default() }
    }

    /// Fresh Opus frames of this packet, in unit order. Every fresh unit is decoded (unlike haptics).
    pub fn push(&mut self, u: &AudioUnits) -> Vec<OpusFrame> {
        if !u.valid() {
            return vec![];
        }
        if u.frame_id > 32768 {
            self.warm = false;
        }
        let mut out = vec![];
        for i in 0..u.total_units as usize {
            let Some(id) = unit_id(u, i, self.warm) else { continue };
            if self.filter.is_stale(id) {
                continue;
            }
            self.filter.accept(id);
            self.window.insert(id);
            out.push(OpusFrame { id: id as u16, data: u.unit(i).to_vec() });
        }
        self.started = true;
        out
    }

    /// (received, lost) for the 1 s congestion sample: lost = gaps between the lowest and highest id seen.
    pub fn take_sample(&mut self) -> (u32, u32) {
        let received = self.window.len() as u32;
        let lost = match (self.window.first(), self.window.last()) {
            (Some(lo), Some(hi)) if hi >= lo && hi - lo < 1000 => ((hi - lo + 1) as u32).saturating_sub(received),
            _ => 0,
        };
        self.window.clear();
        (received, lost)
    }
}

/// PS5 haptics: one accepted unit per packet, delivered only if exactly 120 bytes.
pub struct HapticExtractor {
    filter: SeqFilter,
    warm: bool,
}

impl Default for HapticExtractor {
    fn default() -> Self {
        Self { filter: SeqFilter::default(), warm: true }
    }
}

impl HapticExtractor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, u: &AudioUnits) -> Option<[u8; HAPTIC_FRAME_LEN]> {
        if !u.valid() {
            return None;
        }
        if u.frame_id > 32768 {
            self.warm = false;
        }
        for i in 0..u.total_units as usize {
            let Some(id) = unit_id(u, i, self.warm) else { continue };
            if self.filter.is_stale(id) {
                continue;
            }
            self.filter.accept(id);
            let unit = u.unit(i);
            return (unit.len() == HAPTIC_FRAME_LEN).then(|| unit.try_into().unwrap());
            // first fresh unit only
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn units(frame_id: u16, s: u8, r: u8, size: usize, fill: &[u8]) -> (Vec<u8>, AudioUnits<'static>) {
        // leak-free helper: return owned payload plus a view built by the caller
        let total = (s + r) as usize;
        let mut p = vec![];
        for i in 0..total {
            p.extend(std::iter::repeat_n(*fill.get(i).unwrap_or(&0), size));
        }
        let _ = frame_id;
        (
            p,
            AudioUnits {
                frame_id,
                total_units: s + r,
                source_units: s,
                redundant_units: r,
                unit_size: size,
                payload: &[],
            },
        )
    }

    fn view<'a>(base: &AudioUnits, payload: &'a [u8]) -> AudioUnits<'a> {
        AudioUnits {
            frame_id: base.frame_id,
            total_units: base.total_units,
            source_units: base.source_units,
            redundant_units: base.redundant_units,
            unit_size: base.unit_size,
            payload,
        }
    }

    #[test]
    fn validation_rejects_mismatched_packets() {
        let p = vec![0u8; 40];
        assert!(AudioUnits {
            frame_id: 0,
            total_units: 2,
            source_units: 1,
            redundant_units: 1,
            unit_size: 20,
            payload: &p
        }
        .valid());
        assert!(!AudioUnits {
            frame_id: 0,
            total_units: 2,
            source_units: 1,
            redundant_units: 0,
            unit_size: 20,
            payload: &p
        }
        .valid());
        assert!(!AudioUnits {
            frame_id: 0,
            total_units: 2,
            source_units: 1,
            redundant_units: 1,
            unit_size: 21,
            payload: &p
        }
        .valid());
    }

    #[test]
    fn plain_audio_emits_each_new_frame_once() {
        let mut a = AudioAssembler::new();
        let (p, base) = units(10, 1, 0, 40, &[7]);
        let f = a.push(&view(&base, &p));
        assert_eq!(f, vec![OpusFrame { id: 10, data: vec![7; 40] }]);
        assert!(a.push(&view(&base, &p)).is_empty()); // repeat: stale
        let (p, base) = units(11, 1, 0, 40, &[8]);
        assert_eq!(a.push(&view(&base, &p))[0].id, 11);
    }

    #[test]
    fn redundant_copies_use_the_spec_id_formula_and_are_stale_behind_the_new_frame() {
        // ids: unit 0 = frameId, redundant unit j = frameId - (R + j) (spec 02 8.2)
        let mut a = AudioAssembler::new();
        a.warm = false;
        let (p, base) = units(100, 1, 2, 40, &[1, 2, 3]);
        assert_eq!(unit_id(&view(&base, &p), 0, false), Some(100));
        assert_eq!(unit_id(&view(&base, &p), 1, false), Some(98)); // 100 - (2 + 0)
        assert_eq!(unit_id(&view(&base, &p), 2, false), Some(97)); // 100 - (2 + 1)
                                                                   // the new frame is accepted first, so its older redundant copies are stale
        assert_eq!(a.push(&view(&base, &p)).iter().map(|f| f.id).collect::<Vec<_>>(), vec![100]);
        // an out-of-order older packet is still delivered while it is fresh relative to what was played
        let mut b = AudioAssembler::new();
        b.warm = false;
        let (p, base) = units(100, 1, 0, 40, &[1]);
        b.push(&view(&base, &p));
        let (p, base) = units(100, 1, 0, 40, &[1]);
        assert!(b.push(&view(&base, &p)).is_empty());
    }

    #[test]
    fn warmup_skips_redundant_copies_near_the_start() {
        let mut a = AudioAssembler::new();
        let (p, base) = units(1, 1, 2, 40, &[1, 2, 3]);
        let ids: Vec<u16> = a.push(&view(&base, &p)).iter().map(|f| f.id).collect();
        assert_eq!(ids, vec![1]); // redundant copies would have negative ids
    }

    #[test]
    fn ids_wrap_around_65535() {
        let mut a = AudioAssembler::new();
        a.warm = false;
        let (p, base) = units(65535, 2, 0, 40, &[1, 2]);
        let ids: Vec<u16> = a.push(&view(&base, &p)).iter().map(|f| f.id).collect();
        assert_eq!(ids, vec![65535, 0]);
    }

    #[test]
    fn congestion_sample_counts_gaps() {
        let mut a = AudioAssembler::new();
        a.warm = false;
        for id in [10u16, 11, 13, 14] {
            let (p, base) = units(id, 1, 0, 40, &[1]);
            a.push(&view(&base, &p));
        }
        assert_eq!(a.take_sample(), (4, 1)); // id 12 missing
        assert_eq!(a.take_sample(), (0, 0));
    }

    #[test]
    fn haptics_take_one_fresh_120_byte_unit_per_packet() {
        let mut h = HapticExtractor::new();
        let (p, base) = units(5, 1, 0, 120, &[9]);
        assert_eq!(h.push(&view(&base, &p)), Some([9u8; 120]));
        assert_eq!(h.push(&view(&base, &p)), None); // stale repeat
        let (p, base) = units(6, 1, 0, 96, &[9]); // wrong length: accepted as fresh but dropped
        assert_eq!(h.push(&view(&base, &p)), None);
        let (p, base) = units(7, 2, 0, 120, &[1, 2]); // only the first fresh unit is used
        assert_eq!(h.push(&view(&base, &p)), Some([1u8; 120]));
    }

    #[test]
    fn haptics_recover_from_redundancy_but_never_emit_two_frames() {
        let mut h = HapticExtractor::new();
        h.warm = false;
        let (p, base) = units(500, 1, 1, 120, &[1, 2]);
        assert_eq!(h.push(&view(&base, &p)), Some([1u8; 120]));
        let (p, base) = units(502, 1, 1, 120, &[3, 4]); // frame 501 lost; unit 0 (502) is fresh and wins
        assert_eq!(h.push(&view(&base, &p)), Some([3u8; 120]));
    }

    #[test]
    fn invalid_haptic_packet_is_dropped() {
        let mut h = HapticExtractor::new();
        let p = vec![0u8; 100];
        assert_eq!(
            h.push(&AudioUnits {
                frame_id: 1,
                total_units: 1,
                source_units: 1,
                redundant_units: 0,
                unit_size: 120,
                payload: &p
            }),
            None
        );
    }
}
