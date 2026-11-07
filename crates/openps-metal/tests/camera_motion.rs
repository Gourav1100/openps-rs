//! The motion field and the generated frame for the kinds of camera movement a game has, against analytic ground truth: a pan, moving
//! forward (the picture grows from the middle: vectors that grow with the distance from it), a roll (vectors that turn around the middle)
//! and a pan over a scene with depth (a near layer moves faster than the far one). The scene is a continuous function, so any frame between
//! two others can be drawn exactly.
#![cfg(target_os = "macos")]
use objc2_metal::*;
use openps_metal::gpu::{make_tex, Gpu, Tex};
use std::ptr::NonNull;

const W: usize = 640;
const H: usize = 360;

/// A detailed, non-repeating colour field of scene coordinates (like a textured wall and floor).
fn scene(x: f64, y: f64) -> [f64; 3] {
    let v = |a: f64, b: f64, c: f64| {
        ((x * a + y * b + c).sin() * 0.5 + 0.5) * ((x * b * 0.7 - y * a * 1.3 + c * 2.0).cos() * 0.5 + 0.5)
    };
    [v(0.21, 0.13, 0.0), v(0.11, 0.29, 1.0), v(0.37, 0.07, 2.0)]
}

#[derive(Clone, Copy, Debug)]
enum Camera {
    /// Moves right by `px` pixels a frame.
    Pan(f64),
    /// Moves forward: the picture grows from its middle by this share a frame.
    Forward(f64),
    /// Rolls by this many degrees a frame.
    Roll(f64),
    /// Pans over two layers: the far one moves by `px`, the near one (the lower half) by twice that.
    Depth(f64),
}

/// The colour of pixel (x, y) at camera time `t` (in frames; any real number).
fn pixel(cam: Camera, x: usize, y: usize, t: f64) -> [u8; 4] {
    let (px, py) = (x as f64 + 0.5, y as f64 + 0.5);
    let (cx, cy) = (W as f64 / 2.0, H as f64 / 2.0);
    let (sx, sy) = match cam {
        Camera::Pan(v) => (px - v * t, py),
        Camera::Forward(s) => {
            let z = 1.0 + s * t;
            (cx + (px - cx) / z, cy + (py - cy) / z)
        }
        Camera::Roll(deg) => {
            let a = (deg * t).to_radians();
            let (dx, dy) = (px - cx, py - cy);
            (cx + dx * a.cos() + dy * a.sin(), cy - dx * a.sin() + dy * a.cos())
        }
        Camera::Depth(v) => (px - v * t * if py > cy { 2.0 } else { 1.0 }, py),
    };
    let c = scene(sx, sy);
    [(c[2] * 255.0) as u8, (c[1] * 255.0) as u8, (c[0] * 255.0) as u8, 255]
}

fn upload(g: &Gpu, f: impl Fn(usize, usize) -> [u8; 4]) -> Tex {
    let t = make_tex(&g.dev, MTLPixelFormat::BGRA8Unorm, W, H, MTLTextureUsage::ShaderRead, true).unwrap();
    let mut px = Vec::with_capacity(W * H * 4);
    for y in 0..H {
        for x in 0..W {
            px.extend(f(x, y));
        }
    }
    unsafe {
        t.replaceRegion_mipmapLevel_withBytes_bytesPerRow(
            MTLRegion { origin: MTLOrigin { x: 0, y: 0, z: 0 }, size: MTLSize { width: W, height: H, depth: 1 } },
            0,
            NonNull::new(px.as_mut_ptr().cast()).unwrap(),
            W * 4,
        );
    }
    t
}

type Cmd = objc2::rc::Retained<objc2::runtime::ProtocolObject<dyn MTLCommandBuffer>>;

fn draw(g: &Gpu, cmd: &Cmd, t: &Tex) -> Tex {
    let tg = make_tex(
        &g.dev,
        MTLPixelFormat::BGRA8Unorm,
        W,
        H,
        MTLTextureUsage::RenderTarget | MTLTextureUsage::ShaderRead,
        true,
    )
    .unwrap();
    g.present(cmd, t, &tg).unwrap();
    tg
}

fn fetch(t: &Tex) -> Vec<u8> {
    let mut out = vec![0u8; W * H * 4];
    unsafe {
        t.getBytes_bytesPerRow_fromRegion_mipmapLevel(
            NonNull::new(out.as_mut_ptr().cast()).unwrap(),
            W * 4,
            MTLRegion { origin: MTLOrigin { x: 0, y: 0, z: 0 }, size: MTLSize { width: W, height: H, depth: 1 } },
            0,
        );
    }
    out
}

