//! Cauchy Reed-Solomon over GF(2^8), poly 0x11D, matching jerasure's
//! `cauchy_original_coding_matrix(k, m, 8)` + `jerasure_matrix_decode`.
//! See analysis/spec/03-native-components.md. Polynomial 0x11D is the gf-complete
//! default for w=8 and must be confirmed once against a jerasure test vector.

const POLY: u16 = 0x11D;

/// GF(2^8) tables, built once: log/exp for inversion and a full 256x256 product table so the hot loops are
/// a single indexed load per byte (no branches, no log/exp round trip).
struct Gf {
    exp: [u8; 512],
    log: [u8; 256],
    mul: Box<[[u8; 256]; 256]>,
}

fn gf() -> &'static Gf {
    static GF: std::sync::OnceLock<Gf> = std::sync::OnceLock::new();
    GF.get_or_init(|| {
        let (mut exp, mut log) = ([0u8; 512], [0u8; 256]);
        let mut x: u16 = 1;
        for (i, e) in exp.iter_mut().enumerate().take(255) {
            *e = x as u8;
            log[x as usize] = i as u8;
            x <<= 1;
            if x & 0x100 != 0 {
                x ^= POLY;
            }
        }
        for i in 255..512 {
            exp[i] = exp[i - 255];
        }
        let mut mul = Box::new([[0u8; 256]; 256]);
        for a in 1..256usize {
            for b in 1..256usize {
                mul[a][b] = exp[log[a] as usize + log[b] as usize];
            }
        }
        Gf { exp, log, mul }
    })
}

