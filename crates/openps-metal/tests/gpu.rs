//! Headless GPU tests: synthetic frames with a known right answer, results read back from the GPU.
#![cfg(target_os = "macos")]
use objc2_metal::*;
use openps_metal::gpu::{fit, make_tex, upscale_target, Gpu, Tex};
use std::ptr::NonNull;
use std::time::Instant;

fn gpu() -> Gpu {
    Gpu::new().expect("Metal pipelines (runtime shader compile) must build")
}

fn upload(g: &Gpu, w: usize, h: usize, f: impl Fn(usize, usize) -> [u8; 4]) -> Tex {
    let t = make_tex(&g.dev, MTLPixelFormat::BGRA8Unorm, w, h, MTLTextureUsage::ShaderRead, true).unwrap();
    let mut px = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        for x in 0..w {
            px.extend(f(x, y));
        }
    }
    unsafe {
        t.replaceRegion_mipmapLevel_withBytes_bytesPerRow(
            MTLRegion { origin: MTLOrigin { x: 0, y: 0, z: 0 }, size: MTLSize { width: w, height: h, depth: 1 } },
            0,
            NonNull::new(px.as_mut_ptr().cast()).unwrap(),
            w * 4,
        );
    }
    t
}

fn target(g: &Gpu, w: usize, h: usize) -> Tex {
    make_tex(
        &g.dev,
        MTLPixelFormat::BGRA8Unorm,
        w,
        h,
        MTLTextureUsage::RenderTarget | MTLTextureUsage::ShaderRead,
        true,
    )
    .unwrap()
}

fn read(t: &Tex) -> Vec<u8> {
    let (w, h) = (t.width(), t.height());
    let mut out = vec![0u8; w * h * 4];
    unsafe {
        t.getBytes_bytesPerRow_fromRegion_mipmapLevel(
            NonNull::new(out.as_mut_ptr().cast()).unwrap(),
            w * 4,
            MTLRegion { origin: MTLOrigin { x: 0, y: 0, z: 0 }, size: MTLSize { width: w, height: h, depth: 1 } },
            0,
        );
    }
    out
}

fn finish(cmd: &objc2::rc::Retained<objc2::runtime::ProtocolObject<dyn MTLCommandBuffer>>) {
    cmd.commit();
    cmd.waitUntilCompleted();
    assert_eq!(cmd.status(), MTLCommandBufferStatus::Completed, "{:?}", cmd.error());
}

/// A smooth, detailed, non-repeating-enough texture; `shift` moves it right by that many pixels.
fn texture(x: usize, y: usize, shift: f64) -> [u8; 4] {
    let (fx, fy) = (x as f64 - shift, y as f64);
    let v = |a: f64, b: f64, c: f64| {
        ((fx * a + fy * b + c).sin() * 0.5 + 0.5) * ((fx * b * 0.7 - fy * a * 1.3 + c * 2.0).cos() * 0.5 + 0.5)
    };
    let r = v(0.21, 0.13, 0.0);
    let g = v(0.11, 0.29, 1.0);
    let b = v(0.37, 0.07, 2.0);
    [(b * 255.0) as u8, (g * 255.0) as u8, (r * 255.0) as u8, 255]
}

/// Render `tex` to an offscreen target of the same size and read it back.
fn shown(g: &Gpu, cmd: &objc2::rc::Retained<objc2::runtime::ProtocolObject<dyn MTLCommandBuffer>>, t: &Tex) -> Tex {
    let tg = target(g, t.width(), t.height());
    g.present(cmd, t, &tg).unwrap();
    tg
}

fn mean_abs_err(a: &[u8], b: &[u8], w: usize, h: usize, margin: usize) -> f64 {
    let mut s = 0.0;
    let mut n = 0.0;
    for y in margin..h - margin {
        for x in margin..w - margin {
            for c in 0..3 {
                s += (f64::from(a[(y * w + x) * 4 + c]) - f64::from(b[(y * w + x) * 4 + c])).abs();
                n += 1.0;
            }
        }
    }
    s / n
}

#[test]
fn the_pipelines_build_and_metalfx_is_available_on_this_machine() {
    let g = gpu();
    eprintln!("MetalFX spatial scaler available: {}", g.spatial_scaler_available);
    assert!(g.spatial_scaler_available);
}

