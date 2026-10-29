//! Key derivation for the session cipher and the registration cipher (port of `gy.a` / `gb.a/b/c`).
//! All arithmetic is on bytes with wrap-around, exactly like the Java `(byte)` casts.

use crate::tables::KeyTables;
use openps_crypto::RpCipher;

/// Protocol version string -> derivation family.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    /// PS5, version `1.0`.
    Ps5,
    /// PS4 firmware 8+, version `10.0`.
    Ps4,
    /// PS4 firmware 7 (`9.0`) and 5-6 (`8.0`).
    Legacy,
}

impl Family {
    pub fn from_version(v: &str) -> Family {
        match v {
            "1.0" => Family::Ps5,
            "10.0" => Family::Ps4,
            _ => Family::Legacy,
        }
    }
}

/// Session cipher from the console's `RP-Key` (16 bytes) and the `RP-Nonce` of the init response (16 bytes).
pub fn session_cipher(t: &KeyTables, version: &str, rp_key: &[u8; 16], nonce: &[u8; 16]) -> RpCipher {
    let (mut key, mut iv) = ([0u8; 16], [0u8; 16]);
    match Family::from_version(version) {
        Family::Ps5 => {
            let (ia, ib) = ((nonce[0] >> 3) as usize * 112, (nonce[7] >> 3) as usize * 112);
            for i in 0..16 {
                let n = nonce[i];
                iv[i] = n.wrapping_sub(45).wrapping_sub(i as u8) ^ t.u[ia + i];
                key[i] = rp_key[i].wrapping_add(24).wrapping_add(i as u8) ^ n ^ t.v[ib + i];
            }
            RpCipher::new(key, iv, t.k)
        }
        Family::Ps4 => {
            let (ia, ib) = ((nonce[0] >> 3) as usize * 112, (nonce[7] >> 3) as usize * 112);
            for i in 0..16 {
                let n = nonce[i];
                iv[i] = n.wrapping_add(54).wrapping_add(i as u8) ^ t.r[ia + i];
                key[i] = (t.s[ib + i] ^ rp_key[i]).wrapping_add(33).wrapping_add(i as u8) ^ n;
            }
            RpCipher::new(key, iv, t.l)
        }
        Family::Legacy => {
            for i in 0..16 {
                let n = nonce[i];
                key[i] = n ^ t.a[i] ^ ((rp_key[i] as u32 + 52 - i as u32) & 0xFF) as u8;
                iv[i] = t.p[i] ^ ((n as u32 + 512 - i as u32 - 39) & 0xFF) as u8;
            }
            RpCipher::new(key, iv, t.m)
        }
    }
}

/// Registration cipher from 16 random bytes (used as the nonce), the 8-digit PIN as an integer, and
/// `idx = rand480[397] & 31` (ignored for the legacy family).
pub fn regist_cipher(t: &KeyTables, family: Family, rand16: &[u8; 16], pin: u32, idx: usize) -> RpCipher {
    let mut key = [0u8; 16];
    match family {
        Family::Ps5 | Family::Ps4 => {
            let table = if family == Family::Ps5 { &t.t } else { &t.q };
            for (i, k) in key.iter_mut().enumerate() {
                *k = table[(i << 5) + idx];
            }
            for (k, p) in key[12..].iter_mut().zip(pin.to_be_bytes()) {
                *k ^= p;
            }
            RpCipher::new(key, *rand16, if family == Family::Ps5 { t.k } else { t.l })
        }
        Family::Legacy => {
            let mut x = [0u8; 16];
            x[..4].copy_from_slice(&pin.to_be_bytes());
            for i in 0..16 {
                key[i] = t.o[i] ^ x[i];
            }
            RpCipher::new(key, *rand16, t.m)
        }
    }
}

