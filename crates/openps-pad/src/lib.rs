//! Converts a [`ControllerState`] (from the Con2PC mapper or a physical pad) into the payloads that go
//! inside Takion FEEDBACK (type 6) and FEEDBACK_HISTORY (type 1) packets. Encoding rules come from the
//! decompiled `GamepadUtil`, `ControllerRemoteState` and `FeedbackDataBus` (spec 02 section 9).
//!
//! UNCERTAIN until checked against a captured frame: stick byte order/sign convention (copied as the app
//! emits them), and whether the console accepts analog L2/R2 values in history records.

use openps_input::state::{Button, ControllerState};
use std::collections::VecDeque;

pub const MAX_HISTORY: usize = 20;
pub const IDLE_RESEND_S: f64 = 0.1;
/// Packets built by `update` but not yet taken; a caller that never takes them cannot grow this without bound.
const MAX_PENDING_PACKETS: usize = 64;
/// Touch surface size the app uses to scale normalised coordinates (`RemoteTouchState`).
pub const TOUCH_W: f64 = 1920.0;
pub const TOUCH_H: f64 = 943.0;

// App button bit masks (GamepadUtil.getStandardButtonBitmask).
pub const CROSS: u32 = 1;
pub const CIRCLE: u32 = 2;
pub const SQUARE: u32 = 4;
pub const TRIANGLE: u32 = 8;
pub const DL: u32 = 16;
pub const DR: u32 = 32;
pub const DU: u32 = 64;
pub const DD: u32 = 128;
pub const L1: u32 = 256;
pub const R1: u32 = 512;
pub const L3: u32 = 1024;
pub const R3: u32 = 2048;
pub const OPTIONS: u32 = 4096;
pub const SHARE: u32 = 8192;
pub const TOUCHPAD: u32 = 16384;
pub const PS: u32 = 32768;
pub const L2: u32 = 65536;
pub const R2: u32 = 131072;

/// Button mask for a state. UP+DOWN and LEFT+RIGHT cancel (SOCD), as in all Con2PC encoders. MUTE has no
/// Takion button (UNCERTAIN: it is the stream microphone toggle) and is ignored here.
pub fn button_mask(st: &ControllerState) -> u32 {
    let has = |b: Button| st.buttons.contains(&b);
    let mut m = 0;
    for (b, bit) in [
        (Button::CROSS, CROSS),
        (Button::CIRCLE, CIRCLE),
        (Button::SQUARE, SQUARE),
        (Button::TRIANGLE, TRIANGLE),
        (Button::L1, L1),
        (Button::R1, R1),
        (Button::L3, L3),
        (Button::R3, R3),
        (Button::OPTIONS, OPTIONS),
        (Button::CREATE, SHARE),
        (Button::TOUCHPAD, TOUCHPAD),
        (Button::PS, PS),
    ] {
        if has(b) {
            m |= bit;
        }
    }
    if has(Button::DPAD_UP) != has(Button::DPAD_DOWN) {
        m |= if has(Button::DPAD_UP) { DU } else { DD };
    }
    if has(Button::DPAD_LEFT) != has(Button::DPAD_RIGHT) {
        m |= if has(Button::DPAD_LEFT) { DL } else { DR };
    }
    m
}

/// One 3-byte button event record (`GamepadUtil.getButtonState`): `[0x80, id, value]`. `pressed` is 0xFF/0x00.
pub fn button_record(bit: u32, pressed: bool) -> Option<[u8; 3]> {
    let v = if pressed { 0xFF } else { 0x00 };
    let (id, third) = match bit {
        CROSS => (0x88, v),
        CIRCLE => (0x89, v),
        SQUARE => (0x8A, v),
        TRIANGLE => (0x8B, v),
        DL => (0x82, v),
        DR => (0x83, v),
        DU => (0x80, v),
        DD => (0x81, v),
        L1 => (0x84, v),
        R1 => (0x85, v),
        L3 => (if pressed { 0xAF } else { 0x8F }, v), // only L3 carries the value in the third byte
        R3 => (if pressed { 0xB0 } else { 0x90 }, 0),
        OPTIONS => (if pressed { 0xAC } else { 0x8C }, 0),
        SHARE => (if pressed { 0xAD } else { 0x8D }, 0),
        TOUCHPAD => (if pressed { 0xB1 } else { 0x91 }, 0),
        PS => (if pressed { 0xAE } else { 0x8E }, 0),
        L2 => (0x86, v),
        R2 => (0x87, v),
        _ => return None,
    };
    Some([0x80, id, third])
}

