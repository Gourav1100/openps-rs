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
