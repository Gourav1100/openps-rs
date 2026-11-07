//! DualSense output reports: what the console asked for (rumble, lightbar, adaptive triggers) as the bytes the pad takes.
//! Layout of the 47-byte common block follows the Linux `hid-playstation` driver; the trigger blocks (mode byte + 10 parameters) are
//! exactly what the console sends, so effects arrive unchanged.

use openps_stream::padblob::PadOutput;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bus {
    Usb,
    Bluetooth,
}

/// The user's own strength settings, applied on top of what the console asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tuning {
    /// Rumble and haptics, in percent of what the console asks for (100 = as asked, above it stronger up to the pad's maximum).
    pub rumble_pct: u16,
    /// Adaptive-trigger strength, in percent of the pad's full power (100 = full; the pad cannot go above it).
    pub trigger_pct: u16,
}

impl Default for Tuning {
    fn default() -> Self {
        Self { rumble_pct: 100, trigger_pct: 100 }
    }
}

/// What a Test button makes the pad do for a moment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Probe {
    Rumble,
    Trigger,
}

impl Probe {
    /// How long the probe lasts: a buzz is short; the trigger effect stays long enough to press the triggers and feel it.
    pub fn duration(self) -> std::time::Duration {
        match self {
            Probe::Rumble => std::time::Duration::from_millis(700),
            Probe::Trigger => std::time::Duration::from_secs(4),
        }
    }
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
    /// The pad's firmware version, once read. Decides how rumble is asked for (see `RUMBLE_V2_FIRMWARE`).
    pub firmware: Option<u16>,
    pub tuning: Tuning,
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
            PadOutput::MotionReset => {} // concerns the sensors, not the output report
        }
    }

    /// A probe is over. The pad keeps the last effect it was given until told otherwise, and a report that carries no rumble or trigger
    /// flags tells it nothing, so what the probe started would never stop. Make sure the next reports carry a stop (or whatever the console
    /// asked for before): rumble at 0 and effect-free trigger blocks.
    pub fn settle_after_probe(&mut self, probe: Probe) {
        match probe {
            Probe::Rumble => {
                self.rumble.get_or_insert((0, 0));
            }
            Probe::Trigger => {
                self.right_trigger.get_or_insert([0; 11]);
                self.left_trigger.get_or_insert([0; 11]);
            }
        }
    }

    /// What to write while haptics reach the pad: the original app drops console rumble then (the coils and the motors would fight),
    /// and so do we, without forgetting it. Everything else is as it was.
    pub fn held_back_for_haptics(&self) -> Self {
        Self { rumble: self.rumble.map(|_| (0, 0)), haptic_motor: 0, ..self.clone() }
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
            firmware: self.firmware,
            tuning: self.tuning,
        }
    }

    /// The state for a Test button: the console's requests do not matter (it may have switched vibration off), the user's strength does.
    /// One probe at a time: a rumble probe leaves the triggers as they are and a trigger probe does not rumble.
    pub fn probing(&self, probe: Probe) -> Self {
        let haptic = self.intensity.map_or(DEFAULT_INTENSITY, |(h, _)| h.max(1));
        let mut s = Self { intensity: Some((haptic, 1)), haptic_motor: 0, ..self.clone() };
        match probe {
            Probe::Rumble => s.rumble = Some((200, 200)),
            Probe::Trigger => {
                s.rumble = Some((0, 0));
                let mut effect = [0u8; 11];
                effect[..3].copy_from_slice(&[0x01, 0x20, 0xFF]); // continuous resistance from a fifth of the way down, full force
                s.right_trigger = Some(effect);
                s.left_trigger = Some(effect);
            }
        }
        s
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

fn scale(value: u8, level: u8, pct: u16) -> u8 {
    let (num, den) = intensity_gain(level);
    (u32::from(value) * u32::from(num) * u32::from(pct) / (u32::from(den) * 100)).min(255) as u8
}

/// The pad's power reduction for the user's trigger strength: 0 steps at 100 %, 15 (off) at 0 %.
fn user_trigger_reduction(pct: u16) -> u8 {
    ((u32::from(100 - pct.min(100)) * 15 + 50) / 100) as u8
}

/// From this firmware on the pad takes rumble through `valid_flag2` bit 2 with full-strength values; before it, through the
/// compatible-vibration bit with values the pad doubles (chiaki-ng, `Controller::SetDualSenseRumble`).
pub const RUMBLE_V2_FIRMWARE: u16 = 0x0224;

/// The firmware version in a DualSense feature report 0x20 (64 bytes, version at 44..46 little-endian, as SDL reads it).
pub fn firmware_version(report: &[u8]) -> Option<u16> {
    (report.first() == Some(&0x20) && report.len() >= 46).then(|| u16::from_le_bytes([report[44], report[45]]))
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
pub fn trigger_reduction(level: u8) -> u8 {
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
        let pct = s.tuning.rumble_pct;
        let (mut right, mut left) =
            (scale(right.max(s.haptic_motor), intensity, pct), scale(left.max(s.haptic_motor), intensity, pct));
        match s.firmware {
            Some(f) if f >= RUMBLE_V2_FIRMWARE => {
                c[0] |= 0x02; // haptics select
                c[38] |= 0x04; // compatible vibration 2
            }
            Some(_) => {
                c[0] |= 0x01 | 0x02; // compatible vibration + haptics select
                (right, left) = (right >> 1, left >> 1);
            }
            None => c[0] |= 0x01 | 0x02,
        }
        c[2] = right;
        c[3] = left;
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
    // Once a level is known it is always written (strong too): the pad keeps its last reduction until told otherwise. The user's strength
    // can only add to it.
    let user = user_trigger_reduction(s.tuning.trigger_pct);
    if s.intensity.is_some() || user > 0 {
        let console = s.intensity.map_or(0, |(_, trigger)| trigger_reduction(trigger) & 0xF0);
        c[1] |= 0x40;
        c[36] = console.max(user << 4);
    }
    if let Some([r, g, b]) = s.led {
        c[1] |= 0x04; // lightbar control
        c[38] |= 0x02; // lightbar setup valid
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
