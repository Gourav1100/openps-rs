//! DualSense output reports: what the console asked for (rumble, lightbar, adaptive triggers) as the bytes the pad takes.
//! Layout of the 47-byte common block follows the Linux `hid-playstation` driver; the trigger blocks (mode byte + 10 parameters) are
//! exactly what the console sends, so effects arrive unchanged.

use openps_stream::padblob::PadOutput;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bus {
    Usb,
    Bluetooth,
}

/// What the pad should be doing. `None` = never asked, so the report leaves that part alone.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OutState {
    /// (left/heavy, right/light)
    pub rumble: Option<(u8, u8)>,
    pub led: Option<[u8; 3]>,
    pub right_trigger: Option<[u8; 11]>,
    pub left_trigger: Option<[u8; 11]>,
    /// Console intensity levels (haptic, adaptive trigger), each 0..=3.
    pub intensity: Option<(u8, u8)>,
    /// Rumble made from the console's haptic audio (see `haptic_rumble`), on both motors; the stronger of this and `rumble` counts.
    pub haptic_motor: u8,
}

impl OutState {
    pub fn apply(&mut self, out: &PadOutput) {
        match out {
            PadOutput::Rumble { left, right, .. } => self.rumble = Some((*left, *right)),
            PadOutput::Led { r, g, b, .. } => self.led = Some([*r, *g, *b]),
            PadOutput::AdaptiveTriggers { left_type, right_type, left, right, .. } => {
                self.left_trigger = Some(trigger(*left_type, left));
                self.right_trigger = Some(trigger(*right_type, right));
            }
            PadOutput::Intensity { haptic, adaptive_trigger } => {
                self.intensity = Some((*haptic, *adaptive_trigger));
            }
        }
    }

    /// Everything off: no rumble, no trigger effect (the lightbar stays).
    pub fn silenced(&self) -> Self {
        Self {
            rumble: Some((0, 0)),
            led: self.led,
            right_trigger: Some([0; 11]),
            left_trigger: Some([0; 11]),
            intensity: self.intensity,
            haptic_motor: 0,
        }
    }
}

/// The console's intensity setting as a gain (numerator, denominator): 0 = off, 1 = strong (full), 2 = medium (x0.5), 3 = weak
/// (x0.33). The original app and chiaki-ng agree; anything else is treated as strong, like the original.
pub fn intensity_gain(level: u8) -> (u16, u16) {
    match level {
        0 => (0, 1),
        2 => (1, 2),
        3 => (33, 100),
        _ => (1, 1),
    }
}

/// Used when the console has not said yet: its default is strong.
pub const DEFAULT_INTENSITY: u8 = 1;

fn scale(value: u8, level: u8) -> u8 {
    let (num, den) = intensity_gain(level);
    (u16::from(value) * num / den) as u8
}

fn trigger(kind: u8, params: &[u8; 10]) -> [u8; 11] {
    let mut t = [0u8; 11];
    t[0] = kind;
    t[1..].copy_from_slice(params);
    t
}

const COMMON: usize = 47;

/// The pad's own power reduction for adaptive triggers, for the console's trigger-intensity level: high nibble of byte 36 (strong
/// 0x00, medium 0x60, weak 0x90, off 0xF0), as chiaki-ng writes it. The low nibble is the vibration reduction, applied in software.
fn trigger_reduction(level: u8) -> u8 {
    match level {
        0 => 0xF0,
        2 => 0x60,
        3 => 0x90,
        _ => 0x00,
    }
}

fn common(s: &OutState) -> [u8; COMMON] {
    let mut c = [0u8; COMMON];
    if let Some((left, right)) = s.rumble {
        let intensity = s.intensity.map_or(DEFAULT_INTENSITY, |(haptic, _)| haptic);
        c[0] |= 0x01 | 0x02; // compatible vibration + haptics select
        c[2] = scale(right.max(s.haptic_motor), intensity);
        c[3] = scale(left.max(s.haptic_motor), intensity);
    }
    // With trigger intensity off the console's effects are released instead of played.
    let triggers_off = s.intensity.is_some_and(|(_, trigger)| trigger == 0);
    if let Some(t) = s.right_trigger {
        c[0] |= 0x04;
        c[10..21].copy_from_slice(&if triggers_off { [0; 11] } else { t });
    }
    if let Some(t) = s.left_trigger {
        c[0] |= 0x08;
        c[21..32].copy_from_slice(&if triggers_off { [0; 11] } else { t });
    }
    // Once a level is known it is always written (strong too): the pad keeps its last reduction until told otherwise.
    if let Some((_, trigger)) = s.intensity {
        c[1] |= 0x40;
        c[36] = trigger_reduction(trigger);
    }
    if let Some([r, g, b]) = s.led {
        c[1] |= 0x04; // lightbar control
        c[38] = 0x02; // lightbar setup valid
        c[41] = 0x02; // lightbar setup: fade out the blue pulse of power-on
        c[44..47].copy_from_slice(&[r, g, b]);
    }
    c
}

/// Bluetooth: report 0x31, 78 bytes, CRC32 over 0xA2 + the first 74 bytes. `seq` is a 4-bit counter.
pub fn bluetooth(s: &OutState, seq: u8) -> [u8; 78] {
    let mut b = [0u8; 78];
    b[0] = 0x31;
    b[1] = (seq & 0x0F) << 4;
    b[2] = 0x10;
    b[3..3 + COMMON].copy_from_slice(&common(s));
    let mut h = crc32fast::Hasher::new();
    h.update(&[0xA2]);
    h.update(&b[..74]);
    b[74..].copy_from_slice(&h.finalize().to_le_bytes());
    b
}

/// USB: report 0x02, 63 bytes.
pub fn usb(s: &OutState) -> [u8; 63] {
    let mut b = [0u8; 63];
    b[0] = 0x02;
    b[1..1 + COMMON].copy_from_slice(&common(s));
    b
}

pub fn build(s: &OutState, bus: Bus, seq: u8) -> Vec<u8> {
    match bus {
        Bus::Bluetooth => bluetooth(s, seq).to_vec(),
        Bus::Usb => usb(s).to_vec(),
    }
}