#[test]
fn letterboxing_keeps_the_aspect_ratio_and_paints_the_bars_black() {
    let g = gpu();
    let src = upload(&g, 160, 90, |_, _| [200, 200, 200, 255]);
    let tg = target(&g, 400, 400);
    let cmd = g.command_buffer().unwrap();
    g.present(&cmd, &src, &tg).unwrap();
    finish(&cmd);
    let px = read(&tg);
    let at = |x: usize, y: usize| px[(y * 400 + x) * 4];
    assert_eq!(at(200, 10), 0, "top bar is black");
    assert_eq!(at(200, 390), 0, "bottom bar is black");
    assert!(at(200, 200) > 190, "picture in the middle");
    assert!(at(5, 200) > 190 && at(394, 200) > 190, "full width is used");
    let (r, s) = fit(160.0, 90.0, 400.0, 400.0);
    assert_eq!((r.0, r.2, s), (0.0, 400.0, 2.5));
    assert!((r.1 - 87.5).abs() < 1e-9 && (r.3 - 225.0).abs() < 1e-9);
}

#[test]
fn upscale_targets_are_capped_by_the_mode_and_never_pointless() {
    // 1080p video on a 4K window: 1440p mode stops at 2560x1440, 2160p mode fills the window
    assert_eq!(upscale_target((1920, 1080), (3840, 2160), 1440), Some((2560, 1440)));
    assert_eq!(upscale_target((1920, 1080), (3840, 2160), 2160), Some((3840, 2160)));
    // the ultrawide window of this machine: the picture is height-limited
    assert_eq!(upscale_target((1280, 720), (3440, 1440), 2160), Some((2560, 1440)));
    // window smaller than the source: nothing to upscale to
    assert_eq!(upscale_target((1920, 1080), (1280, 720), 2160), None);
    assert_eq!(upscale_target((1920, 1080), (1920, 1080), 2160), None);
    // degenerate input is harmless
    assert_eq!(upscale_target((0, 0), (100, 100), 1440), None);
    assert_eq!(upscale_target((1280, 720), (0, 0), 1440), None);
    let t = upscale_target((1281, 721), (3000, 2000), 1440).unwrap();
    assert!(t.0.is_multiple_of(2) && t.1.is_multiple_of(2), "even sizes: {t:?}");
}

#[test]
fn metalfx_upscaling_produces_a_larger_faithful_picture() {
    let g = gpu();
    let (w, h) = (640, 360);
    let src = upload(&g, w, h, |x, y| texture(x, y, 0.0));
    let cmd = g.command_buffer().unwrap();
    let mut g = g;
    g.ingest(&cmd, src.clone(), false).unwrap();
    let (out, up) = g.compose(&cmd, 0.0, Some((1280, 720)), (0.0, 0.0)).unwrap();
    assert!(up, "the spatial scaler ran");
    assert_eq!((out.width(), out.height()), (1280, 720));
    let tg = shown(&g, &cmd, &out);
    finish(&cmd);
    let big = read(&tg);
    // downsample the 2x result with a box filter and compare with the source: a faithful upscale round-trips closely
    let small = read(&shown_source(&g, &src));
    let mut err = 0.0;
    let mut n = 0.0;
    for y in 4..h - 4 {
        for x in 4..w - 4 {
            for c in 0..3 {
                let s: f64 =
                    (0..4).map(|k| f64::from(big[((2 * y + k / 2) * 1280 + 2 * x + k % 2) * 4 + c])).sum::<f64>() / 4.0;
                err += (s - f64::from(small[(y * w + x) * 4 + c])).abs();
                n += 1.0;
            }
        }
    }
    assert!(err / n < 4.0, "round-trip error {} / 255", err / n);
}

fn shown_source(g: &Gpu, t: &Tex) -> Tex {
    let cmd = g.command_buffer().unwrap();
    let tg = shown(g, &cmd, t);
    finish(&cmd);
    tg
}

#[test]
fn no_upscale_when_the_target_is_not_larger_or_the_size_is_absent() {
    let mut g = gpu();
    let src = upload(&g, 320, 180, |x, y| texture(x, y, 0.0));
    let cmd = g.command_buffer().unwrap();
    g.ingest(&cmd, src.clone(), false).unwrap();
    for out in [None, Some((320, 180)), Some((200, 100)), Some((640, 100))] {
        let (t, up) = g.compose(&cmd, 0.0, out, (0.0, 0.0)).unwrap();
        assert!(!up && (t.width(), t.height()) == (320, 180), "{out:?}");
    }
    finish(&cmd);
}

