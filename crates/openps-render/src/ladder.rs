//! The adaptive frame rate: when the GPU cannot keep every picture filter running at the rate that was asked for, the rate comes down
//! (and later goes up again) in steps of 5 frames per second instead of a filter being switched off. The picture stays as it was set up;
//! only how often it is drawn changes, so a Mac with room to spare shows the full rate and a weaker one shows the same picture more slowly.
//!
//! The rate moves gently, so that the picture never lurches:
//! * **What is measured.** The GPU time of every picture over the last second: the mean (how much of the time the GPU is busy) and the
//!   95th percentile (how long the slow pictures take: a picture that takes longer than the time between two refreshes is shown late,
//!   which is a spike in the frame time, however idle the GPU is on average). A real picture costs more than a generated one, so it is
//!   the mix over a second that counts, not one picture after another.
//! * **Down.** Only when the load has been too high for [`DOWN_AFTER_S`] seconds, by at most [`MAX_DOWN_HZ`] at a time and at least
//!   [`DOWN_SPACING_S`] seconds apart; each step goes toward the rate the measured times fit in, so a big overload takes a few seconds to
//!   settle and a short spike takes nothing.
//! * **Up.** One step of 5 only after the next step would still be comfortable for [`UP_AFTER_S`] seconds, and not within
//!   [`UP_COOLDOWN_S`] seconds of coming down: no sawtooth.
//!
//! The limits are well apart (down above [`HEAVY_MEAN`], up only below [`LIGHT_MEAN`] at the higher rate), so a steady load settles on one
//! step and stays there. Time is counted in pictures at the rate being shown, so the same load behaves the same whatever the clock does.

use std::collections::VecDeque;

/// The step between two rates on the ladder, frames per second.
pub const STEP_HZ: u32 = 5;
/// The lowest rate the ladder goes to.
pub const MIN_HZ: u32 = 15;
/// The GPU is too busy when it works for more than this share of the time, on average over the last second.
pub const HEAVY_MEAN: f64 = 0.75;
/// A picture is too slow when the slowest 5 % of the last second take longer than this share of the time between two pictures.
pub const HEAVY_PEAK: f64 = 1.0;
/// A step up is only taken when the mean at the higher rate would stay below this share...
pub const LIGHT_MEAN: f64 = 0.60;
/// ...and the slowest 5 % below this one.
pub const LIGHT_PEAK: f64 = 0.90;
/// How long the pictures that are looked at go back, seconds.
pub const WINDOW_S: f64 = 1.0;
/// How long the load must be too high before the rate comes down, seconds.
pub const DOWN_AFTER_S: f64 = 2.0;
/// The shortest time between two steps down, seconds.
pub const DOWN_SPACING_S: f64 = 1.5;
/// The most one step down takes off, frames per second.
pub const MAX_DOWN_HZ: u32 = 10;
/// How long the next step must stay comfortable before the rate goes up, seconds.
pub const UP_AFTER_S: f64 = 5.0;
/// The time after coming down during which the rate does not go up, seconds.
pub const UP_COOLDOWN_S: f64 = 10.0;

/// Most pictures kept for the statistics (a second at 240 fps, with room).
const RING: usize = 512;
/// The statistics are worked out every this many pictures.
const EVERY: u32 = 8;

/// The highest multiple of [`STEP_HZ`] at or below `hz`, never below [`MIN_HZ`].
pub fn floor_step(hz: f64) -> u32 {
    if !hz.is_finite() {
        return MIN_HZ;
    }
    (((hz / f64::from(STEP_HZ)).floor().max(0.0) as u32) * STEP_HZ).max(MIN_HZ)
}

/// The rate whose interval a picture that takes `gpu_ms` fills to exactly `share`: the most it can hold.
pub fn sustainable_hz(gpu_ms: f64, share: f64) -> f64 {
    if gpu_ms.is_finite() && gpu_ms > 0.0 {
        1000.0 * share / gpu_ms
    } else {
        f64::INFINITY
    }
}

