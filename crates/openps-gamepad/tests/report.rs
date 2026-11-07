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

// ---- firmware-aware rumble ---------------------------------------------------------------------------------------------
/// A real feature report 0x20 from a DualSense over Bluetooth (firmware 0x0630).
const FIRMWARE_REPORT: [u8; 64] = [
    0x20, 0x4a, 0x75, 0x6c, 0x20, 0x20, 0x34, 0x20, 0x32, 0x30, 0x32, 0x35, 0x31, 0x30, 0x3a, 0x31, 0x30, 0x3a, 0x33,
    0x32, 0x02, 0x00, 0x04, 0x00, 0x11, 0x07, 0x00, 0x00, 0x2a, 0x00, 0x10, 0x01, 0x01, 0xa8, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0x30, 0x06, 0, 0, 0x3c, 0, 0x01, 0, 0x0a, 0, 0x02, 0, 0x06, 0, 0, 0, 0x58, 0x25, 0xd2, 0x99,
];

#[test]
fn the_firmware_version_is_read_from_the_feature_report() {
    use openps_gamepad::report::firmware_version;
    assert_eq!(firmware_version(&FIRMWARE_REPORT), Some(0x0630));
    assert_eq!(firmware_version(&FIRMWARE_REPORT[..40]), None, "too short");
    let mut other = FIRMWARE_REPORT;
    other[0] = 0x05;
    assert_eq!(firmware_version(&other), None, "not the firmware report");
}

fn rumble_with_firmware(firmware: Option<u16>) -> Vec<u8> {
    let mut s = OutState::default();
    s.apply(&PadOutput::Rumble { pad: 0, left: 200, right: 100 });
    s.firmware = firmware;
    common_of(&s)
}

#[test]
fn newer_firmware_uses_the_second_vibration_flag_and_full_strength() {
    // chiaki-ng (controllermanager.cpp): firmware 0x0224 and up sets valid_flag2 0x04 and haptics select, values unchanged.
    let c = rumble_with_firmware(Some(0x0630));
    assert_eq!(c[0] & 0x03, 0x02, "haptics select only, not the compatible-vibration bit");
    assert_eq!(c[38] & 0x04, 0x04);
    assert_eq!((c[2], c[3]), (100, 200));
}

#[test]
fn older_firmware_uses_the_compatible_vibration_bit_with_halved_strength() {
    let c = rumble_with_firmware(Some(0x0223));
    assert_eq!(c[0] & 0x03, 0x03);
    assert_eq!(c[38] & 0x04, 0);
    assert_eq!((c[2], c[3]), (50, 100));
}

#[test]
fn an_unknown_firmware_keeps_the_behaviour_that_was_verified_before_it_was_read() {
    let c = rumble_with_firmware(None);
    assert_eq!(c[0] & 0x03, 0x03);
    assert_eq!(c[38] & 0x04, 0);
    assert_eq!((c[2], c[3]), (100, 200));
}

#[test]
fn the_second_vibration_flag_and_the_lightbar_setup_flag_share_a_byte() {
    let mut s = OutState::default();
    s.apply(&PadOutput::Rumble { pad: 0, left: 1, right: 1 });
    s.apply(&PadOutput::Led { pad: 0, r: 1, g: 2, b: 3 });
    s.firmware = Some(0x0630);
    assert_eq!(common_of(&s)[38], 0x06, "lightbar setup 0x02 and compatible vibration 2 0x04");
}

#[test]
fn silenced_keeps_the_firmware_so_the_stop_uses_the_same_mode() {
    let mut s = OutState { rumble: Some((9, 9)), firmware: Some(0x0630), ..OutState::default() };
    s.haptic_motor = 5;
    let c = common_of(&s.silenced());
    assert_eq!((c[2], c[3]), (0, 0));
    assert_eq!(c[38] & 0x04, 0x04);
}

