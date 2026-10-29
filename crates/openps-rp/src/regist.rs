//! Console registration (pairing) over HTTP on port 9295 (spec 02 section 4.2). The request body hides a
//! masked copy of the cipher nonce at fixed offsets; the console derives the key from the PIN and tables.

use crate::http::{Request, Response};
use crate::kdf::{regist_cipher, regist_patch, Family};
use crate::tables::KeyTables;
use base64::Engine as _;
use openps_crypto::RpCipher;
use openps_discovery::DeviceType;
use rand_core::RngCore;
use std::collections::BTreeMap;

/// `Client-Type` header value the original sends for versions 1.0 and 10.0 (a fixed client identifier).
pub const CLIENT_TYPE: &str = "dabfa2ec873de5839bee8d3f4c0239c4282c07c25c6077a2931afcf0adc0d34f";
pub const USER_AGENT: &str = "remoteplay Windows";

pub struct RegisterParams<'a> {
    /// Protocol version: `1.0` (PS5), `10.0` (PS4 fw 8+), `9.0` (fw 7) or `8.0` (fw 5-6).
    pub version: &'a str,
    pub device: DeviceType,
    /// The 8-digit PIN shown on the console.
    pub pin: u32,
    /// PSN account id (sent little-endian, base64) for versions 1.0/10.0/9.0.
    pub account_id: u64,
    /// PSN online id, used instead of the account id for the legacy 8.0 protocol.
    pub online_id: &'a str,
}

pub struct RegisterRequest {
    pub request: Request,
    /// Keep this to decrypt the response.
    pub cipher: RpCipher,
}

fn is_new(version: &str) -> bool {
    matches!(version, "1.0" | "10.0")
}

/// Registration endpoint path for a version/device pair.
pub fn path(version: &str, device: DeviceType) -> &'static str {
    if is_new(version) {
        if device == DeviceType::Ps5 {
            "/sie/ps5/rp/sess/rgst"
        } else {
            "/sie/ps4/rp/sess/rgst"
        }
    } else {
        "/sce/rp/regist"
    }
}

/// Header block as the console expects it: keys sorted, `Key: Value\r\n`.
fn header_block(headers: &BTreeMap<String, String>) -> Vec<u8> {
    let mut out = vec![];
    for (k, v) in headers {
        out.extend(format!("{}: {}\r\n", k.trim(), v.trim()).bytes());
    }
    out
}

/// Parse `Key: Value` lines the way the original does: lines that split into exactly two parts on ':' (so a
/// value containing ':' is skipped).
pub fn parse_header_block(data: &[u8]) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for line in String::from_utf8_lossy(data).split('\n') {
        let line = line.trim_end_matches('\r');
        let parts: Vec<&str> = line.split(':').collect();
        if parts.len() == 2 {
            out.insert(parts[0].trim().to_string(), parts[1].trim().to_string());
        }
    }
    out
}

pub fn build_request(t: &KeyTables, p: &RegisterParams, host: &str, rng: &mut impl RngCore) -> RegisterRequest {
    let mut rand480 = [0u8; 480];
    rng.fill_bytes(&mut rand480);
    let mut rand16 = [0u8; 16];
    rng.fill_bytes(&mut rand16);

    let new = is_new(p.version);
    let ps5 = p.device == DeviceType::Ps5;
    let family = if new {
        if ps5 {
            Family::Ps5
        } else {
            Family::Ps4
        }
    } else {
        Family::Legacy
    };
    let idx = (rand480[397] & 0x1F) as usize;
    let cipher = regist_cipher(t, family, &rand16, p.pin, idx);

    let mut headers = BTreeMap::new();
    headers.insert("Client-Type".to_string(), if new { CLIENT_TYPE.to_string() } else { "Windows".to_string() });
    if matches!(p.version, "1.0" | "10.0" | "9.0") {
        headers.insert(
            "Np-AccountId".into(),
            base64::engine::general_purpose::STANDARD.encode(p.account_id.to_le_bytes()),
        );
    } else {
        headers.insert("Np-Online-Id".into(), p.online_id.to_string());
    }
    let mut enc = header_block(&headers);
    cipher.encrypt(&mut enc, 0);

    let mut body = rand480.to_vec();
    body.extend(enc);
    let patch = regist_patch(t, family, ps5, cipher.nonce(), (rand480[0] >> 3) as usize);
    if new {
        body[401..409].copy_from_slice(&patch[..8]);
        body[199..207].copy_from_slice(&patch[8..]);
    } else {
        body[284..300].copy_from_slice(&patch);
    }

    let mut request = Request::new("POST", path(p.version, p.device))
        .header("HOST", format!("{host}:{}", openps_discovery::REGISTRATION_PORT))
        .header("User-Agent", USER_AGENT)
        .header("Connection", "close");
    if matches!(p.version, "1.0" | "10.0" | "9.0") {
        request = request.header("RP-Version", p.version);
    }
    RegisterRequest { request: request.body(body), cipher }
}