/// Analog trigger record: same ids as L2/R2 with the analog value (0..=255) in the third byte.
pub fn trigger_record(is_r2: bool, value: u8) -> [u8; 3] {
    [0x80, if is_r2 { 0x87 } else { 0x86 }, value]
}

/// 5-byte touch record (`GamepadUtil.getTouchPadBuffer`). x, y are clipped to 0..=4095 (12 bit).
pub fn touch_record(x: i32, y: i32, counter: u8, down: bool) -> [u8; 5] {
    let (x, y) = (x.clamp(0, 4095), y.clamp(0, 4095));
    [
        if down { 0xD0 } else { 0xC0 },
        counter & 0x7F,
        (x >> 4) as u8,
        (((x & 15) << 4) | ((y >> 8) & 15)) as u8,
        (y & 0xFF) as u8,
    ]
}

/// Pad-presence record (`PadIdentity.presenceEvent`): `[0x80|pad, 0x1F | 0x20 if connected | 0x80]`.
pub fn presence_record(pad: u8, connected: bool) -> [u8; 2] {
    [0x80 | (pad & 3), 0x1F | if connected { 0x20 } else { 0 } | 0x80]
}

/// Pads 1-3 are stamped into the low bits of a record's first byte when its top bits are 0x80/0xA0/0xC0.
pub fn stamp(first: u8, pad: u8) -> u8 {
    if pad > 0 && matches!(first & 0xE0, 0x80 | 0xA0 | 0xC0) {
        (first & 0xFC) | (pad & 3)
    } else {
        first
    }
}

/// Gyro value (rad/s, range +-30) as i16: `(g + 30) * 65535 / 60`, cast to short (wraps like Java).
pub fn encode_gyro(rad_s: f32) -> i16 {
    ((rad_s + 30.0) * 65535.0 / 60.0) as i32 as i16
}
/// Accel value (g, range +-5) as i16: `(a + 5) * 65535 / 10`.
pub fn encode_accel(g: f32) -> i16 {
    ((g + 5.0) * 65535.0 / 10.0) as i32 as i16
}

/// "Smallest three" orientation compression (`OrientationUtil.compressOrientation`); q = (x, y, z, w).
pub fn compress_orientation(q: [f32; 4]) -> u32 {
    let (mut idx, mut max) = (0usize, 0f32);
    for (i, c) in q.iter().enumerate() {
        if c.abs() > max {
            max = c.abs();
            idx = i;
        }
    }
    let mut v: u32 = (if q[idx] < 0.0 { 1 } else { 0 }) | ((idx as u32) << 1);
    let mut k = 0;
    for (i, c) in q.iter().enumerate() {
        if i == idx {
            continue;
        }
        let c = c.clamp(-0.707_106_77, 0.707_106_77);
        let x = ((c + 0.707_106_77) * 361.331_57) as u32;
        v |= x << (3 + 9 * k);
        k += 1;
    }
    v
}

fn stick_i16(v: f64) -> i16 {
    (v.clamp(-1.0, 1.0) * 32767.0).round() as i16
}

pub struct PadFeedback {
    pad: u8,
    state: [u8; 28],
    history: VecDeque<Vec<u8>>,
    /// One snapshot of the history window per event `update` recorded, oldest first (see `take_history_packets`).
    pending: VecDeque<Vec<u8>>,
    prev_mask: u32,
    prev_l2: u8,
    prev_r2: u8,
    /// The two touch slots: where each finger last was, and the id it was given when it landed.
    touch_prev: [Option<(i32, i32)>; 2],
    touch_id: [u8; 2],
    /// One counter hands out ids for both fingers, so they never share one.
    touch_counter: u8,
    last_history_send: f64,
    state_dirty: bool,
    history_dirty: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Update {
    pub state_changed: bool,
    pub history_changed: bool,
}

impl PadFeedback {
    pub fn new(pad: u8) -> Self {
        let mut state = [0u8; 28];
        state[0] = 0xA0 | (pad & 3);
        for (i, b) in [0xFF, 0x7F, 0xFF, 0x7F, 0xFF, 0x7F].iter().enumerate() {
            state[1 + i] = *b;
        } // gyro
        state[7..13].copy_from_slice(&[0xFF, 0x7F, 0x99, 0x99, 0xFF, 0x7F]); // accel
        state[13..17].copy_from_slice(&[0xFE, 0xF7, 0xEF, 0x1F]); // identity orientation
        state[25..28].copy_from_slice(&[0, 0, 1]);
        Self {
            pad,
            state,
            history: VecDeque::new(),
            pending: VecDeque::new(),
            prev_mask: 0,
            prev_l2: 0,
            prev_r2: 0,
            touch_prev: [None, None],
            touch_id: [0, 0],
            touch_counter: 0,
            last_history_send: 0.0,
            state_dirty: false,
            history_dirty: false,
        }
    }

