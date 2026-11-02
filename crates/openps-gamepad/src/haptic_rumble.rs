//! Rumble from haptic audio, for a pad that cannot play the audio itself (a DualSense over USB: the voice-coil haptics need the pad's
//! separate USB audio device, which is not driven here). The console sends 10 ms frames of 3 kHz stereo 16-bit PCM; chiaki-ng turns
//! them into rumble like this (`StreamSession::PushHapticsFrame`):
//!
//! * a frame's strength is the mean absolute sample of each channel, times 2, on a 16-bit scale; the louder channel counts;
//! * below 100 it is silence, and a non-zero strength is raised to at least 512 so the motors actually turn;
//! * every three frames (30 ms) the strengths are averaged and both motors get that value; when it falls to zero the motors are
//!   told to stop.
//!
//! Differences from chiaki-ng, on purpose: a full-scale frame saturates at 65535 instead of wrapping to 0 (its `uint16_t` cast), and
//! the console's vibration level is applied by the report builder, not here.

use std::time::{Duration, Instant};

const FRAMES_PER_STEP: usize = 3;
/// How long the motors keep running without a new frame. The console sends one every 10 ms while haptics play, so a gap this long
/// means they ended; chiaki-ng's rumble timer would keep the last value forever if the stream simply stopped.
pub const HOLD: Duration = Duration::from_millis(150);
/// Mean absolute sample times 2 at or below this is silence.
const SILENCE: u32 = 100;
/// The smallest non-zero strength: weaker values do not move the motors.
const MIN_STRENGTH: u32 = 512;

/// Strength of one 120-byte frame (30 interleaved stereo little-endian 16-bit pairs), 0 for silence.
pub fn frame_strength(frame: &[u8; 120]) -> u16 {
    let (mut left, mut right) = (0u32, 0u32);
    for pair in frame.as_chunks::<4>().0 {
        left += u32::from(i16::from_le_bytes([pair[0], pair[1]]).unsigned_abs());
        right += u32::from(i16::from_le_bytes([pair[2], pair[3]]).unsigned_abs());
    }
    let pairs = (frame.len() / 4) as u32;
    let (left, right) = (left * 2 / pairs, right * 2 / pairs);
    let loud = left.max(right);
    if loud <= SILENCE {
        return 0;
    }
    loud.max(MIN_STRENGTH).min(u32::from(u16::MAX)) as u16
}

/// A strength on the 16-bit scale as a motor value on the pad's 8-bit scale; anything above silence moves the motor at least a notch.
pub fn motor_value(strength: u16) -> u8 {
    match strength {
        0 => 0,
        s => ((s >> 8) as u8).max(1),
    }
}

/// Averages the strengths of three frames at a time and reports the motor value when it changes.
#[derive(Debug, Default)]
pub struct HapticRumble {
    frames: [u16; FRAMES_PER_STEP],
    filled: usize,
    motor: u8,
    deadline: Option<Instant>,
}

impl HapticRumble {
    /// Feed one frame. Returns the new motor value after every third frame, if it differs from the current one.
    pub fn feed(&mut self, frame: &[u8; 120], now: Instant) -> Option<u8> {
        self.frames[self.filled] = frame_strength(frame);
        self.filled += 1;
        let mut changed = None;
        if self.filled == FRAMES_PER_STEP {
            self.filled = 0;
            let sum: u32 = self.frames.iter().map(|&s| u32::from(s)).sum();
            let motor = motor_value((sum / FRAMES_PER_STEP as u32) as u16);
            if motor != self.motor {
                self.motor = motor;
                changed = Some(motor);
            }
        }
        self.deadline = (self.motor != 0).then_some(now + HOLD);
        changed
    }

    /// The haptics stopped arriving: the motors must stop too. `Some(0)` once if they were running.
    pub fn stop(&mut self) -> Option<u8> {
        self.filled = 0;
        self.deadline = None;
        (self.motor != 0).then(|| {
            self.motor = 0;
            0
        })
    }

    pub fn motor(&self) -> u8 {
        self.motor
    }

