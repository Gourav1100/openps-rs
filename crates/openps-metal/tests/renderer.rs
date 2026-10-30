//! The whole renderer thread with real decoded frames, headless (no window): the pieces that do not need a screen.
#![cfg(target_os = "macos")]
use openps_metal::{FrameMode, MetalRenderer, RenderSettings, Upscale};
use openps_video::nal::{self, Codec, ParameterSets};
use openps_video::vt::{Decoder, Frame, OutputFormat};
use std::time::{Duration, Instant};

fn clip() -> (Vec<Vec<u8>>, ParameterSets) {
    let s = std::fs::read(format!("{}/../openps-video/tests/data/h264.annexb", env!("CARGO_MANIFEST_DIR"))).unwrap();
    (nal::access_units(Codec::H264, &s), ParameterSets::from_annex_b(Codec::H264, &s))
}

/// `n` decoded frames cycling through the clip, ids consecutive.
fn frames(n: usize, format: OutputFormat) -> Vec<Frame> {
    let (units, params) = clip();
    let mut d = Decoder::new_with(Codec::H264, params, format).unwrap();
    let mut out = vec![];
    for k in 0..n {
        // restart at the IDR whenever the clip loops so the decoder always has its references
        if k % units.len() == 0 {
            d.reset();
        }
        let mut f = d.decode_annex_b(&units[k % units.len()], k as i64 * 1500).unwrap();
        out.append(&mut f);
    }
    out
}

fn renderer(settings: RenderSettings) -> MetalRenderer {
    let r = MetalRenderer::new(Default::default(), settings).expect("renderer");
    r.set_drawable_size(960, 540);
    r
}

fn feed(r: &MetalRenderer, frames: Vec<Frame>, fps: f64) {
    let t0 = Instant::now();
    for (k, f) in frames.into_iter().enumerate() {
        let due = t0 + Duration::from_secs_f64(k as f64 / fps);
        if let Some(w) = due.checked_duration_since(Instant::now()) {
            std::thread::sleep(w);
        }
        r.submit(f);
    }
}

#[test]
fn starts_and_stops_cleanly_without_ever_seeing_a_frame() {
    let t0 = Instant::now();
    let r = renderer(RenderSettings::default());
    assert!(!r.layer_ptr().is_null());
    drop(r);
    assert!(t0.elapsed() < Duration::from_secs(3), "{:?}", t0.elapsed());
}

#[test]
fn event_driven_mode_presents_each_real_frame_and_generates_nothing() {
    let r = renderer(RenderSettings { display_hz: 60, ..Default::default() });
    feed(&r, frames(90, OutputFormat::Bgra), 60.0);
    std::thread::sleep(Duration::from_millis(200));
    let s = r.stats();
    eprintln!("60 Hz: {s:?}");
    assert_eq!(s.errors, 0);
    assert_eq!(s.generated, 0, "nothing to generate at the source rate");
    assert!(s.presented >= 70 && s.presented <= 91, "presented {} of 90", s.presented);
}

#[test]
fn display_rate_above_the_source_generates_frames_in_between() {
    // generating frames above the source needs a display that can show them
    if openps_metal::main_display_hz() < 100.0 {
        eprintln!("skipped: the main display refreshes at {} Hz", openps_metal::main_display_hz());
        return;
    }
    let r = renderer(RenderSettings { display_hz: 144, upscale: Upscale::Qhd, ..Default::default() });
    feed(&r, frames(150, OutputFormat::Bgra), 60.0);
    std::thread::sleep(Duration::from_millis(200));
    let s = r.stats();
    eprintln!("144 Hz + upscale: {s:?}");
    assert_eq!(s.errors, 0);
    assert!(s.generating, "{s:?}");
    assert!((s.source_fps - 60.0).abs() < 4.0, "source rate {}", s.source_fps);
    assert!(s.generated > s.real_frames, "more generated frames than real ones: {s:?}");
    assert!(s.upscaled, "the 960x540 drawable is larger than the 320x192 source");
}