fn pan_scene(g: &mut Gpu, w: usize, h: usize, speed: f64, alpha: f32) -> (f64, f64) {
    let f0 = upload(g, w, h, |x, y| texture(x, y, 0.0));
    let f1 = upload(g, w, h, |x, y| texture(x, y, speed));
    let cmd = g.command_buffer().unwrap();
    g.ingest(&cmd, f0, true).unwrap();
    g.ingest(&cmd, f1, true).unwrap();
    let (t, _) = g.compose(&cmd, alpha, None, (0.0, 0.0)).unwrap();
    let got = shown(g, &cmd, &t);
    finish(&cmd);
    let truth = upload(g, w, h, |x, y| texture(x, y, speed * (1.0 + f64::from(alpha))));
    let truth_px = read(&shown_source(g, &truth));
    let cur = upload(g, w, h, |x, y| texture(x, y, speed));
    let cur_px = read(&shown_source(g, &cur));
    let got_px = read(&got);
    let m = 40;
    (mean_abs_err(&got_px, &truth_px, w, h, m), mean_abs_err(&cur_px, &truth_px, w, h, m))
}

#[test]
fn extrapolation_lands_where_the_content_will_be() {
    let mut g = gpu();
    for (speed, alpha) in [(8.0, 0.5f32), (8.0, 1.0), (4.0, 0.5), (12.0, 0.4)] {
        let (warped, unwarped) = pan_scene(&mut g, 640, 360, speed, alpha);
        eprintln!(
            "pan {speed}px/frame alpha {alpha}: warped error {warped:.2}, just showing the last frame {unwarped:.2}"
        );
        assert!(warped < unwarped * 0.45, "speed {speed} alpha {alpha}: warped {warped:.2} vs unwarped {unwarped:.2}");
        assert!(warped < 14.0, "absolute error {warped:.2}");
    }
}

#[test]
fn alpha_zero_and_the_first_frame_show_the_real_frame_untouched() {
    let mut g = gpu();
    let a = upload(&g, 256, 144, |x, y| texture(x, y, 0.0));
    let cmd = g.command_buffer().unwrap();
    g.ingest(&cmd, a.clone(), true).unwrap();
    assert!(!g.motion_ready());
    let (t, _) = g.compose(&cmd, 0.9, None, (0.0, 0.0)).unwrap(); // no motion known yet: alpha must be ignored
    assert_eq!(t.width(), 256);
    let tg = shown(&g, &cmd, &t);
    finish(&cmd);
    let want = read(&shown_source(&g, &a));
    assert_eq!(read(&tg), want, "identical to the source");
    // with motion known, alpha 0 is still exactly the frame
    let b = upload(&g, 256, 144, |x, y| texture(x, y, 5.0));
    let cmd = g.command_buffer().unwrap();
    g.ingest(&cmd, b.clone(), true).unwrap();
    let (t, _) = g.compose(&cmd, 0.0, None, (0.0, 0.0)).unwrap();
    let tg = shown(&g, &cmd, &t);
    finish(&cmd);
    assert_eq!(read(&tg), read(&shown_source(&g, &b)));
}

#[test]
fn a_static_scene_stays_static_when_extrapolated() {
    let mut g = gpu();
    let (warped, unwarped) = pan_scene(&mut g, 320, 180, 0.0, 1.0);
    assert!(warped < 0.01 && unwarped < 0.01, "static content must not drift: {warped} {unwarped}");
}

