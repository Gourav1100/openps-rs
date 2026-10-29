//! Tester A4: NAL parsing on untrusted bytes (platform independent, no decoder involved).
use openps_video::nal::*;
use std::time::Instant;

fn rng(seed: u64) -> impl FnMut() -> u64 {
    let mut x = seed | 1;
    move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    }
}

fn has_start_code(d: &[u8]) -> bool {
    d.windows(3).any(|w| w == [0, 0, 1])
}

#[test]
fn split_output_is_always_inside_the_input_and_free_of_start_codes() {
    let mut r = rng(7);
    for _ in 0..20_000 {
        let n = (r() % 300) as usize;
        let data: Vec<u8> = (0..n).map(|_| [0, 0, 1, 0, 1, 3, 0x67, (r() & 0xFF) as u8][(r() % 8) as usize]).collect();
        let nals = split(&data);
        let total: usize = nals.iter().map(|n| n.len()).sum();
        assert!(total <= data.len());
        let base = data.as_ptr() as usize;
        for n in &nals {
            assert!(!n.is_empty());
            let p = n.as_ptr() as usize;
            assert!(p >= base && p + n.len() <= base + data.len(), "slice escapes the input");
            assert!(!has_start_code(n), "{n:?} contains a start code");
            assert_ne!(*n.last().unwrap(), 0, "trailing zeros are trimmed");
        }
    }
}

#[test]
fn emulation_prevention_bytes_are_preserved() {
    let nal = [0x65, 0x88, 0, 0, 3, 1, 0, 0, 3, 0, 0, 0, 3, 2];
    let mut s = vec![0, 0, 0, 1];
    s.extend(nal);
    assert_eq!(split(&s), vec![&nal[..]]);
    let a = analyse(Codec::H264, &s);
    assert_eq!(&a.avcc[4..], &nal);
}

#[test]
fn start_code_edge_cases() {
    assert!(split(&[0, 0, 0, 0, 0, 0, 0, 0]).is_empty());
    assert!(split(&[0, 0, 0, 1]).is_empty());
    assert_eq!(split(&[0, 0, 0, 0, 0, 1, 9]), vec![&[9u8][..]]); // many leading zeros
    assert_eq!(split(&[0, 0, 1, 9, 0, 0, 0, 1, 8]), vec![&[9u8][..], &[8u8][..]]); // 3 then 4 byte start code
    assert_eq!(split(&[0, 0, 1, 9, 0, 0, 1]), vec![&[9u8][..]]); // dangling start code at the very end
    assert_eq!(split(&[0, 0, 1, 0, 0, 1, 0, 0, 1, 7]), vec![&[7u8][..]]);
    assert_eq!(split(&[0, 0, 2, 5]), Vec::<&[u8]>::new()); // not a start code
}

#[test]
fn every_first_byte_classifies_without_panic_and_matches_the_spec_ranges() {
    for b in 0..=255u8 {
        for second in [0u8, 1, 0xFF] {
            for codec in [Codec::H264, Codec::Hevc] {
                let nal = [b, second, 0x80];
                let k = classify(codec, &nal);
                let t = nal_type(codec, &nal).unwrap();
                match (codec, k) {
                    (Codec::H264, Kind::Idr) => assert_eq!(t, 5),
                    (Codec::H264, Kind::Slice) => assert!((1..=4).contains(&t)),
                    (Codec::Hevc, Kind::Idr) => assert!((16..=21).contains(&t)),
                    (Codec::Hevc, Kind::Slice) => assert!(t <= 9),
                    _ => {}
                }
            }
        }
    }
}

#[test]
fn forbidden_zero_bit_and_enhancement_layers_are_not_filtered_info() {
    // Info (T08-11): a NAL whose forbidden_zero_bit is set (corrupt) or an HEVC NAL with nuh_layer_id > 0 is still passed
    // to the decoder as a picture. VideoToolbox rejects/ignores them, but they are counted as pictures.
    assert_eq!(classify(Codec::H264, &[0x80 | 0x65, 1]), Kind::Idr);
    assert_eq!(classify(Codec::H264, &[0x80 | 0x41, 1]), Kind::Slice);
    assert_eq!(classify(Codec::Hevc, &[0x80 | 0x26, 1]), Kind::Idr);
    assert_eq!(classify(Codec::Hevc, &[0x26, 0x09]), Kind::Idr); // layer id 1, tid 1
}

#[test]
fn analyse_then_avcc_to_annex_b_roundtrips_random_streams() {
    let mut r = rng(99);
    for _ in 0..5_000 {
        let mut s = vec![];
        let mut pics: Vec<Vec<u8>> = vec![];
        for _ in 0..(r() % 8) {
            let mut nal = vec![[0x65u8, 0x41, 0x67, 0x68, 0x06, 0x09][(r() % 6) as usize]];
            for _ in 0..(r() % 40) {
                nal.push(((r() % 254) + 1) as u8 | 0x04); // no zeros => no accidental start codes
            }
            s.extend(if r() & 1 == 0 { vec![0, 0, 1] } else { vec![0, 0, 0, 1] });
            s.extend(&nal);
            if matches!(classify(Codec::H264, &nal), Kind::Idr | Kind::Slice) {
                pics.push(nal);
            }
        }
        let a = analyse(Codec::H264, &s);
        let back = avcc_to_annex_b(&a.avcc).expect("own output parses");
        let got: Vec<Vec<u8>> = split(&back).into_iter().map(<[u8]>::to_vec).collect();
        assert_eq!(got, pics);
        assert_eq!(a.keyframe, pics.iter().any(|p| p[0] & 0x1F == 5));
    }
}