impl Gf {
    #[inline]
    fn mul(&self, a: u8, b: u8) -> u8 {
        self.mul[a as usize][b as usize]
    }
    fn inv(&self, a: u8) -> u8 {
        assert!(a != 0);
        self.exp[255 - self.log[a as usize] as usize]
    }
    /// `dst ^= coef * src` over a whole block.
    #[inline]
    fn mul_acc(&self, dst: &mut [u8], src: &[u8], coef: u8) {
        match coef {
            0 => {}
            1 => dst.iter_mut().zip(src).for_each(|(d, s)| *d ^= s),
            _ => {
                let row = &self.mul[coef as usize];
                dst.iter_mut().zip(src).for_each(|(d, s)| *d ^= row[*s as usize]);
            }
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum FecError {
    BadParams,
    TooManyErasures,
    SizeMismatch,
}

/// m x k coding matrix: M[i][j] = 1 / (i XOR (m + j)).
pub fn cauchy_matrix(k: usize, m: usize) -> Result<Vec<Vec<u8>>, FecError> {
    if k == 0 || m == 0 || k + m > 256 {
        return Err(FecError::BadParams);
    }
    let gf = gf();
    Ok((0..m).map(|i| (0..k).map(|j| gf.inv((i ^ (m + j)) as u8)).collect()).collect())
}

/// Compute `m` coding blocks from `k` equal-sized data blocks.
pub fn encode(data: &[Vec<u8>], m: usize) -> Result<Vec<Vec<u8>>, FecError> {
    let k = data.len();
    let mat = cauchy_matrix(k, m)?;
    let size = data[0].len();
    if data.iter().any(|d| d.len() != size) {
        return Err(FecError::SizeMismatch);
    }
    let gf = gf();
    Ok(mat
        .iter()
        .map(|row| {
            let mut out = vec![0u8; size];
            for (j, d) in data.iter().enumerate() {
                gf.mul_acc(&mut out, d, row[j]);
            }
            out
        })
        .collect())
}

/// Recover missing data blocks in place. `blocks` has k data then m coding blocks;
/// `None` marks an erasure. On success every data block (index < k) is `Some`.
/// Like the native lib, recovered coding blocks are not reconstructed.
pub fn decode(k: usize, m: usize, blocks: &mut [Option<Vec<u8>>]) -> Result<(), FecError> {
    if blocks.len() != k + m {
        return Err(FecError::BadParams);
    }
    let mat = cauchy_matrix(k, m)?;
    let present: Vec<usize> = (0..k + m).filter(|&i| blocks[i].is_some()).collect();
    if present.len() < k {
        return Err(FecError::TooManyErasures);
    }
    let size = blocks[present[0]].as_ref().unwrap().len();
    if present.iter().any(|&i| blocks[i].as_ref().unwrap().len() != size) {
        return Err(FecError::SizeMismatch);
    }
    if (0..k).all(|i| blocks[i].is_some()) {
        return Ok(());
    }
    let gf = gf();
    let rows = &present[..k];
    // Generator rows for chosen survivors: identity for data, matrix row for coding.
    let mut a: Vec<Vec<u8>> = rows
        .iter()
        .map(|&r| if r < k { (0..k).map(|j| (j == r) as u8).collect() } else { mat[r - k].clone() })
        .collect();
    // Gauss-Jordan inverse.
    let mut inv: Vec<Vec<u8>> = (0..k).map(|i| (0..k).map(|j| (i == j) as u8).collect()).collect();
    for c in 0..k {
        let p = (c..k).find(|&r| a[r][c] != 0).ok_or(FecError::TooManyErasures)?;
        a.swap(c, p);
        inv.swap(c, p);
        let iv = gf.inv(a[c][c]);
        for j in 0..k {
            a[c][j] = gf.mul(a[c][j], iv);
            inv[c][j] = gf.mul(inv[c][j], iv);
        }
        for r in 0..k {
            if r != c && a[r][c] != 0 {
                let f = a[r][c];
                for j in 0..k {
                    let (x, y) = (gf.mul(f, a[c][j]), gf.mul(f, inv[c][j]));
                    a[r][j] ^= x;
                    inv[r][j] ^= y;
                }
            }
        }
    }
    for d in 0..k {
        if blocks[d].is_some() {
            continue;
        }
        let mut out = vec![0u8; size];
        for (s, &r) in rows.iter().enumerate() {
            let coef = inv[d][s];
            if coef == 0 {
                continue;
            }
            gf.mul_acc(&mut out, blocks[r].as_ref().unwrap(), coef);
        }
        blocks[d] = Some(out);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(k: usize, size: usize) -> Vec<Vec<u8>> {
        (0..k).map(|i| (0..size).map(|j| (i * 31 + j * 7 + 3) as u8).collect()).collect()
    }

    #[test]
    fn matrix_first_entries() {
        // M[0][0] = 1/(0 xor m) ; with m=1 -> 1/1 = 1
        assert_eq!(cauchy_matrix(2, 1).unwrap()[0][0], 1);
    }

    #[test]
    fn roundtrip_every_erasure_pattern() {
        let (k, m) = (6, 3);
        let data = sample(k, 64);
        let parity = encode(&data, m).unwrap();
        let all: Vec<Vec<u8>> = data.iter().cloned().chain(parity).collect();
        for mask in 0u32..(1 << (k + m)) {
            if mask.count_ones() as usize > m {
                continue;
            }
            let mut blocks: Vec<Option<Vec<u8>>> =
                all.iter().enumerate().map(|(i, b)| if mask >> i & 1 == 1 { None } else { Some(b.clone()) }).collect();
            decode(k, m, &mut blocks).unwrap();
            for i in 0..k {
                assert_eq!(blocks[i].as_ref().unwrap(), &data[i], "mask {mask:b}");
            }
        }
    }

    #[test]
    fn too_many_erasures_fails() {
        let data = sample(4, 8);
        let parity = encode(&data, 1).unwrap();
        let mut blocks: Vec<Option<Vec<u8>>> =
            vec![None, None, Some(data[2].clone()), Some(data[3].clone()), Some(parity[0].clone())];
        assert_eq!(decode(4, 1, &mut blocks), Err(FecError::TooManyErasures));
    }
}
