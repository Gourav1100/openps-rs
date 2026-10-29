//! Coarse timing probes (ignored by default): `cargo test --release -p openps-stream --test perf -- --ignored --nocapture`
use openps_input::keys::{Input, Key};
use openps_input::{load_defaults, InputSnapshot, Mapper};
use std::time::Instant;

#[test]
#[ignore]
fn fec_throughput() {
    let (k, m, block) = (20usize, 4usize, 1400usize);
    let data: Vec<Vec<u8>> = (0..k).map(|i| (0..block).map(|j| (i * 31 + j * 7) as u8).collect()).collect();
    let n = 50;
    let t = Instant::now();
    let mut parity = vec![];
    for _ in 0..n {
        parity = openps_fec::encode(&data, m).unwrap();
    }
    let enc = t.elapsed() / n;
    let all: Vec<Vec<u8>> = data.iter().cloned().chain(parity).collect();
    let t = Instant::now();
    for _ in 0..n {
        let mut blocks: Vec<Option<Vec<u8>>> = all.iter().cloned().map(Some).collect();
        for i in [1, 5, 9, 13] {
            blocks[i] = None;
        }
        openps_fec::decode(k, m, &mut blocks).unwrap();
    }
    let dec = t.elapsed() / n;
    println!("FEC k={k} m={m} block={block} ({} KB frame): encode {enc:?}, decode(4 lost) {dec:?}", k * block / 1024);
}

#[test]
#[ignore]
fn mapper_tick() {
    let mut m = Mapper::new(load_defaults().unwrap(), None).unwrap();
    let snap = InputSnapshot::new([Input::Key(Key::W), Input::Key(Key::D), Input::Key(Key::SPACE)], 3.0, -2.0);
    let n = 200_000;
    let t = Instant::now();
    for i in 0..n {
        std::hint::black_box(m.map(&snap, i as f64 * 0.002));
    }
    println!("mapper.map: {:?}/tick (budget at 500 Hz: 2 ms)", t.elapsed() / n);
}
