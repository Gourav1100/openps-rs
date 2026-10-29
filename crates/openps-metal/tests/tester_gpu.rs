//! Tester A5: headless GPU tests with hostile parameters (NaN/inf, uninitialised motion state, size changes, resets).
#![cfg(target_os = "macos")]
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::*;
use openps_metal::gpu::{make_tex, upscale_target, Gpu, Tex};
use std::ptr::NonNull;
use std::time::Instant;

type Cmd = Retained<ProtocolObject<dyn MTLCommandBuffer>>;

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

fn finish(cmd: &Cmd) {
    cmd.commit();
    cmd.waitUntilCompleted();
    assert_eq!(cmd.status(), MTLCommandBufferStatus::Completed, "{:?}", cmd.error());
}

fn pattern(x: usize, y: usize, shift: f64) -> [u8; 4] {
    let (fx, fy) = (x as f64 - shift, y as f64);
    let v = |a: f64, b: f64| (((fx * a + fy * b).sin() * 0.5 + 0.5) * 255.0) as u8;
    [v(0.21, 0.13), v(0.11, 0.29), v(0.37, 0.07), 255]
}

/// Show `t` through `present` on a same-size target and return the pixels.
fn present_px(g: &Gpu, t: &Tex) -> Vec<u8> {
    let tg = target(g, t.width(), t.height());
    let cmd = g.command_buffer().unwrap();
    g.present(&cmd, t, &tg).unwrap();
    finish(&cmd);
    read(&tg)
}

#[test]
fn compose_before_any_frame_and_after_reset_is_an_error_not_a_crash() {
    let mut g = Gpu::new().unwrap();
    let cmd = g.command_buffer().unwrap();
    assert!(g.compose(&cmd, 0.5, None, (0.0, 0.0)).is_err());
    assert!(g.compose_interp(&cmd, 0.5, None).is_err());
    assert!(g.global_motion().is_none());
    assert!(g.source_size().is_none());
    let a = upload(&g, 64, 36, |x, y| pattern(x, y, 0.0));
    g.ingest(&cmd, a, true).unwrap();
    g.reset_stream();
    assert!(g.compose(&cmd, 0.5, None, (0.0, 0.0)).is_err());
    assert!(g.global_motion().is_none());
    finish(&cmd);
}

#[test]
fn stream_size_changes_mid_session_every_which_way() {
    let mut g = Gpu::new().unwrap();
    let sizes = [(640, 360), (321, 181), (1280, 720), (2, 2), (640, 360), (1, 1), (333, 7), (7, 333), (640, 360)];
    for (n, &(w, h)) in sizes.iter().enumerate() {
        for k in 0..3 {
            let f = upload(&g, w, h, |x, y| pattern(x, y, f64::from(k) * 2.0));
            let cmd = g.command_buffer().unwrap();
            g.ingest(&cmd, f, true).unwrap();
            let up = upscale_target((w, h), (w * 2, h * 2), 2160);
            let (t, _) = g.compose(&cmd, 0.5, up, (0.0, 0.0)).unwrap();
            let tg = target(&g, t.width(), t.height());
            g.present(&cmd, &t, &tg).unwrap();
            let (t2, _) = g.compose_interp(&cmd, 0.5, up).unwrap();
            let tg2 = target(&g, t2.width(), t2.height());
            g.present(&cmd, &t2, &tg2).unwrap();
            finish(&cmd);
            assert_eq!(g.source_size(), Some((w, h)), "step {n}");
            assert_eq!(
                g.global_motion().is_some(),
                k >= 1,
                "global motion needs two frames of the new size (step {n}, frame {k})"
            );
        }
    }
}

#[test]
fn hostile_floats_for_alpha_extra_and_beta_never_fail_the_command_buffer() {
    let mut g = Gpu::new().unwrap();
    let cmd = g.command_buffer().unwrap();
    for k in 0..3 {
        let f = upload(&g, 160, 90, |x, y| pattern(x, y, f64::from(k) * 3.0));
        g.ingest(&cmd, f, true).unwrap();
    }
    finish(&cmd);
    let nan = f32::NAN;
    let inf = f32::INFINITY;
    for alpha in [nan, inf, -inf, -1.0, 1e30, 0.5] {
        for extra in [(0.0, 0.0), (nan, nan), (inf, -inf), (1e30, -1e30), (-5.0, 3.0)] {
            let cmd = g.command_buffer().unwrap();
            let (t, _) = g.compose(&cmd, alpha, None, extra).unwrap();
            let tg = target(&g, t.width(), t.height());
            g.present(&cmd, &t, &tg).unwrap();
            finish(&cmd);
        }
    }
    for beta in [nan, inf, -inf, -1.0, 2.0, 0.5] {
        let cmd = g.command_buffer().unwrap();
        let (t, _) = g.compose_interp(&cmd, beta, None).unwrap();
        let tg = target(&g, t.width(), t.height());
        g.present(&cmd, &t, &tg).unwrap();
        finish(&cmd);
    }
}

/// Saturation and sharpness come from `u8 / 100`, so NaN cannot arrive from the app; but `Gpu` is a pub struct and `f32::clamp`
/// keeps NaN. A NaN saturation reaches the shader unchanged. Characterisation: report what happens to a mid-grey picture.
#[test]
fn nan_saturation_and_sharpness_are_survivable() {
    let mut g = Gpu::new().unwrap();
    let a = upload(&g, 64, 36, |_, _| [128, 128, 128, 255]);
    let ok = present_px(&g, &a);
    assert!(ok.chunks(4).all(|p| (126..=130).contains(&p[0])));
    for (sat, sharp) in [(f32::NAN, 0.0), (0.0, f32::NAN), (f32::INFINITY, 0.0), (1.0, f32::INFINITY), (-5.0, -5.0)] {
        g.saturation = sat;
        g.sharpen = sharp;
        let px = present_px(&g, &a);
        let mid = px[(18 * 64 + 32) * 4];
        eprintln!("saturation {sat}, sharpen {sharp}: centre pixel {mid}");
    }
}

