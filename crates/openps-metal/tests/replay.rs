//! By hand, on a capture of a real session made with `OPENPS_VIDEO_CAPTURE=<dir>` (see `scripts/record-session.sh`):
//!
//! `OPENPS_CAPTURE_FILE=<file.opsvid> [OPENPS_REPLAY_OUT=<dir>] cargo test -p openps-metal --release --test replay -- --ignored --nocapture`
//!
//! Replays the stream through the frame generation. Every second picture of the stream is fed in and the picture in between is generated;
//! the real one that was there is the right answer. The report is about thin vertical structure (railings, a radiator, window frames, text
//! stems), where a shimmer in a pan shows: the error of the generated picture there, against showing the nearer real picture, how often a
//! vertical-edge pixel is far off, and how the motion search behaved (the share of blocks with a motion, the dominant motion). The worst
//! pictures are written as PPM files (truth, generated, nearer real picture) to `OPENPS_REPLAY_OUT` when it is set.
#![cfg(target_os = "macos")]
use objc2_metal::*;
use openps_metal::gpu::{make_tex, Gpu, Tex};
use openps_video::nal::{self, Codec, ParameterSets};
use openps_video::vt::{Decoder, Frame, OutputFormat};
use std::ptr::NonNull;

struct Rec {
    kind: u8,
    id: u16,
    at_us: u64,
    data: Vec<u8>,
}

/// The capture format of `openps_core::capture` (`OPSVID1\n`, then `kind u8, id u16, adaptive u8, at_us u64, len u32, data`).
fn read_capture(path: &str) -> Vec<Rec> {
    let b = std::fs::read(path).expect("capture file");
    assert_eq!(&b[..8], b"OPSVID1\n", "not a video capture");
    let (mut out, mut p) = (vec![], 8);
    while p + 16 <= b.len() {
        let len = u32::from_le_bytes(b[p + 12..p + 16].try_into().unwrap()) as usize;
        if p + 16 + len > b.len() {
            break;
        }
        out.push(Rec {
            kind: b[p],
            id: u16::from_le_bytes([b[p + 1], b[p + 2]]),
            at_us: u64::from_le_bytes(b[p + 4..p + 12].try_into().unwrap()),
            data: b[p + 16..p + 16 + len].to_vec(),
        });
        p += 16 + len;
    }
    out
}

/// A raw Annex B stream (`.hevc`, `.h264`, `.annexb`) as a capture's records, for streams made from a capture by re-encoding it at another
/// bitrate (`ffmpeg -f hevc -i <extracted> -c:v libx265 -b:v 3M -f hevc out.hevc`).
fn read_raw(path: &str) -> Vec<Rec> {
    let b = std::fs::read(path).expect("stream file");
    let codec = nal::detect_codec(&b).expect("codec");
    nal::access_units(codec, &b)
        .iter()
        .enumerate()
        .map(|(i, au)| Rec { kind: 0, id: i as u16, at_us: i as u64 * 16_667, data: au.to_vec() })
        .collect()
}

fn load(path: &str) -> Vec<Rec> {
    if path.ends_with(".opsvid") {
        read_capture(path)
    } else {
        read_raw(path)
    }
}

fn decode_all(recs: &[Rec]) -> Vec<Frame> {
    let header: Vec<u8> = recs.iter().filter(|r| r.kind == 1).flat_map(|r| r.data.clone()).collect();
    let first = recs.iter().find(|r| r.kind == 0).expect("no frames");
    let codec: Codec = nal::detect_codec(if header.is_empty() { &first.data } else { &header }).expect("codec");
    let source = if header.is_empty() { &first.data } else { &header };
    let mut d =
        Decoder::new_with(codec, ParameterSets::from_annex_b(codec, source), OutputFormat::Bgra).expect("decoder");
    let mut out = vec![];
    for r in recs.iter().filter(|r| r.kind == 0) {
        if let Ok(f) = d.decode_annex_b(&r.data, i64::from(r.id) * 1500) {
            out.extend(f);
        }
    }
    out
}

