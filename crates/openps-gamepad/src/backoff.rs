//! Keeps a failing controller from being retried (and logged) on every command. After a failure the pad is left alone for a while;
//! the attempts skipped meanwhile are counted so the next message can say how many there were.

use std::time::{Duration, Instant};

#[derive(Debug, Default)]
pub struct Backoff {
    until: Option<Instant>,
    skipped: u32,
}

impl Backoff {
    /// True while the pad is being left alone; each call during that time counts as one skipped attempt.
    pub fn blocked(&mut self, now: Instant) -> bool {
        match self.until {
            Some(until) if now < until => {
                self.skipped = self.skipped.saturating_add(1);
                true
            }
            _ => false,
        }
    }

    /// Record a failure: leave the pad alone for `hold`. Returns how many attempts were skipped since the previous failure.
    pub fn fail(&mut self, now: Instant, hold: Duration) -> u32 {
        self.until = Some(now + hold);
        std::mem::take(&mut self.skipped)
    }

    /// A success ends the back-off at once.
    pub fn clear(&mut self) {
        self.until = None;
        self.skipped = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOLD: Duration = Duration::from_secs(1);

    #[test]
    fn a_fresh_pad_is_not_blocked() {
        let mut b = Backoff::default();
        assert!(!b.blocked(Instant::now()));
    }

    #[test]
    fn after_a_failure_attempts_are_skipped_until_the_hold_ends() {
        let t0 = Instant::now();
        let mut b = Backoff::default();
        assert_eq!(b.fail(t0, HOLD), 0, "nothing was skipped before the first failure");
        for ms in [0, 10, 500, 999] {
            assert!(b.blocked(t0 + Duration::from_millis(ms)), "{ms} ms");
        }
        assert!(!b.blocked(t0 + HOLD), "the hold is over");
    }

    #[test]
    fn the_next_failure_reports_how_many_attempts_were_skipped() {
        let t0 = Instant::now();
        let mut b = Backoff::default();
        b.fail(t0, HOLD);
        for ms in [1, 2, 3] {
            assert!(b.blocked(t0 + Duration::from_millis(ms)));
        }
        assert_eq!(b.fail(t0 + HOLD, HOLD), 3);
        assert_eq!(b.fail(t0 + HOLD * 2, HOLD), 0, "the count starts over");
    }

    #[test]
    fn a_success_ends_the_hold_and_forgets_the_count() {
        let t0 = Instant::now();
        let mut b = Backoff::default();
        b.fail(t0, HOLD);
        assert!(b.blocked(t0));
        b.clear();
        assert!(!b.blocked(t0));
        assert_eq!(b.fail(t0, HOLD), 0);
    }

    #[test]
    fn it_keeps_holding_when_a_failure_happens_during_the_hold() {
        let t0 = Instant::now();
        let mut b = Backoff::default();
        b.fail(t0, HOLD);
        b.fail(t0 + Duration::from_millis(500), HOLD);
        assert!(b.blocked(t0 + Duration::from_millis(1400)), "the hold restarted at the second failure");
        assert!(!b.blocked(t0 + Duration::from_millis(1500)));
    }
}
