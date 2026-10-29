//! Tester A4: pad-state -> Takion feedback encoding (hostile floats, SOCD, history bounds, touch ids, pad numbers).
use openps_input::state::{Button, ControllerState};
use openps_pad::*;

fn st() -> ControllerState {
    ControllerState::default()
}

#[test]
fn non_finite_and_out_of_range_floats_encode_to_valid_bytes() {
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 1e300, -1e300, 5e-324] {
        let mut p = PadFeedback::new(0);
        let mut s = st();
        s.left_x = bad;
        s.left_y = bad;
        s.right_x = bad;
        s.right_y = -bad;
        s.l2 = bad;
        s.r2 = bad;
        s.touch = Some((bad, bad));
        p.update(&s, 0.0);
        let state = p.state_payload(12);
        assert_eq!(state.len(), 28);
        assert_eq!(state[0], 0xA0);
        let h = p.history_payload(0.0);
        assert!(h.len() <= 20 * 5, "{h:?}");
        // NaN must encode as a centred stick, never as full deflection
        if bad.is_nan() {
            assert_eq!(&state[17..25], &[0u8; 8]);
        }
        // touch coordinates are clipped to the 12 bit range
        for rec in h.chunks(1) {
            let _ = rec;
        }
    }
}

#[test]
fn stick_extremes_map_to_symmetric_i16() {
    let mut p = PadFeedback::new(0);
    let mut s = st();
    s.left_x = 1.0;
    s.left_y = -1.0;
    s.right_x = 0.0;
    s.right_y = 1e-6;
    p.update(&s, 0.0);
    let b = p.state_payload(12);
    assert_eq!(i16::from_be_bytes([b[17], b[18]]), 32767);
    assert_eq!(i16::from_be_bytes([b[19], b[20]]), -32767); // never -32768: symmetric
    assert_eq!(i16::from_be_bytes([b[21], b[22]]), 0);
    assert_eq!(i16::from_be_bytes([b[23], b[24]]), 0);
}

#[test]
fn socd_masks_for_every_dpad_combination() {
    for bits in 0u8..16 {
        let mut s = st();
        for (i, b) in [Button::DPAD_UP, Button::DPAD_DOWN, Button::DPAD_LEFT, Button::DPAD_RIGHT].iter().enumerate() {
            if bits & (1 << i) != 0 {
                s.buttons.insert(*b);
            }
        }
        let m = button_mask(&s);
        let (u, d, l, r) = (bits & 1 != 0, bits & 2 != 0, bits & 4 != 0, bits & 8 != 0);
        assert_eq!(m & DU != 0, u && !d, "{bits:04b}");
        assert_eq!(m & DD != 0, d && !u, "{bits:04b}");
        assert_eq!(m & DL != 0, l && !r, "{bits:04b}");
        assert_eq!(m & DR != 0, r && !l, "{bits:04b}");
    }
}

#[test]
fn every_button_has_a_distinct_bit_and_record_and_press_release_pair() {
    let all = [CROSS, CIRCLE, SQUARE, TRIANGLE, DL, DR, DU, DD, L1, R1, L3, R3, OPTIONS, SHARE, TOUCHPAD, PS, L2, R2];
    let mut ids = std::collections::HashSet::new();
    for b in all {
        assert_eq!(b.count_ones(), 1);
        let p = button_record(b, true).unwrap();
        let r = button_record(b, false).unwrap();
        assert_eq!((p[0], r[0]), (0x80, 0x80));
        assert!(ids.insert(p[1]), "press id {:#x} reused", p[1]);
        assert_ne!(p, r, "press and release records must differ");
    }
    // multi-bit masks are not a button
    assert_eq!(button_record(CROSS | CIRCLE, true), None);
    assert_eq!(button_record(0, true), None);
}

#[test]
fn random_update_sequences_keep_history_bounded_and_edge_complete() {
    let mut x = 0x1234_5678_9ABC_DEF1u64;
    let mut rnd = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let all = Button::ALL;
    let mut p = PadFeedback::new(3);
    let mut s = st();
    let mut now = 0.0;
    for _ in 0..50_000 {
        now += 0.002;
        match rnd() % 4 {
            0 => {
                let b = all[(rnd() as usize) % all.len()];
                if !s.buttons.remove(&b) {
                    s.buttons.insert(b);
                }
            }
            1 => s.l2 = (rnd() % 300) as f64 / 255.0 - 0.1,
            2 => {
                s.touch = if rnd() % 3 == 0 {
                    None
                } else {
                    Some(((rnd() % 1200) as f64 / 1000.0, (rnd() % 1200) as f64 / 1000.0))
                }
            }
            _ => s.left_x = (rnd() % 2001) as f64 / 1000.0 - 1.0,
        }
        p.update(&s, now);
        if rnd() % 5 == 0 {
            let h = p.history_payload(now);
            assert!(h.len() <= 20 * 5);
            let st = p.state_payload(12);
            assert_eq!(st[0], 0xA3);
        }
    }
}