fn to_bytes(g: &Gpu, t: &Tex) -> Vec<u8> {
    let (w, h) = (t.width(), t.height());
    let tg = make_tex(
        &g.dev,
        MTLPixelFormat::BGRA8Unorm,
        w,
        h,
        MTLTextureUsage::RenderTarget | MTLTextureUsage::ShaderRead,
        true,
    )
    .unwrap();
    let cmd = g.command_buffer().unwrap();
    g.present(&cmd, t, &tg).unwrap();
    cmd.commit();
    cmd.waitUntilCompleted();
    let mut out = vec![0u8; w * h * 4];
    unsafe {
        tg.getBytes_bytesPerRow_fromRegion_mipmapLevel(
            NonNull::new(out.as_mut_ptr().cast()).unwrap(),
            w * 4,
            MTLRegion { origin: MTLOrigin { x: 0, y: 0, z: 0 }, size: MTLSize { width: w, height: h, depth: 1 } },
            0,
        );
    }
    out
}

fn luma(px: &[u8]) -> Vec<f32> {
    px.as_chunks::<4>()
        .0
        .iter()
        .map(|p| 0.114 * f32::from(p[0]) + 0.587 * f32::from(p[1]) + 0.299 * f32::from(p[2]))
        .collect()
}

/// Mean absolute Laplacian over the picture and over its flat parts (where the gradient is small: grain, noise, compression texture).
fn detail(l: &[f32], w: usize, h: usize) -> (f64, f64) {
    let (mut all, mut flat, mut nall, mut nflat) = (0.0f64, 0.0f64, 0usize, 0usize);
    for y in (2..h - 2).step_by(2) {
        for x in (2..w - 2).step_by(2) {
            let c = l[y * w + x];
            let lap = (4.0 * c - l[y * w + x - 1] - l[y * w + x + 1] - l[(y - 1) * w + x] - l[(y + 1) * w + x]).abs();
            let grad = (l[y * w + x + 1] - l[y * w + x - 1]).abs() + (l[(y + 1) * w + x] - l[(y - 1) * w + x]).abs();
            all += f64::from(lap);
            nall += 1;
            if grad < 8.0 {
                flat += f64::from(lap);
                nflat += 1;
            }
        }
    }
    (all / nall.max(1) as f64, flat / nflat.max(1) as f64)
}

/// Pixels of thin vertical structure in `truth`: a strong horizontal luma step and a weak vertical one.
fn vertical_edges(l: &[f32], w: usize, h: usize) -> Vec<bool> {
    let mut m = vec![false; w * h];
    for y in 2..h - 2 {
        for x in 2..w - 2 {
            let gx = (l[y * w + x + 1] - l[y * w + x - 1]).abs();
            let gy = (l[(y + 1) * w + x] - l[(y - 1) * w + x]).abs();
            m[y * w + x] = gx > 24.0 && gy < gx * 0.35;
        }
    }
    m
}

fn mae_on(a: &[f32], b: &[f32], mask: &[bool]) -> (f64, f64, usize) {
    let (mut s, mut far, mut n) = (0.0f64, 0.0f64, 0usize);
    for ((x, y), m) in a.iter().zip(b).zip(mask) {
        if *m {
            let d = f64::from((x - y).abs());
            s += d;
            far += f64::from(u8::from(d > 24.0));
            n += 1;
        }
    }
    (s / n.max(1) as f64, far / n.max(1) as f64, n)
}

fn ppm(dir: &str, name: &str, px: &[u8], w: usize, h: usize) {
    let mut out = format!("P6\n{w} {h}\n255\n").into_bytes();
    for p in px.as_chunks::<4>().0 {
        out.extend([p[2], p[1], p[0]]);
    }
    std::fs::write(format!("{dir}/{name}.ppm"), out).unwrap();
}

struct Report {
    pairs: usize,
    skipped: usize,
    edge_px: usize,
    generated_err: f64,
    nearer_err: f64,
    generated_far: f64,
    nearer_far: f64,
    worst: (f64, usize),
    /// per pair: (first picture, error generated, error nearer real, camera motion x and y in pixels a picture)
    each: Vec<(usize, f64, f64, f32, f32)>,
    /// error over the crop rectangle: generated, nearer real picture, pairs
    crop: (f64, f64, usize),
    /// detail (mean absolute Laplacian) of the generated picture and of the real one, over the whole picture and over the flat parts
    /// (grain and compression noise): a generated picture that is softer or smoother than the real ones flickers against them
    detail: [f64; 4],
}

