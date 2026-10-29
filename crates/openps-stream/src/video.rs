//! Video frame assembly and delivery (spec 02 R2/R3; port of `gu`/`gw` + `VideoFrameData`).
//!
//! Units arrive decrypted. A frame is `k` source units plus `m` Cauchy-RS parity units; when source units
//! are missing the frame is recovered with `openps-fec`. Frames are delivered in order; when an out-of-order
//! frame completes and the in-flight window is exhausted, the gap is skipped and reported (CorruptFrame).

use crate::seq::{is_older, next_id, reached};
use std::collections::{BTreeMap, VecDeque};

pub const MAX_BLOCK: usize = 65000;
pub const MAX_UNITS: usize = 512;
/// Hard cap on frames being assembled; a flood of distinct frame ids must not grow memory without bound.
pub const MAX_PENDING_FRAMES: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Late,
    Invalid,
    Pending,
    DeliveredInOrder,
    ReadyButBlocked,
    DeliveredWithGap,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unit<'a> {
    pub frame_id: u16,
    pub unit_index: u16,
    pub total_units: u16,
    pub fec_units: u16,
    pub adaptive_index: u8,
    pub payload: &'a [u8],
}

/// A reassembled access unit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub id: u16,
    pub adaptive_index: u8,
    /// Concatenated source units without their 2-byte pad headers (Annex-B H.264/HEVC).
    pub data: Vec<u8>,
    /// `00 00 00 01` at the start of the first unit's payload.
    pub starts_with_start_code: bool,
}

struct Pending {
    id: i32,
    k: usize,
    m: usize,
    block: usize,
    adaptive: u8,
    units: Vec<Option<Vec<u8>>>, // k source then m parity, each `block` bytes (source zero-padded)
    real: Vec<usize>,            // received length of each source unit
    src: usize,
    par: usize,
}

impl Pending {
    fn ready(&self) -> bool {
        self.src == self.k || self.src + self.par >= self.k
    }
    fn received(&self) -> usize {
        self.src + self.par
    }

    fn add(&mut self, index: usize, payload: &[u8]) -> bool {
        if index > self.k + self.m
            || payload.len() > self.block
            || index >= self.units.len()
            || self.units[index].is_some()
        {
            return false;
        }
        if index >= self.k && index - self.k >= self.m {
            return false;
        }
        let mut buf = vec![0u8; self.block];
        buf[..payload.len()].copy_from_slice(payload);
        self.units[index] = Some(buf);
        if index < self.k {
            self.real[index] = payload.len();
            self.src += 1;
        } else {
            self.par += 1;
        }
        true
    }

    /// Recover missing source units if needed, then concatenate `unit[2..real)`. None if unrecoverable.
    fn finish(mut self) -> Option<Frame> {
        if self.src < self.k {
            if self.k + self.m > 256 {
                return None;
            } // w = 8 cannot code more than 256 units
            let mut blocks = self.units.clone();
            openps_fec::decode(self.k, self.m, &mut blocks).ok()?;
            for (i, slot) in blocks.iter_mut().enumerate().take(self.k) {
                if self.units[i].is_none() {
                    let b = slot.take()?;
                    let pad = u16::from_be_bytes([b[0], b[1]]) as usize;
                    if pad > self.block {
                        return None;
                    }
                    self.real[i] = self.block - pad;
                    self.units[i] = Some(b);
                }
            }
        }
        let mut data = vec![];
        for i in 0..self.k {
            let u = self.units[i].as_ref()?;
            let end = self.real[i];
            if end < 2 {
                return None;
            }
            data.extend(&u[2..end]);
        }
        let starts = self.units[0].as_ref().is_some_and(|u| u.len() >= 6 && u[2..6] == [0, 0, 0, 1]);
        Some(Frame { id: self.id as u16, adaptive_index: self.adaptive, data, starts_with_start_code: starts })
    }
}