    pub fn pad(&self) -> u8 {
        self.pad
    }

    fn push(&mut self, rec: Vec<u8>) {
        if self.history.len() >= MAX_HISTORY {
            self.history.pop_back();
        }
        self.history.push_front(rec);
        self.history_dirty = true;
    }

    /// The console applies only one new event per history packet, so every event `update` records gets a packet of its own: the
    /// window as it stands right after that event. (The original app offers a packet per event; chiaki-ng fixed the same bug,
    /// "simultaneous L2 and R2 presses registered as R2 only".)
    fn emit(&mut self) {
        if self.pending.len() >= MAX_PENDING_PACKETS {
            self.pending.pop_front();
        }
        let packet = self.window();
        self.pending.push_back(packet);
    }

    /// The history window, newest first, each record stamped for this pad.
    fn window(&self) -> Vec<u8> {
        let mut out = vec![];
        for rec in &self.history {
            let mut r = rec.clone();
            r[0] = stamp(r[0], self.pad);
            out.extend(r);
        }
        out
    }

    /// Announce a pad joining/leaving (queued like any other history event).
    pub fn presence(&mut self, connected: bool) {
        self.push(presence_record(self.pad, connected).to_vec());
    }

    /// Overwrite the sensor bytes (gyro, accel, orientation) from a physical pad. Virtual pads keep neutral values.
    pub fn set_sensors(&mut self, gyro: [f32; 3], accel: [f32; 3], orientation: [f32; 4]) {
        for (i, g) in gyro.iter().enumerate() {
            self.state[1 + 2 * i..3 + 2 * i].copy_from_slice(&encode_gyro(*g).to_le_bytes());
        }
        for (i, a) in accel.iter().enumerate() {
            self.state[7 + 2 * i..9 + 2 * i].copy_from_slice(&encode_accel(*a).to_le_bytes());
        }
        self.state[13..17].copy_from_slice(&compress_orientation(orientation).to_le_bytes());
        self.state_dirty = true;
    }

    /// The id for a finger landing in `slot`: the next 7-bit counter value that is not the id of the finger still down in the
    /// other slot (the original app's `increaseMainTouchCounter` / `increaseSecondTouchCounter`).
    fn next_touch_id(&mut self, slot: usize) -> u8 {
        self.touch_counter = (self.touch_counter + 1) & 0x7F;
        if self.touch_prev[1 - slot].is_some() && self.touch_counter == self.touch_id[1 - slot] {
            self.touch_counter = (self.touch_counter + 1) & 0x7F;
        }
        self.touch_counter
    }

    /// Records the digital button edges in `diff`: the presses when `pressed`, the releases otherwise.
    fn button_edges(&mut self, diff: u32, mask: u32, pressed: bool, u: &mut Update) {
        for bit in [CROSS, CIRCLE, SQUARE, TRIANGLE, DL, DR, DU, DD, L1, R1, L3, R3, OPTIONS, SHARE, TOUCHPAD, PS] {
            if diff & bit != 0 && (mask & bit != 0) == pressed {
                if let Some(r) = button_record(bit, pressed) {
                    self.push(r.to_vec());
                    self.emit();
                    u.history_changed = true;
                }
            }
        }
    }

