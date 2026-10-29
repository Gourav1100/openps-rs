//! Tester A4: mapping engine corner cases (SOCD, chords, stuck keys, NaN/inf, sensitivity, fuzz).
use openps_input::keys::{Input, Key, MouseInput};
use openps_input::state::{Button, ControllerState};
use openps_input::{load_defaults, load_layers, Config, InputSnapshot, Mapper};

const L: Input = Input::Mouse(MouseInput::MOUSE_LEFT);
const R: Input = Input::Mouse(MouseInput::MOUSE_RIGHT);
const CMD: Input = Input::Key(Key::LEFT_COMMAND);
const TAB: Input = Input::Key(Key::TAB);

fn k(key: Key) -> Input {
    Input::Key(key)
}
fn snap(i: &[Input]) -> InputSnapshot {
    InputSnapshot::new(i.iter().copied(), 0.0, 0.0)
}
fn snapd(i: &[Input], dx: f64, dy: f64) -> InputSnapshot {
    InputSnapshot::new(i.iter().copied(), dx, dy)
}
fn user(text: &str) -> Config {
    load_layers(Some(text), false).unwrap()
}
fn instant() -> Config {
    // no ramps, no minimum press time, no smoothing: exact responses
    user("[output]\nmin_press_ms = 0.0\n[keyboard_stick]\nleft_ramp_ms=0.0\nright_ramp_ms=0.0\nrelease_ms=0.0\nleft_release_ms=0.0\n[mouse]\nsmoothing = 0.0\n")
}
fn finite_and_bounded(s: &ControllerState) {
    for v in [s.left_x, s.left_y, s.right_x, s.right_y] {
        assert!(v.is_finite() && (-1.0..=1.0).contains(&v), "{s:?}");
    }
    assert!((0.0..=1.0).contains(&s.l2) && (0.0..=1.0).contains(&s.r2), "{s:?}");
    assert!(s.left_x.hypot(s.left_y) <= 1.0 + 1e-9 && s.right_x.hypot(s.right_y) <= 1.0 + 1e-9, "{s:?}");
    if let Some((x, y)) = s.touch {
        assert!((0.0..=1.0).contains(&x) && (0.0..=1.0).contains(&y), "{s:?}");
    }
}

// ---------------------------------------------------------------- SOCD

#[test]
fn socd_opposite_stick_keys_cancel_in_every_order() {
    for (a, b, axis) in [(Key::W, Key::S, 'y'), (Key::A, Key::D, 'x')] {
        for order in [[a, b], [b, a]] {
            let mut m = Mapper::new(instant(), None).unwrap();
            let mut t = 0.0;
            let mut last = ControllerState::default();
            for key in order {
                t += 0.004;
                last = m.map(&snap(&[k(order[0]), k(key)]), t);
            }
            let v = if axis == 'y' { last.left_y } else { last.left_x };
            assert_eq!(v, 0.0, "{order:?}");
            // releasing one of the pair leaves the other direction active (no stuck neutral)
            let s = m.map(&snap(&[k(a)]), t + 0.004);
            let v = if axis == 'y' { s.left_y } else { s.left_x };
            assert!(v != 0.0, "remaining key must act: {order:?}");
            let s = m.map(&snap(&[]), t + 0.008);
            assert_eq!((s.left_x, s.left_y), (0.0, 0.0));
        }
    }
}

#[test]
fn socd_dpad_opposites_cancel_when_two_bindings_fire_at_once() {
    // W+S neutral, plus the shipped Option+arrows D-pad: UP and DOWN together are both emitted by the mapper
    // (the pad encoder cancels them, tested in openps-pad). The mapper itself must not panic or favour one.
    let mut m = Mapper::new(instant(), None).unwrap();
    let s = m.map(&snap(&[k(Key::LEFT_OPTION), k(Key::UP_ARROW), k(Key::DOWN_ARROW)]), 0.0);
    // longest chord wins: both chords have length 2, so both fire
    assert!(s.buttons.contains(&Button::DPAD_UP) && s.buttons.contains(&Button::DPAD_DOWN));
}

