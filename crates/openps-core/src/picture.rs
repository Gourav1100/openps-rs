//! First-picture watchdog: a session that is connected but never shows a picture looks "connected" forever (black screen, working
//! sound). The original app and chiaki-ng report this after a couple of seconds and give up after about 15.

use std::time::Duration;

/// Give up when no picture has been shown this long after connecting.
pub const GIVE_UP_AFTER: Duration = Duration::from_secs(15);
/// Say so in the log when there is still no picture after this long.
pub const WARN_AFTER: Duration = Duration::from_secs(2);

/// Video arrived but nothing could be decoded: the decoder does not cope with this stream. Another codec may.
pub const DECODER_REASON: &str =
    "video arrived but no picture could be decoded; the decoder may not support this stream";
/// Not a single video frame arrived: a network or console problem, not a codec one.
pub const NO_VIDEO_REASON: &str = "no video arrived from the console";

#[derive(Debug, PartialEq, Eq)]
pub enum Watch {
    Fine,
    /// Still nothing after `WARN_AFTER` (reported once).
    Slow,
    GiveUp(&'static str),
}

pub struct PictureWatch {
    give_up_after: Duration,
    warned: bool,
}

impl PictureWatch {
    pub fn new(give_up_after: Duration) -> Self {
        Self { give_up_after, warned: false }
    }

    /// `up`: time since the session connected. `shown`: pictures the media sink has shown, `None` if it does not decode (nothing to
    /// watch). `video_seen`: whether any video frame arrived.
    pub fn check(&mut self, up: Duration, shown: Option<u64>, video_seen: bool) -> Watch {
        if shown.is_none_or(|n| n > 0) {
            return Watch::Fine;
        }
        if up >= self.give_up_after {
            return Watch::GiveUp(if video_seen { DECODER_REASON } else { NO_VIDEO_REASON });
        }
        if up >= WARN_AFTER.min(self.give_up_after) && !self.warned {
            self.warned = true;
            return Watch::Slow;
        }
        Watch::Fine
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: fn(u64) -> Duration = Duration::from_secs;

    #[test]
    fn nothing_to_watch_when_the_sink_does_not_decode_or_has_shown_a_picture() {
        let mut w = PictureWatch::new(GIVE_UP_AFTER);
        assert_eq!(w.check(S(60), None, true), Watch::Fine);
        assert_eq!(w.check(S(60), Some(1), true), Watch::Fine);
        assert_eq!(w.check(S(60), Some(900), false), Watch::Fine);
    }

    #[test]
    fn a_missing_picture_is_reported_once_at_two_seconds_and_fatal_at_fifteen() {
        let mut w = PictureWatch::new(GIVE_UP_AFTER);
        assert_eq!(w.check(Duration::from_millis(1999), Some(0), true), Watch::Fine);
        assert_eq!(w.check(S(2), Some(0), true), Watch::Slow);
        assert_eq!(w.check(S(3), Some(0), true), Watch::Fine, "the warning is not repeated");
        assert_eq!(w.check(Duration::from_millis(14_999), Some(0), true), Watch::Fine);
        assert_eq!(w.check(S(15), Some(0), true), Watch::GiveUp(DECODER_REASON));
    }

    #[test]
    fn the_reason_tells_a_decoder_problem_from_a_missing_stream() {
        let mut w = PictureWatch::new(GIVE_UP_AFTER);
        assert_eq!(w.check(S(15), Some(0), false), Watch::GiveUp(NO_VIDEO_REASON));
        assert_eq!(w.check(S(15), Some(0), true), Watch::GiveUp(DECODER_REASON));
    }

    #[test]
    fn a_picture_that_arrives_late_cancels_the_give_up() {
        let mut w = PictureWatch::new(GIVE_UP_AFTER);
        assert_eq!(w.check(S(14), Some(0), true), Watch::Slow);
        assert_eq!(w.check(S(16), Some(1), true), Watch::Fine);
    }

    #[test]
    fn a_short_limit_still_warns_before_giving_up() {
        let mut w = PictureWatch::new(Duration::from_millis(300));
        assert_eq!(w.check(Duration::from_millis(100), Some(0), true), Watch::Fine);
        assert_eq!(w.check(Duration::from_millis(300), Some(0), true), Watch::GiveUp(DECODER_REASON));
    }
}
