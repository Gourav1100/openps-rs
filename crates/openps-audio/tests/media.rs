#![cfg(target_os = "macos")]
use openps_audio::header::AudioHeader;
use openps_audio::media::{AudioOut, PcmOut, MAX_CONSECUTIVE_ERRORS};
use openps_core::events::MediaSink;
use openps_stream::audio::OpusFrame;
use openps_takion::proto::StreamInfoPayload;
use openps_testkit::ogg;
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Rec {
    pcm: Mutex<Vec<i16>>,
    volume: Mutex<Vec<f32>>,
}
struct RecOut(Arc<Rec>);
impl PcmOut for RecOut {
    fn push(&self, s: &[i16]) {
        self.0.pcm.lock().unwrap().extend_from_slice(s);
    }
    fn set_volume(&self, v: f32) {
        self.0.volume.lock().unwrap().push(v);
    }
}

fn rec_out() -> (AudioOut, Arc<Rec>) {
    let rec = Arc::new(Rec::default());
    let r2 = rec.clone();
    (AudioOut::with_output(Box::new(move |_h: &AudioHeader| Ok(Box::new(RecOut(r2.clone())) as Box<dyn PcmOut>))), rec)
}

fn hdr(ch: u8, rate: u32, frame: u32) -> Vec<u8> {
    let mut b = vec![ch, 16];
    b.extend(rate.to_be_bytes());
    b.extend(frame.to_be_bytes());
    b.extend(1u32.to_be_bytes());
    b
}
fn info(h: Option<Vec<u8>>) -> StreamInfoPayload {
    StreamInfoPayload { audio_header: h, ..Default::default() }
}
fn packets() -> Vec<OpusFrame> {
    let d = std::fs::read(format!("{}/tests/data/stereo_440_880.opus", env!("CARGO_MANIFEST_DIR"))).unwrap();
    ogg::opus_packets(&d).unwrap().into_iter().enumerate().map(|(i, data)| OpusFrame { id: i as u16, data }).collect()
}

#[test]
fn frames_are_decoded_and_delivered_in_order_with_the_right_volume() {
    let (a, rec) = rec_out();
    a.set_volume(40); // before the session: applied when the output opens
    a.stream_info(&info(Some(hdr(2, 48000, 480))));
    assert_eq!(a.problem(), None);
    assert_eq!(*rec.volume.lock().unwrap(), vec![0.4]);
    let frames = packets();
    for chunk in frames.chunks(3) {
        a.audio(chunk.to_vec());
    }
    let pcm = rec.pcm.lock().unwrap();
    assert_eq!(pcm.len(), frames.len() * 960);
    assert_eq!(a.counters().decoded, frames.len() as u64);
    assert!(pcm[960 * 20..960 * 21].iter().any(|&s| s.abs() > 3000), "decoded audio is not silence");
    drop(pcm);
    a.set_volume(250); // clamped
    assert_eq!(a.volume(), 100);
    assert_eq!(*rec.volume.lock().unwrap().last().unwrap(), 1.0);
}

#[test]
fn unusable_streams_report_a_reason_and_audio_is_ignored_not_crashed() {
    for (h, want) in [
        (None, "no audio description"),
        (Some(vec![1, 2, 3]), "not supported"),
        (Some(hdr(6, 48000, 480)), "channel"),
        (Some(hdr(2, 44100, 480)), "sample rate"),
        (Some(hdr(2, 48000, 7)), "frame size"),
    ] {
        let (a, rec) = rec_out();
        a.stream_info(&info(h.clone()));
        let p = a.problem().unwrap_or_else(|| panic!("no problem reported for {h:?}"));
        assert!(p.contains(want), "{p:?} should mention {want:?}");
        a.audio(packets());
        assert!(rec.pcm.lock().unwrap().is_empty());
        assert_eq!(a.counters().ignored_before_config, packets().len() as u64);
    }
}

#[test]
fn an_output_that_cannot_open_is_reported() {
    let a = AudioOut::with_output(Box::new(|_| Err("no device".into())));
    a.stream_info(&info(Some(hdr(2, 48000, 480))));
    assert!(a.problem().unwrap().contains("no device"));
    a.audio(packets()); // ignored
    assert_eq!(a.counters().decoded, 0);
}

#[test]
fn bad_frames_are_counted_skipped_and_a_long_run_resets_the_decoder() {
    let (a, rec) = rec_out();
    a.stream_info(&info(Some(hdr(2, 48000, 480))));
    let good = packets();
    a.audio(good[..5].to_vec());
    let bad: Vec<OpusFrame> =
        (0..MAX_CONSECUTIVE_ERRORS + 5).map(|i| OpusFrame { id: i as u16, data: vec![] }).collect();
    a.audio(bad);
    let c = a.counters();
    assert_eq!(c.errors, u64::from(MAX_CONSECUTIVE_ERRORS) + 5);
    assert_eq!(c.resets, 1);
    a.audio(good[5..30].to_vec()); // still works afterwards
    assert_eq!(a.counters().decoded, 30);
    assert_eq!(rec.pcm.lock().unwrap().len(), 30 * 960);
}

#[test]
fn a_new_session_starts_clean_and_a_failed_one_does_not_inherit_the_old_output() {
    let (a, rec) = rec_out();
    a.stream_info(&info(Some(hdr(2, 48000, 480))));
    a.audio(packets()[..10].to_vec());
    assert_eq!(rec.pcm.lock().unwrap().len(), 10 * 960);
    a.stream_info(&info(Some(vec![0; 14]))); // second session announces garbage
    assert!(a.problem().is_some());
    a.audio(packets()[..10].to_vec());
    assert_eq!(rec.pcm.lock().unwrap().len(), 10 * 960, "nothing is played into the old output");
}

#[test]
fn survives_garbage_frames_of_every_size() {
    let (a, _rec) = rec_out();
    a.stream_info(&info(Some(hdr(2, 48000, 480))));
    let mut x = 0x2545_F491_4F6C_DD1Du64;
    let frames: Vec<OpusFrame> = (0..4000)
        .map(|i| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let n = (x % 400) as usize;
            OpusFrame { id: i as u16, data: (0..n).map(|k| (x >> (k % 56)) as u8).collect() }
        })
        .collect();
    for c in frames.chunks(50) {
        a.audio(c.to_vec());
    }
    let c = a.counters();
    assert_eq!(c.decoded + c.errors, 4000);
}
