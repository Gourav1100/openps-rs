//! Camera latency compensation.
//!
//! The console only shows what the camera did after the network, the game, the encoder and the decoder have all had their
//! turn: tens of milliseconds after you moved the mouse. But the client knows the mouse input *now*. If the view is being
//! turned by the stick, the newest frame can be shifted by the rotation that was commanded but is not in it yet; that is
//! what a VR headset's timewarp and NVIDIA's Reflex 2 frame warp do, here for a remote stream.
//!
//! Two numbers are needed and both are learned while playing, from nothing but the stream and our own input:
//! * the **lag** `L`: how long after a stick movement its effect appears in a frame,
//! * the **gain** `g`: pixels of image motion per unit of stick per second.
//!
//! For every real frame the renderer measures the global image motion `m_k` between frames `k-1` and `k`. If the stick drives
//! the camera, `m_k = g * integral(stick, T_{k-1} - L, T_k - L)`. We search `L` for the best fit and require that the stick
//! really explains the motion; when it does not (a game that uses this stick for something else, a cutscene, a menu) the
//! compensation switches itself off.

use crate::look::{LookHistory, SharedLook};
use std::collections::VecDeque;

/// Real frames of observations used for the fit (2.5 s at 60 fps).
const MAX_OBS: usize = 150;
/// Need this many before the first fit.
const MIN_OBS: usize = 40;
/// Refit every this many new observations.
const REFIT_EVERY: usize = 6;
/// Lags searched (seconds).
const LAG_MIN: f64 = 0.0;
const LAG_MAX: f64 = 0.40;
const LAG_STEP: f64 = 0.0025;
/// Fraction of the image motion the stick must explain.
pub const MIN_EXPLAINED: f64 = 0.60;
/// Below this the estimate is dropped altogether.
const DROP_BELOW: f64 = 0.35;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Estimate {
    /// Seconds from a stick movement to its first appearance in a frame (on the content clock).
    pub lag: f64,
    /// Image pixels per stick-unit-second, per axis.
    pub gain: (f64, f64),
    /// Fraction of the measured image motion explained by the stick, 0...1.
    pub explained: f64,
}

/// (score, lag, sum of m*I per axis, sum of I*I per axis)
type Fit = (f64, f64, (f64, f64), (f64, f64));

#[derive(Clone, Copy, Debug)]
struct Obs {
    t_prev: f64,
    t: f64,
    m: (f64, f64),
}

pub struct CameraComp {
    look: SharedLook,
    obs: VecDeque<Obs>,
    est: Option<Estimate>,
    since_fit: usize,
    last_m: (f64, f64),
}

impl CameraComp {
    pub fn new(look: SharedLook) -> Self {
        Self { look, obs: VecDeque::new(), est: None, since_fit: 0, last_m: (0.0, 0.0) }
    }

    pub fn estimate(&self) -> Option<Estimate> {
        self.est
    }

    pub fn reset(&mut self) {
        self.obs.clear();
        self.est = None;
        self.since_fit = 0;
        self.last_m = (0.0, 0.0);
    }

    /// The image moved by `m` pixels between the content times `t_prev` and `t` (both on the content clock).
    pub fn observe(&mut self, t_prev: f64, t: f64, m: (f32, f32)) {
        if t.partial_cmp(&t_prev) != Some(std::cmp::Ordering::Greater)
            || !t.is_finite()
            || !t_prev.is_finite()
            || !m.0.is_finite()
            || !m.1.is_finite()
        {
            return;
        }
        let m = (f64::from(m.0), f64::from(m.1));
        self.last_m = m;
        self.obs.push_back(Obs { t_prev, t, m });
        while self.obs.len() > MAX_OBS {
            self.obs.pop_front();
        }
        self.since_fit += 1;
        if self.obs.len() >= MIN_OBS && self.since_fit >= REFIT_EVERY {
            self.since_fit = 0;
            self.fit();
        }
    }

    /// The most recent measured global motion (pixels per source frame), for the extrapolator to leave to us.
    pub fn recent_motion(&self) -> (f32, f32) {
        (self.last_m.0 as f32, self.last_m.1 as f32)
    }