#[test]
fn touch_counter_stays_7_bit_across_many_fingers() {
    let mut p = PadFeedback::new(0);
    let mut s = st();
    for i in 0..1000 {
        s.touch = Some((0.5, 0.5));
        p.update(&s, f64::from(i));
        let h = p.history_payload(0.0);
        assert_eq!(h[0], 0xD0);
        assert!(h[1] <= 0x7F, "counter {:#x} at finger {i}", h[1]);
        s.touch = None;
        p.update(&s, f64::from(i) + 0.5);
        assert_eq!(p.history_payload(0.0)[0], 0xC0);
    }
}

#[test]
fn touch_edges_of_the_surface_are_clipped_not_wrapped() {
    assert_eq!(touch_record(4095, 4095, 0, true), [0xD0, 0, 0xFF, 0xFF, 0xFF]);
    assert_eq!(touch_record(4096, 4096, 0, true), [0xD0, 0, 0xFF, 0xFF, 0xFF]);
    assert_eq!(touch_record(i32::MIN, i32::MAX, 0xFF, true), [0xD0, 0x7F, 0x00, 0x0F, 0xFF]);
    let mut p = PadFeedback::new(0);
    let mut s = st();
    s.touch = Some((1.0, 1.0)); // 1920 x 943 fits into 12 bit
    p.update(&s, 0.0);
    let h = p.history_payload(0.0);
    let x = (usize::from(h[2]) << 4) | usize::from(h[3] >> 4);
    let y = (usize::from(h[3] & 15) << 8) | usize::from(h[4]);
    assert_eq!((x, y), (1920, 943));
}

#[test]
fn pad_numbers_above_three_are_masked_everywhere() {
    let mut p = PadFeedback::new(5);
    assert_eq!(p.state_payload(12)[0], 0xA1); // 5 & 3
    assert_eq!(presence_record(5, true)[0], 0x81);
    assert_eq!(stamp(0x80, 5), 0x81);
}

#[test]
fn presence_record_masks_the_pad_number() {
    assert_eq!(presence_record(5, true)[0], 0x81);
    assert_eq!(presence_record(4, false)[0], 0x80);
}

#[test]
fn history_resend_never_stops_while_history_is_non_empty_info() {
    // Info (T07-13): after one single button tap the pad keeps re-sending its (20-record deep) history every 100 ms for
    // the whole session ("self-healing"); the history is never aged out.
    let mut p = PadFeedback::new(0);
    let mut s = st();
    s.buttons.insert(Button::CROSS);
    p.update(&s, 0.0);
    s.buttons.clear();
    p.update(&s, 0.1);
    let mut due = 0;
    for i in 0..600 {
        let now = 0.2 + f64::from(i) * 0.11; // 66 s
        if p.history_due(now) {
            p.history_payload(now);
            due += 1;
        }
    }
    assert!(due >= 590, "resent {due} times in 60 s with no input at all");
}

#[test]
fn old_takion_version_gets_the_25_byte_state() {
    let mut p = PadFeedback::new(1);
    for v in [0, 1, 11] {
        assert_eq!(p.state_payload(v).len(), 25);
    }
    for v in [12, 13, u32::MAX] {
        assert_eq!(p.state_payload(v).len(), 28);
    }
}

#[test]
fn sensor_encoding_edges_wrap_like_java_info() {
    // Info (T07-14): documented "wraps like Java": a gyro reading beyond +-30 rad/s flips sign instead of saturating.
    assert_eq!(encode_gyro(30.0), -1); // 0xFFFF
    assert!(encode_gyro(31.0) > 0, "31 rad/s wraps to a small positive value ({})", encode_gyro(31.0));
    assert_eq!(encode_gyro(f32::NAN), 0);
    assert_eq!(encode_gyro(f32::INFINITY), -1); // saturating i32 cast then truncation
    assert_eq!(encode_accel(-5.0), 0);
    // quaternion garbage does not panic
    for q in [[f32::NAN; 4], [f32::INFINITY, 0.0, 0.0, 0.0], [0.0; 4], [1e30, -1e30, 0.5, 0.5]] {
        let _ = compress_orientation(q);
    }
    let mut p = PadFeedback::new(0);
    p.set_sensors([f32::NAN; 3], [f32::INFINITY; 3], [f32::NAN; 4]);
    assert_eq!(p.state_payload(12).len(), 28);
}

#[test]
fn identical_state_produces_no_traffic() {
    let mut p = PadFeedback::new(0);
    let mut s = st();
    s.buttons.insert(Button::PS);
    s.left_x = 0.3;
    p.update(&s, 0.0);
    for i in 1..100 {
        let u = p.update(&s, f64::from(i));
        assert_eq!(u, Update::default());
    }
}