pub struct VideoAssembler {
    window: i32,
    cursor: i32,
    last_delivered: i32,
    frames: BTreeMap<i32, Pending>,
    out: VecDeque<Frame>,
    /// (received, missed) units since the last telemetry sample
    tel_received: u32,
    tel_missed: u32,
    tel_at: f64,
}

impl VideoAssembler {
    /// `window` = `videoFrameBufferSize` (clamped to at least 2).
    pub fn new(window: usize) -> Self {
        Self {
            window: window.max(2) as i32,
            cursor: -1,
            last_delivered: -1,
            frames: BTreeMap::new(),
            out: VecDeque::new(),
            tel_received: 0,
            tel_missed: 0,
            tel_at: 0.0,
        }
    }

    /// Next expected frame id (-1 until the first delivery).
    pub fn cursor(&self) -> i32 {
        self.cursor
    }
    pub fn pop_frame(&mut self) -> Option<Frame> {
        self.out.pop_front()
    }
    pub fn reset(&mut self) {
        self.cursor = -1;
        self.last_delivered = -1;
        self.frames.clear();
        self.out.clear();
    }

    pub fn push(&mut self, u: &Unit) -> Outcome {
        let id = u.frame_id as i32;
        let late = id != self.cursor && self.cursor != -1 && is_older(self.last_delivered, id);
        if late {
            return Outcome::Late;
        }
        if u.total_units < u.fec_units {
            return Outcome::Invalid;
        }
        let (k, m) = ((u.total_units - u.fec_units) as usize, (u.fec_units as usize).max(1));
        if !self.frames.contains_key(&id) {
            let first_is_source = (u.unit_index as usize) < k;
            let block = if first_is_source {
                if u.payload.len() < 2 {
                    return Outcome::Pending;
                }
                u.payload.len() + u16::from_be_bytes([u.payload[0], u.payload[1]]) as usize
            } else {
                u.payload.len()
            };
            // block >= 2: a recovered unit must hold its 2-byte pad header
            if !(2..=MAX_BLOCK).contains(&block) || k + m > MAX_UNITS {
                return Outcome::Pending;
            }
            if self.frames.len() >= MAX_PENDING_FRAMES {
                self.evict();
            }
            self.frames.insert(
                id,
                Pending {
                    id,
                    k,
                    m,
                    block,
                    adaptive: u.adaptive_index,
                    units: vec![None; k + m],
                    real: vec![0; k],
                    src: 0,
                    par: 0,
                },
            );
        }
        let frame = self.frames.get_mut(&id).unwrap();
        if !frame.add(u.unit_index as usize, u.payload) || !frame.ready() {
            return Outcome::Pending;
        }
        self.deliver_ready(id)
    }

    fn deliver(&mut self, id: i32) {
        if let Some(p) = self.frames.remove(&id) {
            self.tel_received += p.received() as u32;
            if let Some(f) = p.finish() {
                self.out.push_back(f);
            } // unrecoverable frames are dropped but still advance the cursor
        }
        self.last_delivered = id;
        self.cursor = next_id(id);
    }

    fn deliver_ready(&mut self, id: i32) -> Outcome {
        if self.cursor == -1 || self.cursor == id {
            self.deliver(id);
            while self.frames.get(&self.cursor).is_some_and(Pending::ready) {
                let c = self.cursor;
                self.deliver(c);
            }
            return Outcome::DeliveredInOrder;
        }
        // out-of-order completion: wait until the window is exhausted, then skip the gap
        let mut limit = self.cursor + self.window;
        if limit > 65535 {
            limit -= 65536;
        }
        let mut candidate = None;
        for &fid in self.frames.keys() {
            if !reached(fid, limit) {
                continue;
            }
            let mut c = fid - (self.window - 1);
            if c < 0 {
                c = 65535;
            }
            let cand = if self.frames.get(&c).is_some_and(Pending::ready) { c } else { fid };
            if self.frames.get(&cand).is_some_and(Pending::ready) {
                candidate = Some(cand);
                break;
            }
        }
        let Some(first) = candidate else { return Outcome::ReadyButBlocked };
        self.deliver(first);
        loop {
            // pick the highest-id ready frame at or beyond the cursor, skipping intermediate ones
            let (mut pick, mut cmp) = (None, self.cursor);
            for (&fid, p) in self.frames.iter() {
                if p.ready() && reached(fid, cmp) {
                    pick = Some(fid);
                    cmp = fid;
                }
            }
            match pick {
                Some(fid) => {
                    self.discard_before(fid);
                    self.deliver(fid);
                }
                None => break,
            }
        }
        let cur = self.cursor;
        let stale: Vec<i32> = self.frames.keys().copied().filter(|f| !reached(*f, cur)).collect();
        for f in stale {
            if let Some(p) = self.frames.remove(&f) {
                self.tel_received += p.received() as u32;
                self.tel_missed += (p.k.saturating_sub(p.received())) as u32;
            }
        }
        Outcome::DeliveredWithGap
    }