// ---- rumble held back while haptics play ---------------------------------------------------------------------------------------
#[test]
fn while_haptics_play_console_rumble_is_held_back_and_everything_else_stays() {
    let mut s = OutState::default();
    s.apply(&PadOutput::Rumble { pad: 0, left: 200, right: 100 });
    s.apply(&PadOutput::Led { pad: 0, r: 1, g: 2, b: 3 });
    s.apply(&PadOutput::Intensity { haptic: 1, adaptive_trigger: 2 });
    s.haptic_motor = 60;
    s.firmware = Some(0x0630);
    let held = s.held_back_for_haptics();
    let c = common_of(&held);
    assert_eq!((c[2], c[3]), (0, 0), "no motor movement");
    assert_eq!(&c[44..47], &[1, 2, 3], "the lightbar stays");
    assert_eq!(c[36], 0x60, "so does the trigger setting");
    assert_eq!(held.firmware, Some(0x0630));
    assert_eq!(
        s.rumble,
        Some((200, 100)),
        "the remembered rumble is untouched, so it comes back when the haptics stop"
    );
}

#[test]
fn holding_back_a_pad_that_was_never_asked_to_rumble_adds_no_rumble() {
    let held = OutState::default().held_back_for_haptics();
    assert_eq!(held.rumble, None);
}

// ---- the user's rumble and trigger strength ---------------------------------------------------------------------------------
use openps_gamepad::report::{Probe, Tuning};

fn tuned(rumble_pct: u16, trigger_pct: u16) -> OutState {
    OutState { tuning: Tuning { rumble_pct, trigger_pct }, ..OutState::default() }
}

#[test]
fn the_default_tuning_changes_nothing() {
    assert_eq!(Tuning::default(), Tuning { rumble_pct: 100, trigger_pct: 100 });
    let mut s = OutState::default();
    s.apply(&PadOutput::Rumble { pad: 0, left: 200, right: 50 });
    assert_eq!(s.tuning, Tuning::default());
    let c = common_of(&s);
    assert_eq!(
        (c[2], c[3], c[1] & 0x40, c[36]),
        (50, 200, 0, 0),
        "no trigger flag unless something asks for a reduction"
    );
}

#[test]
fn rumble_strength_scales_what_the_console_asks_for_and_never_overflows() {
    let rumble = |pct| {
        let mut s = tuned(pct, 100);
        s.apply(&PadOutput::Rumble { pad: 0, left: 200, right: 100 });
        let c = common_of(&s);
        (c[3], c[2])
    };
    assert_eq!(rumble(0), (0, 0));
    assert_eq!(rumble(50), (100, 50));
    assert_eq!(rumble(100), (200, 100));
    assert_eq!(rumble(150), (255, 150), "left would be 300: it stops at full");
    assert_eq!(rumble(200), (255, 200));
}

#[test]
fn rumble_strength_and_the_consoles_level_multiply() {
    let mut s = tuned(50, 100);
    s.apply(&PadOutput::Rumble { pad: 0, left: 200, right: 200 });
    s.apply(&PadOutput::Intensity { haptic: 2, adaptive_trigger: 1 }); // medium: half
    assert_eq!(common_of(&s)[3], 50, "200 x 0.5 x 0.5");
}

#[test]
fn rumble_made_from_haptics_follows_the_strength_too() {
    let mut s = tuned(50, 100);
    s.rumble = Some((0, 0));
    s.haptic_motor = 100;
    assert_eq!(common_of(&s)[2], 50);
}

#[test]
fn trigger_strength_only_ever_reduces_the_pads_power_and_never_below_the_consoles_own_level() {
    let byte36 = |pct, console: Option<u8>| {
        let mut s = tuned(100, pct);
        if let Some(level) = console {
            s.apply(&PadOutput::Intensity { haptic: 1, adaptive_trigger: level });
        }
        let c = common_of(&s);
        (c[1] & 0x40, c[36])
    };
    assert_eq!(byte36(100, None), (0, 0x00), "nothing asked: nothing written");
    assert_eq!(byte36(100, Some(1)), (0x40, 0x00), "strong stays strong");
    assert_eq!(byte36(50, None), (0x40, 0x80), "half strength: 8 of 15 steps of reduction");
    assert_eq!(byte36(0, None), (0x40, 0xF0), "0 % is the pad's off");
    assert_eq!(byte36(90, Some(2)), (0x40, 0x60), "the console's medium (0x60) is already weaker than 90 %");
    assert_eq!(byte36(30, Some(2)), (0x40, 0xB0), "30 % is weaker than the console's medium");
}