    /// Feed the newest pad state; button/trigger/touch edges become history events, sticks update the state payload.
    pub fn update(&mut self, st: &ControllerState, now: f64) -> Update {
        let mut u = Update::default();
        // sticks (idx 17..24, i16 big-endian)
        let mut sticks = [0u8; 8];
        for (i, v) in [st.left_x, st.left_y, st.right_x, st.right_y].iter().enumerate() {
            sticks[2 * i..2 * i + 2].copy_from_slice(&stick_i16(*v).to_be_bytes());
        }
        if self.state[17..25] != sticks {
            self.state[17..25].copy_from_slice(&sticks);
            self.state_dirty = true;
            u.state_changed = true;
        }

        // One record per edge, in the order chiaki-ng settled on for real consoles: button releases, then touch, then button
        // presses, then the triggers. A touchpad click is therefore never held without its finger (the finger lands before the click
        // is pressed and the click is released before the finger lifts).
        let mask = button_mask(st);
        let diff = mask ^ self.prev_mask;
        self.button_edges(diff, mask, false, &mut u);

        // touch: down/move records while a finger is on the pad, one up record when it lifts (two slots, like the original app)
        for (slot, finger) in [st.touch, st.touch2].into_iter().enumerate() {
            let cur = finger.map(|(x, y)| ((x * TOUCH_W) as i32, (y * TOUCH_H) as i32));
            if cur == self.touch_prev[slot] {
                continue;
            }
            match cur {
                Some((x, y)) => {
                    if self.touch_prev[slot].is_none() {
                        self.touch_id[slot] = self.next_touch_id(slot);
                    } // new finger-down
                    self.push(touch_record(x, y, self.touch_id[slot], true).to_vec());
                }
                None => {
                    let (x, y) = self.touch_prev[slot].unwrap();
                    self.push(touch_record(x, y, self.touch_id[slot], false).to_vec());
                }
            }
            self.emit();
            self.touch_prev[slot] = cur;
            u.history_changed = true;
        }

        self.button_edges(diff, mask, true, &mut u);
        self.prev_mask = mask;

        // analog triggers: record on every change of the 8-bit value
        let (l2, r2) = ((st.l2.clamp(0.0, 1.0) * 255.0).round() as u8, (st.r2.clamp(0.0, 1.0) * 255.0).round() as u8);
        if l2 != self.prev_l2 {
            self.push(trigger_record(false, l2).to_vec());
            self.emit();
            self.prev_l2 = l2;
            u.history_changed = true;
        }
        if r2 != self.prev_r2 {
            self.push(trigger_record(true, r2).to_vec());
            self.emit();
            self.prev_r2 = r2;
            u.history_changed = true;
        }
        let _ = now;
        u
    }

    /// FEEDBACK state payload (28 bytes, or the first 25 for Takion < 12).
    pub fn state_payload(&mut self, takion_version: u32) -> Vec<u8> {
        self.state_dirty = false;
        let n = if takion_version >= 12 { 28 } else { 25 };
        self.state[..n].to_vec()
    }
    pub fn state_dirty(&self) -> bool {
        self.state_dirty
    }

    /// FEEDBACK_HISTORY payload: up to 20 newest-first records, each stamped for this pad.
    pub fn history_payload(&mut self, now: f64) -> Vec<u8> {
        self.history_dirty = false;
        self.last_history_send = now;
        self.window()
    }

    /// The history packets for the events `update` recorded since the last call, oldest first: one per event, each the window
    /// right after that event. Send them in this order.
    pub fn take_history_packets(&mut self, now: f64) -> Vec<Vec<u8>> {
        self.history_dirty = false;
        self.last_history_send = now;
        self.pending.drain(..).collect()
    }

