//! Session setup over HTTP on port 9295 (spec 02 section 4.3): `GET init` returns a nonce, the session cipher is
//! derived from it and the stored `RP-Key`, then `GET ctrl` opens the control channel (kept alive) with
//! cipher-protected headers. Every encrypted header value uses the next outbound counter value.

use crate::http::{Request, Response};
use crate::kdf::session_cipher;
use crate::regist::USER_AGENT;
use crate::tables::{from_hex, KeyTables};
use base64::Engine as _;
use openps_crypto::RpCipher;
use openps_discovery::{DeviceType, REGISTRATION_PORT};
use rand_core::RngCore;

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

pub fn init_path(version: &str) -> &'static str {
    match version {
        "1.0" => "/sie/ps5/rp/sess/init",
        "10.0" => "/sie/ps4/rp/sess/init",
        _ => "/sce/rp/session",
    }
}

pub fn ctrl_path(version: &str) -> &'static str {
    match version {
        "1.0" => "/sie/ps5/rp/sess/ctrl",
        "10.0" => "/sie/ps4/rp/sess/ctrl",
        _ => "/sce/rp/session/ctrl",
    }
}

/// `GET init` with the registration key; the reply carries `RP-Nonce`.
pub fn init_request(host: &str, regist_key_hex: &str, version: &str) -> Request {
    Request::new("GET", init_path(version))
        .header("Host", format!("{host}:{REGISTRATION_PORT}"))
        .header("User-Agent", USER_AGENT)
        .header("Connection", "close")
        .header("Content-Length", "0")
        .header("RP-Registkey", regist_key_hex)
        .header("RP-Version", version)
}

#[derive(Debug, PartialEq, Eq)]
pub enum InitError {
    /// HTTP failure with the console's reason code and the protocol version the console wants.
    Rejected {
        status: u16,
        reason: String,
        server_version: String,
    },
    NonceMissing,
    BadNonce,
}

