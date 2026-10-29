//! Tester A1: adversarial, boundary and property-style tests for the Cauchy RS codec.
use openps_fec::{cauchy_matrix, decode, encode, FecError};
use std::time::Instant;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
    fn block(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.next() as u8).collect()
    }
}

#[test]
fn parameter_validation_boundaries() {
    assert_eq!(cauchy_matrix(0, 1), Err(FecError::BadParams));
    assert_eq!(cauchy_matrix(1, 0), Err(FecError::BadParams));
    assert!(cauchy_matrix(255, 1).is_ok());
    assert!(cauchy_matrix(1, 255).is_ok());
    assert!(cauchy_matrix(128, 128).is_ok()); // k + m == 256 is the maximum for w = 8
    assert_eq!(cauchy_matrix(129, 128), Err(FecError::BadParams));
}

#[test]
fn k_plus_m_overflow_does_not_panic() {
    // k + m would overflow usize; in debug builds this must not panic.
    let r = std::panic::catch_unwind(|| cauchy_matrix(usize::MAX, usize::MAX));
    assert!(r.is_ok(), "cauchy_matrix panicked on usize::MAX parameters (arithmetic overflow)");
}

#[test]
fn encode_empty_input_is_an_error_not_a_panic() {
    assert_eq!(encode(&[], 2), Err(FecError::BadParams));
}

#[test]
fn encode_rejects_unequal_blocks() {
    let d = vec![vec![1u8; 8], vec![2u8; 9]];
    assert_eq!(encode(&d, 1), Err(FecError::SizeMismatch));
}

#[test]
fn zero_length_blocks_roundtrip() {
    let d = vec![vec![], vec![]];
    let p = encode(&d, 2).unwrap();
    assert!(p.iter().all(|b| b.is_empty()));
    let mut blocks = vec![None, None, Some(vec![]), Some(vec![])];
    decode(2, 2, &mut blocks).unwrap();
    assert!(blocks[0].as_ref().unwrap().is_empty());
}

#[test]
fn decode_validates_shape_and_sizes() {
    let d = vec![vec![1u8; 4]; 3];
    let p = encode(&d, 2).unwrap();
    // wrong slice length
    let mut b: Vec<Option<Vec<u8>>> = vec![None; 4];
    assert_eq!(decode(3, 2, &mut b), Err(FecError::BadParams));
    // mismatched survivor sizes
    let mut b: Vec<Option<Vec<u8>>> = vec![None, Some(vec![1; 4]), Some(vec![1; 5]), Some(p[0].clone()), None];
    assert_eq!(decode(3, 2, &mut b), Err(FecError::SizeMismatch));
    // k = 0
    let mut b: Vec<Option<Vec<u8>>> = vec![Some(vec![1])];
    assert_eq!(decode(0, 1, &mut b), Err(FecError::BadParams));
    // nothing present
    let mut b: Vec<Option<Vec<u8>>> = vec![None; 5];
    assert_eq!(decode(3, 2, &mut b), Err(FecError::TooManyErasures));
}

#[test]
fn decode_failure_leaves_input_untouched() {
    let d = vec![vec![9u8; 4]; 3];
    let p = encode(&d, 1).unwrap();
    let mut b: Vec<Option<Vec<u8>>> = vec![None, None, Some(d[2].clone()), Some(p[0].clone())];
    let before = b.clone();
    assert_eq!(decode(3, 1, &mut b), Err(FecError::TooManyErasures));
    assert_eq!(b, before);
}

#[test]
fn recovered_coding_blocks_are_not_reconstructed() {
    let d = vec![vec![1u8, 2, 3], vec![4, 5, 6]];
    let p = encode(&d, 2).unwrap();
    let mut b = vec![None, Some(d[1].clone()), None, Some(p[1].clone())];
    decode(2, 2, &mut b).unwrap();
    assert_eq!(b[0].as_deref(), Some(&d[0][..]));
    assert!(b[2].is_none(), "documented: lost coding blocks stay None");
}