    /// Make room: drop frames already behind the cursor; if that is not enough, drop everything (the window
    /// logic resynchronises on the next complete frame).
    fn evict(&mut self) {
        if self.cursor != -1 {
            let cur = self.cursor;
            self.frames.retain(|f, _| reached(*f, cur));
        }
        if self.frames.len() >= MAX_PENDING_FRAMES {
            self.frames.clear();
        }
    }

    fn discard_before(&mut self, id: i32) {
        let skipped: Vec<i32> = self.frames.keys().copied().filter(|f| *f != id && !reached(*f, id)).collect();
        for f in skipped {
            if let Some(p) = self.frames.remove(&f) {
                self.tel_received += p.received() as u32;
                self.tel_missed += p.k.saturating_sub(p.received()) as u32;
            }
        }
    }

    /// Once per >= 1 s: stored frames already older than the cursor are dropped and counted. Returns (received, missed).
    pub fn poll_telemetry(&mut self, now: f64) -> Option<(u32, u32)> {
        if now - self.tel_at < 1.0 {
            return None;
        }
        self.tel_at = now;
        if self.cursor != -1 {
            let cur = self.cursor;
            let old: Vec<i32> = self.frames.keys().copied().filter(|f| !reached(*f, cur)).collect();
            for f in old {
                let p = self.frames.remove(&f).unwrap();
                self.tel_received += p.received() as u32;
                self.tel_missed += p.k.saturating_sub(p.received()) as u32;
            }
        }
        Some((std::mem::take(&mut self.tel_received), std::mem::take(&mut self.tel_missed)))
    }

    /// The assembler clears its store when it already holds more than 15 frames before opening a new one (`gw`).
    pub fn pending_frames(&self) -> usize {
        self.frames.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build `k` source units for a frame whose content is `data`, plus `m` parity units. Returns (units, block size).
    fn encode_frame(data: &[u8], k: usize, m: usize) -> Vec<Vec<u8>> {
        let per = data.len().div_ceil(k);
        let block = per + 2;
        let mut src: Vec<Vec<u8>> = vec![];
        for i in 0..k {
            let chunk = &data[(i * per).min(data.len())..((i + 1) * per).min(data.len())];
            let real = chunk.len() + 2;
            let pad = (block - real) as u16;
            let mut u = pad.to_be_bytes().to_vec();
            u.extend(chunk);
            u.resize(block, 0);
            src.push(u);
        }
        let parity = openps_fec::encode(&src, m).unwrap();
        // on the wire a source unit has its real (unpadded) length
        let mut wire: Vec<Vec<u8>> = src
            .iter()
            .enumerate()
            .map(|(i, u)| {
                let chunk_len = data[(i * per).min(data.len())..((i + 1) * per).min(data.len())].len();
                u[..chunk_len + 2].to_vec()
            })
            .collect();
        wire.extend(parity);
        wire
    }

    fn unit(frame: u16, idx: u16, k: u16, m: u16, payload: &[u8]) -> Unit<'_> {
        Unit { frame_id: frame, unit_index: idx, total_units: k + m, fec_units: m, adaptive_index: 0, payload }
    }

