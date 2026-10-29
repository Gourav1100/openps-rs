//! Real-footage report (run by hand: OPENPS_REAL_DIR=<dir with bird1080.annexb and bird720.annexb> cargo test -p openps-metal --release --test real_clip -- --nocapture).
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
    let s = std::fs::read(format!("{}/{name}", std::env::var("OPENPS_REAL_DIR").unwrap())).unwrap();
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

/// Luma (0..255) of a BGRA picture.
fn luma(px: &[u8]) -> Vec<f32> {
    px.as_chunks::<4>()
        .0
        .iter()
        .map(|p| 0.114 * f32::from(p[0]) + 0.587 * f32::from(p[1]) + 0.299 * f32::from(p[2]))
        .collect()
}

/// Structural similarity and retained detail over only the 8x8 windows (stride 4) where the scene really changed between `older` and
/// `truth` (mean luma change above 6). Whole-picture averages are dominated by the static background and hide ghosting and blur.
/// Returns (mean SSIM, sum of gradient energy of `got`, sum of gradient energy of `truth`, windows).
fn moving_structure(got: &[u8], truth: &[u8], older: &[u8], w: usize, h: usize) -> (f64, f64, f64, f64) {
    let (lg, lt, lo) = (luma(got), luma(truth), luma(older));
    let (c1, c2) = ((0.01f32 * 255.0).powi(2), (0.03f32 * 255.0).powi(2));
    let (mut sum, mut eg, mut et, mut n) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
    let grad = |l: &[f32], x: usize, y: usize| -> f32 {
        let gx = l[y * w + x + 1] - l[y * w + x - 1];
        let gy = l[(y + 1) * w + x] - l[(y - 1) * w + x];
        (gx * gx + gy * gy).sqrt()
    };
    let mut y = 1;
    while y + 9 < h {
        let mut x = 1;
        while x + 9 < w {
            let mut change = 0.0f32;
            for j in 0..8 {
                for i in 0..8 {
                    change += (lo[(y + j) * w + x + i] - lt[(y + j) * w + x + i]).abs();
                }
            }
            if change / 64.0 > 6.0 {
                let (mut ma, mut mb) = (0.0f32, 0.0f32);
                for j in 0..8 {
                    for i in 0..8 {
                        ma += lg[(y + j) * w + x + i];
                        mb += lt[(y + j) * w + x + i];
                    }
                }
                let (ma, mb) = (ma / 64.0, mb / 64.0);
                let (mut va, mut vb, mut cov) = (0.0f32, 0.0f32, 0.0f32);
                for j in 0..8 {
                    for i in 0..8 {
                        let (da, db) = (lg[(y + j) * w + x + i] - ma, lt[(y + j) * w + x + i] - mb);
                        va += da * da;
                        vb += db * db;
                        cov += da * db;
                        eg += f64::from(grad(&lg, x + i, y + j));
                        et += f64::from(grad(&lt, x + i, y + j));
                    }
                }
                let (va, vb, cov) = (va / 63.0, vb / 63.0, cov / 63.0);
                sum += f64::from((2.0 * ma * mb + c1) * (2.0 * cov + c2) / ((ma * ma + mb * mb + c1) * (va + vb + c2)));
                n += 1.0;
            }
            x += 4;
        }
        y += 4;
    }
    (sum / n.max(1.0), eg, et, n)
}