#[test]
fn settings_can_change_while_streaming_in_every_combination() {
    let r = renderer(RenderSettings::default());
    let all = [
        RenderSettings {
            display_hz: 144,
            mode: FrameMode::ZeroLatency,
            upscale: Upscale::Off,
            camera: false,
            metalfx: false,
            sharpness: 1.0,
            saturation: 1.0,
            gen_factor: 0.0,
        },
        RenderSettings {
            display_hz: 120,
            mode: FrameMode::Smooth,
            upscale: Upscale::Uhd,
            camera: false,
            metalfx: false,
            sharpness: 1.0,
            saturation: 1.0,
            gen_factor: 0.0,
        },
        RenderSettings {
            display_hz: 60,
            mode: FrameMode::ZeroLatency,
            upscale: Upscale::Qhd,
            camera: true,
            metalfx: false,
            sharpness: 1.0,
            saturation: 1.0,
            gen_factor: 0.0,
        },
        RenderSettings {
            display_hz: 144,
            mode: FrameMode::Smooth,
            upscale: Upscale::Off,
            camera: true,
            metalfx: false,
            sharpness: 1.0,
            saturation: 1.0,
            gen_factor: 0.0,
        },
    ];
    let fs = frames(240, OutputFormat::Bgra);
    let mut it = fs.into_iter();
    for round in 0..8 {
        r.set_settings(all[round % all.len()]);
        for _ in 0..30 {
            if let Some(f) = it.next() {
                r.submit(f);
            }
            std::thread::sleep(Duration::from_millis(16));
        }
    }
    let s = r.stats();
    assert_eq!(s.errors, 0, "{s:?}");
    assert!(s.presented > 100, "{s:?}");
}

#[test]
fn a_frame_in_the_wrong_format_is_counted_not_fatal() {
    let r = renderer(RenderSettings::default());
    feed(&r, frames(20, OutputFormat::Native), 60.0); // NV12: the Metal path needs BGRA
    std::thread::sleep(Duration::from_millis(200));
    assert!(r.stats().errors > 0);
    feed(&r, frames(30, OutputFormat::Bgra), 60.0); // and it recovers with good frames
    std::thread::sleep(Duration::from_millis(200));
    assert!(r.stats().presented > 10, "{:?}", r.stats());
}

#[test]
fn a_flood_of_frames_is_absorbed_by_the_mailbox_without_blocking_the_producer() {
    let r = renderer(RenderSettings { display_hz: 144, ..Default::default() });
    let fs = frames(120, OutputFormat::Bgra);
    let t0 = Instant::now();
    for f in fs {
        r.submit(f); // as fast as possible
    }
    assert!(t0.elapsed() < Duration::from_millis(500), "submit must never wait for the GPU: {:?}", t0.elapsed());
    std::thread::sleep(Duration::from_millis(300));
    let s = r.stats();
    assert!(s.mailbox_drops > 50, "{s:?}");
}

#[test]
fn a_new_session_starts_clean_after_reset() {
    let r = renderer(RenderSettings { display_hz: 144, ..Default::default() });
    feed(&r, frames(60, OutputFormat::Bgra), 60.0);
    r.reset();
    feed(&r, frames(60, OutputFormat::Bgra), 60.0);
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(r.stats().errors, 0);
    assert!(r.stats().presented > 60);
}

#[test]
fn dropping_the_renderer_mid_stream_is_prompt_and_safe() {
    for _ in 0..5 {
        let r = renderer(RenderSettings {
            display_hz: 144,
            mode: FrameMode::Smooth,
            camera: true,
            upscale: Upscale::Uhd,
            metalfx: false,
            sharpness: 1.0,
            saturation: 1.0,
            gen_factor: 0.0,
        });
        feed(&r, frames(20, OutputFormat::Bgra), 120.0);
        let t0 = Instant::now();
        drop(r);
        assert!(t0.elapsed() < Duration::from_secs(3), "{:?}", t0.elapsed());
    }
}