fn replay(path: &str, out_dir: Option<&str>) -> Report {
    let frames = decode_all(&load(path));
    assert!(frames.len() > 12, "only {} pictures decoded", frames.len());
    let (w, h) = (frames[0].width(), frames[0].height());
    println!("{} pictures, {w}x{h}", frames.len());
    if let Some(d) = out_dir {
        std::fs::create_dir_all(d).unwrap();
    }
    let mut g = Gpu::new().unwrap();
    // OPENPS_REPLAY_FX=1 makes the midpoints with MetalFX (as in the app at about 2x) instead of the built-in interpolator
    g.fx_use = std::env::var_os("OPENPS_REPLAY_FX").is_some() && g.fx_available;
    let planes: Vec<Vec<u8>> = frames.iter().map(Frame::copy_planes).collect();
    let (mut gen_e, mut near_e, mut gen_far, mut near_far, mut edge_px) = (0.0, 0.0, 0.0, 0.0, 0usize);
    let (mut pairs, mut skipped, mut worst) = (0usize, 0usize, (0.0f64, 0usize));
    let mut each = vec![];
    // OPENPS_REPLAY_RANGE=<from>,<to> limits the pairs; OPENPS_REPLAY_CROP=<x0>,<y0>,<x1>,<y1> adds the error over that rectangle
    let range: Vec<usize> = std::env::var("OPENPS_REPLAY_RANGE")
        .unwrap_or_default()
        .split(',')
        .filter_map(|v| v.trim().parse().ok())
        .collect();
    let crop: Vec<usize> = std::env::var("OPENPS_REPLAY_CROP")
        .unwrap_or_default()
        .split(',')
        .filter_map(|v| v.trim().parse().ok())
        .collect();
    let (mut crop_gen, mut crop_near, mut crop_n) = (0.0f64, 0.0f64, 0usize);
    let mut det = [0.0f64; 4];
    let start = if range.len() == 2 { range[0] } else { 0 };
    let mut j = start;
    let stop = if range.len() == 2 { range[1].min(frames.len()) } else { frames.len() };
    let mut last_in: Option<usize> = None;
    while j + 2 < stop {
        // pictures j and j+2 go in (each once, as in the app), j+1 is the right answer; the stream restarts now and then so that one
        // wrong stretch cannot carry on
        let ingest = |g: &mut Gpu, last: &mut Option<usize>, k: usize| {
            if *last == Some(k) {
                return;
            }
            let tex = openps_metal::renderer::frame_to_texture(g, &frames[k]).unwrap();
            let cmd = g.command_buffer().unwrap();
            g.ingest(&cmd, tex, true).unwrap();
            cmd.commit();
            cmd.waitUntilCompleted();
            *last = Some(k);
        };
        if (j - start) % 600 == 0 {
            g.reset_stream();
            last_in = None;
        }
        ingest(&mut g, &mut last_in, j);
        ingest(&mut g, &mut last_in, j + 2);
        let (older, truth, newer) = (&planes[j], &planes[j + 1], &planes[j + 2]);
        let (lt, lo, ln) = (luma(truth), luma(older), luma(newer));
        // a repeated picture or a cut says nothing about the motion search
        let still = lo.iter().zip(&ln).map(|(a, b)| (a - b).abs()).sum::<f32>() / lo.len() as f32;
        if still < 0.2 || still > 40.0 {
            skipped += 1;
            j += 2;
            continue;
        }
        let cmd = g.command_buffer().unwrap();
        let (t, _) = g.compose_interp(&cmd, 0.5, None).unwrap();
        cmd.commit();
        cmd.waitUntilCompleted();
        let gen = to_bytes(&g, &t);
        let lg = luma(&gen);
        let mask = vertical_edges(&lt, w, h);
        let (e, far, n) = mae_on(&lg, &lt, &mask);
        let nearer = if mae_on(&lo, &lt, &mask).0 < mae_on(&ln, &lt, &mask).0 { &lo } else { &ln };
        let (ne, nfar, _) = mae_on(nearer, &lt, &mask);
        if n > 500 {
            gen_e += e;
            near_e += ne;
            gen_far += far;
            near_far += nfar;
            edge_px += n;
            pairs += 1;
            let m = g.global_motion().map_or((0.0, 0.0), |m| (m.x, m.y));
            each.push((j, e, ne, m.0, m.1));
            if std::env::var("OPENPS_REPLAY_FIELD").ok().and_then(|v| v.parse::<usize>().ok()) == Some(j) {
                // the coarse field as text: the horizontal motion in pixels of the picture, one cell per block, '.' where there is none,
                // upper case where the block is ambiguous
                if let Some((c, cw)) = g.coarse_motion_field() {
                    for row in c.chunks(cw).step_by(2) {
                        let line: String = row
                            .iter()
                            .step_by(2)
                            .map(|v| {
                                let x = (v[0] * 2.0).round() as i32;
                                let ch = if x == 0 {
                                    '.'
                                } else if x > 0 {
                                    char::from(b'a' + (x.min(25) as u8))
                                } else {
                                    char::from(b'0' + ((-x).min(9) as u8))
                                };
                                if v[3] < 0.5 && x > 0 {
                                    ch.to_ascii_uppercase()
                                } else {
                                    ch
                                }
                            })
                            .collect();
                        println!("F {line}");
                    }
                }
            }
            let (dg, fg) = detail(&lg, w, h);
            let (dt, ft) = detail(&lt, w, h);
            det[0] += dg;
            det[1] += dt;
            det[2] += fg;
            det[3] += ft;
            if let [x0, y0, x1, y1] = crop[..] {
                let (mut a, mut b, mut c) = (0.0f64, 0.0f64, 0usize);
                for y in y0..y1.min(h) {
                    for x in x0..x1.min(w) {
                        a += f64::from((lg[y * w + x] - lt[y * w + x]).abs());
                        b += f64::from((nearer[y * w + x] - lt[y * w + x]).abs());
                        c += 1;
                    }
                }
                crop_gen += a / c.max(1) as f64;
                crop_near += b / c.max(1) as f64;
                crop_n += 1;
            }
            let wanted = std::env::var("OPENPS_REPLAY_PAIRS").unwrap_or_default();
            if let (Some(d), true) = (out_dir, wanted.split(',').any(|v| v.trim() == j.to_string())) {
                ppm(d, &format!("pair{j}_generated"), &gen, w, h);
                ppm(d, &format!("pair{j}_truth"), truth, w, h);
                ppm(d, &format!("pair{j}_older"), older, w, h);
                ppm(d, &format!("pair{j}_newer"), newer, w, h);
            }
            if e > worst.0 {
                worst = (e, j);
                if let Some(d) = out_dir {
                    ppm(d, "worst_generated", &gen, w, h);
                    ppm(d, "worst_truth", truth, w, h);
                    ppm(d, "worst_nearer_real", if std::ptr::eq(nearer, &lo) { older } else { newer }, w, h);
                }
            }
        }
        j += 2;
    }
    let p = pairs.max(1) as f64;
    Report {
        pairs,
        skipped,
        edge_px,
        generated_err: gen_e / p,
        nearer_err: near_e / p,
        generated_far: 100.0 * gen_far / p,
        nearer_far: 100.0 * near_far / p,
        worst,
        each,
        crop: (crop_gen, crop_near, crop_n),
        detail: det,
    }
}