#[derive(Default, Clone, Copy)]
struct Score {
    ssim: f64,
    ssim_n: f64,
    detail: f64,
    detail_truth: f64,
    err: f64,
    bad: f64,
    mse: f64,
    movn: f64,
    mov: f64, // error over only the pixels that really changed (|last - truth| > 12)
    n: f64,
}
impl Score {
    fn add_struct(&mut self, got: &[u8], truth: &[u8], older: &[u8], w: usize, h: usize) {
        let (s, g, t, n) = moving_structure(got, truth, older, w, h);
        self.ssim += s * n;
        self.detail += g;
        self.detail_truth += t;
        self.ssim_n += n;
    }
    fn ssim(&self) -> f64 {
        self.ssim / self.ssim_n.max(1.0)
    }
    fn sharp(&self) -> f64 {
        100.0 * self.detail / self.detail_truth.max(1e-9)
    }
    fn add_moving(&mut self, got: &[u8], truth: &[u8], last: &[u8]) {
        let (mut e, mut n) = (0.0, 0.0);
        for ((g, t), l) in got.as_chunks::<4>().0.iter().zip(truth.as_chunks::<4>().0).zip(last.as_chunks::<4>().0) {
            let dl = (0..3).map(|c| (f64::from(l[c]) - f64::from(t[c])).abs()).sum::<f64>() / 3.0;
            if dl > 12.0 {
                e += (0..3).map(|c| (f64::from(g[c]) - f64::from(t[c])).abs()).sum::<f64>() / 3.0;
                n += 1.0;
            }
        }
        // weight by the share of such pixels so that frames with no motion do not count
        self.mov += e;
        self.movn += n;
    }
    fn mov(&self) -> f64 {
        self.mov / self.movn.max(1.0)
    }
    fn add(&mut self, got: &[u8], truth: &[u8]) {
        let (mut e, mut bad, mut se) = (0.0, 0.0, 0.0);
        for (g, t) in got.as_chunks::<4>().0.iter().zip(truth.as_chunks::<4>().0.iter()) {
            let d = (0..3).map(|c| (f64::from(g[c]) - f64::from(t[c])).abs()).sum::<f64>() / 3.0;
            e += d;
            se += (0..3).map(|c| (f64::from(g[c]) - f64::from(t[c])).powi(2)).sum::<f64>() / 3.0;
            if d > 24.0 {
                bad += 1.0;
            }
        }
        let px = (got.len() / 4) as f64;
        self.err += e / px;
        self.bad += bad / px;
        self.mse += se / px;
        self.n += 1.0;
    }
    fn mae(&self) -> f64 {
        self.err / self.n
    }
    fn bad(&self) -> f64 {
        100.0 * self.bad / self.n
    }
    fn psnr(&self) -> f64 {
        10.0 * (255.0f64 * 255.0 / (self.mse / self.n).max(1e-9)).log10()
    }
}

fn dump(dir: &str, name: &str, w: usize, px: &[u8]) {
    std::fs::write(format!("{dir}/{name}_{w}.bgra"), px).unwrap();
}