/// The 16 bytes the registration request hides in its body so the console can recover the nonce:
/// derived from the cipher nonce and `idx_hi = rand480[0] >> 3`.
pub fn regist_patch(t: &KeyTables, family: Family, ps5_device: bool, nonce: &[u8; 16], idx_hi: usize) -> [u8; 16] {
    let mut out = [0u8; 16];
    for i in 0..16 {
        out[i] = match family {
            Family::Ps5 | Family::Ps4 if ps5_device => {
                (nonce[i] ^ t.c[(i << 5) + idx_hi]).wrapping_sub(45).wrapping_add(i as u8)
            }
            Family::Ps5 | Family::Ps4 => (nonce[i] ^ t.b[(i << 5) + idx_hi]).wrapping_add(41).wrapping_add(i as u8),
            Family::Legacy => t.a[i] ^ ((512 + nonce[i] as u32 - i as u32 - 41) & 0xFF) as u8,
        };
    }
    out
}

#[cfg(test)]
pub(crate) mod testutil {
    use crate::tables::KeyTables;

    /// Real tables are not in the repository. Tests that need them look at `$OPENPS_TABLES`, then at the
    /// out-of-tree default `analysis/tables/tables.json`, and skip (loudly) if neither exists.
    pub fn tables() -> Option<KeyTables> {
        if let Some(r) = KeyTables::from_env() {
            return Some(r.expect("OPENPS_TABLES is set but invalid"));
        }
        let default = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../analysis/tables/tables.json");
        match KeyTables::load(&default) {
            Ok(t) => Some(t),
            Err(_) => {
                eprintln!("SKIPPED: key tables not available (run extract-tables, set OPENPS_TABLES)");
                None
            }
        }
    }

    pub fn seq(mul: u8, add: u8) -> [u8; 16] {
        let mut b = [0u8; 16];
        for (i, x) in b.iter_mut().enumerate() {
            *x = (i as u8).wrapping_mul(mul).wrapping_add(add);
        }
        b
    }

    pub fn hex(b: &[u8]) -> String {
        crate::tables::to_hex(b)
    }

    pub fn plaintext() -> Vec<u8> {
        (0..40u8).map(|i| i.wrapping_mul(5).wrapping_add(1)).collect()
    }

    pub fn enc(c: &openps_crypto::RpCipher, ctr: u32) -> String {
        let mut d = plaintext();
        c.encrypt(&mut d, ctr);
        hex(&d)
    }
}

#[cfg(test)]
mod tests {
    use super::testutil::*;
    use super::*;

    // Known-answer vectors produced by running the ORIGINAL application's classes (gy.a / gb.a / gb.b / gb.c) on
    // fixed inputs: rp_key = seq(7,3), nonce = seq(11,200), rand16 = seq(13,77), pin = 12345678, idx = 21,
    // plaintext[i] = i*5+1 (40 bytes).
    const SESSION_PS5_IV: &str = "c3b16a1b1a22fa7c7c02135ce4336b3e";
    const SESSION_PS5_ENC0: &str = "c1df2d07b4a3434f2f8cdcab9c82aef5317851210944a31d451e9aa8f5bc0cb4a6a0b55b2aea2fcf";
    const SESSION_PS5_ENC3: &str = "d7d958a9342216bbf4d6583f42b8b3ecaf8fcf4bc5d0428134ac1cc4db28e2d0f7f9d3b8edc1574f";
    const SESSION_PS4_IV: &str = "4a2d6e09e32c56d3ef33886695f8777d";
    const SESSION_PS4_ENC0: &str = "c5e9646971c73160d277d28f51b14ef0f5ed9d66313d3b81a2cc26799545533a4a1a69506f213fa3";
    const SESSION_PS4_ENC3: &str = "0f135babb8d9952657f3065d19814818a58a5219cac1bd90786d51ad41b2060edd54bd47a0ed825b";
    const SESSION_LEGACY_IV: &str = "a0e23224acea56debac18847da29c266";
    const SESSION_LEGACY_ENC0: &str =
        "7edb9500b9eb6a75f1f81165abb6200e154ab8ed2a40162e7c3d629e33fd5e3fb43bbfbe8d994e2d";
    const SESSION_LEGACY_ENC3: &str =
        "9ad220120db95662ff7b9b2856c1b69d34a277babc6587638065b2819ec775e929eef8139c34a7e7";
    const REG_NONCE: &str = "4d5a6774818e9ba8b5c2cfdce9f60310";
    const REG_PS5_ENC0: &str = "db8d5eab67e1853f36a08a1a37d49685c2a7992b502a85998f1c1ca269d12264f15bc09a8348494c";
    const REG_PS4_ENC0: &str = "fff5298e819c60103dc41abd1ee0075ff9a9a53bdd1c29287aa9c76f6023ca7c7397a4c1276717c4";
    const REG_LEGACY_ENC0: &str = "1657156db27e8e30ce2dbaca0a4a6f48bb7ae4b5e490136d28a4cac73849ba322a17b9435a59597a";