#[test]
fn nan_saturation_leaves_the_picture_untouched() {
    let mut g = Gpu::new().unwrap();
    let a = upload(&g, 64, 36, |_, _| [128, 128, 128, 255]);
    g.saturation = f32::NAN;
    let px = present_px(&g, &a);
    assert!(px.chunks(4).all(|p| (126..=130).contains(&p[0])), "centre pixel {}", px[(18 * 64 + 32) * 4]);
}

/// Motion data is only computed when `motion` is true. A stream that started with generation off (display rate == source rate) and is
/// then switched to a higher display rate calls `compose` with alpha > 0 before the first motion field was ever computed: the motion
/// textures are freshly allocated, never written. The result must still be the untouched frame (or at least a sane picture).
#[test]
fn generation_switched_on_before_any_motion_was_computed_shows_a_sane_picture() {
    let mut g = Gpu::new().unwrap();
    let cmd = g.command_buffer().unwrap();
    let mut last = None;
    for k in 0..4 {
        let f = upload(&g, 128, 72, |x, y| pattern(x, y, f64::from(k) * 4.0));
        last = Some(f.clone());
        g.ingest(&cmd, f, false).unwrap(); // motion off, as at display rate == source rate
    }
    finish(&cmd);
    let cmd = g.command_buffer().unwrap();
    let (t, _) = g.compose(&cmd, 0.8, None, (0.0, 0.0)).unwrap();
    let tg = target(&g, 128, 72);
    g.present(&cmd, &t, &tg).unwrap();
    finish(&cmd);
    let got = read(&tg);
    let want = present_px(&g, last.as_ref().unwrap());
    let err: f64 = got
        .chunks(4)
        .zip(want.chunks(4))
        .map(|(a, b)| (0..3).map(|c| (f64::from(a[c]) - f64::from(b[c])).abs()).sum::<f64>() / 3.0)
        .sum::<f64>()
        / (128.0 * 72.0);
    eprintln!("mean error of the first generated frame vs the real frame: {err:.2} / 255");
    assert!(err < 6.0, "garbage from uninitialised motion textures: {err}");
}

#[test]
fn repeated_gpu_creation_is_fast_enough_and_does_not_balloon() {
    let t0 = Instant::now();
    let rss0 = rss_mb();
    for _ in 0..12 {
        let g = Gpu::new().unwrap();
        drop(g);
    }
    let per = t0.elapsed().as_secs_f64() * 1e3 / 12.0;
    let grew = rss_mb() - rss0;
    eprintln!("Gpu::new + drop: {per:.0} ms each, RSS growth over 12: {grew:.1} MB");
    assert!(grew < 150.0, "{grew} MB");
}

#[test]
fn many_frames_at_1080p_do_not_grow_memory_with_a_pool_per_frame() {
    let mut g = Gpu::new().unwrap();
    let frames: Vec<Tex> = (0..3).map(|i| upload(&g, 1920, 1080, |x, y| pattern(x, y, f64::from(i) * 5.0))).collect();
    let tg = target(&g, 1920, 1080);
    let run = |g: &mut Gpu, n: usize| {
        for i in 0..n {
            objc2::rc::autoreleasepool(|_| {
                let cmd = g.command_buffer().unwrap();
                g.ingest(&cmd, frames[i % 3].clone(), true).unwrap();
                let (t, _) = g.compose(&cmd, 0.5, None, (0.0, 0.0)).unwrap();
                g.present(&cmd, &t, &tg).unwrap();
                cmd.commit();
                cmd.waitUntilCompleted();
            });
        }
    };
    run(&mut g, 30);
    let r0 = rss_mb();
    run(&mut g, 400);
    let grew = rss_mb() - r0;
    eprintln!("RSS growth over 400 frames at 1080p: {grew:.1} MB");
    assert!(grew < 60.0, "{grew} MB");
}

fn rss_mb() -> f64 {
    let out = std::process::Command::new("/bin/ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().parse::<f64>().unwrap_or(0.0) / 1024.0
}

#[test]
fn upscale_target_never_returns_odd_zero_or_overflowing_sizes() {
    let sizes = [0usize, 1, 2, 3, 321, 1920, 3840, 16384, 100_000, usize::MAX / 2, usize::MAX];
    for &sw in &sizes {
        for &sh in &sizes {
            for &dw in &sizes {
                for &dh in &sizes {
                    for mode in [0usize, 1440, 2160, usize::MAX] {
                        if let Some((w, h)) = upscale_target((sw, sh), (dw, dh), mode) {
                            assert!(
                                w % 2 == 0 && h % 2 == 0 && w >= sw && h >= sh && w > 0 && h > 0,
                                "{sw}x{sh} -> {dw}x{dh} @{mode}: {w}x{h}"
                            );
                        }
                    }
                }
            }
        }
    }
}

/// `upscale_target` promises `None` "when that is not larger than the source", but the final `& !1` can round the result back down to
/// (or equal to) the source size for odd sources. `Gpu::upscale` re-checks `ow > w && oh > h`, so nothing breaks (hardening only).
#[test]
fn upscale_target_is_strictly_larger_when_it_returns_some() {
    for (src, dst) in [((1usize, 2usize), (2usize, 3usize)), ((321, 181), (642, 362)), ((641, 361), (1283, 723))] {
        if let Some((w, h)) = upscale_target(src, dst, 0) {
            assert!(w > src.0 && h > src.1, "{src:?} -> {dst:?}: {w}x{h}");
        }
    }
}
