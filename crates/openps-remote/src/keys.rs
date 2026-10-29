//! Key material and the register request/response of remote registration (09b sections 2 and 3.4-3.5).
//!
//! Everything here is checked byte-for-byte against the ORIGINAL application's own classes (`tests/oracle`).

use base64::Engine as _;
use openps_crypto::RpCipher;
use openps_rp::regist::{parse_header_block, CLIENT_TYPE};
use openps_rp::tables::KeyTables;
use rand_core::RngCore;
use std::net::Ipv4Addr;

const B32: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

#[derive(Debug, PartialEq, Eq)]
pub enum KeyError {
    /// `customData1` is not the expected double-base64 of at least 16 bytes. The text says which stage failed and how long things were
    /// (never the data itself, which is key material).
    BadCustomData(String),
    /// The register response is not an HTTP 200 with the expected headers.
    BadResponse(String),
}

impl std::fmt::Display for KeyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KeyError::BadCustomData(d) => write!(f, "the console's key data is not in the expected format ({d})"),
            KeyError::BadResponse(e) => write!(f, "the console's registration answer is not usable: {e}"),
        }
    }
}
impl std::error::Error for KeyError {}

/// The two per-attempt secrets sent to the console in the join command: 16 characters each from `A-Z2-7`.
pub fn attempt_secrets(rng: &mut impl RngCore) -> ([u8; 16], [u8; 16]) {
    let mut one = |out: &mut [u8; 16]| {
        for o in out.iter_mut() {
            let mut b = [0u8; 1];
            rng.fill_bytes(&mut b);
            *o = B32[(b[0] % 32) as usize];
        }
    };
    let (mut i, mut j) = ([0u8; 16], [0u8; 16]);
    one(&mut i);
    one(&mut j);
    (i, j)
}

/// `customData1` (base64 of base64 of 16 bytes) -> the 16-byte register key `l`.
/// The console encrypts with a cipher keyed by what we sent it (`i` = AES key, `j` = nonce, HMAC key `k`); we apply the same cipher
/// (the original calls its ENCRYPT; for one 16-byte block that equals decrypt).
pub fn decode_custom_data(t: &KeyTables, i: &[u8; 16], j: &[u8; 16], custom: &str) -> Result<[u8; 16], KeyError> {
    // Java's decoder (what the original uses) does not require the `=` padding and ignores non-zero trailing bits; ours must not be stricter
    let lenient = base64::engine::GeneralPurpose::new(
        &base64::alphabet::STANDARD,
        base64::engine::GeneralPurposeConfig::new()
            .with_decode_padding_mode(base64::engine::DecodePaddingMode::Indifferent)
            .with_decode_allow_trailing_bits(true),
    );
    let custom = custom.trim();
    let bad = |what: String| KeyError::BadCustomData(what);
    let inner = lenient
        .decode(custom)
        .map_err(|_| bad(format!("outer layer, {} characters, not valid base64", custom.len())))?;
    let text = String::from_utf8(inner).map_err(|_| bad("outer layer is not text".into()))?;
    let text = text.trim();
    let raw =
        lenient.decode(text).map_err(|_| bad(format!("inner layer, {} characters, not valid base64", text.len())))?;
    // the original keeps whatever length arrives and only ever reads the first 16 bytes
    let first: [u8; 16] = raw
        .get(..16)
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| bad(format!("{} bytes, need at least 16", raw.len())))?;
    let mut l = first;
    RpCipher::new(*i, *j, t.k).encrypt(&mut l, 0);
    Ok(l)
}

/// The cipher of the register request: AES key = `t[(i << 5) + idx] ^ l[i]`, nonce random, HMAC key `k`.
pub fn register_cipher(t: &KeyTables, l: &[u8; 16], idx: usize, nonce: [u8; 16]) -> RpCipher {
    let mut key = [0u8; 16];
    for (i, k) in key.iter_mut().enumerate() {
        *k = t.t[(i << 5) + idx] ^ l[i];
    }
    RpCipher::new(key, nonce, t.k)
}

/// The register request as it goes over the reliable-UDP layer: an HTTP-like head and a body of 480 random bytes followed by the
/// encrypted headers, with the nonce hidden at offsets 199 and 401. `body480` is the random part (its byte 397 selects the key
/// column, byte 0 the obfuscation column); `host` is OUR local address toward the console (the original puts it in `HOST`).
pub fn build_register_request(
    t: &KeyTables,
    l: &[u8; 16],
    body480: &[u8; 480],
    nonce: [u8; 16],
    account_id: u64,
    host: Ipv4Addr,
) -> (Vec<u8>, RpCipher) {
    let idx = (body480[397] & 0x1F) as usize;
    let cipher = register_cipher(t, l, idx, nonce);
    let account = base64::engine::general_purpose::STANDARD.encode(account_id.to_le_bytes());
    // keys sorted: Client-Type, Np-AccountId
    let mut headers = format!("Client-Type: {CLIENT_TYPE}\r\nNp-AccountId: {account}\r\n").into_bytes();
    cipher.encrypt(&mut headers, 0);
    let mut body = body480.to_vec();
    body.extend(headers);
    let n = (body480[0] >> 3) as usize;
    let mut obf = [0u8; 16];
    for (i, o) in obf.iter_mut().enumerate() {
        *o = nonce[i].wrapping_sub(i as u8).wrapping_add(43) ^ t.c[(i << 5) + n];
    }
    body[199..207].copy_from_slice(&obf[8..]);
    body[401..409].copy_from_slice(&obf[..8]);
    let mut req = format!(
        "POST /sie/ps5/rp/sess/rgst HTTP/1.1\r\nHOST: {host}\r\nUser-Agent: remoteplay Windows\r\nConnection: close\r\nRP-Version: 1.0\r\nContent-Length: {}\r\n\r\n",
        body.len()
    )
    .into_bytes();
    req.extend(body);
    (req, cipher)
}

/// What a successful remote registration yields (same secrets as a local one).
#[derive(Clone, PartialEq, Eq)]
pub struct RemoteRegistered {
    pub mac: String,
    pub nickname: String,
    pub regist_key: String,
    pub rp_key: String,
}

impl std::fmt::Debug for RemoteRegistered {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteRegistered")
            .field("mac", &self.mac)
            .field("nickname", &self.nickname)
            .field("keys", &"<redacted>")
            .finish()
    }
}

/// The reliable-UDP payload of the answer: HTTP-like head, `\r\n\r\n`, then the headers block encrypted with the register cipher.
pub fn parse_register_response(cipher: &RpCipher, payload: &[u8]) -> Result<RemoteRegistered, KeyError> {
    let text = String::from_utf8_lossy(payload);
    let status: u16 = text
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.get(..3))
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| KeyError::BadResponse("no status line".into()))?;
    if status != 200 {
        return Err(KeyError::BadResponse(format!("HTTP {status}")));
    }
    let split =
        payload.windows(4).position(|w| w == b"\r\n\r\n").ok_or_else(|| KeyError::BadResponse("no body".into()))?;
    let mut body = payload[split + 4..].to_vec();
    cipher.decrypt(&mut body, 0);
    let h = parse_header_block(&body);
    let need = |k: &str| h.get(k).cloned().ok_or_else(|| KeyError::BadResponse(format!("{k} is missing")));
    Ok(RemoteRegistered {
        regist_key: need("PS5-RegistKey")?,
        rp_key: need("RP-Key")?,
        mac: h.get("PS5-Mac").or_else(|| h.get("PS4-Mac")).cloned().unwrap_or_default(),
        nickname: h.get("PS5-Nickname").cloned().unwrap_or_else(|| "unknown".into()),
    })
}