#[test]
fn a_scene_cut_does_not_smear_garbage() {
    let mut g = gpu();
    let (w, h) = (320, 180);
    // two unrelated images: no consistent motion exists, so confidence collapses and the output must be the real frame
    let rnd = |seed: u64| {
        move |x: usize, y: usize| {
            let mut v = (x as u64).wrapping_mul(0x9E37_79B9).wrapping_add((y as u64).wrapping_mul(0x85EB_CA6B)) ^ seed;
            v ^= v >> 13;
            v = v.wrapping_mul(0xC2B2_AE35);
            v ^= v >> 16;
            [(v & 255) as u8, ((v >> 8) & 255) as u8, ((v >> 16) & 255) as u8, 255]
        }
    };
    let a = upload(&g, w, h, rnd(1));
    let b = upload(&g, w, h, rnd(99));
    let cmd = g.command_buffer().unwrap();
    g.ingest(&cmd, a, true).unwrap();
    g.ingest(&cmd, b.clone(), true).unwrap();
    let (t, _) = g.compose(&cmd, 1.0, None, (0.0, 0.0)).unwrap();
    let tg = shown(&g, &cmd, &t);
    finish(&cmd);
    let err = mean_abs_err(&read(&tg), &read(&shown_source(&g, &b)), w, h, 0);
    assert!(err < 12.0, "after a cut the picture should stay (nearly) the real frame; error {err}");
}

#[test]
fn motion_faster_than_the_search_range_is_contained() {
    let mut g = gpu();
    // 60 px/frame is far beyond the 16 px search range: the result may be wrong, but must stay sane (no NaN-like garbage)
    let (warped, unwarped) = pan_scene(&mut g, 640, 360, 60.0, 0.5);
    assert!(warped < unwarped * 1.6 + 5.0, "must not be much worse than not extrapolating: {warped} vs {unwarped}");
}

#[test]
fn odd_tiny_and_changing_sizes_do_not_crash() {
    let mut g = gpu();
    for (w, h) in [(641, 361), (2, 2), (1, 1), (16, 9), (17, 3), (1920, 1080), (321, 181)] {
        let a = upload(&g, w, h, |x, y| texture(x, y, 0.0));
        let b = upload(&g, w, h, |x, y| texture(x, y, 3.0));
        let cmd = g.command_buffer().unwrap();
        g.ingest(&cmd, a, true).unwrap();
        g.ingest(&cmd, b, true).unwrap();
        let (t, _) = g.compose(&cmd, 0.5, Some((w * 2, h * 2)), (0.0, 0.0)).unwrap();
        let _ = shown(&g, &cmd, &t);
        finish(&cmd);
    }
}

#[test]
fn per_frame_gpu_cost_at_1080p_to_4k_leaves_room_for_144_hz() {
    let mut g = gpu();
    let (w, h) = (1920, 1080);
    let frames: Vec<Tex> = (0..4).map(|i| upload(&g, w, h, |x, y| texture(x, y, i as f64 * 6.0))).collect();
    let tg = target(&g, 3840, 2160);
    let run = |g: &mut Gpu, i: usize, alpha: f32| {
        let t0 = Instant::now();
        let cmd = g.command_buffer().unwrap();
        g.ingest(&cmd, frames[i % 4].clone(), true).unwrap();
        let (t, up) = g.compose(&cmd, alpha, Some((3840, 2160)), (0.0, 0.0)).unwrap();
        assert!(up);
        g.present(&cmd, &t, &tg).unwrap();
        finish(&cmd);
        t0.elapsed().as_secs_f64() * 1000.0
    };
    for i in 0..6 {
        run(&mut g, i, 0.5); // warm up (scaler and texture creation)
    }
    let mut ms: Vec<f64> = (0..40).map(|i| run(&mut g, i, 0.5)).collect();
    ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
    eprintln!(
        "1080p -> 4K: ingest + motion + warp + MetalFX + present, p50 {:.2} ms, p90 {:.2} ms, max {:.2} ms",
        ms[20], ms[36], ms[39]
    );
    // one 144 Hz frame is 6.9 ms; a real frame (motion search + everything) must fit in a 60 fps interval with room to spare
    assert!(ms[20] < 14.0, "p50 {:.2} ms is too slow for 60 fps real frames", ms[20]);
}

// ---- moving objects over a background: where extrapolation artifacts actually live ----------------------------------------
/// A textured object (a rectangle with its own texture) moving right over a static textured background.
fn object_scene(x: usize, y: usize, ox: f64) -> [u8; 4] {
    let (w, h) = (140.0, 110.0);
    let (oy, fx, fy) = (120.0, x as f64, y as f64);
    if fx >= ox && fx < ox + w && fy >= oy && fy < oy + h {
        let t = texture(((fx - ox) * 1.0 + 300.0) as usize, (fy - oy + 40.0) as usize, 0.0);
        [t[0].saturating_add(30), t[1], t[2].saturating_sub(30), 255]
    } else {
        texture(x, y, 0.0)
    }
}

