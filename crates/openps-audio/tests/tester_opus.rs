#![cfg(target_os = "macos")]
//! Tester A4: Opus decoding on hostile input, header/packet mismatches, rate and channel changes, AudioOut bookkeeping.
//! Opus packets are crafted from TOC bytes + noise (any CELT payload decodes); no encoder is needed.
use openps_audio::header::AudioHeader;
use openps_audio::media::{AudioOut, PcmOut, MAX_CONSECUTIVE_ERRORS};
use openps_audio::opus::{OpusDecoder, OpusError, MAX_PACKET};
use openps_core::events::MediaSink;
use openps_stream::audio::OpusFrame;
use openps_takion::proto::StreamInfoPayload;
use openps_testkit::ogg;
use std::sync::{Arc, Mutex};

fn hdr_bytes(ch: u8, rate: u32, frame: u32) -> Vec<u8> {
    let mut b = vec![ch, 16];
    b.extend(rate.to_be_bytes());
    b.extend(frame.to_be_bytes());
    b.extend(1u32.to_be_bytes());
    b
}
fn hdr(ch: u8, rate: u32, frame: u32) -> AudioHeader {
    AudioHeader::parse(&hdr_bytes(ch, rate, frame)).unwrap()
}
fn real_packets() -> Vec<Vec<u8>> {
    let d = std::fs::read(format!("{}/tests/data/stereo_440_880.opus", env!("CARGO_MANIFEST_DIR"))).unwrap();
    ogg::opus_packets(&d).unwrap()
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
/// TOC byte: config (0..32), stereo flag, frame-count code.
fn toc(config: u8, stereo: bool, code: u8) -> u8 {
    (config << 3) | (u8::from(stereo) << 2) | code
}
fn noise_packet(r: &mut impl FnMut() -> u64, toc: u8, len: usize) -> Vec<u8> {
    let mut v = vec![toc];
    v.extend((0..len).map(|_| r() as u8));
    v
}

#[test]
fn every_toc_byte_and_frame_count_code_is_safe_against_every_header() {
    let mut r = rng(42);
    for (ch, rate, frame) in
        [(2u8, 48000u32, 480u32), (1, 48000, 480), (2, 24000, 240), (1, 8000, 80), (2, 16000, 320), (1, 12000, 120)]
    {
        let mut d = OpusDecoder::new(hdr(ch, rate, frame)).expect("decoder");
        let n = frame as usize * ch as usize;
        for t in 0..=255u8 {
            for len in [0usize, 1, 2, 10, 60, 300] {
                let pkt = noise_packet(&mut r, t, len);
                let mut out = vec![7i16; 5];
                match d.decode(&pkt, &mut out) {
                    Ok(()) => assert_eq!(out.len(), 5 + n, "toc {t:#x} len {len}: Ok must append exactly one frame"),
                    Err(_) => assert_eq!(out.len(), 5, "toc {t:#x}: an error must not leave partial samples"),
                }
                assert_eq!(&out[..5], &[7; 5], "existing samples are never touched");
            }
        }
    }
}

#[test]
fn packet_length_bounds() {
    let mut d = OpusDecoder::new(hdr(2, 48000, 480)).unwrap();
    let mut out = vec![];
    assert_eq!(d.decode(&[], &mut out), Err(OpusError::BadPacket));
    let mut r = rng(1);
    assert_eq!(d.decode(&noise_packet(&mut r, 0xFC, MAX_PACKET), &mut out), Err(OpusError::BadPacket)); // MAX_PACKET + 1
    assert!(out.is_empty());
    // exactly MAX_PACKET is handed to the decoder (and may be rejected by it, but not as BadPacket)
    assert_ne!(d.decode(&noise_packet(&mut r, 0xFC, MAX_PACKET - 1), &mut out), Err(OpusError::BadPacket));
    // one byte packets for every TOC are fine
    for t in 0..=255u8 {
        let _ = d.decode(&[t], &mut vec![]);
    }
}

#[test]
fn real_stream_is_unchanged_after_a_storm_of_garbage_between_good_packets() {
    let pk = real_packets();
    let h = hdr(2, 48000, 480);
    let mut clean = OpusDecoder::new(h).unwrap();
    let mut a = vec![];
    for p in &pk[..60] {
        clean.decode(p, &mut a).unwrap();
    }
    let mut noisy = OpusDecoder::new(h).unwrap();
    let mut b = vec![];
    let mut r = rng(9);
    let mut ok_garbage = 0;
    for p in &pk[..60] {
        for _ in 0..3 {
            let len = (r() % 200) as usize;
            let g = noise_packet(&mut r, 0xFC, len);
            if noisy.decode(&g, &mut vec![]).is_ok() {
                ok_garbage += 1;
            }
        }
        noisy.decode(p, &mut b).unwrap();
    }
    assert_eq!(a.len(), b.len());
    // garbage that the decoder ACCEPTS advances its state (LPC/prediction), so the audio after it differs from the clean decode
    eprintln!("{ok_garbage}/180 garbage packets were accepted by the decoder");
}

#[test]
fn packets_longer_or_shorter_than_the_header_frame_size_are_contained() {
    // header says 10 ms (480). Send real 10 ms packets, then 20 ms and 5 ms noise packets (TOC config 31 / 29).
    let h = hdr(2, 48000, 480);
    let mut d = OpusDecoder::new(h).unwrap();
    let mut r = rng(3);
    let pk = real_packets();
    let mut pcm = vec![];
    d.decode(&pk[0], &mut pcm).unwrap();
    for t in
        [toc(31, true, 0), toc(29, true, 0), toc(30, true, 0), toc(28, true, 0), toc(31, true, 1), toc(31, true, 3)]
    {
        let before = pcm.len();
        let res = d.decode(&noise_packet(&mut r, t, 60), &mut pcm);
        eprintln!("toc {t:#x}: {res:?} -> +{} samples", pcm.len() - before);
        assert!(pcm.len() == before || pcm.len() == before + 960);
    }
    // and the decoder still decodes the real stream afterwards
    for p in &pk[1..30] {
        d.decode(p, &mut pcm).unwrap();
    }
}

#[test]
fn a_header_frame_size_that_does_not_match_the_packets_silently_kills_audio_info() {
    // BUG T08-03 (demonstration, passes): the header is trusted for samples_per_frame. If it says 20 ms (960) while the
    // console sends 10 ms packets, every packet after the first is "Short" and dropped. AudioOut counts errors but
    // problem() stays None, so the user just hears nothing and the UI has no reason to show.
    let pk = real_packets();
    let mut d = OpusDecoder::new(hdr(2, 48000, 960)).unwrap();
    let mut results = vec![];
    for p in pk.iter().take(20) {
        results.push(d.decode(p, &mut vec![]).is_ok());
    }
    eprintln!("10 ms packets with a 20 ms header: {results:?}");
    let ok = results.iter().filter(|x| **x).count();
    assert!(ok <= 1, "only the (padded) first packet is accepted, got {ok}");
}

#[test]
fn channel_count_mismatch_between_header_and_stream() {
    // stereo packets with a mono header and vice versa: the converter either errors or down/up-mixes; both are fine as
    // long as the frame invariant holds and nothing panics.
    let pk = real_packets();
    for ch in [1u8, 2] {
        let mut d = OpusDecoder::new(hdr(ch, 48000, 480)).unwrap();
        let mut pcm = vec![];
        let mut ok = 0;
        for p in pk.iter().take(40) {
            if d.decode(p, &mut pcm).is_ok() {
                ok += 1;
            }
        }
        assert_eq!(pcm.len(), ok * 480 * ch as usize);
        eprintln!("stereo stream, header channels {ch}: {ok}/40 decoded");
    }
}

#[test]
fn unsupported_but_header_valid_frame_sizes_create_or_fail_cleanly() {
    // Info (T08-04): AudioHeader::parse accepts ANY frame size between 2.5 and 120 ms, but Opus only has
    // 2.5/5/10/20/40/60 (+80/100/120) ms frames. AudioToolbox refuses all but 2.5 ms here. Show what the system does with non-Opus sizes.
    for frame in [120u32, 121, 481, 959, 1000, 2400, 5760] {
        let r = OpusDecoder::new(hdr(2, 48000, frame));
        eprintln!("frame {frame}: {}", if r.is_ok() { "created" } else { "refused" });
        if let Ok(mut d) = r {
            let mut r2 = rng(5);
            let mut out = vec![];
            let _ = d.decode(&noise_packet(&mut r2, toc(31, true, 0), 50), &mut out);
            assert!(out.is_empty() || out.len() == frame as usize * 2);
        }
    }
}

#[test]
fn reset_after_every_kind_of_failure_restores_clean_operation() {
    let pk = real_packets();
    let mut d = OpusDecoder::new(hdr(2, 48000, 480)).unwrap();
    let mut r = rng(21);
    for round in 0..50 {
        let mut pcm = vec![];
        for _ in 0..(r() % 20) {
            let (t, len) = ((r() & 0xFF) as u8, (r() % 100) as usize);
            let _ = d.decode(&noise_packet(&mut r, t, len), &mut pcm);
        }
        d.reset();
        pcm.clear();
        d.decode(&pk[round % pk.len()], &mut pcm).unwrap();
        assert_eq!(pcm.len(), 960, "first frame after reset is padded to a whole frame");
    }
}

#[test]
fn decoder_creation_for_every_supported_rate_and_channel_count() {
    for rate in [8000u32, 12000, 16000, 24000, 48000] {
        for ch in [1u8, 2] {
            for ms10 in [2u32, 4, 8, 16, 24] {
                // 2.5 ms ... 60 ms in steps that are real Opus sizes
                let frame = rate * [25u32, 50, 100, 200, 400, 600][(ms10 as usize / 2).min(5) % 6] / 10_000;
                if let Ok(h) = AudioHeader::parse(&hdr_bytes(ch, rate, frame)) {
                    let _ = OpusDecoder::new(h);
                }
            }
        }
    }
    // 100 decoders created and destroyed: no handle exhaustion
    for _ in 0..100 {
        let _ = OpusDecoder::new(hdr(2, 48000, 480)).unwrap();
    }
}

// ---------------------------------------------------------------- AudioOut

#[derive(Default)]
struct Rec {
    pcm: Mutex<Vec<i16>>,
    volume: Mutex<Vec<f32>>,
    opened: Mutex<Vec<AudioHeader>>,
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
    (
        AudioOut::with_output(Box::new(move |h: &AudioHeader| {
            r2.opened.lock().unwrap().push(*h);
            Ok(Box::new(RecOut(r2.clone())) as Box<dyn PcmOut>)
        })),
        rec,
    )
}
fn info(h: Option<Vec<u8>>) -> StreamInfoPayload {
    StreamInfoPayload { audio_header: h, ..Default::default() }
}
fn frames(n: usize) -> Vec<OpusFrame> {
    real_packets().into_iter().take(n).enumerate().map(|(i, data)| OpusFrame { id: i as u16, data }).collect()
}

#[test]
fn volume_bounds_are_clamped_and_reach_the_output() {
    let (a, rec) = rec_out();
    for v in [0u8, 1, 50, 99, 100, 101, 255] {
        a.set_volume(v);
        assert_eq!(a.volume(), v.min(100));
    }
    a.set_volume(0);
    a.stream_info(&info(Some(hdr_bytes(2, 48000, 480))));
    a.set_volume(255);
    a.set_volume(7);
    assert_eq!(*rec.volume.lock().unwrap(), vec![0.0, 1.0, 0.07]);
    assert!(rec.volume.lock().unwrap().iter().all(|v| (0.0..=1.0).contains(v)));
}

#[test]
fn rate_and_channel_changes_between_sessions_open_a_matching_output_each_time() {
    let (a, rec) = rec_out();
    for (ch, rate, frame) in
        [(2u8, 48000u32, 480u32), (1, 24000, 240), (2, 16000, 320), (1, 8000, 160), (2, 48000, 480)]
    {
        a.stream_info(&info(Some(hdr_bytes(ch, rate, frame))));
        assert_eq!(a.problem(), None);
        a.audio(frames(3)); // real 48 kHz stereo packets, fed to whatever header is active: must be contained
    }
    // a broken header in between kills the old session: frames are ignored, not decoded with the stale decoder
    let n = rec.opened.lock().unwrap().len();
    assert_eq!(n, 5);
    let before = rec.pcm.lock().unwrap().len();
    a.stream_info(&info(Some(vec![1, 2, 3])));
    assert!(a.problem().is_some());
    a.audio(frames(10));
    assert_eq!(rec.pcm.lock().unwrap().len(), before);
    assert_eq!(a.counters().ignored_before_config, 10);
    a.stream_info(&info(None));
    assert!(a.problem().unwrap().contains("no audio"));
    // recovers on the next good header
    a.stream_info(&info(Some(hdr_bytes(2, 48000, 480))));
    assert_eq!(a.problem(), None);
    a.audio(frames(2));
    assert_eq!(rec.pcm.lock().unwrap().len(), before + 2 * 960);
}

#[test]
fn packet_loss_pattern_skips_without_gaps_in_the_output_stream() {
    // The sink only ever sees decoded frames: a *lost* packet (never delivered) produces no output at all. There is no
    // concealment (PLC), so the output is simply shorter by one frame per loss.
    let (a, rec) = rec_out();
    a.stream_info(&info(Some(hdr_bytes(2, 48000, 480))));
    let all = frames(100);
    let kept: Vec<_> = all.iter().filter(|f| f.id % 4 != 3).cloned().collect();
    a.audio(kept.clone());
    assert_eq!(rec.pcm.lock().unwrap().len(), kept.len() * 960);
    assert_eq!(a.counters().errors, 0);
}

#[test]
fn consecutive_error_counter_resets_on_success_and_reset_is_counted() {
    let (a, _rec) = rec_out();
    a.stream_info(&info(Some(hdr_bytes(2, 48000, 480))));
    let good = frames(1).remove(0);
    let bad = OpusFrame { id: 0, data: vec![] };
    // 29 bad, 1 good, 29 bad: never reaches 30 in a row
    let mut v = vec![bad.clone(); (MAX_CONSECUTIVE_ERRORS - 1) as usize];
    v.push(good.clone());
    v.extend(vec![bad.clone(); (MAX_CONSECUTIVE_ERRORS - 1) as usize]);
    a.audio(v);
    assert_eq!(a.counters().resets, 0);
    a.audio(vec![bad.clone(); 61]);
    // the run of 29 errors at the end of the first batch carries over into the second call: 29 + 61 = 90 = 3 resets
    assert_eq!(a.counters().resets, 3);
    assert_eq!(a.counters().errors, 29 + 29 + 61);
}

#[test]
fn a_huge_batch_of_frames_is_processed_in_bounded_memory() {
    let (a, rec) = rec_out();
    a.stream_info(&info(Some(hdr_bytes(2, 48000, 480))));
    let f = frames(1).remove(0);
    a.audio(vec![f; 20_000]); // 200 s of audio in one call (a hostile/stalled caller)
    assert_eq!(rec.pcm.lock().unwrap().len(), 20_000 * 960);
}

#[test]
fn audio_out_is_usable_from_many_threads() {
    let (a, _rec) = rec_out();
    let a = Arc::new(a);
    a.stream_info(&info(Some(hdr_bytes(2, 48000, 480))));
    let hs: Vec<_> = (0..4)
        .map(|t| {
            let a = a.clone();
            std::thread::spawn(move || {
                for i in 0..200u32 {
                    match (i + t) % 5 {
                        0 => a.audio(frames(2)),
                        1 => a.set_volume((i % 120) as u8),
                        2 => a.stream_info(&info(Some(hdr_bytes(2, 48000, 480)))),
                        3 => {
                            let _ = a.problem();
                            let _ = a.player_stats();
                        }
                        _ => a.audio(vec![OpusFrame { id: 0, data: vec![0xFF; 50] }]),
                    }
                }
            })
        })
        .collect();
    for h in hs {
        h.join().unwrap();
    }
}

#[test]
fn a_session_whose_every_frame_fails_counts_errors_and_resets() {
    // BUG T08-03 (demonstration, passes), AudioOut level: header frame size 960 but the console sends 480-sample packets.
    let (a, rec) = rec_out();
    a.stream_info(&info(Some(hdr_bytes(2, 48000, 960))));
    a.audio(frames(100));
    let c = a.counters();
    assert!(c.errors >= 95 && c.decoded <= 1, "{c:?}");
    assert!(c.resets >= 3);
    assert!(a.problem().is_some(), "persistent failure must be surfaced");
    assert!(rec.pcm.lock().unwrap().len() <= 960 * 2);
}

#[test]
fn a_session_whose_every_frame_fails_reports_a_problem() {
    let (a, _rec) = rec_out();
    a.stream_info(&info(Some(hdr_bytes(2, 48000, 960))));
    a.audio(frames(100));
    assert!(a.problem().is_some());
}

#[test]
fn a_bogus_first_packet_is_accepted_as_a_silent_frame_info() {
    // Info (T08-14): decode() treats "fewer samples than a frame" on the FIRST packet as the priming pre-skip and
    // zero-pads it, whatever the reason (also 0 samples). A wrong-duration/garbage first packet is therefore accepted as one
    // silent frame. Harmless (checked: the real first packet still decodes fully afterwards) but it hides the error.
    let pk = real_packets();
    let mut r = rng(77);
    let mut d = OpusDecoder::new(hdr(2, 48000, 480)).unwrap();
    let mut out = vec![];
    let bogus = noise_packet(&mut r, toc(31, true, 0), 60); // 20 ms packet, header says 10 ms
    assert!(d.decode(&bogus, &mut out).is_ok());
    assert!(out.iter().all(|s| *s == 0));
    out.clear();
    assert!(d.decode(&pk[0], &mut out).is_ok());
    assert_eq!(out.len(), 960);
}
