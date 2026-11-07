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
    /// Frame generation on or off. Off: frames are shown as they arrive. On: the client makes extra frames to fill the display's refresh
    /// rate (up to 240), and the adaptive frame rate steps the rate down in steps of 5 fps when the Mac cannot keep it up.
    #[serde(default = "default_framegen")]
    pub framegen_on: bool,
    /// When the Mac cannot keep every filter running at the rate asked for, lower the frame rate gently, in steps of 5 fps (to the closest
    /// multiple of 5 it can hold, at most 10 at a time, only when the load stays too high for a couple of seconds) and keep the picture as it is, instead of switching filters off. On by default.
    #[serde(default = "default_true")]
    pub adaptive_fps: bool,
    /// Smooth mode's smoothing buffer: the picture is held back by how late the slowest recent frames arrive (at most 25 ms), so a frame
    /// that comes late is still there in time and the motion does not stall (the 20-30 ms gaps in frame pacing). Costs that much latency
    /// in Smooth mode; off, the picture is as early as the network's best case allowed. On by default.
    #[serde(default = "default_true")]
    pub pacing_buffer: bool,
    /// Let the renderer make and present frames above the display's refresh rate (frame generation up to 240 on a 60 Hz screen). The
    /// display shows its newest frame at each refresh, so this lowers latency and drops older pictures; it does not show more of them, and
    /// can tear in full screen. Costs GPU time (the adaptive frame rate still lowers it when the Mac cannot keep up). Off by default.
    #[serde(default)]
    pub uncapped_rate: bool,
    /// Run the MetalFX upscaler in linear light (the FSR engine has no such mode). On by default.
    #[serde(default = "default_true")]
    pub linear_light: bool,
    /// Each picture filter has its own switch, so it can be turned off without losing its slider value: the sharpness blend with a plain
    /// enlargement (off: the upscaler's picture as it draws it), adaptive sharpening (RCAS) and colour saturation. All on by default.
    #[serde(default = "default_true")]
    pub sharpness_on: bool,
    #[serde(default = "default_true")]
    pub rcas_on: bool,
    #[serde(default = "default_true")]
    pub saturation_on: bool,
    /// How generated frames are made: `"smooth"` (the default: interpolated, about one source frame of added latency, the
    /// cleanest picture) or `"zero_latency"` (shown in the app as "Low latency": predicted forward, no added latency, more artifacts).
    #[serde(default = "default_frame_mode")]
    pub frame_mode: String,
    /// Shift the picture by camera rotation that the stick has commanded but the stream does not show yet. On by default: it
    /// learns the game's response and switches itself off when the stick does not explain the motion.
    #[serde(default = "default_camera_comp")]
    pub camera_comp: bool,
    /// Who upscales the picture: `metalfx` (Apple's spatial scaler, the default; it also makes the generated frames at about 2x) or
    /// `fsr` (AMD's FidelityFX Super Resolution 1: EASU upscaling, ported to Metal). Our own interpolator makes the other generated frames.
    /// A file from a build that had the VideoToolbox engine (`videotoolbox`, and the retired `vt_latency`) opens with `metalfx`.
    #[serde(default = "default_engine")]
    pub engine: String,
    /// Sharpness in percent (0..=200). With the upscaler: 0 smooth enlargement, 100 as the upscaler draws it, above that sharper. Without it:
    /// 50 leaves the picture alone, above sharpens, below softens.
    #[serde(default = "default_sharpness")]
    pub sharpness: u8,
    /// Adaptive sharpening (AMD's RCAS) applied to the upscaled picture, in percent (0..=200): 0 is off, 100 is the strength it was measured
    /// best at, 200 is twice the lobe. Independent of `sharpness`, which blends the upscaler with a plain enlargement. Only with upscaling.
    #[serde(default = "default_rcas")]
    pub rcas: u8,
    /// Colour saturation in percent: 100 = as the stream is, above = more vivid, below = paler.
    #[serde(default = "default_saturation")]
    pub saturation: u8,
    /// Fill the black bars around the picture (a wide window, a narrower picture) with a soft blurred glow of it. Opt-in.
    #[serde(default)]
    pub immersion: bool,
    /// Show the debug tools in the stream bar, such as switching the engine on the running stream. Off by default.
    #[serde(default)]
    pub debug: bool,
    /// Rumble and haptics strength in percent of what the console asks for: 0 to 200, 100 as asked.
    #[serde(default = "default_rumble_pct")]
    pub rumble_pct: u16,
    /// Adaptive-trigger strength in percent of the pad's full power: 0 to 100 (the pad cannot go above full).
    #[serde(default = "default_trigger_pct")]
    pub trigger_pct: u16,
    /// Which version of the settings this was saved by (see `migrated`).
    #[serde(default)]
    pub settings_version: u8,
}