/// Error of the extrapolated frame against the true frame, over the whole image and over the band the object sweeps through
/// (where occlusion and disocclusion happen).
fn object_error(g: &mut Gpu, speed: f64, alpha: f32) -> (f64, f64, Vec<u8>, Vec<u8>) {
    let (w, h) = (640, 360);
    let x0 = 200.0;
    let f0 = upload(g, w, h, |x, y| object_scene(x, y, x0));
    let f1 = upload(g, w, h, |x, y| object_scene(x, y, x0 + speed));
    let cmd = g.command_buffer().unwrap();
    g.ingest(&cmd, f0, true).unwrap();
    g.ingest(&cmd, f1, true).unwrap();
    let (t, _) = g.compose(&cmd, alpha, None, (0.0, 0.0)).unwrap();
    let got = shown(g, &cmd, &t);
    finish(&cmd);
    let truth = upload(g, w, h, |x, y| object_scene(x, y, x0 + speed * (1.0 + f64::from(alpha))));
    let truth_px = read(&shown_source(g, &truth));
    let got_px = read(&got);
    let all = mean_abs_err(&got_px, &truth_px, w, h, 30);
    // the swept band: the object's rows, from its left edge before the move to its right edge after
    let (bx0, bx1) = ((x0 - 20.0) as usize, (x0 + speed * 2.0 + 140.0 + 20.0) as usize);
    let mut s = 0.0;
    let mut n = 0.0;
    for y in 100..250 {
        for x in bx0..bx1 {
            for c in 0..3 {
                s += (f64::from(got_px[(y * w + x) * 4 + c]) - f64::from(truth_px[(y * w + x) * 4 + c])).abs();
                n += 1.0;
            }
        }
    }
    (all, s / n, got_px, truth_px)
}

#[test]
fn v2_extrapolation_beats_v1_where_it_matters_around_moving_objects() {
    let mut g = gpu();
    for (speed, alpha) in [(6.0, 0.5f32), (10.0, 0.5), (10.0, 1.0), (4.0, 0.42)] {
        g.legacy_warp = true;
        let (all1, band1, _, _) = object_error(&mut g, speed, alpha);
        g.legacy_warp = false;
        let (all2, band2, _, _) = object_error(&mut g, speed, alpha);
        eprintln!(
            "object {speed}px/frame alpha {alpha}: v1 all {all1:.3} band {band1:.3} | v2 all {all2:.3} band {band2:.3}"
        );
        assert!(band2 < band1 * 0.5, "v2 must at least halve the artifact band error: {band2:.2} vs {band1:.2}");
        assert!(all2 < all1, "{all2} vs {all1}");
    }
}

#[test]
fn global_motion_is_read_back_and_a_shift_moves_the_picture() {
    let mut g = gpu();
    let (w, h) = (640, 360);
    let f0 = upload(&g, w, h, |x, y| texture(x, y, 0.0));
    let f1 = upload(&g, w, h, |x, y| texture(x, y, 8.0));
    let cmd = g.command_buffer().unwrap();
    g.ingest(&cmd, f0, true).unwrap();
    assert!(g.global_motion().is_none(), "needs two frames");
    g.ingest(&cmd, f1, true).unwrap();
    // a camera-compensation shift of +10 px (content moves right) with no extrapolation
    let (t, _) = g.compose(&cmd, 0.0, None, (-10.0, 0.0)).unwrap();
    let got = shown(&g, &cmd, &t);
    finish(&cmd);
    let gm = g.global_motion().expect("two frames");
    eprintln!("global motion {gm:?}");
    assert!((gm.x - 8.0).abs() < 1.0 && gm.y.abs() < 1.0 && gm.support > 0.9, "{gm:?}");
    let truth = upload(&g, w, h, |x, y| texture(x, y, 18.0));
    let truth_px = read(&shown_source(&g, &truth));
    let err = mean_abs_err(&read(&got), &truth_px, w, h, 40);
    assert!(err < 1.0, "shifted picture should match content moved 10 px further: {err}");
}

