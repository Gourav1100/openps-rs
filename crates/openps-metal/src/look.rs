//! What the user has told the console to do with the camera, with timestamps: the right-stick values we *sent*, held
//! between samples. The camera compensator integrates this to know how far the view has been commanded to turn since the
//! newest frame was captured.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};

/// Seconds of history kept.
const KEEP_S: f64 = 6.0;
/// Hard cap on samples (a 1 kHz producer for the whole window).
const MAX_SAMPLES: usize = 8192;

#[derive(Debug, Default, Clone)]
pub struct LookHistory {
    /// (time, x, y): from this time on the stick is at (x, y) until the next sample.
    samples: VecDeque<(f64, f32, f32)>,
}

impl LookHistory {
    pub fn push(&mut self, t: f64, x: f32, y: f32) {
        if !t.is_finite() || !x.is_finite() || !y.is_finite() {
            return;
        }
        if self.samples.back().is_some_and(|s| t < s.0) {
            return; // time never runs backwards
        }
        let (x, y) = (x.clamp(-1.0, 1.0), y.clamp(-1.0, 1.0));
        // unchanged values add nothing: keep the history short
        if self.samples.back().is_some_and(|s| s.1 == x && s.2 == y) {
            return;
        }
        self.samples.push_back((t, x, y));
        while self.samples.len() > MAX_SAMPLES
            || self.samples.front().is_some_and(|s| t - s.0 > KEEP_S) && self.samples.len() > 1
        {
            self.samples.pop_front();
        }
    }

    /// Stick value at time `t` (zero before the first sample).
    pub fn at(&self, t: f64) -> (f32, f32) {
        match self.samples.partition_point(|s| s.0 <= t) {
            0 => (0.0, 0.0),
            n => (self.samples[n - 1].1, self.samples[n - 1].2),
        }
    }

    /// Integral of the stick over `[t0, t1]` (stick-units times seconds). Zero for an empty or reversed range.
    pub fn integral(&self, t0: f64, t1: f64) -> (f64, f64) {
        if t1.partial_cmp(&t0) != Some(std::cmp::Ordering::Greater) || !t0.is_finite() || !t1.is_finite() {
            return (0.0, 0.0);
        }
        let mut sx = 0.0;
        let mut sy = 0.0;
        let first = self.samples.partition_point(|s| s.0 <= t0);
        let (mut cx, mut cy) = if first == 0 {
            (0.0, 0.0)
        } else {
            (f64::from(self.samples[first - 1].1), f64::from(self.samples[first - 1].2))
        };
        let mut cur = t0;
        for s in self.samples.iter().skip(first) {
            if s.0 >= t1 {
                break;
            }
            sx += cx * (s.0 - cur);
            sy += cy * (s.0 - cur);
            cur = s.0;
            cx = f64::from(s.1);
            cy = f64::from(s.2);
        }
        sx += cx * (t1 - cur);
        sy += cy * (t1 - cur);
        (sx, sy)
    }

    pub fn len(&self) -> usize {
        self.samples.len()
    }
    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }
}

/// Shared between the input thread (writes) and the renderer (reads).
#[derive(Clone, Default)]
pub struct SharedLook(Arc<Mutex<LookHistory>>);

impl SharedLook {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn push(&self, t: f64, x: f32, y: f32) {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).push(t, x, y);
    }
    pub fn integral(&self, t0: f64, t1: f64) -> (f64, f64) {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).integral(t0, t1)
    }
    pub fn at(&self, t: f64) -> (f32, f32) {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).at(t)
    }
    /// A private copy, so a long computation does not hold the lock the input thread needs.
    pub fn snapshot(&self) -> LookHistory {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
    pub fn clear(&self) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = LookHistory::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integral_of_a_piecewise_constant_stick() {
        let mut h = LookHistory::default();
        h.push(1.0, 0.5, 0.0);
        h.push(2.0, -1.0, 0.25);
        h.push(3.0, 0.0, 0.0);
        let close = |a: (f64, f64), b: (f64, f64)| (a.0 - b.0).abs() < 1e-9 && (a.1 - b.1).abs() < 1e-9;
        assert!(close(h.integral(0.0, 1.0), (0.0, 0.0)), "before the first sample the stick is neutral");
        assert!(close(h.integral(1.0, 2.0), (0.5, 0.0)));
        assert!(close(h.integral(1.5, 2.5), (0.25 - 0.5, 0.125)));
        assert!(close(h.integral(0.0, 10.0), (0.5 - 1.0, 0.25)));
        assert!(close(h.integral(2.2, 2.4), (-0.2, 0.05)));
        assert!(close(h.integral(3.0, 9.0), (0.0, 0.0)));
        assert_eq!(h.at(0.5), (0.0, 0.0));
        assert_eq!(h.at(1.0), (0.5, 0.0));
        assert_eq!(h.at(2.5), (-1.0, 0.25));
    }

    #[test]
    fn bad_input_is_ignored_and_the_history_stays_bounded() {
        let mut h = LookHistory::default();
        h.push(f64::NAN, 0.0, 0.0);
        h.push(1.0, f32::NAN, 0.0);
        h.push(1.0, 0.0, f32::INFINITY);
        assert!(h.is_empty());
        h.push(2.0, 5.0, -9.0); // clamped to the stick's range
        assert_eq!(h.at(2.0), (1.0, -1.0));
        h.push(1.0, 0.1, 0.1); // time going backwards
        assert_eq!(h.len(), 1);
        for k in 0..50_000 {
            h.push(3.0 + f64::from(k) * 0.001, if k % 2 == 0 { 0.2 } else { -0.2 }, 0.0);
        }
        assert!(h.len() <= MAX_SAMPLES);
        // old history is dropped but recent integrals still work
        let i = h.integral(50.0, 51.0);
        assert!(i.0.abs() < 0.01, "{i:?}");
        assert_eq!(h.integral(5.0, 4.0), (0.0, 0.0));
        assert_eq!(h.integral(f64::NAN, 4.0), (0.0, 0.0));
    }

    #[test]
    fn unchanged_values_do_not_grow_the_history() {
        let mut h = LookHistory::default();
        for k in 0..1000 {
            h.push(f64::from(k) * 0.004, 0.3, 0.0);
        }
        assert_eq!(h.len(), 1);
        assert!((h.integral(0.0, 4.0).0 - 1.2).abs() < 1e-3 + 0.3 * 0.004);
    }

    #[test]
    fn shared_handle_works_across_threads() {
        let s = SharedLook::new();
        let s2 = s.clone();
        let t = std::thread::spawn(move || {
            for k in 0..2000 {
                s2.push(f64::from(k) * 0.001, (k % 3) as f32 * 0.3, 0.0);
            }
        });
        for _ in 0..2000 {
            let _ = s.integral(0.0, 2.0);
        }
        t.join().unwrap();
        assert!(s.integral(0.0, 2.0).0 > 0.0);
        s.clear();
        assert_eq!(s.integral(0.0, 2.0), (0.0, 0.0));
    }
}
