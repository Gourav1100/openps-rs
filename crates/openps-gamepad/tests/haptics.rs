use openps_gamepad::haptics::{pair_report, pcm8, setup_report, silence_report, single_report, HapticStream, Out};

fn crc_ok(report: &[u8], id: u8) -> bool {
    let n = report.len() - 4;
    let mut crc = crc32fast::Hasher::new();
    crc.update(&[0xA2]);
    crc.update(&report[..n]);
    assert_eq!(report[0], id);
    report[n..] == crc.finalize().to_le_bytes()
}

/// A 10 ms console frame (30 stereo 16-bit pairs) whose every left sample is `l` and right sample is `r`.
fn frame(l: i16, r: i16) -> [u8; 120] {
    let mut f = [0u8; 120];
    for pair in f.as_chunks_mut::<4>().0 {
        pair[..2].copy_from_slice(&l.to_le_bytes());
        pair[2..].copy_from_slice(&r.to_le_bytes());
    }
    f
}

// ---- report layout: PXPlay's DualSenseBtHapticReport ---------------------------------------------------------------------
#[test]
fn a_single_block_report_has_the_control_bytes_then_the_counter_then_the_pcm() {
    let mut block = [0u8; 64];
    block[0] = 0x7F;
    block[63] = 0x81;
    let r = single_report(&block, 0x1F, 0xA3);
    assert_eq!(r.len(), 142);
    // report id, sequence in the high nibble, audio-control sub-packet 0x91 (7 bytes: mask 0xFE, four zeros... then 0xFF), the
    // block counter, and the sized PCM sub-packet 0x92 of 64 bytes
    assert_eq!(&r[..13], &[0x32, 0xF0, 0x91, 0x07, 0xFE, 0, 0, 0, 0, 0xFF, 0xA3, 0x92, 0x40]);
    assert_eq!((r[13], r[76]), (0x7F, 0x81));
    assert!(r[77..138].iter().all(|b| *b == 0));
    assert!(crc_ok(&r, 0x32));
}

#[test]
fn a_pair_report_carries_two_blocks_after_a_shorter_control_sub_packet() {
    let mut pair = [0u8; 128];
    pair[0] = 1;
    pair[64] = 2;
    pair[127] = 3;
    let r = pair_report(&pair, 0x02, 0x10);
    assert_eq!(&r[..10], &[0x32, 0x20, 0x91, 0x04, 0x72, 0x00, 0x30, 0x10, 0xD2, 0x40]);
    assert_eq!((r[10], r[74], r[137]), (1, 2, 3));
    assert!(crc_ok(&r, 0x32));
}

#[test]
fn silence_is_a_single_block_of_zeros() {
    let r = silence_report(5, 9);
    assert_eq!(&r[..13], &[0x32, 0x50, 0x91, 0x07, 0xFE, 0, 0, 0, 0, 0xFF, 9, 0x92, 0x40]);
    assert!(r[13..138].iter().all(|b| *b == 0));
    assert!(crc_ok(&r, 0x32));
}

#[test]
fn the_coil_setup_report_enables_the_haptic_coils_and_carries_the_trigger_reduction() {
    let r = setup_report(0x0B, 0x60);
    assert_eq!(r.len(), 78);
    assert_eq!(
        (r[0], r[1], r[2], r[3], r[4]),
        (0x31, 0xB0, 0x10, 0x00, 0x62),
        "valid flags: power save, reduce motor power and 0x20"
    );
    assert_eq!(r[12], 0, "power save control: nothing muted");
    assert_eq!(r[39], 0x60, "trigger reduction, as in the state report");
    assert_eq!(r[42], 1);
    assert!(r[5..12].iter().chain(&r[13..39]).chain(&r[40..42]).chain(&r[43..74]).all(|b| *b == 0));
    assert!(crc_ok(&r, 0x31));
}

// ---- samples ---------------------------------------------------------------------------------------------------------------
#[test]
fn pcm_is_reduced_to_the_high_byte_of_each_sample_and_scaled_by_the_console_level() {
    let f = frame(i16::MAX, i16::MIN);
    let strong = pcm8(&f, 1, 100);
    assert_eq!((strong.len(), strong[0], strong[1], strong[58], strong[59]), (60, 0x7F, 0x80, 0x7F, 0x80));
    assert_eq!(&pcm8(&f, 2, 100)[..2], &[0x3F, 0xC0], "medium is half");
    assert_eq!(&pcm8(&f, 3, 100)[..2], &[0x29, 0xD6], "weak is about a third");
    assert!(pcm8(&f, 0, 100).iter().all(|b| *b == 0), "off");
}

// ---- pacing: PXPlay's DualSenseBtHapticWriter ------------------------------------------------------------------------------
fn pump(s: &mut HapticStream, frames: usize, per_frame: &[u8; 120], tick_each: usize) -> Vec<Out> {
    let mut out = vec![];
    for _ in 0..frames {
        s.offer(per_frame, 1, 100);
        for _ in 0..tick_each {
            out.extend(s.tick());
        }
    }
    out
}

#[test]
fn nothing_is_sent_before_any_haptics_and_the_stream_does_not_ask_for_ticks() {
    let mut s = HapticStream::new(true);
    assert!(!s.wants_tick());
    assert_eq!(s.tick(), None);
    assert!(!s.delivering());
}

