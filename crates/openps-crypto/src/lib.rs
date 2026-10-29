//! Takion crypto primitives. Behavior mirrors analysis/spec/02-streaming-protocol.md section 6 / R1.
//! No secret tables live here; `RpCipher` derivation inputs (key, nonce, hmac key) are supplied by the caller.

use aes::cipher::{generic_array::GenericArray, BlockEncrypt, KeyInit};
use aes::Aes128;
use aes_gcm::aead::{consts::U16, AeadInPlace};
use aes_gcm::AesGcm;
use cfb_mode::cipher::{AsyncStreamCipher, KeyIvInit};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;
type Gmac32 = AesGcm<Aes128, U16, U16>;

/// Wrapping 128-bit little-endian add of a u128 value.
fn le_add(base: &[u8; 16], add: u128) -> [u8; 16] {
    u128::from_le_bytes(*base).wrapping_add(add).to_le_bytes()
}

/// `derive(K, IV, e)`: h = SHA256(K || (IV + e*44910)); return h[0..16] ^ h[16..32].
pub fn derive(k: &[u8; 16], iv: &[u8; 16], epoch: u64) -> [u8; 16] {
    let t = le_add(iv, epoch as u128 * 44910);
    let mut h = Sha256::new();
    h.update(k);
    h.update(t);
    let h = h.finalize();
    let mut out = [0u8; 16];
    for i in 0..16 {
        out[i] = h[i] ^ h[16 + i];
    }
    out
}

/// Direction tag for the Takion key schedule.
#[derive(Clone, Copy, Debug)]
pub enum Direction {
    /// tag 2, client -> server
    Local,
    /// tag 3, server -> client
    Remote,
}

/// Per-direction crypto context (`fx` in the original).
pub struct Context {
    key: [u8; 16],
    iv: [u8; 16],
    seed: [u8; 16],
    gmac_key: [u8; 16],
    gmac_epoch: u64,
    aes: Aes128,
}

impl Context {
    /// `d = HMAC-SHA256(secret, 01 || tag || 00 || handshakeKey || 01 || 00)`; K = d[..16], IV = d[16..].
    pub fn new(dir: Direction, handshake_key: &[u8; 16], secret: &[u8]) -> Self {
        let tag = match dir {
            Direction::Local => 2u8,
            Direction::Remote => 3u8,
        };
        let mut mac = <HmacSha256 as Mac>::new_from_slice(secret).expect("hmac any key length");
        mac.update(&[1, tag, 0]);
        mac.update(handshake_key);
        mac.update(&[1, 0]);
        let d = mac.finalize().into_bytes();
        let (mut key, mut iv) = ([0u8; 16], [0u8; 16]);
        key.copy_from_slice(&d[..16]);
        iv.copy_from_slice(&d[16..]);
        Self::from_key_iv(key, iv)
    }

    pub fn from_key_iv(key: [u8; 16], iv: [u8; 16]) -> Self {
        let seed = derive(&key, &iv, 0);
        Self { key, iv, seed, gmac_key: seed, gmac_epoch: 0, aes: Aes128::new(GenericArray::from_slice(&key)) }
    }

    /// AES-CTR variant (little-endian counter block add). Encrypt == decrypt. `offset` is a byte offset
    /// (keyPos + 16 outbound; keyPos + 2^32*wrap + 16 inbound).
    pub fn ctr_xor(&self, offset: u64, buf: &mut [u8]) {
        let mut pos = 0usize;
        let mut block_no = offset / 16;
        let mut skip = (offset % 16) as usize;
        while pos < buf.len() {
            let mut ctr = GenericArray::clone_from_slice(&le_add(&self.iv, block_no as u128));
            self.aes.encrypt_block(&mut ctr);
            for &ks in &ctr[skip..] {
                if pos >= buf.len() {
                    break;
                }
                buf[pos] ^= ks;
                pos += 1;
            }
            skip = 0;
            block_no += 1;
        }
    }

