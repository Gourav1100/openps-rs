//! ECDH for the Big/Bang exchange. Curve by negotiated Takion version: > 12 -> secp521r1, else secp256k1.
//! The shared secret reproduces the Java `BigInteger.toByteArray()` quirks literally (spec 02 R7).

use hmac::{Hmac, Mac};
use k256::elliptic_curve::sec1::ToEncodedPoint;
use rand_core::OsRng;
use sha2::Sha256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Curve {
    K256,
    P521,
}

pub fn curve_for(version: i32) -> Curve {
    if version > 12 {
        Curve::P521
    } else {
        Curve::K256
    }
}

#[derive(Debug)]
pub enum EcdhError {
    BadKey,
    BadPeer,
}

enum Secret {
    K(k256::SecretKey),
    P(p521::SecretKey),
}

pub struct Keypair {
    curve: Curve,
    secret: Secret,
}

impl Keypair {
    pub fn generate(curve: Curve) -> Self {
        let secret = match curve {
            Curve::K256 => Secret::K(k256::SecretKey::random(&mut OsRng)),
            Curve::P521 => Secret::P(p521::SecretKey::random(&mut OsRng)),
        };
        Self { curve, secret }
    }

    /// Deterministic key from raw secret bytes (tests, replay).
    pub fn from_secret(curve: Curve, bytes: &[u8]) -> Result<Self, EcdhError> {
        let secret = match curve {
            Curve::K256 => Secret::K(k256::SecretKey::from_slice(bytes).map_err(|_| EcdhError::BadKey)?),
            Curve::P521 => Secret::P(p521::SecretKey::from_slice(bytes).map_err(|_| EcdhError::BadKey)?),
        };
        Ok(Self { curve, secret })
    }

    pub fn curve(&self) -> Curve {
        self.curve
    }

    /// Uncompressed SEC1 point `04 || X || Y`.
    pub fn public_bytes(&self) -> Vec<u8> {
        match &self.secret {
            Secret::K(s) => s.public_key().to_encoded_point(false).as_bytes().to_vec(),
            Secret::P(s) => {
                p521::elliptic_curve::sec1::ToEncodedPoint::to_encoded_point(&s.public_key(), false).as_bytes().to_vec()
            }
        }
    }

    /// Raw ECDH x-coordinate (fixed width), before the Java normalisation.
    pub fn shared_x(&self, peer: &[u8]) -> Result<Vec<u8>, EcdhError> {
        match &self.secret {
            Secret::K(s) => {
                let p = k256::PublicKey::from_sec1_bytes(peer).map_err(|_| EcdhError::BadPeer)?;
                Ok(k256::ecdh::diffie_hellman(s.to_nonzero_scalar(), p.as_affine()).raw_secret_bytes().to_vec())
            }
            Secret::P(s) => {
                let p = p521::PublicKey::from_sec1_bytes(peer).map_err(|_| EcdhError::BadPeer)?;
                Ok(p521::ecdh::diffie_hellman(s.to_nonzero_scalar(), p.as_affine()).raw_secret_bytes().to_vec())
            }
        }
    }

    /// Shared secret exactly as the app feeds it to the key schedule.
    pub fn shared_secret(&self, peer: &[u8], version: i32) -> Result<Vec<u8>, EcdhError> {
        Ok(normalise_secret(&self.shared_x(peer)?, version))
    }
}

/// Java `BigInteger.toByteArray()` of a non-negative big-endian magnitude: minimal length plus a leading
/// 0x00 when the top bit would otherwise read as a sign.
pub fn java_to_byte_array(x: &[u8]) -> Vec<u8> {
    let first = x.iter().position(|b| *b != 0).unwrap_or(x.len());
    let mut v = x[first..].to_vec();
    if v.is_empty() {
        v.push(0);
    }
    if v[0] & 0x80 != 0 {
        v.insert(0, 0);
    }
    v
}

