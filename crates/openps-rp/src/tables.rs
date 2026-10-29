//! The constant tables the key derivation needs. They are Sony protocol constants embedded in the original
//! application; this repository does not contain them. Generate a `tables.json` once from your own copy with
//! `extract-tables` (see `src/bin/extract-tables.rs`) and point `OPENPS_TABLES` at it, or load it from your
//! application's data directory.

use serde::{Deserialize, Serialize};

pub const SMALL: usize = 16;
pub const MEDIUM: usize = 512;
pub const LARGE: usize = 3584;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyTables {
    pub a: [u8; SMALL],
    pub k: [u8; SMALL],
    pub l: [u8; SMALL],
    pub m: [u8; SMALL],
    pub o: [u8; SMALL],
    pub p: [u8; SMALL],
    #[serde(with = "hexvec")]
    pub q: Vec<u8>,
    #[serde(with = "hexvec")]
    pub b: Vec<u8>,
    #[serde(with = "hexvec")]
    pub t: Vec<u8>,
    #[serde(with = "hexvec")]
    pub c: Vec<u8>,
    #[serde(with = "hexvec")]
    pub r: Vec<u8>,
    #[serde(with = "hexvec")]
    pub s: Vec<u8>,
    #[serde(with = "hexvec")]
    pub u: Vec<u8>,
    #[serde(with = "hexvec")]
    pub v: Vec<u8>,
}

/// Sony protocol constants must never reach a log, a panic message or a crash report, so `Debug` shows only sizes.
impl std::fmt::Debug for KeyTables {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "KeyTables(<redacted: {} bytes of protocol constants>)",
            6 * SMALL
                + self.q.len()
                + self.b.len()
                + self.t.len()
                + self.c.len()
                + self.r.len()
                + self.s.len()
                + self.u.len()
                + self.v.len()
        )
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum TableError {
    Missing(&'static str),
    BadLength { name: &'static str, expected: usize, got: usize },
    Json(String),
}

impl std::fmt::Display for TableError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TableError::Missing(n) => write!(f, "table {n} not found"),
            TableError::BadLength { name, expected, got } => {
                write!(f, "table {name}: expected {expected} bytes, got {got}")
            }
            TableError::Json(e) => write!(f, "invalid tables file: {e}"),
        }
    }
}
impl std::error::Error for TableError {}

pub fn to_hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub fn from_hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) || !s.is_ascii() {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok()).collect()
}

mod hexvec {
    use super::{from_hex, to_hex};
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(v: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&to_hex(v))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        from_hex(&s).ok_or_else(|| serde::de::Error::custom("bad hex"))
    }
}

impl KeyTables {
    /// Check every table has the length the derivations index into.
    pub fn validate(&self) -> Result<(), TableError> {
        for (name, t, expected) in [
            ("q", &self.q, MEDIUM),
            ("b", &self.b, MEDIUM),
            ("t", &self.t, MEDIUM),
            ("c", &self.c, MEDIUM),
            ("r", &self.r, LARGE),
            ("s", &self.s, LARGE),
            ("u", &self.u, LARGE),
            ("v", &self.v, LARGE),
        ] {
            if t.len() != expected {
                return Err(TableError::BadLength { name, expected, got: t.len() });
            }
        }
        Ok(())
    }

    pub fn from_json(text: &str) -> Result<Self, TableError> {
        let t: KeyTables = serde_json::from_str(text).map_err(|e| TableError::Json(e.to_string()))?;
        t.validate()?;
        Ok(t)
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("serialisable")
    }

    pub fn load(path: &std::path::Path) -> Result<Self, TableError> {
        Self::from_json(&std::fs::read_to_string(path).map_err(|e| TableError::Json(e.to_string()))?)
    }

    /// Where tables are looked up: `$OPENPS_TABLES`, else `None`.
    pub fn from_env() -> Option<Result<Self, TableError>> {
        std::env::var_os("OPENPS_TABLES").map(|p| Self::load(std::path::Path::new(&p)))
    }

