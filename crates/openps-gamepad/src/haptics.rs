//! Voice-coil haptics over Bluetooth: the console's 10 ms frames of 3 kHz stereo PCM become 64-byte blocks of signed 8-bit samples,
//! paced out at the rate they play (one block per 10.667 ms), the way PXPlay's `DualSenseBtHapticWriter` does it, with the report
//! layouts of its `DualSenseBtHapticReport`.

use crate::report::intensity_gain;

pub const REPORT_LEN: usize = 142;
pub const SETUP_LEN: usize = 78;
/// One block lasts this long: 64 bytes = 32 stereo pairs at 3 kHz.
pub const PERIOD_NS: u64 = 10_666_667;

const BLOCK: usize = 64;
const QUEUE_CAPACITY: usize = 512;
/// More than this queued means the pad fell behind: the oldest samples are dropped so the haptics stay in step with the picture.
const QUEUE_LIMIT: usize = 192;
/// A queue shorter than a block waits this many cycles for more before it is sent padded.
const CYCLES_BEFORE_DRAIN: u32 = 2;
/// Silent cycles after which the stream parks (nothing is sent until the next signal).
const CYCLES_BEFORE_PARK: u32 = 24;

fn sign(report: &mut [u8], report_id: u8) {
    let n = report.len() - 4;
    let mut crc = crc32fast::Hasher::new();
    crc.update(&[0xA2, report_id]);
    crc.update(&report[1..n]);
    report[n..].copy_from_slice(&crc.finalize().to_le_bytes());
}

fn header(seq: u8) -> [u8; REPORT_LEN] {
    let mut r = [0u8; REPORT_LEN];
    r[0] = 0x32;
    r[1] = (seq & 0x0F) << 4;
    r[2] = 0x91; // audio-control sub-packet
    r
}

/// One block of 64 samples.
pub fn single_report(block: &[u8; BLOCK], seq: u8, counter: u8) -> [u8; REPORT_LEN] {
    let mut r = header(seq);
    r[3] = 0x07; // length of the audio-control sub-packet
    r[4] = 0xFE;
    r[9] = 0xFF;
    r[10] = counter;
    r[11] = 0x92; // haptic PCM sub-packet, sized
    r[12] = BLOCK as u8;
    r[13..13 + BLOCK].copy_from_slice(block);
    sign(&mut r, 0x32);
    r
}

/// Two consecutive blocks in one report: half the writes, which a Bluetooth link on macOS needs.
pub fn pair_report(pair: &[u8; 2 * BLOCK], seq: u8, counter: u8) -> [u8; REPORT_LEN] {
    let mut r = header(seq);
    r[3] = 0x04;
    r[4] = 0x72;
    r[6] = 0x30;
    r[7] = counter;
    r[8] = 0xD2; // haptic PCM sub-packet, sized, two frames
    r[9] = BLOCK as u8;
    r[10..10 + 2 * BLOCK].copy_from_slice(pair);
    sign(&mut r, 0x32);
    r
}

pub fn silence_report(seq: u8, counter: u8) -> [u8; REPORT_LEN] {
    single_report(&[0; BLOCK], seq, counter)
}

/// Switches the haptic coils on (the pad leaves them off until told, which is why the sound would otherwise never arrive). Sent
/// before the first block, and again whenever the pad has been re-opened. `trigger_reduction` is the same byte as in the state report.
pub fn setup_report(seq: u8, trigger_reduction: u8) -> [u8; SETUP_LEN] {
    let mut r = [0u8; SETUP_LEN];
    r[0] = 0x31;
    r[1] = (seq & 0x0F) << 4;
    r[2] = 0x10;
    r[4] = 0x62; // valid flags: power-save control, reduce motor power and bit 5
    r[39] = trigger_reduction;
    r[42] = 1;
    sign(&mut r, 0x31);
    r
}

