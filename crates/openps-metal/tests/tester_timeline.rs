//! Tester A5: black-box tests of the content clock (`Timeline`), the rate scheduler (`effective_rate`), the camera estimator and
//! the look history with hostile / extreme inputs. No GPU needed.
#![cfg(target_os = "macos")]
use openps_metal::camera::CameraComp;
use openps_metal::effective_rate;
use openps_metal::look::{LookHistory, SharedLook};
use openps_metal::timeline::{Timeline, MAX_ALPHA};
use std::time::Instant;

const I: f64 = 1.0 / 60.0;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
}

fn steady(tl: &mut Timeline, first: u16, n: u16, t0: f64) -> f64 {
    let mut t = t0;
    for k in 0..n {
        assert!(tl.on_frame(first.wrapping_add(k), t), "frame {k} refused");
        t += I;
    }
    t
}

#[test]
fn alpha_content_time_and_beta_stay_finite_and_bounded_for_any_finite_input() {
    let mut rng = Rng(0xA5);
    let mut tl = Timeline::default();
    let mut t = 0.0;
    for step in 0..100_000 {
        // arrival gaps from 0 (burst) to 3 s, ids from the whole u16 range
        t += match step % 7 {
            0 => 0.0,
            1 => rng.next() * 3.0,
            _ => rng.next() * 0.03,
        };
        let id = (rng.next() * 65536.0) as u16;
        let _ = tl.on_frame(id, t);
        let now = t + (rng.next() - 0.5) * 1e6;
        let a = tl.alpha(now);
        assert!(a.is_finite() && (0.0..=MAX_ALPHA as f32).contains(&a), "alpha {a}");
        let b = tl.interp_beta(now, (rng.next() * 65536.0) as u16);
        assert!(b.is_finite() && (0.0..=1.0).contains(&b), "beta {b}");
        if let Some(c) = tl.content_time(id) {
            assert!(c.is_finite());
        }
        let s = tl.stats();
        assert!(
            s.interval.is_finite() && s.interval > 1.0 / 401.0 && s.interval < 1.0 / 4.9,
            "interval {}",
            s.interval
        );
        assert!(s.queue_delay.is_finite() && s.jitter.is_finite());
    }
}

#[test]
fn extreme_arrival_values_do_not_panic() {
    for arr in [0.0, -1.0, 1e-300, 1e300, f64::MAX, f64::MIN_POSITIVE, -0.0] {
        let mut tl = Timeline::default();
        for k in 0..200u16 {
            let _ = tl.on_frame(k, arr + f64::from(k) * I);
            let _ = tl.alpha(arr);
        }
    }
    // non-finite arrival times are refused and change nothing
    let mut tl = Timeline::default();
    assert!(tl.on_frame(1, 1.0));
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(!tl.on_frame(2, bad));
    }
    assert_eq!(tl.stats().frames, 1);
}

#[test]
fn the_restart_threshold_is_600_ids() {
    let mut tl = Timeline::default();
    steady(&mut tl, 100, 100, 1.0); // ids 100..199
    assert!(tl.on_frame(199 + 600, 20.0)); // exactly 600 ahead: continuation, 599 lost
    assert_eq!(tl.stats().gaps, 599);
    assert!(tl.stats().frames > 100);
    let mut tl2 = Timeline::default();
    steady(&mut tl2, 100, 100, 1.0);
    assert!(tl2.on_frame(199 + 601, 20.0)); // 601: restart
    assert_eq!(tl2.stats().frames, 1);
    assert_eq!(tl2.stats().gaps, 0);
}

#[test]
fn ids_wrap_at_65536_without_a_hiccup_for_ten_minutes() {
    let mut tl = Timeline::default();
    let mut t = 0.5;
    for k in 0..40_000u32 {
        assert!(tl.on_frame(k as u16, t), "frame {k}");
        t += I;
    }
    assert_eq!(tl.stats().gaps, 0);
    assert!(tl.alpha(t - I) < 0.5);
}

#[test]
fn the_timeline_state_stays_bounded_over_a_very_long_session() {
    // 3 hours of 60 fps: nothing may grow (the window is capped) and nothing may drift into NaN
    let mut tl = Timeline::default();
    let t0 = Instant::now();
    let mut t = 0.5;
    for k in 0..648_000u32 {
        assert!(tl.on_frame(k as u16, t));
        t += I;
    }
    let s = tl.stats();
    assert!((s.interval - I).abs() < 1e-6, "{}", s.interval);
    assert_eq!(s.gaps, 0);
    eprintln!("648k frames in {:?}", t0.elapsed());
}

