//! Console -> client controller output carried in DATA chunks with payload kind 7, 9 or 11 (spec 02 9.5).

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PadOutput {
    /// kind 7: `[pad, left, right]`, 0..=255 each.
    Rumble { pad: u8, left: u8, right: u8 },
    /// kind 9: lightbar colour for a pad.
    Led { pad: u8, r: u8, g: u8, b: u8 },
    /// kind 9: haptic / adaptive-trigger intensity (only the layouts that carry them).
    Intensity { haptic: u8, adaptive_trigger: u8 },
    /// kind 11: adaptive trigger effects, one type byte + 10 parameter bytes per trigger.
    AdaptiveTriggers { pad: u8, left_type: u8, right_type: u8, left: [u8; 10], right: [u8; 10] },
}

/// `ec.a(type, params)`: type 5 is re-derived from its 10-bit mask; everything else passes through.
pub fn adaptive_trigger_type(t: u8, params: &[u8; 10]) -> u8 {
    if t != 5 {
        return t;
    }
    let mask = params[0] as u32 | ((params[1] as u32 & 3) << 8);
    if mask == 0 {
        return 5;
    }
    let bits: Vec<u32> = (0..10).filter(|i| mask >> i & 1 == 1).collect();
    if bits.len() == 2 && (2..=7).contains(&bits[0]) && bits[1] <= 8 {
        0x25
    } else {
        0x21
    }
}

/// Parse one blob. `data` is the DATA payload after the kind byte. Unknown or short blobs yield nothing.
pub fn parse(kind: u8, data: &[u8]) -> Vec<PadOutput> {
    let mut out = vec![];
    match kind {
        7 if data.len() == 3 => out.push(PadOutput::Rumble { pad: data[0], left: data[1], right: data[2] }),
        9 => {
            let l = data.len();
            let (o, long) = if l == 17 {
                (0, true)
            } else if l >= 12 {
                (8, l > 15)
            } else if l == 7 {
                (0, false)
            } else {
                return out;
            };
            if data.len() > o + 3 && data[o] < 4 && (!long || data.get(o + 7).is_some_and(|v| *v != 0)) {
                out.push(PadOutput::Led { pad: data[o], r: data[o + 1], g: data[o + 2], b: data[o + 3] });
            }
            if l == 25 {
                out.push(PadOutput::Intensity { haptic: data[20], adaptive_trigger: data[21] });
            } else if l == 17 {
                out.push(PadOutput::Intensity { haptic: data[12], adaptive_trigger: data[13] });
            }
        }
        11 if data.len() == 25 || data.len() == 33 => {
            let o = if data.len() == 25 { 0 } else { 8 };
            let (mut left, mut right) = ([0u8; 10], [0u8; 10]);
            left.copy_from_slice(&data[o + 5..o + 15]);
            right.copy_from_slice(&data[o + 15..o + 25]);
            out.push(PadOutput::AdaptiveTriggers {
                pad: data[o],
                left_type: adaptive_trigger_type(data[o + 1], &left),
                right_type: adaptive_trigger_type(data[o + 2], &right),
                left,
                right,
            });
        }
        _ => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rumble() {
        assert_eq!(parse(7, &[1, 200, 50]), vec![PadOutput::Rumble { pad: 1, left: 200, right: 50 }]);
        assert!(parse(7, &[1, 2]).is_empty());
    }

    #[test]
    fn led_layouts() {
        // short form (7 bytes): pad, r, g, b at offset 0
        assert_eq!(parse(9, &[2, 10, 20, 30, 0, 0, 0]), vec![PadOutput::Led { pad: 2, r: 10, g: 20, b: 30 }]);
        // 17 bytes: long form at offset 0, applied only if byte 7 != 0; carries intensities at 12/13
        let mut b = [0u8; 17];
        b[..4].copy_from_slice(&[0, 1, 2, 3]);
        b[7] = 1;
        b[12] = 3;
        b[13] = 2;
        assert_eq!(
            parse(9, &b),
            vec![PadOutput::Led { pad: 0, r: 1, g: 2, b: 3 }, PadOutput::Intensity { haptic: 3, adaptive_trigger: 2 }]
        );
        b[7] = 0;
        assert_eq!(parse(9, &b), vec![PadOutput::Intensity { haptic: 3, adaptive_trigger: 2 }]); // LED ignored
                                                                                                 // 25 bytes: offset 8, long form
        let mut c = [0u8; 25];
        c[8..12].copy_from_slice(&[3, 9, 8, 7]);
        c[15] = 1;
        c[20] = 1;
        c[21] = 3;
        assert_eq!(
            parse(9, &c),
            vec![PadOutput::Led { pad: 3, r: 9, g: 8, b: 7 }, PadOutput::Intensity { haptic: 1, adaptive_trigger: 3 }]
        );
        // pad index must be < 4
        let mut d = [0u8; 7];
        d[0] = 4;
        assert!(parse(9, &d).is_empty());
    }

    #[test]
    fn adaptive_triggers_layout_and_type_rewrite() {
        let mut b = [0u8; 25];
        b[0] = 1;
        b[1] = 2;
        b[2] = 5;
        b[5..15].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
        b[15..25].copy_from_slice(&[0b0000_1100, 0, 0, 0, 0, 0, 0, 0, 0, 0]); // mask with bits 2 and 3
        let out = parse(11, &b);
        assert_eq!(
            out,
            vec![PadOutput::AdaptiveTriggers {
                pad: 1,
                left_type: 2,
                right_type: 0x25,
                left: [1, 2, 3, 4, 5, 6, 7, 8, 9, 10],
                right: [12, 0, 0, 0, 0, 0, 0, 0, 0, 0]
            }]
        );
        // 33 bytes: offset 8
        let mut c = [0u8; 33];
        c[8] = 2;
        assert!(matches!(parse(11, &c)[0], PadOutput::AdaptiveTriggers { pad: 2, .. }));
        assert!(parse(11, &[0; 20]).is_empty());
    }

    #[test]
    fn type_five_cases() {
        let z = [0u8; 10];
        assert_eq!(adaptive_trigger_type(5, &z), 5); // release
        let mut p = z;
        p[0] = 0b0000_0111; // three bits
        assert_eq!(adaptive_trigger_type(5, &p), 0x21);
        p[0] = 0b0000_0011; // bits 0 and 1: lo < 2
        assert_eq!(adaptive_trigger_type(5, &p), 0x21);
        p[0] = 0b1000_0100; // bits 2 and 7
        assert_eq!(adaptive_trigger_type(5, &p), 0x25);
        assert_eq!(adaptive_trigger_type(9, &p), 9);
    }
}
