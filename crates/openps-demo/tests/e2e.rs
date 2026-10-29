//! End to end: the real Core + session stack + FEC + hardware decode + renderer, against the demo console.
#![cfg(target_os = "macos")]
use openps_audio::header::AudioHeader;
use openps_audio::media::{AudioOut, PcmOut};
use openps_audio::opus::OpusDecoder;
use openps_core::events::{AppEvent, EventSink, VecSink};
use openps_core::store::ConsoleStore;
use openps_core::types::{Device, SessionState, StreamOptions};
use openps_core::{Core, CoreBuilder};
use openps_demo::{DemoBackend, DemoConfig, DemoPsn, HEIGHT, WIDTH};
use openps_psn::store::MemoryStore;
use openps_takion::senkusha::SenkushaConfig;
use openps_video::media::{DecodingMedia, PictureSink};
use openps_video::nal::Codec;
use openps_video::render::Renderer;
use openps_video::vt::Frame;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

struct Counting {
    renderer: Renderer,
    pictures: AtomicU64,
    wrong_size: AtomicU64,
    render_errors: AtomicU64,
    unavailable: Mutex<Vec<String>>,
}
impl PictureSink for Counting {
    fn picture(&self, p: Frame) {
        self.pictures.fetch_add(1, Ordering::Relaxed);
        if (p.width(), p.height()) != (WIDTH as usize, HEIGHT as usize) {
            self.wrong_size.fetch_add(1, Ordering::Relaxed);
        }
        if self.renderer.show(&p).is_err() {
            self.render_errors.fetch_add(1, Ordering::Relaxed);
        }
    }
    fn unavailable(&self, r: &str) {
        self.unavailable.lock().unwrap().push(r.into());
    }
}

struct Rig {
    core: Core,
    events: Arc<VecSink>,
    out: Arc<Counting>,
    media: Arc<DecodingMedia>,
    _dir: tempfile::TempDir,
}

fn rig(cfg: DemoConfig) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let out = Arc::new(Counting {
        renderer: Renderer::new().unwrap(),
        pictures: 0.into(),
        wrong_size: 0.into(),
        render_errors: 0.into(),
        unavailable: Mutex::new(vec![]),
    });
    let media = Arc::new(DecodingMedia::new(out.clone(), None));
    let events = Arc::new(VecSink::default());
    let core = Core::new(CoreBuilder {
        store: ConsoleStore::open(dir.path().join("state.json")).unwrap(),
        secrets: Arc::new(MemoryStore::default()),
        backend: Arc::new(DemoBackend::new(cfg)),
        psn: Some(Arc::new(DemoPsn)),
        events: events.clone() as Arc<dyn EventSink>,
        media: media.clone(),
        settle_s: 0.0,
        senkusha: SenkushaConfig::default(),
    });
    Rig { core, events, out, media, _dir: dir }
}

fn pair_and_connect(r: &Rig, hevc: bool) {
    r.core.psn_complete(openps_demo::DEMO_SIGN_IN_URL).unwrap();
    let found = r.core.scan(&[]).unwrap();
    assert_eq!(found.len(), 1);
    let rec = r.core.register("127.0.0.1", Device::Ps5, "12345678", None).unwrap();
    r.core.connect(&rec.id, StreamOptions { hevc, ..Default::default() }).unwrap();
}