/// A console frame (30 stereo 16-bit little-endian pairs) as 60 signed 8-bit samples, scaled by the console's haptic level and the user's
/// rumble strength (in percent).
pub fn pcm8(frame: &[u8; 120], intensity: u8, pct: u16) -> [u8; 60] {
    let (num, den) = intensity_gain(intensity);
    let (num, den, pct) = (i32::from(num), i32::from(den), i32::from(pct));
    let mut out = [0u8; 60];
    for (sample, o) in frame.as_chunks::<2>().0.iter().zip(&mut out) {
        let value = i32::from(i16::from_le_bytes(*sample) >> 8);
        *o = (value * num * pct / (den * 100)).clamp(-128, 127) as i8 as u8;
    }
    out
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Out {
    Single([u8; BLOCK]),
    Pair([u8; 2 * BLOCK]),
}

/// The queue and pacing state for one pad. The caller calls `tick` every `PERIOD_NS` while `wants_tick` and reports each write with
/// `accepted`.
pub struct HapticStream {
    queue: Vec<u8>,
    pair_blocks: bool,
    held: Option<[u8; BLOCK]>,
    empty_cycles: u32,
    silent_cycles: u32,
    parked: bool,
    delivering: bool,
    /// A report has gone out since the last silence, so the coils may still be driven.
    active: bool,
}

impl HapticStream {
    pub fn new(pair_blocks: bool) -> Self {
        Self {
            queue: Vec::with_capacity(QUEUE_CAPACITY),
            pair_blocks,
            held: None,
            empty_cycles: 0,
            silent_cycles: 0,
            parked: true,
            delivering: false,
            active: false,
        }
    }

    pub fn offer(&mut self, frame: &[u8; 120], intensity: u8, pct: u16) {
        let samples = pcm8(frame, intensity, pct);
        if self.queue.len() + samples.len() > QUEUE_CAPACITY {
            let keep = QUEUE_CAPACITY - samples.len();
            self.queue.drain(..self.queue.len() - keep);
        }
        self.queue.extend_from_slice(&samples);
    }

    /// Whether the next block time matters: something is queued or the coils have not been parked yet.
    pub fn wants_tick(&self) -> bool {
        !self.parked || !self.queue.is_empty()
    }

    /// True while haptics are reaching the pad, so console rumble must stay out of the way.
    pub fn delivering(&self) -> bool {
        self.delivering
    }

    fn take(&mut self) -> Option<[u8; BLOCK]> {
        if self.queue.len() > QUEUE_LIMIT {
            let excess = self.queue.len() - QUEUE_LIMIT;
            self.queue.drain(..excess);
        }
        let n = if self.queue.len() >= BLOCK {
            BLOCK
        } else if self.queue.is_empty() || self.empty_cycles < CYCLES_BEFORE_DRAIN {
            return None;
        } else {
            self.queue.len()
        };
        let mut block = [0u8; BLOCK];
        block[..n].copy_from_slice(&self.queue[..n]);
        self.queue.drain(..n);
        Some(block)
    }

    /// One block time. The report to write now, if any.
    pub fn tick(&mut self) -> Option<Out> {
        let block = match self.take() {
            Some(b) => {
                self.empty_cycles = 0;
                b
            }
            None => {
                self.empty_cycles += 1;
                [0; BLOCK]
            }
        };
        if block.iter().any(|b| *b != 0) {
            self.silent_cycles = 0;
            self.parked = false;
        } else if !self.parked {
            self.silent_cycles += 1;
            if self.silent_cycles >= CYCLES_BEFORE_PARK {
                self.parked = true;
            }
        }
        if self.parked {
            self.delivering = false;
            self.held = None;
            return None;
        }
        if self.pair_blocks {
            let Some(first) = self.held.take() else {
                self.held = Some(block);
                return None;
            };
            let mut pair = [0u8; 2 * BLOCK];
            pair[..BLOCK].copy_from_slice(&first);
            pair[BLOCK..].copy_from_slice(&block);
            return Some(Out::Pair(pair));
        }
        Some(Out::Single(block))
    }

    /// The report from the last `tick` was written (or refused).
    pub fn accepted(&mut self, ok: bool) {
        self.delivering = ok;
        self.active |= ok;
    }

    /// The haptics are over (session end, pad switched away): whether a silence report is needed to settle the coils. Resets the stream.
    pub fn stop(&mut self) -> bool {
        let was_active = self.active;
        *self = Self::new(self.pair_blocks);
        was_active
    }
}