#[test]
fn a_decoded_frame_reaches_the_gpu_with_the_right_pixels() {
    use objc2_metal::*;
    let gpu = openps_metal::gpu::Gpu::new().unwrap();
    let fs = frames(3, OutputFormat::Bgra);
    for f in &fs {
        let tex = openps_metal::renderer::frame_to_texture(&gpu, f).expect("copy");
        assert_eq!((tex.width(), tex.height()), (f.width(), f.height()));
        // read the texture back through a shared staging texture
        let (w, h) = (tex.width(), tex.height());
        let stage =
            openps_metal::gpu::make_tex(&gpu.dev, MTLPixelFormat::BGRA8Unorm, w, h, MTLTextureUsage::ShaderRead, true)
                .unwrap();
        let cmd = gpu.command_buffer().unwrap();
        let blit = cmd.blitCommandEncoder().unwrap();
        unsafe {
            blit.copyFromTexture_sourceSlice_sourceLevel_sourceOrigin_sourceSize_toTexture_destinationSlice_destinationLevel_destinationOrigin(
                &tex, 0, 0, MTLOrigin { x: 0, y: 0, z: 0 }, MTLSize { width: w, height: h, depth: 1 }, &stage, 0, 0, MTLOrigin { x: 0, y: 0, z: 0 });
        }
        blit.endEncoding();
        cmd.commit();
        cmd.waitUntilCompleted();
        let mut got = vec![0u8; w * h * 4];
        unsafe {
            stage.getBytes_bytesPerRow_fromRegion_mipmapLevel(
                std::ptr::NonNull::new(got.as_mut_ptr().cast()).unwrap(),
                w * 4,
                MTLRegion { origin: MTLOrigin { x: 0, y: 0, z: 0 }, size: MTLSize { width: w, height: h, depth: 1 } },
                0,
            );
        }
        let want = f.copy_planes();
        assert_eq!(got.len(), want.len());
        let nonzero = got.iter().filter(|&&b| b != 0).count();
        assert!(
            nonzero > got.len() / 4,
            "the texture is (nearly) all black: {nonzero} non-zero bytes of {}",
            got.len()
        );
        let diff: f64 =
            got.iter().zip(&want).map(|(a, b)| (f64::from(*a) - f64::from(*b)).abs()).sum::<f64>() / got.len() as f64;
        assert!(diff < 1.0, "GPU copy differs from the decoded frame by {diff}");
    }
}

/// A 60 per second stream of a game that renders `content` pictures per second: every picture is carried by one frame, and the frames in
/// between repeat it (the same pixels under a new id), as a console does when the game is slower than the stream.
fn repeating_stream(stream_frames: usize, content: f64, stream: f64) -> Vec<Frame> {
    phased_stream(&[(stream_frames, content)], stream)
}

/// Like `repeating_stream`, but the game's rate changes: `(stream frames, content fps)` per phase, ids carrying on from one to the next.
fn phased_stream(phases: &[(usize, f64)], stream: f64) -> Vec<Frame> {
    let (units, params) = clip();
    // decoders fed the same units in step give frames with the same pixels: the copies of a picture come from the extra ones
    const COPIES: usize = 6;
    let mut dec: Vec<Decoder> =
        (0..=COPIES).map(|_| Decoder::new_with(Codec::H264, params.clone(), OutputFormat::Bgra).unwrap()).collect();
    let (mut out, mut k, mut next_pic) = (vec![], 0usize, 0.0f64);
    let mut copies: std::collections::VecDeque<Frame> = Default::default();
    let mut id = 0usize;
    for &(frames, content) in phases {
        for _ in 0..frames {
            // a new picture whenever the game's own clock has ticked since the last one
            let due = next_pic <= id as f64 / stream + 1e-9;
            if due {
                next_pic += 1.0 / content;
                if k % units.len() == 0 {
                    dec.iter_mut().for_each(Decoder::reset);
                }
                let u = &units[k % units.len()];
                out.extend(dec[0].decode_annex_b(u, id as i64 * 1500).unwrap());
                copies.clear();
                for (j, d) in dec.iter_mut().enumerate().skip(1) {
                    copies.extend(d.decode_annex_b(u, (id + j) as i64 * 1500).unwrap());
                }
                k += 1;
            } else if let Some(c) = copies.pop_front() {
                out.push(c);
            }
            id += 1;
        }
    }
    out
}

