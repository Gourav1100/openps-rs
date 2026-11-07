use openps_gamepad::map::{to_state, RawPad, DEFAULT_DEADZONE};
use openps_gamepad::mock::MockPad;
use openps_gamepad::{PadEvent, PadKind, PadSource};
use openps_input::state::Button;

#[test]
fn a_stick_inside_the_dead_zone_is_neutral() {
    let raw = RawPad { left: (0.05, -0.05), ..Default::default() };
    let s = to_state(&raw, DEFAULT_DEADZONE);
    assert_eq!((s.left_x, s.left_y), (0.0, 0.0));
}

#[test]
fn full_deflection_stays_full_and_up_becomes_negative_y() {
    let raw = RawPad { left: (0.0, 1.0), right: (1.0, 0.0), ..Default::default() };
    let s = to_state(&raw, DEFAULT_DEADZONE);
    assert_eq!(s.left_y, -1.0);
    assert!((s.right_x - 1.0).abs() < 1e-9);
}

#[test]
fn the_range_outside_the_dead_zone_starts_from_zero() {
    let raw = RawPad { left: (0.0, 0.0804), ..Default::default() };
    assert!(to_state(&raw, DEFAULT_DEADZONE).left_y > -0.01);
}

#[test]
fn nan_and_out_of_range_values_cannot_reach_the_network() {
    let raw = RawPad { left: (f64::NAN, 5.0), l2: 3.0, r2: -1.0, ..Default::default() };
    let s = to_state(&raw, DEFAULT_DEADZONE);
    assert!(s.left_x.abs() <= 1.0 && s.left_y.abs() <= 1.0);
    assert_eq!((s.l2, s.r2), (1.0, 0.0));
}

#[test]
fn buttons_and_touch_pass_through() {
    let mut raw = RawPad { touch: Some((0.5, 0.25)), ..Default::default() };
    raw.buttons.extend([Button::CROSS, Button::MUTE, Button::TOUCHPAD]);
    let s = to_state(&raw, DEFAULT_DEADZONE);
    assert_eq!(s.buttons, raw.buttons);
    assert_eq!(s.touch, Some((0.5, 0.25)));
}

#[test]
fn the_mock_source_hands_over_events_once_in_order() {
    let mut m = MockPad::default();
    m.push(PadEvent::Connected { id: 1, kind: PadKind::DualSense, name: "x".into() });
    m.push(PadEvent::Disconnected { id: 1 });
    assert_eq!(m.poll().len(), 2);
    assert!(m.poll().is_empty());
}

mod orientation {
    use openps_gamepad::orient::Orient;

    #[test]
    fn a_pad_lying_still_stays_level() {
        let mut o = Orient::default();
        let mut q = [0.0; 4];
        for _ in 0..500 {
            q = o.update([0.0; 3], [0.0, 1.0, 0.0], 0.008);
        }
        assert!(q[0].abs() < 1e-3 && q[1].abs() < 1e-3 && q[2].abs() < 1e-3 && (q[3] - 1.0).abs() < 1e-3);
    }

    #[test]
    fn turning_about_the_vertical_axis_integrates_the_angle() {
        let mut o = Orient::default();
        let mut q = [0.0; 4];
        for _ in 0..125 {
            q = o.update([0.0, 1.0, 0.0], [0.0, 1.0, 0.0], 0.008); // 1 rad/s for 1 s
        }
        let angle = 2.0 * q[1].atan2(q[3]);
        assert!((angle - 1.0).abs() < 0.05, "{angle}");
    }

    #[test]
    fn gravity_pulls_a_drifting_estimate_back() {
        let mut o = Orient::default();
        let mut q = [0.0; 4];
        for _ in 0..50 {
            q = o.update([0.5, 0.0, 0.0], [0.0, 1.0, 0.0], 0.008); // gyro bias while the pad is really still
        }
        for _ in 0..1000 {
            q = o.update([0.0; 3], [0.0, 1.0, 0.0], 0.008);
        }
        assert!(q[0].abs() < 0.01, "{q:?}");
    }

