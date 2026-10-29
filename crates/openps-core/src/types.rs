//! Plain serialisable types shared with the front end.

use openps_discovery::{DeviceType, Status};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Device {
    Ps4,
    Ps5,
}

impl From<DeviceType> for Device {
    fn from(d: DeviceType) -> Self {
        match d {
            DeviceType::Ps4 => Device::Ps4,
            DeviceType::Ps5 => Device::Ps5,
        }
    }
}

impl From<Device> for DeviceType {
    fn from(d: Device) -> Self {
        match d {
            Device::Ps4 => DeviceType::Ps4,
            Device::Ps5 => DeviceType::Ps5,
        }
    }
}

impl Device {
    /// Protocol version to try first: PS5 `1.0`, PS4 `10.0` (the init step downgrades for older firmware).
    pub fn default_version(self) -> &'static str {
        match self {
            Device::Ps5 => "1.0",
            Device::Ps4 => "10.0",
        }
    }
}

/// A paired console. Contains no secrets: the registration key and RP-Key live in the secret store.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsoleRecord {
    /// Console MAC without separators, upper-case; also the discovery `host-id`.
    pub id: String,
    pub device: Device,
    pub nickname: String,
    pub ap_name: String,
    /// Protocol version that last worked.
    pub version: String,
    /// Last address the console was seen at.
    pub host: Option<String>,
}

/// Secrets for one console (stored in the OS keychain, never in the JSON file).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsoleSecrets {
    pub regist_key: String,
    pub rp_key: String,
}

impl std::fmt::Debug for ConsoleSecrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ConsoleSecrets(<redacted>)")
    }
}

// ---- input validation (everything that arrives from the UI or the network) ---------------------------------------------------

/// The PIN is exactly 8 ASCII digits. (Parsing as a number alone would accept `+1234567`.)
pub fn parse_pin(s: &str) -> Result<u32, String> {
    let t = s.trim();
    if t.len() == 8 && t.bytes().all(|b| b.is_ascii_digit()) {
        Ok(t.parse().expect("8 digits fit in u32"))
    } else {
        Err("the PIN is the 8 digits shown on the console".into())
    }
}

/// A console address: an IP literal (IPv6 may be bracketed) or a plain hostname. No ports, paths, userinfo, spaces or
/// control characters: this string goes into a network request and is stored for later sessions.
pub fn validate_host(host: &str) -> Result<(), String> {
    let bad = || format!("{:?} is not a valid console address", host.chars().take(40).collect::<String>());
    if host.is_empty() || host.len() > 253 {
        return Err(bad());
    }
    let bare = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')).unwrap_or(host);
    if bare.parse::<std::net::IpAddr>().is_ok() {
        return Ok(());
    }
    // hostname: dot-separated labels of 1..=63 ASCII letters, digits and inner hyphens; the last label is not numeric
    let labels: Vec<&str> = host.split('.').collect();
    let label_ok = |l: &&str| {
        (1..=63).contains(&l.len())
            && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            && !l.starts_with('-')
            && !l.ends_with('-')
    };
    if labels.iter().all(label_ok) && !labels.last().is_some_and(|l| l.bytes().all(|b| b.is_ascii_digit())) {
        Ok(())
    } else {
        Err(bad())
    }
}

/// Protocol versions the registration step speaks. The value is sent in an HTTP header, so it must never be free text.
pub fn validate_version(v: &str) -> Result<(), String> {
    if matches!(v, "1.0" | "10.0" | "9.0" | "8.0") {
        Ok(())
    } else {
        Err(format!("unsupported protocol version {:?}", v.chars().take(16).collect::<String>()))
    }
}