/// The settings as this version of the app writes them.
pub const SETTINGS_VERSION: u8 = 4;

/// The upscaling multiples offered, as the setting writes them and as the factor they stand for ("1.33x" is exactly 4/3, so 1080p
/// becomes 1440p).
const UPSCALE_STEPS: [(&str, f64); 3] = [("1.33x", 4.0 / 3.0), ("1.5x", 1.5), ("2x", 2.0)];

impl StreamOptions {
    /// How many times larger than the stream the picture is drawn when the GPU upscales (0 = not at all). The first versions had the
    /// choices "up to 1440p" and "up to 4K", which are heights: they mean whichever multiple of the chosen resolution comes nearest.
    pub fn upscale_factor(&self) -> f64 {
        let cap = match self.upscale.as_str() {
            "1440p" => 1440.0,
            "2160p" => 2160.0,
            other => return UPSCALE_STEPS.iter().find(|(name, _)| *name == other).map_or(0.0, |(_, f)| *f),
        };
        let wanted = cap / f64::from(self.height.max(1));
        UPSCALE_STEPS
            .iter()
            .map(|(_, f)| *f)
            .min_by(|a, b| (a - wanted).abs().total_cmp(&(b - wanted).abs()))
            .unwrap_or(0.0)
    }

    /// Settings saved by an older version, brought up to date: the stream always runs at 60 fps (the setting is gone), an old upscaling
    /// cap becomes a multiple, a sharpness that is still the old default of 50 becomes the new default of 60, the retired VideoToolbox
    /// engine becomes MetalFX, and the old frame-generation multiple (or display rate) becomes the frame-generation switch of version 4
    /// (the rate itself is automatic now: the display's, lowered by the adaptive frame rate). Once saved the version is current and a
    /// chosen 50 stays.
    pub fn migrated(mut self) -> Self {
        self.fps = 60;
        if self.settings_version < 2 && self.sharpness == 50 {
            self.sharpness = default_sharpness();
        }
        if matches!(self.upscale.as_str(), "1440p" | "2160p") {
            let factor = self.upscale_factor();
            if let Some((name, _)) = UPSCALE_STEPS.iter().find(|(_, f)| *f == factor) {
                self.upscale = (*name).into();
            }
        }
        if self.engine == "videotoolbox" {
            self.engine = default_engine();
        }
        if self.settings_version < 4 {
            self.framegen_on =
                matches!(self.frame_gen, 15 | 20 | 25 | 30) || (self.frame_gen == 0 && self.display_fps > 60);
        }
        self.settings_version = SETTINGS_VERSION;
        self
    }
}

fn default_framegen() -> bool {
    true
}
fn default_true() -> bool {
    true
}