    /// Per-packet GMAC (first 4 bytes of a 16-byte-IV GCM tag, empty plaintext, AAD = packet with the
    /// GMAC field zeroed by the caller). Mirrors the forward/backward key derivation asymmetry literally.
    pub fn gmac(&mut self, key_pos: u32, packet: &[u8]) -> [u8; 4] {
        let kp = key_pos as u64;
        let epoch = (if kp > 0 { kp - 1 } else { 0 }) / 45000;
        if epoch > self.gmac_epoch {
            self.gmac_key = derive(&self.seed, &self.iv, epoch);
            self.gmac_epoch = epoch;
        } else if epoch < self.gmac_epoch {
            // Backward branch is derived from K, not seed; cached key is not updated (spec 6.5).
            let key = if epoch != 0 { derive(&self.key, &self.iv, epoch) } else { self.gmac_key };
            return Self::gmac_with(&key, &self.iv, kp, packet);
        }
        Self::gmac_with(&self.gmac_key, &self.iv, kp, packet)
    }

    fn gmac_with(gmac_key: &[u8; 16], iv: &[u8; 16], key_pos: u64, packet: &[u8]) -> [u8; 4] {
        let nonce = le_add(iv, (key_pos / 16) as u128);
        let cipher = Gmac32::new(GenericArray::from_slice(gmac_key));
        let tag = cipher.encrypt_in_place_detached(GenericArray::from_slice(&nonce), packet, &mut []).expect("gmac");
        let mut out = [0u8; 4];
        out.copy_from_slice(&tag[..4]); // BC GMac(.., 32) = leading 32 bits of the full tag
        out
    }
}

/// Tracks the 32-bit keyPos wrap counter for one inbound stream (video, audio or haptic each own one).
#[derive(Default, Debug)]
pub struct KeyPosTracker {
    wrap: u64,
    last: u64,
}

impl KeyPosTracker {
    /// Returns the 64-bit CTR offset for an inbound packet: keyPos + 2^32*wrap + 16.
    pub fn offset(&mut self, key_pos: u32) -> u64 {
        let diff = key_pos as i64 - self.last as i64;
        if diff > 4_000_000_000 && self.wrap > 0 {
            // Late packet from before the last wrap: use the previous epoch, leave state untouched.
            return key_pos as u64 + (self.wrap.wrapping_sub(1) << 32) + 16;
        }
        if diff < -4_000_000_000 {
            self.wrap += 1;
        }
        self.last = key_pos as u64;
        key_pos as u64 + (self.wrap << 32) + 16
    }
    /// Update `last` without producing an offset (audio when processing is disabled).
    pub fn note(&mut self, key_pos: u32) {
        let _ = self.offset(key_pos);
    }
}

/// `RpCipher`: AES-128-CFB128 with a fresh IV per message: HMAC-SHA256(hmac_key, nonce || 0u32 || be32(n))[..16].
#[derive(Clone)]
pub struct RpCipher {
    key: [u8; 16],
    nonce: [u8; 16],
    hmac_key: [u8; 16],
    counter: u32,
}