#[test]
fn diagonal_is_limited_to_the_unit_circle() {
    let mut m = Mapper::new(instant(), None).unwrap();
    let s = m.map(&snap(&[k(Key::W), k(Key::D)]), 0.0);
    let mag = s.left_x.hypot(s.left_y);
    assert!((mag - 1.0).abs() < 1e-9, "{mag}");
}

// ---------------------------------------------------------------- stuck keys / chords

#[test]
fn releasing_everything_always_returns_to_neutral() {
    // default config incl. ramps, min-press: after the longest possible hold-over time nothing may remain
    let mut m = Mapper::new(load_defaults().unwrap(), None).unwrap();
    let all: Vec<Input> =
        Key::ALL.iter().map(|x| k(*x)).chain(MouseInput::ALL.iter().map(|x| Input::Mouse(*x))).collect();
    let mut t = 0.0;
    for _ in 0..50 {
        t += 0.004;
        m.map(&snapd(&all, 50.0, 50.0), t);
    }
    let mut last = ControllerState::default();
    for _ in 0..400 {
        t += 0.004;
        last = m.map(&snap(&[]), t);
    }
    assert!(last.buttons.is_empty(), "{:?}", last.buttons);
    assert_eq!(
        (last.left_x, last.left_y, last.right_x, last.right_y, last.l2, last.r2),
        (0.0, 0.0, 0.0, 0.0, 0.0, 0.0)
    );
    assert_eq!(last.touch, None);
}

#[test]
fn chord_modifier_release_order_never_leaves_a_stuck_output() {
    let cfg = user(
        "[output]\nmin_press_ms = 0.0\n[bindings]\nCROSS = [\"G\"]\nCIRCLE = [\"LEFT_CONTROL+G\"]\nSQUARE = [\"LEFT_CONTROL+RIGHT_SHIFT+G\"]\n",
    );
    let (c, sh, g) = (k(Key::LEFT_CONTROL), k(Key::RIGHT_SHIFT), k(Key::G));
    let seqs: Vec<Vec<Vec<Input>>> = vec![
        vec![vec![c, sh, g], vec![c, g], vec![g], vec![]], // release shift, then ctrl
        vec![vec![c, sh, g], vec![sh, g], vec![g], vec![]], // release ctrl, then shift
        vec![vec![c, sh, g], vec![c, sh], vec![c], vec![]], // release g first
        vec![vec![g], vec![c, g], vec![c, sh, g], vec![c, g], vec![g]], // press up then down
    ];
    for seq in seqs {
        let mut m = Mapper::new(cfg.clone(), None).unwrap();
        let mut t = 0.0;
        let mut outs = vec![];
        for frame in &seq {
            t += 0.004;
            outs.push(m.map(&snap(frame), t));
        }
        // longest chord wins at every step and shorter chords are not additionally pressed
        for (frame, out) in seq.iter().zip(&outs) {
            let want = if frame.contains(&c) && frame.contains(&sh) && frame.contains(&g) {
                vec![Button::SQUARE]
            } else if frame.contains(&c) && frame.contains(&g) {
                vec![Button::CIRCLE]
            } else if frame.contains(&g) {
                vec![Button::CROSS]
            } else {
                vec![]
            };
            assert_eq!(out.buttons.iter().copied().collect::<Vec<_>>(), want, "frame {frame:?}");
        }
    }
}

#[test]
fn min_press_holds_a_tap_but_never_longer() {
    let mut m = Mapper::new(user("[output]\nmin_press_ms = 35.0\n"), None).unwrap();
    assert!(m.map(&snap(&[k(Key::SPACE)]), 0.0).buttons.contains(&Button::CROSS));
    assert!(m.map(&snap(&[]), 0.010).buttons.contains(&Button::CROSS), "held for the minimum");
    assert!(!m.map(&snap(&[]), 0.036).buttons.contains(&Button::CROSS), "released afterwards");
    // clock jumping far forward or backwards must not leave it pressed
    assert!(m.map(&snap(&[k(Key::SPACE)]), 100.0).buttons.contains(&Button::CROSS));
    assert!(!m.map(&snap(&[]), 99.0).buttons.contains(&Button::CROSS), "time going backwards: released");
}

