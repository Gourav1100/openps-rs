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

/// The one stored form of an address: an IPv6 literal without brackets (what `IpAddr::parse` and the resolver understand).
pub fn canonical_host(host: &str) -> String {
    host.strip_prefix('[').and_then(|h| h.strip_suffix(']')).unwrap_or(host).to_string()
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
            || matches!(c, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{2069}' | '\u{FEFF}' | '\u{2028}' | '\u{2029}' | '\u{061C}' | '\u{180E}' | '\u{00AD}' | '\u{FFF9}'..='\u{FFFB}' | '\u{E0000}'..='\u{E007F}')
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
    /// Client-side upscaling with MetalFX: `"off"`, `"1440p"` or `"2160p"` (a cap on the output height).
    #[serde(default = "default_upscale")]
    pub upscale: String,
    /// Frames per second to present. The console streams at most 60; above that the client generates frames.
    #[serde(default = "default_display_fps")]
    pub display_fps: u16,
    /// Frame generation as a multiple of the stream's rate, in tenths: 10 = off (frames are shown as they arrive), 15 = 1.5x, 20 = 2x,
    /// 25 = 2.5x, 30 = 3x. The rate presented is the stream's rate times this, never above what the display can show (and it steps down
    /// when the Mac cannot keep it). 0 = not chosen yet (settings saved before this existed): `display_fps` is used as it was.
    #[serde(default)]
    pub frame_gen: u8,
    /// How generated frames are made: `"smooth"` (the default: interpolated, about one source frame of added latency, the
    /// cleanest picture) or `"zero_latency"` (shown in the app as "Low latency": predicted forward, no added latency, more artifacts).
    #[serde(default = "default_frame_mode")]
    pub frame_mode: String,
    /// Shift the picture by camera rotation that the stick has commanded but the stream does not show yet. On by default: it
    /// learns the game's response and switches itself off when the stick does not explain the motion.
    #[serde(default = "default_camera_comp")]
    pub camera_comp: bool,
    /// Who makes the interpolated frames in Smooth mode: `builtin` (ours) or `metalfx` (Apple's, experimental).
    #[serde(default = "default_framegen")]
    pub framegen: String,
    /// Sharpness in percent (0..=200). With the upscaler: 0 smooth enlargement, 100 as the upscaler draws it, above that sharper. Without it:
    /// 50 leaves the picture alone, above sharpens, below softens.
    #[serde(default = "default_sharpness")]
    pub sharpness: u8,
    /// Colour saturation in percent: 100 = as the stream is, above = more vivid, below = paler.
    #[serde(default = "default_saturation")]
    pub saturation: u8,
    /// Which version of the settings this was saved by (see `migrated`).
    #[serde(default)]
    pub settings_version: u8,
}

/// The settings as this version of the app writes them.
pub const SETTINGS_VERSION: u8 = 2;

impl StreamOptions {
    /// Settings saved by an older version, brought up to date: the stream always runs at 60 fps (the setting is gone), and a sharpness that
    /// is still the old default of 50 becomes the new default of 60. Once saved the version is current and a chosen 50 stays.
    pub fn migrated(mut self) -> Self {
        self.fps = 60;
        if self.settings_version < 2 && self.sharpness == 50 {
            self.sharpness = default_sharpness();
        }
        self.settings_version = SETTINGS_VERSION;
        self
    }
}

fn default_upscale() -> String {
    "off".into()
}
fn default_display_fps() -> u16 {
    60
}
fn default_frame_mode() -> String {
    "smooth".into()
}
fn default_saturation() -> u8 {
    100
}
fn default_sharpness() -> u8 {
    60
}
fn default_framegen() -> String {
    "builtin".into()
}
fn default_camera_comp() -> bool {
    true
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
        if self.width != self.height * 16 / 9 {
            return Err(format!(
                "resolution {}x{} is not 16:9 (use 640x360, 960x540, 1280x720 or 1920x1080)",
                self.width, self.height
            ));
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
        if !matches!(self.upscale.as_str(), "off" | "1440p" | "2160p") {
            return Err(format!(
                "unsupported upscaling {:?} (use off, 1440p or 2160p)",
                self.upscale.chars().take(12).collect::<String>()
            ));
        }
        if !matches!(self.frame_gen, 0 | 10 | 15 | 20 | 25 | 30) {
            return Err(format!(
                "unsupported frame generation {} (use 10, 15, 20, 25 or 30: tenths of the stream rate)",
                self.frame_gen
            ));
        }
        if !matches!(self.display_fps, 30 | 60 | 75 | 120 | 144) {
            return Err(format!("unsupported display rate {} (use 30, 60, 75, 120 or 144)", self.display_fps));
        }
        if self.sharpness > 200 {
            return Err(format!("sharpness {} is not between 0 and 200", self.sharpness));
        }
        if self.saturation > 200 {
            return Err(format!("saturation {} is not between 0 and 200", self.saturation));
        }
        if !matches!(self.framegen.as_str(), "builtin" | "metalfx") {
            return Err(format!(
                "unknown frame generation engine {:?}",
                self.framegen.chars().take(16).collect::<String>()
            ));
        }
        if !matches!(self.frame_mode.as_str(), "zero_latency" | "smooth") {
            return Err(format!(
                "unsupported frame mode {:?} (use zero_latency or smooth)",
                self.frame_mode.chars().take(16).collect::<String>()
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
        Self {
            width: 1280,
            height: 720,
            fps: 60,
            bitrate: -1,
            hevc: false,
            hdr: false,
            volume: default_volume(),
            upscale: default_upscale(),
            display_fps: default_display_fps(),
            frame_gen: 20,
            frame_mode: default_frame_mode(),
            camera_comp: default_camera_comp(),
            framegen: default_framegen(),
            sharpness: default_sharpness(),
            saturation: default_saturation(),
            settings_version: SETTINGS_VERSION,
        }
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
    fn old_settings_are_brought_up_to_date_once() {
        // always 60 fps: a saved 30 (or a hand-edited file) comes back as 60
        let old = StreamOptions { fps: 30, sharpness: 50, settings_version: 0, ..Default::default() };
        let m = old.clone().migrated();
        assert_eq!(
            (m.fps, m.sharpness, m.settings_version),
            (60, 60, SETTINGS_VERSION),
            "the old default sharpness becomes the new one"
        );
        // an old file that has no version at all reads as version 0
        let from_file: StreamOptions = serde_json::from_str(
            r#"{"width":1280,"height":720,"fps":30,"bitrate":-1,"hevc":true,"hdr":false,"sharpness":50}"#,
        )
        .unwrap();
        assert_eq!(from_file.settings_version, 0);
        assert_eq!(from_file.migrated().sharpness, 60);
        // once saved by this version, a chosen 50 stays
        let chosen = StreamOptions { sharpness: 50, settings_version: SETTINGS_VERSION, ..Default::default() };
        assert_eq!(chosen.clone().migrated().sharpness, 50);
        // any other sharpness is left alone, even in an old file
        assert_eq!(StreamOptions { sharpness: 80, settings_version: 0, ..Default::default() }.migrated().sharpness, 80);
        assert_eq!(StreamOptions::default().sharpness, 60);
        assert_eq!(StreamOptions::default().fps, 60);
        assert_eq!(
            chosen.migrated().migrated(),
            StreamOptions { sharpness: 50, ..Default::default() },
            "migrating twice changes nothing"
        );
    }

    #[test]
    fn stream_options_validation() {
        let ok = StreamOptions::default();
        assert!(ok.validate().is_ok());
        for h in [360, 540, 720, 1080] {
            assert!(StreamOptions { height: h, width: h * 16 / 9, ..ok.clone() }.validate().is_ok(), "{h}");
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
        assert_eq!(
            (old.upscale.as_str(), old.display_fps, old.frame_mode.as_str(), old.camera_comp),
            ("off", 60, "smooth", true)
        );
        for u in ["off", "1440p", "2160p"] {
            assert!(StreamOptions { upscale: u.into(), ..ok.clone() }.validate().is_ok(), "{u}");
        }
        for f in [30, 60, 75, 120, 144] {
            assert!(StreamOptions { display_fps: f, ..ok.clone() }.validate().is_ok(), "{f}");
        }
        for g in [0, 10, 15, 20, 25, 30] {
            assert!(StreamOptions { frame_gen: g, ..ok.clone() }.validate().is_ok(), "{g}");
        }
        assert_eq!(
            old.frame_gen, 0,
            "settings saved before frame generation modes existed keep their display rate until a mode is chosen"
        );
        for bad in [
            StreamOptions { upscale: "4k".into(), ..ok.clone() },
            StreamOptions { upscale: String::new(), ..ok.clone() },
            StreamOptions { upscale: "x".repeat(10_000), ..ok.clone() },
            StreamOptions { frame_gen: 12, ..ok.clone() },
            StreamOptions { frame_gen: 40, ..ok.clone() },
            StreamOptions { display_fps: 90, ..ok.clone() },
            StreamOptions { display_fps: 0, ..ok.clone() },
            StreamOptions { display_fps: 240, ..ok.clone() },
            StreamOptions { frame_mode: "interpolate".into(), ..ok.clone() },
            StreamOptions { frame_mode: "\u{0}".into(), ..ok.clone() },
            StreamOptions { framegen: "dlss".into(), ..ok.clone() },
            StreamOptions { sharpness: 201, ..ok.clone() },
            StreamOptions { saturation: 201, ..ok.clone() },
            StreamOptions { framegen: String::new(), ..ok.clone() },
            StreamOptions { framegen: "m".repeat(10_000), ..ok.clone() },
        ] {
            assert!(bad.validate().is_err(), "{bad:?}");
        }
        assert!(StreamOptions { frame_mode: "smooth".into(), camera_comp: true, ..ok.clone() }.validate().is_ok());
        for (sharp, sat) in [(0, 0), (50, 100), (200, 200)] {
            assert!(
                StreamOptions { sharpness: sharp, saturation: sat, ..ok.clone() }.validate().is_ok(),
                "{sharp}/{sat}"
            );
        }
        for engine in ["builtin", "metalfx"] {
            assert!(StreamOptions { framegen: engine.into(), ..ok.clone() }.validate().is_ok(), "{engine}");
        }
        // settings saved by an older version have no engine: they get the built-in one
        let old: StreamOptions =
            serde_json::from_str(&serde_json::to_string(&ok).unwrap().replace(",\"framegen\":\"builtin\"", ""))
                .unwrap();
        assert_eq!(old.framegen, "builtin");
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

    #[test]
    fn the_width_must_match_the_height() {
        let ok = StreamOptions::default();
        assert!(ok.validate().is_ok());
        for (w, h) in [(0, 720), (99999, 720), (1280, 1080), (1920, 720), (-1280, 720)] {
            assert!(StreamOptions { width: w, height: h, ..ok.clone() }.validate().is_err(), "{w}x{h}");
        }
        for (w, h) in [(640, 360), (960, 540), (1280, 720), (1920, 1080)] {
            assert!(StreamOptions { width: w, height: h, ..ok.clone() }.validate().is_ok(), "{w}x{h}");
        }
    }
}
