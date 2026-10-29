#![cfg(target_os = "macos")]
use openps_core::events::MediaSink;
use openps_stream::video::Frame as Unit;
use openps_takion::proto::{ResolutionPayload, StreamInfoPayload};
use openps_video::media::{DecodingMedia, PictureSink};
use openps_video::nal::{self, Codec};
use openps_video::vt::Frame as Picture;
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Out {
    pics: Mutex<Vec<(usize, usize)>>,
    unavailable: Mutex<Vec<String>>,
}
impl PictureSink for Out {
    fn picture(&self, p: Picture) {
        self.pics.lock().unwrap().push((p.width(), p.height()));
    }
    fn unavailable(&self, r: &str) {
        self.unavailable.lock().unwrap().push(r.into());
    }
}

fn load(name: &str, codec: Codec) -> (Vec<u8>, Vec<Unit>) {
    let s = std::fs::read(format!("{}/tests/data/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap();
    let mut header = vec![];
    let mut units: Vec<Vec<u8>> = vec![];
    let mut have = false;
    let mut seen_picture = false;
    for (i, n) in nal::split(&s).into_iter().enumerate() {
        let k = nal::classify(codec, n);
        let pic = matches!(k, nal::Kind::Idr | nal::Kind::Slice);
        seen_picture |= pic;
        if !seen_picture {
            header.extend([0, 0, 0, 1]);
            header.extend(n);
        }
        if units.is_empty() || (have && (pic || !pic && i > 0 && !matches!(k, nal::Kind::Other))) {
            units.push(vec![]);
            have = false;
        }
        have |= pic;
        units.last_mut().unwrap().extend([0, 0, 0, 1]);
        units.last_mut().unwrap().extend(n);
    }
    let units = units
        .into_iter()
        .enumerate()
        .map(|(i, data)| Unit { id: i as u16, adaptive_index: 0, data, starts_with_start_code: true })
        .collect();
    (header, units)
}

fn info(h: Vec<u8>) -> StreamInfoPayload {
    StreamInfoPayload {
        resolution: vec![ResolutionPayload { width: Some(320), height: Some(192), video_header: Some(h) }],
        ..Default::default()
    }
}

#[test]
fn decodes_through_the_media_sink_interface_for_both_codecs() {
    for (name, codec) in [("hevc.annexb", Codec::Hevc), ("h264.annexb", Codec::H264)] {
        let out = Arc::new(Out::default());
        let m = DecodingMedia::new(out.clone(), None);
        let (h, units) = load(name, codec);
        m.stream_info(&info(h));
        for u in units {
            m.video(u);
            assert!(!m.take_keyframe_request());
        }
        assert_eq!(out.pics.lock().unwrap().len(), 30, "{name}");
        assert!(out.pics.lock().unwrap().iter().all(|&s| s == (320, 192)));
    }
}

#[test]
fn bad_header_is_reported_and_video_without_a_decoder_requests_nothing_fatal() {
    let out = Arc::new(Out::default());
    let m = DecodingMedia::new(out.clone(), None);
    m.stream_info(&info(vec![1, 2, 3]));
    assert_eq!(out.unavailable.lock().unwrap().len(), 1);
    let (_, units) = load("h264.annexb", Codec::H264);
    m.video(units[0].clone()); // no decoder: dropped, no panic
    assert!(out.pics.lock().unwrap().is_empty());
}

#[test]
fn a_delta_frame_first_sets_the_keyframe_flag_exactly_once() {
    let out = Arc::new(Out::default());
    let m = DecodingMedia::new(out.clone(), None);
    let (h, units) = load("h264.annexb", Codec::H264);
    m.stream_info(&info(h));
    m.video(units[3].clone()); // joined mid-GOP
    assert!(m.take_keyframe_request());
    assert!(!m.take_keyframe_request(), "flag is consumed");
    m.video(units[15].clone()); // IDR
    assert_eq!(out.pics.lock().unwrap().len(), 1);
}

#[test]
fn a_new_session_starts_with_a_fresh_decoder() {
    let out = Arc::new(Out::default());
    let m = DecodingMedia::new(out.clone(), None);
    let (h, units) = load("h264.annexb", Codec::H264);
    m.stream_info(&info(h.clone()));
    m.video(units[0].clone());
    m.stream_info(&info(h)); // reconnect
    m.video(units[1].clone()); // delta frame must NOT decode against the previous session's references
    assert_eq!(out.pics.lock().unwrap().len(), 1);
    assert_eq!(m.counters().decoded, 0, "counters reset with the pipeline");
}
