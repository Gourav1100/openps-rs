//! Repeating vertical lines (a radiator, railings, a fence) in a camera pan: the generated frame must put the lines where they really are.
//! Block matching cannot tell one line from the next, so these scenes shimmer when the match jumps between lines from block to block or
//! from frame to frame. The scene is an analytic function of position, so a pan by a fraction of a pixel has an exact right answer.
#![cfg(target_os = "macos")]
use objc2_metal::*;
use openps_metal::gpu::{make_tex, Gpu, Tex};
use std::ptr::NonNull;

const W: usize = 640;
const H: usize = 360;

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

/// Draw `t` into an offscreen target in `cmd`; read it back with `fetch` once `cmd` has completed.
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

fn fetch(tg: &Tex) -> Vec<u8> {
    let mut out = vec![0u8; W * H * 4];
    unsafe {
        tg.getBytes_bytesPerRow_fromRegion_mipmapLevel(
            NonNull::new(out.as_mut_ptr().cast()).unwrap(),
            W * 4,
            MTLRegion { origin: MTLOrigin { x: 0, y: 0, z: 0 }, size: MTLSize { width: W, height: H, depth: 1 } },
            0,
        );
    }
    out
}

/// Rounded bright fins with a darker gap between them, a faint vertical streak texture on each (it moves with the fin), a wall behind
/// with a soft horizontal gradient and a baseboard (`wall` false: a plain one). `shift` pans everything right by that many pixels (any real number).
fn radiator(x: usize, y: usize, shift: f64, period: f64, wall: bool) -> [u8; 4] {
    let fx = x as f64 + 0.5 - shift;
    let ph = (fx / period).rem_euclid(1.0);
    let fin = {
        let d = (ph - 0.5).abs() * 2.0; // 0 centre, 1 gap middle
        let w = ((0.72 - d) / 0.3).clamp(0.0, 1.0);
        w * w * (3.0 - 2.0 * w)
    };
    let streak = 0.06 * (fx * 0.9).sin() + 0.04 * (fx * 2.3 + 1.0).sin();
    let rows = 0.02 * ((y as f64) * 0.8).sin(); // fine horizontal lines
    let v = 0.25 + 0.6 * fin + streak * fin + rows;
    // the wall above and the floor below are not repeating: something else in the picture says how far the camera moved
    let yf = y as f64;
    let texture = if wall {
        0.5 + 0.18 * (fx * 0.031 + yf * 0.017).sin()
            + 0.14 * (fx * 0.053 - yf * 0.041 + 1.0).sin()
            + 0.08 * (fx * 0.117 + yf * 0.093 + 2.0).sin()
    } else {
        0.5 // a plain wall: nothing but the fins to go by
    };
    let l = if yf < 130.0 {
        texture * 0.8
    } else if yf > 640.0 {
        texture * 0.5
    } else {
        v
    };
    let l = l.clamp(0.0, 1.0);
    let c = (l * 255.0) as u8;
    [c, (c as f64 * 0.98) as u8, (c as f64 * 0.93) as u8, 255]
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

/// The picture at `beta` between the last two of a pan of `speed` px a frame, and what it should be.
fn run(g: &mut Gpu, speed: f64, period: f64, beta: f32) -> (f64, f64) {
    run_with_prior(g, speed, period, beta, None, true, true)
}

/// `run`, with the motion the controller says the camera makes between two frames (a multiple of the true speed: 1.0 is exact) given to the
/// motion search before each new frame.
/// With `ramp` the camera speeds up over the first frames, as a thumbstick does; without, it is at full speed from the first frame (a flick,
/// where nothing has been seen to track).
fn run_with_prior(
    g: &mut Gpu,
    speed: f64,
    period: f64,
    beta: f32,
    prior: Option<(f64, f32)>,
    ramp: bool,
    wall: bool,
) -> (f64, f64) {
    // the camera speeds up to `speed` over the first frames, as a thumbstick does
    g.reset_stream();
    let frames = 7usize;
    let pos = |k: usize| -> f64 {
        if ramp {
            (0..k).map(|j| speed * (j + 1) as f64 / (frames - 1) as f64).sum()
        } else {
            speed * k as f64
        }
    };
    for k in 0..frames {
        let f = upload(g, |x, y| radiator(x, y, pos(k), period, wall));
        if let (Some((scale, trust)), true) = (prior, k > 0) {
            // the motion between frame k - 1 and k, as the stick would have told it
            let v = pos(k) - pos(k - 1);
            g.set_input_prior(Some((((v * scale) as f32, 0.0), trust)));
        }
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
    let at = pos(frames - 2) + speed * f64::from(beta);
    let truth = upload(g, |x, y| radiator(x, y, at, period, wall));
    let near_shift = if beta < 0.5 { pos(frames - 2) } else { pos(frames - 1) };
    let nearest = upload(g, |x, y| radiator(x, y, near_shift, period, wall));
    let cmd = g.command_buffer().unwrap();
    let (a, b) = (draw(g, &cmd, &truth), draw(g, &cmd, &nearest));
    cmd.commit();
    cmd.waitUntilCompleted();
    let (truth, nearest) = (fetch(&a), fetch(&b));
    (mae(&got, &truth, 40), mae(&nearest, &truth, 40))
}

#[test]
#[ignore = "measurement: prints the error of the generated frame for repeating lines at many speeds"]
fn repeating_vertical_lines_in_a_pan_sweep() {
    let mut g = Gpu::new().unwrap();
    g.fx_use = false;
    for period in [14.0, 22.0, 31.0, 47.0] {
        for speed in [0.6, 1.5, 3.0, 5.0, 8.0, 12.0, 18.0, 26.0] {
            let mut line = format!("period {period:>4} speed {speed:>5}:");
            for beta in [0.25f32, 0.5, 0.75] {
                let (e, base) = run(&mut g, speed, period, beta);
                line += &format!("  beta {beta}: err {e:5.2} (nearest real {base:5.2})");
            }
            eprintln!("{line}");
        }
    }
}

/// The generated frame beats showing the nearer real frame by a wide margin where the lines are told apart (the pan moves them less than
/// half their spacing a frame, or the rest of the picture says how far the camera went), at every position between two frames. Before
/// the search preferred the shorter vector and the last frame's, it matched a line to the next one and these were worse than the real frame.
#[test]
fn a_pan_over_repeating_lines_is_generated_at_the_right_place() {
    let mut g = Gpu::new().unwrap();
    g.fx_use = false;
    let cases: &[(f64, &[f64])] =
        &[(14.0, &[1.5, 8.0]), (22.0, &[3.0, 12.0, 18.0]), (31.0, &[5.0, 18.0, 26.0]), (47.0, &[3.0, 26.0])];
    for (period, speeds) in cases {
        for &speed in *speeds {
            for beta in [0.25f32, 0.5, 0.75] {
                let (e, base) = run(&mut g, speed, *period, beta);
                assert!(
                    e < base * 0.6 + 0.2,
                    "period {period} speed {speed} beta {beta}: generated {e:.2}, nearer real frame {base:.2}"
                );
            }
        }
    }
}

/// What the controller knows and the picture does not: a flick over a plain wall and a radiator, with nothing but the fins to go by, which
/// look almost the same one fin over (a 19 px flick over fins 22 px apart looks like a 3 px step back). With the stick's motion handed to the
/// search the right one is found, and a prior that is a little off (the game's response is learned, not exact) still finds it.
#[test]
fn the_sticks_motion_finds_a_pan_the_picture_alone_cannot() {
    let mut g = Gpu::new().unwrap();
    g.fx_use = false;
    for (period, speed) in [(22.0, 19.0), (14.0, 11.0)] {
        let (alone, near) = run_with_prior(&mut g, speed, period, 0.5, None, false, false);
        assert!(alone > 30.0, "the picture alone should be fooled here: {alone:.2}");
        for scale in [1.0, 0.95, 1.05] {
            let (e, _) = run_with_prior(&mut g, speed, period, 0.5, Some((scale, 0.9)), false, false);
            assert!(e < 5.0, "fins {period} px apart, {speed} px a frame, prior x{scale}: with {e:.2}, without {alone:.2}, nearer real {near:.2}");
        }
    }
}

#[test]
fn a_wrong_prior_does_not_spoil_a_pan_the_picture_can_tell() {
    let mut g = Gpu::new().unwrap();
    g.fx_use = false;
    // a clean pan over spacing 47 (found without any help), with a prior that says the camera is still, or going the other way
    for (speed, scale) in [(8.0, 0.0), (8.0, -1.0), (12.0, 0.2), (5.0, -2.0)] {
        let (alone, near) = run_with_prior(&mut g, speed, 47.0, 0.5, None, false, false);
        let (e, _) = run_with_prior(&mut g, speed, 47.0, 0.5, Some((scale, 0.6)), false, false);
        assert!(
            e < near * 0.3 + 1.0,
            "speed {speed}, prior x{scale}: {e:.2} (alone {alone:.2}, nearer real {near:.2})"
        );
    }
}

#[test]
#[ignore = "measurement: flicks at speeds near a multiple of the spacing, with and without the stick"]
fn flick_sweep() {
    let mut g = Gpu::new().unwrap();
    g.fx_use = false;
    for period in [14.0, 22.0, 31.0] {
        for k in [1.0, 2.0] {
            for off in [-3.0, -1.0, 0.0, 1.0, 3.0] {
                let speed = period * k + off;
                let (alone, near) = run_with_prior(&mut g, speed, period, 0.5, None, false, false);
                let (with, _) = run_with_prior(&mut g, speed, period, 0.5, Some((1.0, 0.9)), false, false);
                eprintln!("period {period} speed {speed}: alone {alone:.2} with {with:.2} nearer {near:.2}");
            }
        }
    }
}
