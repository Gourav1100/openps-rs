use crate::report::intensity_gain;

const PCM_FRAME_LEN: usize = 120;
const SAMPLE_BYTES: usize = 64;
const REPORT_LEN: usize = 142;

/// Build a Bluetooth haptics-only report from a 10 ms frame of 3 kHz interleaved stereo PCM.
///
/// The 120-byte frame contains 30 signed 16-bit little-endian stereo pairs. The controller report holds
/// 32 signed 8-bit pairs, so samples are reduced to their high byte and the final pair remains neutral.
pub fn bluetooth_report(pcm: &[u8; PCM_FRAME_LEN], seq: u8, counter: u8, intensity: u8) -> [u8; REPORT_LEN] {
    let mut report = [0u8; REPORT_LEN];
    report[0] = 0x32;
    report[1] = (seq & 0x0F) << 4;
    report[2] = 0x91;
    report[3] = 0x07;
    report[4] = 0xFE;
    report[9] = counter;
    report[11] = 0x92;
    report[12] = SAMPLE_BYTES as u8;

    let (num, den) = intensity_gain(intensity);
    let (num, den) = (i16::try_from(num).unwrap_or(1), i16::try_from(den).unwrap_or(1));
    let (samples, _) = pcm.as_chunks::<2>();
    for (sample, out) in samples.iter().zip(&mut report[13..13 + PCM_FRAME_LEN / 2]) {
        let value = i16::from_le_bytes([sample[0], sample[1]]);
        *out = (((value >> 8) * num) / den) as i8 as u8;
    }

    let mut crc = crc32fast::Hasher::new();
    crc.update(&[0xA2]);
    crc.update(&report[..REPORT_LEN - 4]);
    report[REPORT_LEN - 4..].copy_from_slice(&crc.finalize().to_le_bytes());
    report
}