/// Text that came from the network (console names): drop control and invisible formatting characters (including the
/// bidirectional overrides used to disguise text), trim, and cap the length.
pub fn clean_text(s: &str, max_chars: usize) -> String {
    let invisible = |c: char| {
        c.is_control()
            || matches!(c, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{2069}' | '\u{FEFF}')
    };
    s.chars().filter(|c| !invisible(*c)).collect::<String>().trim().chars().take(max_chars).collect()
}

/// Longest console name we keep or show.
pub const MAX_NAME_CHARS: usize = 64;

/// Normalise a MAC / host id for comparison: drop separators, upper-case.
pub fn normalise_id(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_ascii_uppercase()
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DiscoveredConsole {
    pub host_id: String,
    pub name: String,
    pub device: Device,
    /// "ready", "standby" or "other".
    pub status: String,
    pub addr: String,
    /// The paired console this is, if any.
    pub registered_id: Option<String>,
}

pub fn status_text(s: Status) -> &'static str {
    match s {
        Status::Ready => "ready",
        Status::Standby => "standby",
        Status::Other(_) => "other",
    }
}

/// Non-secret PSN account facts kept for registration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PsnAccountInfo {
    pub online_id: String,
    /// Decimal string: a u64 does not fit a JavaScript number.
    pub account_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamOptions {
    pub width: i32,
    pub height: i32,
    pub fps: i32,
    /// kbps, or -1 for automatic.
    pub bitrate: i32,
    pub hevc: bool,
    pub hdr: bool,
    /// Output volume, 0..=100 (applied by the audio sink, not part of the launch spec).
    #[serde(default = "default_volume")]
    pub volume: u8,
}

fn default_volume() -> u8 {
    100
}

impl StreamOptions {
    /// Values the protocol actually supports. The launch layer would silently clamp anything else, which would make a
    /// settings screen lie about what was applied, so reject it up front instead.
    pub fn validate(&self) -> Result<(), String> {
        if !matches!(self.height, 360 | 540 | 720 | 1080) {
            return Err(format!("unsupported resolution height {} (use 360, 540, 720 or 1080)", self.height));
        }
        if !matches!(self.fps, 30 | 60) {
            return Err(format!("unsupported frame rate {} (use 30 or 60)", self.fps));
        }
        if !matches!(self.bitrate, -1 | 2000 | 6000 | 10000 | 15000) {
            return Err(format!(
                "unsupported bitrate {} (use automatic, 2000, 6000, 10000 or 15000 kbps)",
                self.bitrate
            ));
        }
        if self.volume > 100 {
            return Err(format!("volume {} is out of range (0 to 100)", self.volume));
        }
        if self.hdr && !self.hevc {
            return Err("HDR needs HEVC".into());
        }
        Ok(())
    }
}

impl Default for StreamOptions {
    fn default() -> Self {
        Self { width: 1280, height: 720, fps: 60, bitrate: -1, hevc: false, hdr: false, volume: default_volume() }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SessionState {
    Idle,
    /// Opening the control channel with the console.
    Connecting,
    TestingNetwork,
    Handshaking {
        attempt: u32,
    },
    Connected {
        takion_version: i32,
    },
    Closed {
        reason: Option<String>,
    },
    Failed {
        reason: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_options_validation() {
        let ok = StreamOptions::default();
        assert!(ok.validate().is_ok());
        for h in [360, 540, 720, 1080] {
            assert!(StreamOptions { height: h, ..ok.clone() }.validate().is_ok(), "{h}");
        }
        for bad in [
            StreamOptions { height: 1234, ..ok.clone() },
            StreamOptions { height: 0, ..ok.clone() },
            StreamOptions { height: -720, ..ok.clone() },
            StreamOptions { fps: 45, ..ok.clone() },
            StreamOptions { fps: 0, ..ok.clone() },
            StreamOptions { bitrate: 5, ..ok.clone() },
            StreamOptions { bitrate: i32::MAX, ..ok.clone() },
            StreamOptions { hevc: false, hdr: true, ..ok.clone() },
            StreamOptions { volume: 101, ..ok.clone() },
            StreamOptions { volume: 255, ..ok.clone() },
        ] {
            assert!(bad.validate().is_err(), "{bad:?}");
        }
        assert!(StreamOptions { hevc: true, hdr: true, ..ok.clone() }.validate().is_ok());
        assert!(StreamOptions { volume: 0, ..ok.clone() }.validate().is_ok());
        assert!(StreamOptions { volume: 100, ..ok.clone() }.validate().is_ok());
        // settings saved before volume existed still load, at full volume
        let old: StreamOptions =
            serde_json::from_str(r#"{"width":1280,"height":720,"fps":60,"bitrate":-1,"hevc":true,"hdr":false}"#)
                .unwrap();
        assert_eq!(old.volume, 100);
    }

    #[test]
    fn ids_normalise() {
        assert_eq!(normalise_id("aa:bb-CC dd"), "AABBCCDD");
        assert_eq!(normalise_id("001122334455"), "001122334455");
    }

    #[test]
    fn secrets_never_debug_print() {
        assert!(!format!("{:?}", ConsoleSecrets { regist_key: "K".into(), rp_key: "R".into() }).contains('K'));
    }

    #[test]
    fn state_serialises_with_a_tag() {
        assert_eq!(
            serde_json::to_string(&SessionState::Handshaking { attempt: 2 }).unwrap(),
            r#"{"state":"handshaking","attempt":2}"#
        );
        assert_eq!(serde_json::to_string(&SessionState::Idle).unwrap(), r#"{"state":"idle"}"#);
    }
}
