//! Annex-B bitstream handling for H.264 and HEVC: NAL splitting, classification, parameter sets, keyframe
//! detection, and conversion to the length-prefixed form hardware decoders want.
//!
//! The console sends Annex-B (`00 00 01` / `00 00 00 01` start codes). Parameter sets arrive in the stream info
//! (`ResolutionPayload.video_header`) and may be repeated in-band before keyframes.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    H264,
    Hevc,
}

/// Iterate the NAL units of an Annex-B buffer (start codes removed, trailing zero bytes of the previous NAL
/// trimmed). Tolerates 3- and 4-byte start codes, leading garbage and empty NALs.
pub fn split(data: &[u8]) -> Vec<&[u8]> {
    let mut starts = vec![]; // index of the first byte after each start code
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut out = vec![];
    for (k, &s) in starts.iter().enumerate() {
        let mut end = starts.get(k + 1).map_or(data.len(), |next| next - 3);
        while end > s && data[end - 1] == 0 {
            end -= 1; // the zero that belongs to a following 4-byte start code (or trailing padding)
        }
        if end > s {
            out.push(&data[s..end]);
        }
    }
    out
}

/// NAL unit type of the first byte(s) of `nal`.
pub fn nal_type(codec: Codec, nal: &[u8]) -> Option<u8> {
    let b = *nal.first()?;
    Some(match codec {
        Codec::H264 => b & 0x1F,
        Codec::Hevc => (b >> 1) & 0x3F,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Vps,
    Sps,
    Pps,
    /// Instantaneous refresh: decoding can start here.
    Idr,
    /// Non-IDR coded slice.
    Slice,
    /// Access-unit delimiter, SEI, filler, anything else that carries no picture data we care about.
    Other,
}

pub fn classify(codec: Codec, nal: &[u8]) -> Kind {
    let Some(t) = nal_type(codec, nal) else { return Kind::Other };
    match codec {
        Codec::H264 => match t {
            7 => Kind::Sps,
            8 => Kind::Pps,
            5 => Kind::Idr,
            1..=4 => Kind::Slice,
            _ => Kind::Other,
        },
        Codec::Hevc => match t {
            32 => Kind::Vps,
            33 => Kind::Sps,
            34 => Kind::Pps,
            16..=21 => Kind::Idr, // BLA, IDR, CRA: random access points
            0..=9 => Kind::Slice,
            _ => Kind::Other,
        },
    }
}

/// Guess the codec from a parameter-set header: HEVC starts with a VPS (type 32), H.264 with an SPS (type 7).
pub fn detect_codec(annex_b: &[u8]) -> Option<Codec> {
    let first = *split(annex_b).first()?.first()?;
    match (first & 0x1F, (first >> 1) & 0x3F) {
        (7, _) => Some(Codec::H264),
        (_, 32) => Some(Codec::Hevc),
        _ => None,
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParameterSets {
    pub vps: Vec<Vec<u8>>,
    pub sps: Vec<Vec<u8>>,
    pub pps: Vec<Vec<u8>>,
}

impl ParameterSets {
    pub fn from_annex_b(codec: Codec, data: &[u8]) -> Self {
        let mut p = ParameterSets::default();
        for nal in split(data) {
            match classify(codec, nal) {
                Kind::Vps => p.vps.push(nal.to_vec()),
                Kind::Sps => p.sps.push(nal.to_vec()),
                Kind::Pps => p.pps.push(nal.to_vec()),
                _ => {}
            }
        }
        p
    }

    /// Enough to create a decoder: H.264 needs SPS+PPS, HEVC needs VPS+SPS+PPS.
    pub fn complete(&self, codec: Codec) -> bool {
        !self.sps.is_empty() && !self.pps.is_empty() && (codec == Codec::H264 || !self.vps.is_empty())
    }

    /// All sets in decoder order (VPS, SPS, PPS) as one list, for APIs that take an array of parameter sets.
    pub fn ordered(&self, codec: Codec) -> Vec<&[u8]> {
        let mut v: Vec<&[u8]> = vec![];
        if codec == Codec::Hevc {
            v.extend(self.vps.iter().map(|x| x.as_slice()));
        }
        v.extend(self.sps.iter().map(|x| x.as_slice()));
        v.extend(self.pps.iter().map(|x| x.as_slice()));
        v
    }
}

/// One access unit after analysis.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccessUnit {
    /// Picture NAL units, each prefixed with its 4-byte big-endian length (what VideoToolbox decodes).
    pub avcc: Vec<u8>,
    pub keyframe: bool,
    /// Parameter sets found in-band (they may differ from the header's; a change needs a new decoder).
    pub in_band: ParameterSets,
}

/// Convert an Annex-B access unit: drop parameter sets, delimiters and SEI from the payload (decoders get them
/// through the format description), length-prefix the picture NAL units, and report keyframe / in-band sets.
pub fn analyse(codec: Codec, annex_b: &[u8]) -> AccessUnit {
    let mut avcc = vec![];
    let mut keyframe = false;
    let mut in_band = ParameterSets::default();
    for nal in split(annex_b) {
        match classify(codec, nal) {
            Kind::Vps => in_band.vps.push(nal.to_vec()),
            Kind::Sps => in_band.sps.push(nal.to_vec()),
            Kind::Pps => in_band.pps.push(nal.to_vec()),
            k @ (Kind::Idr | Kind::Slice) => {
                keyframe |= k == Kind::Idr;
                avcc.extend((nal.len() as u32).to_be_bytes());
                avcc.extend(nal);
            }
            Kind::Other => {}
        }
    }
    AccessUnit { avcc, keyframe, in_band }
}

/// Split an Annex-B byte stream into access units (one picture each, with the parameter sets that precede it), each
/// re-emitted with 4-byte start codes. For streams with one slice per picture and no B-frame reordering.
pub fn access_units(codec: Codec, stream: &[u8]) -> Vec<Vec<u8>> {
    let mut units: Vec<Vec<u8>> = vec![];
    let mut have_picture = false;
    for n in split(stream) {
        let k = classify(codec, n);
        let is_pic = matches!(k, Kind::Idr | Kind::Slice);
        let is_set = matches!(k, Kind::Vps | Kind::Sps | Kind::Pps);
        if units.is_empty() || (have_picture && (is_pic || is_set)) {
            units.push(vec![]);
            have_picture = false;
        }
        have_picture |= is_pic;
        let u = units.last_mut().unwrap();
        u.extend([0, 0, 0, 1]);
        u.extend(n);
    }
    units
}

/// The leading parameter sets of a stream (what the console reports as the stream header), Annex-B.
pub fn header(codec: Codec, stream: &[u8]) -> Vec<u8> {
    let mut h = vec![];
    for n in split(stream) {
        match classify(codec, n) {
            Kind::Vps | Kind::Sps | Kind::Pps => {
                h.extend([0, 0, 0, 1]);
                h.extend(n);
            }
            Kind::Idr | Kind::Slice => break,
            Kind::Other => {}
        }
    }
    h
}

/// Inverse of [`analyse`] for tests and tools: length-prefixed NAL units back to Annex-B with 4-byte start codes.
pub fn avcc_to_annex_b(avcc: &[u8]) -> Option<Vec<u8>> {
    let mut out = vec![];
    let mut i = 0;
    while i < avcc.len() {
        let len = u32::from_be_bytes(avcc.get(i..i + 4)?.try_into().ok()?) as usize;
        i += 4;
        out.extend([0, 0, 0, 1]);
        out.extend(avcc.get(i..i.checked_add(len)?)?);
        i += len;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const H264_SPS: &[u8] = &[0x67, 0x42, 0x00, 0x1f, 0xaa];
    const H264_PPS: &[u8] = &[0x68, 0xce, 0x3c, 0x80];
    const H264_IDR: &[u8] = &[0x65, 0x88, 0x84, 0x80];
    const H264_P: &[u8] = &[0x41, 0x9a, 0x24];
    const HEVC_VPS: &[u8] = &[0x40, 0x01, 0x0c, 0x01];
    const HEVC_SPS: &[u8] = &[0x42, 0x01, 0x01, 0x01];
    const HEVC_PPS: &[u8] = &[0x44, 0x01, 0xc0];
    const HEVC_IDR: &[u8] = &[0x26, 0x01, 0xaf]; // type 19
    const HEVC_TRAIL: &[u8] = &[0x02, 0x01, 0xd0]; // type 1

    fn annex_b(nals: &[&[u8]], four: bool) -> Vec<u8> {
        nals.iter()
            .flat_map(|n| {
                let mut v = if four { vec![0, 0, 0, 1] } else { vec![0, 0, 1] };
                v.extend(*n);
                v
            })
            .collect()
    }

    #[test]
    fn splitting_handles_both_start_code_lengths_and_padding() {
        let a = annex_b(&[H264_SPS, H264_PPS, H264_IDR], true);
        assert_eq!(split(&a), vec![H264_SPS, H264_PPS, H264_IDR]);
        let b = annex_b(&[H264_SPS, H264_PPS, H264_IDR], false);
        assert_eq!(split(&b), vec![H264_SPS, H264_PPS, H264_IDR]);
        let mut c = vec![0xAA, 0xBB]; // leading garbage
        c.extend(annex_b(&[H264_P], true));
        c.extend([0, 0, 0]); // trailing zeros
        assert_eq!(split(&c), vec![H264_P]);
        assert!(split(&[]).is_empty() && split(&[1, 2, 3]).is_empty() && split(&[0, 0, 1]).is_empty());
        assert_eq!(split(&[0, 0, 1, 0, 0, 1, 7]), vec![&[7u8][..]]); // empty NAL between start codes is dropped
    }

    #[test]
    fn classification_for_both_codecs() {
        for (nal, want) in [
            (H264_SPS, Kind::Sps),
            (H264_PPS, Kind::Pps),
            (H264_IDR, Kind::Idr),
            (H264_P, Kind::Slice),
            (&[0x09, 0x10][..], Kind::Other),
            (&[0x06, 0x01][..], Kind::Other),
        ] {
            assert_eq!(classify(Codec::H264, nal), want);
        }
        for (nal, want) in [
            (HEVC_VPS, Kind::Vps),
            (HEVC_SPS, Kind::Sps),
            (HEVC_PPS, Kind::Pps),
            (HEVC_IDR, Kind::Idr),
            (HEVC_TRAIL, Kind::Slice),
            (&[0x4E, 0x01][..], Kind::Other),
        ] {
            assert_eq!(classify(Codec::Hevc, nal), want);
        }
        assert_eq!(classify(Codec::H264, &[]), Kind::Other);
        // HEVC random access types 16..=21 are all keyframes (BLA_W_LP .. CRA)
        for t in 16u8..=21 {
            assert_eq!(classify(Codec::Hevc, &[t << 1, 1]), Kind::Idr, "{t}");
        }
    }

    #[test]
    fn codec_detection_and_parameter_sets() {
        let h264 = annex_b(&[H264_SPS, H264_PPS], true);
        let hevc = annex_b(&[HEVC_VPS, HEVC_SPS, HEVC_PPS], true);
        assert_eq!(
            (detect_codec(&h264), detect_codec(&hevc), detect_codec(&[1, 2])),
            (Some(Codec::H264), Some(Codec::Hevc), None)
        );
        let p = ParameterSets::from_annex_b(Codec::H264, &h264);
        assert!(p.complete(Codec::H264) && p.vps.is_empty());
        let p = ParameterSets::from_annex_b(Codec::Hevc, &hevc);
        assert!(p.complete(Codec::Hevc));
        assert_eq!(p.ordered(Codec::Hevc), vec![HEVC_VPS, HEVC_SPS, HEVC_PPS]); // decoder order
        assert!(!ParameterSets::from_annex_b(Codec::Hevc, &h264).complete(Codec::Hevc)); // HEVC needs a VPS
        assert!(!ParameterSets::default().complete(Codec::H264));
    }

    #[test]
    fn access_unit_analysis_strips_parameter_sets_and_length_prefixes_pictures() {
        let au = annex_b(&[&[0x09, 0xF0], H264_SPS, H264_PPS, &[0x06, 0x05], H264_IDR], true);
        let a = analyse(Codec::H264, &au);
        assert!(a.keyframe);
        assert_eq!(a.avcc, [&[0, 0, 0, 4][..], H264_IDR].concat());
        assert_eq!((a.in_band.sps.len(), a.in_band.pps.len()), (1, 1));
        let p = analyse(Codec::H264, &annex_b(&[H264_P], false));
        assert!(!p.keyframe && p.in_band == ParameterSets::default());
        assert_eq!(p.avcc, [&[0, 0, 0, 3][..], H264_P].concat());
        let both = analyse(Codec::Hevc, &annex_b(&[HEVC_VPS, HEVC_SPS, HEVC_PPS, HEVC_IDR, HEVC_TRAIL], true));
        assert!(both.keyframe);
        assert_eq!(both.avcc.len(), 4 + HEVC_IDR.len() + 4 + HEVC_TRAIL.len()); // multiple slices stay in one access unit
        assert!(analyse(Codec::H264, &[]).avcc.is_empty());
    }

    #[test]
    fn avcc_roundtrip_and_malformed_lengths() {
        let a = analyse(Codec::H264, &annex_b(&[H264_IDR, H264_P], true));
        assert_eq!(avcc_to_annex_b(&a.avcc).unwrap(), annex_b(&[H264_IDR, H264_P], true));
        assert_eq!(avcc_to_annex_b(&[0, 0, 0, 9, 1, 2]), None); // length beyond the data
        assert_eq!(avcc_to_annex_b(&[0, 0, 1]), None); // truncated length field
        assert_eq!(avcc_to_annex_b(&[0xFF, 0xFF, 0xFF, 0xFF, 1]), None); // huge length does not overflow
        assert_eq!(avcc_to_annex_b(&[]), Some(vec![]));
    }

    #[test]
    fn access_units_and_header_helpers() {
        let s = annex_b(&[H264_SPS, H264_PPS, H264_IDR, H264_P, H264_P, H264_SPS, H264_PPS, H264_IDR], true);
        let units = access_units(Codec::H264, &s);
        assert_eq!(units.len(), 4);
        assert_eq!(units[0], annex_b(&[H264_SPS, H264_PPS, H264_IDR], true));
        assert_eq!(units[1], annex_b(&[H264_P], true));
        assert_eq!(units[3], annex_b(&[H264_SPS, H264_PPS, H264_IDR], true));
        assert_eq!(header(Codec::H264, &s), annex_b(&[H264_SPS, H264_PPS], true));
        assert!(access_units(Codec::H264, &[]).is_empty() && header(Codec::Hevc, &[1, 2]).is_empty());
    }

    #[test]
    fn hostile_input_never_panics() {
        let mut x = 0x2545_F491_4F6C_DD1Du64;
        for _ in 0..30_000 {
            let n = {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                (x % 200) as usize
            };
            let data: Vec<u8> = (0..n)
                .map(|i| {
                    if i % 5 == 0 {
                        0
                    } else if i % 7 == 0 {
                        1
                    } else {
                        (x >> (i % 50)) as u8
                    }
                })
                .collect();
            for codec in [Codec::H264, Codec::Hevc] {
                let _ = analyse(codec, &data);
                let _ = ParameterSets::from_annex_b(codec, &data);
            }
            let _ = detect_codec(&data);
            let _ = avcc_to_annex_b(&data);
        }
    }
}
