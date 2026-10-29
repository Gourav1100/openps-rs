//! Launch specification JSON (spec 02 section 5.3; `LaunchSpecification.java`), byte-for-byte template.

/// Highest bitrate Auto may request: 100 Mbit/s, the most the original accepts for a custom bitrate. Asking a real PS5 for 800 Mbit/s
/// made it end the stream seconds after the start ("video send timeout").
pub const MAX_AUTO_KBPS: i32 = 100_000;

#[derive(Clone, Debug)]
pub struct LaunchParams {
    pub width: i32,
    pub height: i32,
    pub fps: i32,
    /// Requested bitrate in kbps, or -1 for auto.
    pub bitrate: i32,
    /// For auto (`bitrate == -1`): what the link to the console can carry, in kbps (see `openps_core::netlink`). `None` keeps the
    /// resolution-based default. Only used for auto; an explicit bitrate is never overridden.
    pub auto_kbps: Option<i32>,
    pub mtu_in: i32,
    pub rtt_ms: i32,
    pub ps5: bool,
    pub hevc: bool,
    pub hdr: bool,
}

impl Default for LaunchParams {
    fn default() -> Self {
        Self {
            width: 1280,
            height: 720,
            fps: 60,
            bitrate: -1,
            auto_kbps: None,
            mtu_in: 1454,
            rtt_ms: 2,
            ps5: true,
            hevc: false,
            hdr: false,
        }
    }
}

impl LaunchParams {
    pub fn eff_width(&self) -> i32 {
        if matches!(self.width, 640 | 960 | 1280 | 1920) {
            self.width
        } else {
            960
        }
    }
    pub fn eff_height(&self) -> i32 {
        if matches!(self.height, 360 | 540 | 720 | 1080) {
            self.height
        } else {
            540
        }
    }
    pub fn eff_fps(&self) -> i32 {
        if matches!(self.fps, 30 | 60) {
            self.fps
        } else {
            30
        }
    }
    pub fn eff_bitrate(&self) -> i32 {
        match self.bitrate {
            -1 => {
                let default = match self.width {
                    960 => 6000,
                    1280 => 10000,
                    1920 => 15000,
                    _ => 2000,
                };
                // never below the resolution default, never above 1 Gbit/s
                self.auto_kbps.map_or(default, |k| k.clamp(default, MAX_AUTO_KBPS))
            }
            2000 | 6000 | 10000 | 15000 => self.bitrate,
            b if (2000..=100_000).contains(&b) => b,
            _ => 15000,
        }
    }
    pub fn eff_mtu(&self) -> i32 {
        if self.mtu_in < 576 || self.mtu_in > 1454 {
            1454
        } else {
            self.mtu_in
        }
    }
    /// Measured RTT if 1..=500 ms else 2.
    pub fn eff_rtt(&self) -> i32 {
        if (1..=500).contains(&self.rtt_ms) {
            self.rtt_ms
        } else {
            2
        }
    }

