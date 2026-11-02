use openps_gamepad::haptics::bluetooth_report;

fn crc_ok(report: &[u8; 142]) -> bool {
    let mut crc = crc32fast::Hasher::new();
    crc.update(&[0xA2]);
    crc.update(&report[..138]);
    report[138..] == crc.finalize().to_le_bytes()
}

#[test]
fn bluetooth_pcm_report_contains_the_downconverted_haptics_and_crc() {
    let mut pcm = [0u8; 120];
    pcm[..2].copy_from_slice(&0x7F00i16.to_le_bytes());
    pcm[2..4].copy_from_slice(&i16::MIN.to_le_bytes());
    let report = bluetooth_report(&pcm, 0x1F, 0xA3, 1);

    assert_eq!(report.len(), 142);
    assert_eq!(&report[..13], &[0x32, 0xF0, 0x91, 0x07, 0xFE, 0, 0, 0, 0, 0xA3, 0, 0x92, 0x40]);
    assert_eq!(
        &report[13..17],
        &[0x7F, 0x80, 0, 0],
        "PCM is signed 16-bit little-endian stereo reduced to signed 8-bit"
    );
    assert!(report[73..77].iter().all(|v| *v == 0), "the unused final stereo pair is neutral");
    assert!(report[77..138].iter().all(|v| *v == 0));
    assert!(crc_ok(&report));
}

#[test]
fn haptic_intensity_follows_the_console_levels_strong_medium_weak_and_off() {
    // Console levels (PXPlay RemoteViewModel, chiaki-ng streamconnection.h): 0 off, 1 strong (full), 2 medium (x0.5), 3 weak (x0.33).
    let mut pcm = [0u8; 120];
    pcm[..2].copy_from_slice(&i16::MAX.to_le_bytes());
    pcm[2..4].copy_from_slice(&i16::MIN.to_le_bytes());

    let strong = bluetooth_report(&pcm, 0, 0, 1);
    assert_eq!(&strong[13..15], &[0x7F, 0x80], "strong is full strength");
    let medium = bluetooth_report(&pcm, 0, 0, 2);
    assert_eq!(&medium[13..15], &[0x3F, 0xC0]);
    let weak = bluetooth_report(&pcm, 0, 0, 3);
    assert_eq!(&weak[13..15], &[0x29, 0xD6]);

    let off = bluetooth_report(&pcm, 0, 0, 0);
    assert_eq!(&off[13..73], &[0; 60]);
}
