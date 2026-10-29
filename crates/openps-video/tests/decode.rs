#![cfg(target_os = "macos")]
use openps_video::nal::{self, Codec, ParameterSets};
use openps_video::vt::{Decoder, Error, Frame};

fn data(n: &str) -> Vec<u8> {
    std::fs::read(format!("{}/tests/data/{n}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

/// Split an Annex-B stream into access units: a new one starts at each keyframe-or-slice NAL that follows a
/// picture NAL (these test streams have one slice per frame and no B-frames).
fn access_units(codec: Codec, stream: &[u8]) -> Vec<Vec<u8>> {
    let mut units: Vec<Vec<u8>> = vec![];
    let mut cur: Vec<u8> = vec![];
    let mut have_picture = false;
    for n in nal::split(stream) {
        let k = nal::classify(codec, n);
        let is_pic = matches!(k, nal::Kind::Idr | nal::Kind::Slice);
        let starts_new =
            (is_pic && have_picture) || (matches!(k, nal::Kind::Vps | nal::Kind::Sps | nal::Kind::Pps) && have_picture);
        if starts_new {
            units.push(std::mem::take(&mut cur));
            have_picture = false;
        }
        have_picture |= is_pic;
        cur.extend([0, 0, 0, 1]);
        cur.extend(n);
    }
    if !cur.is_empty() {
        units.push(cur);
    }
    units
}

/// Mean absolute difference between two equal-length planes dumps (0..255 scale).
fn mad(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len());
    a.iter().zip(b).map(|(x, y)| (*x as i32 - *y as i32).unsigned_abs() as f64).sum::<f64>() / a.len() as f64
}

fn run(codec: Codec, name: &str) -> Vec<Frame> {
    let stream = data(&format!("{name}.annexb"));
    assert_eq!(nal::detect_codec(&stream), Some(codec));
    let mut dec = Decoder::new(codec, ParameterSets::from_annex_b(codec, &stream)).expect("decoder");
    let units = access_units(codec, &stream);
    assert_eq!(units.len(), 30);
    let mut frames = vec![];
    for (i, u) in units.iter().enumerate() {
        frames.extend(dec.decode_annex_b(u, i as i64 * 3000).unwrap_or_else(|e| panic!("unit {i}: {e}")));
    }
    frames
}

fn check(codec: Codec, name: &str) {
    let frames = run(codec, name);
    assert_eq!(frames.len(), 30, "one picture per access unit");
    let f = &frames[0];
    assert_eq!((f.width(), f.height()), (320, 192));
    assert!(matches!(
        f.pixel_format(),
        openps_video::vt::PIXEL_FORMAT_NV12_VIDEO | openps_video::vt::PIXEL_FORMAT_NV12_FULL
    ));
    // presentation times survive in order
    assert!(frames.windows(2).all(|w| w[0].pts < w[1].pts));
    // pixels match ffmpeg's software decode (hardware/software rounding may differ slightly)
    for (idx, tag) in [(0, "first"), (29, "last")] {
        let got = frames[idx].copy_planes();
        let want = data(&format!("{name}.{tag}.nv12"));
        let d = mad(&got, &want);
        assert!(d < 2.0, "{name} {tag}: mean abs diff {d}");
    }
    // and the picture is not just similar-looking noise: first and last frames differ (the test pattern animates)
    assert!(mad(&frames[0].copy_planes(), &frames[29].copy_planes()) > 0.5);
}

#[test]
fn h264_decodes_to_reference() {
    check(Codec::H264, "h264");
}

#[test]
fn hevc_decodes_to_reference() {
    check(Codec::Hevc, "hevc");
}

#[test]
fn refuses_to_start_on_a_delta_frame_and_recovers_at_keyframe() {
    let stream = data("h264.annexb");
    let units = access_units(Codec::H264, &stream);
    let mut dec = Decoder::new(Codec::H264, ParameterSets::from_annex_b(Codec::H264, &stream)).unwrap();
    assert!(matches!(dec.decode_annex_b(&units[1], 0), Err(Error::NeedKeyframe))); // P frame first
    assert!(dec.decode_annex_b(&units[0], 0).unwrap().len() == 1);
    dec.reset(); // simulated loss
    assert!(matches!(dec.decode_annex_b(&units[2], 0), Err(Error::NeedKeyframe)));
    assert_eq!(dec.decode_annex_b(&units[15], 0).unwrap().len(), 1); // next IDR (gop 15)
    assert_eq!(dec.decode_annex_b(&units[16], 0).unwrap().len(), 1);
}

#[test]
fn rejects_bad_setup_and_empty_input() {
    assert!(matches!(Decoder::new(Codec::H264, ParameterSets::default()), Err(Error::ParameterSets(_))));
    let hevc = data("hevc.annexb");
    let mut only_sps = ParameterSets::from_annex_b(Codec::Hevc, &hevc);
    only_sps.vps.clear();
    assert!(Decoder::new(Codec::Hevc, only_sps).is_err());
    let h264 = data("h264.annexb");
    let mut dec = Decoder::new(Codec::H264, ParameterSets::from_annex_b(Codec::H264, &h264)).unwrap();
    assert!(matches!(dec.decode_annex_b(&[], 0), Err(Error::Empty)));
    assert!(matches!(dec.decode_annex_b(&[0, 0, 1, 0x09, 0xF0], 0), Err(Error::Empty)));
    // delimiter only
}

#[test]
fn garbage_picture_data_errors_without_crashing_and_decoder_recovers() {
    let stream = data("h264.annexb");
    let units = access_units(Codec::H264, &stream);
    let mut dec = Decoder::new(Codec::H264, ParameterSets::from_annex_b(Codec::H264, &stream)).unwrap();
    dec.decode_annex_b(&units[0], 0).unwrap();
    let mut x = 0x9E37_79B9_7F4A_7C15u64;
    for round in 0..200 {
        let junk: Vec<u8> = (0..(20 + round % 400))
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            })
            .collect();
        let kind = if round % 2 == 0 { 0x41 } else { 0x65 }; // slice / IDR header byte
        let mut au = vec![0, 0, 0, 1, kind];
        au.extend(junk);
        let _ = dec.decode_annex_b(&au, 1); // any result is fine, a crash or hang is not
    }
    dec.reset();
    assert_eq!(dec.decode_annex_b(&units[15], 0).unwrap().len(), 1, "still decodes real data afterwards");
}

#[test]
fn midstream_resolution_change_rebuilds_the_decoder() {
    let big = data("h264.annexb");
    let small = data("h264_small.annexb");
    let (ub, us) = (access_units(Codec::H264, &big), access_units(Codec::H264, &small));
    let mut dec = Decoder::new(Codec::H264, ParameterSets::from_annex_b(Codec::H264, &big)).unwrap();
    // identical in-band sets at every keyframe must NOT disturb decoding across a GOP boundary
    for u in &ub[..17] {
        let f = dec.decode_annex_b(u, 0).unwrap();
        assert_eq!((f.len(), f[0].width(), f[0].height()), (1, 320, 192));
    }
    // the encoder restarts at another resolution: the first access unit carries new sets and rebuilds the session
    for u in &us {
        let f = dec.decode_annex_b(u, 0).unwrap();
        assert_eq!((f.len(), f[0].width(), f[0].height()), (1, 256, 144));
    }
    // and back again
    let f = dec.decode_annex_b(&ub[0], 0).unwrap();
    assert_eq!((f[0].width(), f[0].height()), (320, 192));
}
