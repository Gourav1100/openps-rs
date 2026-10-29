//! Objective quality of generated frames on REAL encoded video. A 60 fps clip is fed to the renderer as if it were a 30 fps
//! stream (every second frame); the frames it generates for the missing ones are compared with the frames that were really
//! there. This is the honest measure of "artifacts": error against the truth, for each strategy.
#![cfg(target_os = "macos")]
use objc2_metal::*;
use openps_metal::gpu::{make_tex, Gpu, Tex};
use openps_video::nal::{self, Codec, ParameterSets};
use openps_video::vt::{Decoder, Frame, OutputFormat};
use std::ptr::NonNull;

fn decode(name: &str) -> Vec<Frame> {
    let s = std::fs::read(format!("{}/tests/data/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap();
    let mut d =
        Decoder::new_with(Codec::H264, ParameterSets::from_annex_b(Codec::H264, &s), OutputFormat::Bgra).unwrap();
    nal::access_units(Codec::H264, &s)
        .iter()
        .enumerate()
        .flat_map(|(i, u)| d.decode_annex_b(u, i as i64 * 1500).unwrap())
        .collect()
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

fn to_shared(g: &Gpu, t: &Tex) -> Vec<u8> {
    let tg = make_tex(
        &g.dev,
        MTLPixelFormat::BGRA8Unorm,
        t.width(),
        t.height(),
        MTLTextureUsage::RenderTarget | MTLTextureUsage::ShaderRead,
        true,
    )
    .unwrap();
    let cmd = g.command_buffer().unwrap();
    g.present(&cmd, t, &tg).unwrap();
    cmd.commit();
    cmd.waitUntilCompleted();
    read(&tg)
}

#[derive(Default, Clone, Copy)]
struct Score {
    err: f64,
    bad: f64, // fraction of pixels off by more than 24 levels: visible artifacts
    n: f64,
}
impl Score {
    fn add(&mut self, got: &[u8], truth: &[u8]) {
        let (mut e, mut bad) = (0.0, 0.0);
        for (g, t) in got.as_chunks::<4>().0.iter().zip(truth.as_chunks::<4>().0.iter()) {
            let d = (0..3).map(|c| (f64::from(g[c]) - f64::from(t[c])).abs()).sum::<f64>() / 3.0;
            e += d;
            if d > 24.0 {
                bad += 1.0;
            }
        }
        let px = (got.len() / 4) as f64;
        self.err += e / px;
        self.bad += bad / px;
        self.n += 1.0;
    }
    fn mean(&self) -> (f64, f64) {
        (self.err / self.n, 100.0 * self.bad / self.n)
    }
}

struct Result {
    repeat: Score,
    v1: Score,
    v2: Score,
    smooth: Score,
}

fn evaluate(name: &str) -> Result {
    let frames = decode(name);
    assert!(frames.len() >= 40, "{} frames", frames.len());
    let mut res =
        Result { repeat: Score::default(), v1: Score::default(), v2: Score::default(), smooth: Score::default() };
    let mut g = Gpu::new().unwrap();
    let mut j = 0;
    while j + 1 < frames.len() {
        let tex = openps_metal::renderer::frame_to_texture(&g, &frames[j]).unwrap();
        let cmd = g.command_buffer().unwrap();
        g.ingest(&cmd, tex, true).unwrap();
        cmd.commit();
        cmd.waitUntilCompleted();
        if j >= 4 {
            // smooth: halfway between the previous two source frames is the real frame j-1
            let cmd = g.command_buffer().unwrap();
            let (t, _) = g.compose_interp(&cmd, 0.5, None).unwrap();
            cmd.commit();
            cmd.waitUntilCompleted();
            res.smooth.add(&to_shared(&g, &t), &frames[j - 1].copy_planes());
            // extrapolation: half an interval ahead of frame j is the real frame j+1
            let truth = frames[j + 1].copy_planes();
            res.repeat.add(&frames[j].copy_planes(), &truth);
            for legacy in [true, false] {
                g.legacy_warp = legacy;
                let cmd = g.command_buffer().unwrap();
                let (t, _) = g.compose(&cmd, 0.5, None, (0.0, 0.0)).unwrap();
                cmd.commit();
                cmd.waitUntilCompleted();
                let px = to_shared(&g, &t);
                if legacy {
                    res.v1.add(&px, &truth)
                } else {
                    res.v2.add(&px, &truth)
                }
            }
        }
        j += 2;
    }
    res
}

#[test]
fn generated_frames_against_the_truth_on_real_encoded_video() {
    println!(
        "{:<12} {:>18} {:>18} {:>18} {:>18}",
        "clip", "repeat last", "extrapolate v1", "extrapolate v2", "smooth (interp)"
    );
    let mut rows = vec![];
    for name in ["testsrc60.annexb", "mandelbrot60.annexb", "gradients60.annexb"] {
        let r = evaluate(name);
        let f = |s: &Score| {
            let (e, b) = s.mean();
            format!("{e:6.2} / {b:5.2}%")
        };
        println!(
            "{:<12} {:>18} {:>18} {:>18} {:>18}",
            name.trim_end_matches("60.annexb"),
            f(&r.repeat),
            f(&r.v1),
            f(&r.v2),
            f(&r.smooth)
        );
        rows.push((name, r));
    }
    println!("(mean abs error in levels of 255 / share of pixels more than 24 levels wrong)");
    for (name, r) in &rows {
        let (rep, v1, v2, sm) = (r.repeat.mean(), r.v1.mean(), r.v2.mean(), r.smooth.mean());
        assert!(
            v2.0 < rep.0 * 0.8,
            "{name}: extrapolation must clearly beat repeating the last frame: {:.2} vs {:.2}",
            v2.0,
            rep.0
        );
        assert!(v2.0 <= v1.0 + 0.15, "{name}: v2 must not be worse than v1: {:.2} vs {:.2}", v2.0, v1.0);
        assert!(
            v2.1 <= rep.1 + 0.05,
            "{name}: v2 must not create more visibly wrong pixels than repeating: {:.2}% vs {:.2}%",
            v2.1,
            rep.1
        );
        assert!(sm.0 < rep.0, "{name}: interpolation must beat repeating");
        // the bound: extrapolation cannot beat interpolation (which sees the future frame) by much, and should come close
        assert!(
            v2.0 < sm.0 * 1.6 + 0.2,
            "{name}: extrapolation {:.2} is too far from the interpolation ceiling {:.2}",
            v2.0,
            sm.0
        );
    }
}

#[test]
#[ignore = "diagnostic: where does v2's error live?"]
fn where_is_the_error() {
    let frames = decode("gradients60.annexb");
    let mut g = Gpu::new().unwrap();
    let (w, h) = (frames[0].width(), frames[0].height());
    let mut grid = vec![0.0f64; 8 * 8];
    let mut count = 0.0;
    let mut j = 0;
    while j + 1 < frames.len() {
        let tex = openps_metal::renderer::frame_to_texture(&g, &frames[j]).unwrap();
        let cmd = g.command_buffer().unwrap();
        g.ingest(&cmd, tex, true).unwrap();
        cmd.commit();
        cmd.waitUntilCompleted();
        if j >= 4 {
            let truth = frames[j + 1].copy_planes();
            let cmd = g.command_buffer().unwrap();
            let (t, _) = g.compose(&cmd, 0.5, None, (0.0, 0.0)).unwrap();
            cmd.commit();
            cmd.waitUntilCompleted();
            let got = to_shared(&g, &t);
            for y in 0..h {
                for x in 0..w {
                    let i = (y * w + x) * 4;
                    let d = (0..3).map(|c| (f64::from(got[i + c]) - f64::from(truth[i + c])).abs()).sum::<f64>() / 3.0;
                    grid[(y * 8 / h) * 8 + x * 8 / w] += d;
                }
            }
            count += 1.0;
            if j == 20 {
                let gm = g.global_motion();
                eprintln!("global motion {gm:?}");
            }
        }
        j += 2;
    }
    for r in 0..8 {
        let row: Vec<String> =
            (0..8).map(|c| format!("{:5.1}", grid[r * 8 + c] / count / ((w / 8) * (h / 8)) as f64)).collect();
        eprintln!("{}", row.join(" "));
    }
}