    /// When the motors must be stopped unless another frame arrives first; `None` while they are off.
    pub fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    /// Stops the motors if no frame arrived for `HOLD`. `Some(0)` if that changed them.
    pub fn expire(&mut self, now: Instant) -> Option<u8> {
        if self.deadline.is_some_and(|d| now >= d) {
            return self.stop();
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::OnceLock;

    fn t0() -> Instant {
        static BASE: OnceLock<Instant> = OnceLock::new();
        *BASE.get_or_init(Instant::now)
    }

    /// A frame with constant samples `l` and `r` on the two channels.
    fn frame(l: i16, r: i16) -> [u8; 120] {
        let mut f = [0u8; 120];
        for pair in f.as_chunks_mut::<4>().0 {
            pair[..2].copy_from_slice(&l.to_le_bytes());
            pair[2..].copy_from_slice(&r.to_le_bytes());
        }
        f
    }

    #[test]
    fn silence_and_faint_noise_are_not_rumble() {
        assert_eq!(frame_strength(&[0; 120]), 0);
        assert_eq!(frame_strength(&frame(50, -50)), 0, "mean 50 x 2 = 100 is still silence");
        assert_eq!(frame_strength(&frame(51, 0)), 512, "just above silence is raised to the minimum");
    }

    #[test]
    fn strength_is_twice_the_mean_absolute_sample_of_the_louder_channel() {
        assert_eq!(frame_strength(&frame(8192, 0)), 16384, "left only");
        assert_eq!(frame_strength(&frame(0, -8192)), 16384, "right only, negative samples count by size");
        assert_eq!(frame_strength(&frame(1000, 3000)), 6000, "the louder channel wins");
    }

    #[test]
    fn a_full_scale_frame_saturates_instead_of_wrapping_to_zero() {
        assert_eq!(frame_strength(&frame(i16::MIN, i16::MIN)), u16::MAX);
        assert_eq!(motor_value(u16::MAX), 255);
    }

    #[test]
    fn motor_values_scale_from_16_to_8_bits_and_never_round_a_live_motor_to_zero() {
        assert_eq!(motor_value(0), 0);
        assert_eq!(motor_value(512), 2);
        assert_eq!(motor_value(16384), 64);
        assert_eq!(motor_value(1), 1, "anything above silence moves the motor");
    }

    #[test]
    fn the_motor_follows_the_average_of_every_three_frames() {
        let mut h = HapticRumble::default();
        let loud = frame(8192, 8192); // strength 16384
        assert_eq!(h.feed(&loud, t0()), None);
        assert_eq!(h.feed(&loud, t0()), None);
        assert_eq!(
            h.feed(&frame(0, 0), t0()),
            Some(motor_value(((16384u32 * 2) / 3) as u16)),
            "two loud and one silent frame average out"
        );
        assert_eq!(h.motor(), 42);
    }

    #[test]
    fn only_a_changed_motor_value_is_reported() {
        let mut h = HapticRumble::default();
        let loud = frame(8192, 8192);
        for _ in 0..2 {
            h.feed(&loud, t0());
        }
        assert_eq!(h.feed(&loud, t0()), Some(64));
        for _ in 0..2 {
            assert_eq!(h.feed(&loud, t0()), None);
        }
        assert_eq!(h.feed(&loud, t0()), None, "same value again: nothing to write");
        for _ in 0..2 {
            h.feed(&frame(0, 0), t0());
        }
        assert_eq!(h.feed(&frame(0, 0), t0()), Some(0), "silence stops the motor");
    }

    #[test]
    fn stop_reports_zero_once_and_only_if_the_motor_was_running() {
        let mut h = HapticRumble::default();
        assert_eq!(h.stop(), None, "nothing was running");
        let loud = frame(8192, 8192);
        for _ in 0..3 {
            h.feed(&loud, t0());
        }
        assert_eq!(h.motor(), 64);
        assert_eq!(h.stop(), Some(0));
        assert_eq!(h.motor(), 0);
        assert_eq!(h.stop(), None);
    }

    #[test]
    fn a_stop_in_the_middle_of_a_group_forgets_the_frames_already_counted() {
        let mut h = HapticRumble::default();
        let loud = frame(8192, 8192);
        h.feed(&loud, t0());
        h.feed(&loud, t0());
        h.stop();
        h.feed(&frame(0, 0), t0());
        h.feed(&frame(0, 0), t0());
        assert_eq!(h.feed(&frame(0, 0), t0()), None, "the old loud frames no longer count and the motor is already 0");
    }

    #[test]
    fn running_motors_expire_150_ms_after_the_last_frame_and_not_before() {
        let mut h = HapticRumble::default();
        let loud = frame(8192, 8192);
        for _ in 0..3 {
            h.feed(&loud, t0());
        }
        assert_eq!(h.deadline(), Some(t0() + HOLD));
        assert_eq!(h.expire(t0() + HOLD - Duration::from_millis(1)), None, "still within the hold");
        assert_eq!(h.expire(t0() + HOLD), Some(0));
        assert_eq!((h.motor(), h.deadline()), (0, None));
        assert_eq!(h.expire(t0() + HOLD * 2), None, "already stopped");
    }

    #[test]
    fn every_frame_pushes_the_deadline_out_even_while_the_value_stays_the_same() {
        let mut h = HapticRumble::default();
        let loud = frame(8192, 8192);
        let step = Duration::from_millis(10);
        for i in 0..30 {
            h.feed(&loud, t0() + step * i);
        }
        assert_eq!(h.motor(), 64);
        assert_eq!(h.deadline(), Some(t0() + step * 29 + HOLD));
        assert_eq!(h.expire(t0() + step * 29 + HOLD - Duration::from_millis(1)), None);
    }

    #[test]
    fn silence_and_stop_leave_no_deadline_behind() {
        let mut h = HapticRumble::default();
        let loud = frame(8192, 8192);
        for _ in 0..3 {
            h.feed(&loud, t0());
        }
        for _ in 0..3 {
            h.feed(&frame(0, 0), t0());
        }
        assert_eq!((h.motor(), h.deadline()), (0, None), "the motors are off, nothing to expire");
        for _ in 0..3 {
            h.feed(&loud, t0());
        }
        h.stop();
        assert_eq!(h.deadline(), None);
    }
}