fn default_rumble_pct() -> u16 {
    100
}
fn default_trigger_pct() -> u16 {
    100
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
fn default_rcas() -> u8 {
    100
}
fn default_sharpness() -> u8 {
    60
}
fn default_engine() -> String {
    "metalfx".into()
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
        if !matches!(self.upscale.as_str(), "off" | "1.33x" | "1.5x" | "2x" | "1440p" | "2160p") {
            return Err(format!(
                "unsupported upscaling {:?} (use off, 1.33x, 1.5x or 2x)",
                self.upscale.chars().take(12).collect::<String>()
            ));
        }
        if self.rumble_pct > 200 || self.trigger_pct > 100 {
            return Err(format!(
                "unsupported controller strength (rumble {} %, adaptive trigger {} %; use up to 200 and 100)",
                self.rumble_pct, self.trigger_pct
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
        if self.rcas > 200 {
            return Err(format!("adaptive sharpening {} is not between 0 and 200", self.rcas));
        }
        if self.sharpness > 200 {
            return Err(format!("sharpness {} is not between 0 and 200", self.sharpness));
        }
        if self.saturation > 200 {
            return Err(format!("saturation {} is not between 0 and 200", self.saturation));
        }
        if !matches!(self.engine.as_str(), "metalfx" | "fsr" | "videotoolbox") {
            return Err(format!(
                "unknown engine {:?} (use metalfx or fsr)",
                self.engine.chars().take(16).collect::<String>()
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
            framegen_on: default_framegen(),
            adaptive_fps: true,
            pacing_buffer: true,
            uncapped_rate: false,
            linear_light: true,
            sharpness_on: true,
            rcas_on: true,
            saturation_on: true,
            frame_mode: default_frame_mode(),
            camera_comp: default_camera_comp(),
            engine: default_engine(),
            sharpness: default_sharpness(),
            rcas: default_rcas(),
            saturation: default_saturation(),
            immersion: false,
            debug: false,
            rumble_pct: default_rumble_pct(),
            trigger_pct: default_trigger_pct(),
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
    fn immersion_is_opt_in_and_old_settings_load_without_it() {
        assert!(!StreamOptions::default().immersion, "off unless the user turns it on");
        let old: StreamOptions =
            serde_json::from_str(r#"{"width":1280,"height":720,"fps":60,"bitrate":-1,"hevc":true,"hdr":false}"#)
                .unwrap();
        assert!(!old.immersion);
        let on = StreamOptions { immersion: true, ..Default::default() };
        let round: StreamOptions = serde_json::from_str(&serde_json::to_string(&on).unwrap()).unwrap();
        assert!(round.immersion, "and the choice is saved");
    }

    #[test]
    fn controller_strengths_default_to_the_consoles_own_and_are_bounded() {
        let ok = StreamOptions::default();
        assert_eq!((ok.rumble_pct, ok.trigger_pct), (100, 100));
        // settings saved before these existed load with the defaults
        let old: StreamOptions =
            serde_json::from_str(r#"{"width":1280,"height":720,"fps":60,"bitrate":-1,"hevc":true,"hdr":false}"#)
                .unwrap();
        assert_eq!((old.rumble_pct, old.trigger_pct), (100, 100));
        for (r, t) in [(0, 0), (100, 100), (200, 100), (10, 90)] {
            assert!(StreamOptions { rumble_pct: r, trigger_pct: t, ..ok.clone() }.validate().is_ok(), "{r} {t}");
        }
        for (r, t) in [(201, 100), (100, 101), (u16::MAX, 0), (0, u16::MAX)] {
            assert!(StreamOptions { rumble_pct: r, trigger_pct: t, ..ok.clone() }.validate().is_err(), "{r} {t}");
        }
    }

    #[test]
    fn upscaling_is_a_multiple_of_the_stream_resolution() {
        let with =
            |u: &str, h: i32| StreamOptions { upscale: u.into(), height: h, width: h * 16 / 9, ..Default::default() };
        for u in ["off", "1.33x", "1.5x", "2x"] {
            assert!(with(u, 1080).validate().is_ok(), "{u}");
        }
        for u in ["3x", "1.25x", "4k", "", "2X"] {
            assert!(with(u, 1080).validate().is_err(), "{u}");
        }
        assert_eq!(with("off", 1080).upscale_factor(), 0.0);
        assert!(
            (with("1.33x", 1080).upscale_factor() - 4.0 / 3.0).abs() < 1e-9,
            "1.33x is exactly 4/3: 1080p becomes 1440p"
        );
        assert_eq!(with("1.5x", 720).upscale_factor(), 1.5);
        assert_eq!(with("2x", 540).upscale_factor(), 2.0);
    }

    #[test]
    fn the_old_upscaling_caps_become_the_nearest_multiple_of_the_chosen_resolution() {
        // "up to 1440p" / "up to 4K" were heights; what they gave depends on the stream resolution
        let old = |u: &str, h: i32| StreamOptions {
            upscale: u.into(),
            height: h,
            width: h * 16 / 9,
            settings_version: 2,
            ..Default::default()
        };
        for (cap, height, now) in [
            ("1440p", 1080, "1.33x"),
            ("1440p", 720, "2x"),
            ("1440p", 540, "2x"),
            ("2160p", 1080, "2x"),
            ("2160p", 720, "2x"),
            ("2160p", 360, "2x"),
            ("off", 1080, "off"),
        ] {
            assert_eq!(old(cap, height).migrated().upscale, now, "{cap} at {height}p");
        }
        // the legacy value still validates, so a settings file from before loads
        assert!(old("1440p", 1080).validate().is_ok());
        // a value chosen with this version is left alone, and migrating twice changes nothing
        let chosen = StreamOptions { upscale: "1.5x".into(), settings_version: SETTINGS_VERSION, ..Default::default() };
        assert_eq!(chosen.clone().migrated().upscale, "1.5x");
        assert_eq!(old("1440p", 1080).migrated().migrated().upscale, "1.33x");
        // even before it is migrated the legacy cap has a sensible factor
        assert!((old("1440p", 1080).upscale_factor() - 4.0 / 3.0).abs() < 1e-9);
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
        for u in ["off", "1440p", "2160p", "1.33x", "1.5x", "2x"] {
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
            StreamOptions { sharpness: 201, ..ok.clone() },
            StreamOptions { rcas: 201, ..ok.clone() },
            StreamOptions { saturation: 201, ..ok.clone() },
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
        for engine in ["metalfx", "fsr"] {
            assert!(StreamOptions { engine: engine.into(), ..ok.clone() }.validate().is_ok(), "{engine}");
        }
        // a settings file written while the VideoToolbox engine existed still loads, and opens with MetalFX
        let vt = StreamOptions { engine: "videotoolbox".into(), ..ok.clone() };
        assert!(vt.validate().is_ok(), "an old file must not be thrown away for its engine");
        assert_eq!(vt.migrated().engine, "metalfx");
        for bad in [
            StreamOptions { engine: "dlss".into(), ..ok.clone() },
            StreamOptions { engine: "fsr4".into(), ..ok.clone() },
            StreamOptions { engine: "builtin".into(), ..ok.clone() },
            StreamOptions { engine: String::new(), ..ok.clone() },
            StreamOptions { engine: "v".repeat(10_000), ..ok.clone() },
        ] {
            assert!(bad.validate().is_err(), "{bad:?}");
        }
        // the engine and the debug switch default for every older file, and the retired VideoToolbox latency key is ignored
        let json = serde_json::to_string(&ok).unwrap();
        let old: StreamOptions =
            serde_json::from_str(&json.replace(",\"engine\":\"metalfx\"", "").replace(",\"debug\":false", "")).unwrap();
        assert_eq!((old.engine.as_str(), old.debug), ("metalfx", false));
        let with_latency: StreamOptions = serde_json::from_str(
            &json.replace("\"engine\":\"metalfx\"", "\"vt_latency\":\"low\",\"engine\":\"videotoolbox\""),
        )
        .unwrap();
        assert_eq!(with_latency.migrated().engine, "metalfx", "an old vt_latency key is not an error");
        let d = StreamOptions::default();
        assert_eq!((d.engine.as_str(), d.debug), ("metalfx", false), "debug tools are off by default");
        // adaptive sharpening: 100 % by default, the whole range validates, and a file from before it existed gets the default
        assert_eq!(d.rcas, 100);
        for r in [0u8, 100, 200] {
            assert!(StreamOptions { rcas: r, ..ok.clone() }.validate().is_ok(), "{r}");
        }
        let no_rcas: StreamOptions = serde_json::from_str(&json.replace(",\"rcas\":100", "")).unwrap();
        assert_eq!(no_rcas.rcas, 100, "an older settings file has no rcas key");
        // a file written by an older version still has the two choices this replaced: they are ignored, not an error
        let older: StreamOptions = serde_json::from_str(&json.replace(
            "\"engine\":\"metalfx\"",
            "\"framegen\":\"builtin\",\"upscaler\":\"videotoolbox\",\"engine\":\"metalfx\"",
        ))
        .unwrap();
        assert_eq!(older.engine, "metalfx");
    }

    #[test]
    fn every_filter_has_its_own_switch_and_the_frame_rate_is_automatic() {
        let d = StreamOptions::default();
        assert!(d.linear_light && d.sharpness_on && d.rcas_on && d.saturation_on, "all filters on by default");
        assert!(d.adaptive_fps, "lowering the frame rate instead of dropping filters is the default");
        assert!(d.pacing_buffer, "the smoothing buffer is on by default");
        assert!(!d.uncapped_rate, "rendering above the display's rate is something to choose");
        assert!(d.framegen_on, "frame generation is on by default; its rate is the display's, no setting for it");
        // a file from before the switches existed has them all on, and keeps its sliders
        let ok = StreamOptions::default();
        let json = serde_json::to_string(&ok).unwrap();
        let mut v: serde_json::Value = serde_json::from_str(&json).unwrap();
        for k in ["linear_light", "sharpness_on", "rcas_on", "saturation_on", "adaptive_fps"] {
            v.as_object_mut().unwrap().remove(k);
        }
        v.as_object_mut().unwrap().remove("pacing_buffer");
        v.as_object_mut().unwrap().remove("uncapped_rate");
        let old: StreamOptions = serde_json::from_value(v).unwrap();
        assert!(old.linear_light && old.sharpness_on && old.rcas_on && old.saturation_on && old.adaptive_fps);
        assert!(
            old.pacing_buffer && !old.uncapped_rate,
            "files from before the two options get the buffer and the display's rate"
        );
        // a file written while the target-rate slider existed still loads: the key is ignored
        let mut v: serde_json::Value = serde_json::from_str(&json).unwrap();
        v["target_fps"] = serde_json::json!(90);
        let with_target: StreamOptions = serde_json::from_value(v).unwrap();
        assert_eq!(with_target, ok, "the retired target_fps key is not an error and changes nothing");
    }

    #[test]
    fn the_old_frame_generation_multiples_become_the_frame_generation_switch() {
        let from = |frame_gen: u8, display_fps: u16| {
            StreamOptions { frame_gen, display_fps, settings_version: 3, ..Default::default() }.migrated()
        };
        for multiple in [15u8, 20, 25, 30] {
            assert!(from(multiple, 60).framegen_on, "{multiple} tenths was on");
        }
        assert!(!from(10, 60).framegen_on, "1.0x was Off");
        // a file from before the multiples existed had only a display rate
        assert!(!from(0, 60).framegen_on);
        assert!(!from(0, 30).framegen_on);
        assert!(from(0, 144).framegen_on);
        assert!(from(0, 75).framegen_on);
        assert!(from(20, 60).validate().is_ok());
        // a file already at this version is not touched again
        let chosen = StreamOptions { framegen_on: false, frame_gen: 20, ..Default::default() };
        let m = chosen.clone().migrated();
        assert!(!m.framegen_on);
        assert_eq!(m.clone().migrated(), m, "migrating twice changes nothing");
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