    fn feed(a: &mut VideoAssembler, frame: u16, wire: &[Vec<u8>], k: u16, m: u16, skip: &[usize]) -> Vec<Outcome> {
        wire.iter()
            .enumerate()
            .filter(|(i, _)| !skip.contains(i))
            .map(|(i, w)| a.push(&unit(frame, i as u16, k, m, w)))
            .collect()
    }

    fn content(n: usize, seed: u8) -> Vec<u8> {
        let mut v = vec![0, 0, 0, 1];
        v.extend((0..n).map(|i| (i as u8).wrapping_mul(7).wrapping_add(seed)));
        v
    }

    #[test]
    fn in_order_frames_are_delivered_as_they_complete() {
        let mut a = VideoAssembler::new(3);
        let d0 = content(500, 1);
        let w = encode_frame(&d0, 4, 2);
        let out = feed(&mut a, 0, &w, 4, 2, &[]);
        assert_eq!(out[..3], [Outcome::Pending; 3]);
        assert_eq!(out[3], Outcome::DeliveredInOrder);
        let f = a.pop_frame().unwrap();
        assert_eq!((f.id, f.data.clone(), f.starts_with_start_code), (0, d0, true));
        assert_eq!(a.cursor(), 1);
        // late parity unit of a delivered frame is ignored
        assert_eq!(a.push(&unit(0, 4, 4, 2, &w[4])), Outcome::Late);
    }

    #[test]
    fn missing_source_units_are_recovered_by_fec() {
        for lost in [vec![0usize], vec![1, 3], vec![0, 2]] {
            let mut a = VideoAssembler::new(3);
            let d = content(777, 9);
            let w = encode_frame(&d, 4, 2);
            feed(&mut a, 0, &w, 4, 2, &lost);
            let f = a.pop_frame().unwrap_or_else(|| panic!("no frame for lost {lost:?}"));
            assert_eq!(f.data, d, "lost {lost:?}");
        }
    }

    #[test]
    fn too_few_units_never_complete() {
        let mut a = VideoAssembler::new(3);
        let w = encode_frame(&content(300, 3), 4, 1);
        let out = feed(&mut a, 0, &w, 4, 1, &[0, 1]);
        assert!(out.iter().all(|o| *o == Outcome::Pending));
        assert!(a.pop_frame().is_none());
    }

    #[test]
    fn a_complete_future_frame_waits_for_the_missing_one() {
        let mut a = VideoAssembler::new(3);
        let (d0, d1) = (content(200, 1), content(200, 2));
        let (w0, w1) = (encode_frame(&d0, 2, 1), encode_frame(&d1, 2, 1));
        feed(&mut a, 0, &w0, 2, 1, &[]);
        a.pop_frame();
        // frame 2 completes before frame 1: blocked
        let w2 = encode_frame(&content(200, 5), 2, 1);
        let out = feed(&mut a, 2, &w2, 2, 1, &[]);
        assert_eq!(*out.last().unwrap(), Outcome::ReadyButBlocked);
        assert!(a.pop_frame().is_none());
        // frame 1 arrives: both flush in order
        let out = feed(&mut a, 1, &w1, 2, 1, &[]);
        assert!(out.contains(&Outcome::DeliveredInOrder)); // the third unit then finds the frame already delivered
        assert_eq!(*out.last().unwrap(), Outcome::Late);
        let ids: Vec<u16> = std::iter::from_fn(|| a.pop_frame()).map(|f| f.id).collect();
        assert_eq!(ids, vec![1, 2]);
        assert_eq!(a.cursor(), 3);
    }