/// After a multi-second delivery stall with consecutive ids (decoder stall, App Nap), how long does the content clock pin the
/// extrapolation at its maximum? (characterisation: prints the number; asserts it recovers at all)
#[test]
fn a_five_second_stall_with_consecutive_ids_recovers() {
    let mut tl = Timeline::default();
    let mut t = steady(&mut tl, 0, 300, 1.0);
    t += 5.0; // everything stops for 5 s, but the console numbering simply continues
    let mut pinned = 0;
    for k in 0..400u16 {
        assert!(tl.on_frame(300 + k, t));
        if tl.alpha(t) >= (MAX_ALPHA as f32) - 1e-3 {
            pinned += 1;
        }
        t += I;
    }
    eprintln!("frames with alpha pinned at MAX after a 5 s stall: {pinned} of 400");
    assert!(pinned < 400, "never recovers");
}

/// BUG T09-02: a permanent step in arrival delay (here +300 ms with consecutive ids, e.g. a path change or bufferbloat that stays) makes
/// the interval estimator swing by 17 % (80 ms step: 4 %, 1 s: 56 %, 5 s: 278 %) for about 1.5 s, and then the base slews back at only
/// 60 us/frame, so the renderer shows the maximum extrapolation (alpha 1.25) for ~9 s. `source_fps`/`interval` reported in the HUD
/// jump too. Expected: the interval is a property of the console and must not follow a delay step.
#[test]
fn a_delay_step_does_not_disturb_the_interval_estimate() {
    let mut tl = Timeline::default();
    let mut t = steady(&mut tl, 0, 300, 1.0);
    t += 0.3;
    let mut worst = 0.0f64;
    for k in 0..400u16 {
        assert!(tl.on_frame(300 + k, t));
        worst = worst.max((tl.interval() - I).abs() / I);
        t += I;
    }
    assert!(worst < 0.05, "interval deviated by {:.1} %", worst * 100.0);
}

/// BUG T09-01: a frame-id counter that restarts *backwards* (an encoder or console that resets its frame counter mid-session
/// without a new session) is treated as an unbounded run of reordered frames and refused for up to 32767 frames (9 minutes at 60 fps).
/// Forward jumps over 600 restart the clock; backward jumps never do. The renderer shows nothing new for the whole period.
#[test]
fn a_backwards_frame_id_restart_is_recovered_from() {
    let mut tl = Timeline::default();
    let mut t = steady(&mut tl, 5000, 200, 1.0);
    let before = tl.stats().frames;
    let mut accepted = 0;
    for k in 0..120u16 {
        if tl.on_frame(k, t) {
            accepted += 1;
        }
        t += I;
    }
    assert!(
        accepted > 100,
        "only {accepted} of 120 frames after the id restart were accepted (frames before: {before})"
    );
}

#[test]
fn nan_interval_in_the_constructor_does_not_panic() {
    // `Timeline::new` is pub: NaN is not clamped away by f64::clamp (it propagates). Make sure at least nothing panics.
    let mut tl = Timeline::new(f64::NAN);
    for k in 0..300u16 {
        let _ = tl.on_frame(k, f64::from(k) * I);
        let _ = tl.alpha(f64::from(k) * I);
        let _ = tl.interp_beta(1.0, 0);
    }
    for v in [0.0, -1.0, f64::INFINITY, 1e-30, 1e30] {
        let tl = Timeline::new(v);
        assert!(tl.interval() >= 1.0 / 240.0 && tl.interval() <= 0.1, "{v} -> {}", tl.interval());
    }
}

/// `alpha(NaN)` is NaN (clamp keeps NaN). The renderer only passes finite times, so this is hardening only.
#[test]
fn alpha_of_a_nan_clock_is_documented() {
    let mut tl = Timeline::default();
    steady(&mut tl, 0, 100, 1.0);
    let a = tl.alpha(f64::NAN);
    eprintln!("alpha(NaN) = {a}");
    assert!(a.is_nan() || (0.0..=1.25).contains(&a));
}