#[test]
fn a_game_that_starts_rendering_again_after_loading_loses_no_pictures_and_the_rate_follows_at_once() {
    // 3 s of a loading screen (the game renders 12 pictures a second) and then 4 s at 60
    let frames = phased_stream(&[(180, 12.0), (240, 60.0)], 60.0);
    let r = renderer(RenderSettings { display_hz: 60, gen_factor: 1.5, mode: FrameMode::Smooth, ..Default::default() });
    let total = frames.len();
    let t0 = Instant::now();
    let mut at_switch = None;
    for (k, f) in frames.into_iter().enumerate() {
        let due = t0 + Duration::from_secs_f64(k as f64 / 60.0);
        if let Some(w) = due.checked_duration_since(Instant::now()) {
            std::thread::sleep(w);
        }
        if k == 180 {
            at_switch = Some(r.stats());
        }
        r.submit(f);
        if k == 180 + 45 {
            // three quarters of a second after the game began to render again
            let s = r.stats();
            assert!(s.content_fps >= 50.0, "the rate has to be back within a second, not seconds: {}", s.content_fps);
        }
    }
    std::thread::sleep(Duration::from_millis(300));
    let (s0, s1) = (at_switch.unwrap(), r.stats());
    let dropped = s1.mailbox_drops - s0.mailbox_drops;
    eprintln!("{total} frames; after the switch: dropped {dropped}, content_fps {}", s1.content_fps);
    assert!(dropped <= 3, "{dropped} real pictures were dropped when the game started to render again");
}

#[test]
fn the_rate_follows_the_distinct_pictures_not_the_streams_frame_ids() {
    // (content fps, multiple in tenths) -> expected presented rate
    for (content, gen) in [(37.0f64, 15.0f32), (37.0, 20.0), (37.0, 25.0), (37.0, 30.0), (30.0, 20.0)] {
        let r = renderer(RenderSettings {
            display_hz: 60,
            gen_factor: gen / 10.0,
            mode: FrameMode::ZeroLatency,
            ..Default::default()
        });
        let frames = repeating_stream(300, content, 60.0);
        let t0 = Instant::now();
        feed(&r, frames, 60.0);
        std::thread::sleep(Duration::from_millis(300));
        let s = r.stats();
        let want = content * f64::from(gen) / 10.0;
        eprintln!(
            "content {content} x{}: content_fps {:.1} repeats {:.2} presented {} in {:?} (want ~{want:.0}/s)",
            gen / 10.0,
            s.content_fps,
            s.repeat_share,
            s.presented,
            t0.elapsed()
        );
        // the accepted rate: the game's pictures, plus the repeats that were kept to hold the floor of 30 (never fewer than the pictures)
        assert!(
            s.content_fps >= content - 2.5 && s.content_fps <= content.max(30.0) + 8.0,
            "accepted rate {} for {content}",
            s.content_fps
        );
        assert!(s.repeat_share > 0.2, "{s:?}");
    }
}

#[test]
fn repeated_pictures_are_displayed_as_a_fallback_without_changing_repeat_classification() {
    // A game that renders 12 pictures a second in a 60 fps stream: distinct-picture rate remains 12 fps, while repeated pictures
    // provide a fallback about every 100/3 ms.
    let frames = phased_stream(&[(300, 12.0)], 60.0);
    let r = renderer(RenderSettings { display_hz: 60, gen_factor: 1.0, mode: FrameMode::Smooth, ..Default::default() });
    feed(&r, frames, 60.0);
    std::thread::sleep(Duration::from_millis(300));
    let s = r.stats();
    eprintln!("12 pictures a second: {} real pictures displayed, repeats {:.2}", s.real_frames, s.repeat_share);
    assert!(s.repeat_share > 0.7, "{}", s.repeat_share);
    assert!(s.real_frames >= 130, "{} pictures displayed in 5 s", s.real_frames);
    // and a game that renders 60 keeps all of them
    let r = renderer(RenderSettings { display_hz: 60, gen_factor: 1.0, mode: FrameMode::Smooth, ..Default::default() });
    feed(&r, phased_stream(&[(300, 60.0)], 60.0), 60.0);
    std::thread::sleep(Duration::from_millis(300));
    let s = r.stats();
    assert!((56.0..=62.0).contains(&s.content_fps), "{}", s.content_fps);
}
