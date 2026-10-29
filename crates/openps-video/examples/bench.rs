//! `cargo run --release -p openps-video --example bench -- <file.annexb>`: decode throughput on a real stream.
use openps_video::nal::{self, ParameterSets};
use openps_video::vt::Decoder;
use std::time::Instant;

fn main() {
    let path = std::env::args().nth(1).expect("path to an Annex-B file");
    let s = std::fs::read(path).unwrap();
    let codec = nal::detect_codec(&s).expect("codec");
    let mut units: Vec<Vec<u8>> = vec![];
    let mut have = false;
    for n in nal::split(&s) {
        let k = nal::classify(codec, n);
        let pic = matches!(k, nal::Kind::Idr | nal::Kind::Slice);
        if units.is_empty() || (have && (pic || matches!(k, nal::Kind::Sps | nal::Kind::Vps | nal::Kind::Pps))) {
            units.push(vec![]);
            have = false;
        }
        have |= pic;
        units.last_mut().unwrap().extend([0, 0, 0, 1]);
        units.last_mut().unwrap().extend(n);
    }
    let mut dec = Decoder::new(codec, ParameterSets::from_annex_b(codec, &s)).unwrap();
    let mut times = vec![];
    let t0 = Instant::now();
    let mut pics = 0;
    for (i, u) in units.iter().enumerate() {
        let t = Instant::now();
        pics += dec.decode_annex_b(u, i as i64).unwrap().len();
        times.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    let total = t0.elapsed().as_secs_f64();
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p = |q: f64| times[((times.len() as f64 - 1.0) * q) as usize];
    println!(
        "{:?} {} frames in {:.2}s = {:.0} fps | per-frame ms p50 {:.2} p99 {:.2} max {:.2}",
        codec,
        pics,
        total,
        pics as f64 / total,
        p(0.5),
        p(0.99),
        times[times.len() - 1]
    );
}
