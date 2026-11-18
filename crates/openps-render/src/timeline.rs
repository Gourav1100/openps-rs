//! The content clock: decides *what time of the game each displayed frame should show*.
//!
//! Network delivery is jittery, but the console produces frames at an even rate and numbers them. So the frame id says
//! exactly when a frame's content belongs, however late it arrived. We learn the earliest plausible arrival time of each
//! id (the "base": a leaky minimum of arrival - id * interval, i.e. the best case the network has shown recently) and
//! define frame k's content time as `base + k * interval`. A frame that arrives late is then simply *further behind* the
//! content clock, and the renderer advances it by that much (extrapolation) instead of showing it at the wrong time. The
//! result: displayed motion keeps a constant pace through jitter, loss and bursts, and latency is never worse than the
//! network's own best case plus a bounded catch-up.

use std::collections::VecDeque;

/// Frames of history kept for estimating the interval and the base (3 s at 60 fps).
const WINDOW: usize = 180;
/// Frames needed before the long-baseline estimate is trusted; before that the median of recent deltas is used.
const MIN_BASELINE: usize = 48;
/// Frames during which the clock is still settling. A decoder or a network path that starts late delivers its first frames in a
/// catch-up burst, much faster than the console produced them; those arrival times say nothing about the stream's rate or its
/// delay, so they are shown but not learned from, and the base follows the best case directly instead of creeping.
const WARMUP: u64 = 90;
/// Ids this far behind the newest are a counter restart, not a reordered frame.
const REORDER_WINDOW: i64 = 300;
/// Consecutive frames this late (seconds) mean the path delay stepped for good: re-base instead of mixing old and new.
const STEP_LATE: f64 = 0.05;
const STEP_FRAMES: u32 = 30;
/// An arrival gap below this fraction of the expected one (per frame id) is a catch-up burst while warming up.
const BURST_RATIO: f64 = 0.85;
/// The content clock may move by at most this much per frame when its base is re-estimated (seconds): a slow, invisible
/// slew instead of a jump. Larger corrections (a real change of path delay) are taken over several frames.
const MAX_BASE_SLEW: f64 = 60e-6;
/// Longest advance (in frame intervals) the renderer may apply to a frame. Beyond this we hold rather than invent more.
pub const MAX_ALPHA: f64 = 1.25;
/// The most the smoothing buffer of Smooth mode may delay the picture (seconds).
pub const MAX_PLAYOUT: f64 = 0.025;
/// The share of the recent frames that the smoothing buffer covers: a frame later than this share of the others still causes a hold.
const PLAYOUT_QUANTILE: f64 = 0.98;
/// How fast the buffer may grow and shrink per frame (seconds). A change moves the content back or forward in time, so it is slow: a
/// gentle drift is not seen, a jump would be.
const PLAYOUT_UP: f64 = 0.0004;
const PLAYOUT_DOWN: f64 = 0.0001;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TimelineStats {
    /// Estimated source frame interval, seconds.
    pub interval: f64,
    /// Smoothed delivery delay beyond the best case (queueing), seconds.
    pub queue_delay: f64,
    /// Smoothed absolute deviation of that delay, seconds: the network jitter.
    pub jitter: f64,
    pub frames: u64,
    /// Ids missing from the sequence (lost or skipped by the console).
    pub gaps: u64,
}

#[derive(Debug)]
pub struct Timeline {
    interval: f64,
    base: Option<f64>,
    last_id: Option<i64>, // unwrapped id of the newest frame
    last_raw: Option<u16>,
    last_arrival: f64,
    /// (unwrapped id, arrival seconds)
    window: VecDeque<(i64, f64)>,
    queue_delay: f64,
    jitter: f64,
    frames: u64,
    gaps: u64,
    warmup: u64,
    late_run: u32,
    /// How late (beyond the best case) the recent frames arrived, seconds.
    delays: VecDeque<f64>,
    /// The smoothing buffer is on, and how much it delays Smooth mode's picture now (seconds).
    buffer: bool,
    playout: f64,
}

impl Default for Timeline {
    fn default() -> Self {
        Self::new(1.0 / 60.0)
    }
}

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    v[v.len() / 2]
}