/// What a successful registration yields; persist it (registration key and RP-Key are secrets).
#[derive(Clone, PartialEq, Eq)]
pub struct Registered {
    pub device: DeviceType,
    pub mac: String,
    pub nickname: String,
    pub regist_key: String,
    pub rp_key: String,
    pub ap_name: String,
}

/// The registration key and RP-Key are long-term secrets: `Debug` must never print them.
impl std::fmt::Debug for Registered {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registered")
            .field("device", &self.device)
            .field("mac", &self.mac)
            .field("nickname", &self.nickname)
            .field("ap_name", &self.ap_name)
            .field("regist_key", &"<redacted>")
            .field("rp_key", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum RegisterError {
    /// HTTP failure; `reason` is the `RP-Application-Reason` hex code (may be empty).
    Rejected {
        status: u16,
        reason: String,
    },
    EmptyBody,
    /// The decrypted response lacks a mandatory field.
    MissingField(&'static str),
}

/// Human text for an `RP-Application-Reason` code.
pub fn reason_text(code: &str) -> &'static str {
    match code.to_ascii_lowercase().as_str() {
        "80108b09" => "registration failed",
        "80108b02" => "wrong PSN id",
        "80108b10" => "another user is connected",
        "80108b15" => "the console crashed",
        "80108b11" => "the console must be updated",
        "80108b13" => "not allowed",
        "80108bff" => "wrong credentials",
        "80108b12" => "Remote Play is disabled on the console",
        _ => "unknown reason",
    }
}

/// What to try, for a refusal. Most refusals of a correct request are one of three things the user can fix.
pub fn rejection_hint(status: u16, reason: &str) -> &'static str {
    match reason.to_ascii_lowercase().as_str() {
        "80108b10" => "Someone else is using Remote Play on this console. Try again when they have finished.",
        "80108b11" => "Update the console's system software, then try again.",
        "80108b12" => "Turn on Remote Play in the console's settings.",
        // The request is built the way the original builds it (40 captures of the original's own requests decode with our code, and the
        // running app's wire bytes decode with the typed PIN and the stored account id), so a refusal comes from what the console holds:
        // the code, or which PSN account it accepts. The original maps b09 to "check your PIN" and bff to "credentials are incorrect".
        "80108bff" => {
            "The console did not accept the credentials. Check: (1) you are using a NEW 8-digit code (open Link Device again; codes expire after a few minutes); \
(2) the PlayStation Network account you signed in with here is the account that is signed in on the console (and, if you have several users on it, the one you chose for Remote Play)."
        }
        "" | "80108b02" | "80108b09" if status == 403 || status == 401 => {
            "Check that: (1) the 8-digit code is current, since it expires after a few minutes (open Link Device again for a new one); \
(2) the PlayStation Network account you signed in with here is a user that is signed in on the console; \
(3) the console type (PS4 or PS5) is the right one."
        }
        _ => "",
    }
}

pub fn parse_response(cipher: &RpCipher, resp: &Response) -> Result<Registered, RegisterError> {
    if !resp.ok() {
        return Err(RegisterError::Rejected {
            status: resp.status,
            reason: crate::session::clean_reason(resp.header("RP-Application-Reason").unwrap_or("")),
        });
    }
    if resp.body.is_empty() {
        return Err(RegisterError::EmptyBody);
    }
    let mut body = resp.body.clone();
    cipher.decrypt(&mut body, 0);
    let h = parse_header_block(&body);
    let (device, prefix) = if h.contains_key("PS5-Mac") { (DeviceType::Ps5, "PS5") } else { (DeviceType::Ps4, "PS4") };
    let get = |k: String, name: &'static str| h.get(&k).cloned().ok_or(RegisterError::MissingField(name));
    Ok(Registered {
        device,
        mac: get(format!("{prefix}-Mac"), "Mac")?,
        nickname: h.get(&format!("{prefix}-Nickname")).cloned().unwrap_or_default(),
        regist_key: get(format!("{prefix}-RegistKey"), "RegistKey")?,
        rp_key: get("RP-Key".into(), "RP-Key")?,
        ap_name: h.get("AP-Name").cloned().unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kdf::testutil::tables;

    /// Deterministic RNG for reproducible requests.
    pub struct Seq(pub u8);
    impl RngCore for Seq {
        fn next_u32(&mut self) -> u32 {
            let mut b = [0u8; 4];
            self.fill_bytes(&mut b);
            u32::from_le_bytes(b)
        }
        fn next_u64(&mut self) -> u64 {
            let mut b = [0u8; 8];
            self.fill_bytes(&mut b);
            u64::from_le_bytes(b)
        }
        fn fill_bytes(&mut self, dest: &mut [u8]) {
            for d in dest {
                self.0 = self.0.wrapping_mul(167).wrapping_add(13);
                *d = self.0;
            }
        }
        fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand_core::Error> {
            self.fill_bytes(dest);
            Ok(())
        }
    }

    fn params(version: &'static str, device: DeviceType) -> RegisterParams<'static> {
        RegisterParams { version, device, pin: 12_345_678, account_id: 0x0102_0304_0506_0708, online_id: "player" }
    }

    #[test]
    fn paths_and_headers_per_version() {
        let Some(t) = tables() else { return };
        for (v, d, p, rp) in [
            ("1.0", DeviceType::Ps5, "/sie/ps5/rp/sess/rgst", Some("1.0")),
            ("10.0", DeviceType::Ps4, "/sie/ps4/rp/sess/rgst", Some("10.0")),
            ("9.0", DeviceType::Ps4, "/sce/rp/regist", Some("9.0")),
            ("8.0", DeviceType::Ps4, "/sce/rp/regist", None),
        ] {
            let r = build_request(&t, &params(v, d), "192.168.1.20", &mut Seq(1)).request;
            assert_eq!((r.method.as_str(), r.path.as_str()), ("POST", p), "{v}");
            assert_eq!(r.get_header("RP-Version"), rp, "{v}");
            assert_eq!(r.get_header("HOST"), Some("192.168.1.20:9295"));
            assert_eq!(r.get_header("User-Agent"), Some("remoteplay Windows"));
            assert_eq!(r.get_header("Connection"), Some("close"));
            assert!(r.body.len() > 480, "{v}: body carries the encrypted header block after the 480 random bytes");
        }
    }

    #[test]
    fn body_layout_and_decryptable_headers() {
        let Some(t) = tables() else { return };
        let req = build_request(&t, &params("1.0", DeviceType::Ps5), "h", &mut Seq(5));
        let body = &req.request.body;
        // rand480 || encrypted header block; the cipher recovers the headers at counter 0
        let mut enc = body[480..].to_vec();
        req.cipher.decrypt(&mut enc, 0);
        let h = parse_header_block(&enc);
        assert_eq!(h["Client-Type"], CLIENT_TYPE);
        assert_eq!(
            h["Np-AccountId"],
            base64::engine::general_purpose::STANDARD.encode(0x0102_0304_0506_0708u64.to_le_bytes())
        );
        // the keystream-related patch bytes overwrite offsets 199..207 and 401..409 of the random block
        let mut r = Seq(5);
        let mut rand480 = [0u8; 480];
        r.fill_bytes(&mut rand480);
        assert_ne!(&body[401..409], &rand480[401..409]);
        assert_ne!(&body[199..207], &rand480[199..207]);
        assert_eq!(&body[..199], &rand480[..199]);
        assert_eq!(&body[207..401], &rand480[207..401]);
        assert_eq!(&body[409..480], &rand480[409..]);
    }

    #[test]
    fn legacy_uses_online_id_and_patches_offset_284() {
        let Some(t) = tables() else { return };
        let req = build_request(&t, &params("8.0", DeviceType::Ps4), "h", &mut Seq(9));
        let mut enc = req.request.body[480..].to_vec();
        req.cipher.decrypt(&mut enc, 0);
        let h = parse_header_block(&enc);
        assert_eq!((h["Client-Type"].as_str(), h["Np-Online-Id"].as_str()), ("Windows", "player"));
        assert!(!h.contains_key("Np-AccountId"));
        let mut rand480 = [0u8; 480];
        Seq(9).fill_bytes(&mut rand480);
        assert_ne!(&req.request.body[284..300], &rand480[284..300]);
        assert_eq!(&req.request.body[..284], &rand480[..284]);
    }

    #[test]
    fn a_different_pin_yields_a_different_body() {
        let Some(t) = tables() else { return };
        let a = build_request(&t, &params("1.0", DeviceType::Ps5), "h", &mut Seq(2)).request.body;
        let mut p = params("1.0", DeviceType::Ps5);
        p.pin = 87_654_321;
        let b = build_request(&t, &p, "h", &mut Seq(2)).request.body;
        assert_ne!(a[480..], b[480..]);
    }

    fn console_reply(cipher: &RpCipher, text: &str) -> Response {
        let mut body = text.as_bytes().to_vec();
        cipher.encrypt(&mut body, 0);
        Response { status: 200, body, ..Default::default() }
    }

    #[test]
    fn response_parsing_ps5_and_ps4() {
        let Some(t) = tables() else { return };
        let req = build_request(&t, &params("1.0", DeviceType::Ps5), "h", &mut Seq(3));
        let r = console_reply(&req.cipher, "PS5-Mac: AABBCCDDEEFF\r\nPS5-Nickname: Living Room\r\nPS5-RegistKey: 3a3b3c\r\nRP-Key: 00112233445566778899aabbccddeeff\r\nAP-Name: ps5-001\r\n");
        let reg = parse_response(&req.cipher, &r).unwrap();
        assert_eq!(
            reg,
            Registered {
                device: DeviceType::Ps5,
                mac: "AABBCCDDEEFF".into(),
                nickname: "Living Room".into(),
                regist_key: "3a3b3c".into(),
                rp_key: "00112233445566778899aabbccddeeff".into(),
                ap_name: "ps5-001".into()
            }
        );
        let r = console_reply(&req.cipher, "PS4-Mac: 112233445566\r\nPS4-RegistKey: aa\r\nRP-Key: bb\r\n");
        let reg = parse_response(&req.cipher, &r).unwrap();
        assert_eq!((reg.device, reg.nickname.as_str()), (DeviceType::Ps4, ""));
        // a nickname containing ':' is skipped by the original's two-part rule, so it ends up empty
        let r = console_reply(&req.cipher, "PS5-Mac: 1\r\nPS5-Nickname: a:b\r\nPS5-RegistKey: 2\r\nRP-Key: 3\r\n");
        assert_eq!(parse_response(&req.cipher, &r).unwrap().nickname, "");
    }

    #[test]
    fn response_errors() {
        let Some(t) = tables() else { return };
        let req = build_request(&t, &params("1.0", DeviceType::Ps5), "h", &mut Seq(3));
        let rejected = Response {
            status: 403,
            headers: vec![("RP-Application-Reason".into(), "80108b02".into())],
            ..Default::default()
        };
        assert_eq!(
            parse_response(&req.cipher, &rejected),
            Err(RegisterError::Rejected { status: 403, reason: "80108b02".into() })
        );
        assert_eq!(reason_text("80108B02"), "wrong PSN id");
        assert_eq!(reason_text("zz"), "unknown reason");
        assert_eq!(
            parse_response(&req.cipher, &Response { status: 200, ..Default::default() }),
            Err(RegisterError::EmptyBody)
        );
        let r = console_reply(&req.cipher, "PS5-Mac: 1\r\n");
        assert_eq!(parse_response(&req.cipher, &r), Err(RegisterError::MissingField("RegistKey")));
        // wrong key (wrong PIN) decrypts to garbage: no fields
        let other = regist_cipher(&t, Family::Ps5, &[9; 16], 1, 0);
        let r = console_reply(&other, "PS5-Mac: 1\r\nPS5-RegistKey: 2\r\nRP-Key: 3\r\n");
        assert!(parse_response(&req.cipher, &r).is_err());
    }

    #[test]
    fn header_block_parsing_quirks() {
        let h = parse_header_block(b"A: 1\r\nB:2\nC: x:y\r\nnoise\r\n D : spaced \r\n");
        assert_eq!(h.get("A").map(String::as_str), Some("1"));
        assert_eq!(h.get("B").map(String::as_str), Some("2"));
        assert!(!h.contains_key("C")); // two colons: skipped
        assert_eq!(h.get("D").map(String::as_str), Some("spaced"));
    }

    #[test]
    fn debug_output_never_contains_the_secrets() {
        let r = Registered {
            device: DeviceType::Ps5,
            mac: "AABBCCDDEE01".into(),
            nickname: "Den".into(),
            regist_key: "SUPER-SECRET-REGIST-KEY".into(),
            rp_key: "SUPER-SECRET-RP-KEY".into(),
            ap_name: "ap".into(),
        };
        for text in [format!("{r:?}"), format!("{r:#?}"), format!("{:?}", Some(&r)), format!("{:?}", vec![r.clone()])] {
            assert!(!text.contains("SECRET"), "{text}");
            assert!(text.contains("Den") && text.contains("<redacted>"), "{text}");
        }
    }

    #[test]
    fn a_refusal_names_its_code_and_says_what_to_check() {
        use crate::client::ClientError;
        let e = ClientError::Register(RegisterError::Rejected { status: 403, reason: "80108b02".into() }).to_string();
        assert!(
            e.contains("HTTP 403, code 80108b02") && e.contains("wrong PSN id") && e.contains("code is current"),
            "{e}"
        );
        // no reason header at all: still helpful, no empty ", code"
        let e = ClientError::Register(RegisterError::Rejected { status: 403, reason: String::new() }).to_string();
        assert!(e.contains("HTTP 403)") && !e.contains("code ,") && e.contains("signed in on the console"), "{e}");
        // specific codes get their own advice, not the generic one
        let e = ClientError::Register(RegisterError::Rejected { status: 403, reason: "80108b11".into() }).to_string();
        assert!(e.contains("Update the console") && !e.contains("8-digit"), "{e}");
        // a server error has no hint
        let e = ClientError::Register(RegisterError::Rejected { status: 500, reason: String::new() }).to_string();
        assert!(!e.contains("Check that"), "{e}");
    }

    #[test]
    fn wrong_credentials_lists_both_the_code_and_the_account() {
        use crate::client::ClientError;
        let e = ClientError::Register(RegisterError::Rejected { status: 403, reason: "80108bff".into() }).to_string();
        assert!(e.contains("code 80108bff") && e.contains("NEW 8-digit code"), "{e}");
        assert!(e.contains("account that is signed in on the console"), "{e}");
        assert!(!e.contains("almost always"), "no overconfident diagnosis: {e}");
    }
}