#[test]
fn interpolation_lands_exactly_between_two_frames_and_beats_extrapolation_around_objects() {
    let mut g = gpu();
    let (w, h) = (640, 360);
    // pure pan: the midpoint of 0 and 8 px is 4 px
    let f0 = upload(&g, w, h, |x, y| texture(x, y, 0.0));
    let f1 = upload(&g, w, h, |x, y| texture(x, y, 8.0));
    let cmd = g.command_buffer().unwrap();
    g.ingest(&cmd, f0, true).unwrap();
    g.ingest(&cmd, f1, true).unwrap();
    let (t, _) = g.compose_interp(&cmd, 0.5, None).unwrap();
    let got = shown(&g, &cmd, &t);
    finish(&cmd);
    let truth = upload(&g, w, h, |x, y| texture(x, y, 4.0));
    let err = mean_abs_err(&read(&got), &read(&shown_source(&g, &truth)), w, h, 40);
    assert!(err < 0.6, "pan midpoint error {err}");

    // ends are the real frames themselves
    let cmd = g.command_buffer().unwrap();
    let (a, _) = g.compose_interp(&cmd, 0.0, None).unwrap();
    let (b, _) = g.compose_interp(&cmd, 1.0, None).unwrap();
    assert_eq!((a.width(), b.width()), (w, w));
    finish(&cmd);

    // a moving object: interpolating halfway is easier than extrapolating halfway (both neighbours are known)
    for speed in [6.0f64, 10.0] {
        let mut g2 = gpu();
        let o0 = upload(&g2, w, h, |x, y| object_scene(x, y, 200.0));
        let o1 = upload(&g2, w, h, |x, y| object_scene(x, y, 200.0 + speed));
        let cmd = g2.command_buffer().unwrap();
        g2.ingest(&cmd, o0, true).unwrap();
        g2.ingest(&cmd, o1, true).unwrap();
        let (t, _) = g2.compose_interp(&cmd, 0.5, None).unwrap();
        let got = shown(&g2, &cmd, &t);
        finish(&cmd);
        let truth = upload(&g2, w, h, |x, y| object_scene(x, y, 200.0 + speed * 0.5));
        let truth_px = read(&shown_source(&g2, &truth));
        let got_px = read(&got);
        let band = {
            let (mut s, mut n) = (0.0, 0.0);
            for y in 100..250 {
                for x in 180..(200.0 + speed + 160.0) as usize {
                    for c in 0..3 {
                        s += (f64::from(got_px[(y * w + x) * 4 + c]) - f64::from(truth_px[(y * w + x) * 4 + c])).abs();
                        n += 1.0;
                    }
                }
            }
            s / n
        };
        let (_, ex_band, _, _) = object_error(&mut g2, speed, 0.5);
        eprintln!("object {speed}px/frame: interpolated band error {band:.2}, extrapolated {ex_band:.2}");
        assert!(band < ex_band, "interpolation should be the cleaner picture: {band} vs {ex_band}");
        assert!(band < 3.0, "{band}");
    }
}

/// MetalFX frame interpolation fed with our own motion field: after its history is warm, the generated frame is the picture halfway
/// between the last two real frames (a pan: half a step further than the older frame), clearly closer to the truth than either neighbour.
#[test]
fn metalfx_midpoint_lands_between_two_frames_on_a_pan() {
    let mut g = gpu();
    if !g.fx_available {
        eprintln!("MetalFX frame interpolation is not available here: skipped");
        return;
    }
    g.fx_use = true;
    let (w, h, step) = (640, 360, 8.0f64);
    let mut last = None;
    for k in 0..8 {
        let f = upload(&g, w, h, |x, y| texture(x, y, step * f64::from(k)));
        let cmd = g.command_buffer().unwrap();
        g.ingest(&cmd, f, true).unwrap();
        finish(&cmd);
        last = Some(k);
    }
    let k = last.unwrap();
    let cmd = g.command_buffer().unwrap();
    let (t, _) = g.compose_interp(&cmd, 0.5, None).unwrap();
    let got = shown(&g, &cmd, &t);
    finish(&cmd);
    assert!(g.fx_active(), "MetalFX should have been used for the midpoint");
    let truth = upload(&g, w, h, |x, y| texture(x, y, step * (f64::from(k) - 0.5)));
    let truth_px = read(&shown_source(&g, &truth));
    let err = mean_abs_err(&read(&got), &truth_px, w, h, 40);
    let newer = upload(&g, w, h, |x, y| texture(x, y, step * f64::from(k)));
    let older = upload(&g, w, h, |x, y| texture(x, y, step * (f64::from(k) - 1.0)));
    let e_new = mean_abs_err(&read(&shown_source(&g, &newer)), &truth_px, w, h, 40);
    let e_old = mean_abs_err(&read(&shown_source(&g, &older)), &truth_px, w, h, 40);
    eprintln!("MetalFX midpoint error {err:.2}; newer frame {e_new:.2}; older frame {e_old:.2}");
    assert!(err < e_new.min(e_old) * 0.5, "midpoint {err:.2} should beat both neighbours ({e_new:.2}, {e_old:.2})");
}