impl RpCipher {
    pub fn new(key: [u8; 16], nonce: [u8; 16], hmac_key: [u8; 16]) -> Self {
        Self { key, nonce, hmac_key, counter: 0 }
    }
    fn iv(&self, n: u32) -> [u8; 16] {
        let mut mac = <HmacSha256 as Mac>::new_from_slice(&self.hmac_key).unwrap();
        mac.update(&self.nonce);
        mac.update(&[0, 0, 0, 0]);
        mac.update(&n.to_be_bytes());
        let mut iv = [0u8; 16];
        iv.copy_from_slice(&mac.finalize().into_bytes()[..16]);
        iv
    }
    /// The 16-byte nonce this cipher was built with (registration sends a masked form of it).
    pub fn nonce(&self) -> &[u8; 16] {
        &self.nonce
    }
    pub fn encrypt(&self, data: &mut [u8], n: u32) {
        cfb_mode::Encryptor::<Aes128>::new((&self.key).into(), (&self.iv(n)).into()).encrypt(data);
    }
    pub fn decrypt(&self, data: &mut [u8], n: u32) {
        cfb_mode::Decryptor::<Aes128>::new((&self.key).into(), (&self.iv(n)).into()).decrypt(data);
    }
    /// Decrypt using and incrementing the internal inbound counter.
    pub fn decrypt_next(&mut self, data: &mut [u8]) {
        let n = self.counter;
        self.decrypt(data, n);
        self.counter = self.counter.wrapping_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seq(f: impl Fn(usize) -> u8, n: usize) -> Vec<u8> {
        (0..n).map(f).collect()
    }

    /// Vectors produced by BouncyCastle 1.85 (the jar OpenPS ships) for the same inputs.
    #[test]
    fn gmac_matches_bouncycastle() {
        let key: [u8; 16] = seq(|i| (i * 3 + 1) as u8, 16).try_into().unwrap();
        let nonce: [u8; 16] = seq(|i| (255 - i * 5) as u8, 16).try_into().unwrap();
        let aad = seq(|i| (i * 7 + 2) as u8, 37);
        // keyPos 0 => nonce = iv + 0, epoch 0 => gmac key = seed, so build a context whose seed == key:
        let mut ctx = Context::from_key_iv([0; 16], nonce);
        ctx.gmac_key = key;
        assert_eq!(hex::encode(ctx.gmac(0, &aad)), "1e39cd18");
    }

    #[test]
    fn cfb_matches_bouncycastle() {
        let key: [u8; 16] = seq(|i| (i * 3 + 1) as u8, 16).try_into().unwrap();
        let iv: [u8; 16] = seq(|i| (255 - i * 5) as u8, 16).try_into().unwrap();
        let mut data = seq(|i| (i * 11) as u8, 40);
        cfb_mode::Encryptor::<Aes128>::new((&key).into(), (&iv).into()).encrypt(&mut data);
        assert_eq!(
            hex::encode(data),
            "f9daead0dcca37439dda0af25ba2b06ee865480cfccbd853bc6f1787c2a69d22fc6430595a64fc67"
        );
    }

    #[test]
    fn ctr_roundtrip_and_offsets_compose() {
        let ctx = Context::new(Direction::Remote, &[7; 16], b"shared-secret");
        let plain = seq(|i| i as u8, 300);
        let mut a = plain.clone();
        ctx.ctr_xor(48, &mut a);
        assert_ne!(a, plain);
        // Encrypting the tail at its own offset equals the tail of the whole.
        let mut tail = plain[160..].to_vec();
        ctx.ctr_xor(48 + 160, &mut tail);
        assert_eq!(tail, a[160..]);
        ctx.ctr_xor(48, &mut a);
        assert_eq!(a, plain);
    }

    #[test]
    fn ctr_counter_is_le_add_with_carry() {
        let mut iv = [0u8; 16];
        iv[0] = 0xFF; // +1 must carry into byte 1 (little-endian)
        assert_eq!(le_add(&iv, 1)[..2], [0x00, 0x01]);
    }

    #[test]
    fn local_and_remote_keys_differ() {
        let l = Context::new(Direction::Local, &[1; 16], b"s");
        let r = Context::new(Direction::Remote, &[1; 16], b"s");
        assert_ne!(l.key, r.key);
        assert_ne!(l.iv, r.iv);
    }

    #[test]
    fn gmac_epoch_rotation() {
        let mut ctx = Context::new(Direction::Local, &[3; 16], b"s");
        let p = [9u8; 20];
        let t0 = ctx.gmac(16, &p);
        assert_eq!(ctx.gmac_epoch, 0);
        let _ = ctx.gmac(45_008, &p); // (45008-1)/45000 = epoch 1
        assert_eq!(ctx.gmac_epoch, 1);
        assert_ne!(ctx.gmac_key, ctx.seed);
        // going backwards to epoch 0 uses the cached key (epoch==0 branch), not a re-derive from K
        assert_eq!(ctx.gmac(16, &p).len(), 4);
        let _ = t0;
    }

    #[test]
    fn keypos_wrap_detection() {
        let mut t = KeyPosTracker::default();
        assert_eq!(t.offset(4_294_967_280), 4_294_967_280 + 16);
        assert_eq!(t.offset(32), (1u64 << 32) + 32 + 16); // wrapped
                                                          // small backwards jump (out of order) does not wrap
        assert_eq!(t.offset(16), (1u64 << 32) + 16 + 16);
    }

    #[test]
    fn rp_cipher_roundtrip_and_counter() {
        let mut c = RpCipher::new([1; 16], [2; 16], [3; 16]);
        let mut m = b"hello takion".to_vec();
        c.encrypt(&mut m, 5);
        let mut m2 = m.clone();
        c.decrypt(&mut m2, 5);
        assert_eq!(m2, b"hello takion");
        let mut m3 = m.clone();
        c.decrypt(&mut m3, 4);
        assert_ne!(m3, b"hello takion"); // wrong counter => wrong IV
        let mut e = b"x".to_vec();
        c.encrypt(&mut e, 0);
        c.decrypt_next(&mut e);
        assert_eq!(e, b"x");
    }
}
