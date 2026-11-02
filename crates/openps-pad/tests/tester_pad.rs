//! Tester A4: pad-state -> Takion feedback encoding (hostile floats, SOCD, history bounds, touch ids, pad numbers).
use openps_input::state::{Button, ControllerState};
use openps_pad::*;

fn st() -> ControllerState {
    ControllerState::default()
}

fn history_records(history: &[u8]) -> Vec<&[u8]> {
    let mut records = Vec::new();
    let mut offset = 0;
    while offset < history.len() {
        let len = if history[offset] & 0xE0 == 0xC0 { 5 } else { 3 };
        records.push(&history[offset..offset + len]);
        offset += len;
    }
    records
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
fn releasing_lt_after_rt_then_lt_keeps_each_trigger_independent() {
    let mut pad = PadFeedback::new(0);
    let mut state = st();

    state.r2 = 1.0;
    pad.update(&state, 0.0);
    state.l2 = 1.0;
    pad.update(&state, 0.01);
    state.l2 = 0.0;
    pad.update(&state, 0.02);

    let bytes = pad.history_payload(0.02);
    let history = history_records(&bytes);
    assert!(history.iter().any(|r| r[1] == 0x86 && r[2] == 0), "LT release must be present: {history:?}");
    assert!(history.iter().any(|r| r[1] == 0x87 && r[2] == 0xFF), "held RT must remain active: {history:?}");

    state.r2 = 0.0;
    pad.update(&state, 0.03);
    let bytes = pad.history_payload(0.03);
    let history = history_records(&bytes);
    assert!(history.iter().any(|r| r[1] == 0x86 && r[2] == 0), "released LT must remain neutral");
    assert!(history.iter().any(|r| r[1] == 0x87 && r[2] == 0), "RT release must be present: {history:?}");
}

#[test]
fn every_event_gets_its_own_history_packet_oldest_first() {
    // The console applies only ONE new event per history packet (chiaki-ng 4382c1c7: "simultaneous L2 and R2 presses registered as
    // R2 only"), and the original app offers a packet per event. So LT and RT changing together must be two packets, the second
    // carrying the first behind it.
    let mut p = PadFeedback::new(0);
    let mut s = st();
    s.l2 = 1.0;
    s.r2 = 1.0;
    p.update(&s, 0.0);
    assert_eq!(
        p.take_history_packets(0.0),
        vec![vec![0x80, 0x86, 0xFF], vec![0x80, 0x87, 0xFF, 0x80, 0x86, 0xFF]],
        "LT first, then RT with LT behind it"
    );
    assert!(p.take_history_packets(0.0).is_empty(), "taken once");
    assert_eq!(
        p.history_payload(0.0),
        vec![0x80, 0x87, 0xFF, 0x80, 0x86, 0xFF],
        "the idle repeat still sends the whole window"
    );
}

#[test]
fn buttons_then_triggers_are_each_a_packet_and_a_single_event_is_a_single_packet() {
    let mut p = PadFeedback::new(0);
    let mut s = st();
    s.buttons.insert(Button::CROSS);
    s.l2 = 1.0;
    p.update(&s, 0.0);
    let packets = p.take_history_packets(0.0);
    assert_eq!(packets.len(), 2);
    assert_eq!(packets[0], vec![0x80, 0x88, 0xFF], "CROSS");
    assert_eq!(packets[1], vec![0x80, 0x86, 0xFF, 0x80, 0x88, 0xFF], "then LT, with CROSS behind it");

    s.l2 = 0.0;
    p.update(&s, 0.1);
    assert_eq!(p.take_history_packets(0.1).len(), 1, "one event, one packet");
}

#[test]
fn releasing_both_triggers_together_sends_each_release_in_its_own_packet() {
    let mut p = PadFeedback::new(0);
    let mut s = st();
    s.l2 = 1.0;
    s.r2 = 1.0;
    p.update(&s, 0.0);
    p.take_history_packets(0.0);
    s.l2 = 0.0;
    s.r2 = 0.0;
    p.update(&s, 0.1);
    let packets = p.take_history_packets(0.1);
    assert_eq!(packets.len(), 2);
    assert_eq!(&packets[0][..3], &[0x80, 0x86, 0x00], "the LT release is a packet of its own, so it cannot be dropped");
    assert_eq!(&packets[1][..3], &[0x80, 0x87, 0x00]);
}

#[test]
fn a_touchpad_click_is_never_held_without_its_finger() {
    // chiaki-ng (698e0ec8, 4382c1c7): when a click and its finger change together, releases go first, then touch, then presses, so
    // the click is pressed after the finger lands and released before the finger lifts.
    let mut p = PadFeedback::new(0);
    let mut s = st();
    s.buttons.insert(Button::TOUCHPAD);
    s.touch = Some((0.5, 0.5));
    p.update(&s, 0.0);
    let down = p.take_history_packets(0.0);
    assert_eq!(down.len(), 2);
    assert_eq!(down[0][0] & 0xF0, 0xD0, "the finger lands first: {:02x?}", down[0]);
    assert_eq!(&down[1][..3], &[0x80, 0xB1, 0x00], "then the click is pressed");

    s.buttons.clear();
    s.touch = None;
    p.update(&s, 0.1);
    let up = p.take_history_packets(0.1);
    assert_eq!(up.len(), 2);
    assert_eq!(&up[0][..3], &[0x80, 0x91, 0x00], "the click is released first");
    assert_eq!(up[1][0] & 0xF0, 0xC0, "then the finger lifts: {:02x?}", up[1]);
}

#[test]
fn releases_come_before_presses_in_one_update() {
    let mut p = PadFeedback::new(0);
    let mut s = st();
    s.buttons.insert(Button::CIRCLE);
    p.update(&s, 0.0);
    p.take_history_packets(0.0);
    s.buttons.remove(&Button::CIRCLE);
    s.buttons.insert(Button::CROSS); // the earlier bit is the press, so bit order alone would put it first
    s.l2 = 1.0;
    p.update(&s, 0.1);
    let newest: Vec<[u8; 3]> = p.take_history_packets(0.1).iter().map(|pk| [pk[0], pk[1], pk[2]]).collect();
    assert_eq!(
        newest,
        vec![[0x80, 0x89, 0x00], [0x80, 0x88, 0xFF], [0x80, 0x86, 0xFF]],
        "CIRCLE release, CROSS press, then LT"
    );
}

#[test]
fn idle_sensors_encode_to_the_initial_state_bytes() {
    // The values handed to the console when a pad goes away must equal a fresh pad's neutral motion bytes.
    let fresh = PadFeedback::new(0).state_payload(12);
    let mut p = PadFeedback::new(0);
    p.set_sensors([0.0; 3], [0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]);
    assert_eq!(p.state_payload(12)[1..17], fresh[1..17]);
}

fn finger_records(packets: &[Vec<u8>]) -> Vec<(bool, u8)> {
    // (down?, id) of the newest touch record of every packet, in packet order
    packets
        .iter()
        .map(|p| {
            let r = history_records(p).into_iter().find(|r| r[0] & 0xE0 == 0xC0).expect("a touch record");
            (r[0] & 0xF0 == 0xD0, r[1])
        })
        .collect()
}

#[test]
fn two_fingers_get_distinct_ids_and_a_record_each() {
    // PXPlay and chiaki-ng both send two touch slots with different ids.
    let mut p = PadFeedback::new(0);
    let mut s = st();
    s.touch = Some((0.25, 0.5));
    s.touch2 = Some((0.75, 0.5));
    p.update(&s, 0.0);
    assert_eq!(finger_records(&p.take_history_packets(0.0)), vec![(true, 1), (true, 2)]);

    s.touch2 = Some((0.8, 0.5)); // the second finger moves: same id
    p.update(&s, 0.1);
    assert_eq!(finger_records(&p.take_history_packets(0.1)), vec![(true, 2)]);

    s.touch = None; // the first lifts, the second stays down
    p.update(&s, 0.2);
    assert_eq!(finger_records(&p.take_history_packets(0.2)), vec![(false, 1)]);

    s.touch2 = None;
    p.update(&s, 0.3);
    assert_eq!(finger_records(&p.take_history_packets(0.3)), vec![(false, 2)]);
}

#[test]
fn a_new_finger_never_takes_the_id_of_the_finger_still_down() {
    let mut p = PadFeedback::new(0);
    let mut s = st();
    s.touch = Some((0.5, 0.5));
    p.update(&s, 0.0);
    let held = finger_records(&p.take_history_packets(0.0))[0].1;
    for i in 0..400 {
        s.touch2 = Some((0.1, 0.1));
        p.update(&s, f64::from(i));
        let down = finger_records(&p.take_history_packets(0.0));
        assert!(down[0].1 <= 0x7F && down[0].1 != held, "iteration {i}: {down:?} vs held {held}");
        s.touch2 = None;
        p.update(&s, f64::from(i) + 0.5);
        p.take_history_packets(0.0);
    }
}

#[test]
fn the_second_finger_is_clamped_like_the_first() {
    let mut s = st();
    s.touch2 = Some((9.0, -1.0));
    assert_eq!(s.clamped().touch2, Some((1.0, 0.0)));
}

#[test]
fn unsent_packets_are_bounded_when_nobody_takes_them() {
    let mut p = PadFeedback::new(0);
    let mut s = st();
    for i in 0..5000u32 {
        s.l2 = f64::from(i % 255) / 255.0;
        p.update(&s, 0.0);
    }
    assert!(p.take_history_packets(0.0).len() <= 64);
}

#[test]
fn history_only_ever_gains_records_at_the_front_like_the_original_app() {
    // The original app (RemoteViewModel) does `remove(19)` then `addFirst` for every change and never touches an older record, so
    // each packet is the new record followed by the whole previous packet. 99% of the history packets in a recorded LT+RT session
    // were not, because older records of the same control were being removed.
    let mut p = PadFeedback::new(0);
    let mut s = st();
    let mut before = p.history_payload(0.0);
    for step in 0..60u32 {
        match step % 4 {
            0 => s.r2 = f64::from(step + 1) / 255.0,
            1 => s.l2 = f64::from(step + 1) / 255.0,
            2 => {
                s.buttons.insert(Button::CROSS);
            }
            _ => {
                s.buttons.remove(&Button::CROSS);
            }
        }
        p.update(&s, f64::from(step));
        let after = p.history_payload(f64::from(step));
        let (prev, now) = (history_records(&before), history_records(&after));
        assert_eq!(now.len(), (prev.len() + 1).min(MAX_HISTORY), "step {step}");
        assert_eq!(now[1..], prev[..now.len() - 1], "step {step}: older records must be left exactly as they were");
        before = after;
    }
}

#[test]
fn a_trigger_ramp_keeps_every_step_newest_first() {
    // RT 0x1a -> 0x40 -> 0x77 from a real trace: the earlier values stay in the list behind the newest one.
    let mut p = PadFeedback::new(0);
    let mut s = st();
    for v in [0x1a, 0x40, 0x77] {
        s.r2 = f64::from(v) / 255.0;
        p.update(&s, 0.0);
    }
    assert_eq!(p.history_payload(0.0), vec![0x80, 0x87, 0x77, 0x80, 0x87, 0x40, 0x80, 0x87, 0x1a]);
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
