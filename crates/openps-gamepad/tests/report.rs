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

fn with_trigger_level(level: u8) -> OutState {
    let mut s = OutState::default();
    s.apply(&PadOutput::AdaptiveTriggers { pad: 0, left_type: 0x25, right_type: 0x21, left: [7; 10], right: [9; 10] });
    s.apply(&PadOutput::Intensity { haptic: 1, adaptive_trigger: level });
    s
}

fn common_of(s: &OutState) -> Vec<u8> {
    bluetooth(s, 0)[3..50].to_vec()
}

#[test]
fn trigger_intensity_is_written_as_the_pads_power_reduction() {
    // chiaki-ng (controllermanager.cpp, streamsession.cpp): byte 36 holds the trigger reduction in its high nibble (strong 0x00,
    // medium 0x60, weak 0x90, off 0xF0) and the second enable byte carries 0x40 so the pad reads it. The low nibble is the
    // vibration reduction, which is applied in software here, so it stays 0.
    for (level, byte) in [(1u8, 0x00u8), (2, 0x60), (3, 0x90), (0, 0xF0)] {
        let c = common_of(&with_trigger_level(level));
        assert_eq!(c[36], byte, "level {level}");
        assert_eq!(c[1] & 0x40, 0x40, "level {level}: the pad is told to read byte 36");
    }
}

#[test]
fn nothing_about_trigger_power_is_written_until_the_console_reports_a_level() {
    let mut s = OutState::default();
    s.apply(&PadOutput::AdaptiveTriggers { pad: 0, left_type: 0x25, right_type: 0x21, left: [7; 10], right: [9; 10] });
    let c = common_of(&s);
    assert_eq!((c[1] & 0x40, c[36]), (0, 0), "behaves as before the level was known");
}

#[test]
fn going_back_to_strong_rewrites_the_reduction_to_zero() {
    // the pad keeps its last reduction until it is told otherwise
    let mut s = with_trigger_level(3);
    assert_eq!(common_of(&s)[36], 0x90);
    s.apply(&PadOutput::Intensity { haptic: 1, adaptive_trigger: 1 });
    let c = common_of(&s);
    assert_eq!((c[1] & 0x40, c[36]), (0x40, 0x00));
}

#[test]
fn trigger_effects_are_released_when_the_console_turns_trigger_intensity_off() {
    let c = common_of(&with_trigger_level(1));
    assert_eq!(&c[10..13], &[0x21, 9, 9], "effects pass through unchanged at strong");
    assert_eq!(&c[21..24], &[0x25, 7, 7]);

    let c = common_of(&with_trigger_level(0));
    assert_eq!(c[0] & 0x0C, 0x0C, "both trigger blocks are still written");
    assert_eq!(&c[10..21], &[0u8; 11], "right trigger released");
    assert_eq!(&c[21..32], &[0u8; 11], "left trigger released");
}

#[test]
fn the_vibration_level_does_not_disturb_the_trigger_reduction() {
    let mut s = with_trigger_level(2);
    s.apply(&PadOutput::Intensity { haptic: 3, adaptive_trigger: 2 });
    s.apply(&PadOutput::Rumble { pad: 0, left: 255, right: 255 });
    let c = common_of(&s);
    assert_eq!(c[36], 0x60);
    assert_eq!((c[2], c[3]), (84, 84), "rumble is still scaled in software (weak)");
}

#[test]
fn usb_reports_carry_the_same_trigger_power_byte() {
    let u = usb(&with_trigger_level(3));
    assert_eq!((u[1 + 1] & 0x40, u[1 + 36]), (0x40, 0x90));
}

#[test]
fn usb_reports_use_id_2_and_no_checksum() {
    let mut s = OutState::default();
    s.apply(&PadOutput::Rumble { pad: 0, left: 9, right: 8 });
    let b = usb(&s);
    assert_eq!((b[0], b[3], b[4]), (0x02, 8, 9));
    assert_eq!(openps_gamepad::report::build(&s, Bus::Usb, 0).len(), 63);
}

#[test]
fn rumble_made_from_haptics_joins_the_consoles_rumble_by_taking_the_stronger_value() {
    let mut s = OutState::default();
    s.apply(&PadOutput::Rumble { pad: 0, left: 200, right: 10 });
    s.haptic_motor = 64;
    let c = common_of(&s);
    assert_eq!((c[2], c[3]), (64, 200), "right: haptics win; left: the console wins");
}

#[test]
fn haptic_rumble_alone_moves_both_motors_and_goes_through_the_vibration_level() {
    let mut s = OutState { rumble: Some((0, 0)), haptic_motor: 100, ..OutState::default() };
    let c = common_of(&s);
    assert_eq!((c[0] & 0x03, c[2], c[3]), (0x03, 100, 100));
    s.apply(&PadOutput::Intensity { haptic: 2, adaptive_trigger: 1 });
    let c = common_of(&s);
    assert_eq!((c[2], c[3]), (50, 50), "medium halves it, like console rumble");
}

#[test]
fn silenced_also_stops_haptic_rumble() {
    let s = OutState { rumble: Some((0, 0)), haptic_motor: 255, ..OutState::default() };
    let c = common_of(&s.silenced());
    assert_eq!((c[2], c[3]), (0, 0));
}