/// What the caller should do about a failed init (the app's retry rules).
#[derive(Debug, PartialEq, Eq)]
pub enum Retry {
    /// Retry with an older protocol version after a pause.
    Downgrade(&'static str),
    /// Retry the same request after a pause, at most `max` times.
    Again {
        max: u32,
    },
    GiveUp,
}

impl InitError {
    /// `80108b11` + a lower server version -> retry with 9.0 (from 10.0) or 8.0 (from 9.0) after 2 s;
    /// `80108b10` (another user connected) -> retry up to twice after 2 s.
    pub fn retry(&self, local_version: &str) -> Retry {
        let InitError::Rejected { reason, server_version, .. } = self else { return Retry::GiveUp };
        match reason.to_ascii_lowercase().as_str() {
            "80108b11" if server_is_lower(local_version, server_version) => match local_version {
                "10.0" => Retry::Downgrade("9.0"),
                "9.0" => Retry::Downgrade("8.0"),
                _ => Retry::GiveUp,
            },
            "80108b10" => Retry::Again { max: 2 },
            _ => Retry::GiveUp,
        }
    }
}

fn server_is_lower(local: &str, server: &str) -> bool {
    match (local.parse::<f32>(), server.parse::<f32>()) {
        (Ok(l), Ok(s)) => !server.is_empty() && s < l,
        _ => false,
    }
}

pub fn parse_init_response(resp: &Response) -> Result<[u8; 16], InitError> {
    if !resp.ok() {
        return Err(InitError::Rejected {
            status: resp.status,
            reason: resp.header("RP-Application-Reason").unwrap_or("").to_string(),
            server_version: resp.header("RP-Version").unwrap_or("").to_string(),
        });
    }
    let nonce = resp.header("RP-Nonce").ok_or(InitError::NonceMissing)?;
    let bytes = b64().decode(nonce).map_err(|_| InitError::BadNonce)?;
    bytes.try_into().map_err(|_| InitError::BadNonce)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamingType {
    Avc,
    HevcSdr,
    HevcHdr,
}

impl StreamingType {
    fn code(self) -> u8 {
        match self {
            StreamingType::Avc => 1,
            StreamingType::HevcSdr => 2,
            StreamingType::HevcHdr => 3,
        }
    }
}

pub struct CtrlParams<'a> {
    pub host: &'a str,
    pub version: &'a str,
    pub device: DeviceType,
    pub regist_key_hex: &'a str,
    pub rp_key_hex: &'a str,
    /// Only PS5 sends a streaming type; PS4 always streams AVC.
    pub streaming: StreamingType,
}

/// Cipher plus the two counters the control channel continues with.
pub struct CtrlState {
    pub cipher: RpCipher,
    /// Next outbound counter (4 for PS4 10.0, 5 for PS5 after the request headers; 3 for older PS4).
    pub out_counter: u32,
    /// Next inbound counter (1 once `RP-Server-Type` was decrypted).
    pub in_counter: u32,
}

#[derive(Debug, PartialEq, Eq)]
pub enum CtrlError {
    BadKey,
}

/// Build the `GET ctrl` request. Returns it with the cipher state to continue with.
pub fn ctrl_request(
    t: &KeyTables,
    p: &CtrlParams,
    nonce: &[u8; 16],
    rng: &mut impl RngCore,
) -> Result<(Request, CtrlState), CtrlError> {
    let rp_key: [u8; 16] = from_hex(p.rp_key_hex).and_then(|b| b.try_into().ok()).ok_or(CtrlError::BadKey)?;
    let regist_key = from_hex(p.regist_key_hex).ok_or(CtrlError::BadKey)?;
    let cipher = session_cipher(t, p.version, &rp_key, nonce);
    let mut ctr = 0u32;
    let mut take = |data: &[u8], cipher: &RpCipher| {
        let mut d = data.to_vec();
        cipher.encrypt(&mut d, ctr);
        ctr += 1;
        b64().encode(d)
    };
    let ps5 = p.device == DeviceType::Ps5;
    let new = matches!(p.version, "1.0" | "10.0");

    let mut auth = regist_key;
    auth.extend([0u8; 8]);
    let rp_auth = take(&auth, &cipher);

    let mut uuid = [0u8; 16];
    rng.fill_bytes(&mut uuid);
    let mut did = if new {
        vec![0x00, 0x18, 0x00, 0x00, 0x00, 0x07, 0x00, 0x40, 0x00, 0x80]
    } else {
        vec![0x00, 0x18, 0x00, 0x00, 0x00, 0x07, 0x00, 0x00, 0x80]
    };
    did.extend(uuid);
    did.extend(if new { [0xAB; 6].to_vec() } else { b"0000000".to_vec() });
    let rp_did = take(&did, &cipher);

    let mut os = if new { b"Win10.0".to_vec() } else { b"Win10.0.0".to_vec() };
    os.push(0);
    let rp_os = take(&os, &cipher);

    let controller_type = if ps5 || p.version == "10.0" { "0" } else { "3" };
    let mut req = Request::new("GET", ctrl_path(p.version))
        .header("Host", format!("{}:{REGISTRATION_PORT}", p.host))
        .header("User-Agent", USER_AGENT)
        .header("Connection", "keep-alive")
        .header("Content-Length", "0")
        .header("RP-Auth", rp_auth)
        .header("RP-Version", p.version)
        .header("RP-Did", rp_did)
        .header("RP-ControllerType", controller_type)
        .header("RP-ClientType", "11")
        .header("RP-OSType", rp_os)
        .header("RP-ConPath", "1");
    if ps5 || p.version == "10.0" {
        req = req.header("RP-StartBitrate", take(&[0, 0, 0, 0], &cipher));
    }
    if ps5 {
        let st = if ps5 { p.streaming.code() } else { 1 };
        req = req.header("RP-StreamingType", take(&[st, 0, 0, 0], &cipher));
    }
    Ok((req, CtrlState { cipher, out_counter: ctr, in_counter: 0 }))
}

#[derive(Debug, PartialEq, Eq)]
pub struct CtrlInfo {
    /// Decoded `RP-Server-Type` (little-endian u16), if the console sent one.
    pub server_type: Option<u16>,
    /// PS4 only: a value other than 1 means the console cannot do full HD.
    pub full_hd: bool,
    /// `RP-Prohibit` was set: streaming is currently not allowed.
    pub prohibited: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum CtrlResponseError {
    Rejected { status: u16, reason: String },
}

/// Interpret the ctrl response and advance the inbound counter if `RP-Server-Type` was present.
pub fn parse_ctrl_response(
    state: &mut CtrlState,
    device: DeviceType,
    resp: &Response,
) -> Result<CtrlInfo, CtrlResponseError> {
    if !resp.ok() {
        return Err(CtrlResponseError::Rejected {
            status: resp.status,
            reason: resp.header("RP-Application-Reason").unwrap_or("").to_string(),
        });
    }
    let mut server_type = None;
    if let Some(v) = resp.header("RP-Server-Type").filter(|v| !v.is_empty()) {
        if let Ok(mut raw) = b64().decode(v) {
            state.cipher.decrypt(&mut raw, state.in_counter);
            state.in_counter += 1;
            if raw.len() >= 2 {
                server_type = Some(u16::from_le_bytes([raw[0], raw[1]]));
            }
        }
    }
    let prohibited = resp.header("RP-Prohibit").is_some_and(|v| v.trim() != "0");
    let full_hd = device == DeviceType::Ps5 || server_type.is_none_or(|t| t == 1);
    Ok(CtrlInfo { server_type, full_hd, prohibited })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kdf::testutil::{seq, tables};

    struct Seq(u8);
    impl RngCore for Seq {
        fn next_u32(&mut self) -> u32 {
            self.0 = self.0.wrapping_add(1);
            self.0 as u32
        }
        fn next_u64(&mut self) -> u64 {
            self.next_u32() as u64
        }
        fn fill_bytes(&mut self, d: &mut [u8]) {
            for x in d {
                self.0 = self.0.wrapping_add(7);
                *x = self.0;
            }
        }
        fn try_fill_bytes(&mut self, d: &mut [u8]) -> Result<(), rand_core::Error> {
            self.fill_bytes(d);
            Ok(())
        }
    }

    fn hex(b: &[u8]) -> String {
        crate::tables::to_hex(b)
    }

    #[test]
    fn init_request_and_paths() {
        let r = init_request("10.0.0.5", "abcd", "1.0");
        assert_eq!((r.method.as_str(), r.path.as_str()), ("GET", "/sie/ps5/rp/sess/init"));
        assert_eq!(
            (r.get_header("RP-Registkey"), r.get_header("RP-Version"), r.get_header("Host")),
            (Some("abcd"), Some("1.0"), Some("10.0.0.5:9295"))
        );
        assert_eq!(init_path("10.0"), "/sie/ps4/rp/sess/init");
        assert_eq!(init_path("9.0"), "/sce/rp/session");
        assert_eq!(ctrl_path("1.0"), "/sie/ps5/rp/sess/ctrl");
        assert_eq!(ctrl_path("8.0"), "/sce/rp/session/ctrl");
    }

    #[test]
    fn init_response_parsing_and_retry_rules() {
        let nonce = [7u8; 16];
        let ok =
            Response { status: 200, headers: vec![("RP-Nonce".into(), b64().encode(nonce))], ..Default::default() };
        assert_eq!(parse_init_response(&ok), Ok(nonce));
        let none = Response { status: 200, ..Default::default() };
        assert_eq!(parse_init_response(&none), Err(InitError::NonceMissing));
        let bad =
            Response { status: 200, headers: vec![("RP-Nonce".into(), b64().encode([1u8; 5]))], ..Default::default() };
        assert_eq!(parse_init_response(&bad), Err(InitError::BadNonce));

        let rej = |reason: &str, ver: &str| Response {
            status: 403,
            headers: vec![("RP-Application-Reason".into(), reason.into()), ("RP-Version".into(), ver.into())],
            ..Default::default()
        };
        let e = parse_init_response(&rej("80108b11", "9.0")).unwrap_err();
        assert_eq!(e.retry("10.0"), Retry::Downgrade("9.0"));
        assert_eq!(parse_init_response(&rej("80108b11", "8.0")).unwrap_err().retry("9.0"), Retry::Downgrade("8.0"));
        assert_eq!(parse_init_response(&rej("80108b11", "8.0")).unwrap_err().retry("1.0"), Retry::GiveUp); // PS5 never downgrades
        assert_eq!(parse_init_response(&rej("80108b11", "11.0")).unwrap_err().retry("10.0"), Retry::GiveUp); // server not older
        assert_eq!(parse_init_response(&rej("80108b10", "")).unwrap_err().retry("1.0"), Retry::Again { max: 2 });
        assert_eq!(parse_init_response(&rej("80108b09", "")).unwrap_err().retry("1.0"), Retry::GiveUp);
        assert_eq!(InitError::NonceMissing.retry("1.0"), Retry::GiveUp);
    }

    fn params<'a>(version: &'a str, device: DeviceType, key: &'a str) -> CtrlParams<'a> {
        CtrlParams {
            host: "10.0.0.5",
            version,
            device,
            regist_key_hex: "3a3b3c3d",
            rp_key_hex: key,
            streaming: StreamingType::HevcSdr,
        }
    }

    #[test]
    fn ctrl_request_headers_counters_and_decryption() {
        let Some(t) = tables() else { return };
        let key = hex(&seq(7, 3));
        let nonce = seq(11, 200);
        // PS5: auth(0) did(1) os(2) startBitrate(3) streamingType(4) -> next outbound counter 5
        let (req, st) = ctrl_request(&t, &params("1.0", DeviceType::Ps5, &key), &nonce, &mut Seq(0)).unwrap();
        assert_eq!(st.out_counter, 5);
        assert_eq!(req.get_header("RP-ControllerType"), Some("0"));
        assert_eq!(req.get_header("RP-ClientType"), Some("11"));
        assert_eq!(req.get_header("RP-ConPath"), Some("1"));
        assert_eq!(req.get_header("Connection"), Some("keep-alive"));
        let dec = |name: &str, ctr: u32| {
            let mut v = b64().decode(req.get_header(name).unwrap()).unwrap();
            st.cipher.decrypt(&mut v, ctr);
            v
        };
        assert_eq!(dec("RP-Auth", 0), [&[0x3a, 0x3b, 0x3c, 0x3d][..], &[0u8; 8]].concat());
        let did = dec("RP-Did", 1);
        assert_eq!(
            (&did[..10], did.len(), &did[26..]),
            (&[0, 0x18, 0, 0, 0, 7, 0, 0x40, 0, 0x80][..], 32, &[0xAB; 6][..])
        );
        assert_eq!(dec("RP-OSType", 2), b"Win10.0\0");
        assert_eq!(dec("RP-StartBitrate", 3), [0, 0, 0, 0]);
        assert_eq!(dec("RP-StreamingType", 4), [2, 0, 0, 0]); // HEVC SDR
                                                              // PS4 10.0: no streaming type; counters 0..3 -> 4
        let (req4, st4) = ctrl_request(&t, &params("10.0", DeviceType::Ps4, &key), &nonce, &mut Seq(0)).unwrap();
        assert_eq!(st4.out_counter, 4);
        assert_eq!((req4.get_header("RP-ControllerType"), req4.get_header("RP-StreamingType")), (Some("0"), None));
        assert!(req4.get_header("RP-StartBitrate").is_some());
        // legacy PS4: type 3, no start bitrate, different Did/OS payloads
        let (req0, st0) = ctrl_request(&t, &params("9.0", DeviceType::Ps4, &key), &nonce, &mut Seq(0)).unwrap();
        assert_eq!(st0.out_counter, 3);
        assert_eq!((req0.get_header("RP-ControllerType"), req0.get_header("RP-StartBitrate")), (Some("3"), None));
        let mut os = b64().decode(req0.get_header("RP-OSType").unwrap()).unwrap();
        st0.cipher.decrypt(&mut os, 2);
        assert_eq!(os, b"Win10.0.0\0");
    }

    #[test]
    fn ctrl_response_decodes_the_server_type_and_advances_the_counter() {
        let Some(t) = tables() else { return };
        let key = hex(&seq(7, 3));
        let (_, mut st) = ctrl_request(&t, &params("10.0", DeviceType::Ps4, &key), &seq(11, 200), &mut Seq(0)).unwrap();
        let mut server_type = vec![1u8, 0];
        st.cipher.encrypt(&mut server_type, 0);
        let resp = Response {
            status: 200,
            headers: vec![("RP-Server-Type".into(), b64().encode(&server_type)), ("RP-Prohibit".into(), "0".into())],
            ..Default::default()
        };
        let info = parse_ctrl_response(&mut st, DeviceType::Ps4, &resp).unwrap();
        assert_eq!(info, CtrlInfo { server_type: Some(1), full_hd: true, prohibited: false });
        assert_eq!(st.in_counter, 1);
        // PS4 server type != 1: no full HD; prohibit flag
        let (_, mut st) = ctrl_request(&t, &params("10.0", DeviceType::Ps4, &key), &seq(11, 200), &mut Seq(0)).unwrap();
        let mut two = vec![2u8, 0];
        st.cipher.encrypt(&mut two, 0);
        let resp = Response {
            status: 200,
            headers: vec![("RP-Server-Type".into(), b64().encode(&two)), ("RP-Prohibit".into(), "1".into())],
            ..Default::default()
        };
        let info = parse_ctrl_response(&mut st, DeviceType::Ps4, &resp).unwrap();
        assert_eq!((info.full_hd, info.prohibited), (false, true));
        // no header: counter untouched, defaults
        let resp = Response { status: 200, ..Default::default() };
        let info = parse_ctrl_response(&mut st, DeviceType::Ps4, &resp).unwrap();
        assert_eq!((info.server_type, info.prohibited), (None, false));
        assert_eq!(st.in_counter, 1);
        let rej = Response {
            status: 403,
            headers: vec![("RP-Application-Reason".into(), "80108b10".into())],
            ..Default::default()
        };
        assert_eq!(
            parse_ctrl_response(&mut st, DeviceType::Ps4, &rej),
            Err(CtrlResponseError::Rejected { status: 403, reason: "80108b10".into() })
        );
    }

    #[test]
    fn bad_keys_are_rejected_not_panicked_on() {
        let Some(t) = tables() else { return };
        assert!(ctrl_request(&t, &params("1.0", DeviceType::Ps5, "zz"), &[0; 16], &mut Seq(0)).is_err());
        assert!(ctrl_request(&t, &params("1.0", DeviceType::Ps5, "00"), &[0; 16], &mut Seq(0)).is_err()); // wrong length
        let zero_key = hex(&[0; 16]);
        let mut p = params("1.0", DeviceType::Ps5, &zero_key);
        p.regist_key_hex = "xyz";
        assert!(ctrl_request(&t, &p, &[0; 16], &mut Seq(0)).is_err());
    }
}