#[test]
#[ignore = "needs a capture: OPENPS_CAPTURE_FILE"]
fn replay_a_capture_through_frame_generation() {
    let Ok(path) = std::env::var("OPENPS_CAPTURE_FILE") else { return };
    let out = std::env::var("OPENPS_REPLAY_OUT").ok();
    let r = replay(&path, out.as_deref());
    println!(
        "pairs measured {} (skipped {}: repeated pictures or cuts), vertical-edge pixels {}",
        r.pairs, r.skipped, r.edge_px
    );
    println!("                          error on vertical edges   share more than 24 levels off");
    println!("generated picture         {:>8.2}                  {:>6.2} %", r.generated_err, r.generated_far);
    println!("nearer real picture       {:>8.2}                  {:>6.2} %", r.nearer_err, r.nearer_far);
    let mut by_gap = r.each.clone();
    by_gap.sort_by(|a, b| (b.1 - b.2).total_cmp(&(a.1 - a.2)));
    println!("pairs where the generated picture is furthest behind the nearer real one (first picture, generated, nearer, motion px):");
    for e in by_gap.iter().take(12) {
        println!("  {:>5}  {:>6.2}  {:>6.2}   {:>6.1} {:>6.1}", e.0, e.1, e.2, e.3, e.4);
    }
    let sideways: Vec<_> = r.each.iter().filter(|e| e.3.abs() > 6.0 && e.3.abs() > e.4.abs() * 2.0).collect();
    let mut bad_sideways: Vec<_> = sideways.iter().filter(|e| e.1 > e.2).collect();
    bad_sideways.sort_by(|a, b| (b.1 - b.2).total_cmp(&(a.1 - a.2)));
    for e in bad_sideways.iter().take(8) {
        println!(
            "  sideways, worse than the real picture: {:>5}  {:>6.2}  {:>6.2}   {:>6.1} {:>6.1}",
            e.0, e.1, e.2, e.3, e.4
        );
    }
    let worse = sideways.iter().filter(|e| e.1 > e.2).count();
    println!("sideways camera motion (more than 6 px a picture): {} pairs, generated worse than the nearer real picture in {worse}", sideways.len());
    println!(
        "detail (mean |Laplacian|) generated / real: whole picture {:.3}, flat parts (grain, noise) {:.3}",
        r.detail[0] / r.detail[1].max(1e-9),
        r.detail[2] / r.detail[3].max(1e-9)
    );
    if r.crop.2 > 0 {
        let n = r.crop.2 as f64;
        println!("crop: generated {:.2}, nearer real {:.2} over {} pairs", r.crop.0 / n, r.crop.1 / n, r.crop.2);
    }
    println!("worst pair: {} (error {:.2}){}", r.worst.1, r.worst.0, if out.is_some() { ", written out" } else { "" });
}

