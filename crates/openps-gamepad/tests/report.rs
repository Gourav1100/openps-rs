use openps_gamepad::report::{bluetooth, usb, Bus, OutState};
use openps_stream::padblob::PadOutput;

fn crc_ok(b: &[u8; 78]) -> bool {
    let mut h = crc32fast::Hasher::new();
    h.update(&[0xA2]);
    h.update(&b[..74]);
    b[74..] == h.finalize().to_le_bytes()
}

#[test]
fn rumble_lightbar_and_triggers_land_at_the_documented_offsets() {
    let mut s = OutState::default();
    s.apply(&PadOutput::Rumble { pad: 0, left: 200, right: 50 });
    s.apply(&PadOutput::Led { pad: 0, r: 1, g: 2, b: 3 });
    let mut r = [0u8; 10];
    r[0] = 0x30;
    let mut l = [0u8; 10];
    l[2] = 7;
    s.apply(&PadOutput::AdaptiveTriggers { pad: 0, left_type: 0x25, right_type: 0x21, left: l, right: r });
    let b = bluetooth(&s, 5);
    assert_eq!((b[0], b[1], b[2]), (0x31, 0x50, 0x10));
    let c = &b[3..50];
    assert_eq!(c[0], 0x01 | 0x02 | 0x04 | 0x08);
    assert_eq!(c[1] & 0x04, 0x04);
    assert_eq!((c[2], c[3]), (50, 200), "right/light motor first, then left/heavy");
    assert_eq!(&c[10..13], &[0x21, 0x30, 0]);
    assert_eq!(&c[21..25], &[0x25, 0, 0, 7]);
    assert_eq!(&c[44..47], &[1, 2, 3]);
    assert!(crc_ok(&b));
}

#[test]
fn a_part_that_was_never_asked_for_is_left_alone() {
    let mut s = OutState::default();
    s.apply(&PadOutput::Led { pad: 0, r: 9, g: 9, b: 9 });
    let c = bluetooth(&s, 0)[3..50].to_vec();
    assert_eq!(c[0], 0, "no rumble or trigger flags");
}

#[test]
fn silenced_stops_rumble_and_triggers_but_keeps_the_lightbar() {
    let mut s = OutState::default();
    s.apply(&PadOutput::Rumble { pad: 0, left: 255, right: 255 });
    s.apply(&PadOutput::Led { pad: 0, r: 4, g: 5, b: 6 });
    let q = s.silenced();
    let b = bluetooth(&q, 0);
    assert_eq!((b[5], b[6]), (0, 0));
    assert_eq!(&b[47..50], &[4, 5, 6]);
    assert_eq!(s.rumble, Some((255, 255)), "the remembered state is untouched");
}

#[test]
fn console_intensity_follows_the_levels_strong_medium_weak_and_off() {
    // 0 off, 1 strong (full), 2 medium (x0.5), 3 weak (x0.33): the original app and chiaki-ng agree. The console's default is strong.
    let mut s = OutState::default();
    s.apply(&PadOutput::Rumble { pad: 0, left: 255, right: 120 });
    let b = bluetooth(&s, 0);
    assert_eq!((b[5], b[6]), (120, 255), "no level received yet: full strength");

    s.apply(&PadOutput::Intensity { haptic: 1, adaptive_trigger: 3 });
    let b = bluetooth(&s, 0);
    assert_eq!((b[5], b[6]), (120, 255), "strong");

    s.apply(&PadOutput::Intensity { haptic: 2, adaptive_trigger: 3 });
    let b = bluetooth(&s, 0);
    assert_eq!((b[5], b[6]), (60, 127), "medium");

    s.apply(&PadOutput::Intensity { haptic: 3, adaptive_trigger: 3 });
    let b = bluetooth(&s, 0);
    assert_eq!((b[5], b[6]), (39, 84), "weak");

    s.apply(&PadOutput::Intensity { haptic: 0, adaptive_trigger: 3 });
    let b = bluetooth(&s, 0);
    assert_eq!((b[5], b[6]), (0, 0), "off");
}

#[test]
fn usb_reports_use_id_2_and_no_checksum() {
    let mut s = OutState::default();
    s.apply(&PadOutput::Rumble { pad: 0, left: 9, right: 8 });
    let b = usb(&s);
    assert_eq!((b[0], b[3], b[4]), (0x02, 8, 9));
    assert_eq!(openps_gamepad::report::build(&s, Bus::Usb, 0).len(), 63);
}