    /// Extract the tables from decompiled source of the original `gb` class: fields of the form
    /// `static final byte[] name = HexUtil.unhexlify("..." [+ "..."])`. The 16-byte constant `n` aliases `a`.
    pub fn from_gb_source(src: &str) -> Result<Self, TableError> {
        let mut found: std::collections::HashMap<String, Vec<u8>> = Default::default();
        let marker = "static final byte[] ";
        let mut rest = src;
        while let Some(i) = rest.find(marker) {
            rest = &rest[i + marker.len()..];
            let name: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
            let after = &rest[name.len()..];
            let Some(call) = after.trim_start().strip_prefix("= HexUtil.unhexlify(") else { continue };
            // collect adjacent string literals up to the closing parenthesis
            let mut hex = String::new();
            let mut in_str = false;
            for c in call.chars() {
                match (in_str, c) {
                    (false, '"') => in_str = true,
                    (true, '"') => in_str = false,
                    (true, c) => hex.push(c),
                    (false, ')') => break,
                    _ => {}
                }
            }
            if let Some(bytes) = from_hex(&hex) {
                found.insert(name, bytes);
            }
        }
        let small = |n: &'static str| -> Result<[u8; SMALL], TableError> {
            let v = found.get(n).ok_or(TableError::Missing(n))?;
            v.as_slice().try_into().map_err(|_| TableError::BadLength { name: n, expected: SMALL, got: v.len() })
        };
        let big = |n: &'static str| found.get(n).cloned().ok_or(TableError::Missing(n));
        let t = KeyTables {
            a: small("a")?,
            k: small("k")?,
            l: small("l")?,
            m: small("m")?,
            o: small("o")?,
            p: small("p")?,
            q: big("q")?,
            b: big("b")?,
            t: big("t")?,
            c: big("c")?,
            r: big("r")?,
            s: big("s")?,
            u: big("u")?,
            v: big("v")?,
        };
        t.validate()?;
        Ok(t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake() -> KeyTables {
        let f = |n: usize, seed: u8| (0..n).map(|i| (i as u8).wrapping_mul(3).wrapping_add(seed)).collect::<Vec<u8>>();
        let s = |seed| -> [u8; 16] { f(16, seed).try_into().unwrap() };
        KeyTables {
            a: s(1),
            k: s(2),
            l: s(3),
            m: s(4),
            o: s(5),
            p: s(6),
            q: f(512, 7),
            b: f(512, 8),
            t: f(512, 9),
            c: f(512, 10),
            r: f(3584, 11),
            s: f(3584, 12),
            u: f(3584, 13),
            v: f(3584, 14),
        }
    }

    #[test]
    fn json_roundtrip_and_validation() {
        let t = fake();
        assert_eq!(KeyTables::from_json(&t.to_json()).unwrap(), t);
        let mut bad = t.clone();
        bad.q.pop();
        assert_eq!(
            KeyTables::from_json(&bad.to_json()),
            Err(TableError::BadLength { name: "q", expected: 512, got: 511 })
        );
        assert!(matches!(KeyTables::from_json("{"), Err(TableError::Json(_))));
    }

    #[test]
    fn extraction_from_decompiled_source_shape() {
        let t = fake();
        let field = |name: &str, v: &[u8]| {
            let h = to_hex(v);
            // split into two literals joined with + to exercise multi-literal parsing
            let (a, b) = h.split_at(h.len() / 2);
            format!("   private static final byte[] {name} = HexUtil.unhexlify(\n      \"{a}\" + \"{b}\"\n   );\n")
        };
        let mut src = String::from("public final class gb {\n   private static final byte[] g = new byte[2000];\n");
        for (n, v) in [
            ("a", &t.a[..]),
            ("k", &t.k),
            ("l", &t.l),
            ("m", &t.m),
            ("o", &t.o),
            ("p", &t.p),
            ("q", &t.q),
            ("b", &t.b),
            ("t", &t.t),
            ("c", &t.c),
            ("r", &t.r),
            ("s", &t.s),
            ("u", &t.u),
            ("v", &t.v),
        ] {
            src += &field(n, v);
        }
        src += "   private static final byte[] n = a;\n}";
        assert_eq!(KeyTables::from_gb_source(&src).unwrap(), t);
        assert_eq!(KeyTables::from_gb_source("class gb {}"), Err(TableError::Missing("a")));
    }

    #[test]
    fn hex_helpers() {
        assert_eq!(from_hex("00ff10"), Some(vec![0, 255, 16]));
        assert_eq!(from_hex("0"), None);
        assert_eq!(from_hex("zz"), None);
        assert_eq!(to_hex(&[1, 171]), "01ab");
    }
}