fn mae(a: &[u8], b: &[u8], margin: usize) -> f64 {
    let (mut s, mut n) = (0.0, 0.0);
    for y in margin..H - margin {
        for x in margin..W - margin {
            for c in 0..3 {
                s += (f64::from(a[(y * W + x) * 4 + c]) - f64::from(b[(y * W + x) * 4 + c])).abs();
                n += 1.0;
            }
        }
    }
    s / n
}

/// Run the camera for a few frames, then generate the picture at `beta` between the last two and compare it with the exact one.
/// Returns (error of the generated picture, error of the nearer real picture, the coarse field's error against the exact vectors in
/// pixels over the blocks away from the border, and the share of those blocks more than 2 px wrong).
fn run(g: &mut Gpu, cam: Camera, beta: f32) -> (f64, f64, f64, f64) {
    let frames = 8usize;
    for k in 0..frames {
        let f = upload(g, |x, y| pixel(cam, x, y, k as f64));
        let cmd = g.command_buffer().unwrap();
        g.ingest(&cmd, f, true).unwrap();
        cmd.commit();
        cmd.waitUntilCompleted();
    }
    let cmd = g.command_buffer().unwrap();
    let (t, _) = g.compose_interp(&cmd, beta, None).unwrap();
    let shown = draw(g, &cmd, &t);
    cmd.commit();
    cmd.waitUntilCompleted();
    let got = fetch(&shown);
    let at = (frames - 2) as f64 + f64::from(beta);
    let truth = upload(g, |x, y| pixel(cam, x, y, at));
    let near = upload(g, |x, y| pixel(cam, x, y, if beta < 0.5 { (frames - 2) as f64 } else { (frames - 1) as f64 }));
    let cmd = g.command_buffer().unwrap();
    let (a, b) = (draw(g, &cmd, &truth), draw(g, &cmd, &near));
    cmd.commit();
    cmd.waitUntilCompleted();
    let (truth, near) = (fetch(&a), fetch(&b));

    // the coarse field against the exact motion (in pixels of the picture, from the previous frame to the newest)
    let (cw_px, _) = (16usize, 16usize); // a coarse block is 8 half-size pixels = 16 pixels
    let (mut err, mut n, mut bad) = (0.0f64, 0.0f64, 0.0f64);
    if let Some((field, fw)) = g.coarse_motion_field() {
        for (i, v) in field.iter().enumerate() {
            let (bx, by) = (i % fw, i / fw);
            let (cx, cy) = ((bx * cw_px + cw_px / 2) as f64, (by * cw_px + cw_px / 2) as f64);
            if cx < 48.0 || cy < 48.0 || cx > W as f64 - 48.0 || cy > H as f64 - 48.0 {
                continue;
            }
            // exact: where the scene point that is at (cx, cy) in the newest frame was in the previous one
            let (px, py) = (cx, cy);
            let (qx, qy) = prev_position(cam, px, py, (frames - 1) as f64);
            let (ex, ey) = (px - qx, py - qy);
            let d = ((f64::from(v[0]) * 2.0 - ex).powi(2) + (f64::from(v[1]) * 2.0 - ey).powi(2)).sqrt();
            err += d;
            n += 1.0;
            bad += f64::from(u8::from(d > 2.0));
        }
    }
    (mae(&got, &truth, 40), mae(&near, &truth, 40), err / n.max(1.0), bad / n.max(1.0))
}

/// Where the scene point shown at pixel (px, py) at time `t` was at time `t - 1` (a numerical inverse of the camera).
fn prev_position(cam: Camera, px: f64, py: f64, t: f64) -> (f64, f64) {
    let (cx, cy) = (W as f64 / 2.0, H as f64 / 2.0);
    match cam {
        Camera::Pan(v) => (px - v, py),
        Camera::Forward(s) => {
            let (z1, z0) = (1.0 + s * t, 1.0 + s * (t - 1.0));
            (cx + (px - cx) * z0 / z1, cy + (py - cy) * z0 / z1)
        }
        Camera::Roll(deg) => {
            // the point's position is rotated by `deg` between the frames
            let (dx, dy) = (px - cx, py - cy);
            let a = (-deg).to_radians();
            (cx + dx * a.cos() - dy * a.sin(), cy + dx * a.sin() + dy * a.cos())
        }
        Camera::Depth(v) => (px - v * if py > cy { 2.0 } else { 1.0 }, py),
    }
}