#[test]
fn tab_click_toggle_fires_once_per_click_edge_and_does_not_leak_the_click() {
    let mut m = Mapper::new(instant(), None).unwrap();
    let mut t = 0.0;
    let mut step = |m: &mut Mapper, i: &[Input]| {
        t += 0.004;
        m.map(&snap(i), t)
    };
    step(&mut m, &[TAB]);
    let s = step(&mut m, &[TAB, L]);
    assert_eq!(s.r2, 0.0, "the toggling click is consumed");
    assert_eq!(m.click_modes().0, "RB");
    for _ in 0..20 {
        step(&mut m, &[TAB, L]); // holding: no re-toggle
    }
    assert_eq!(m.click_modes().0, "RB");
    // release TAB while the click is still held: the click stays consumed until released
    let s = step(&mut m, &[L]);
    assert_eq!((s.r2, s.buttons.contains(&Button::R1)), (0.0, false), "consumed click must not fire after TAB release");
    step(&mut m, &[]);
    let s = step(&mut m, &[L]);
    assert!(s.buttons.contains(&Button::R1), "now acts as RB");
    // events were queued for the toggle only
    assert_eq!(m.pop_events().len(), 1);
}

#[test]
fn adaptive_trigger_release_of_command_with_click_held_never_sticks() {
    let mut m = Mapper::new(load_defaults().unwrap(), None).unwrap();
    let mut t = 0.0;
    for _ in 0..200 {
        t += 0.004;
        m.map(&snap(&[CMD, L]), t);
    }
    // command released, click still down: trigger must drop to 0 and stay 0 until the click is released
    for _ in 0..50 {
        t += 0.004;
        let s = m.map(&snap(&[L]), t);
        assert_eq!(s.l2, 0.0);
        assert_eq!(s.r2, 0.0, "stuck trigger");
    }
    t += 0.004;
    m.map(&snap(&[]), t);
    t += 0.004;
    assert!(m.map(&snap(&[L]), t).r2 > 0.0 || m.map(&snap(&[L]), t + 0.05).r2 > 0.0, "click works again");
}

#[test]
fn profile_switch_and_reset_clear_all_transient_state() {
    let mut m = Mapper::new(load_defaults().unwrap(), None).unwrap();
    let mut t = 0.0;
    for _ in 0..30 {
        t += 0.004;
        m.map(&snapd(&[k(Key::W), k(Key::SPACE), L], 30.0, 30.0), t);
    }
    m.set_profile("racing").unwrap();
    t += 0.004;
    let s = m.map(&snap(&[]), t);
    assert_eq!((s.left_x, s.left_y, s.right_x, s.right_y), (0.0, 0.0, 0.0, 0.0));
    assert!(s.buttons.is_empty());
    assert!(m.set_profile("does-not-exist").is_err());
    assert_eq!(m.profile, "racing", "failed switch keeps the old profile");
}

// ---------------------------------------------------------------- mouse

#[test]
fn hostile_mouse_deltas_never_produce_non_finite_output() {
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, f64::MAX, f64::MIN, 1e300, -1e300, 5e-324] {
        let mut m = Mapper::new(load_defaults().unwrap(), None).unwrap();
        let mut t = 0.0;
        for i in 0..50 {
            t += 0.004;
            let s = m.map(&snapd(&[], if i % 2 == 0 { bad } else { 3.0 }, bad), t);
            finite_and_bounded(&s);
        }
    }
}

#[test]
fn a_single_nan_delta_does_not_poison_the_filter_while_the_mouse_keeps_moving() {
    // Cursor::delta() is i32-derived on macOS so NaN cannot occur there; any other Cursor impl could feed one.
    let mut m = Mapper::new(load_defaults().unwrap(), None).unwrap();
    let mut t = 0.0;
    m.map(&snapd(&[], f64::NAN, 0.0), t);
    let mut last = 0.0;
    for _ in 0..200 {
        t += 0.004;
        last = m.map(&snapd(&[], 20.0, 0.0), t).right_x; // keeps moving: never idle long enough to recenter
    }
    assert!(last > 0.5, "right stick is stuck at {last} after one NaN sample (filter state is NaN)");
}