impl Timeline {
    pub fn new(initial_interval: f64) -> Self {
        Self {
            interval: if initial_interval.is_finite() {
                initial_interval.clamp(1.0 / 240.0, 1.0 / 10.0)
            } else {
                1.0 / 60.0
            },
            base: None,
            last_id: None,
            last_raw: None,
            last_arrival: f64::NEG_INFINITY,
            window: VecDeque::new(),
            queue_delay: 0.0,
            jitter: 0.0,
            frames: 0,
            gaps: 0,
            warmup: WARMUP,
            late_run: 0,
            delays: VecDeque::new(),
            buffer: false,
            playout: 0.0,
        }
    }

    pub fn reset(&mut self) {
        let (w, b) = (self.warmup, self.buffer);
        *self = Self::new(self.interval);
        self.warmup = w;
        self.buffer = b;
    }

    /// Switch the smoothing buffer of Smooth mode on or off. On, the picture is held back by how late the slowest frames usually are (at most
    /// [`MAX_PLAYOUT`]), so a frame that arrives late still arrives before it is needed and the motion does not stall; off, the picture is
    /// as early as the network's best case ever allowed, and every late frame is a hold.
    pub fn set_buffer(&mut self, on: bool) {
        self.buffer = on;
        if !on {
            self.playout = 0.0;
        }
    }

    /// How much the smoothing buffer delays Smooth mode's picture now, seconds (0 when it is off).
    pub fn playout(&self) -> f64 {
        if self.buffer {
            self.playout
        } else {
            0.0
        }
    }

    pub fn interval(&self) -> f64 {
        self.interval
    }

    pub fn stats(&self) -> TimelineStats {
        TimelineStats {
            interval: self.interval,
            queue_delay: self.queue_delay,
            jitter: self.jitter,
            frames: self.frames,
            gaps: self.gaps,
        }
    }

    /// Extend a 16-bit id to a monotonically increasing integer, treating a small backward step as a reorder.
    fn unwrap(&self, raw: u16) -> i64 {
        match (self.last_id, self.last_raw) {
            (Some(last), Some(lraw)) => last + i64::from(raw.wrapping_sub(lraw) as i16), // signed distance modulo 2^16
            _ => i64::from(raw),
        }
    }

    /// Re-estimate the interval. Median of recent per-id deltas while the window is short (unbiased for symmetric
    /// jitter, learns any rate); afterwards the slope between the best-case arrivals of the two halves of the window,
    /// whose long baseline makes the error tiny whatever the jitter.
    fn estimate_interval(&mut self) {
        let n = self.window.len();
        let est = if n >= MIN_BASELINE {
            let best = |range: std::ops::Range<usize>| {
                let i = self.interval;
                range
                    .map(|k| self.window[k])
                    .min_by(|a, b| {
                        (a.1 - a.0 as f64 * i).partial_cmp(&(b.1 - b.0 as f64 * i)).unwrap_or(std::cmp::Ordering::Equal)
                    })
                    .unwrap()
            };
            let (p, q) = (best(0..n / 2), best(n / 2..n));
            ((q.0 - p.0) > (n as i64) / 4).then(|| (q.1 - p.1) / (q.0 - p.0) as f64)
        } else if n >= 4 {
            let mut d: Vec<f64> = self
                .window
                .iter()
                .zip(self.window.iter().skip(1))
                .map(|(a, b)| (b.1 - a.1) / (b.0 - a.0) as f64)
                .collect();
            Some(median(&mut d))
        } else {
            None
        };
        if let Some(e) = est.filter(|e| e.is_finite() && *e > 1.0 / 400.0 && *e < 1.0 / 5.0) {
            let w = if n >= MIN_BASELINE { 0.15 } else { 0.5 };
            self.interval += (e - self.interval) * w;
        }
    }

