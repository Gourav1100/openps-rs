#![cfg(target_os = "macos")]
//! Tester A4: VideoToolbox decoder / pipeline robustness against truncated, garbage and hostile access units.
//! Software-visible only: nothing is rendered, no window, no display change.
use openps_stream::video::Frame as Unit;
use openps_takion::proto::{ResolutionPayload, StreamInfoPayload};
use openps_video::nal::{self, Codec, ParameterSets};
use openps_video::pipeline::Pipeline;
use openps_video::vt::{Decoder, Error, OutputFormat};
use std::time::Instant;

fn data(n: &str) -> Vec<u8> {
    std::fs::read(format!("{}/tests/data/{n}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}
fn units(codec: Codec, name: &str) -> (Vec<u8>, Vec<Vec<u8>>) {
    let s = data(name);
    let u = nal::access_units(codec, &s);
    (s, u)
}
fn decoder(codec: Codec, stream: &[u8]) -> Decoder {
    Decoder::new(codec, ParameterSets::from_annex_b(codec, stream)).expect("decoder")
}

#[repr(C)]
struct Rusage {
    utime: [i64; 2],
    stime: [i64; 2],
    maxrss: i64,
    rest: [i64; 13],
}
extern "C" {
    fn getrusage(who: i32, usage: *mut Rusage) -> i32;
}
fn max_rss_mb() -> f64 {
    let mut r = Rusage { utime: [0; 2], stime: [0; 2], maxrss: 0, rest: [0; 13] };
    unsafe { getrusage(0, &mut r) };
    r.maxrss as f64 / 1048576.0 // bytes on macOS
}

fn rng(seed: u64) -> impl FnMut() -> u64 {
    let mut x = seed | 1;
    move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    }
}

#[test]
fn truncated_access_units_never_panic_and_the_decoder_recovers_at_the_next_keyframe() {
    for (codec, name) in [(Codec::H264, "h264.annexb"), (Codec::Hevc, "hevc.annexb")] {
        let (s, units) = units(codec, name);
        let mut dec = decoder(codec, &s);
        let mut r = rng(3);
        let mut recovered = 0;
        for round in 0..3 {
            for (i, u) in units.iter().enumerate() {
                // damage 1 out of 3 units: cut at a random byte
                let cut = if r().is_multiple_of(3) { (r() as usize) % u.len() } else { u.len() };
                let _ = dec.decode_annex_b(&u[..cut], (round * 100 + i) as i64 * 3000);
            }
            // a clean pass over the stream must work again (first unit is a keyframe)
            dec.reset();
            let mut got = 0;
            for (i, u) in units.iter().enumerate() {
                if let Ok(f) = dec.decode_annex_b(u, i as i64 * 3000) {
                    got += f.len();
                }
            }
            assert_eq!(got, units.len(), "{name} round {round}: decoder did not recover");
            recovered += 1;
        }
        assert_eq!(recovered, 3);
    }
}

#[test]
fn garbage_slice_payloads_are_errors_or_dropped_frames_not_crashes() {
    let (s, units) = units(Codec::H264, "h264.annexb");
    let mut dec = decoder(Codec::H264, &s);
    let mut r = rng(11);
    for round in 0..200 {
        let mut au = vec![0, 0, 0, 1, if round % 2 == 0 { 0x65 } else { 0x41 }];
        for _ in 0..(r() % 2000) {
            au.push(r() as u8);
        }
        let _ = dec.decode_annex_b(&au, round);
    }
    // and bit-flip every byte of one real keyframe region
    let key = &units[0];
    for i in (0..key.len()).step_by(7) {
        let mut bad = key.clone();
        bad[i] ^= 0xFF;
        let _ = dec.decode_annex_b(&bad, 0);
    }
    dec.reset();
    let mut ok = 0;
    for u in &units {
        ok += dec.decode_annex_b(u, 0).map(|f| f.len()).unwrap_or(0);
    }
    assert_eq!(ok, units.len());
}

#[test]
fn garbage_parameter_sets_fail_cleanly() {
    let cases: Vec<Vec<u8>> = vec![
        vec![0, 0, 0, 1, 0x67, 0x00, 0, 0, 0, 1, 0x68, 0x00],
        vec![0, 0, 0, 1, 0x67, 0xFF, 0xFF, 0xFF, 0xFF, 0, 0, 0, 1, 0x68, 0xFF],
        [vec![0, 0, 0, 1, 0x67], vec![0xAA; 5000], vec![0, 0, 0, 1, 0x68, 1]].concat(),
        [vec![0, 0, 0, 1, 0x67, 1, 2, 3], [0, 0, 0, 1, 0x68, 1].repeat(5000)].concat(),
    ];
    for c in &cases {
        let p = ParameterSets::from_annex_b(Codec::H264, c);
        let _ = Decoder::new(Codec::H264, p);
    }
    for c in [
        vec![0, 0, 0, 1, 0x40, 1, 0, 0, 0, 1, 0x42, 1, 0, 0, 0, 1, 0x44, 1],
        [vec![0, 0, 0, 1, 0x42, 1], vec![0xFF; 4000]].concat(),
    ] {
        let p = ParameterSets::from_annex_b(Codec::Hevc, &c);
        let r = Decoder::new(Codec::Hevc, p);
        assert!(r.is_err());
    }
    // missing sets
    assert!(matches!(Decoder::new(Codec::H264, ParameterSets::default()), Err(Error::ParameterSets(_))));
}

#[test]
fn state_machine_keyframe_gating_reset_and_empty_units() {
    let (s, units) = units(Codec::H264, "h264.annexb");
    let mut dec = decoder(Codec::H264, &s);
    // P frame first
    assert!(matches!(dec.decode_annex_b(&units[1], 0), Err(Error::NeedKeyframe)));
    // AUD only
    assert!(matches!(dec.decode_annex_b(&[0, 0, 0, 1, 0x09, 0xF0], 0), Err(Error::Empty)));
    assert!(matches!(dec.decode_annex_b(&[], 0), Err(Error::Empty)));
    assert!(matches!(dec.decode_annex_b(&[0xFF; 64], 0), Err(Error::Empty)));
    assert_eq!(dec.decode_annex_b(&units[0], 0).unwrap().len(), 1);
    assert_eq!(dec.decode_annex_b(&units[1], 3000).unwrap().len(), 1);
    dec.reset();
    assert!(matches!(dec.decode_annex_b(&units[2], 6000), Err(Error::NeedKeyframe)));
    dec.reset();
    dec.reset();
    assert_eq!(dec.decode_annex_b(&units[0], 0).unwrap().len(), 1);
}

#[test]
fn error_display_is_a_debug_dump_info() {
    // Info (T08-13): Display for the error is the Debug text, e.g. "ParameterSets(-1)": the user-facing message
    // "cannot start video decoder: ParameterSets(-1)" does not say what is wrong.
    assert_eq!(Error::ParameterSets(-1).to_string(), "ParameterSets(-1)");
    assert_eq!(Error::NeedKeyframe.to_string(), "NeedKeyframe");
}

#[test]
fn frames_outlive_the_decoder_and_can_cross_threads() {
    let (s, units) = units(Codec::H264, "h264.annexb");
    let mut dec = decoder(Codec::H264, &s);
    let mut frames = vec![];
    for (i, u) in units.iter().enumerate() {
        frames.extend(dec.decode_annex_b(u, i as i64 * 1500).unwrap());
    }
    drop(dec); // the session is gone, the retained pixel buffers must still be valid
    let h = std::thread::spawn(move || {
        let mut total = 0;
        for f in &frames {
            let p = f.copy_planes();
            assert_eq!(p.len(), 320 * 192 * 3 / 2);
            total += p.len();
            assert!(f.source_id() < 30);
        }
        total
    });
    assert!(h.join().unwrap() > 0);
}

#[test]
fn bgra_output_has_tightly_packed_rows() {
    let (s, units) = units(Codec::H264, "h264.annexb");
    let mut dec =
        Decoder::new_with(Codec::H264, ParameterSets::from_annex_b(Codec::H264, &s), OutputFormat::Bgra).unwrap();
    let f = dec.decode_annex_b(&units[0], 0).unwrap();
    assert_eq!(f[0].pixel_format(), openps_video::vt::PIXEL_FORMAT_BGRA);
    assert_eq!(f[0].copy_planes().len(), 320 * 192 * 4);
    assert!(!f[0].io_surface().is_null());
}

#[test]
fn source_id_survives_the_full_u16_range_but_pts_arithmetic_is_i64() {
    let (s, units) = units(Codec::H264, "h264.annexb");
    let mut dec = decoder(Codec::H264, &s);
    for id in [0u16, 1, 1000, 32767, 65535] {
        let f = dec.decode_annex_b(&units[0], i64::from(id) * 1500).unwrap();
        assert_eq!(f[0].source_id(), id);
    }
    // absurd timestamps must not crash CoreMedia
    for pts in [i64::MIN, -1, i64::MAX] {
        let _ = dec.decode_annex_b(&units[0], pts);
    }
}

#[test]
fn many_in_band_parameter_sets_are_handled_without_hanging() {
    let (s, units) = units(Codec::H264, "h264.annexb");
    let mut dec = decoder(Codec::H264, &s);
    let mut flood = vec![];
    for i in 0..100_000u32 {
        flood.extend([0, 0, 1, 0x67, 0x42, (i & 0xFF) as u8 | 1, (i >> 8) as u8 | 1]);
        flood.extend([0, 0, 1, 0x68, 0xce, (i & 0xFF) as u8 | 1]);
    }
    flood.extend(&units[0]);
    let t = Instant::now();
    let r = dec.decode_annex_b(&flood, 0);
    let el = t.elapsed();
    eprintln!("100k in-band parameter sets: {:?} in {el:?}, rss {:.0} MB", r.as_ref().map(|f| f.len()), max_rss_mb());
    assert!(el.as_secs() < 30, "took {el:?}");
    assert!(r.is_err(), "garbage parameter sets must not silently replace a working decoder with a broken one");
    // the old (working) session is kept: a clean keyframe still decodes
    dec.reset();
    assert_eq!(dec.decode_annex_b(&units[0], 0).unwrap().len(), 1);
}

#[test]
fn long_run_does_not_grow_memory() {
    let (s, units) = units(Codec::H264, "h264.annexb");
    let mut dec = decoder(Codec::H264, &s);
    let mut run = |n: usize| {
        for _ in 0..n {
            dec.reset();
            for (i, u) in units.iter().enumerate() {
                let f = dec.decode_annex_b(u, i as i64).unwrap();
                drop(f);
            }
        }
    };
    run(20);
    let before = max_rss_mb();
    run(300); // 9000 frames
    let after = max_rss_mb();
    assert!(after - before < 60.0, "RSS grew from {before:.0} to {after:.0} MB over 9000 decoded frames");
}

#[test]
fn creating_and_dropping_many_decoders_is_clean() {
    let (s, units) = units(Codec::H264, "h264.annexb");
    let before = max_rss_mb();
    for _ in 0..150 {
        let mut d = decoder(Codec::H264, &s);
        d.decode_annex_b(&units[0], 0).unwrap();
    }
    assert!(max_rss_mb() - before < 150.0, "{before} -> {}", max_rss_mb());
}

// ---------------------------------------------------------------- pipeline

fn info(header: &[u8]) -> StreamInfoPayload {
    StreamInfoPayload {
        resolution: vec![ResolutionPayload {
            width: Some(320),
            height: Some(192),
            video_header: Some(header.to_vec()),
        }],
        ..Default::default()
    }
}
fn unit(id: u16, data: Vec<u8>) -> Unit {
    Unit { id, adaptive_index: 0, data, starts_with_start_code: true }
}

#[test]
fn pipeline_configure_failures_leave_it_usable_and_do_not_request_keyframes_forever() {
    let (s, units) = units(Codec::H264, "h264.annexb");
    let mut p = Pipeline::new();
    // no resolution entries / empty header / garbage header
    assert!(p.configure(&StreamInfoPayload::default()).is_err());
    assert!(p.configure(&info(&[])).is_err());
    assert!(p.configure(&info(&[1, 2, 3])).is_err());
    assert!(p.configure(&info(&[0, 0, 0, 1, 0x67, 0x42, 0, 0x1f])).is_err()); // SPS only
                                                                              // unconfigured: units are dropped, no keyframe request storm
    let r = p.push(&unit(0, units[0].clone()));
    assert!(r.pictures.is_empty() && !r.request_keyframe);
    assert_eq!(p.counters.dropped, 1);
    // a later good configure works, also when called twice
    let header = nal::header(Codec::H264, &s);
    p.configure(&info(&header)).unwrap();
    p.configure(&info(&header)).unwrap();
    let r = p.push(&unit(0, units[0].clone()));
    assert_eq!(r.pictures.len(), 1);
}

#[test]
fn pipeline_frame_id_wraparound_and_gaps() {
    let (s, units) = units(Codec::H264, "h264.annexb");
    let header = nal::header(Codec::H264, &s);
    let mut p = Pipeline::new();
    p.configure(&info(&header)).unwrap();
    // wraps 65535 -> 0 without being treated as a gap
    assert_eq!(p.push(&unit(65534, units[0].clone())).pictures.len(), 1);
    assert_eq!(p.push(&unit(65535, units[1].clone())).pictures.len(), 1);
    assert_eq!(p.push(&unit(0, units[2].clone())).pictures.len(), 1);
    // a real gap before a P frame: refused and a keyframe requested (rate limited)
    let r = p.push(&unit(5, units[3].clone()));
    assert!(r.pictures.is_empty() && r.request_keyframe);
    let r = p.push(&unit(6, units[4].clone()));
    assert!(r.pictures.is_empty() && !r.request_keyframe, "second request within 500 ms is suppressed");
    // duplicate id (a replay of the same frame id): treated as a gap too
    assert_eq!(p.push(&unit(7, units[0].clone())).pictures.len(), 1);
    let r = p.push(&unit(7, units[1].clone()));
    assert!(r.pictures.is_empty(), "duplicate frame id before a P frame");
}

#[test]
fn pipeline_survives_random_units() {
    let (s, units) = units(Codec::H264, "h264.annexb");
    let header = nal::header(Codec::H264, &s);
    let mut p = Pipeline::new();
    p.configure(&info(&header)).unwrap();
    let mut r = rng(5);
    for i in 0..3000u32 {
        let d = match r() % 4 {
            0 => units[(r() as usize) % units.len()].clone(),
            1 => vec![],
            2 => (0..(r() % 300)).map(|_| r() as u8).collect(),
            _ => {
                let u = &units[(r() as usize) % units.len()];
                u[..(r() as usize) % u.len()].to_vec()
            }
        };
        let _ = p.push(&unit(i as u16, d));
        if i % 500 == 0 {
            let _ = p.on_loss();
        }
    }
    assert!(p.counters.dropped > 0 && p.counters.decoded > 0);
}