    /// The JSON text plus the trailing NUL, with `handshake_key_b64` substituted.
    pub fn build(&self, handshake_key_b64: &str) -> String {
        let (w, h, f) = (self.eff_width(), self.eff_height(), self.eff_fps());
        let head = format!(
            "{{\"sessionId\":\"sessionId4321\",\"streamResolutions\":[{{\"resolution\":{{\"width\":{w},\"height\":{h}}},\"maxFps\":{f},\"score\":10}}],\"network\":{{\"bwKbpsSent\":{},\"bwLoss\":0.001000,\"mtu\":{},\"rtt\":{},\"ports\":[53,2053]}},\"slotId\":1,\"appSpecification\":{{\"minFps\":{f},\"minBandwidth\":0,\"extTitleId\":\"ps3\",\"version\":1,\"timeLimit\":1,\"startTimeout\":100,\"afkTimeout\":100,\"afkTimeoutDisconnect\":100}},\"konan\":{{\"ps3AccessToken\":\"accessToken\",\"ps3RefreshToken\":\"refreshToken\"}},\"requestGameSpecification\":{{\"model\":\"bravia_tv\",\"platform\":\"android\",\"audioChannels\":\"5.1\",\"language\":\"sp\",\"acceptButton\":\"X\",\"connectedControllers\":[\"xinput\",\"ds3\",\"ds4\"],\"yuvCoefficient\":\"bt601\",\"videoEncoderProfile\":\"hw4.1\",\"audioEncoderProfile\":\"audio1\"}},\"userProfile\":{{\"onlineId\":\"psnId\",\"npId\":\"npId\",\"region\":\"US\",\"languagesUsed\":[\"en\",\"jp\"]}},",
            self.eff_bitrate(), self.eff_mtu(), self.eff_rtt());
        let codec = if self.ps5 {
            format!(
                "\"videoCodec\":\"{}\",\"dynamicRange\":\"{}\",",
                if self.hevc { "hevc" } else { "avc" },
                if self.hdr { "HDR" } else { "SDR" }
            )
        } else {
            String::new()
        };
        format!("{head}{codec}\"handshakeKey\":\"{handshake_key_b64}\"}}\u{0}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn value_rules() {
        let mut p = LaunchParams { width: 1234, height: 1, fps: 45, bitrate: 5, ..Default::default() };
        assert_eq!((p.eff_width(), p.eff_height(), p.eff_fps(), p.eff_bitrate()), (960, 540, 30, 15000));
        p.bitrate = 3000;
        assert_eq!(p.eff_bitrate(), 3000);
        p.width = 1920;
        p.bitrate = -1;
        assert_eq!(p.eff_bitrate(), 15000);
        p.width = 640;
        assert_eq!(p.eff_bitrate(), 2000);
        // auto with a measured link: a large request, but never below the default and never above the cap
        p.width = 1920;
        p.auto_kbps = Some(60_000);
        assert_eq!(p.eff_bitrate(), 60_000);
        p.auto_kbps = Some(5_000_000);
        assert_eq!(p.eff_bitrate(), MAX_AUTO_KBPS);
        p.auto_kbps = Some(3000);
        assert_eq!(p.eff_bitrate(), 15000, "not below the resolution default");
        // an explicit choice is never overridden by the link
        p.auto_kbps = Some(800_000);
        p.bitrate = 6000;
        p.auto_kbps = Some(60_000);
        assert_eq!(p.eff_bitrate(), 6000);
        p.bitrate = -1;
        p.auto_kbps = None;
        p.width = 640;
        p.mtu_in = 100;
        p.rtt_ms = 9999;
        assert_eq!((p.eff_mtu(), p.eff_rtt()), (1454, 2));
        p.mtu_in = 1200;
        p.rtt_ms = 17;
        assert_eq!((p.eff_mtu(), p.eff_rtt()), (1200, 17));
    }

    #[test]
    fn ps4_and_ps5_templates() {
        let mut p = LaunchParams { ps5: false, ..Default::default() };
        let s = p.build("QUJD");
        assert!(s.ends_with("\"handshakeKey\":\"QUJD\"}\u{0}") && !s.contains("videoCodec"));
        assert!(s.starts_with("{\"sessionId\":\"sessionId4321\",\"streamResolutions\":[{\"resolution\":{\"width\":1280,\"height\":720},\"maxFps\":60,\"score\":10}],\"network\":{\"bwKbpsSent\":10000,\"bwLoss\":0.001000,\"mtu\":1454,\"rtt\":2,"));
        p.ps5 = true;
        p.hevc = true;
        p.hdr = true;
        let s = p.build("QUJD");
        assert!(s.contains("\"videoCodec\":\"hevc\",\"dynamicRange\":\"HDR\",\"handshakeKey\":\"QUJD\"}"));
        p.hevc = false;
        p.hdr = false;
        assert!(p.build("x").contains("\"videoCodec\":\"avc\",\"dynamicRange\":\"SDR\""));
    }
}