    /// A real frame arrived. Returns false (and changes nothing) for a stale, duplicate or nonsensical one.
    pub fn on_frame(&mut self, raw_id: u16, arrival: f64) -> bool {
        if !arrival.is_finite() || arrival < self.last_arrival {
            return false; // the clock must not run backwards
        }
        let id = self.unwrap(raw_id);
        if self.last_id.is_some_and(|l| l - id > REORDER_WINDOW) {
            // far behind: the counter restarted rather than a frame being reordered
            let i = self.interval;
            *self = Self::new(i);
            return self.on_frame(raw_id, arrival);
        }
        if self.last_id.is_some_and(|l| id <= l) {
            return false; // older or repeated frame: never show time going backwards
        }
        if let Some(last) = self.last_id {
            if id - last > 600 {
                // a huge jump means the stream restarted: start over
                let i = self.interval;
                *self = Self::new(i);
                return self.on_frame(raw_id, arrival);
            }
            let missing = id - last - 1;
            if missing > 0 {
                self.gaps += missing as u64;
            }
        }
        let burst = self.frames < WARMUP
            && self
                .last_id
                .is_some_and(|l| (arrival - self.last_arrival) < BURST_RATIO * self.interval * (id - l) as f64);
        if !burst {
            self.window.push_back((id, arrival));
            while self.window.len() > WINDOW {
                self.window.pop_front();
            }
            self.estimate_interval();
        }
        // base: the best case seen in the window, approached by a bounded slew
        let i = self.interval;
        let target = self.window.iter().map(|(k, a)| a - *k as f64 * i).fold(f64::INFINITY, f64::min);
        let target = if target.is_finite() { target } else { arrival - id as f64 * i }; // burst before any steady frame
        self.base = Some(match self.base {
            None => target,
            Some(b) => {
                let d = target - b;
                if d.abs() > 0.05 || self.frames < self.warmup {
                    target
                } else {
                    b + d.clamp(-MAX_BASE_SLEW, MAX_BASE_SLEW)
                }
            }
        });
        let mut q = (arrival - id as f64 * i - self.base.unwrap_or(0.0)).max(0.0);
        self.late_run = if q > STEP_LATE { self.late_run + 1 } else { 0 };
        if self.late_run >= STEP_FRAMES {
            // sustained step in arrival delay: forget the old window so it cannot corrupt the interval estimate
            self.window.clear();
            self.window.push_back((id, arrival));
            self.base = Some(arrival - id as f64 * i);
            self.late_run = 0;
            q = 0.0;
        }
        self.delays.push_back(q);
        while self.delays.len() > WINDOW {
            self.delays.pop_front();
        }
        if self.buffer && self.delays.len() >= 30 && self.frames >= self.warmup {
            let mut d: Vec<f64> = self.delays.iter().copied().collect();
            d.sort_by(f64::total_cmp);
            let target =
                d[((d.len() as f64 * PLAYOUT_QUANTILE).ceil() as usize).clamp(1, d.len()) - 1].min(MAX_PLAYOUT);
            self.playout = if target > self.playout {
                (self.playout + PLAYOUT_UP).min(target)
            } else {
                (self.playout - PLAYOUT_DOWN).max(target)
            };
        }
        self.queue_delay += (q - self.queue_delay) * 0.1;
        self.jitter += ((q - self.queue_delay).abs() - self.jitter) * 0.1;
        self.last_id = Some(id);
        self.last_raw = Some(raw_id);
        self.last_arrival = arrival;
        self.frames += 1;
        true
    }

    /// Where frame `raw_id`'s content belongs on the local clock (seconds).
    pub fn content_time(&self, raw_id: u16) -> Option<f64> {
        let base = self.base?;
        let id = self.unwrap(raw_id);
        Some(base + id as f64 * self.interval)
    }

    /// How far (in source frame intervals) to advance the newest frame so it shows the content of time `now`.
    /// Zero before any frame; clamped to `[0, MAX_ALPHA]`.
    pub fn alpha(&self, now: f64) -> f32 {
        let (Some(base), Some(id)) = (self.base, self.last_id) else { return 0.0 };
        if !now.is_finite() {
            return 0.0;
        }
        let t = base + id as f64 * self.interval;
        (((now - t) / self.interval).clamp(0.0, MAX_ALPHA)) as f32
    }

    /// Smooth (interpolating) mode: the fraction between the previous real frame (0) and the newest (1) that the content clock
    /// currently points at. The clock is held back by one source interval, so the newest frame is first *shown* when the next
    /// one arrives: that hold-back is the latency cost of interpolation. With the smoothing buffer on it is held back further by how late the
    /// slowest recent frames were, so a late frame is still there in time. `prev_raw_id` is the previously shown real frame.
    pub fn interp_beta(&self, now: f64, prev_raw_id: u16) -> f32 {
        let (Some(base), Some(id)) = (self.base, self.last_id) else { return 1.0 };
        let prev = self.unwrap(prev_raw_id);
        if prev >= id {
            return 1.0;
        }
        let (t_prev, t_cur) = (base + prev as f64 * self.interval, base + id as f64 * self.interval);
        let t_content = now - self.interval - self.playout(); // one interval (and the smoothing buffer) behind the arrival clock
        (((t_content - t_prev) / (t_cur - t_prev)).clamp(0.0, 1.0)) as f32
    }

