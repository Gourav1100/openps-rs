#![cfg(target_os = "macos")]
//! By hand, on a capture made with `OPENPS_VIDEO_CAPTURE=<dir>`:
//! `OPENPS_CAPTURE_FILE=<file.opsvid> cargo test -p openps-video --release --test capture_analysis -- --ignored --nocapture`
//!
//! Reports the shape of the stream (keyframes, gaps, adaptive-stream changes, in-band parameter sets) and simulates losses: one
//! frame is dropped, the decoder keeps going WITHOUT a keyframe, and each picture is compared with the undamaged decode. A frame that
//! comes out identical never needed the dropped one, so it could have been shown instead of waiting for an IDR.
use openps_core::capture::{read, Kind};
use openps_video::nal::{self, Codec, ParameterSets};
use openps_video::vt::{Decoder, Frame};

struct Au {
    id: u16,
    adaptive: u8,
    at_us: u64,
    data: Vec<u8>,
    key: bool,
    has_sets: bool,
}

fn load(path: &str) -> (Codec, Vec<u8>, Vec<Au>) {
    let records = read(std::path::Path::new(path)).expect("capture file");
    let header: Vec<u8> =
        records.iter().filter(|r| r.kind == Kind::ParameterSets).flat_map(|r| r.data.clone()).collect();
    let first = records.iter().find(|r| r.kind == Kind::Frame).expect("no frames");
    let codec = nal::detect_codec(if header.is_empty() { &first.data } else { &header }).expect("codec");
    let aus = records
        .iter()
        .filter(|r| r.kind == Kind::Frame)
        .map(|r| {
            let a = nal::analyse(codec, &r.data);
            Au {
                id: r.id,
                adaptive: r.adaptive,
                at_us: r.at_us,
                data: r.data.clone(),
                key: a.keyframe,
                has_sets: !a.in_band.sps.is_empty() && !a.in_band.pps.is_empty(),
            }
        })
        .collect();
    (codec, header, aus)
}

fn decoder(codec: Codec, header: &[u8], aus: &[Au]) -> Decoder {
    let source = if header.is_empty() { &aus[0].data } else { header };
    Decoder::new(codec, ParameterSets::from_annex_b(codec, source)).expect("decoder")
}

fn pts(id: u16) -> i64 {
    i64::from(id) * 1500
}

fn mean_abs_diff(a: &Frame, b: &Frame) -> f64 {
    let (x, y) = (a.copy_planes(), b.copy_planes());
    if x.len() != y.len() {
        return f64::INFINITY;
    }
    x.iter().zip(&y).map(|(p, q)| f64::from(p.abs_diff(*q))).sum::<f64>() / x.len().max(1) as f64
}

#[test]
#[ignore = "needs a capture: OPENPS_CAPTURE_FILE"]
fn report() {
    let Ok(path) = std::env::var("OPENPS_CAPTURE_FILE") else { return };
    let (codec, header, aus) = load(&path);
    let secs = (aus.last().unwrap().at_us - aus[0].at_us) as f64 / 1e6;
    println!("codec {codec:?}, {} access units in {secs:.1} s ({:.1} fps)", aus.len(), aus.len() as f64 / secs);
    let total: usize = aus.iter().map(|a| a.data.len()).sum();
    println!("{:.1} Mbit/s average", total as f64 * 8.0 / secs / 1e6);
    let keys: Vec<usize> = (0..aus.len()).filter(|&i| aus[i].key).collect();
    println!("keyframes at positions {keys:?} (ids {:?})", keys.iter().map(|&i| aus[i].id).collect::<Vec<_>>());
    println!("access units carrying parameter sets in-band: {}", aus.iter().filter(|a| a.has_sets).count());
    let mut adaptive: Vec<u8> = aus.iter().map(|a| a.adaptive).collect();
    adaptive.dedup();
    println!("adaptive-stream indexes seen, in order: {adaptive:?}");
    for w in aus.windows(2) {
        if w[1].id != w[0].id.wrapping_add(1) {
            println!("GAP: frame {} -> {} at {:.3} s", w[0].id, w[1].id, w[1].at_us as f64 / 1e6);
        }
    }
    let sizes: Vec<usize> = aus.iter().map(|a| a.data.len()).collect();
    println!("size min/max {} / {} bytes", sizes.iter().min().unwrap(), sizes.iter().max().unwrap());

    // The undamaged decode, from the first keyframe.
    let Some(&start) = keys.first() else { return println!("no keyframe in the capture") };
    let mut full = decoder(codec, &header, &aus);
    let mut reference: Vec<Option<Frame>> = vec![];
    for a in &aus[start..] {
        reference.push(full.decode_annex_b(&a.data, pts(a.id)).ok().and_then(|mut f| f.pop()));
    }
    let decoded = reference.iter().filter(|f| f.is_some()).count();
    println!("undamaged decode: {decoded} of {} pictures", aus.len() - start);

    // Drop each sampled frame in turn and keep decoding without a keyframe.
    let span = aus.len() - start;
    let positions: Vec<usize> = (3..span.saturating_sub(12)).step_by((span / 25).max(1)).collect();
    let (mut safe_next, mut unsafe_next) = (0, 0);
    let mut recovered_within: Vec<usize> = vec![];
    for &n in &positions {
        let dropped = &aus[start + n];
        if dropped.key {
            continue;
        }
        let mut d = decoder(codec, &header, &aus);
        let mut diffs = vec![];
        for (i, a) in aus[start..].iter().enumerate().take(n + 12) {
            if i == n {
                continue;
            }
            let shown = d.decode_annex_b(&a.data, pts(a.id)).ok().and_then(|mut f| f.pop());
            if i > n {
                diffs.push(match (&shown, &reference[i]) {
                    (Some(s), Some(r)) => Some(mean_abs_diff(s, r)),
                    _ => None,
                });
            }
        }
        let first = diffs.first().copied().flatten();
        match first {
            Some(x) if x < 0.01 => safe_next += 1,
            _ => unsafe_next += 1,
        }
        if let Some(k) = diffs.iter().position(|x| x.is_some_and(|v| v < 0.01)) {
            recovered_within.push(k + 1);
        }
        println!(
            "drop #{n} (id {}, {} bytes): frames after it differ by {}",
            dropped.id,
            dropped.data.len(),
            diffs.iter().map(|x| x.map_or("err".to_string(), |v| format!("{v:.2}"))).collect::<Vec<_>>().join(" ")
        );
    }
    println!("RESULT: the frame right after a drop was identical to the undamaged one in {safe_next} cases, damaged in {unsafe_next}");
    println!("frames until a picture matched again, per drop: {recovered_within:?}");
}