/// The replay itself works: a capture file made from a known clip is read, decoded and measured, and the generated picture is better
/// than the nearer real one on the moving thin structure of the test pattern.
#[test]
fn the_replay_runs_on_a_capture_made_from_a_clip() {
    let clip = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/testsrc60.annexb")).unwrap();
    let mut file = b"OPSVID1\n".to_vec();
    let mut rec = |kind: u8, id: u16, data: &[u8]| {
        file.extend([kind]);
        file.extend(id.to_le_bytes());
        file.extend([0u8]);
        file.extend((u64::from(id) * 16_666).to_le_bytes());
        file.extend((data.len() as u32).to_le_bytes());
        file.extend(data);
    };
    for (i, au) in nal::access_units(Codec::H264, &clip).iter().enumerate() {
        rec(0, i as u16, au);
    }
    let path = std::env::temp_dir().join(format!("openps-replay-test-{}.opsvid", std::process::id()));
    std::fs::write(&path, file).unwrap();
    let r = replay(path.to_str().unwrap(), None);
    std::fs::remove_file(&path).ok();
    eprintln!("{} pairs, generated {:.2}, nearer real {:.2}", r.pairs, r.generated_err, r.nearer_err);
    assert!(r.pairs > 0, "nothing was measured ({} skipped)", r.skipped);
    assert!(r.generated_err.is_finite() && r.nearer_err.is_finite());
}