/// Random (k, m, block size, erasure set) incl. the extreme k + m == 256 and survivors that are mostly parity.
#[test]
fn property_random_roundtrip() {
    let mut r = Rng(0x1234_5678_9abc_def1);
    for iter in 0..300 {
        let (k, m) = match iter % 6 {
            0 => (1 + r.below(10), 1 + r.below(6)),
            1 => (1, 1 + r.below(255)),
            2 => (1 + r.below(255), 1),
            3 => (128, 128),
            4 => (200, 56),
            _ => (1 + r.below(60), 1 + r.below(60)),
        };
        let size = r.below(40);
        let data: Vec<Vec<u8>> = (0..k).map(|_| r.block(size)).collect();
        let parity = encode(&data, m).unwrap();
        let mut all: Vec<Option<Vec<u8>>> = data.iter().cloned().chain(parity).map(Some).collect();
        // erase up to m random distinct blocks, biased to erase data blocks
        let e = r.below(m.min(k + m) + 1);
        let mut erased = 0;
        while erased < e {
            let i = if r.below(4) == 0 { r.below(k + m) } else { r.below(k) };
            if all[i].is_some() {
                all[i] = None;
                erased += 1;
            }
        }
        decode(k, m, &mut all).unwrap_or_else(|e| panic!("k={k} m={m} erased={erased}: {e:?}"));
        for i in 0..k {
            assert_eq!(all[i].as_ref().unwrap(), &data[i], "k={k} m={m} block {i}");
        }
    }
}

/// Every possible survivor subset of size exactly k for a (4, 4) code (MDS property).
#[test]
fn mds_property_every_k_subset_decodes() {
    let (k, m) = (4usize, 4usize);
    let mut r = Rng(77);
    let data: Vec<Vec<u8>> = (0..k).map(|_| r.block(33)).collect();
    let parity = encode(&data, m).unwrap();
    let all: Vec<Vec<u8>> = data.iter().cloned().chain(parity).collect();
    let mut n = 0;
    for mask in 0u32..(1 << (k + m)) {
        if mask.count_ones() as usize != k {
            continue;
        }
        let mut b: Vec<Option<Vec<u8>>> =
            all.iter().enumerate().map(|(i, x)| if mask >> i & 1 == 1 { Some(x.clone()) } else { None }).collect();
        decode(k, m, &mut b).unwrap();
        for i in 0..k {
            assert_eq!(b[i].as_ref().unwrap(), &data[i], "mask {mask:08b}");
        }
        n += 1;
    }
    assert_eq!(n, 70);
}

#[test]
fn corrupted_parity_is_not_detected_but_does_not_panic() {
    // FEC has no integrity check: documents that a flipped parity byte silently yields wrong data.
    let d = vec![vec![1u8; 16], vec![2u8; 16]];
    let mut p = encode(&d, 1).unwrap();
    p[0][3] ^= 0x55;
    let mut b = vec![None, Some(d[1].clone()), Some(p[0].clone())];
    decode(2, 1, &mut b).unwrap();
    assert_ne!(b[0].as_ref().unwrap(), &d[0]);
}

/// Timing probe (prints; generous bounds so it only fails on pathological regressions). Run with --release.
#[test]
fn perf_encode_decode() {
    let mut r = Rng(5);
    for &(k, m, block) in &[(20usize, 4usize, 1400usize), (100, 20, 1400), (200, 56, 1400)] {
        let data: Vec<Vec<u8>> = (0..k).map(|_| r.block(block)).collect();
        let t = Instant::now();
        let parity = encode(&data, m).unwrap();
        let enc = t.elapsed();
        let mut b: Vec<Option<Vec<u8>>> = data.iter().cloned().chain(parity).map(Some).collect();
        for i in 0..m.min(k) {
            b[i * 2 % k] = None;
        }
        let t = Instant::now();
        decode(k, m, &mut b).unwrap();
        let dec = t.elapsed();
        let t = Instant::now();
        let _ = cauchy_matrix(k, m).unwrap();
        let mat = t.elapsed();
        println!("FEC k={k} m={m} block={block}: encode {enc:?} decode {dec:?} (matrix build {mat:?})");
        if !cfg!(debug_assertions) {
            assert!(dec.as_millis() < 500, "decode too slow: {dec:?}");
        }
    }
}