#[test]
fn silenced_keeps_the_tuning() {
    let s = tuned(60, 40);
    assert_eq!(s.silenced().tuning, Tuning { rumble_pct: 60, trigger_pct: 40 });
    assert_eq!(s.held_back_for_haptics().tuning, Tuning { rumble_pct: 60, trigger_pct: 40 });
}

#[test]
fn a_rumble_probe_buzzes_both_motors_and_a_trigger_probe_resists_both_triggers() {
    let base = OutState::default();
    let c = common_of(&base.probing(Probe::Rumble));
    assert_eq!((c[2], c[3]), (200, 200));
    let c = common_of(&base.probing(Probe::Trigger));
    assert_eq!((c[10], c[21]), (0x01, 0x01), "continuous resistance on R2 and L2");
    assert_eq!((c[0] & 0x0C, c[2], c[3]), (0x0C, 0, 0), "triggers flagged, no rumble");
}

#[test]
fn a_probe_ignores_what_the_console_asked_for_but_follows_the_users_strength() {
    let mut s = tuned(50, 50);
    s.apply(&PadOutput::Rumble { pad: 0, left: 10, right: 10 });
    s.apply(&PadOutput::Intensity { haptic: 0, adaptive_trigger: 0 }); // the console has switched both off
    let c = common_of(&s.probing(Probe::Rumble));
    assert_eq!((c[2], c[3]), (100, 100), "200 at the user's 50 %, not silenced by the console's off");
    let c = common_of(&s.probing(Probe::Trigger));
    assert_eq!(c[10], 0x01, "the effect is not released by the console's off");
    assert_eq!(c[36], 0x80, "the user's 50 % applies");
}

// ---- a probe must end ---------------------------------------------------------------------------------------------------------
#[test]
fn when_a_trigger_probe_ends_the_triggers_are_explicitly_released() {
    // the pad keeps the last effect it was given until told otherwise: with no trigger flags in the next report the resistance would
    // stay on for ever (the console never asked for any, so nothing else would write them)
    let mut s = OutState::default();
    assert_eq!(
        common_of(&s)[0] & 0x0C,
        0,
        "nothing asked for yet: no trigger flags, so nothing would release the probe's effect"
    );
    s.settle_after_probe(Probe::Trigger);
    let c = common_of(&s);
    assert_eq!(c[0] & 0x0C, 0x0C, "both triggers are written");
    assert!(c[10..32].iter().all(|b| *b == 0), "as an effect-free block");
}

#[test]
fn when_a_rumble_probe_ends_the_motors_are_explicitly_stopped() {
    let mut s = OutState::default();
    assert_eq!(common_of(&s)[0] & 0x03, 0);
    s.settle_after_probe(Probe::Rumble);
    let c = common_of(&s);
    assert_eq!((c[0] & 0x03 != 0, c[2], c[3]), (true, 0, 0));
}

#[test]
fn settling_never_overrides_what_the_console_asked_for() {
    let mut s = OutState::default();
    s.apply(&PadOutput::Rumble { pad: 0, left: 90, right: 40 });
    s.apply(&PadOutput::AdaptiveTriggers { pad: 0, left_type: 0x25, right_type: 0x21, left: [7; 10], right: [9; 10] });
    s.settle_after_probe(Probe::Rumble);
    s.settle_after_probe(Probe::Trigger);
    let c = common_of(&s);
    assert_eq!((c[3], c[2]), (90, 40));
    assert_eq!((c[10], c[21]), (0x21, 0x25));
}

#[test]
fn each_probe_has_a_length_the_screen_can_wait_for() {
    use std::time::Duration;
    assert_eq!(Probe::Rumble.duration(), Duration::from_millis(700));
    assert_eq!(Probe::Trigger.duration(), Duration::from_secs(4));
}