    /// Unclamped lateness of the newest frame behind the content clock, in seconds (diagnostics: end-to-end catch-up).
    pub fn behind(&self, now: f64) -> f64 {
        let (Some(base), Some(id)) = (self.base, self.last_id) else { return 0.0 };
        now - (base + id as f64 * self.interval)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> f64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 >> 11) as f64 / (1u64 << 53) as f64
        }
    }

    const I: f64 = 1.0 / 60.0;

    /// Position error (in "frame intervals of motion") of what is displayed at each display tick, assuming perfect
    /// extrapolation of a constant-velocity object: shown content time minus the time we wanted to show.
    fn run(arrival_of: impl Fn(u16) -> Option<f64>, frames: u16, display_hz: f64, seed: u64) -> (Vec<f64>, Timeline) {
        let mut tl = Timeline::default();
        let mut errs = vec![];
        let mut rng = Rng(seed);
        let _ = rng.next();
        let mut arrivals: Vec<(f64, u16)> = (0..frames).filter_map(|k| arrival_of(k).map(|a| (a, k))).collect();
        arrivals.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        let mut next = 0;
        let end = arrivals.last().map_or(0.0, |a| a.0); // stop when the stream ends: nothing is expected after it
        let mut t = 0.0;
        let mut min_delay = f64::INFINITY;
        for (a, k) in &arrivals {
            min_delay = min_delay.min(a - f64::from(*k) * I);
        }
        while t < end {
            while next < arrivals.len() && arrivals[next].0 <= t {
                tl.on_frame(arrivals[next].1, arrivals[next].0);
                next += 1;
            }
            if let Some(last) = tl.last_id {
                // displayed content time = frame content time (true, id based) + alpha * interval; we want `t - min_delay`
                let shown = last as f64 * I + f64::from(tl.alpha(t)) * tl.interval();
                if tl.frames > 30 {
                    errs.push(shown - (t - min_delay));
                }
            }
            t += 1.0 / display_hz;
        }
        (errs, tl)
    }

    fn rms(v: &[f64]) -> f64 {
        (v.iter().map(|x| x * x).sum::<f64>() / v.len() as f64).sqrt()
    }

    /// Smooth mode: at every display tick the picture needs the next real frame once the content clock (held back by one interval and the
    /// buffer) passes the newest one; if that frame has not arrived the picture holds. Returns the longest hold in seconds and the total, from
    /// the moment the buffer has had time to settle, and the buffer at the end.
    fn holds(buffer: bool, arrival_of: impl Fn(u16) -> f64, frames: u16) -> (f64, f64, f64) {
        let mut tl = Timeline::default();
        tl.set_buffer(buffer);
        // frames arrive in order: a late one holds up the ones behind it
        let mut latest = 0.0f64;
        let arrivals: Vec<(f64, u16)> = (0..frames)
            .map(|k| {
                latest = latest.max(arrival_of(k));
                (latest, k)
            })
            .collect();
        let (mut next, mut t, mut hold_start, mut longest, mut total) = (0, 0.0, None::<f64>, 0.0f64, 0.0f64);
        let end = arrivals.last().unwrap().0;
        while t < end {
            while next < arrivals.len() && arrivals[next].0 <= t {
                tl.on_frame(arrivals[next].1, arrivals[next].0);
                next += 1;
            }
            if let (Some(id), true) = (tl.last_id, t > 8.0) {
                let t_cur = tl.content_time(id as u16).unwrap();
                let wanted = t - tl.interval() - tl.playout();
                if wanted > t_cur + 1e-9 {
                    let s = *hold_start.get_or_insert(t);
                    longest = longest.max(t - s);
                    total += 1.0 / 144.0;
                } else {
                    hold_start = None;
                }
            }
            t += 1.0 / 144.0;
        }
        (longest, total, tl.playout())
    }

    #[test]
    fn a_smoothing_buffer_turns_late_frames_into_no_hold_at_the_cost_of_a_bounded_delay() {
        // every 25th frame arrives 24 ms late, the others up to 2 ms: like the arrival gaps of a real stream
        let late = |k: u16| {
            let jitter = f64::from((u32::from(k) * 7919) % 5) * 0.0005;
            0.2 + f64::from(k) * I + jitter + if k % 25 == 7 { 0.024 } else { 0.0 }
        };
        let (longest_off, total_off, p_off) = holds(false, late, 2400);
        assert_eq!(p_off, 0.0);
        assert!(longest_off > 0.012, "without the buffer a late frame is a hold of its own lateness: {longest_off}");
        let (longest_on, total_on, p_on) = holds(true, late, 2400);
        assert!(
            longest_on < 0.004 && total_on < total_off * 0.2,
            "holds with the buffer: {longest_on} s, {total_on} against {total_off}"
        );
        assert!(p_on > 0.015 && p_on <= MAX_PLAYOUT, "the buffer covers the late frames: {p_on}");
    }

    #[test]
    fn the_smoothing_buffer_is_zero_on_a_clean_stream_and_never_above_its_limit() {
        let (_, _, p) = holds(true, |k| 0.2 + f64::from(k) * I + f64::from((u32::from(k) * 31) % 3) * 0.0002, 2400);
        assert!(p < 0.002, "a clean stream needs no buffer: {p}");
        // a very late frame now and then: capped
        let (_, _, p) = holds(true, |k| 0.2 + f64::from(k) * I + if k % 10 == 3 { 0.2 } else { 0.0 }, 2400);
        assert!(p <= MAX_PLAYOUT + 1e-12, "{p}");
        // off: always zero, and the setting survives a reset
        let mut tl = Timeline::default();
        tl.set_buffer(true);
        tl.reset();
        assert!(tl.buffer);
        tl.set_buffer(false);
        assert_eq!(tl.playout(), 0.0);
    }

    #[test]
    fn the_buffer_changes_slowly() {
        // from a clean stream to a jittery one: the buffer grows by at most 0.4 ms a frame, never a jump of the content
        let mut tl = Timeline::default();
        tl.set_buffer(true);
        let mut last = 0.0;
        let mut worst_step = 0.0f64;
        for k in 0..1200u16 {
            let jitter = if k > 600 && k % 4 == 0 { 0.02 } else { 0.0 };
            tl.on_frame(k, 0.2 + f64::from(k) * I + jitter);
            worst_step = worst_step.max((tl.playout() - last).abs());
            last = tl.playout();
        }
        assert!(worst_step <= PLAYOUT_UP + 1e-12, "{worst_step}");
        assert!(last > 0.01, "and it got there: {last}");
    }

    #[test]
    fn perfect_delivery_gives_a_sawtooth_alpha_and_no_content_error() {
        let (errs, tl) = run(|k| Some(0.2 + f64::from(k) * I), 600, 144.0, 1);
        assert!(rms(&errs) < 0.3e-3, "rms content error {:.3} ms", rms(&errs) * 1e3);
        assert!((tl.interval() - I).abs() < 1e-5);
        assert_eq!(tl.stats().gaps, 0);
    }

    #[test]
    fn jitter_is_absorbed_the_content_clock_stays_even() {
        let mut rng = Rng(7);
        let jit: Vec<f64> = (0..900).map(|_| rng.next() * 0.012).collect(); // up to 12 ms late, never early
        let (errs, tl) = run(|k| Some(0.2 + f64::from(k) * I + jit[k as usize]), 900, 144.0, 2);
        // compare with showing each frame at arrival (alpha measured from arrival): error ~ the jitter itself
        let mut tl2 = Timeline::default();
        let mut naive = vec![];
        let mut t = 0.0;
        let mut next = 0u16;
        let mut last_arr = 0.0;
        let mut last_id = 0u16;
        while t < 15.0 {
            while next < 900 && 0.2 + f64::from(next) * I + jit[next as usize] <= t {
                last_arr = 0.2 + f64::from(next) * I + jit[next as usize];
                last_id = next;
                tl2.on_frame(next, last_arr);
                next += 1;
            }
            if tl2.frames > 30 {
                let shown = f64::from(last_id) * I + ((t - last_arr) / I).clamp(0.0, 1.0) * I;
                naive.push(shown - (t - 0.2));
            }
            t += 1.0 / 144.0;
        }
        let (a, b) = (rms(&errs), rms(&naive));
        eprintln!(
            "content-time rms error: id-based clock {:.2} ms, arrival-based {:.2} ms; jitter estimate {:.2} ms",
            a * 1e3,
            b * 1e3,
            tl.stats().jitter * 1e3
        );
        assert!(a < b * 0.5, "the id-based clock must beat arrival-based pacing: {a} vs {b}");
        assert!(a < 3.0e-3, "residual error {:.2} ms", a * 1e3);
    }

    #[test]
    fn lost_frames_leave_gaps_in_ids_but_not_in_content_time() {
        let (errs, tl) = run(|k| (k % 20 != 3).then(|| 0.2 + f64::from(k) * I), 1000, 144.0, 3);
        // while a frame is missing the advance is capped at MAX_ALPHA intervals (deliberately: no inventing 33 ms of motion)
        assert!(tl.stats().gaps >= 45, "{:?}", tl.stats());
        assert!(rms(&errs) < 2.0e-3, "rms {:.3} ms", rms(&errs) * 1e3);
    }

    fn assert_never_backwards(errs: &[f64]) {
        // shown content time = error + wanted time; it must never go backwards (the error may fall by at most one tick per tick)
        let step = 1.0 / 144.0;
        for w in errs.windows(2) {
            assert!(w[1] - w[0] >= -step - 1e-9, "content time ran backwards: {} -> {}", w[0], w[1]);
        }
    }

    #[test]
    fn a_stall_followed_by_a_burst_never_moves_time_backwards_and_reconverges() {
        // everything from frame 100 is held for 400 ms, then released 2 ms apart until the schedule is caught up
        let release = 0.2 + 100.0 * I + 0.4;
        let (errs, tl) = run(
            |k| {
                let on_time = 0.2 + f64::from(k) * I;
                Some(if k < 100 { on_time } else { on_time.max(release + f64::from(k - 100) * 0.002) })
            },
            500,
            144.0,
            4,
        );
        assert_never_backwards(&errs);
        let tail = &errs[errs.len() - 100..];
        assert!(rms(tail) < 1.5e-3, "tail rms {:.2} ms", rms(tail) * 1e3);
        assert_eq!(tl.stats().gaps, 0, "nothing was lost, only delayed");
    }

    #[test]
    fn frames_that_arrive_after_newer_ones_are_dropped_not_shown_late() {
        // frames 100..130 take 400 ms longer than the frames after them (a reordering path)
        let (errs, tl) = run(
            |k| {
                let on_time = 0.2 + f64::from(k) * I;
                Some(if (100..130).contains(&k) { on_time + 0.4 } else { on_time })
            },
            400,
            144.0,
            4,
        );
        assert_never_backwards(&errs);
        assert!(tl.stats().gaps >= 20, "{:?}", tl.stats()); // the late frames that were overtaken are dropped
        assert!(rms(&errs[errs.len() - 100..]) < 1.0e-3);
    }

    #[test]
    fn ids_wrap_around_65535_cleanly() {
        let mut tl = Timeline::default();
        let mut id: u16 = 65500;
        for n in 0..200 {
            assert!(tl.on_frame(id, 1.0 + f64::from(n) * I), "frame {n} (id {id})");
            id = id.wrapping_add(1);
        }
        assert_eq!(tl.stats().gaps, 0);
        let c = tl.content_time(id.wrapping_sub(1)).unwrap();
        assert!((c - (1.0 + 199.0 * I)).abs() < 1e-3, "{c}");
    }

    #[test]
    fn duplicates_reordering_and_backwards_clocks_are_ignored() {
        let mut tl = Timeline::default();
        assert!(tl.on_frame(10, 1.0));
        assert!(!tl.on_frame(10, 1.02), "duplicate");
        assert!(!tl.on_frame(9, 1.03), "older id");
        assert!(!tl.on_frame(11, 0.5), "arrival time went backwards");
        assert!(!tl.on_frame(12, f64::NAN));
        assert!(!tl.on_frame(12, f64::INFINITY));
        assert!(tl.on_frame(11, 1.0167));
        assert_eq!(tl.stats().frames, 2);
    }

    #[test]
    fn a_thirty_fps_source_is_learned() {
        let (errs, tl) = run(|k| Some(0.2 + f64::from(k) * 2.0 * I), 300, 144.0, 5);
        assert!((tl.interval() - 2.0 * I).abs() < 2e-4, "interval {:.2} ms", tl.interval() * 1e3);
        let _ = errs;
    }

    #[test]
    fn a_restarted_stream_resets_instead_of_extrapolating_for_minutes() {
        let mut tl = Timeline::default();
        for k in 0..100u16 {
            tl.on_frame(k, 1.0 + f64::from(k) * I);
        }
        assert!(tl.on_frame(30000, 20.0)); // stream restarted with unrelated ids
        assert!(tl.alpha(20.001) < 0.2, "alpha {}", tl.alpha(20.001));
        assert!(tl.alpha(20.0 + 5.0) <= MAX_ALPHA as f32);
    }

    #[test]
    fn clock_rate_difference_between_console_and_display_is_tracked() {
        // the console runs 300 ppm fast relative to us: its frames arrive a little early each second
        let i_true = I * (1.0 - 300e-6);
        let mut tl = Timeline::default();
        let mut worst = 0.0f32;
        let mut t = 0.0;
        let mut next = 0u16;
        let mut seen = 0;
        while t < 60.0 {
            while f64::from(next) * i_true + 0.2 <= t {
                tl.on_frame(next, f64::from(next) * i_true + 0.2);
                next += 1;
            }
            if tl.stats().frames > 200 {
                worst = worst.max(tl.alpha(t));
                seen += 1;
            }
            t += 1.0 / 144.0;
        }
        assert!(seen > 8000);
        assert!(
            (tl.interval() - i_true).abs() < 2e-6,
            "learned {:.6} ms, true {:.6} ms",
            tl.interval() * 1e3,
            i_true * 1e3
        );
        assert!(worst < 1.15, "alpha crept up to {worst}: the clock is not following the console");
    }

    #[test]
    fn alpha_is_bounded_and_zero_before_the_first_frame() {
        let mut tl = Timeline::default();
        assert_eq!(tl.alpha(5.0), 0.0);
        tl.on_frame(0, 1.0);
        assert_eq!(tl.alpha(0.5), 0.0, "never negative");
        assert!(f64::from(tl.alpha(100.0)) <= MAX_ALPHA + 1e-6);
        assert!(tl.behind(100.0) > 90.0, "the raw lateness is still visible for diagnostics");
    }

    #[test]
    fn interpolation_holds_the_clock_back_by_exactly_one_interval() {
        let mut tl = Timeline::default();
        for k in 0..120u16 {
            tl.on_frame(k, 1.0 + f64::from(k) * I);
        }
        let arrival = 1.0 + 119.0 * I;
        // the moment frame 119 arrives it is not shown yet: the picture is still at frame 118
        assert!(tl.interp_beta(arrival, 118) < 0.02);
        // halfway to the next frame the picture is halfway between 118 and 119
        assert!((tl.interp_beta(arrival + 0.5 * I, 118) - 0.5).abs() < 0.02);
        // one interval later it has reached frame 119 (exactly when frame 120 would arrive)
        assert!(tl.interp_beta(arrival + I, 118) > 0.98);
        // so what is on screen is always one interval older than the newest frame: that is the latency cost
        assert_eq!(tl.interp_beta(arrival + 5.0, 118), 1.0);
        assert_eq!(tl.interp_beta(arrival, 119), 1.0, "nothing to interpolate");
        assert_eq!(Timeline::default().interp_beta(1.0, 0), 1.0);
    }

    #[test]
    fn hostile_sequences_never_panic() {
        let mut rng = Rng(99);
        let mut tl = Timeline::default();
        let mut t = 0.0;
        for _ in 0..50_000 {
            t += rng.next() * 0.05 - 0.005;
            let id = (rng.next() * 65536.0) as u16;
            let _ = tl.on_frame(id, if rng.next() < 0.01 { f64::NAN } else { t });
            let _ = tl.alpha(t + rng.next());
            let _ = tl.content_time(id);
        }
    }

    /// Regression: the first frames of a stream arrive in a catch-up burst (decoder warm-up). The clock used to learn a 10 ms
    /// interval from it, think every later frame was 40-80 ms late, pin the extrapolation at its maximum and take seconds to recover.
    #[test]
    fn a_startup_catch_up_burst_does_not_poison_the_clock() {
        // the decoder delivers its first frame 300 ms late, then catches up at one frame per 7 ms (what the real trace showed:
        // ~40 frames); once caught up the stream is steady 60 fps
        let arrival = |k: u16| {
            let ideal = 0.2 + f64::from(k) * I;
            Some((0.5 + f64::from(k) * 0.007).max(ideal))
        };
        let (errs, tl) = run(arrival, 400, 144.0, 3);
        assert!((tl.interval() - I).abs() < 0.5e-3, "interval {:.2} ms", tl.interval() * 1e3);
        // judge the first second after the burst, when the old code was still wrong
        let early = &errs[..errs.len().min(150)];
        assert!(rms(early) < 3e-3, "rms content error right after start {:.2} ms", rms(early) * 1e3);
    }

    #[test]
    fn warm_up_catches_a_late_first_frame_quickly() {
        // only the first frame is 80 ms late; with a slew-limited base the clock would stay wrong for ~1300 frames
        let (errs, tl) = run(|k| Some(0.2 + f64::from(k) * I + if k == 0 { 0.08 } else { 0.0 }), 300, 144.0, 4);
        assert!(
            tl.behind(0.2 + 298.0 * I) < 2e-3,
            "behind {:.2} ms, interval {:.3} ms",
            tl.behind(0.2 + 298.0 * I) * 1e3,
            tl.interval() * 1e3
        );
        assert!(rms(&errs) < 2e-3, "rms {:.2} ms", rms(&errs) * 1e3);
    }

    /// Replays recorded arrival traces (`ARR <id> <seconds>` lines in $OPENPS_TRACES/*.txt) through the clock and measures how far
    /// its idea of lateness is from the stream's settled one during the first frames. Positive = frames believed later than they are =
    /// extrapolation too far. Run by hand: OPENPS_TRACES=<dir> cargo test -p openps-metal --lib replay -- --nocapture
    #[test]
    fn replay_recorded_traces() {
        let Ok(dir) = std::env::var("OPENPS_TRACES") else { return };
        let mut files: Vec<_> = std::fs::read_dir(dir).unwrap().filter_map(|e| e.ok()).map(|e| e.path()).collect();
        files.sort();
        for (name, warm) in [("old (no warm-up handling)", 0u64), ("new", WARMUP)] {
            let (mut sum_abs, mut sum_pos, mut n, mut worst) = (0.0, 0.0, 0.0, 0.0f64);
            let mut over = 0u32;
            for f in &files {
                let arr: Vec<(u16, f64)> = std::fs::read_to_string(f)
                    .unwrap()
                    .lines()
                    .filter_map(|l| {
                        let mut p = l.split_whitespace();
                        (p.next()? == "ARR").then_some(())?;
                        Some((p.next()?.parse().ok()?, p.next()?.parse().ok()?))
                    })
                    .collect();
                let ideal =
                    arr.iter().filter(|a| a.0 >= 60).map(|(k, a)| a - f64::from(*k) * I).fold(f64::INFINITY, f64::min);
                let mut tl = Timeline { warmup: warm, ..Timeline::default() };
                for (idx, (id, a)) in arr.iter().enumerate() {
                    if !tl.on_frame(*id, *a) || idx >= 120 {
                        continue;
                    }
                    let believed = tl.behind(*a);
                    let settled = a - (ideal + f64::from(*id) * I);
                    let d = believed - settled;
                    sum_abs += d.abs();
                    if d > 0.0 {
                        sum_pos += d;
                    }
                    if d > 0.010 {
                        over += 1;
                    }
                    worst = worst.max(d);
                    n += 1.0;
                }
            }
            println!(
                "{name:<26} mean |error| {:5.1} ms   mean too-far {:5.1} ms   worst {:6.1} ms   frames >10 ms too far: {over} of {n}",
                sum_abs / n * 1e3,
                sum_pos / n * 1e3,
                worst * 1e3
            );
        }
    }
}