#[test]
fn real_footage_report() {
    let Ok(dir) = std::env::var("OPENPS_REAL_DIR") else {
        eprintln!("OPENPS_REAL_DIR not set: skipping the real-footage report");
        return;
    };
    let out = format!("{dir}/dump");
    std::fs::create_dir_all(&out).unwrap();
    let f1080 = decode("bird1080.annexb");
    let f720 = decode("bird720.annexb");
    println!("\nclip: {} frames 1080p, {} frames 720p", f1080.len(), f720.len());

    // ---- frame generation: feed every second frame, compare with the frame that was really there
    let (mut rep, mut v1, mut v2, mut sm) = (Score::default(), Score::default(), Score::default(), Score::default());
    let mut g = Gpu::new().unwrap();
    let mut worst = (0.0, None);
    let mut worst_sm = (0.0, None);
    let (mut j, mut t_ms, mut t_n, mut cuts) = (0, 0.0, 0.0, 0);
    let mut shots: Vec<(usize, Score, Score, Score)> = vec![(0, Score::default(), Score::default(), Score::default())];
    while j + 1 < f1080.len() {
        let tex = openps_metal::renderer::frame_to_texture(&g, &f1080[j]).unwrap();
        let cmd = g.command_buffer().unwrap();
        g.ingest(&cmd, tex, true).unwrap();
        cmd.commit();
        cmd.waitUntilCompleted();
        if j >= 4 {
            let cmd = g.command_buffer().unwrap();
            let (t, _) = g.compose_interp(&cmd, 0.5, None).unwrap();
            cmd.commit();
            cmd.waitUntilCompleted();
            let smooth = to_shared(&g, &t);
            let truth_prev = f1080[j - 1].copy_planes();
            let truth = f1080[j + 1].copy_planes();
            let last = f1080[j].copy_planes();
            let mut one = Score::default();
            one.add(&last, &truth);
            let mut pair = Score::default();
            pair.add(&f1080[j - 2].copy_planes(), &last);
            if one.mae() > 20.0 || pair.mae() > 20.0 {
                // a hard cut in the footage: nothing can predict the next shot; counted apart so it cannot hide the rest
                cuts += 1;
                shots.push((j + 2, Score::default(), Score::default(), Score::default()));
                j += 2;
                continue;
            }
            if shots.len() == 2 {
                let mut o = Score::default();
                o.add(&smooth, &truth_prev);
                if o.mae() > worst_sm.0 {
                    worst_sm = (o.mae(), Some((j, smooth.clone(), truth_prev.clone(), f1080[j - 2].copy_planes())));
                }
            }
            rep.add(&last, &truth);
            rep.add_moving(&last, &truth, &last);
            sm.add_moving(&smooth, &truth_prev, &f1080[j - 2].copy_planes());
            shots.last_mut().unwrap().1.add(&last, &truth);
            shots.last_mut().unwrap().3.add(&smooth, &truth_prev);
            sm.add(&smooth, &truth_prev);
            for legacy in [true, false] {
                g.legacy_warp = legacy;
                let t0 = std::time::Instant::now();
                let cmd = g.command_buffer().unwrap();
                let (t, _) = g.compose(&cmd, 0.5, None, (0.0, 0.0)).unwrap();
                cmd.commit();
                cmd.waitUntilCompleted();
                if !legacy {
                    t_ms += t0.elapsed().as_secs_f64() * 1e3;
                    t_n += 1.0;
                }
                let px = to_shared(&g, &t);
                if legacy {
                    v1.add(&px, &truth);
                    v1.add_moving(&px, &truth, &last);
                } else {
                    v2.add(&px, &truth);
                    v2.add_moving(&px, &truth, &last);
                    shots.last_mut().unwrap().2.add(&px, &truth);
                    if j == std::env::var("OPENPS_REAL_J").ok().and_then(|v| v.parse().ok()).unwrap_or(100) {
                        worst =
                            (one.mae(), Some((j, px, last.clone(), truth.clone(), smooth.clone(), truth_prev.clone())));
                    }
                }
            }
        }
        j += 2;
    }
    println!("\nFRAME GENERATION from a 15 fps feed (every 2nd frame of a 30 fps clip; harder than 60 -> 30), vs the real frame");
    println!("{:<22} {:>10} {:>12} {:>9} {:>16}", "strategy", "mean err", "bad pixels", "PSNR dB", "err, moving px");
    for (n, s) in [
        ("repeat last frame", &rep),
        ("extrapolate v1", &v1),
        ("extrapolate v2 (ours)", &v2),
        ("smooth (interpolate)", &sm),
    ] {
        println!("{n:<22} {:>10.2} {:>11.2}% {:>9.2} {:>16.2}", s.mae(), s.bad(), s.psnr(), s.mov());
    }
    println!("scene cuts skipped: {cuts}");
    println!("per shot (start frame; samples; mean error repeat / v2 / smooth; PSNR repeat / v2 / smooth):");
    for (st, a, b, c) in shots.iter().filter(|s| s.1.n >= 5.0) {
        println!(
            "  shot @{st:<4} n={:<3} err {:5.2} / {:5.2} / {:5.2}   PSNR {:5.2} / {:5.2} / {:5.2}",
            a.n,
            a.mae(),
            b.mae(),
            c.mae(),
            a.psnr(),
            b.psnr(),
            c.psnr()
        );
    }
    if let (e, Some((j, a, b, c))) = worst_sm {
        println!("worst smooth frame in shot 2: j={j} err {e:.2}");
        dump(&out, "sm_got", 1920, &a);
        dump(&out, "sm_truth", 1920, &b);
        dump(&out, "sm_prevsrc", 1920, &c);
    }
    println!("extrapolate v2 time incl. readback: {:.2} ms/frame", t_ms / t_n);
    if let (_, Some((j, px, last, truth, smooth, tp))) = worst {
        println!("most-motion frame: {j}");
        dump(&out, "gen_v2", 1920, &px);
        dump(&out, "gen_repeat", 1920, &last);
        dump(&out, "gen_truth", 1920, &truth);
        dump(&out, "gen_smooth", 1920, &smooth);
        dump(&out, "gen_smooth_truth", 1920, &tp);
    }

    // ---- upscaling: 720p -> 1080p with MetalFX vs plain bilinear, against the original 1080p frame
    let (mut mfx, mut bil) = (Score::default(), Score::default());
    let mut g = Gpu::new().unwrap();
    println!("\nUPSCALING 720p -> 1080p (MetalFX spatial scaler available: {})", g.spatial_scaler_available);
    for k in (0..f720.len().min(f1080.len())).step_by(12) {
        g.reset_stream();
        let tex = openps_metal::renderer::frame_to_texture(&g, &f720[k]).unwrap();
        let cmd = g.command_buffer().unwrap();
        g.ingest(&cmd, tex.clone(), false).unwrap();
        cmd.commit();
        cmd.waitUntilCompleted();
        let cmd = g.command_buffer().unwrap();
        let (t, up) = g.compose(&cmd, 0.0, Some((1920, 1080)), (0.0, 0.0)).unwrap();
        cmd.commit();
        cmd.waitUntilCompleted();
        assert!(up, "MetalFX did not run");
        let a = to_shared(&g, &t);
        let tg = make_tex(
            &g.dev,
            MTLPixelFormat::BGRA8Unorm,
            1920,
            1080,
            MTLTextureUsage::RenderTarget | MTLTextureUsage::ShaderRead,
            true,
        )
        .unwrap();
        let cmd = g.command_buffer().unwrap();
        g.present(&cmd, &tex, &tg).unwrap();
        cmd.commit();
        cmd.waitUntilCompleted();
        let b = read(&tg);
        let truth = f1080[k].copy_planes();
        mfx.add(&a, &truth);
        bil.add(&b, &truth);
        if k == 24 {
            dump(&out, "up_metalfx", 1920, &a);
            dump(&out, "up_bilinear", 1920, &b);
            dump(&out, "up_truth", 1920, &truth);
        }
    }
    println!("{:<22} {:>10} {:>9}", "method", "mean err", "PSNR dB");
    println!("{:<22} {:>10.2} {:>9.2}", "bilinear", bil.mae(), bil.psnr());
    println!("{:<22} {:>10.2} {:>9.2}", "MetalFX spatial", mfx.mae(), mfx.psnr());

    // ---- cost of upscaling a 1080p stream to the display modes (GPU time, includes extrapolation)
    println!("\nGPU COST per displayed frame, 1080p source (extrapolation + upscale, no readback)");
    for (w, h) in [(1920, 1080), (2560, 1440), (3840, 2160)] {
        g.reset_stream();
        for f in f1080.iter().take(3) {
            let tex = openps_metal::renderer::frame_to_texture(&g, f).unwrap();
            let cmd = g.command_buffer().unwrap();
            g.ingest(&cmd, tex, true).unwrap();
            cmd.commit();
            cmd.waitUntilCompleted();
        }
        let mut ts = vec![];
        for i in 0..120 {
            let t0 = std::time::Instant::now();
            let cmd = g.command_buffer().unwrap();
            let (_t, _) = g.compose(&cmd, 0.1 + (i % 8) as f32 * 0.1, Some((w, h)), (0.0, 0.0)).unwrap();
            cmd.commit();
            cmd.waitUntilCompleted();
            ts.push(t0.elapsed().as_secs_f64() * 1e3);
        }
        ts.sort_by(|a, b| a.total_cmp(b));
        println!(
            "{w}x{h}: median {:.2} ms, p95 {:.2} ms, max {:.2} ms (budget at 144 Hz: 6.94 ms)",
            ts[60], ts[113], ts[119]
        );
    }
}

