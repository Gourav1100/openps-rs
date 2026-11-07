#![cfg(target_os = "macos")]
use openps_stream::video::Frame as Unit;
use openps_takion::proto::{ResolutionPayload, StreamInfoPayload};
use openps_video::nal::{self, Codec};
use openps_video::pipeline::{Pipeline, KEYFRAME_REQUEST_INTERVAL};

fn stream(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/tests/data/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

fn units(codec: Codec, name: &str) -> Vec<Unit> {
    let s = stream(name);
    let mut out: Vec<Vec<u8>> = vec![];
    let mut have = false;
    for n in nal::split(&s) {
        let k = nal::classify(codec, n);
        let pic = matches!(k, nal::Kind::Idr | nal::Kind::Slice);
        if have && (pic || matches!(k, nal::Kind::Sps | nal::Kind::Vps | nal::Kind::Pps)) {
            have = false;
            out.push(vec![]);
        }
        if out.is_empty() {
            out.push(vec![]);
        }
        have |= pic;
        out.last_mut().unwrap().extend([0, 0, 0, 1]);
        out.last_mut().unwrap().extend(n);
    }
    out.into_iter()
        .enumerate()
        .map(|(i, data)| Unit { id: i as u16, adaptive_index: 0, data, starts_with_start_code: true })
        .collect()
}

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

fn header_of(codec: Codec, name: &str) -> Vec<u8> {
    // the first access unit's parameter sets, as the console would send them in the stream info
    let s = stream(name);
    let mut h = vec![];
    for n in nal::split(&s) {
        if matches!(nal::classify(codec, n), nal::Kind::Vps | nal::Kind::Sps | nal::Kind::Pps) {
            h.extend([0, 0, 0, 1]);
            h.extend(n);
        } else {
            break;
        }
    }
    h
}

#[test]
fn codec_is_chosen_from_the_stream_header_and_both_decode_end_to_end() {
    for (codec, name) in [(Codec::Hevc, "hevc.annexb"), (Codec::H264, "h264.annexb")] {
        let mut p = Pipeline::new();
        assert_eq!(p.configure(&info(&header_of(codec, name))).unwrap(), codec);
        let mut shown = 0;
        for u in units(codec, name) {
            let r = p.push(&u);
            assert!(!r.request_keyframe);
            shown += r.pictures.len();
        }
        assert_eq!((shown, p.counters.decoded, p.counters.dropped), (30, 30, 0), "{name}");
    }
}

#[test]
fn configure_rejects_missing_or_unknown_headers() {
    let mut p = Pipeline::new();
    assert!(p.configure(&StreamInfoPayload::default()).is_err());
    assert!(p.configure(&info(&[])).is_err());
    assert!(p.configure(&info(&[0, 0, 0, 1, 0x09, 0xF0])).is_err()); // not parameter sets
    assert!(p.codec().is_none());
    // unconfigured pipelines drop quietly instead of panicking
    let u = &units(Codec::H264, "h264.annexb")[0];
    assert!(p.push(u).pictures.is_empty());
    assert_eq!(p.counters.dropped, 1);
}

#[test]
fn loss_waits_for_a_keyframe_and_requests_are_rate_limited() {
    let us = units(Codec::H264, "h264.annexb");
    let mut p = Pipeline::new();
    p.configure(&info(&header_of(Codec::H264, "h264.annexb"))).unwrap();
    assert_eq!(p.push(&us[0]).pictures.len(), 1);
    assert!(p.on_loss()); // first request goes out
                          // frames 1..14 are deltas: nothing shown, and no request storm
    let mut requests = 0;
    for u in &us[1..15] {
        let r = p.push(u);
        assert!(r.pictures.is_empty());
        requests += r.request_keyframe as u32;
    }
    assert_eq!(requests, 0, "within the interval no further request is made");
    std::thread::sleep(KEYFRAME_REQUEST_INTERVAL + std::time::Duration::from_millis(50));
    assert!(p.push(&us[15 - 14]).request_keyframe, "after the interval a still-blind pipeline asks again");
    // the next IDR (frame 15) restores output
    assert_eq!(p.push(&us[15]).pictures.len(), 1);
    assert_eq!(p.push(&us[16]).pictures.len(), 1);
    assert_eq!(p.counters.keyframe_requests, 2);
}

#[test]
fn hardware_support_probe_and_preference() {
    use openps_video::vt::{hardware_decode_supported, preferred_codec};
    assert!(hardware_decode_supported(Codec::H264)); // every supported Mac
    let want = if hardware_decode_supported(Codec::Hevc) { Codec::Hevc } else { Codec::H264 };
    assert_eq!(preferred_codec(), want);
    eprintln!("preferred codec on this machine: {:?}", preferred_codec());
}

#[test]
fn a_skipped_frame_id_blocks_delta_frames_until_the_next_keyframe() {
    let us = units(Codec::H264, "h264.annexb");
    let mut p = Pipeline::new();
    p.configure(&info(&header_of(Codec::H264, "h264.annexb"))).unwrap();
    for u in &us[..5] {
        assert_eq!(p.push(u).pictures.len(), 1);
    }
    // frame 5 never arrives (the stream layer skipped it); 6 is a perfectly decodable delta frame, but not safe to show
    let r = p.push(&us[6]);
    assert!(r.pictures.is_empty(), "must not show a picture built on a missing reference");
    assert!(r.request_keyframe);
    assert!(p.push(&us[7]).pictures.is_empty());
    assert_eq!(p.push(&us[15]).pictures.len(), 1); // IDR resumes (ids jump again, but a keyframe is self-contained)
    assert_eq!(p.push(&us[16]).pictures.len(), 1);
    // id wrap-around is not a gap
    let mut q = Pipeline::new();
    q.configure(&info(&header_of(Codec::H264, "h264.annexb"))).unwrap();
    let mut a = us[0].clone();
    a.id = 65535;
    let mut b = us[1].clone();
    b.id = 0;
    assert_eq!(q.push(&a).pictures.len(), 1);
    assert_eq!(q.push(&b).pictures.len(), 1);
}

#[test]
fn a_rebuilt_decoder_session_resumes_at_the_next_keyframe() {
    use openps_video::nal::ParameterSets;
    use openps_video::vt::{Decoder, Error};
    let us = units(Codec::H264, "h264.annexb");
    let header = header_of(Codec::H264, "h264.annexb");
    let mut dec = Decoder::new(Codec::H264, ParameterSets::from_annex_b(Codec::H264, &header)).unwrap();
    assert_eq!(dec.decode_annex_b(&us[0].data, 0).unwrap().len(), 1);
    assert_eq!(dec.decode_annex_b(&us[1].data, 1500).unwrap().len(), 1);
    dec.rebuild().expect("a new session from the same parameter sets");
    assert!(
        matches!(dec.decode_annex_b(&us[2].data, 3000), Err(Error::NeedKeyframe)),
        "a delta frame has no reference in the new session"
    );
    assert_eq!(dec.decode_annex_b(&us[15].data, 4500).unwrap().len(), 1, "the next keyframe resumes output");
    assert_eq!(dec.decode_annex_b(&us[16].data, 6000).unwrap().len(), 1);
}