#[test]
fn smoothing_state_survives_a_single_infinite_delta_when_motion_continues() {
    let mut m = Mapper::new(load_defaults().unwrap(), None).unwrap();
    let mut t = 0.0;
    m.map(&snapd(&[], f64::INFINITY, 0.0), t);
    let mut last = 0.0;
    for _ in 0..200 {
        t += 0.004;
        last = m.map(&snapd(&[], 20.0, 0.0), t).right_x;
    }
    assert!(last > 0.5, "right stick is stuck at {last} after one infinite sample");
}

#[test]
fn nan_recovers_once_the_mouse_is_idle_for_the_recenter_time() {
    let mut m = Mapper::new(load_defaults().unwrap(), None).unwrap();
    let mut t = 0.0;
    m.map(&snapd(&[], f64::NAN, 0.0), t);
    for _ in 0..30 {
        t += 0.004;
        m.map(&snap(&[]), t);
    }
    let mut last = 0.0;
    for _ in 0..200 {
        t += 0.004;
        last = m.map(&snapd(&[], 20.0, 0.0), t).right_x;
    }
    assert!(last > 0.5, "{last}");
}

#[test]
fn sensitivity_scales_monotonically_and_saturates_at_the_circle() {
    let mut prev = -1.0;
    for sens in [0.01, 0.02, 0.05, 0.1, 0.3] {
        let cfg = user(&format!("[mouse]\nsensitivity = {sens}\nsmoothing = 0.0\n"));
        let mut m = Mapper::new(cfg, None).unwrap();
        let s = m.map(&snapd(&[], 4.0, 0.0), 0.0);
        assert!(s.right_x >= prev, "sens {sens}: {} < {prev}", s.right_x);
        prev = s.right_x;
        finite_and_bounded(&s);
    }
    let cfg = user("[mouse]\nsensitivity = 1e300\nsmoothing = 0.0\n");
    let mut m = Mapper::new(cfg, None).unwrap();
    for i in 0..10 {
        finite_and_bounded(&m.map(&snapd(&[], 1000.0, -1000.0), f64::from(i) * 0.004));
    }
}

#[test]
fn sensitivity_means_the_same_at_any_loop_rate() {
    let steady = |dt: f64, dx: f64| {
        let mut m = Mapper::new(user("[mouse]\nsensitivity = 0.05\nsmoothing = 0.6\n"), None).unwrap();
        let mut t = 0.0;
        let mut v = 0.0;
        for _ in 0..(2.0 / dt) as usize {
            t += dt;
            v = m.map(&snapd(&[], dx, 0.0), t).right_x;
        }
        v
    };
    let a = steady(0.004, 2.0); // 500 counts/s
    let b = steady(0.002, 1.0); // 500 counts/s at 500 Hz
    let c = steady(0.001, 0.5); // 500 counts/s at 1 kHz
    assert!((a - b).abs() < 0.02 && (a - c).abs() < 0.02, "{a} {b} {c}");
}

#[test]
fn mouse_look_none_and_left_stick_and_invert_y() {
    let d = |extra: &str| {
        let mut m = Mapper::new(user(&format!("[mouse]\nsmoothing = 0.0\n{extra}\n")), None).unwrap();
        m.map(&snapd(&[], 0.0, 10.0), 0.0)
    };
    let s = d("look = \"NONE\"");
    assert_eq!((s.right_x, s.right_y, s.left_x, s.left_y), (0.0, 0.0, 0.0, 0.0));
    let s = d("look = \"LEFT_STICK\"");
    assert!(s.left_y > 0.0 && s.right_y == 0.0);
    let s = d("invert_y = true");
    assert!(s.right_y < 0.0);
    let s = d("");
    assert!(s.right_y > 0.0);
}

#[test]
fn smoothing_values_above_099_are_silently_capped_info() {
    // Info (T07-19): validate() accepts smoothing up to 0.999999 but smoothing_tau clamps to 0.99.
    use openps_input::mapping::smoothing_tau;
    assert_eq!(smoothing_tau(0.999_999), smoothing_tau(0.99));
    assert_eq!(smoothing_tau(0.0), 0.0);
    assert_eq!(smoothing_tau(-5.0), 0.0);
    assert!(smoothing_tau(f64::NAN).is_nan(), "NaN smoothing is not sanitised (config validation rejects it)");
}