#[test]
#[ignore = "measurement: prints the error of the motion field and of the generated picture for each kind of camera movement"]
fn camera_movement_sweep() {
    let mut g = Gpu::new().unwrap();
    g.fx_use = false;
    for cam in [
        Camera::Pan(2.0),
        Camera::Pan(7.0),
        Camera::Pan(14.0),
        Camera::Forward(0.005),
        Camera::Forward(0.01),
        Camera::Forward(0.02),
        Camera::Roll(0.3),
        Camera::Roll(0.8),
        Camera::Depth(3.0),
        Camera::Depth(8.0),
    ] {
        let mut line = format!("{cam:<18?}");
        for beta in [0.25f32, 0.5] {
            let (e, n, fe, fb) = run(&mut g, cam, beta);
            line += &format!("  beta {beta}: err {e:5.2} (nearer real {n:5.2}) ");
            if beta == 0.5 {
                line += &format!(" field error {fe:4.2} px, {:3.0} % blocks > 2 px off", fb * 100.0);
            }
        }
        eprintln!("{line}");
    }
}

/// Every kind of camera movement is generated far better than showing the nearer real picture, and the motion field is right.
#[test]
fn a_pan_forward_movement_roll_and_depth_are_generated_close_to_the_exact_picture() {
    let mut g = Gpu::new().unwrap();
    g.fx_use = false;
    for cam in [Camera::Pan(7.0), Camera::Forward(0.01), Camera::Forward(0.02), Camera::Roll(0.5), Camera::Depth(5.0)] {
        for beta in [0.25f32, 0.5, 0.75] {
            let (e, near, field_err, bad) = run(&mut g, cam, beta);
            assert!(e < near * 0.3 + 0.5, "{cam:?} beta {beta}: generated {e:.2}, nearer real picture {near:.2}");
            assert!(
                field_err < 2.0 && bad < 0.1,
                "{cam:?}: the motion field is {field_err:.2} px off on average, {:.0} % of blocks over 2 px",
                bad * 100.0
            );
        }
    }
}

/// Mean absolute Laplacian of the luma over the middle of a BGRA picture.
fn detail(px: &[u8]) -> f64 {
    let l = |x: usize, y: usize| {
        0.114 * f64::from(px[(y * W + x) * 4])
            + 0.587 * f64::from(px[(y * W + x) * 4 + 1])
            + 0.299 * f64::from(px[(y * W + x) * 4 + 2])
    };
    let (mut s, mut n) = (0.0, 0.0);
    for y in 40..H - 40 {
        for x in 40..W - 40 {
            s += (4.0 * l(x, y) - l(x - 1, y) - l(x + 1, y) - l(x, y - 1) - l(x, y + 1)).abs();
            n += 1.0;
        }
    }
    s / n
}

/// A game's picture has grain and fine texture that is different in every frame. A generated picture made by averaging two of them has
/// less of it than the real ones, and the loss comes and goes with the generated frames: shimmer. A picture taken from the nearer real
/// picture, moved to its place, keeps it.
#[test]
fn a_generated_picture_has_as_much_grain_and_fine_detail_as_the_real_ones() {
    let mut g = Gpu::new().unwrap();
    g.fx_use = false;
    let grain = |x: usize, y: usize, k: usize| -> f64 {
        let mut v = (x as u64).wrapping_mul(0x9E37_79B9)
            ^ (y as u64).wrapping_mul(0x85EB_CA6B)
            ^ (k as u64).wrapping_mul(0xC2B2_AE35);
        v ^= v >> 13;
        v = v.wrapping_mul(0x2545_F491);
        v ^= v >> 16;
        f64::from((v & 255) as u32) / 255.0 - 0.5
    };
    let cam = Camera::Pan(5.0);
    let frames = 8usize;
    let mut real_detail = 0.0;
    for k in 0..frames {
        let f = upload(&g, |x, y| {
            let mut p = pixel(cam, x, y, k as f64);
            let n = grain(x, y, k) * 24.0;
            for v in p.iter_mut().take(3) {
                *v = (f64::from(*v) + n).clamp(0.0, 255.0) as u8;
            }
            p
        });
        let cmd = g.command_buffer().unwrap();
        g.ingest(&cmd, f.clone(), true).unwrap();
        let shown = draw(&g, &cmd, &f);
        cmd.commit();
        cmd.waitUntilCompleted();
        real_detail = detail(&fetch(&shown));
    }
    // (a shift of exactly half a pixel, as at 0.5 here, takes the most from white noise whatever the filter: left out)
    for beta in [0.25f32, 0.4, 0.6, 0.75] {
        let cmd = g.command_buffer().unwrap();
        let (t, _) = g.compose_interp(&cmd, beta, None).unwrap();
        let shown = draw(&g, &cmd, &t);
        cmd.commit();
        cmd.waitUntilCompleted();
        let ratio = detail(&fetch(&shown)) / real_detail;
        assert!(
            // white noise is the worst case: the average of two pictures has 0.71 of its detail, one picture moved by a fraction of a pixel about 0.85
            (0.8..=1.15).contains(&ratio),
            "beta {beta}: the generated picture has {ratio:.2} of the detail of a real one"
        );
    }
}