    #[test]
    fn session_ciphers_match_the_original_application() {
        let Some(t) = tables() else { return };
        let (rp_key, nonce) = (seq(7, 3), seq(11, 200));
        for (v, iv, e0, e3) in [
            ("1.0", SESSION_PS5_IV, SESSION_PS5_ENC0, SESSION_PS5_ENC3),
            ("10.0", SESSION_PS4_IV, SESSION_PS4_ENC0, SESSION_PS4_ENC3),
            ("9.0", SESSION_LEGACY_IV, SESSION_LEGACY_ENC0, SESSION_LEGACY_ENC3),
            ("8.0", SESSION_LEGACY_IV, SESSION_LEGACY_ENC0, SESSION_LEGACY_ENC3), // 8.0 shares the legacy derivation
        ] {
            let c = session_cipher(&t, v, &rp_key, &nonce);
            assert_eq!(hex(c.nonce()), iv, "{v} iv");
            assert_eq!(enc(&c, 0), e0, "{v} counter 0");
            assert_eq!(enc(&c, 3), e3, "{v} counter 3");
        }
    }

    #[test]
    fn registration_ciphers_match_the_original_application() {
        let Some(t) = tables() else { return };
        let rand16 = seq(13, 77);
        for (family, e0) in
            [(Family::Ps5, REG_PS5_ENC0), (Family::Ps4, REG_PS4_ENC0), (Family::Legacy, REG_LEGACY_ENC0)]
        {
            let c = regist_cipher(&t, family, &rand16, 12_345_678, 21);
            assert_eq!(hex(c.nonce()), REG_NONCE);
            assert_eq!(enc(&c, 0), e0, "{family:?}");
        }
    }

    #[test]
    fn pin_and_index_change_the_registration_key() {
        let Some(t) = tables() else { return };
        let r = seq(13, 77);
        let base = enc(&regist_cipher(&t, Family::Ps5, &r, 12_345_678, 21), 0);
        assert_ne!(base, enc(&regist_cipher(&t, Family::Ps5, &r, 12_345_679, 21), 0));
        assert_ne!(base, enc(&regist_cipher(&t, Family::Ps5, &r, 12_345_678, 22), 0));
        // the legacy family ignores idx
        assert_eq!(
            enc(&regist_cipher(&t, Family::Legacy, &r, 1, 3), 0),
            enc(&regist_cipher(&t, Family::Legacy, &r, 1, 9), 0)
        );
    }

    #[test]
    fn patch_bytes_are_deterministic_and_depend_on_the_device() {
        let Some(t) = tables() else { return };
        let n = seq(13, 77);
        let a = regist_patch(&t, Family::Ps5, true, &n, 5);
        assert_eq!(a, regist_patch(&t, Family::Ps5, true, &n, 5));
        assert_ne!(a, regist_patch(&t, Family::Ps4, false, &n, 5));
        assert_ne!(a, regist_patch(&t, Family::Ps5, true, &n, 6));
        assert_ne!(regist_patch(&t, Family::Legacy, false, &n, 0), a);
    }

    #[test]
    fn family_selection() {
        assert_eq!(Family::from_version("1.0"), Family::Ps5);
        assert_eq!(Family::from_version("10.0"), Family::Ps4);
        assert_eq!(Family::from_version("9.0"), Family::Legacy);
        assert_eq!(Family::from_version("8.0"), Family::Legacy);
    }
}
