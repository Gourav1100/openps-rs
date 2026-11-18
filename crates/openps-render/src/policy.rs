//! Presentation rate and quality settings, independent of graphics APIs.

pub fn requested_rate(s: &RenderSettings, source_hz: f64) -> u16 {
    if s.gen_factor > 0.0 {
        (source_hz.max(1.0) * f64::from(s.gen_factor)).round().clamp(1.0, 1000.0) as u16
    } else {
        s.display_hz
    }
}

/// The rate to present at, and how. Returns `(hz, vsyncs_per_frame, capped)`.
/// * never above the display's refresh rate;
/// * a rate that divides the refresh rate evenly (144, 72, 48 on a 144 Hz panel) is shown by holding each frame for a whole number of
///   refreshes: `capped` is false;
/// * any other rate (120 or 75 on a 144 Hz panel) is exactly what was asked for, no faster: each frame is scheduled for its own moment
///   and appears on the first refresh at or after it (`capped` is true). Some frames last one refresh and some two, so the cadence is a
///   little uneven, but the rate is the selected one.
pub fn effective_rate(requested: u16, refresh: f64) -> (f64, u32, bool) {
    let req = f64::from(requested.max(1));
    if refresh.is_nan() || refresh < 1.0 {
        return (req, 1, false);
    }
    if req >= refresh * 0.99 {
        return (refresh, 1, false);
    }
    let ratio = refresh / req;
    let k = ratio.round().max(1.0);
    if (ratio - k).abs() < 0.01 * ratio {
        return (refresh / k, k as u32, false);
    }
    (req, 1, true)
}

/// `effective_rate`, except that with `uncapped` a rate above the display's refresh rate is kept as it is (`capped` is true: each frame is
/// scheduled for its own moment and handed over then, and the display shows the newest at each refresh).
pub fn effective_rate_with(requested: u16, refresh: f64, uncapped: bool) -> (f64, u32, bool) {
    if uncapped && refresh >= 1.0 && f64::from(requested) > refresh * 1.01 {
        return (f64::from(requested), 1, true);
    }
    effective_rate(requested, refresh)
}

/// Rate the renderer paces to after the adaptive ladder. On a fixed-refresh display a rate below the stream's (real pictures are being
/// dropped) is rounded down to one that holds every picture for the same number of refreshes, unless it is already within a tenth of the
/// next one up (`ladder::settled_rate`); on a variable-refresh display it is kept.
/// A rate above the stream's (generated frames, placed for the refresh they are shown on) is kept. When the display's refresh rate
/// cannot be read the ask is capped, so "the display's rate" never turns into 240.
pub fn settle_held_rate(held: u16, src_fps: f64, refresh: f64, vrr: bool) -> u16 {
    const UNKNOWN_REFRESH_CAP_HZ: u16 = 120;
    if !(refresh.is_finite() && refresh >= 1.0) {
        return held.min(UNKNOWN_REFRESH_CAP_HZ);
    }
    if !vrr && src_fps > 1.0 && f64::from(held) < src_fps * 0.97 {
        return crate::ladder::settled_rate(held, refresh);
    }
    held
}

/// How much larger than the stream the GPU draws the picture (never larger than the window can show).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Upscale {
    Off,
    /// 4/3 of the stream's height (1080p becomes 1440p).
    X1_33,
    X1_5,
    X2,
}
impl Upscale {
    pub fn factor(self) -> f64 {
        match self {
            Upscale::Off => 0.0,
            Upscale::X1_33 => 4.0 / 3.0,
            Upscale::X1_5 => 1.5,
            Upscale::X2 => 2.0,
        }
    }

    /// The nearest known multiple; anything that is not one of them does not upscale.
    pub fn from_factor(f: f64) -> Self {
        [Upscale::X1_33, Upscale::X1_5, Upscale::X2]
            .into_iter()
            .find(|u| (u.factor() - f).abs() < 1e-6)
            .unwrap_or(Upscale::Off)
    }

    /// The height to draw at for a stream `src_height` tall; `None` when not upscaling.
    pub fn target_height(self, src_height: usize) -> Option<usize> {
        (self != Upscale::Off).then(|| (src_height as f64 * self.factor()).round() as usize)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameMode {
    /// Real frames show at once; extra frames are predicted forward. No added latency.
    ZeroLatency,
    /// Extra frames are interpolated between real ones. Cleanest picture, one source frame of added latency.
    Smooth,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RenderSettings {
    pub upscale: Upscale,
    /// Frames per second to present. At or below the source rate, no frames are generated.
    pub display_hz: u16,
    /// Frame generation as a multiple of the stream's rate (1.5, 2.0, 2.5, 3.0; 1.0 = none). When above zero it decides the rate
    /// (the stream's rate times this) and `display_hz` is not used; 0.0 = use `display_hz`.
    pub gen_factor: f32,
    pub mode: FrameMode,
    /// Shift the picture by camera rotation the stick has commanded but the stream does not show yet.
    pub camera: bool,
    /// Make the interpolated frames with Apple's MetalFX instead of the built-in interpolator (Smooth mode, at any rate).
    pub metalfx: bool,
    /// With `metalfx`: when the GPU cannot keep the rate up, fall back to the built-in interpolator for a while before lowering the rate.
    pub fx_fallback: bool,
    /// Upscale with AMD's FSR 1 (EASU, then the same finishing pass) instead of MetalFX's spatial scaler.
    pub fsr: bool,
    /// Run the MetalFX scaler in linear light (the FSR path has no such mode).
    pub linear_light: bool,
    /// When the GPU cannot keep the rate that is asked for, lower the rate in steps of 5 fps and keep every filter on (see
    /// [`crate::ladder`]); off: the finishing pass is given up instead, as before.
    pub adaptive: bool,
    /// Smooth mode's smoothing buffer (see `Timeline::set_buffer`).
    pub jitter_buffer: bool,
    /// Make and present frames above the display's refresh rate (see `effective_rate`).
    pub uncapped: bool,
    /// Edge contrast of the upscaler, 0.0 (smooth) ..= 1.0 (as MetalFX makes it).
    pub sharpness: f32,
    /// Adaptive sharpening (RCAS) of the upscaled picture, 0.0 (off) ..= 2.0; 1.0 is the measured best (see `gpu::rcas_lobe`).
    pub rcas: f32,
    /// Colour saturation, 1.0 = as the stream is.
    pub saturation: f32,
    /// Fill the black bars around the picture with a soft blurred glow of it.
    pub immersion: bool,
}
impl Default for RenderSettings {
    fn default() -> Self {
        Self {
            upscale: Upscale::Off,
            display_hz: 60,
            gen_factor: 0.0,
            mode: FrameMode::ZeroLatency,
            camera: false,
            metalfx: false,
            fx_fallback: false,
            fsr: false,
            linear_light: true,
            adaptive: false,
            jitter_buffer: false,
            uncapped: false,
            sharpness: 1.0,
            rcas: 1.0,
            saturation: 1.0,
            immersion: false,
        }
    }
}

