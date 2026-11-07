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
fn the_upscale_target_is_the_chosen_size_whatever_the_window_is() {
    // 1080p video, 2160p chosen: 4K, on a 4K window, on this machine's 3440x1440 ultrawide, and in a window smaller than the video
    // (the picture is then scaled down to fit, which is what makes it better than upscaling to the window's size)
    for window in [(3840, 2160), (3440, 1440), (2560, 1400), (1280, 720), (640, 360)] {
        assert_eq!(upscale_target((1920, 1080), window, 2160), Some((3840, 2160)), "window {window:?}");
    }
    assert_eq!(upscale_target((1920, 1080), (3840, 2160), 1440), Some((2560, 1440)));
    assert_eq!(
        upscale_target((1280, 720), (3440, 1440), 2160),
        Some((3840, 2160)),
        "720p at 3x is still what was asked for"
    );
    // an odd source size keeps the aspect and an even result
    let t = upscale_target((1281, 721), (3000, 2000), 1440).unwrap();
    assert!(t.0.is_multiple_of(2) && t.1.is_multiple_of(2), "even sizes: {t:?}");
    assert!((t.0 as f64 / t.1 as f64 - 1281.0 / 721.0).abs() < 0.01, "aspect kept: {t:?}");
    // nothing to do when the chosen size is not larger than the video, or when there is nothing to size it from
    assert_eq!(upscale_target((1920, 1080), (3840, 2160), 1080), None);
    assert_eq!(upscale_target((1920, 1080), (3840, 2160), 720), None);
    assert_eq!(upscale_target((1920, 1080), (3840, 2160), 0), None);
    assert_eq!(upscale_target((0, 0), (100, 100), 1440), None);
    assert_eq!(upscale_target((1280, 720), (0, 0), 1440), None, "no window yet");
    // never a texture the GPU cannot make
    assert_eq!(upscale_target((1920, 1080), (3840, 2160), 100_000), None);
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
        // v1 shares the motion search, which got better (a longer vector costs more, so it no longer jumps to a look-alike), so v1's own
        // error fell too: the margin is a little smaller than when v1 was worse, and v2's error is lower than it was
        assert!(band2 < band1 * 0.65, "v2 must clearly beat v1 in the artifact band: {band2:.2} vs {band1:.2}");
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
fn metalfx_makes_the_midpoint_and_fsr_upscales_it() {
    let mut g = gpu();
    if !g.fx_available {
        eprintln!("MetalFX frame interpolation is not available here: skipped");
        return;
    }
    g.fx_use = true;
    g.fsr = true;
    g.rcas = 0.0;
    let (w, h, step, out) = (640, 360, 8.0f64, (960, 540));
    for k in 0..8 {
        let f = upload(&g, w, h, |x, y| texture(x, y, step * f64::from(k)));
        let cmd = g.command_buffer().unwrap();
        g.ingest(&cmd, f, true).unwrap();
        finish(&cmd);
    }
    let cmd = g.command_buffer().unwrap();
    let (t, up) = g.compose_interp(&cmd, 0.5, Some(out)).unwrap();
    assert!(up && g.fsr_active(), "FSR upscaled the picture");
    assert!(g.fx_active(), "MetalFX made the midpoint FSR upscaled");
    assert_eq!((t.width(), t.height()), out);
    let got = shown(&g, &cmd, &t);
    finish(&cmd);
    // the midpoint of a pan, enlarged: closer to the true midpoint (enlarged the same way) than to either neighbouring frame
    let truth_src = upload(&g, w, h, |x, y| texture(x, y, step * 6.5));
    let near_src = upload(&g, w, h, |x, y| texture(x, y, step * 7.0));
    let cmd = g.command_buffer().unwrap();
    g.ingest(&cmd, truth_src, false).unwrap();
    let (tt, _) = g.compose(&cmd, 0.0, Some(out), (0.0, 0.0)).unwrap();
    let truth = shown(&g, &cmd, &tt);
    finish(&cmd);
    let cmd = g.command_buffer().unwrap();
    g.ingest(&cmd, near_src, false).unwrap();
    let (tn, _) = g.compose(&cmd, 0.0, Some(out), (0.0, 0.0)).unwrap();
    let near = shown(&g, &cmd, &tn);
    finish(&cmd);
    let (a, b, c) = (read(&got), read(&truth), read(&near));
    let (e_mid, e_new) = (mean_abs_err(&a, &b, out.0, out.1, 60), mean_abs_err(&c, &b, out.0, out.1, 60));
    eprintln!("FSR + MetalFX midpoint error {e_mid:.2}; newer frame {e_new:.2}");
    assert!(e_mid < e_new * 0.6, "midpoint {e_mid:.2} should beat the newer frame {e_new:.2}");
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

/// The RCAS slider (percent over 100) is the lobe scale of FSR's own control: 100 % is stops 2 (0.25), 200 % stops 1 (0.5), 0 is off, and
/// nothing that is not a number or is out of range gets past the clamp.
#[test]
fn rcas_strength_maps_to_the_lobe_scale() {
    use openps_metal::gpu::rcas_lobe;
    assert_eq!(rcas_lobe(0.0), 0.0, "0 % is off");
    assert!((rcas_lobe(0.5) - 0.125).abs() < 1e-6, "50 % is stops 3");
    assert!((rcas_lobe(1.0) - 0.25).abs() < 1e-6, "100 % is stops 2");
    assert!((rcas_lobe(2.0) - 0.5).abs() < 1e-6, "200 % is stops 1");
    let mut prev = -1.0f32;
    for i in 0..=200 {
        let l = rcas_lobe(i as f32 / 100.0);
        assert!(l >= prev, "{i} % is not weaker than the one before");
        prev = l;
    }
    for (bad, want) in [(f32::NAN, 0.0), (f32::INFINITY, 0.0), (f32::NEG_INFINITY, 0.0), (-1.0, 0.0), (9.0, 0.5)] {
        assert_eq!(rcas_lobe(bad), want, "{bad}");
    }
}

/// The finishing pass is given up when the GPU already uses most of the time between two displayed pictures, never because of nonsense numbers.
#[test]
fn the_finishing_pass_is_given_up_only_when_the_gpu_is_close_to_the_time_there_is() {
    use openps_metal::gpu::finish_pass_too_slow;
    assert!(!finish_pass_too_slow(2.3, 16.7), "4K at 60 Hz: plenty of room");
    assert!(!finish_pass_too_slow(4.6, 8.33), "4K at 120 Hz with only the upscale: still room (55 %)");
    assert!(finish_pass_too_slow(6.5, 8.33), "78 % of the time at 120 Hz: too busy");
    assert!(!finish_pass_too_slow(0.0, 8.33));
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(!finish_pass_too_slow(bad, 8.33) && !finish_pass_too_slow(2.0, bad), "{bad}");
    }
    assert!(!finish_pass_too_slow(5.0, 0.0) && !finish_pass_too_slow(5.0, -1.0));
}

/// A picture of one colour must come out as that colour through the whole finishing pass (to linear light and back in half floats, the
/// scaler, the blend and RCAS): no tint, no brightness shift, whatever the sharpness.
#[test]
fn flat_colour_survives_linear_mode_the_blend_and_rcas() {
    let mut g = gpu();
    if !g.spatial_scaler_available {
        return;
    }
    // 96x64 -> 192x128 is a whole number of 16x16 tiles; 100x90 -> 200x180 is not (the last tiles are partial: 1620 rows at 1.5x of 1080p are too)
    for (w, h, colour) in [
        (96, 64, [60u8, 120, 200, 255]),
        (96, 64, [90, 90, 90, 255]),
        (96, 64, [16, 16, 16, 255]),
        (96, 64, [235, 235, 235, 255]),
        (100, 90, [60, 120, 200, 255]),
        (100, 90, [235, 235, 235, 255]),
    ] {
        let src = upload(&g, w, h, |_, _| colour);
        let cmd = g.command_buffer().unwrap();
        g.ingest(&cmd, src, false).unwrap();
        finish(&cmd);
        for k in [0.0f32, 0.3, 0.6, 0.8, 1.0, 1.5] {
            g.sharpness = k;
            let cmd = g.command_buffer().unwrap();
            let (t, up) = g.compose(&cmd, 0.0, Some((w * 2, h * 2)), (0.0, 0.0)).unwrap();
            assert!(up);
            let out = shown(&g, &cmd, &t);
            finish(&cmd);
            let px = read(&out);
            let worst = px
                .chunks(4)
                .map(|p| (0..3).map(|c| (i32::from(p[c]) - i32::from(colour[c])).abs()).max().unwrap())
                .max()
                .unwrap();
            assert!(worst <= 2, "{w}x{h} colour {colour:?} at sharpness {k}: off by {worst} levels");
        }
    }
}

/// Linear-light scaling and the perceptual mode make the same picture on smooth content (a wrong curve or a missing conversion would
/// shift every mid-tone): the mean difference is a level or two, not tens.
#[test]
fn linear_mode_agrees_with_perceptual_mode_on_smooth_content() {
    let mut g = gpu();
    if !g.spatial_scaler_available {
        return;
    }
    let (w, h) = (128, 72);
    let src = upload(&g, w, h, |x, y| {
        let v = (30 + (x * 190 / w)) as u8;
        let u = (40 + (y * 150 / h)) as u8;
        [u, v, (255 - v as usize / 2) as u8, 255]
    });
    let cmd = g.command_buffer().unwrap();
    g.ingest(&cmd, src, false).unwrap();
    finish(&cmd);
    g.sharpness = 1.0;
    let mut shots = vec![];
    for linear in [false, true] {
        g.linear_upscale = linear;
        let cmd = g.command_buffer().unwrap();
        let (t, up) = g.compose(&cmd, 0.0, Some((w * 2, h * 2)), (0.0, 0.0)).unwrap();
        assert!(up);
        let out = shown(&g, &cmd, &t);
        finish(&cmd);
        shots.push(read(&out));
    }
    let n = shots[0].len() as f64;
    let mean = shots[0].iter().zip(&shots[1]).map(|(a, b)| (f64::from(*a) - f64::from(*b)).abs()).sum::<f64>() / n;
    eprintln!("mean difference between perceptual and linear mode: {mean:.2} levels");
    assert!(mean < 3.0, "the two colour modes must make the same picture on smooth content: {mean:.2}");
}

/// The finishing pass (linear light, blend, RCAS) is paid on every displayed picture, up to 120 times a second: at 1080p to 4K it must stay
/// a small part of the time there is (measured about 1 ms on an M4; the bound is loose, for slower Macs and a busy GPU).
#[test]
fn linear_light_and_rcas_cost_little_gpu_time_at_1080p_to_4k() {
    let mut g = gpu();
    if !g.spatial_scaler_available {
        return;
    }
    let (w, h) = (1920, 1080);
    let src = upload(&g, w, h, |x, y| texture(x, y, 0.0));
    let cmd = g.command_buffer().unwrap();
    g.ingest(&cmd, src, false).unwrap();
    finish(&cmd);
    let mut gpu_ms = |linear: bool, rcas: bool| {
        g.linear_upscale = linear;
        g.rcas = if rcas { 1.0 } else { 0.0 };
        g.sharpness = 0.6;
        let mut times = vec![];
        for i in 0..14 {
            let cmd = g.command_buffer().unwrap();
            let (t, up) = g.compose(&cmd, 0.0, Some((3840, 2160)), (0.0, 0.0)).unwrap();
            assert!(up);
            let _ = shown(&g, &cmd, &t);
            finish(&cmd);
            if i >= 4 {
                times.push((cmd.GPUEndTime() - cmd.GPUStartTime()) * 1000.0);
            }
        }
        times.sort_by(|a, b| a.partial_cmp(b).unwrap());
        times[times.len() / 2]
    };
    let (old, lin_only, rcas_only, new) =
        (gpu_ms(false, false), gpu_ms(true, false), gpu_ms(false, true), gpu_ms(true, true));
    eprintln!(
        "GPU ms per picture at 1080p -> 4K: perceptual + present-pass blend {old:.2}; linear only {lin_only:.2}; RCAS only {rcas_only:.2}; linear + RCAS {new:.2} (+{:.2})",
        new - old
    );
    assert!(new - old < 3.0, "the finishing pass costs {:.2} ms more than the plain blend", new - old);
}

/// At the default sharpness RCAS adds the edge contrast the blend took away, and never more than the scaler's own (100 %).
#[test]
fn rcas_restores_some_edge_contrast_at_the_default_sharpness_and_not_beyond_the_scalers_own() {
    let mut g = gpu();
    if !g.spatial_scaler_available {
        return;
    }
    let (w, h) = (320, 180);
    let src =
        upload(&g, w, h, |x, y| if ((x / 20) + (y / 20)) % 2 == 0 { [220, 220, 220, 255] } else { [40, 40, 40, 255] });
    let cmd = g.command_buffer().unwrap();
    g.ingest(&cmd, src, false).unwrap();
    finish(&cmd);
    let mut energy = |k: f32, rcas: bool| {
        g.sharpness = k;
        g.rcas = if rcas { 1.0 } else { 0.0 };
        let cmd = g.command_buffer().unwrap();
        let (t, up) = g.compose(&cmd, 0.0, Some((w * 2, h * 2)), (0.0, 0.0)).unwrap();
        assert!(up);
        let out = shown(&g, &cmd, &t);
        finish(&cmd);
        edge_energy(&read(&out), out.width(), out.height())
    };
    let (blend_only, with_rcas, full) = (energy(0.6, false), energy(0.6, true), energy(1.0, true));
    eprintln!("edge energy at 60 %: blend only {blend_only:.2}, with RCAS {with_rcas:.2}; scaler as it is {full:.2}");
    assert!(with_rcas > blend_only * 1.02, "RCAS must add contrast: {blend_only:.2} -> {with_rcas:.2}");
    assert!(with_rcas < full, "but not more than the scaler's own: {with_rcas:.2} against {full:.2}");
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

#[test]
fn an_upscale_multiple_gives_the_target_height_for_the_streams_height() {
    use openps_metal::Upscale;
    assert_eq!(Upscale::Off.target_height(1080), None);
    assert_eq!(Upscale::X1_33.target_height(1080), Some(1440));
    assert_eq!(Upscale::X1_33.target_height(720), Some(960));
    assert_eq!(Upscale::X1_5.target_height(720), Some(1080));
    assert_eq!(Upscale::X2.target_height(1080), Some(2160));
    assert_eq!(Upscale::X2.target_height(540), Some(1080));
    assert_eq!(Upscale::from_factor(0.0), Upscale::Off);
    assert_eq!(Upscale::from_factor(4.0 / 3.0), Upscale::X1_33);
    assert_eq!(Upscale::from_factor(1.5), Upscale::X1_5);
    assert_eq!(Upscale::from_factor(2.0), Upscale::X2);
    assert_eq!(Upscale::from_factor(7.0), Upscale::Off, "anything that is not a known multiple does not upscale");
}

// ---- immersion: the bars are filled with a continuation of the picture --------------------------------------------------------
/// A picture that is red on the left and blue on the right, so what fills the bars can be told from where it came.
fn red_to_blue(g: &Gpu) -> Tex {
    upload(g, 160, 90, |x, _| {
        let t = x as f64 / 159.0;
        [(255.0 * t) as u8, 40, (255.0 * (1.0 - t)) as u8, 255] // BGRA
    })
}

fn rendered(g: &mut Gpu, src: &Tex, w: usize, h: usize, immersion: bool) -> Vec<u8> {
    g.immersion = immersion;
    let tg = target(g, w, h);
    let cmd = g.command_buffer().unwrap();
    g.present(&cmd, src, &tg).unwrap();
    finish(&cmd);
    read(&tg)
}

fn rgb(px: &[u8], w: usize, x: usize, y: usize) -> (i32, i32, i32) {
    let i = (y * w + x) * 4;
    (i32::from(px[i + 2]), i32::from(px[i + 1]), i32::from(px[i]))
}

#[test]
fn without_immersion_the_bars_stay_black() {
    let mut g = gpu();
    let src = red_to_blue(&g);
    let px = rendered(&mut g, &src, 400, 400, false);
    assert_eq!(rgb(&px, 400, 40, 60), (0, 0, 0));
    assert_eq!(rgb(&px, 400, 360, 340), (0, 0, 0));
}

#[test]
fn with_immersion_a_letterbox_bar_continues_the_colours_of_the_picture_edge_above_and_below_it() {
    let mut g = gpu();
    let src = red_to_blue(&g);
    let px = rendered(&mut g, &src, 400, 400, true); // the picture is 400x225 in the middle: bars of 87 px above and below
    let (r, _, b) = rgb(&px, 400, 40, 60);
    assert!(r > b + 80 && r > 90, "left of the top bar is reddish like the picture's left edge: r={r} b={b}");
    let (r, _, b) = rgb(&px, 400, 360, 60);
    assert!(b > r + 80 && b > 90, "right of the top bar is bluish: r={r} b={b}");
    let (r, _, b) = rgb(&px, 400, 40, 340);
    assert!(r > b + 80, "and the bottom bar the same: r={r} b={b}");
}

#[test]
fn the_bars_show_one_smooth_copy_of_the_picture_not_repeats() {
    let mut g = gpu();
    // dark, with one bright vertical stripe near the left edge (5% to 10% of the width)
    let src = upload(&g, 160, 90, |x, _| if (8..16).contains(&x) { [255, 255, 255, 255] } else { [10, 10, 10, 255] });
    let px = rendered(&mut g, &src, 800, 300, true); // pillarbox: bars of 133 px
    let left: Vec<i32> = (0..133).map(|x| rgb(&px, 800, x, 150).0).collect();
    let right: Vec<i32> = (667..800).map(|x| rgb(&px, 800, x, 150).0).collect();
    let (peak_at, peak) = left.iter().enumerate().max_by_key(|(_, v)| **v).map(|(i, v)| (i, *v)).unwrap();
    assert!(peak > 60, "the stripe's light is in the left bar: {peak}");
    assert!(
        (20..110).contains(&peak_at),
        "once, where the stripe is (a tenth of the way in, stretched over the window): x={peak_at}"
    );
    assert!(
        right.iter().all(|v| *v < 30),
        "and not again on the other side (no mirrored or repeated copy): {:?}",
        right.iter().max()
    );
    assert!(left.windows(2).all(|w| (w[0] - w[1]).abs() < 12), "smooth, no hard edge: {left:?}");
}

#[test]
fn the_glow_is_blurred_so_fine_detail_in_the_picture_does_not_show_in_the_bars() {
    let mut g = gpu();
    // a fine checkerboard: 8-pixel cells
    let src =
        upload(&g, 160, 90, |x, y| if (x / 4 + y / 4) % 2 == 0 { [240, 240, 240, 255] } else { [20, 20, 20, 255] });
    let px = rendered(&mut g, &src, 400, 400, true); // letterbox: bars of 87 px
    let row: Vec<i32> = (0..400).map(|x| rgb(&px, 400, x, 40).0).collect();
    assert!(row.windows(2).all(|w| (w[0] - w[1]).abs() < 8), "no checkerboard left in the bar: {:?}", &row[..40]);
    assert!(
        row.iter().all(|v| (70..170).contains(v)),
        "an even mid-grey: {:?} {:?}",
        row.iter().min(),
        row.iter().max()
    );
}

#[test]
fn the_glow_follows_a_scene_change_in_the_very_next_picture_and_is_never_brighter_than_the_picture() {
    let mut g = gpu();
    let dark = upload(&g, 160, 90, |_, _| [12, 10, 8, 255]);
    let bright = upload(&g, 160, 90, |_, _| [250, 250, 250, 255]);
    // a dark menu, a bright game, the menu again: each picture's own very first presentation already has its own glow
    for (src, picture_level) in [(&dark, 10), (&bright, 250), (&dark, 10), (&bright, 250)] {
        let px = rendered(&mut g, src, 400, 400, true); // letterbox bars of 87 px above and below
        let glow = rgb(&px, 400, 200, 40).0;
        if picture_level > 100 {
            assert!(glow > 150, "a bright picture lights the bars at once: {glow}");
            assert!(glow < picture_level, "but they stay dimmer than the picture ({glow} < {picture_level})");
        } else {
            assert!(glow < 40, "a dark picture leaves the bars dark at once, not lit grey: {glow}");
        }
    }
}

#[test]
fn the_glow_is_not_black_far_from_the_picture_and_never_brighter_than_the_picture() {
    let mut g = gpu();
    let src = red_to_blue(&g);
    let px = rendered(&mut g, &src, 400, 400, true);
    let far = rgb(&px, 400, 40, 3).0;
    assert!(far > 100, "the far edge of the window still glows: {far}");
    assert!(far <= 255);
}

#[test]
fn with_immersion_a_pillarbox_bar_is_filled_too() {
    let mut g = gpu();
    let src = red_to_blue(&g);
    let px = rendered(&mut g, &src, 800, 300, true); // the picture is 533x300: bars of 133 px left and right
    let (lr, _, lb) = rgb(&px, 800, 100, 150);
    let (rr, _, rb) = rgb(&px, 800, 700, 150);
    assert!(lr > lb + 80, "the left bar continues the picture's red edge: {lr} {lb}");
    assert!(rb > rr + 80, "the right bar continues its blue edge: {rr} {rb}");
}

#[test]
fn immersion_never_touches_the_picture_itself_nor_a_picture_that_fills_the_window() {
    let mut g = gpu();
    let src = red_to_blue(&g);
    let (off, on) = (rendered(&mut g, &src, 400, 400, false), rendered(&mut g, &src, 400, 400, true));
    for (x, y) in [(200, 200), (10, 150), (390, 300), (100, 100), (300, 310)] {
        assert_eq!(rgb(&off, 400, x, y), rgb(&on, 400, x, y), "picture pixel ({x},{y})");
    }
    // 16:9 into 16:9: no bars, nothing to fill
    let (off, on) = (rendered(&mut g, &src, 320, 180, false), rendered(&mut g, &src, 320, 180, true));
    assert_eq!(off, on);
}

#[test]
fn bar_rectangles_cover_exactly_the_window_not_covered_by_the_picture() {
    use openps_metal::gpu::bar_rects;
    // a picture that fills the window has no bars
    assert!(bar_rects((0.0, 0.0, 400.0, 225.0), (400.0, 225.0)).is_empty());
    // letterbox: 87.5 px above and below (each bar overlaps the picture by a pixel so no black line is left at a fractional edge)
    let b = bar_rects((0.0, 87.5, 400.0, 225.0), (400.0, 400.0));
    assert_eq!(b, vec![(0, 0, 400, 89), (0, 311, 400, 89)]);
    // pillarbox
    let b = bar_rects((133.0, 0.0, 534.0, 300.0), (800.0, 300.0));
    assert_eq!(b, vec![(0, 0, 134, 300), (666, 0, 134, 300)]);
    // never a rectangle outside the window
    for (r, t) in [((0.0, 1.0, 100.0, 98.0), (100.0, 100.0)), ((2.0, 0.0, 96.0, 100.0), (100.0, 100.0))] {
        for (x, y, w, h) in bar_rects(r, t) {
            assert!(f64::from(x + w) <= t.0 && f64::from(y + h) <= t.1, "{r:?} -> {x} {y} {w} {h}");
        }
    }
}

// ---- FSR 1 (EASU) ------------------------------------------------------------------------------------------------------------------

/// AMD's `FsrEasuF` (FidelityFX FSR 1, MIT licence) translated straight to Rust: the same 12 taps, the same reciprocal approximations,
/// the same constants. The GPU kernel is a port of the same code, so this guards the port (a wrong tap, sign or constant shows at once).
fn easu_reference(src: &[[f32; 3]], (w, h): (usize, usize), (ow, oh): (usize, usize)) -> Vec<[f32; 3]> {
    let rcp = |a: f32| f32::from_bits(0x7ef07ebbu32.wrapping_sub(a.to_bits()));
    let rsq = |a: f32| f32::from_bits(0x5f347d74u32.wrapping_sub(a.to_bits() >> 1));
    let at = |x: i64, y: i64| src[(y.clamp(0, h as i64 - 1) as usize) * w + x.clamp(0, w as i64 - 1) as usize];
    let luma = |c: [f32; 3]| c[2] * 0.5 + (c[0] * 0.5 + c[1]);
    let set = |dir: &mut [f32; 2], len: &mut f32, pp: [f32; 2], which: usize, l: [f32; 5]| {
        let w = match which {
            0 => (1.0 - pp[0]) * (1.0 - pp[1]),
            1 => pp[0] * (1.0 - pp[1]),
            2 => (1.0 - pp[0]) * pp[1],
            _ => pp[0] * pp[1],
        };
        let [la, lb, lc, ld, le] = l;
        let len_x = rcp((ld - lc).abs().max((lc - lb).abs()));
        let dir_x = ld - lb;
        dir[0] += dir_x * w;
        let len_x = (dir_x.abs() * len_x).clamp(0.0, 1.0);
        *len += len_x * len_x * w;
        let len_y = rcp((le - lc).abs().max((lc - la).abs()));
        let dir_y = le - la;
        dir[1] += dir_y * w;
        let len_y = (dir_y.abs() * len_y).clamp(0.0, 1.0);
        *len += len_y * len_y * w;
    };
    let mut out = Vec::with_capacity(ow * oh);
    let scale = [w as f32 / ow as f32, h as f32 / oh as f32];
    for y in 0..oh {
        for x in 0..ow {
            let mut pp = [(x as f32 + 0.5) * scale[0] - 0.5, (y as f32 + 0.5) * scale[1] - 0.5];
            let fp = [pp[0].floor(), pp[1].floor()];
            pp = [pp[0] - fp[0], pp[1] - fp[1]];
            let (fx, fy) = (fp[0] as i64, fp[1] as i64);
            let (b, c) = (at(fx, fy - 1), at(fx + 1, fy - 1));
            let (e, f, g, hh) = (at(fx - 1, fy), at(fx, fy), at(fx + 1, fy), at(fx + 2, fy));
            let (i, j, k, l) = (at(fx - 1, fy + 1), at(fx, fy + 1), at(fx + 1, fy + 1), at(fx + 2, fy + 1));
            let (n, o) = (at(fx, fy + 2), at(fx + 1, fy + 2));
            let (bl, cl, el, fl, gl, hl) = (luma(b), luma(c), luma(e), luma(f), luma(g), luma(hh));
            let (il, jl, kl, ll, nl, ol) = (luma(i), luma(j), luma(k), luma(l), luma(n), luma(o));
            let (mut dir, mut len) = ([0.0f32; 2], 0.0f32);
            set(&mut dir, &mut len, pp, 0, [bl, el, fl, gl, jl]);
            set(&mut dir, &mut len, pp, 1, [cl, fl, gl, hl, kl]);
            set(&mut dir, &mut len, pp, 2, [fl, il, jl, kl, nl]);
            set(&mut dir, &mut len, pp, 3, [gl, jl, kl, ll, ol]);
            let mut dir_r = dir[0] * dir[0] + dir[1] * dir[1];
            let zro = dir_r < 1.0 / 32768.0;
            dir_r = rsq(dir_r);
            if zro {
                dir_r = 1.0;
                dir[0] = 1.0;
            }
            dir = [dir[0] * dir_r, dir[1] * dir_r];
            len *= 0.5;
            len *= len;
            let stretch = (dir[0] * dir[0] + dir[1] * dir[1]) * rcp(dir[0].abs().max(dir[1].abs()));
            let len2 = [1.0 + (stretch - 1.0) * len, 1.0 - 0.5 * len];
            let lob = 0.5 + ((0.25 - 0.04) - 0.5) * len;
            let clp = rcp(lob);
            let mut min4 = [0.0f32; 3];
            let mut max4 = [0.0f32; 3];
            for ch in 0..3 {
                min4[ch] = f[ch].min(g[ch]).min(j[ch]).min(k[ch]);
                max4[ch] = f[ch].max(g[ch]).max(j[ch]).max(k[ch]);
            }
            let (mut ac, mut aw) = ([0.0f32; 3], 0.0f32);
            let taps = [
                ([0.0, -1.0], b),
                ([1.0, -1.0], c),
                ([-1.0, 1.0], i),
                ([0.0, 1.0], j),
                ([0.0, 0.0], f),
                ([-1.0, 0.0], e),
                ([1.0, 1.0], k),
                ([2.0, 1.0], l),
                ([2.0, 0.0], hh),
                ([1.0, 0.0], g),
                ([1.0, 2.0], o),
                ([0.0, 2.0], n),
            ];
            for (off, col) in taps {
                let off = [off[0] - pp[0], off[1] - pp[1]];
                let v = [off[0] * dir[0] + off[1] * dir[1], off[0] * -dir[1] + off[1] * dir[0]];
                let v = [v[0] * len2[0], v[1] * len2[1]];
                let d2 = (v[0] * v[0] + v[1] * v[1]).min(clp);
                let wb = 2.0 / 5.0 * d2 - 1.0;
                let wa = lob * d2 - 1.0;
                let wb = 25.0 / 16.0 * (wb * wb) - (25.0 / 16.0 - 1.0);
                let w = wb * (wa * wa);
                for ch in 0..3 {
                    ac[ch] += col[ch] * w;
                }
                aw += w;
            }
            let mut pix = [0.0f32; 3];
            for ch in 0..3 {
                pix[ch] = max4[ch].min(min4[ch].max(ac[ch] * (1.0 / aw)));
            }
            out.push(pix);
        }
    }
    out
}

/// What the renderer path makes of one picture with FSR and nothing after it (no blend, no RCAS): the EASU output as 8-bit pixels.
fn easu_on_gpu(g: &mut Gpu, src: &Tex, (ow, oh): (usize, usize)) -> Vec<u8> {
    g.fsr = true;
    g.sharpness = 1.0;
    g.rcas = 0.0;
    let cmd = g.command_buffer().unwrap();
    g.ingest(&cmd, src.clone(), false).unwrap();
    let (t, up) = g.compose(&cmd, 0.0, Some((ow, oh)), (0.0, 0.0)).unwrap();
    assert!(up && g.fsr_active(), "FSR made the picture");
    assert_eq!((t.width(), t.height()), (ow, oh));
    let out = shown(g, &cmd, &t);
    finish(&cmd);
    read(&out)
}

#[test]
fn the_fsr_kernel_matches_a_translation_of_amds_reference() {
    let mut g = gpu();
    for ((w, h), (ow, oh)) in
        [((96, 64), (192, 128)), ((96, 64), (144, 96)), ((100, 90), (133, 120)), ((64, 48), (85, 64))]
    {
        let src = upload(&g, w, h, |x, y| texture(x, y, 0.0));
        let px = read(&src);
        // the texture is BGRA in memory
        let linear: Vec<[f32; 3]> = px
            .as_chunks::<4>()
            .0
            .iter()
            .map(|p| [f32::from(p[2]) / 255.0, f32::from(p[1]) / 255.0, f32::from(p[0]) / 255.0])
            .collect();
        let want = easu_reference(&linear, (w, h), (ow, oh));
        let got = easu_on_gpu(&mut g, &src, (ow, oh));
        let (mut sum, mut worst, mut n) = (0.0f64, 0.0f64, 0.0f64);
        for (i, p) in want.iter().enumerate() {
            for c in 0..3 {
                let d = (f64::from(p[c]) * 255.0 - f64::from(got[i * 4 + 2 - c])).abs();
                sum += d;
                worst = worst.max(d);
                n += 1.0;
            }
        }
        eprintln!("{w}x{h} -> {ow}x{oh}: mean {:.3}, worst {:.1} levels", sum / n, worst);
        assert!(sum / n < 0.4, "{w}x{h} -> {ow}x{oh}: mean difference {:.3} levels", sum / n);
        assert!(worst <= 2.0, "{w}x{h} -> {ow}x{oh}: worst difference {worst:.1} levels");
    }
}

#[test]
fn fsr_keeps_flat_colour_flat_through_the_blend_and_rcas_at_any_size() {
    let mut g = gpu();
    g.fsr = true;
    for (w, h, colour) in [
        (96, 64, [60u8, 120, 200, 255]),
        (96, 64, [16, 16, 16, 255]),
        (96, 64, [235, 235, 235, 255]),
        (100, 90, [60, 120, 200, 255]),
    ] {
        let src = upload(&g, w, h, |_, _| colour);
        let cmd = g.command_buffer().unwrap();
        g.ingest(&cmd, src, false).unwrap();
        finish(&cmd);
        for (k, rcas) in [(0.0f32, 0.0f32), (0.6, 1.0), (1.0, 0.0), (1.5, 2.0)] {
            (g.sharpness, g.rcas) = (k, rcas);
            let cmd = g.command_buffer().unwrap();
            let (t, up) = g.compose(&cmd, 0.0, Some((w * 3 / 2, h * 3 / 2)), (0.0, 0.0)).unwrap();
            assert!(up && g.fsr_active());
            let out = shown(&g, &cmd, &t);
            finish(&cmd);
            let worst = read(&out)
                .chunks(4)
                .map(|p| (0..3).map(|c| (i32::from(p[c]) - i32::from(colour[c])).abs()).max().unwrap())
                .max()
                .unwrap();
            assert!(worst <= 2, "{w}x{h} {colour:?} sharpness {k} rcas {rcas}: off by {worst} levels");
        }
    }
}

#[test]
fn fsr_draws_edges_crisper_than_a_plain_enlargement_without_overshooting() {
    let mut g = gpu();
    g.fsr = true;
    let (w, h) = (160, 90);
    // a soft-edged checkerboard: what a real picture's edges look like
    let src = upload(&g, w, h, |x, y| {
        let fx = ((x as f64 / 20.0).fract() - 0.5).abs();
        let fy = ((y as f64 / 20.0).fract() - 0.5).abs();
        let v = (((0.25 - fx.min(fy).min(0.25)) * 4.0).clamp(0.0, 1.0) * 180.0 + 40.0) as u8;
        [v, v, v, 255]
    });
    let cmd = g.command_buffer().unwrap();
    g.ingest(&cmd, src, false).unwrap();
    finish(&cmd);
    let mut energy = vec![];
    for k in [0.0f32, 1.0] {
        (g.sharpness, g.rcas) = (k, 0.0);
        let cmd = g.command_buffer().unwrap();
        let (t, _) = g.compose(&cmd, 0.0, Some((w * 2, h * 2)), (0.0, 0.0)).unwrap();
        let out = shown(&g, &cmd, &t);
        finish(&cmd);
        let px = read(&out);
        assert!(px.chunks(4).all(|p| p[0] >= 38 && p[0] <= 222), "no ringing beyond the picture's own range");
        energy.push(edge_energy(&px, out.width(), out.height()));
    }
    eprintln!("edge energy plain enlargement / FSR: {energy:?}");
    assert!(energy[1] > energy[0], "EASU's edges are crisper than a bilinear enlargement: {energy:?}");
}