    /// True when a new event is waiting, or (with history present) 100 ms passed since the last send so loss self-heals.
    pub fn history_due(&self, now: f64) -> bool {
        !self.history.is_empty() && (self.history_dirty || now - self.last_history_send >= IDLE_RESEND_S)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st() -> ControllerState {
        ControllerState::default()
    }

    #[test]
    fn initial_state_payload_matches_the_app() {
        let mut p = PadFeedback::new(0);
        let b = p.state_payload(12);
        assert_eq!(b.len(), 28);
        assert_eq!(b[0], 0xA0);
        assert_eq!(&b[1..7], &[0xFF, 0x7F, 0xFF, 0x7F, 0xFF, 0x7F]);
        assert_eq!(&b[7..13], &[0xFF, 0x7F, 0x99, 0x99, 0xFF, 0x7F]);
        assert_eq!(&b[13..17], &[0xFE, 0xF7, 0xEF, 0x1F]);
        assert_eq!(&b[25..], &[0, 0, 1]);
        assert_eq!(p.state_payload(9).len(), 25);
        assert_eq!(PadFeedback::new(2).state_payload(12)[0], 0xA2);
    }

    #[test]
    fn identity_orientation_compresses_to_the_apps_initial_value() {
        assert_eq!(compress_orientation([0.0, 0.0, 0.0, 1.0]), 0x1FEF_F7FE);
        assert_eq!(compress_orientation([0.0, 0.0, 0.0, -1.0]) & 7, 0b111); // negative sign bit
    }

    #[test]
    fn sensor_encoding_ranges() {
        assert_eq!(encode_gyro(0.0), 0x7FFF);
        assert_eq!(encode_accel(0.0), 0x7FFF);
        assert_eq!(encode_gyro(-30.0), 0);
        let mut p = PadFeedback::new(0);
        p.set_sensors([0.0; 3], [0.0; 3], [0.0, 0.0, 0.0, 1.0]);
        assert_eq!(&p.state_payload(12)[1..3], &[0xFF, 0x7F]);
        assert_eq!(&p.state_payload(12)[13..17], &[0xFE, 0xF7, 0xEF, 0x1F]);
    }

    #[test]
    fn sticks_are_i16_big_endian_and_clamped() {
        let mut p = PadFeedback::new(0);
        let mut s = st();
        s.left_x = 1.0;
        s.left_y = -1.0;
        s.right_x = 0.5;
        s.right_y = 3.0;
        let u = p.update(&s, 0.0);
        assert!(u.state_changed && !u.history_changed);
        let b = p.state_payload(12);
        assert_eq!(&b[17..19], &32767i16.to_be_bytes());
        assert_eq!(&b[19..21], &(-32767i16).to_be_bytes());
        assert_eq!(&b[21..23], &16384i16.to_be_bytes());
        assert_eq!(&b[23..25], &32767i16.to_be_bytes());
        assert!(!p.update(&s, 0.1).state_changed); // unchanged
    }

    #[test]
    fn button_records_match_gamepad_util() {
        assert_eq!(button_record(CROSS, true), Some([0x80, 0x88, 0xFF]));
        assert_eq!(button_record(CROSS, false), Some([0x80, 0x88, 0x00]));
        assert_eq!(button_record(DU, true), Some([0x80, 0x80, 0xFF]));
        assert_eq!(button_record(DD, true), Some([0x80, 0x81, 0xFF]));
        assert_eq!(button_record(DL, true), Some([0x80, 0x82, 0xFF]));
        assert_eq!(button_record(DR, true), Some([0x80, 0x83, 0xFF]));
        assert_eq!(button_record(L1, true), Some([0x80, 0x84, 0xFF]));
        assert_eq!(button_record(R1, false), Some([0x80, 0x85, 0x00]));
        assert_eq!(button_record(L3, true), Some([0x80, 0xAF, 0xFF])); // L3: id by state, value in 3rd byte
        assert_eq!(button_record(L3, false), Some([0x80, 0x8F, 0x00]));
        assert_eq!(button_record(R3, true), Some([0x80, 0xB0, 0x00])); // others: 3rd byte 0
        assert_eq!(button_record(R3, false), Some([0x80, 0x90, 0x00]));
        assert_eq!(button_record(OPTIONS, true), Some([0x80, 0xAC, 0]));
        assert_eq!(button_record(SHARE, false), Some([0x80, 0x8D, 0]));
        assert_eq!(button_record(TOUCHPAD, true), Some([0x80, 0xB1, 0]));
        assert_eq!(button_record(PS, true), Some([0x80, 0xAE, 0]));
        assert_eq!(button_record(12345, true), None);
    }

    #[test]
    fn touch_record_layout_and_clipping() {
        assert_eq!(touch_record(0x123, 0x456, 5, true), [0xD0, 5, 0x12, 0x34, 0x56]);
        assert_eq!(touch_record(-5, 99999, 0x85, false), [0xC0, 0x05, 0x00, 0x0F, 0xFF]);
    }

    #[test]
    fn presence_and_pad_stamping() {
        assert_eq!(presence_record(1, true), [0x81, 0xBF]);
        assert_eq!(presence_record(1, false), [0x81, 0x9F]);
        assert_eq!(stamp(0x80, 2), 0x82);
        assert_eq!(stamp(0xA0, 3), 0xA3);
        assert_eq!(stamp(0xD0, 1), 0xD1); // touch down keeps its 0xD0 bits
        assert_eq!(stamp(0xC0, 1), 0xC1);
        assert_eq!(stamp(0x80, 0), 0x80);
        assert_eq!(stamp(0x12, 2), 0x12); // not a pad-tagged record
    }

    #[test]
    fn edges_become_history_newest_first_and_socd_cancels() {
        let mut p = PadFeedback::new(0);
        let mut s = st();
        s.buttons.insert(Button::CROSS);
        assert!(p.update(&s, 0.0).history_changed);
        s.buttons.insert(Button::CIRCLE);
        s.buttons.insert(Button::DPAD_UP);
        s.buttons.insert(Button::DPAD_DOWN); // cancels: no event
        p.update(&s, 0.1);
        let h = p.history_payload(0.1);
        // newest first: events of the second update (CIRCLE) precede the CROSS press
        assert_eq!(&h[..3], &[0x80, 0x89, 0xFF]);
        assert_eq!(&h[3..6], &[0x80, 0x88, 0xFF]);
        assert_eq!(h.len(), 6);
        s.buttons.remove(&Button::CROSS);
        p.update(&s, 0.2);
        assert_eq!(&p.history_payload(0.2)[..3], &[0x80, 0x88, 0x00]);
    }

    #[test]
    fn history_is_capped_at_20_and_stamped_for_the_pad() {
        let mut p = PadFeedback::new(2);
        let mut s = st();
        for i in 0..30 {
            if i % 2 == 0 {
                s.buttons.insert(Button::CROSS);
            } else {
                s.buttons.remove(&Button::CROSS);
            }
            p.update(&s, i as f64);
        }
        let h = p.history_payload(30.0);
        assert_eq!(h.len(), MAX_HISTORY * 3);
        assert!(h.chunks(3).all(|r| r[0] == 0x82)); // stamped with pad 2
    }

    #[test]
    fn triggers_emit_analog_records_on_change() {
        let mut p = PadFeedback::new(0);
        let mut s = st();
        s.r2 = 1.0;
        p.update(&s, 0.0);
        assert_eq!(&p.history_payload(0.0)[..3], &[0x80, 0x87, 0xFF]);
        s.r2 = 0.5;
        s.l2 = 1.0;
        p.update(&s, 0.1);
        let h = p.history_payload(0.1);
        assert_eq!(&h[..3], &[0x80, 0x87, 128]); // R2 change is processed after L2, so it is newest
        assert_eq!(&h[3..6], &[0x80, 0x86, 0xFF]);
        assert!(!p.update(&s, 0.2).history_changed);
    }

    #[test]
    fn touch_scales_to_the_apps_surface_and_counts_fingers() {
        let mut p = PadFeedback::new(0);
        let mut s = st();
        s.touch = Some((0.5, 0.5));
        p.update(&s, 0.0);
        let h = p.history_payload(0.0);
        assert_eq!(h, touch_record(960, 471, 1, true).to_vec()); // 1920*0.5, 943*0.5, first id = 1
        s.touch = Some((0.6, 0.5)); // move: same finger, same id
        p.update(&s, 0.1);
        assert_eq!(p.history_payload(0.1)[0..2], [0xD0, 1]);
        s.touch = None; // lift
        p.update(&s, 0.2);
        assert_eq!(p.history_payload(0.2)[0..2], [0xC0, 1]);
        s.touch = Some((0.1, 0.1)); // new finger: id increments
        p.update(&s, 0.3);
        assert_eq!(p.history_payload(0.3)[0..2], [0xD0, 2]);
    }

    #[test]
    fn idle_resend_every_100ms_only_with_history() {
        let mut p = PadFeedback::new(0);
        assert!(!p.history_due(10.0)); // nothing to resend
        let mut s = st();
        s.buttons.insert(Button::CROSS);
        p.update(&s, 0.0);
        assert!(p.history_due(0.0)); // new event
        p.history_payload(0.0);
        assert!(!p.history_due(0.05));
        assert!(p.history_due(0.11)); // self-healing resend
    }

    #[test]
    fn presence_events_are_queued() {
        let mut p = PadFeedback::new(1);
        p.presence(true);
        assert_eq!(p.history_payload(0.0), vec![0x81, 0xBF]);
    }

    #[test]
    fn mute_is_ignored() {
        let mut p = PadFeedback::new(0);
        let mut s = st();
        s.buttons.insert(Button::MUTE);
        assert!(!p.update(&s, 0.0).history_changed);
    }
}