    /// Where the estimate thinks gravity is, in the pad's own frame.
    fn gravity(q: [f32; 4]) -> [f32; 3] {
        let [x, y, z, w] = q;
        [2.0 * (x * y + w * z), w * w - x * x + y * y - z * z, 2.0 * (y * z - w * x)]
    }

    #[test]
    fn a_pad_held_tilted_when_it_starts_snaps_to_gravity_within_the_warm_up() {
        // chiaki-ng uses a high filter gain for the first 30 samples so the estimate does not crawl towards the real attitude
        let tilted = [0.866_025_4, 0.5, 0.0]; // rolled 60 degrees
        let mut o = Orient::default();
        let mut q = [0.0; 4];
        for _ in 0..30 {
            q = o.update([0.0; 3], tilted, 0.008);
        }
        let g = gravity(q);
        let dot = g[0] * tilted[0] + g[1] * tilted[1] + g[2] * tilted[2];
        assert!(dot > 0.996, "within 5 degrees after 30 samples, dot = {dot}");
    }

    #[test]
    fn after_the_warm_up_the_filter_is_gentle_again() {
        let mut o = Orient::default();
        for _ in 0..30 {
            o.update([0.0; 3], [0.0, 1.0, 0.0], 0.008);
        }
        // a brief knock (accelerometer swings for one sample) must not move the estimate much any more
        let q = o.update([0.0; 3], [1.0, 0.2, 0.0], 0.008);
        assert!(q[2].abs() < 0.005 && q[0].abs() < 0.005, "{q:?}");
    }

    #[test]
    fn a_reset_makes_the_current_heading_the_origin_and_warms_up_again() {
        let mut o = Orient::default();
        let mut q = [0.0; 4];
        for _ in 0..125 {
            q = o.update([0.0, 1.0, 0.0], [0.0, 1.0, 0.0], 0.008); // turned 1 rad
        }
        assert!(2.0 * q[1].atan2(q[3]) > 0.9);
        o.reset();
        q = o.update([0.0; 3], [0.0, 1.0, 0.0], 0.008);
        assert!(q[0].abs() < 1e-3 && q[1].abs() < 1e-3 && q[2].abs() < 1e-3 && (q[3] - 1.0).abs() < 1e-3, "{q:?}");
        // and it is level-seeking fast again, like at the start
        let tilted = [0.866_025_4, 0.5, 0.0];
        for _ in 0..30 {
            q = o.update([0.0; 3], tilted, 0.008);
        }
        let g = gravity(q);
        assert!(g[0] * tilted[0] + g[1] * tilted[1] > 0.996);
    }

    #[test]
    fn the_result_is_always_a_unit_quaternion() {
        let mut o = Orient::default();
        let mut q = [0.0; 4];
        for i in 0..300 {
            q = o.update([3.0, -2.0, 1.0], [0.1 * (i % 7) as f32, 1.0, 0.3], 0.008);
        }
        assert!((q.iter().map(|v| v * v).sum::<f32>() - 1.0).abs() < 1e-4);
    }
}

#[test]
fn a_second_finger_passes_through_and_is_clamped() {
    let raw = RawPad { touch: Some((0.1, 0.2)), touch2: Some((0.9, 0.8)), ..Default::default() };
    let s = to_state(&raw, DEFAULT_DEADZONE);
    assert_eq!((s.touch, s.touch2), (Some((0.1, 0.2)), Some((0.9, 0.8))));

    let wild = RawPad { touch2: Some((7.0, -3.0)), ..Default::default() };
    assert_eq!(
        to_state(&wild, DEFAULT_DEADZONE).touch2,
        Some((1.0, 0.0)),
        "out-of-range finger positions cannot reach the network"
    );
}