/// MetalFX frame interpolation (fed with our motion field) against our own interpolation and against simply repeating the last frame, on
/// real footage: every second frame is fed, the generated middle frame is compared with the frame that really was there. Also times the
/// GPU cost of each. Run by hand like the report above.
#[test]
fn metalfx_against_builtin_on_real_footage() {
    let Ok(dir) = std::env::var("OPENPS_REAL_DIR") else {
        eprintln!("OPENPS_REAL_DIR not set: skipping");
        return;
    };
    for clip in ["bird720.annexb", "bird1080.annexb"] {
        let frames = decode(clip);
        let mut g = Gpu::new().unwrap();
        if !g.fx_available {
            eprintln!("MetalFX frame interpolation is not available here");
            return;
        }
        g.fx_use = true;
        let (mut rep, mut own, mut fx, mut blend) =
            (Score::default(), Score::default(), Score::default(), Score::default());
        let (mut t_fx_extra, mut t_own, mut n_t) = (0.0f64, 0.0f64, 0.0f64);
        let (mut base_ms, mut with_fx_ms) = (0.0f64, 0.0f64);
        let mut j = 0;
        while j + 1 < frames.len() {
            let tex = openps_metal::renderer::frame_to_texture(&g, &frames[j]).unwrap();
            let cmd = g.command_buffer().unwrap();
            g.ingest(&cmd, tex, true).unwrap();
            cmd.commit();
            cmd.waitUntilCompleted();
            let ingest_ms = (cmd.GPUEndTime() - cmd.GPUStartTime()) * 1e3;
            if j >= 6 {
                let truth = frames[j - 1].copy_planes();
                let last = frames[j].copy_planes();
                let mut cut = Score::default();
                cut.add(&last, &truth);
                let mut pair = Score::default();
                pair.add(&frames[j - 2].copy_planes(), &last);
                if cut.mae() > 20.0 || pair.mae() > 20.0 {
                    j += 2; // a hard cut in the footage: nothing predicts it, left out for every method alike
                    continue;
                }
                // repeating the frame: the previous real frame is what a plain 30 -> 60 would show at the midpoint
                rep.add(&frames[j - 2].copy_planes(), &truth);
                rep.add_moving(&frames[j - 2].copy_planes(), &truth, &frames[j - 2].copy_planes());
                let (fw, fh) = (frames[j].width(), frames[j].height());
                rep.add_struct(&frames[j - 2].copy_planes(), &truth, &frames[j - 2].copy_planes(), fw, fh);
                {
                    let (a, b) = (frames[j - 2].copy_planes(), frames[j].copy_planes());
                    let mix: Vec<u8> =
                        a.iter().zip(&b).map(|(x, y)| ((u16::from(*x) + u16::from(*y)) / 2) as u8).collect();
                    blend.add(&mix, &truth);
                    blend.add_moving(&mix, &truth, &frames[j - 2].copy_planes());
                    blend.add_struct(&mix, &truth, &frames[j - 2].copy_planes(), fw, fh);
                }
                for use_fx in [false, true] {
                    g.fx_use = use_fx;
                    let cmd = g.command_buffer().unwrap();
                    let (t, _) = g.compose_interp(&cmd, 0.5, None).unwrap();
                    cmd.commit();
                    cmd.waitUntilCompleted();
                    let ms = (cmd.GPUEndTime() - cmd.GPUStartTime()) * 1e3;
                    let px = to_shared(&g, &t);
                    if use_fx {
                        fx.add(&px, &truth);
                        fx.add_moving(&px, &truth, &frames[j - 2].copy_planes());
                        fx.add_struct(&px, &truth, &frames[j - 2].copy_planes(), fw, fh);
                        t_fx_extra += ms;
                    } else {
                        own.add(&px, &truth);
                        own.add_moving(&px, &truth, &frames[j - 2].copy_planes());
                        own.add_struct(&px, &truth, &frames[j - 2].copy_planes(), fw, fh);
                        t_own += ms;
                    }
                }
                g.fx_use = true;
                with_fx_ms += ingest_ms;
                n_t += 1.0;
            } else {
                base_ms += ingest_ms;
            }
            j += 2;
        }
        println!("\n== {clip}: {} generated frames scored ==", n_t as u32);
        println!(
            "{:<28}{:>8}{:>9}{:>9}{:>9}{:>16}{:>12}",
            "", "MAE", "PSNR dB", "SSIM*", "bad %", "MAE moving", "detail* %"
        );
        for (name, sc) in [
            ("repeat previous frame", &rep),
            ("50/50 blend of the two", &blend),
            ("built-in interpolation", &own),
            ("MetalFX interpolation", &fx),
        ] {
            println!(
                "{:<28}{:>8.2}{:>9.2}{:>9.4}{:>9.2}{:>16.2}{:>12.1}",
                name,
                sc.mae(),
                sc.psnr(),
                sc.ssim(),
                sc.bad(),
                sc.mov(),
                sc.sharp()
            );
        }
        println!("(* over the moving regions only)");
        println!(
            "GPU time per generated frame: built-in compose {:.2} ms | MetalFX compose {:.2} ms | ingest incl. MetalFX {:.2} ms (first frames, no MetalFX {:.2} ms)",
            t_own / n_t,
            t_fx_extra / n_t,
            with_fx_ms / n_t,
            base_ms / 6.0f64.max(1.0)
        );
        let _ = (dir.len(), t_own);
    }
}

/// Writes matching decoded frames of the 720p and 1080p encodes of the same clip as raw BGRA (`bird720_NNN.bgra` / `bird1080_NNN.bgra`) so
/// upscalers can be scored against the real higher-resolution picture. By hand: OPENPS_REAL_DIR=<dir> ... --ignored.
#[test]
#[ignore = "writes files: used for the upscaling study"]
fn dump_clip_frames() {
    let Ok(dir) = std::env::var("OPENPS_REAL_DIR") else { return };
    let out = format!("{dir}/dump");
    std::fs::create_dir_all(&out).unwrap();
    for (name, tag) in [("bird720.annexb", "bird720"), ("bird1080.annexb", "bird1080")] {
        let frames = decode(name);
        for (n, i) in (8..frames.len()).step_by(5).take(24).enumerate() {
            let f = &frames[i];
            assert_eq!(f.copy_planes().len(), f.width() * f.height() * 4);
            std::fs::write(format!("{out}/{tag}_{n:03}.bgra"), f.copy_planes()).unwrap();
        }
        println!("{tag}: {}x{}", frames[0].width(), frames[0].height());
    }
}