/// The limit on the frame rate that the GPU's load calls for, if any.
#[derive(Clone, Debug, Default)]
pub struct RateLadder {
    cap: Option<u32>,
    /// GPU milliseconds of the most recent pictures.
    ring: VecDeque<f32>,
    since_eval: u32,
    over_s: f64,
    light_s: f64,
    since_change_s: f64,
    since_down_s: Option<f64>,
    /// Mean and 95th percentile load at the rate being shown, as of the last look.
    load: (f64, f64),
}

impl RateLadder {
    /// The limit in frames per second, or `None` when the asked rate can be held.
    pub fn cap(&self) -> Option<u32> {
        self.cap
    }

    /// The load over the last second at the rate being shown: how much of the time the GPU is busy, and how much of the time between two
    /// pictures the slowest of them take (1.0 = a picture takes as long as it has).
    pub fn load(&self) -> (f64, f64) {
        self.load
    }

    /// Seconds the load has been too high so far (the rate comes down at [`DOWN_AFTER_S`]).
    pub fn overloaded_for(&self) -> f64 {
        self.over_s
    }

    /// The rate to show: `requested` held to the limit.
    pub fn apply(&self, requested: u16) -> u16 {
        self.cap.map_or(requested, |c| requested.min(c.min(u32::from(u16::MAX)) as u16))
    }

    /// Forget the limit (the settings changed, so what the GPU can hold is to be found out again).
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// One displayed picture: `gpu_ms` is what it took on the GPU, `rate_hz` the rate being shown now (after the limit) and `requested_hz`
    /// the rate the settings ask for. Returns whether the limit changed.
    pub fn observe(&mut self, gpu_ms: f64, rate_hz: f64, requested_hz: f64) -> bool {
        if !(gpu_ms.is_finite() && gpu_ms >= 0.0 && rate_hz.is_finite() && rate_hz >= 1.0) {
            return false;
        }
        if self.ring.len() == RING {
            self.ring.pop_front();
        }
        self.ring.push_back(gpu_ms as f32);
        self.since_eval += 1;
        if self.since_eval < EVERY {
            return false;
        }
        let dt = f64::from(self.since_eval) / rate_hz;
        self.since_eval = 0;
        let n = ((rate_hz * WINDOW_S).ceil() as usize).clamp(EVERY as usize, RING).min(self.ring.len());
        if n < EVERY as usize {
            return false;
        }
        let mut recent: Vec<f32> = self.ring.iter().rev().take(n).copied().collect();
        let mean_ms = recent.iter().map(|v| f64::from(*v)).sum::<f64>() / n as f64;
        recent.sort_unstable_by(f32::total_cmp);
        let p95_ms = f64::from(recent[((n as f64 * 0.95).ceil() as usize).clamp(1, n) - 1]);
        self.load = (mean_ms * rate_hz / 1000.0, p95_ms * rate_hz / 1000.0);
        self.since_change_s += dt;
        if let Some(s) = self.since_down_s.as_mut() {
            *s += dt;
        }

        let overloaded = self.load.0 > HEAVY_MEAN || self.load.1 > HEAVY_PEAK;
        if overloaded {
            self.over_s += dt;
            self.light_s = 0.0;
            if self.over_s < DOWN_AFTER_S || self.since_change_s < DOWN_SPACING_S {
                return false;
            }
            // toward the rate the measured times fit in, but only a step at a time
            let fits = floor_step(sustainable_hz(mean_ms, HEAVY_MEAN).min(sustainable_hz(p95_ms, HEAVY_PEAK)));
            let now = floor_step(rate_hz);
            let new = fits.min(floor_step(rate_hz - 1.0)).max(now.saturating_sub(MAX_DOWN_HZ)).max(MIN_HZ);
            if f64::from(new) >= rate_hz || self.cap == Some(new) {
                return false; // already at the bottom of the ladder
            }
            self.cap = Some(new);
            (self.over_s, self.light_s, self.since_change_s, self.since_down_s) = (0.0, 0.0, 0.0, Some(0.0));
            return true;
        }
        // not overloaded: the count of too-heavy seconds runs out twice as fast as it built up
        self.over_s = (self.over_s - 2.0 * dt).max(0.0);
        let Some(cap) = self.cap else { return false };
        let next = cap.max(floor_step(rate_hz)) + STEP_HZ;
        let comfortable = mean_ms * f64::from(next) / 1000.0 < LIGHT_MEAN
            && p95_ms * f64::from(next) / 1000.0 < LIGHT_PEAK
            && self.since_down_s.is_none_or(|s| s >= UP_COOLDOWN_S);
        if !comfortable {
            self.light_s = 0.0;
            return false;
        }
        self.light_s += dt;
        if self.light_s < UP_AFTER_S {
            return false;
        }
        self.cap = (f64::from(next) < requested_hz).then_some(next);
        (self.light_s, self.since_change_s) = (0.0, 0.0);
        true
    }
}