#[test]
fn metalfx_is_off_unless_asked_for_and_falls_back_to_the_built_in_midpoint() {
    let mut g = gpu();
    let (w, h) = (640, 360);
    let f0 = upload(&g, w, h, |x, y| texture(x, y, 0.0));
    let f1 = upload(&g, w, h, |x, y| texture(x, y, 8.0));
    let cmd = g.command_buffer().unwrap();
    g.ingest(&cmd, f0, true).unwrap();
    g.ingest(&cmd, f1, true).unwrap();
    let (t, _) = g.compose_interp(&cmd, 0.5, None).unwrap();
    let got = shown(&g, &cmd, &t);
    finish(&cmd);
    assert!(!g.fx_use && !g.fx_active(), "off by default");
    let truth = upload(&g, w, h, |x, y| texture(x, y, 4.0));
    let err = mean_abs_err(&read(&got), &read(&shown_source(&g, &truth)), w, h, 40);
    assert!(err < 0.6, "the built-in midpoint is used: {err}");
}

/// Showing the picture smaller than the stream (a small window) must average the source pixels it covers. A single bilinear tap lands on
/// one stripe or the next and the whole picture shimmers: fine 1-pixel stripes must come out as an even grey.
#[test]
fn shrinking_a_picture_averages_instead_of_skipping_pixels() {
    let g = gpu();
    let (w, h) = (1280, 720);
    let stripes = upload(&g, w, h, |x, _| if x % 2 == 0 { [0, 0, 0, 255] } else { [255, 255, 255, 255] });
    for (tw, th) in [(427, 240), (640, 360), (320, 180)] {
        let tgt = target(&g, tw, th);
        let cmd = g.command_buffer().unwrap();
        g.present(&cmd, &stripes, &tgt).unwrap();
        finish(&cmd);
        let px = read(&tgt);
        let vals: Vec<f64> = px.as_chunks::<4>().0.iter().map(|p| f64::from(p[1])).collect();
        let mean = vals.iter().sum::<f64>() / vals.len() as f64;
        let sd = (vals.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / vals.len() as f64).sqrt();
        eprintln!("{w}x{h} -> {tw}x{th}: mean {mean:.1}, spread {sd:.1}");
        assert!((mean - 127.5).abs() < 12.0, "mean {mean}");
        assert!(sd < 20.0, "{tw}x{th}: stripes should average out, spread {sd:.1}");
    }
}

/// At or above its own size the picture is shown with a single bilinear tap (nothing is blurred).
#[test]
fn a_picture_shown_at_its_own_size_is_untouched() {
    let g = gpu();
    let (w, h) = (256, 144);
    let src = upload(&g, w, h, |x, y| [(x % 256) as u8, (y % 256) as u8, ((x + y) % 256) as u8, 255]);
    let tgt = target(&g, w, h);
    let cmd = g.command_buffer().unwrap();
    g.present(&cmd, &src, &tgt).unwrap();
    finish(&cmd);
    assert_eq!(read(&tgt), read(&src));
}

fn edge_energy(px: &[u8], w: usize, h: usize) -> f64 {
    let l: Vec<f64> = px
        .as_chunks::<4>()
        .0
        .iter()
        .map(|p| 0.114 * f64::from(p[0]) + 0.587 * f64::from(p[1]) + 0.299 * f64::from(p[2]))
        .collect();
    let mut s = 0.0;
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let (gx, gy) = (l[y * w + x + 1] - l[y * w + x - 1], l[(y + 1) * w + x] - l[(y - 1) * w + x]);
            s += (gx * gx + gy * gy).sqrt();
        }
    }
    s / ((w - 2) * (h - 2)) as f64
}

