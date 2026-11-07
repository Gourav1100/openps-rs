//! FSR (EASU) against MetalFX on real footage, measured against the truth (run by hand; needs ffmpeg):
//!   OPENPS_STILLS_DIR=tasks/upscale-compare cargo test -p openps-metal --release --test fsr_compare -- --ignored --nocapture
//! Real 1080p pictures (stills and a frame of a clip of your own PS5 capture) are shrunk to 540p with Lanczos, either left clean or pushed
//! through a real HEVC encode like a stream, then enlarged back to 1080p by each method; the result is compared with the original:
//! PSNR, SSIM (on luma) and the detail ratio (mean edge strength of the result over the truth: 1.00 equal, above over-sharpened, below soft).
#![cfg(target_os = "macos")]
use objc2_metal::*;
use openps_metal::gpu::{make_tex, Gpu, Tex};
use std::process::Command;
use std::ptr::NonNull;

const TW: usize = 1920;
const TH: usize = 1080;

fn ffmpeg(args: &[&str]) -> Vec<u8> {
    let out = Command::new("ffmpeg").args(["-v", "error", "-nostdin", "-y"]).args(args).output().expect("ffmpeg");
    assert!(out.status.success(), "ffmpeg {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    out.stdout
}

/// The truth: one 1080p picture as RGB bytes, from an image or frame `n` of a clip.
fn truth(path: &str, frame: Option<usize>) -> Vec<u8> {
    let pick = frame.map_or(String::new(), |n| format!("select=eq(n\\,{n}),"));
    ffmpeg(&[
        "-i",
        path,
        "-vf",
        &format!("{pick}scale={TW}:{TH}:flags=lanczos"),
        "-frames:v",
        "1",
        "-f",
        "rawvideo",
        "-pix_fmt",
        "rgb24",
        "-",
    ])
}

/// The input: the truth shrunk to 960x540, clean or after an HEVC encode (4.5 Mbit/s for a clip, crf 30 for a still).
fn input(path: &str, frame: Option<usize>, hevc: bool) -> Vec<u8> {
    let pick = frame.map_or(String::new(), |n| format!("select=eq(n\\,{n}),"));
    let vf = format!("{pick}scale={TW}:{TH}:flags=lanczos,scale=960:540:flags=lanczos");
    if !hevc {
        return ffmpeg(&["-i", path, "-vf", &vf, "-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "rgb24", "-"]);
    }
    let tmp = std::env::temp_dir().join(format!("fsr_cmp_{}.mkv", std::process::id()));
    let t = tmp.to_str().unwrap();
    if let Some(last) = frame {
        // a clip: the frames up to the one wanted, encoded as a stream would be (no B-frames), the last one decoded
        let n = last + 1;
        ffmpeg(&[
            "-i",
            path,
            "-vf",
            &vf,
            "-frames:v",
            &n.to_string(),
            "-c:v",
            "libx265",
            "-b:v",
            "4500k",
            "-bf",
            "0",
            "-g",
            "30",
            "-pix_fmt",
            "yuv420p",
            "-x265-params",
            "log-level=error",
            t,
        ]);
        let all = ffmpeg(&["-i", t, "-f", "rawvideo", "-pix_fmt", "rgb24", "-"]);
        let one = 960 * 540 * 3;
        return all[all.len() - one..].to_vec();
    }
    ffmpeg(&[
        "-i",
        path,
        "-vf",
        &vf,
        "-frames:v",
        "1",
        "-c:v",
        "libx265",
        "-crf",
        "30",
        "-pix_fmt",
        "yuv420p",
        "-x265-params",
        "log-level=error",
        t,
    ]);
    ffmpeg(&["-i", t, "-f", "rawvideo", "-pix_fmt", "rgb24", "-"])
}

fn upload(g: &Gpu, rgb: &[u8], w: usize, h: usize) -> Tex {
    let t = make_tex(&g.dev, MTLPixelFormat::BGRA8Unorm, w, h, MTLTextureUsage::ShaderRead, true).unwrap();
    let mut px = Vec::with_capacity(w * h * 4);
    for p in rgb.chunks(3) {
        px.extend([p[2], p[1], p[0], 255]);
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

fn read_rgb(t: &Tex) -> Vec<u8> {
    let (w, h) = (t.width(), t.height());
    let mut bgra = vec![0u8; w * h * 4];
    unsafe {
        t.getBytes_bytesPerRow_fromRegion_mipmapLevel(
            NonNull::new(bgra.as_mut_ptr().cast()).unwrap(),
            w * 4,
            MTLRegion { origin: MTLOrigin { x: 0, y: 0, z: 0 }, size: MTLSize { width: w, height: h, depth: 1 } },
            0,
        );
    }
    bgra.chunks(4).flat_map(|p| [p[2], p[1], p[0]]).collect()
}

struct Method {
    name: &'static str,
    fsr: bool,
    linear: bool,
    sharpness: f32,
    rcas: f32,
}

fn run(g: &mut Gpu, src: &Tex, m: &Method) -> Vec<u8> {
    (g.fsr, g.linear_upscale, g.sharpness, g.rcas) = (m.fsr, m.linear, m.sharpness, m.rcas);
    g.reset_stream();
    let cmd = g.command_buffer().unwrap();
    g.ingest(&cmd, src.clone(), false).unwrap();
    let (t, up) = g.compose(&cmd, 0.0, Some((TW, TH)), (0.0, 0.0)).unwrap();
    assert!(up);
    let tg = make_tex(
        &g.dev,
        MTLPixelFormat::BGRA8Unorm,
        TW,
        TH,
        MTLTextureUsage::RenderTarget | MTLTextureUsage::ShaderRead,
        true,
    )
    .unwrap();
    g.present(&cmd, &t, &tg).unwrap();
    cmd.commit();
    cmd.waitUntilCompleted();
    read_rgb(&tg)
}

fn luma(rgb: &[u8]) -> Vec<f64> {
    rgb.chunks(3).map(|p| 0.299 * f64::from(p[0]) + 0.587 * f64::from(p[1]) + 0.114 * f64::from(p[2])).collect()
}

fn psnr(a: &[u8], b: &[u8]) -> f64 {
    let mse = a.iter().zip(b).map(|(x, y)| (f64::from(*x) - f64::from(*y)).powi(2)).sum::<f64>() / a.len() as f64;
    10.0 * (255.0f64 * 255.0 / mse.max(1e-9)).log10()
}

/// SSIM of two luma planes over 8x8 windows, stride 4.
fn ssim(a: &[f64], b: &[f64]) -> f64 {
    let (c1, c2) = ((0.01f64 * 255.0).powi(2), (0.03f64 * 255.0).powi(2));
    let (mut sum, mut n) = (0.0, 0.0);
    let mut y = 0;
    while y + 8 <= TH {
        let mut x = 0;
        while x + 8 <= TW {
            let (mut ma, mut mb) = (0.0, 0.0);
            for j in 0..8 {
                for i in 0..8 {
                    ma += a[(y + j) * TW + x + i];
                    mb += b[(y + j) * TW + x + i];
                }
            }
            (ma, mb) = (ma / 64.0, mb / 64.0);
            let (mut va, mut vb, mut cov) = (0.0, 0.0, 0.0);
            for j in 0..8 {
                for i in 0..8 {
                    let (da, db) = (a[(y + j) * TW + x + i] - ma, b[(y + j) * TW + x + i] - mb);
                    (va, vb, cov) = (va + da * da, vb + db * db, cov + da * db);
                }
            }
            (va, vb, cov) = (va / 63.0, vb / 63.0, cov / 63.0);
            sum += ((2.0 * ma * mb + c1) * (2.0 * cov + c2)) / ((ma * ma + mb * mb + c1) * (va + vb + c2));
            n += 1.0;
            x += 4;
        }
        y += 4;
    }
    sum / n
}

fn detail(l: &[f64]) -> f64 {
    let mut s = 0.0;
    for y in 1..TH - 1 {
        for x in 1..TW - 1 {
            let (gx, gy) = (l[y * TW + x + 1] - l[y * TW + x - 1], l[(y + 1) * TW + x] - l[(y - 1) * TW + x]);
            s += (gx * gx + gy * gy).sqrt();
        }
    }
    s / ((TW - 2) * (TH - 2)) as f64
}

#[test]
#[ignore = "needs OPENPS_STILLS_DIR (tasks/upscale-compare) and ffmpeg; run by hand"]
fn fsr_against_metalfx_on_real_pictures_measured_against_the_truth() {
    let dir = std::env::var("OPENPS_STILLS_DIR").expect("OPENPS_STILLS_DIR");
    let sources: [(&str, String, Option<usize>); 3] = [
        ("dark_scene (still)", format!("{dir}/stills/dark_scene_0_source_1080p.png"), None),
        ("day_forest (still)", format!("{dir}/stills/day_forest_0_source_1080p.png"), None),
        ("dark_real (clip, frame 8)", format!("{dir}/videos/dark_real_0_source_1080p.mov"), Some(8)),
    ];
    let methods = [
        Method { name: "plain enlargement (bilinear)", fsr: true, linear: false, sharpness: 0.0, rcas: 0.0 },
        Method {
            name: "MetalFX linear, as the app has it (sharpness 60 %, RCAS 100 %)",
            fsr: false,
            linear: true,
            sharpness: 0.6,
            rcas: 1.0,
        },
        Method { name: "MetalFX linear, scaler alone", fsr: false, linear: true, sharpness: 1.0, rcas: 0.0 },
        Method { name: "FSR EASU alone", fsr: true, linear: false, sharpness: 1.0, rcas: 0.0 },
        Method { name: "FSR EASU + RCAS 50 %", fsr: true, linear: false, sharpness: 1.0, rcas: 0.5 },
        Method { name: "FSR EASU + RCAS 100 %", fsr: true, linear: false, sharpness: 1.0, rcas: 1.0 },
        Method {
            name: "FSR, as the app has it (sharpness 60 %, RCAS 100 %)",
            fsr: true,
            linear: false,
            sharpness: 0.6,
            rcas: 1.0,
        },
        Method { name: "FSR sharpness 80 % + RCAS 100 %", fsr: true, linear: false, sharpness: 0.8, rcas: 1.0 },
    ];
    let mut g = Gpu::new().unwrap();
    for (label, path, frame) in &sources {
        let truth_rgb = truth(path, *frame);
        let (tl, td) = (luma(&truth_rgb), detail(&luma(&truth_rgb)));
        for (cond, hevc) in [("clean 540p", false), ("HEVC 540p", true)] {
            let small = input(path, *frame, hevc);
            let src = upload(&g, &small, 960, 540);
            println!("\n== {label}, {cond} -> 1080p");
            println!("{:<66} {:>7} {:>7} {:>8}", "method", "PSNR", "SSIM", "detail");
            for m in &methods {
                let out = run(&mut g, &src, m);
                let ol = luma(&out);
                println!(
                    "{:<66} {:>7.2} {:>7.4} {:>8.3}",
                    m.name,
                    psnr(&out, &truth_rgb),
                    ssim(&ol, &tl),
                    detail(&ol) / td
                );
            }
        }
    }
}