#[test]
fn effective_rate_property_over_every_request_and_realistic_and_hostile_refresh_rates() {
    let refreshes = [
        1.0, 24.0, 30.0, 47.95, 48.0, 50.0, 59.94, 60.0, 74.97, 75.0, 90.0, 100.0, 119.88, 120.0, 143.96, 144.0, 165.0,
        240.0, 360.0, 1000.0, 1e6,
    ];
    for req in 0..=u16::MAX {
        for &refresh in &refreshes {
            let (hz, k, capped) = effective_rate(req, refresh);
            assert!(hz.is_finite() && hz > 0.0, "{req}@{refresh}: hz {hz}");
            assert!(k >= 1, "{req}@{refresh}: k {k}");
            assert!(hz <= refresh * 1.000_001, "{req}@{refresh}: faster than the panel: {hz}");
            if capped {
                assert_eq!(k, 1);
                assert_eq!(hz, f64::from(req.max(1)));
            }
            if !capped && k > 1 {
                assert!((hz * f64::from(k) - refresh).abs() < 1e-6 * refresh, "{req}@{refresh}: {hz}x{k}");
            }
        }
    }
    for refresh in [0.0, -0.0, -1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 0.999, f64::MIN_POSITIVE, f64::MAX] {
        for req in [0u16, 1, 30, 60, 144, u16::MAX] {
            let (hz, k, _) = effective_rate(req, refresh);
            assert!(hz.is_finite() && hz > 0.0 && k >= 1, "{req}@{refresh}: {hz} x{k}");
        }
    }
}

/// With every refresh rate the app allows (30/60/75/120/144 requested, see `StreamOptions::validate`) the scheduler asks for a sane
/// number of refreshes per frame.
#[test]
fn the_rates_the_app_offers_map_to_sane_schedules() {
    for refresh in [60.0, 119.88, 120.0, 143.96, 144.0, 165.0, 59.94] {
        for req in [30u16, 60, 75, 120, 144] {
            let (hz, k, capped) = effective_rate(req, refresh);
            assert!(k <= 5, "{req}@{refresh}: {k} refreshes per frame");
            assert!(hz >= 29.0, "{req}@{refresh}: {hz}");
            eprintln!("{req:>3} @ {refresh:>6.2}: {hz:>7.3} Hz, {k} vsync(s), capped {capped}");
        }
    }
}

#[test]
fn camera_estimator_fit_cost_on_the_render_thread() {
    // observe() runs the lag search (161 lags x 150 observations, each an integral over a 1 kHz stick history) on the render thread
    // every 6th real frame. In a debug build this is the worst case; report it and bound it.
    let look = SharedLook::new();
    let mut rng = Rng(7);
    let mut t = 0.0;
    while t < 8.0 {
        look.push(t, (rng.next() * 2.0 - 1.0) as f32, (rng.next() * 2.0 - 1.0) as f32);
        t += 0.001;
    }
    let mut cam = CameraComp::new(look);
    let mut worst = 0.0f64;
    for k in 0..400 {
        let t1 = 1.0 + f64::from(k) * I;
        let s = Instant::now();
        cam.observe(t1 - I, t1, ((rng.next() - 0.5) as f32 * 8.0, (rng.next() - 0.5) as f32 * 8.0));
        worst = worst.max(s.elapsed().as_secs_f64() * 1e3);
    }
    eprintln!("worst observe() = {worst:.2} ms (debug build)");
    assert!(worst < 120.0, "{worst} ms");
}

#[test]
fn camera_and_look_survive_hostile_values() {
    let look = SharedLook::new();
    let mut cam = CameraComp::new(look.clone());
    for k in 0..300 {
        let t = f64::from(k) * I;
        look.push(t, f32::NAN, 0.0);
        look.push(t, 1e30, -1e30);
        cam.observe(t - I, t, (f32::MAX, f32::MIN));
        cam.observe(t, t - I, (1.0, 1.0));
        cam.observe(f64::NAN, t, (1.0, 1.0));
        let _ = cam.shift(t, t + 1e12);
        let _ = cam.shift(f64::NAN, f64::INFINITY);
    }
    if let Some(e) = cam.estimate() {
        assert!(e.lag.is_finite() && e.gain.0.is_finite() && e.gain.1.is_finite());
    }
    let mut h = LookHistory::default();
    h.push(1e300, 0.5, 0.5);
    h.push(1.0, 0.1, 0.1); // time backwards: refused, so one stuck-in-the-future sample freezes the history
    assert_eq!(h.len(), 1);
}

/// A single far-future timestamp from the input thread makes every later (correct) sample be refused: the stick history is dead.
/// `now()` is monotonic so this cannot happen from the app today; documented for the API user.
#[test]
fn one_far_future_look_sample_blocks_all_later_samples() {
    let mut h = LookHistory::default();
    h.push(1.0, 0.0, 0.0);
    h.push(1e9, 0.5, 0.0);
    h.push(2.0, 0.9, 0.0);
    assert_eq!(h.at(2.0), (0.0, 0.0));
}

#[test]
fn non_finite_interval_and_clock_do_not_poison_the_timeline() {
    let mut tl = Timeline::new(f64::NAN);
    assert!(tl.interval().is_finite());
    let t = steady(&mut tl, 0, 100, 1.0);
    assert_eq!(tl.alpha(f64::NAN), 0.0);
    assert!(tl.alpha(t).is_finite());
}