/// The sharpness setting blends MetalFX's upscaler with a plain smooth enlargement: more sharpness, more edge contrast, in order.
#[test]
fn the_sharpness_control_orders_edge_contrast_from_soft_to_sharp() {
    let mut g = gpu();
    if !g.spatial_scaler_available {
        return;
    }
    let (w, h) = (320, 180);
    let src = upload(&g, w, h, |x, y| {
        let on = ((x / 20) + (y / 20)) % 2 == 0;
        if on {
            [220, 220, 220, 255]
        } else {
            [40, 40, 40, 255]
        }
    });
    let mut energy = vec![];
    let cmd = g.command_buffer().unwrap();
    g.ingest(&cmd, src, false).unwrap();
    finish(&cmd);
    for k in [0.0f32, 0.5, 1.0] {
        g.sharpness = k;
        let cmd = g.command_buffer().unwrap();
        let (t, up) = g.compose(&cmd, 0.0, Some((w * 2, h * 2)), (0.0, 0.0)).unwrap();
        assert!(up, "the scaler should have been used");
        let out = shown(&g, &cmd, &t);
        finish(&cmd);
        let px = read(&out);
        energy.push(edge_energy(&px, out.width(), out.height()));
    }
    eprintln!("edge energy soft/natural/sharp: {energy:?}");
    assert!(energy[0] < energy[1] && energy[1] < energy[2], "{energy:?}");
}

/// Saturation scales a colour's distance from its own luma: 0 is grey, 1 untouched, above 1 more vivid; neutral greys never change.
#[test]
fn saturation_moves_colours_toward_grey_or_away_from_it() {
    let mut g = gpu();
    let (w, h) = (64, 64);
    // BGRA: a muted orange and a neutral grey
    let src = upload(&g, w, h, |x, _| if x < 32 { [60, 120, 200, 255] } else { [128, 128, 128, 255] });
    let mut chroma = vec![];
    for sat in [0.0f32, 1.0, 1.6] {
        g.saturation = sat;
        let tgt = target(&g, w, h);
        let cmd = g.command_buffer().unwrap();
        g.present(&cmd, &src, &tgt).unwrap();
        finish(&cmd);
        let px = read(&tgt);
        let o = (10 * w + 8) * 4; // orange
        let n = (10 * w + 50) * 4; // grey
        chroma.push((i32::from(px[o + 2]) - i32::from(px[o])).abs()); // red - blue spread
        assert!(
            (i32::from(px[n]) - 128).abs() <= 1 && (i32::from(px[n + 2]) - 128).abs() <= 1,
            "grey stays grey at {sat}"
        );
    }
    eprintln!("colour spread at saturation 0 / 1 / 1.6: {chroma:?}");
    assert!(chroma[0] <= 1, "no saturation is grey: {chroma:?}");
    assert!(chroma[0] < chroma[1] && chroma[1] < chroma[2], "{chroma:?}");
    assert_eq!(chroma[1], 140, "saturation 1 leaves the colour as it is");
}

/// Without the upscaler the sharpness setting is an unsharp mask on the final picture: positive sharpens, negative softens, zero is untouched.
#[test]
fn the_unsharp_amount_orders_edge_contrast_when_nothing_is_upscaled() {
    let mut g = gpu();
    let (w, h) = (256, 144);
    let src =
        upload(&g, w, h, |x, y| if ((x / 16) + (y / 16)) % 2 == 0 { [200, 200, 200, 255] } else { [60, 60, 60, 255] });
    let mut energy = vec![];
    for amount in [-0.5f32, 0.0, 0.5, 1.5] {
        g.sharpen = amount;
        let tgt = target(&g, w, h);
        let cmd = g.command_buffer().unwrap();
        g.present(&cmd, &src, &tgt).unwrap();
        finish(&cmd);
        energy.push(edge_energy(&read(&tgt), w, h));
    }
    eprintln!("edge energy at unsharp -0.5 / 0 / 0.5 / 1.5: {energy:?}");
    assert!(energy[0] < energy[1] && energy[1] < energy[2] && energy[2] < energy[3], "{energy:?}");
}