#[test]
fn avcc_parser_rejects_every_truncation_of_a_valid_unit() {
    let a = analyse(Codec::H264, &[0, 0, 0, 1, 0x65, 1, 2, 3, 0, 0, 1, 0x41, 9, 9]);
    for cut in 1..a.avcc.len() {
        let r = avcc_to_annex_b(&a.avcc[..cut]);
        // only cut points that fall exactly on a NAL boundary may parse
        if r.is_some() {
            assert_eq!(cut, 4 + 4); // first NAL (4 length + 4 bytes) ends at 8
        }
    }
    assert!(avcc_to_annex_b(&[0, 0, 0, 0]).is_some_and(|v| v == vec![0, 0, 0, 1]), "zero-length NAL");
    assert_eq!(avcc_to_annex_b(&[0x80, 0, 0, 0, 1]), None);
    assert_eq!(avcc_to_annex_b(&[0xFF; 3]), None);
}

#[test]
fn detect_codec_from_odd_headers() {
    let h264 = [0, 0, 0, 1, 0x67, 0x42, 0, 0x1f, 0, 0, 0, 1, 0x68, 0xce];
    assert_eq!(detect_codec(&h264), Some(Codec::H264));
    assert_eq!(detect_codec(&[0, 0, 0, 1, 0x40, 1, 0xc]), Some(Codec::Hevc));
    assert_eq!(detect_codec(&[0xFF; 100]), None);
    assert_eq!(detect_codec(&[]), None);
    // SPS before VPS: the whole header is scanned, so the VPS behind it still identifies HEVC
    assert_eq!(detect_codec(&[0, 0, 0, 1, 0x42, 1, 1, 0, 0, 0, 1, 0x40, 1]), Some(Codec::Hevc));
}

#[test]
fn detect_codec_skips_leading_delimiters_and_sei() {
    let h264 = [0, 0, 0, 1, 0x09, 0xF0, 0, 0, 0, 1, 0x67, 0x42, 0, 0x1f, 0, 0, 0, 1, 0x68, 0xce];
    let hevc =
        [0, 0, 0, 1, 0x46, 0x01, 0x50, 0, 0, 0, 1, 0x40, 0x01, 0x0c, 0, 0, 0, 1, 0x42, 1, 1, 0, 0, 0, 1, 0x44, 1];
    assert_eq!(detect_codec(&h264), Some(Codec::H264));
    assert_eq!(detect_codec(&hevc), Some(Codec::Hevc));
}

#[test]
fn parameter_set_helpers_on_degenerate_input() {
    let p = ParameterSets::from_annex_b(Codec::H264, &[]);
    assert!(!p.complete(Codec::H264) && p.ordered(Codec::H264).is_empty());
    let p = ParameterSets::from_annex_b(Codec::H264, &[0, 0, 1, 0x67, 1, 0, 0, 1, 0x68, 1]);
    assert!(p.complete(Codec::H264));
    assert!(!p.complete(Codec::Hevc));
    // header() on a stream that has pictures first returns nothing
    assert!(header(Codec::H264, &[0, 0, 1, 0x65, 1, 0, 0, 1, 0x67, 1]).is_empty());
    // access_units on pictures-only / sets-only / garbage
    assert_eq!(access_units(Codec::H264, &[0, 0, 1, 0x65, 1, 0, 0, 1, 0x41, 1]).len(), 2);
    assert_eq!(access_units(Codec::H264, &[0, 0, 1, 0x67, 1, 0, 0, 1, 0x68, 1]).len(), 1);
}

#[test]
fn pathological_one_byte_nal_flood_cost_is_bounded_info() {
    // Info (T08-12)
    // The stream layer caps a frame at MAX_BLOCK * MAX_UNITS (65000 * 512 = 33 MB). A hostile console could send
    // 33 MB of `00 00 01 67` (one-byte SPS NALs). Measure what analyse() does with 8 MB of it (scaled down 4x).
    let mut buf = Vec::with_capacity(8 << 20);
    while buf.len() < (8 << 20) {
        buf.extend([0, 0, 1, 0x67]);
    }
    let t = Instant::now();
    let a = analyse(Codec::H264, &buf);
    let el = t.elapsed();
    assert_eq!(a.in_band.sps.len(), buf.len() / 4);
    assert!(a.avcc.is_empty());
    assert!(el.as_secs() < 20, "took {el:?}");
    eprintln!("8 MB flood -> {} separately allocated SPS vectors in {el:?}", a.in_band.sps.len());
    // slices: avcc gets a 4 byte length for every 1 byte NAL: 2x blow-up bounded by the input
    let mut buf = Vec::with_capacity(8 << 20);
    while buf.len() < (8 << 20) {
        buf.extend([0, 0, 1, 0x41]);
    }
    let a = analyse(Codec::H264, &buf);
    assert_eq!(a.avcc.len(), buf.len() / 4 * 5);
}