#[test]
fn deadzone_never_exceeds_one_and_zero_input_stays_zero() {
    let cfg = user("[mouse]\ndeadzone = 0.999999\nsmoothing = 0.0\n");
    let mut m = Mapper::new(cfg, None).unwrap();
    assert_eq!(m.map(&snapd(&[], 0.0, 0.0), 0.0).right_x, 0.0);
    finite_and_bounded(&m.map(&snapd(&[], 500.0, 500.0), 0.004));
}

// ---------------------------------------------------------------- gestures & clock

#[test]
fn touch_gestures_stay_on_the_pad_and_hold_does_not_retrigger() {
    let cfg = user("[touchpad]\nswipe_length = 1.0\nswipe_ms = 100.0\ntap_ms = 50.0\n");
    let mut m = Mapper::new(cfg, None).unwrap();
    let mut t = 0.0;
    let mut touches = 0;
    let mut finger_up_after_hold = false;
    for _ in 0..300 {
        t += 0.002;
        let s = m.map(&snap(&[k(Key::KEY_3)]), t); // SWIPE_LEFT held down for 600 ms
        finite_and_bounded(&s);
        touches += i32::from(s.touch.is_some());
    }
    for _ in 0..100 {
        t += 0.002;
        finger_up_after_hold |= m.map(&snap(&[k(Key::KEY_3)]), t).touch.is_none();
    }
    assert!(finger_up_after_hold, "a held key plays ONE stroke");
    assert!(touches > 0 && touches <= 55, "stroke lasted {touches} ticks");
}

#[test]
fn clock_anomalies_do_not_break_the_output() {
    let mut m = Mapper::new(load_defaults().unwrap(), None).unwrap();
    for t in [0.0, 1.0, 0.5, 0.5, -3.0, 1e9, 1e9 + 0.004, f64::MAX / 2.0, 0.0] {
        finite_and_bounded(&m.map(&snapd(&[k(Key::W), L], 10.0, 10.0), t));
    }
    // NaN time: must not panic (output may be neutral)
    let s = m.map(&snap(&[k(Key::W)]), f64::NAN);
    assert!(s.left_y.is_finite());
    let _ = m.map(&snap(&[k(Key::W)]), 1.0);
}

#[test]
fn fuzz_random_snapshots_always_yield_bounded_state() {
    let mut x = 0x9E37_79B9_7F4A_7C15_u64;
    let mut rnd = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let keys: Vec<Input> =
        Key::ALL.iter().map(|x| k(*x)).chain(MouseInput::ALL.iter().map(|x| Input::Mouse(*x))).collect();
    for cfg in [load_defaults().unwrap(), instant()] {
        let mut m = Mapper::new(cfg, None).unwrap();
        let mut t = 0.0;
        for i in 0..20_000 {
            t += (rnd() % 20) as f64 / 1000.0;
            let n = (rnd() % 6) as usize;
            let pressed: Vec<Input> = (0..n).map(|_| keys[(rnd() as usize) % keys.len()]).collect();
            let dx = (rnd() % 4001) as f64 - 2000.0;
            let dy = (rnd() % 4001) as f64 - 2000.0;
            finite_and_bounded(&m.map(&snapd(&pressed, dx, dy), t));
            if i % 5000 == 0 {
                m.reset();
            }
            let _ = m.pop_events();
        }
    }
}

#[test]
fn event_queue_is_drained_by_pop_events() {
    // memory growth: toggle events accumulate until popped; the engine pops every tick
    let mut m = Mapper::new(instant(), None).unwrap();
    let mut t = 0.0;
    for _ in 0..1000 {
        t += 0.004;
        m.map(&snap(&[TAB, R]), t);
        t += 0.004;
        m.map(&snap(&[TAB]), t);
    }
    assert_eq!(m.pop_events().len(), 1000);
    assert!(m.pop_events().is_empty());
}
