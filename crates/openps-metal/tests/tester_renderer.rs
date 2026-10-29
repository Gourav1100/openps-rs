//! Tester A5: lifecycle and concurrency of the whole renderer thread (headless).
#![cfg(target_os = "macos")]
use openps_metal::{FrameMode, MetalRenderer, RenderSettings, Upscale};
use openps_video::nal::{self, Codec, ParameterSets};
use openps_video::vt::{Decoder, Frame, OutputFormat};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn decode(name: &str, n: usize, first_id: usize) -> Vec<Frame> {
    let s = std::fs::read(format!("{}/../openps-video/tests/data/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap();
    let (units, params) = (nal::access_units(Codec::H264, &s), ParameterSets::from_annex_b(Codec::H264, &s));
    let mut d = Decoder::new_with(Codec::H264, params, OutputFormat::Bgra).unwrap();
    let mut out = vec![];
    for k in 0..n {
        if k % units.len() == 0 {
            d.reset();
        }
        out.append(&mut d.decode_annex_b(&units[k % units.len()], (first_id + k) as i64 * 1500).unwrap());
    }
    out
}

fn renderer(s: RenderSettings) -> MetalRenderer {
    let r = MetalRenderer::new(Default::default(), s).expect("renderer");
    r.set_drawable_size(960, 540);
    r
}

fn rss_mb() -> f64 {
    let out = std::process::Command::new("/bin/ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().parse::<f64>().unwrap_or(0.0) / 1024.0
}

fn pace(r: &MetalRenderer, frames: Vec<Frame>, fps: f64) {
    let t0 = Instant::now();
    for (k, f) in frames.into_iter().enumerate() {
        if let Some(w) = (t0 + Duration::from_secs_f64(k as f64 / fps)).checked_duration_since(Instant::now()) {
            std::thread::sleep(w);
        }
        r.submit(f);
    }
}

fn busy() -> RenderSettings {
    RenderSettings {
        display_hz: 144,
        mode: FrameMode::Smooth,
        camera: true,
        upscale: Upscale::Qhd,
        ..Default::default()
    }
}

#[test]
fn creating_and_destroying_the_renderer_repeatedly_does_not_leak() {
    let fs = decode("h264.annexb", 10, 0);
    // warm up one-time allocations (shader cache, MetalFX)
    for _ in 0..3 {
        let r = renderer(busy());
        for f in decode("h264.annexb", 6, 0) {
            r.submit(f);
            std::thread::sleep(Duration::from_millis(8));
        }
    }
    let rss0 = rss_mb();
    let t0 = Instant::now();
    let n = 25;
    for _ in 0..n {
        let r = renderer(busy());
        for f in decode("h264.annexb", 8, 0) {
            r.submit(f);
            std::thread::sleep(Duration::from_millis(8));
        }
        drop(r);
    }
    let grew = rss_mb() - rss0;
    eprintln!(
        "{n} create/stream/drop cycles in {:?}; RSS growth {grew:.1} MB ({:.2} MB per cycle)",
        t0.elapsed(),
        grew / n as f64
    );
    drop(fs);
    assert!(grew < 120.0, "RSS grew {grew} MB over {n} cycles");
}

#[test]
fn stream_size_changes_mid_session_with_frames_in_flight() {
    let r = renderer(busy());
    let big = decode("h264.annexb", 40, 0);
    let small = decode("h264_small.annexb", 40, 40);
    eprintln!("sizes: {}x{} then {}x{}", big[0].width(), big[0].height(), small[0].width(), small[0].height());
    assert_ne!((big[0].width(), big[0].height()), (small[0].width(), small[0].height()));
    pace(&r, big, 60.0);
    pace(&r, small, 60.0);
    // and back again, ids continuing
    pace(&r, decode("h264.annexb", 40, 80), 60.0);
    std::thread::sleep(Duration::from_millis(200));
    let s = r.stats();
    assert_eq!(s.errors, 0, "{s:?}");
    assert!(s.presented > 100, "{s:?}");
}

#[test]
fn a_settings_storm_while_streaming_neither_deadlocks_nor_errors() {
    let r = Arc::new(renderer(RenderSettings::default()));
    let stop = Arc::new(AtomicBool::new(false));
    let (r2, stop2) = (r.clone(), stop.clone());
    let storm = std::thread::spawn(move || {
        let mut n = 0u64;
        while !stop2.load(Ordering::Relaxed) {
            r2.set_settings(RenderSettings {
                upscale: [Upscale::Off, Upscale::Qhd, Upscale::Uhd][(n % 3) as usize],
                display_hz: [30, 60, 75, 120, 144][(n % 5) as usize],
                mode: if n.is_multiple_of(2) { FrameMode::Smooth } else { FrameMode::ZeroLatency },
                camera: n.is_multiple_of(3),
                metalfx: n.is_multiple_of(4),
                sharpness: (n % 200) as f32 / 100.0,
                saturation: (n % 250) as f32 / 100.0,
                prompts: false,
                gen_factor: 0.0,
            });
            n += 1;
        }
        n
    });
    pace(&r, decode("h264.annexb", 120, 0), 60.0);
    stop.store(true, Ordering::Relaxed);
    let changes = storm.join().unwrap();
    eprintln!("{changes} settings changes during 2 s of streaming");
    std::thread::sleep(Duration::from_millis(200));
    let s = r.stats();
    assert_eq!(s.errors, 0, "{s:?}");
    assert!(s.presented > 60, "{s:?}");
    let t0 = Instant::now();
    drop(r);
    assert!(t0.elapsed() < Duration::from_secs(3));
}

#[test]
fn reset_from_another_thread_while_frames_arrive() {
    let r = Arc::new(renderer(busy()));
    let stop = Arc::new(AtomicBool::new(false));
    let (r2, stop2) = (r.clone(), stop.clone());
    let t = std::thread::spawn(move || {
        while !stop2.load(Ordering::Relaxed) {
            r2.reset();
            std::thread::sleep(Duration::from_millis(7));
        }
    });
    pace(&r, decode("h264.annexb", 120, 0), 60.0);
    stop.store(true, Ordering::Relaxed);
    t.join().unwrap();
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(r.stats().errors, 0);
    // after the storm a clean stream is displayed again
    let before = r.stats().real_frames;
    r.reset();
    pace(&r, decode("h264.annexb", 60, 0), 60.0);
    std::thread::sleep(Duration::from_millis(150));
    assert!(r.stats().real_frames > before + 30, "{:?}", r.stats());
}

#[test]
fn several_producers_submit_concurrently() {
    let r = Arc::new(renderer(busy()));
    let mut ts = vec![];
    for t in 0..4 {
        let r = r.clone();
        ts.push(std::thread::spawn(move || {
            for f in decode("h264.annexb", 40, t * 1000) {
                r.submit(f);
                std::thread::sleep(Duration::from_millis(5));
            }
        }));
    }
    for t in ts {
        t.join().unwrap();
    }
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(r.stats().errors, 0, "{:?}", r.stats());
}

/// Drop must be prompt for every pacing mode, including the exact-rate scheduler (`presentDrawable(atTime:)`) that sleeps until the next
/// slot on the render thread, and for a rate that is far below the display's (the scheduler may then sleep for several intervals).
#[test]
fn drop_is_prompt_in_the_exact_rate_scheduler_even_for_low_rates() {
    for hz in [7u16, 13, 75, 100] {
        let r = renderer(RenderSettings {
            display_hz: hz,
            camera: true,
            mode: FrameMode::ZeroLatency,
            ..Default::default()
        });
        pace(&r, decode("h264.annexb", 10, 0), 60.0);
        let t0 = Instant::now();
        drop(r);
        let el = t0.elapsed();
        eprintln!("display_hz {hz}: drop took {el:?}");
        assert!(el < Duration::from_millis(1500), "{hz}: {el:?}");
    }
}

/// With a rate of 1 Hz (`effective_rate` maps 0 to 1) the exact scheduler can sleep for seconds on the render thread, in a loop that
/// does not look at `stop` while sleeping. `StreamOptions::validate` only allows 30..144, so this needs a direct API user.
#[test]
fn drop_is_prompt_at_one_hertz_with_camera_on() {
    let r = renderer(RenderSettings { display_hz: 1, camera: true, ..Default::default() });
    pace(&r, decode("h264.annexb", 10, 0), 60.0);
    let t0 = Instant::now();
    drop(r);
    let el = t0.elapsed();
    eprintln!("display_hz 1: drop took {el:?}");
    assert!(el < Duration::from_secs(5), "{el:?}");
}

#[test]
fn submit_after_reset_after_drop_of_a_neighbour_and_double_reset_are_harmless() {
    let a = renderer(busy());
    let b = renderer(busy());
    a.reset();
    a.reset();
    pace(&a, decode("h264.annexb", 20, 0), 60.0);
    drop(b);
    pace(&a, decode("h264.annexb", 20, 20), 60.0);
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(a.stats().errors, 0);
}

/// Duplicate / out-of-order ids reach the renderer from the decoder when frames are lost: a frame whose id is older than the newest is
/// dropped by the timeline *after* it was copied to the GPU; make sure the stats stay consistent and nothing is shown twice.
#[test]
fn duplicate_and_out_of_order_frame_ids_are_ignored_by_the_renderer() {
    let r = renderer(busy());
    let fs = decode("h264.annexb", 30, 0);
    let dup = decode("h264.annexb", 30, 0); // same ids again
    pace(&r, fs, 60.0);
    pace(&r, dup, 60.0);
    std::thread::sleep(Duration::from_millis(200));
    let s = r.stats();
    assert_eq!(s.errors, 0, "{s:?}");
    assert!(s.real_frames <= 31, "ids that were already shown must not count as new frames: {}", s.real_frames);
}
