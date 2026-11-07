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
    let r = renderer(RenderSettings { display_hz: 144, upscale: Upscale::X1_33, ..Default::default() });
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

/// The clip is 320x192 and the drawable 960x540: a window far larger than the stream.
fn upscale_stats(u: Upscale) -> openps_metal::RenderStats {
    let r = renderer(RenderSettings { display_hz: 60, upscale: u, ..Default::default() });
    feed(&r, frames(60, OutputFormat::Bgra), 60.0);
    std::thread::sleep(Duration::from_millis(200));
    r.stats()
}

#[test]
fn upscaling_off_leaves_the_picture_at_the_streams_size_even_in_a_larger_window() {
    let s = upscale_stats(Upscale::Off);
    assert_eq!(s.errors, 0);
    assert!(!s.upscaled, "{s:?}");
    assert_eq!(s.upscale_height, 0);
}

#[test]
fn a_multiple_draws_the_picture_that_many_times_larger_than_the_stream_not_as_large_as_the_window() {
    for (u, height) in [(Upscale::X1_33, 256), (Upscale::X1_5, 288), (Upscale::X2, 384)] {
        let s = upscale_stats(u);
        assert_eq!(s.errors, 0);
        assert!(s.upscaled, "{u:?}: {s:?}");
        assert_eq!(s.upscale_height, height, "{u:?} of a 192 px tall stream");
    }
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
            fsr: false,
            linear_light: true,
            adaptive: false,
            jitter_buffer: false,
            uncapped: false,
            sharpness: 1.0,
            rcas: 1.0,
            saturation: 1.0,
            immersion: true,
            gen_factor: 0.0,
        },
        RenderSettings {
            display_hz: 120,
            mode: FrameMode::Smooth,
            upscale: Upscale::X2,
            camera: false,
            metalfx: false,
            fsr: true,
            linear_light: false,
            adaptive: true,
            jitter_buffer: false,
            uncapped: false,
            sharpness: 1.0,
            rcas: 1.0,
            saturation: 1.0,
            immersion: false,
            gen_factor: 0.0,
        },
        RenderSettings {
            display_hz: 60,
            mode: FrameMode::ZeroLatency,
            upscale: Upscale::X1_33,
            camera: true,
            metalfx: false,
            fsr: false,
            linear_light: false,
            adaptive: true,
            jitter_buffer: false,
            uncapped: false,
            sharpness: 1.0,
            rcas: 1.0,
            saturation: 1.0,
            immersion: true,
            gen_factor: 0.0,
        },
        RenderSettings {
            display_hz: 144,
            mode: FrameMode::Smooth,
            upscale: Upscale::X1_5,
            camera: true,
            metalfx: false,
            fsr: true,
            linear_light: true,
            adaptive: false,
            jitter_buffer: false,
            uncapped: false,
            sharpness: 1.0,
            rcas: 1.0,
            saturation: 1.0,
            immersion: false,
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
            upscale: Upscale::X2,
            metalfx: false,
            fsr: true,
            linear_light: true,
            adaptive: true,
            jitter_buffer: false,
            uncapped: false,
            sharpness: 1.0,
            rcas: 1.0,
            saturation: 1.0,
            immersion: true,
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

// ---- FSR in the whole renderer ----------------------------------------------------------------------------------------------------
fn fsr_stats(settings: RenderSettings, n: usize) -> openps_metal::RenderStats {
    let r = renderer(settings);
    feed(&r, frames(n, OutputFormat::Bgra), 60.0);
    std::thread::sleep(Duration::from_millis(300));
    r.stats()
}

#[test]
fn fsr_upscales_in_the_renderer_at_every_multiple_and_the_stats_say_so() {
    // the clip is 320x192
    for (u, height) in [(Upscale::X1_33, 256), (Upscale::X1_5, 288), (Upscale::X2, 384)] {
        let s = fsr_stats(RenderSettings { display_hz: 60, upscale: u, fsr: true, ..Default::default() }, 90);
        assert_eq!(s.errors, 0, "{u:?}: {s:?}");
        assert!(s.upscaled && s.fsr_active, "{u:?}: {s:?}");
        assert_eq!(s.upscale_height, height, "{u:?} of a 192 px tall stream");
        assert!(
            s.finish_pass && s.rcas_active,
            "FSR's picture gets the finishing pass and RCAS at the defaults: {s:?}"
        );
        assert!(!s.linear_active || s.fsr_active, "linear light belongs to the MetalFX scaler");
    }
}

#[test]
fn fsr_is_only_used_when_asked_for_and_only_when_upscaling() {
    let metalfx = fsr_stats(RenderSettings { display_hz: 60, upscale: Upscale::X2, ..Default::default() }, 60);
    assert!(!metalfx.fsr_active, "{metalfx:?}");
    let off = fsr_stats(RenderSettings { display_hz: 60, upscale: Upscale::Off, fsr: true, ..Default::default() }, 60);
    assert!(!off.upscaled && !off.fsr_active, "nothing to upscale: {off:?}");
}

#[test]
fn fsr_without_adaptive_sharpening_is_blended_in_the_present_pass() {
    let s = fsr_stats(
        RenderSettings { display_hz: 60, upscale: Upscale::X2, fsr: true, rcas: 0.0, ..Default::default() },
        60,
    );
    assert_eq!(s.errors, 0, "{s:?}");
    assert!(s.fsr_active && !s.rcas_active && !s.finish_pass, "{s:?}");
}

// ---- the adaptive frame rate --------------------------------------------------------------------------------------------------------
fn adaptive_settings() -> RenderSettings {
    RenderSettings { display_hz: 60, gen_factor: 1.0, upscale: Upscale::X2, adaptive: true, ..Default::default() }
}

/// A heavy load that the GPU can hold at 50 of 60 but not at 55 (13 ms added to the real few: about 13.4 ms a picture; 70 % of 18.2 ms,
/// the interval at 55, is 12.7).
const HEAVY_MS: f64 = 13.0;

/// What a held rate of `held` fps (of a 60 fps stream) is really shown at on this Mac's display: on a fixed-refresh display it is rounded
/// down to a whole number of refreshes per picture (50 -> 30 on 60 Hz), on a variable-refresh display it is kept.
fn shown_at(held: u16) -> f64 {
    let (refresh, vrr) = (openps_metal::main_display_hz(), openps_metal::display_is_variable());
    if vrr || refresh < 1.0 {
        f64::from(held)
    } else {
        f64::from(openps_metal::ladder::settled_rate(held, refresh))
    }
}

/// Run a 60 fps stream of `seconds` through a renderer whose pictures look `extra_ms` heavier than they are.
fn adaptive_run(settings: RenderSettings, extra_ms: f64, seconds: usize) -> (MetalRenderer, openps_metal::RenderStats) {
    let r = renderer(settings);
    r.set_extra_gpu_ms(extra_ms);
    feed(&r, frames(seconds * 60, OutputFormat::Bgra), 60.0);
    std::thread::sleep(Duration::from_millis(200));
    let s = r.stats();
    (r, s)
}

#[test]
fn a_heavy_load_lowers_the_frame_rate_in_steps_of_5_and_every_filter_stays_on() {
    // 13 ms added to the real few against the 16.7 ms there is at 60 fps: 70 % of the interval fits 52 pictures a second, so 50
    let (_r, s) = adaptive_run(adaptive_settings(), HEAVY_MS, 5);
    eprintln!("{s:?}");
    assert_eq!(s.errors, 0, "{s:?}");
    assert!(s.adaptive && s.asked_hz == 60.0, "{s:?}");
    // the real GPU time moves by a millisecond or so from run to run, which is one step of 5 either way
    let cap = s.adaptive_cap_hz;
    assert!((45.0..=55.0).contains(&cap) && cap % 5.0 == 0.0, "the closest multiple of 5 that fits: {s:?}");
    let shown = shown_at(cap as u16);
    assert!((s.effective_hz - shown).abs() < 0.5, "shown at {shown}: {}", s.effective_hz);
    assert!(s.finish_pass && s.rcas_active && !s.finish_guard_off, "the picture keeps every filter: {s:?}");
    assert!(s.upscaled);
}

#[test]
fn the_frames_that_are_not_shown_are_dropped_evenly_not_in_pairs() {
    let r = renderer(adaptive_settings());
    r.set_extra_gpu_ms(HEAVY_MS);
    let mut all = frames(600, OutputFormat::Bgra); // one stream, fed in two parts so the frame numbers go on
    let rest = all.split_off(240);
    feed(&r, all, 60.0); // 4 s: long enough for the rate to come down to 50
    std::thread::sleep(Duration::from_millis(300));
    let (a, t0, held) = (r.stats().real_frames, Instant::now(), r.stats().adaptive_cap_hz as u16);
    feed(&r, rest, 60.0); // 6 s at the lowered rate
    std::thread::sleep(Duration::from_millis(300));
    let (b, secs) = (r.stats().real_frames, t0.elapsed().as_secs_f64() - 0.3);
    let shown_per_s = (b - a) as f64 / secs;
    let want = shown_at(held.max(1));
    eprintln!(
        "{shown_per_s:.1} pictures a second shown from a 60 fps stream held to {held} (the display shows {want})"
    );
    assert!(
        (want * 0.85..=want * 1.15).contains(&shown_per_s),
        "evenly spaced, about {want} a second ({held} held; a fixed display rounds it to a whole number of refreshes), not 60: {shown_per_s}"
    );
}

#[test]
fn a_light_load_never_lowers_the_rate() {
    let (_r, s) = adaptive_run(adaptive_settings(), 0.0, 4);
    assert_eq!(s.errors, 0, "{s:?}");
    assert_eq!(s.adaptive_cap_hz, 0.0, "{s:?}");
    assert!((s.effective_hz - 60.0).abs() < 1.0, "{}", s.effective_hz);
}

#[test]
fn with_the_adaptive_rate_off_the_same_load_gives_up_the_finishing_pass_instead() {
    let (_r, s) = adaptive_run(RenderSettings { adaptive: false, ..adaptive_settings() }, HEAVY_MS, 5);
    assert_eq!(s.errors, 0, "{s:?}");
    assert_eq!(s.adaptive_cap_hz, 0.0, "the rate is not touched: {s:?}");
    assert!((s.effective_hz - 60.0).abs() < 1.0, "{}", s.effective_hz);
    assert!(s.finish_guard_off, "the filters are the ones that give way: {s:?}");
}

#[test]
fn changing_a_setting_forgets_the_limit_so_the_gpu_is_tried_again() {
    let (r, s) = adaptive_run(adaptive_settings(), HEAVY_MS, 4);
    assert!((45.0..=55.0).contains(&s.adaptive_cap_hz), "{s:?}");
    r.set_extra_gpu_ms(0.0);
    r.set_settings(RenderSettings { sharpness: 0.8, ..adaptive_settings() });
    feed(&r, frames(90, OutputFormat::Bgra), 60.0); // the stats are published as pictures are drawn
    std::thread::sleep(Duration::from_millis(300));
    let s = r.stats();
    assert_eq!(s.adaptive_cap_hz, 0.0, "{s:?}");
}

#[test]
fn when_the_load_goes_away_the_rate_climbs_back_to_the_one_that_was_asked_for() {
    let r = renderer(adaptive_settings());
    r.set_extra_gpu_ms(HEAVY_MS);
    // one stream of 40 s, fed in two parts so the frame numbers go on: each of the two steps up (50, 55, 60) needs 600 shown pictures,
    // which take 20 s at 30 a second (a fixed 60 Hz display shows the held 50 as 30) and 11 s at 55
    let mut all = frames(2400, OutputFormat::Bgra);
    let rest = all.split_off(240);
    feed(&r, all, 60.0);
    std::thread::sleep(Duration::from_millis(200));
    assert!((45.0..=55.0).contains(&r.stats().adaptive_cap_hz), "{:?}", r.stats());
    r.set_extra_gpu_ms(0.0); // the load is gone
    feed(&r, rest, 60.0);
    std::thread::sleep(Duration::from_millis(300));
    let s = r.stats();
    assert_eq!(s.adaptive_cap_hz, 0.0, "the limit ends at the asked rate: {s:?}");
    assert!((s.effective_hz - 60.0).abs() < 1.0, "back to 60: {}", s.effective_hz);
    assert_eq!(s.errors, 0, "{s:?}");
}

#[test]
fn the_extra_load_hook_ignores_nonsense() {
    let r = renderer(adaptive_settings());
    for bad in [f64::NAN, -1.0, 5000.0, f64::INFINITY] {
        r.set_extra_gpu_ms(bad);
    }
    feed(&r, frames(120, OutputFormat::Bgra), 60.0);
    assert_eq!(r.stats().adaptive_cap_hz, 0.0);
}

#[test]
fn the_chosen_upscale_size_is_made_even_in_a_window_smaller_than_it_and_scaled_down_to_fit() {
    // the clip is 320x192; 2x is 384 tall; the window is 300x200, smaller than the video itself
    let r = MetalRenderer::new(
        Default::default(),
        RenderSettings { display_hz: 60, upscale: Upscale::X2, ..Default::default() },
    )
    .expect("renderer");
    r.set_drawable_size(300, 200);
    feed(&r, frames(60, OutputFormat::Bgra), 60.0);
    std::thread::sleep(Duration::from_millis(200));
    let s = r.stats();
    assert_eq!(s.errors, 0, "{s:?}");
    assert!(s.upscaled, "{s:?}");
    assert_eq!(s.upscale_height, 384, "made at the size that was chosen, not at the window's");
    assert!(
        s.finish_pass,
        "the upscaled picture goes through the linear-light and sharpening pass at the default sharpness: {s:?}"
    );
    assert_eq!(s.drawable_size, (300, 200));
}

#[test]
fn the_finishing_pass_is_off_when_nothing_is_upscaled() {
    // no upscaling: nothing to finish
    let r = renderer(RenderSettings { display_hz: 60, upscale: Upscale::Off, ..Default::default() });
    feed(&r, frames(30, OutputFormat::Bgra), 60.0);
    std::thread::sleep(Duration::from_millis(150));
    let s = r.stats();
    assert!(!s.upscaled && !s.finish_pass, "{s:?}");
}

#[test]
fn adaptive_sharpening_is_its_own_setting_and_the_guard_state_is_reported() {
    let run = |rcas: f32| {
        let r = renderer(RenderSettings { display_hz: 60, upscale: Upscale::X2, rcas, ..Default::default() });
        feed(&r, frames(40, OutputFormat::Bgra), 60.0);
        std::thread::sleep(Duration::from_millis(200));
        r.stats()
    };
    let (on, off) = (run(1.0), run(0.0));
    assert_eq!(on.errors + off.errors, 0);
    if !on.upscaled {
        return; // no MetalFX scaler on this Mac
    }
    assert!(on.rcas_active && on.finish_pass, "RCAS at 100 % is applied: {on:?}");
    assert!(!off.rcas_active, "0 % is off whatever else runs: {off:?}");
    assert!(
        (on.settings.rcas - 1.0).abs() < 1e-6 && off.settings.rcas == 0.0,
        "the stats carry the settings it runs with"
    );
    assert!(
        !on.finish_guard_off && on.finish_guard_slow < on.finish_guard_limit,
        "an idle GPU does not trip the guard: {on:?}"
    );
    assert_eq!(on.finish_guard_limit, 120);
    assert!(
        (on.finish_guard_ms - 0.7 * 1000.0 / 60.0).abs() < 0.5,
        "70 % of the 60 Hz interval: {}",
        on.finish_guard_ms
    );
}

// ---- running above the display's refresh rate, and the smoothing buffer --------------------------------------------------------------
fn smooth_settings(uncapped: bool, jitter_buffer: bool) -> RenderSettings {
    RenderSettings {
        display_hz: 240,
        gen_factor: 0.0,
        mode: FrameMode::Smooth,
        uncapped,
        jitter_buffer,
        ..RenderSettings::default()
    }
}

fn smooth_run(settings: RenderSettings, seconds: usize) -> openps_metal::RenderStats {
    let r = renderer(settings);
    feed(&r, frames(seconds * 60, OutputFormat::Bgra), 60.0);
    std::thread::sleep(Duration::from_millis(300));
    r.stats()
}

#[test]
fn the_renderer_runs_above_the_displays_refresh_rate_only_when_asked_to() {
    let refresh = openps_metal::main_display_hz();
    if refresh < 1.0 || refresh > 200.0 {
        eprintln!("display refresh {refresh}: nothing to run above, skipped");
        return;
    }
    let on = smooth_run(smooth_settings(true, false), 4);
    assert_eq!(on.errors, 0, "{on:?}");
    assert!(on.uncapped_active, "{on:?}");
    assert!(
        (on.effective_hz - 240.0).abs() < 0.5,
        "frames are made at 240 on a {refresh} Hz display: {}",
        on.effective_hz
    );
    assert!(on.generated > on.real_frames, "most of the pictures are generated: {on:?}");
    let off = smooth_run(smooth_settings(false, false), 4);
    assert!(!off.uncapped_active);
    assert!(off.effective_hz <= refresh * 1.001, "capped at the refresh rate: {} on {refresh}", off.effective_hz);
}

#[test]
fn the_smoothing_buffer_is_only_held_in_smooth_mode_and_only_when_chosen() {
    let off = smooth_run(smooth_settings(false, false), 5);
    assert_eq!(off.playout_ms, 0.0, "off by choice: {off:?}");
    let low = smooth_run(RenderSettings { mode: FrameMode::ZeroLatency, ..smooth_settings(false, true) }, 5);
    assert_eq!(low.playout_ms, 0.0, "never in Low latency mode: {low:?}");
    let on = smooth_run(smooth_settings(false, true), 5);
    assert!((0.0..=25.0).contains(&on.playout_ms), "bounded: {}", on.playout_ms);
}

#[test]
fn the_stick_prior_is_not_used_when_the_stick_does_not_drive_the_picture() {
    use openps_metal::look::SharedLook;
    // a stick pushed back and forth while the stream (a clip that has nothing to do with it) plays on: nothing is explained, so the motion
    // search is not handed a guess
    let look = SharedLook::new();
    let r =
        MetalRenderer::new(look.clone(), RenderSettings { display_hz: 144, ..Default::default() }).expect("renderer");
    r.set_drawable_size(960, 540);
    let t0 = Instant::now();
    for (k, f) in frames(120, OutputFormat::Bgra).into_iter().enumerate() {
        let due = t0 + Duration::from_secs_f64(k as f64 / 60.0);
        if let Some(w) = due.checked_duration_since(Instant::now()) {
            std::thread::sleep(w);
        }
        look.push(openps_metal::now(), ((k * 7) % 5) as f32 / 5.0 - 0.4, 0.0);
        r.submit(f);
    }
    std::thread::sleep(Duration::from_millis(200));
    let s = r.stats();
    assert_eq!(s.errors, 0);
    assert!(!s.input_prior_active, "{s:?}");
}