    fn fit(&mut self) {
        let hist: LookHistory = self.look.snapshot();
        if hist.is_empty() {
            self.decay();
            return;
        }
        let (mut mm_x, mut mm_y) = (0.0, 0.0);
        for o in &self.obs {
            mm_x += o.m.0 * o.m.0;
            mm_y += o.m.1 * o.m.1;
        }
        let mm = mm_x + mm_y;
        // no image motion at all (a still scene): nothing to learn, nothing to lose; keep what we know
        if mm < 1.0 {
            return;
        }
        let steps = ((LAG_MAX - LAG_MIN) / LAG_STEP).round() as usize;
        let mut best: Option<Fit> = None;
        let mut scores = Vec::with_capacity(steps + 1);
        for i in 0..=steps {
            let lag = LAG_MIN + i as f64 * LAG_STEP;
            let (mut sxy_x, mut sxx_x, mut sxy_y, mut sxx_y) = (0.0, 0.0, 0.0, 0.0);
            for o in &self.obs {
                let (ix, iy) = hist.integral(o.t_prev - lag, o.t - lag);
                sxy_x += o.m.0 * ix;
                sxx_x += ix * ix;
                sxy_y += o.m.1 * iy;
                sxx_y += iy * iy;
            }
            let ex = if sxx_x > 1e-9 { sxy_x * sxy_x / sxx_x } else { 0.0 };
            let ey = if sxx_y > 1e-9 { sxy_y * sxy_y / sxx_y } else { 0.0 };
            let score = (ex + ey) / mm;
            scores.push(score);
            if best.is_none_or(|b| score > b.0) {
                best = Some((score, lag, (sxy_x, sxy_y), (sxx_x, sxx_y)));
            }
        }
        let Some((score, mut lag, sxy, sxx)) = best else { return };
        // parabolic refinement between grid points
        if let Some(k) = scores.iter().position(|s| *s == score).filter(|k| *k > 0 && *k + 1 < scores.len()) {
            let (a, b, c) = (scores[k - 1], scores[k], scores[k + 1]);
            let d = a - 2.0 * b + c;
            if d.abs() > 1e-12 {
                lag += (0.5 * (a - c) / d).clamp(-1.0, 1.0) * LAG_STEP;
            }
        }
        // a flat score curve means the lag is not identifiable (e.g. a constant stick): do not trust it
        let spread = scores.iter().cloned().fold(f64::INFINITY, f64::min);
        let identifiable = score - spread > 0.15;
        if score < MIN_EXPLAINED || !identifiable {
            self.decay();
            return;
        }
        let gain = (if sxx.0 > 1e-6 { sxy.0 / sxx.0 } else { 0.0 }, if sxx.1 > 1e-6 { sxy.1 / sxx.1 } else { 0.0 });
        self.est = Some(match self.est {
            None => Estimate { lag, gain, explained: score },
            Some(e) => {
                let w = 0.35;
                Estimate {
                    lag: e.lag + (lag - e.lag) * w,
                    gain: (e.gain.0 + (gain.0 - e.gain.0) * w, e.gain.1 + (gain.1 - e.gain.1) * w),
                    explained: e.explained + (score - e.explained) * 0.5,
                }
            }
        });
    }

    fn decay(&mut self) {
        if let Some(e) = &mut self.est {
            e.explained *= 0.8;
            if e.explained < DROP_BELOW {
                self.est = None;
            }
        }
    }

    /// The rotation (image pixels) that has been commanded since the newest frame's content time minus the lag, and so is
    /// missing from that frame at display time `now`. `None` when the stick does not (yet) demonstrably drive the camera.
    pub fn shift(&self, frame_content_time: f64, now: f64) -> Option<(f32, f32)> {
        let e = self.est.filter(|e| e.explained >= MIN_EXPLAINED * 0.85)?;
        let (ix, iy) = self.look.integral(frame_content_time - e.lag, now);
        Some(((e.gain.0 * ix) as f32, (e.gain.1 * iy) as f32))
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
        fn gauss(&mut self) -> f64 {
            let (a, b) = (self.next().max(1e-12), self.next());
            (-2.0 * a.ln()).sqrt() * (2.0 * std::f64::consts::PI * b).cos()
        }
    }

    /// A stick that moves like a hand on a mouse: bursts of movement and pauses.
    fn stick_signal(rng: &mut Rng, dur: f64) -> Vec<(f64, f32, f32)> {
        let mut v = vec![];
        let mut t = 0.0;
        while t < dur {
            let len = 0.08 + rng.next() * 0.5;
            let (x, y) =
                if rng.next() < 0.35 { (0.0, 0.0) } else { (rng.next() * 2.0 - 1.0, (rng.next() * 2.0 - 1.0) * 0.5) };
            let mut s = t;
            while s < t + len {
                v.push((s, x as f32, y as f32));
                s += 0.004; // the engine ticks at 250 Hz
            }
            t += len;
        }
        v
    }