/// How the stream behaves over time: per second, the pictures that arrived, how many of them were new (the game's own frame rate: a
/// console repeats a picture when the game was late), and the longest gap between two arrivals.
#[test]
#[ignore = "needs a capture: OPENPS_CAPTURE_FILE"]
fn the_streams_timeline_second_by_second() {
    let Ok(path) = std::env::var("OPENPS_CAPTURE_FILE") else { return };
    let recs = load(&path);
    let times: Vec<(u16, u64)> = recs.iter().filter(|r| r.kind == 0).map(|r| (r.id, r.at_us)).collect();
    let frames = decode_all(&recs);
    let planes: Vec<Vec<f32>> = frames.iter().map(|f| luma(&f.copy_planes())).collect();
    let n = planes.len().min(times.len());
    let new: Vec<bool> = (0..n)
        .map(|i| {
            i == 0
                || planes[i]
                    .iter()
                    .step_by(7)
                    .zip(planes[i - 1].iter().step_by(7))
                    .map(|(a, b)| (a - b).abs())
                    .sum::<f32>()
                    / (planes[i].len() / 7) as f32
                    > 0.15
        })
        .collect();
    println!("second  arrived  new pictures  longest gap ms  new-picture gap max ms");
    let mut sec = 0u64;
    let (mut arrived, mut fresh, mut gap, mut last_t, mut last_new_t, mut new_gap) = (0, 0, 0u64, 0u64, 0u64, 0u64);
    for i in 0..n {
        let t = times[i].1;
        if t / 1_000_000 != sec {
            println!("{sec:>6}  {arrived:>7}  {fresh:>12}  {:>14.1}  {:>22.1}", gap as f64 / 1e3, new_gap as f64 / 1e3);
            sec = t / 1_000_000;
            (arrived, fresh, gap, new_gap) = (0, 0, 0, 0);
        }
        arrived += 1;
        if i > 0 {
            gap = gap.max(t - last_t);
        }
        if new[i] {
            fresh += 1;
            if last_new_t > 0 {
                new_gap = new_gap.max(t - last_new_t);
            }
            last_new_t = t;
        }
        last_t = t;
    }
}

/// How long Smooth mode would hold the picture because the next real frame is late, on the arrival times of the capture, with the
/// smoothing buffer off and on: the 20-30 ms gaps in frame pacing.
#[test]
#[ignore = "needs a capture: OPENPS_CAPTURE_FILE"]
fn the_holds_smooth_mode_has_on_this_streams_arrivals() {
    let Ok(path) = std::env::var("OPENPS_CAPTURE_FILE") else { return };
    let arrivals: Vec<(u16, f64)> =
        load(&path).iter().filter(|r| r.kind == 0).map(|r| (r.id, r.at_us as f64 / 1e6)).collect();
    let span = arrivals.last().unwrap().1 - arrivals[0].1;
    for buffer in [false, true] {
        let mut tl = openps_metal::timeline::Timeline::default();
        tl.set_buffer(buffer);
        let (mut next, mut t, mut newest, mut hold_start) = (0usize, 0.0f64, None::<u16>, None::<f64>);
        let (mut holds, mut longest, mut total) = (vec![], 0.0f64, 0.0f64);
        let end = arrivals.last().unwrap().1 - 3.0; // the stream's pause at the end is not jitter
        while t < end {
            while next < arrivals.len() && arrivals[next].1 <= t {
                if tl.on_frame(arrivals[next].0, arrivals[next].1) {
                    newest = Some(arrivals[next].0);
                }
                next += 1;
            }
            if let (Some(id), true) = (newest, t > arrivals[0].1 + 4.0) {
                if t - tl.interval() - tl.playout() > tl.content_time(id).unwrap() + 1e-9 {
                    hold_start.get_or_insert(t);
                    total += 1.0 / 144.0;
                } else if let Some(s) = hold_start.take() {
                    holds.push(t - s);
                    longest = longest.max(t - s);
                }
            }
            t += 1.0 / 144.0;
        }
        let over = |ms: f64| holds.iter().filter(|h| **h * 1e3 >= ms).count();
        println!(
            "smoothing buffer {}: {} holds in {span:.0} s, longest {:.1} ms, {} of 12 ms or more, {} of 20 ms or more, held {:.1} % of the time, buffer now {:.1} ms (stream jitter {:.1} ms)",
            if buffer { "on " } else { "off" },
            holds.len(),
            longest * 1e3,
            over(12.0),
            over(20.0),
            100.0 * total / span,
            tl.playout() * 1e3,
            tl.stats().jitter * 1e3
        );
    }
}