    #[test]
    fn an_exhausted_window_skips_the_gap() {
        let mut a = VideoAssembler::new(2);
        let w0 = encode_frame(&content(100, 1), 2, 1);
        feed(&mut a, 0, &w0, 2, 1, &[]);
        a.pop_frame(); // cursor = 1
                       // frame 1 never arrives; frames 2 then 3 complete (limit = cursor + window = 3)
        let w2 = encode_frame(&content(100, 2), 2, 1);
        assert_eq!(*feed(&mut a, 2, &w2, 2, 1, &[]).last().unwrap(), Outcome::ReadyButBlocked);
        let w3 = encode_frame(&content(100, 3), 2, 1);
        let out = feed(&mut a, 3, &w3, 2, 1, &[]);
        assert!(out.contains(&Outcome::DeliveredWithGap));
        let ids: Vec<u16> = std::iter::from_fn(|| a.pop_frame()).map(|f| f.id).collect();
        assert_eq!(ids, vec![2, 3]);
        assert_eq!(a.cursor(), 4);
        // the skipped frame is late now
        assert_eq!(a.push(&unit(1, 0, 2, 1, &[0, 0, 1])), Outcome::Late);
    }

    #[test]
    fn invalid_and_oversized_frames_are_rejected() {
        let mut a = VideoAssembler::new(3);
        assert_eq!(
            a.push(&Unit {
                frame_id: 0,
                unit_index: 0,
                total_units: 1,
                fec_units: 2,
                adaptive_index: 0,
                payload: &[0, 0, 1]
            }),
            Outcome::Invalid
        );
        assert_eq!(a.push(&unit(0, 0, 2, 1, &[0xFF, 0xFF, 1])), Outcome::Pending); // block 65535+3 > 65000
        assert_eq!(a.push(&unit(1, 0, 2, 1, &[1])), Outcome::Pending); // source unit shorter than its 2-byte header
        assert_eq!(a.pending_frames(), 0);
    }

    #[test]
    fn a_flood_of_distinct_frame_ids_cannot_grow_the_store_without_bound() {
        let mut a = VideoAssembler::new(3);
        for id in 0..5000u16 {
            a.push(&unit(id.wrapping_mul(31), 0, 4, 2, &[0, 0, 1, 2, 3]));
        }
        assert!(a.pending_frames() <= MAX_PENDING_FRAMES);
    }

    #[test]
    fn one_byte_parity_first_unit_is_rejected_not_a_panic() {
        let mut a = VideoAssembler::new(3);
        assert_eq!(a.push(&unit(0, 3, 3, 2, &[7])), Outcome::Pending); // block would be 1
        assert_eq!(a.pending_frames(), 0);
    }

    #[test]
    fn duplicates_are_ignored_and_parity_first_frames_work() {
        let mut a = VideoAssembler::new(3);
        let d = content(300, 4);
        let w = encode_frame(&d, 3, 2);
        a.push(&unit(0, 3, 3, 2, &w[3])); // parity first: block size = its length
        assert_eq!(a.push(&unit(0, 3, 3, 2, &w[3])), Outcome::Pending); // duplicate
        feed(&mut a, 0, &w, 3, 2, &[3]);
        assert_eq!(a.pop_frame().unwrap().data, d);
    }

    #[test]
    fn frame_ids_wrap_at_65536() {
        let mut a = VideoAssembler::new(3);
        for (i, id) in [65534u16, 65535, 0, 1].iter().enumerate() {
            let d = content(50, i as u8);
            let w = encode_frame(&d, 2, 1);
            feed(&mut a, *id, &w, 2, 1, &[]);
            assert_eq!(a.pop_frame().unwrap().id, *id);
        }
        assert_eq!(a.cursor(), 2);
    }

    #[test]
    fn telemetry_reports_once_per_second() {
        let mut a = VideoAssembler::new(3);
        let w = encode_frame(&content(100, 1), 2, 1);
        feed(&mut a, 0, &w, 2, 1, &[]);
        a.pop_frame();
        assert_eq!(a.poll_telemetry(0.5), None);
        let (r, m) = a.poll_telemetry(1.5).unwrap();
        assert!(r >= 2 && m == 0);
        assert_eq!(a.poll_telemetry(1.6), None);
    }
}