    struct World {
        cam: CameraComp,
        /// zero-latency reference: camera driven by the inputs with no delay
        instant: LookHistory,
        delayed_gain: (f64, f64),
        lag: f64,
    }

    const I: f64 = 1.0 / 60.0;

    /// Run a closed loop for `dur` seconds. `gain_at(t)` lets the gain change mid-run.
    fn run(seed: u64, gain: impl Fn(f64) -> (f64, f64), lag: f64, dur: f64, noise: f64, outliers: f64) -> World {
        let mut rng = Rng(seed);
        let sig = stick_signal(&mut rng, dur + 1.0);
        let sent = SharedLook::new();
        let mut instant = LookHistory::default();
        for &(t, x, y) in &sig {
            sent.push(t, x, y);
            instant.push(t, x, y);
        }
        let mut cam = CameraComp::new(sent.clone());
        let base = 0.3;
        let mut k = 0;
        while base + f64::from(k) * I < dur {
            let (t0, t1) = (base + f64::from(k - 1) * I, base + f64::from(k) * I);
            if k > 0 {
                // the console's camera applied the stick `lag` ago, with the gain valid at that moment
                let g = gain(t1);
                let (ix, iy) = instant.integral(t0 - lag, t1 - lag);
                let mut m = (g.0 * ix + rng.gauss() * noise, g.1 * iy + rng.gauss() * noise);
                if rng.next() < outliers {
                    m = (m.0 + (rng.next() - 0.5) * 30.0, m.1 + (rng.next() - 0.5) * 30.0);
                    // a moving object skews the measurement
                }
                cam.observe(t0, t1, (m.0 as f32, m.1 as f32));
            }
            k += 1;
        }
        World { cam, instant, delayed_gain: gain(dur), lag }
    }

    /// RMS error (pixels) of what is displayed, relative to a zero-latency camera, for three strategies, over `[t0, t1)`.
    fn evaluate(w: &World, t0: f64, t1: f64) -> (f64, f64, f64) {
        let (mut none, mut extrap, mut comp, mut n) = (0.0, 0.0, 0.0, 0.0);
        let mut t = t0;
        while t < t1 {
            // newest frame at display time t, and the global motion measured for it
            let k = ((t - 0.3) / I).floor();
            let tk = 0.3 + k * I;
            let (tp, _) = (tk - I, 0);
            let g = w.delayed_gain;
            // what the frame shows vs what zero latency would show
            let (nx, ny) = w.instant.integral(tk - w.lag, t);
            let needed = (g.0 * nx, g.1 * ny);
            // extrapolation: continue the last measured motion for alpha intervals
            let (mx, my) = w.instant.integral(tp - w.lag, tk - w.lag);
            let alpha = (t - tk) / I;
            let ex = (g.0 * mx * alpha, g.1 * my * alpha);
            let c = w.cam.shift(tk, t).map_or((0.0, 0.0), |s| (f64::from(s.0), f64::from(s.1)));
            none += needed.0.powi(2) + needed.1.powi(2);
            extrap += (needed.0 - ex.0).powi(2) + (needed.1 - ex.1).powi(2);
            comp += (needed.0 - c.0).powi(2) + (needed.1 - c.1).powi(2);
            n += 2.0;
            t += 1.0 / 144.0;
        }
        ((none / n).sqrt(), (extrap / n).sqrt(), (comp / n).sqrt())
    }

    #[test]
    fn it_learns_the_lag_and_gain_and_cuts_the_visible_lag_by_most() {
        for (seed, lag, gain) in [
            (1u64, 0.095, (900.0, 600.0)),
            (2, 0.060, (1400.0, 1000.0)),
            (3, 0.150, (500.0, 350.0)),
            (4, 0.210, (1100.0, 800.0)),
        ] {
            let w = run(seed, |_| gain, lag, 14.0, 0.4, 0.02);
            let e = w.cam.estimate().unwrap_or_else(|| panic!("no estimate for lag {lag}"));
            eprintln!(
                "true lag {:.0} ms gain {:?} -> learned lag {:.1} ms gain ({:.0}, {:.0}) explained {:.2}",
                lag * 1e3,
                gain,
                e.lag * 1e3,
                e.gain.0,
                e.gain.1,
                e.explained
            );
            assert!((e.lag - lag).abs() < 0.010, "lag {:.1} ms vs {:.1} ms", e.lag * 1e3, lag * 1e3);
            assert!(
                (e.gain.0 / gain.0 - 1.0).abs() < 0.08 && (e.gain.1 / gain.1 - 1.0).abs() < 0.10,
                "gain {:?} vs {gain:?}",
                e.gain
            );
            let (none, extrap, comp) = evaluate(&w, 8.0, 13.5);
            eprintln!("  displayed error vs zero latency (px rms): repeat {none:.1}, extrapolate {extrap:.1}, camera comp {comp:.1}");
            assert!(comp < extrap * 0.30, "compensated {comp:.2} should be far below extrapolation {extrap:.2}");
            assert!(comp < none * 0.25);
        }
    }