#[test]
fn full_blocks_are_cut_from_the_queue_and_paired_up_before_sending() {
    let mut s = HapticStream::new(true);
    let f = frame(0x4000, 0x4000); // 60 bytes of 0x40 per frame
    s.offer(&f, 1, 100);
    s.offer(&f, 1, 100); // 120 bytes queued
    assert!(s.wants_tick());
    assert_eq!(s.tick(), None, "the first block is held until its partner is ready");
    assert!(!s.delivering(), "nothing has gone out yet");
    s.offer(&f, 1, 100);
    s.offer(&f, 1, 100);
    match s.tick() {
        Some(Out::Pair(p)) => assert!(p.iter().all(|b| *b == 0x40), "two full blocks of the signal"),
        other => panic!("expected a pair, got {other:?}"),
    }
    s.accepted(true);
    assert!(s.delivering());
}

#[test]
fn without_pairing_each_block_goes_out_alone() {
    let mut s = HapticStream::new(false);
    let f = frame(0x4000, 0x4000);
    s.offer(&f, 1, 100);
    s.offer(&f, 1, 100);
    assert!(matches!(s.tick(), Some(Out::Single(b)) if b.iter().all(|x| *x == 0x40)));
}

#[test]
fn a_short_queue_waits_two_cycles_and_then_sends_what_it_has_padded_with_silence() {
    let mut s = HapticStream::new(false);
    s.offer(&frame(0x4000, 0x4000), 1, 100); // 60 bytes: less than a block
    assert_eq!(s.tick(), None, "first cycle: wait for more");
    assert_eq!(s.tick(), None, "second cycle: still waiting");
    match s.tick() {
        Some(Out::Single(b)) => {
            assert!(b[..60].iter().all(|x| *x == 0x40));
            assert!(b[60..].iter().all(|x| *x == 0), "the rest of the block is neutral");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn the_stream_parks_after_24_silent_cycles_and_wakes_on_the_next_signal() {
    let mut s = HapticStream::new(false);
    let f = frame(0x4000, 0x4000);
    pump(&mut s, 4, &f, 0);
    let mut sent = 0;
    for _ in 0..4 {
        if s.tick().is_some() {
            sent += 1;
            s.accepted(true);
        }
    }
    assert!(sent >= 3);
    // now only silence: it keeps sending zero blocks for a while so the coils settle, then stops
    let mut silent_sends = 0;
    while s.wants_tick() && silent_sends < 100 {
        if let Some(Out::Single(b)) = s.tick() {
            assert!(b.iter().all(|x| *x == 0));
            silent_sends += 1;
            s.accepted(true);
        }
    }
    assert!((20..=26).contains(&silent_sends), "about 24 silent blocks, got {silent_sends}");
    assert!(!s.wants_tick());
    assert!(!s.delivering());
    // signal again
    pump(&mut s, 2, &f, 0);
    assert!(s.wants_tick());
    assert!(s.tick().is_some());
}

#[test]
fn the_queue_never_holds_more_than_192_bytes_and_keeps_the_newest() {
    let mut s = HapticStream::new(false);
    for i in 0..20u8 {
        s.offer(&frame(i16::from(i) << 8, i16::from(i) << 8), 1, 100); // 60 bytes of value i per frame: 1200 bytes offered
    }
    let mut seen = vec![];
    while let Some(Out::Single(b)) = s.tick() {
        s.accepted(true);
        if b.iter().all(|x| *x == 0) {
            break; // the signal is used up; what follows is the silence that lets the stream park
        }
        seen.push(b[0]);
    }
    assert_eq!(seen.len(), 3, "192 bytes = three blocks");
    assert!(seen.iter().all(|v| *v >= 16), "only the newest frames survive: {seen:?}");
}

#[test]
fn a_refused_report_stops_the_delivering_flag() {
    let mut s = HapticStream::new(false);
    let f = frame(0x4000, 0x4000);
    pump(&mut s, 3, &f, 0);
    assert!(s.tick().is_some());
    s.accepted(true);
    assert!(s.delivering());
    assert!(s.tick().is_some());
    s.accepted(false);
    assert!(!s.delivering());
}

#[test]
fn stopping_asks_for_one_silence_report_if_the_coils_could_be_running() {
    let mut s = HapticStream::new(false);
    assert!(!s.stop(), "nothing was playing");
    let f = frame(0x4000, 0x4000);
    pump(&mut s, 3, &f, 0);
    assert!(s.tick().is_some());
    s.accepted(true);
    assert!(s.stop(), "a block went out: send silence");
    assert!(!s.wants_tick());
    assert!(!s.delivering());
    assert!(!s.stop(), "and only once");
}

#[test]
fn the_users_rumble_strength_scales_the_haptic_samples_and_stops_at_full_scale() {
    let f = frame(0x4000, -0x4000);
    assert_eq!(&pcm8(&f, 1, 100)[..2], &[0x40, 0xC0]);
    assert_eq!(&pcm8(&f, 1, 50)[..2], &[0x20, 0xE0]);
    assert!(pcm8(&f, 1, 0).iter().all(|b| *b == 0));
    assert_eq!(&pcm8(&f, 1, 200)[..2], &[0x7F, 0x80], "doubling 0x40 would be 0x80: it stops at the largest sample");
    assert_eq!(&pcm8(&f, 2, 200)[..2], &[0x40, 0xC0], "medium (half) and 200 % cancel out");
}