/// The sequence a display shows in Smooth mode: the stream's pictures at `OPENPS_REPLAY_HZ` (default 144) output ticks a second, each with its
/// own position between two real pictures. Shimmer is the generated pictures differing from the real ones in character (softer, less grain)
/// so that detail pulses at the beat of the generated frames. Reports the detail (mean |Laplacian|) of pictures by how far they are from a
/// real one, and how much the detail changes from one picture to the next.
#[test]
#[ignore = "needs a capture: OPENPS_CAPTURE_FILE"]
fn the_detail_of_the_pictures_a_display_would_show_in_a_row() {
    let Ok(path) = std::env::var("OPENPS_CAPTURE_FILE") else { return };
    let hz: f64 = std::env::var("OPENPS_REPLAY_HZ").ok().and_then(|v| v.parse().ok()).unwrap_or(144.0);
    let range: Vec<usize> = std::env::var("OPENPS_REPLAY_RANGE")
        .unwrap_or("500,620".into())
        .split(',')
        .filter_map(|v| v.parse().ok())
        .collect();
    let frames = decode_all(&load(&path));
    let (w, h) = (frames[0].width(), frames[0].height());
    let mut g = Gpu::new().unwrap();
    g.fx_use = std::env::var_os("OPENPS_REPLAY_FX").is_some() && g.fx_available;
    let step = 60.0 / hz; // source pictures per output tick
    let (mut last_in, mut c) = (None::<usize>, range[0] as f64);
    let mut series: Vec<(f64, f64)> = vec![]; // (distance from the nearest real picture in source intervals, detail)
    let ingest = |g: &mut Gpu, last: &mut Option<usize>, k: usize| {
        if *last == Some(k) {
            return;
        }
        let tex = openps_metal::renderer::frame_to_texture(g, &frames[k]).unwrap();
        let cmd = g.command_buffer().unwrap();
        g.ingest(&cmd, tex, true).unwrap();
        cmd.commit();
        cmd.waitUntilCompleted();
        *last = Some(k);
    };
    g.reset_stream();
    while c < range[1] as f64 - 2.0 {
        let (j, beta) = (c.floor() as usize, c.fract() as f32);
        ingest(&mut g, &mut last_in, j);
        ingest(&mut g, &mut last_in, j + 1);
        let cmd = g.command_buffer().unwrap();
        let (t, _) = g.compose_interp(&cmd, beta, None).unwrap();
        cmd.commit();
        cmd.waitUntilCompleted();
        let px = to_bytes(&g, &t);
        let d = detail(&luma(&px), w, h).0;
        series.push((f64::from(beta.min(1.0 - beta)), d));
        c += step;
    }
    let mean = |v: &[f64]| v.iter().sum::<f64>() / v.len().max(1) as f64;
    let real: Vec<f64> = series.iter().filter(|s| s.0 < 0.05).map(|s| s.1).collect();
    println!("{} pictures at {hz} Hz from a 60 fps stream; real-like {} ", series.len(), real.len());
    let base = mean(&real).max(1e-9);
    for (lo, hi) in [(0.0, 0.05), (0.05, 0.15), (0.15, 0.3), (0.3, 0.41), (0.41, 0.51)] {
        let b: Vec<f64> = series.iter().filter(|s| s.0 >= lo && s.0 < hi).map(|s| s.1).collect();
        if !b.is_empty() {
            println!(
                "  {lo:.2}-{hi:.2} from a real picture: {:>4} pictures, detail {:.3} of the real pictures'",
                b.len(),
                mean(&b) / base
            );
        }
    }
    let flick: Vec<f64> = series.windows(2).map(|p| (p[1].1 - p[0].1).abs()).collect();
    println!(
        "detail change from one picture to the next: {:.2} % of the mean",
        100.0 * mean(&flick) / mean(&series.iter().map(|s| s.1).collect::<Vec<_>>())
    );
}