    #[test]
    fn it_stays_off_when_the_stick_does_not_drive_the_camera() {
        // the image moves (autonomously), the stick moves too, but independently
        let mut rng = Rng(11);
        let sig = stick_signal(&mut rng, 15.0);
        let sent = SharedLook::new();
        for &(t, x, y) in &sig {
            sent.push(t, x, y);
        }
        let mut cam = CameraComp::new(sent.clone());
        let mut other = Rng(12);
        let sig2 = stick_signal(&mut other, 15.0);
        let mut h2 = LookHistory::default();
        for &(t, x, y) in &sig2 {
            h2.push(t, x, y);
        }
        for k in 1..800 {
            let (t0, t1) = (0.3 + f64::from(k - 1) * I, 0.3 + f64::from(k) * I);
            let (ix, iy) = h2.integral(t0, t1);
            cam.observe(t0, t1, ((800.0 * ix) as f32, (600.0 * iy) as f32));
        }
        assert!(cam.estimate().is_none(), "unrelated motion must not enable compensation: {:?}", cam.estimate());
        assert!(cam.shift(5.0, 5.05).is_none());
    }

    #[test]
    fn it_stays_off_without_input_or_without_motion() {
        let sent = SharedLook::new();
        let mut cam = CameraComp::new(sent.clone());
        for k in 1..200 {
            cam.observe(f64::from(k - 1) * I, f64::from(k) * I, (3.0, 0.0)); // a cutscene pans, no stick history at all
        }
        assert!(cam.estimate().is_none());
        for t in 0..400 {
            sent.push(f64::from(t) * 0.01, (t % 7) as f32 / 7.0, 0.0); // stick moves, the image is perfectly still
        }
        for k in 200..400 {
            cam.observe(f64::from(k - 1) * I, f64::from(k) * I, (0.0, 0.0));
        }
        assert!(cam.estimate().is_none());
        assert!(cam.shift(1.0, 1.1).is_none());
    }

    #[test]
    fn a_constant_stick_cannot_identify_the_lag_and_is_not_trusted() {
        let sent = SharedLook::new();
        sent.push(0.0, 0.6, 0.0);
        let mut cam = CameraComp::new(sent);
        for k in 1..300 {
            cam.observe(f64::from(k - 1) * I, f64::from(k) * I, (9.0, 0.0)); // steady rotation
        }
        assert!(cam.estimate().is_none(), "{:?}", cam.estimate());
    }

    #[test]
    fn it_readapts_when_the_gain_changes() {
        // the player zooms (narrower field of view): the same stick turns the image half as many pixels
        let w = run(5, |t| if t < 8.0 { (900.0, 600.0) } else { (450.0, 300.0) }, 0.1, 15.0, 0.4, 0.02);
        let e = w.cam.estimate().expect("estimate");
        assert!((e.gain.0 / 450.0 - 1.0).abs() < 0.12, "gain {:?} after the change", e.gain);
    }

    #[test]
    fn heavy_noise_degrades_gracefully_not_dangerously() {
        let w = run(6, |_| (900.0, 600.0), 0.1, 14.0, 3.0, 0.10);
        // switching off (None) is the safe answer; a wrong estimate would be worse than none
        if let Some(e) = w.cam.estimate() {
            assert!((e.lag - 0.1).abs() < 0.03 && (e.gain.0 / 900.0 - 1.0).abs() < 0.2, "{e:?}");
        }
    }

    #[test]
    fn nonsense_observations_are_ignored() {
        let mut cam = CameraComp::new(SharedLook::new());
        cam.observe(1.0, 0.5, (1.0, 1.0));
        cam.observe(f64::NAN, 1.0, (1.0, 1.0));
        cam.observe(0.0, 1.0, (f32::NAN, 1.0));
        cam.observe(0.0, 1.0, (f32::INFINITY, 1.0));
        assert!(cam.obs.is_empty());
        cam.reset();
        assert!(cam.estimate().is_none());
    }
}