/// The highest rate at or below `hz` that a fixed-refresh display of `refresh` Hz can show evenly: every picture held for the same whole
/// number of refreshes (60 Hz: 60, 30, 20; 144 Hz: 144, 72, 48, 36). A rate between them alternates one and two refreshes (45 on a 60 Hz
/// screen measured ±8 ms and 40 late frames in 2 s, against ±0.6 ms and none at 30). Used when real pictures are being dropped to hold
/// a lower rate; frames that are generated are placed for the refresh they are shown on and need no such rounding. An unknown refresh
/// rate leaves `hz` as it is.
pub fn even_rate(hz: u16, refresh: f64) -> u16 {
    if !(refresh.is_finite() && refresh >= 1.0) || hz == 0 {
        return hz;
    }
    let want = f64::from(hz);
    if want >= refresh * 0.99 {
        return hz;
    }
    // the fewest refreshes per picture whose rate does not exceed the wanted one (a hair of tolerance for 59.94 and the like)
    let k0 = (refresh / want - 0.01).ceil().max(1.0);
    // the rate is a whole number of Hz, so take the first count whose rate is within 1 % of one (144 Hz / 10 = 14.4 is not; 144 / 12 is)
    (0..24)
        .map(|extra| refresh / (k0 + f64::from(extra)))
        .find(|r| (r - r.round()).abs() <= 0.01 * r && r.round() >= 1.0)
        .map_or(hz, |r| r.round() as u16)
}

