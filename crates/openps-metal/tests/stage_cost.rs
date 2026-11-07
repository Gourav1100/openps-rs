//! By hand: what each stage of frame generation costs on the GPU, on real pictures (a capture, `OPENPS_CAPTURE_FILE`) or a synthetic
//! pan, at the stream's size and an upscaled size.
//! `OPENPS_CAPTURE_FILE=<file.opsvid> [OPENPS_STAGE_OUT=2880x1620] cargo test -p openps-metal --release --test stage_cost -- --ignored --nocapture`
#![cfg(target_os = "macos")]
use objc2_metal::*;
use openps_metal::gpu::{make_tex, Gpu, Tex};
use openps_video::nal::{self, Codec, ParameterSets};
use openps_video::vt::{Decoder, Frame, OutputFormat};

fn frames_from_capture(path: &str, limit: usize) -> Vec<Frame> {
    let b = std::fs::read(path).expect("capture file");
    let (mut aus, mut header, mut p) = (vec![], vec![], 8usize);
    while p + 16 <= b.len() {
        let len = u32::from_le_bytes(b[p + 12..p + 16].try_into().unwrap()) as usize;
        if p + 16 + len > b.len() {
            break;
        }
        let data = b[p + 16..p + 16 + len].to_vec();
        if b[p] == 1 {
            header.extend(data);
        } else {
            aus.push((u16::from_le_bytes([b[p + 1], b[p + 2]]), data));
        }
        p += 16 + len;
    }
    let src = if header.is_empty() { &aus[0].1 } else { &header };
    let codec: Codec = nal::detect_codec(src).expect("codec");
    let mut d = Decoder::new_with(codec, ParameterSets::from_annex_b(codec, src), OutputFormat::Bgra).unwrap();
    let mut out = vec![];
    // from the start (the pictures depend on each other); the ones after the first 300 are used (a camera that is moving)
    for (id, au) in &aus {
        if let Ok(f) = d.decode_annex_b(au, i64::from(*id) * 1500) {
            out.extend(f);
        }
        if out.len() >= 300 + limit {
            break;
        }
    }
    out.drain(..300.min(out.len()));
    out
}

fn ms(cmd: &objc2::rc::Retained<objc2::runtime::ProtocolObject<dyn MTLCommandBuffer>>) -> f64 {
    cmd.commit();
    cmd.waitUntilCompleted();
    assert_eq!(cmd.status(), MTLCommandBufferStatus::Completed, "{:?}", cmd.error());
    (cmd.GPUEndTime() - cmd.GPUStartTime()) * 1e3
}

fn stats(mut v: Vec<f64>) -> String {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = v.len();
    format!(
        "mean {:5.2}  p50 {:5.2}  p90 {:5.2}  max {:5.2}",
        v.iter().sum::<f64>() / n as f64,
        v[n / 2],
        v[n * 9 / 10],
        v[n - 1]
    )
}

#[test]
#[ignore = "measurement: needs OPENPS_CAPTURE_FILE"]
fn what_each_stage_of_frame_generation_costs() {
    let Ok(path) = std::env::var("OPENPS_CAPTURE_FILE") else { return };
    let out: Vec<usize> = std::env::var("OPENPS_STAGE_OUT")
        .unwrap_or("2880x1620".into())
        .split('x')
        .filter_map(|v| v.parse().ok())
        .collect();
    let frames = frames_from_capture(&path, 80);
    let mut g = Gpu::new().unwrap();
    g.fx_use = false;
    let (mut ingest, mut interp, mut up, mut present) = (vec![], vec![], vec![], vec![]);
    let target = make_tex(
        &g.dev,
        MTLPixelFormat::BGRA8Unorm,
        3440,
        1440,
        MTLTextureUsage::RenderTarget | MTLTextureUsage::ShaderRead,
        false,
    )
    .unwrap();
    for (i, f) in frames.iter().enumerate() {
        let tex: Tex = openps_metal::renderer::frame_to_texture(&g, f).unwrap();
        let cmd = g.command_buffer().unwrap();
        g.ingest(&cmd, tex, true).unwrap();
        let t_ingest = ms(&cmd);
        if i < 8 {
            continue;
        }
        let cmd = g.command_buffer().unwrap();
        let _ = g.compose_interp(&cmd, 0.5, None).unwrap();
        let t_interp = ms(&cmd);
        let cmd = g.command_buffer().unwrap();
        let (t, _) = g.compose_interp(&cmd, 0.5, Some((out[0], out[1]))).unwrap();
        let t_both = ms(&cmd);
        let cmd = g.command_buffer().unwrap();
        g.present(&cmd, &t, &target).unwrap();
        let t_present = ms(&cmd);
        ingest.push(t_ingest);
        interp.push(t_interp);
        up.push((t_both - t_interp).max(0.0));
        present.push(t_present);
    }
    let (w, h) = (frames[0].width(), frames[0].height());
    println!("stream {w}x{h} -> {}x{} -> 3440x1440 window, {} pictures", out[0], out[1], ingest.len());
    println!("real picture: ingest (luma, motion, refine) {}", stats(ingest.clone()));
    println!("generated picture: interpolation            {}", stats(interp.clone()));
    println!("generated picture: upscale (MetalFX/finish) {}", stats(up.clone()));
    println!("every picture: present                      {}", stats(present.clone()));
    let n = ingest.len() as f64;
    let (a, b, c, d) = (
        ingest.iter().sum::<f64>() / n,
        interp.iter().sum::<f64>() / n,
        up.iter().sum::<f64>() / n,
        present.iter().sum::<f64>() / n,
    );
    println!("a real picture costs about {:.2} ms, a generated one {:.2} ms; at 60 -> 144 (1 real in 2.4) the GPU is busy {:.0} % of the time", a + c + d, b + c + d, 100.0 * (60.0 * (a + c + d) + 84.0 * (b + c + d)) / 1000.0);
}