/// version > 12: prepend 0x00 if exactly 65 bytes. else: strip a leading 0x00 if length > 32. Short values
/// are NOT zero-padded (about 1 in 256 sessions); mirrored literally.
pub fn normalise_secret(x_fixed: &[u8], version: i32) -> Vec<u8> {
    let mut b = java_to_byte_array(x_fixed);
    if version > 12 {
        if b.len() == 65 {
            b.insert(0, 0);
        }
    } else if b[0] == 0 && b.len() > 32 {
        b.remove(0);
    }
    b
}

/// `ecdhSig = HMAC-SHA256(handshakeKey, pubKey)` (`gb.d`).
pub fn ecdh_sig(handshake_key: &[u8; 16], pub_key: &[u8]) -> [u8; 32] {
    let mut m = <Hmac<Sha256> as Mac>::new_from_slice(handshake_key).unwrap();
    m.update(pub_key);
    m.finalize().into_bytes().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curve_selection() {
        assert_eq!(curve_for(9), Curve::K256);
        assert_eq!(curve_for(12), Curve::K256);
        assert_eq!(curve_for(13), Curve::P521);
        assert_eq!(curve_for(20), Curve::P521);
    }

    #[test]
    fn both_sides_agree_on_both_curves() {
        for (curve, version, pub_len, x_len) in [(Curve::K256, 9, 65, 32), (Curve::P521, 14, 133, 66)] {
            let (a, b) = (Keypair::generate(curve), Keypair::generate(curve));
            assert_eq!(a.public_bytes().len(), pub_len);
            assert_eq!(a.public_bytes()[0], 0x04);
            assert_eq!(a.shared_x(&b.public_bytes()).unwrap().len(), x_len);
            assert_eq!(
                a.shared_secret(&b.public_bytes(), version).unwrap(),
                b.shared_secret(&a.public_bytes(), version).unwrap()
            );
        }
        assert!(Keypair::generate(Curve::K256).shared_x(&[1, 2, 3]).is_err());
    }

    #[test]
    fn java_byte_array_semantics() {
        assert_eq!(java_to_byte_array(&[0x00, 0x7F]), vec![0x7F]);
        assert_eq!(java_to_byte_array(&[0x00, 0x80]), vec![0x00, 0x80]);
        assert_eq!(java_to_byte_array(&[0, 0, 0]), vec![0]);
        assert_eq!(java_to_byte_array(&[0xFF, 0x01]), vec![0x00, 0xFF, 0x01]);
    }

    #[test]
    fn normalisation_rules_match_the_spec() {
        // secp256k1: high bit set -> 33 bytes with leading 0 -> stripped back to 32
        let mut x = [0xABu8; 32];
        assert_eq!(normalise_secret(&x, 9), x.to_vec());
        // leading zero byte but next byte has its high bit set: the sign byte comes back, still 32 bytes
        x[0] = 0;
        assert_eq!(normalise_secret(&x, 9).len(), 32);
        // leading zero byte and next byte below 0x80: genuinely short (31 bytes), NOT zero-padded
        x[1] = 0x40;
        assert_eq!(normalise_secret(&x, 9).len(), 31);
        // secp521r1: top byte 0x00, next byte high bit clear -> 65 bytes -> prepend 0 -> 66
        let mut y = vec![0u8; 66];
        y[1] = 0x40;
        assert_eq!(java_to_byte_array(&y).len(), 65);
        assert_eq!(normalise_secret(&y, 14).len(), 66);
        // top byte 0x01: 66 bytes stays 66
        let mut z = vec![0xCDu8; 66];
        z[0] = 1;
        assert_eq!(normalise_secret(&z, 14).len(), 66);
    }

    #[test]
    fn deterministic_keys_and_sig() {
        let k = Keypair::from_secret(Curve::K256, &[7u8; 32]).unwrap();
        assert_eq!(k.public_bytes(), Keypair::from_secret(Curve::K256, &[7u8; 32]).unwrap().public_bytes());
        assert!(Keypair::from_secret(Curve::K256, &[0u8; 32]).is_err()); // zero is not a valid scalar
        assert_ne!(ecdh_sig(&[1; 16], b"a"), ecdh_sig(&[2; 16], b"a"));
        assert_eq!(ecdh_sig(&[1; 16], b"a").len(), 32);
    }
}