/// What a fixed-refresh display is asked to show when `hz` pictures a second are wanted: the even rate below (see [`even_rate`]), unless
/// `hz` is within a tenth of the next even rate up. A rate that close (55 on a 60 Hz screen: one picture in eleven is shown for two
/// refreshes) is far less uneven than one in the middle (45), and giving up a tenth or more of the speed to remove it would be holding the
/// GPU back for little; below that the even rate is the better picture.
pub fn settled_rate(hz: u16, refresh: f64) -> u16 {
    let even = even_rate(hz, refresh);
    if even == hz || !(refresh.is_finite() && refresh >= 1.0) {
        return even;
    }
    let k = (refresh / f64::from(even)).round();
    if k < 2.0 {
        return even;
    }
    let next_up = refresh / (k - 1.0);
    if f64::from(hz) >= next_up * 0.9 {
        hz
    } else {
        even
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fixed_display_shows_only_rates_that_hold_every_picture_for_the_same_number_of_refreshes() {
        // 60 Hz: 45 would alternate one and two refreshes
        for (want, even) in [(60, 60), (59, 30), (45, 30), (31, 30), (30, 30), (29, 20), (20, 20), (15, 15), (1, 1)] {
            assert_eq!(even_rate(want, 60.0), even, "{want} on 60 Hz");
        }
        // 144 Hz
        for (want, even) in
            [(144, 144), (140, 72), (120, 72), (75, 72), (72, 72), (60, 48), (48, 48), (45, 36), (30, 29), (15, 13)]
        {
            assert_eq!(even_rate(want, 144.0), even, "{want} on 144 Hz");
        }
        // 120 Hz
        for (want, even) in [(120, 120), (90, 60), (75, 60), (60, 60), (45, 40)] {
            assert_eq!(even_rate(want, 120.0), even, "{want} on 120 Hz");
        }
    }

    #[test]
    fn a_rate_at_or_above_the_refresh_rate_and_unknown_inputs_are_left_alone() {
        assert_eq!(even_rate(240, 144.0), 240, "the display caps it elsewhere");
        assert_eq!(even_rate(144, 143.9), 144);
        assert_eq!(
            even_rate(143, 144.0),
            143,
            "within 1 % of the refresh rate counts as the refresh rate, as in effective_rate"
        );
        for refresh in [0.0, -1.0, f64::NAN, f64::INFINITY, 0.5] {
            assert_eq!(even_rate(45, refresh), 45, "refresh {refresh}");
        }
        assert_eq!(even_rate(0, 60.0), 0);
        // a 59.94 Hz display counts as 60
        assert_eq!(even_rate(30, 59.94), 30);
    }

    #[test]
    fn a_rate_close_below_an_even_one_is_kept_and_one_in_the_middle_is_not() {
        // 60 Hz: 55 and 54 are within a tenth of 60; 50 and 45 are not and go to 30
        for (want, shown) in
            [(60, 60), (55, 55), (54, 54), (53, 30), (50, 30), (45, 30), (31, 30), (29, 29), (27, 27), (26, 20)]
        {
            assert_eq!(settled_rate(want, 60.0), shown, "{want} on 60 Hz");
        }
        // 144 Hz: 130 is within a tenth of 144; 120 is not and goes to 72; 70 is within a tenth of 72
        for (want, shown) in [(144, 144), (130, 130), (120, 72), (72, 72), (65, 65), (64, 48), (45, 45), (40, 36)] {
            assert_eq!(settled_rate(want, 144.0), shown, "{want} on 144 Hz");
        }
        assert_eq!(settled_rate(45, 0.0), 45, "an unknown refresh rate leaves it");
        assert_eq!(settled_rate(240, 144.0), 240);
    }

    /// Show pictures for `seconds` (at the rate being shown), each taking `cost(i)` ms, asking for `requested`. Returns the rate shown at the
    /// end and every change as (seconds into the run, new rate).
    fn run_with(
        l: &mut RateLadder,
        mut shown: u16,
        requested: u16,
        seconds: f64,
        cost: impl Fn(usize) -> f64,
    ) -> (u16, Vec<(f64, u16)>) {
        let (mut t, mut i, mut changes) = (0.0, 0usize, vec![]);
        while t < seconds {
            if l.observe(cost(i), f64::from(shown), f64::from(requested)) {
                shown = l.apply(requested);
                changes.push((t, shown));
            }
            t += 1.0 / f64::from(shown);
            i += 1;
        }
        (shown, changes)
    }

    fn run(l: &mut RateLadder, gpu_ms: f64, requested: u16, seconds: f64) -> u16 {
        let start = l.apply(requested);
        run_with(l, start, requested, seconds, |_| gpu_ms).0
    }

    #[test]
    fn rates_floor_to_the_closest_multiple_of_5() {
        for (hz, want) in [
            (120.0, 120),
            (119.9, 115),
            (77.8, 75),
            (74.9, 70),
            (59.0, 55),
            (44.9, 40),
            (15.0, 15),
            (14.0, 15),
            (3.0, 15),
        ] {
            assert_eq!(floor_step(hz), want, "{hz}");
        }
        assert_eq!(floor_step(f64::NAN), MIN_HZ);
        assert_eq!(floor_step(f64::INFINITY), MIN_HZ);
        assert_eq!(floor_step(-5.0), MIN_HZ);
    }

    #[test]
    fn a_gpu_with_room_never_lowers_the_rate() {
        let mut l = RateLadder::default();
        assert_eq!(run(&mut l, 3.0, 120, 60.0), 120, "3 ms of 8.3 ms is 36 %");
        assert_eq!(l.cap(), None);
        assert_eq!(run(&mut l, 5.9, 120, 60.0), 120, "71 %: under the 75 % line");
    }

    #[test]
    fn too_heavy_a_load_comes_down_a_step_at_a_time_never_more_than_10_and_a_second_apart() {
        // 9 ms a picture at 120 asked for: 108 % busy; 75 % of the time fits 83 pictures a second, so it settles on 80
        let mut l = RateLadder::default();
        let (end, changes) = run_with(&mut l, 120, 120, 30.0, |_| 9.0);
        assert_eq!(end, 80, "{changes:?}");
        let rates: Vec<u16> = changes.iter().map(|c| c.1).collect();
        assert_eq!(rates, vec![110, 100, 90, 80], "{changes:?}");
        let mut last = (0.0, 120u16);
        for (t, r) in &changes {
            assert!(last.1 - r <= 10, "one step off at most 10: {last:?} -> {r}");
            assert!(t - last.0 >= DOWN_SPACING_S - 0.2 || last.0 == 0.0, "{changes:?}");
            last = (*t, *r);
        }
        assert!(changes[0].0 >= DOWN_AFTER_S - 0.2, "not before a second of overload: {changes:?}");
        // a load that nothing fits stops at the bottom instead of going on
        let mut l = RateLadder::default();
        assert_eq!(run(&mut l, 200.0, 60, 120.0), MIN_HZ as u16);
    }

    #[test]
    fn a_short_spike_takes_nothing() {
        let mut l = RateLadder::default();
        let mut shown = 144u16;
        // 5 s of light pictures, then half a second of heavy ones, then light again
        for (secs, ms) in [(5.0, 3.0), (0.5, 12.0), (30.0, 3.0)] {
            let (s, changes) = run_with(&mut l, shown, 144, secs, |_| ms);
            assert!(changes.is_empty(), "{secs} s of {ms} ms: {changes:?}");
            shown = s;
        }
        assert_eq!((shown, l.cap()), (144, None));
    }

    #[test]
    fn slow_real_pictures_between_quick_generated_ones_count_even_though_the_mean_is_fine() {
        // 60 -> 144 on a GPU that takes 9 ms for a real picture and 2.5 ms for a generated one: the mean is 5.3 ms (76 % at 144 fps, a hair
        // over) and every real picture takes longer than the 6.9 ms between two refreshes: they are shown late. One picture after another
        // never saw it (each light one cancelled the count of heavy ones).
        let mut l = RateLadder::default();
        let (end, changes) = run_with(&mut l, 144, 144, 40.0, |i| if i % 12 < 5 { 9.0 } else { 2.5 });
        assert!(end < 144 && !changes.is_empty(), "the slow pictures call for a lower rate: {end} {changes:?}");
        // and a mix that fits is left alone: 5 ms and 2.5 ms
        let mut l = RateLadder::default();
        let (end, changes) = run_with(&mut l, 144, 144, 40.0, |i| if i % 12 < 5 { 5.0 } else { 2.5 });
        assert!(changes.is_empty() && end == 144, "{end} {changes:?}");
    }

    #[test]
    fn when_the_load_goes_away_the_rate_climbs_back_one_step_at_a_time_and_not_at_once() {
        let mut l = RateLadder::default();
        let (down, _) = run_with(&mut l, 120, 120, 30.0, |_| 14.0);
        assert_eq!(down, 50);
        // a light load: 3 ms a picture would use 36 % at 120 fps; but not within 10 s of coming down, then 5 s a step
        let (end, changes) = run_with(&mut l, down, 120, 200.0, |_| 3.0);
        let rates: Vec<u16> = changes.iter().map(|c| c.1).collect();
        assert_eq!(rates, (55..=120).step_by(5).collect::<Vec<u16>>(), "one step of 5 at a time: {changes:?}");
        assert_eq!((end, l.cap()), (120, None), "the limit ends at the asked rate: {changes:?}");
        for w in changes.windows(2) {
            assert!(w[1].0 - w[0].0 >= UP_AFTER_S - 0.3, "a step up every {UP_AFTER_S} s at most: {changes:?}");
        }
    }

    #[test]
    fn the_rate_does_not_go_back_up_within_the_cooldown_of_coming_down() {
        let mut l = RateLadder::default();
        // heavy for just long enough to come down once, then light at once
        let (down, drops) = run_with(&mut l, 120, 120, DOWN_AFTER_S + 0.5, |_| 9.0);
        assert_eq!(drops.len(), 1, "{drops:?}");
        let (_, ups) = run_with(&mut l, down, 120, 60.0, |_| 2.0);
        let since_drop = ups[0].0 + (DOWN_AFTER_S + 0.5 - drops[0].0);
        assert!(
            since_drop >= UP_COOLDOWN_S - 0.3,
            "the first step up came {since_drop:.1} s after the step down: {ups:?}"
        );
    }

    #[test]
    fn a_step_up_is_only_taken_when_the_next_one_would_still_be_comfortable() {
        // 9 ms: 50 fps uses 45 %; 60 would use 54 % (below 60 %), 65 would use 59 % and 70 would use 63 %: it climbs to 65 and stops
        let mut l = RateLadder::default();
        assert_eq!(run(&mut l, 14.0, 120, 30.0), 50);
        assert_eq!(run(&mut l, 9.0, 120, 120.0), 65);
        // at 11 ms the next step (55: 60.5 %) is not under the 60 % line: it stays at 50
        let mut l = RateLadder::default();
        assert_eq!(run(&mut l, 14.0, 120, 30.0), 50);
        assert_eq!(run(&mut l, 11.0, 120, 120.0), 50);
    }

    #[test]
    fn a_steady_load_settles_on_one_step_and_does_not_flap() {
        for gpu_ms in [6.0, 9.0, 12.0, 20.0, 30.0] {
            let mut l = RateLadder::default();
            let (settled, changes) = run_with(&mut l, 120, 120, 120.0, |_| gpu_ms);
            assert!(changes.len() <= 12, "{gpu_ms} ms changed the rate {} times", changes.len());
            assert!(
                gpu_ms * f64::from(settled) / 1000.0 <= HEAVY_MEAN + 1e-9 || settled == MIN_HZ as u16,
                "{gpu_ms} ms at {settled}"
            );
            // and another two minutes change nothing
            let (after, more) = run_with(&mut l, settled, 120, 120.0, |_| gpu_ms);
            assert!(more.is_empty() && after == settled, "{gpu_ms} ms keeps its step: {more:?}");
        }
    }

    #[test]
    fn a_noisy_load_around_the_line_does_not_make_the_rate_go_up_and_down() {
        // pictures between 4 and 8 ms at random-ish: the mean (6 ms) is 72 % at 120 fps, the slowest are over the line at times
        let mut l = RateLadder::default();
        let noise = |i: usize| 4.0 + ((i * 7919) % 17) as f64 * 0.25;
        let (end, changes) = run_with(&mut l, 120, 120, 300.0, noise);
        let ups = changes.windows(2).filter(|w| w[1].1 > w[0].1).count();
        assert!(changes.len() <= 8 && ups <= 2, "{end} {changes:?}");
    }

    #[test]
    fn a_new_ask_below_the_limit_is_not_held_and_a_reset_forgets_it() {
        let mut l = RateLadder::default();
        assert_eq!(run(&mut l, 14.0, 120, 30.0), 50);
        assert_eq!(l.apply(30), 30, "asking for less than the limit is not raised to it");
        assert_eq!(l.apply(60), 50);
        l.reset();
        assert_eq!((l.cap(), l.apply(120)), (None, 120));
    }

    #[test]
    fn nonsense_readings_change_nothing() {
        let mut l = RateLadder::default();
        for _ in 0..1000 {
            assert!(!l.observe(f64::NAN, 120.0, 120.0));
            assert!(!l.observe(-3.0, 120.0, 120.0));
            assert!(!l.observe(9.0, 0.0, 120.0));
            assert!(!l.observe(9.0, f64::NAN, 120.0));
            assert!(!l.observe(f64::INFINITY, 120.0, 120.0));
        }
        assert_eq!(l.cap(), None);
    }

    #[test]
    fn the_load_is_reported_for_the_rate_being_shown() {
        let mut l = RateLadder::default();
        run(&mut l, 4.0, 100, 3.0);
        let (mean, peak) = l.load();
        assert!((mean - 0.4).abs() < 0.02 && (peak - 0.4).abs() < 0.02, "{mean} {peak}");
    }
}