fn wait(limit: Duration, f: impl Fn() -> bool) -> bool {
    let end = Instant::now() + limit;
    while Instant::now() < end {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

fn run_stream(cfg: DemoConfig, seconds: u64) -> Rig {
    let r = rig(cfg);
    pair_and_connect(&r, cfg.codec == Codec::Hevc);
    assert!(
        wait(Duration::from_secs(10), || r.events.states().iter().any(|s| matches!(s, SessionState::Connected { .. }))),
        "never connected: {:?}",
        r.events.states()
    );
    std::thread::sleep(Duration::from_secs(seconds));
    r
}

fn finish(r: &Rig) {
    r.core.disconnect().unwrap();
    assert!(wait(Duration::from_secs(5), || matches!(r.core.session_state(), SessionState::Closed { .. })));
    assert!(!r.events.states().iter().any(|s| matches!(s, SessionState::Failed { .. })), "{:?}", r.events.states());
}

#[test]
fn hevc_stream_plays_at_the_right_size_and_rate() {
    let r = run_stream(DemoConfig::default(), 4);
    let n = r.out.pictures.load(Ordering::Relaxed);
    // 30 fps for ~4 s minus setup latency: be generous about the lower bound, strict about no corruption
    assert!(n >= 60, "only {n} pictures in 4 s");
    assert_eq!(r.out.wrong_size.load(Ordering::Relaxed), 0);
    assert_eq!(r.out.render_errors.load(Ordering::Relaxed), 0);
    assert!(r.out.unavailable.lock().unwrap().is_empty());
    assert_eq!(r.media.counters().dropped, 0, "a clean link must not drop a picture");
    finish(&r);
}

#[test]
fn h264_fallback_stream_plays() {
    let r = run_stream(DemoConfig { codec: Codec::H264, ..Default::default() }, 3);
    assert!(r.out.pictures.load(Ordering::Relaxed) >= 40);
    assert_eq!(r.out.wrong_size.load(Ordering::Relaxed), 0);
    assert_eq!(r.media.counters().dropped, 0);
    finish(&r);
}

#[test]
fn recoverable_loss_is_invisible_to_the_viewer() {
    // one unit lost in every 7th frame: FEC must repair every one of them
    let r = run_stream(DemoConfig { recoverable_loss_every: 7, ..Default::default() }, 4);
    let n = r.out.pictures.load(Ordering::Relaxed);
    assert!(n >= 60, "{n}");
    assert_eq!(r.media.counters().dropped, 0, "FEC repaired frames must all decode");
    assert_eq!(r.media.counters().keyframe_requests, 0);
    finish(&r);
}

#[test]
fn fatal_loss_recovers_at_the_next_keyframe_without_stalling() {
    // every 20th frame loses more than FEC can repair; keyframes come once per second
    let r = run_stream(DemoConfig { fatal_loss_every: 20, ..Default::default() }, 6);
    let n = r.out.pictures.load(Ordering::Relaxed);
    let last =
        r.events.0.lock().unwrap().iter().rev().find_map(|e| if let AppEvent::Stats(s) = e { Some(*s) } else { None });
    let _ = last;
    assert!(n >= 100, "stream stalled after loss: {n} pictures");
    assert_eq!(r.out.render_errors.load(Ordering::Relaxed), 0);
    assert!(r.media.counters().keyframe_requests >= 1, "loss must trigger a keyframe request");
    assert!(r.events.states().iter().all(|s| !matches!(s, SessionState::Failed { .. })));
    // and statistics are being reported while all of this happens
    assert!(r.events.0.lock().unwrap().iter().any(|e| matches!(e, AppEvent::Stats(s) if s.fps > 0.0)));
    finish(&r);
}

#[test]
fn reconnect_after_disconnect_starts_clean() {
    let r = run_stream(DemoConfig::default(), 1);
    finish(&r);
    let before = r.out.pictures.load(Ordering::Relaxed);
    let id = r.core.consoles()[0].id.clone();
    r.core.connect(&id, StreamOptions::default()).unwrap();
    assert!(
        wait(Duration::from_secs(10), || r.out.pictures.load(Ordering::Relaxed) > before + 10),
        "no video after reconnect"
    );
    assert_eq!(r.out.wrong_size.load(Ordering::Relaxed), 0);
    finish(&r);
}

// ---- audio ------------------------------------------------------------------------------------------------------------
#[derive(Default)]
struct Pcm(Mutex<Vec<i16>>);
struct PcmSink(Arc<Pcm>);
impl PcmOut for PcmSink {
    fn push(&self, s: &[i16]) {
        self.0 .0.lock().unwrap().extend_from_slice(s);
    }
    fn set_volume(&self, _: f32) {}
}

fn audio_rig(cfg: DemoConfig) -> (Rig, Arc<AudioOut>, Arc<Pcm>) {
    let mut r = rig(cfg);
    let pcm = Arc::new(Pcm::default());
    let p2 = pcm.clone();
    let audio = Arc::new(AudioOut::with_output(Box::new(move |_h: &AudioHeader| {
        Ok(Box::new(PcmSink(p2.clone())) as Box<dyn PcmOut>)
    })));
    // rebuild the core with the audio chained behind the video sink
    let dir = tempfile::tempdir().unwrap();
    let media = Arc::new(DecodingMedia::new(r.out.clone(), Some(audio.clone())));
    r.core = Core::new(CoreBuilder {
        store: ConsoleStore::open(dir.path().join("state.json")).unwrap(),
        secrets: Arc::new(MemoryStore::default()),
        backend: Arc::new(DemoBackend::new(cfg)),
        psn: Some(Arc::new(DemoPsn)),
        events: r.events.clone() as Arc<dyn EventSink>,
        media: media.clone(),
        settle_s: 0.0,
        senkusha: SenkushaConfig::default(),
    });
    r._dir = dir;
    r.media = media;
    (r, audio, pcm)
}

/// What a perfect decode of the demo clip sounds like.
fn reference_pcm() -> Vec<i16> {
    let clip = include_bytes!("../assets/demo_audio.opus");
    let packets = openps_testkit::ogg::opus_packets(clip).unwrap();
    let mut dec = OpusDecoder::new(AudioHeader::parse(&openps_demo::audio_header()).unwrap()).unwrap();
    let mut out = vec![];
    for p in packets.iter().take(300) {
        dec.decode(p, &mut out).unwrap();
    }
    out
}

fn run_audio(cfg: DemoConfig, seconds: u64) -> (Rig, Arc<AudioOut>, Arc<Pcm>) {
    let (r, a, p) = audio_rig(cfg);
    pair_and_connect(&r, cfg.codec == Codec::Hevc);
    assert!(wait(Duration::from_secs(10), || r
        .events
        .states()
        .iter()
        .any(|s| matches!(s, SessionState::Connected { .. }))));
    std::thread::sleep(Duration::from_secs(seconds));
    (r, a, p)
}

#[test]
fn audio_arrives_intact_in_order_alongside_video() {
    let (r, a, pcm) = run_audio(DemoConfig::default(), 3);
    assert_eq!(a.problem(), None);
    let got = pcm.0.lock().unwrap().clone();
    let c = a.counters();
    assert!(c.decoded >= 200, "only {} audio frames in 3 s (expect ~100/s)", c.decoded);
    assert_eq!(c.errors, 0);
    // the strongest check available: after encryption, packetisation, reassembly and decode, the samples are
    // bit-identical to decoding the clip directly
    let reference = reference_pcm();
    let n = 200 * 960;
    assert_eq!(&got[..n], &reference[..n], "audio differs from a direct decode of the clip");
    assert!(r.out.pictures.load(Ordering::Relaxed) > 40, "video keeps flowing while audio plays");
    finish(&r);
}

#[test]
fn audio_under_packet_loss_has_gaps_but_no_errors_and_never_stalls() {
    let (r, a, pcm) =
        run_audio(DemoConfig { recoverable_loss_every: 7, fatal_loss_every: 97, ..Default::default() }, 4);
    let c = a.counters();
    let expected_if_perfect = 400.0;
    assert!(f64::from(c.decoded as u32) > expected_if_perfect * 0.6, "audio nearly stopped under loss: {c:?}");
    assert_eq!(c.errors, 0, "lost packets are gaps, not decode errors");
    assert!(!pcm.0.lock().unwrap().is_empty());
    finish(&r);
}

#[test]
fn a_session_without_audio_still_plays_video() {
    let (r, a, _) = run_audio(DemoConfig { audio: false, ..Default::default() }, 2);
    assert_eq!(a.counters().decoded, 0);
    assert!(r.out.pictures.load(Ordering::Relaxed) > 30);
    finish(&r);
}
