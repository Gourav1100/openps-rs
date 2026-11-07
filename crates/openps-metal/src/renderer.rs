//! The renderer thread: owns the GPU and a `CAMetalLayer`, takes decoded frames from a mailbox and presents at the display's
//! pace. Everything that decides latency is here:
//!
//! * **The decoder never waits for the GPU.** `submit` queues new pictures in a bounded mailbox and keeps at most one repeated picture as a
//!   fallback for the display thread.
//! * **Real frames are shown immediately.** In the default mode nothing is held back: the newest frame is advanced by how
//!   late it already is (extrapolation), never delayed.
//! * **At most two frames in flight** on the GPU, and the loop is paced by the display (`nextDrawable`).

use crate::camera::CameraComp;
use crate::gpu::{upscale_target, Cmd, Gpu, Tex};
use crate::ladder::RateLadder;
use crate::look::SharedLook;
use crate::timeline::Timeline;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::*;
use objc2_quartz_core::{CAMetalDrawable, CAMetalLayer};
use openps_video::vt::Frame;
use std::collections::VecDeque;
use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::channel;
use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Seconds on a monotonic clock shared by the renderer and the input thread (camera compensation compares their timestamps).
pub fn now() -> f64 {
    static T0: OnceLock<Instant> = OnceLock::new();
    T0.get_or_init(Instant::now).elapsed().as_secs_f64()
}

extern "C" {
    fn CACurrentMediaTime() -> f64;
}

/// The rate a set of settings asks for, in Hz (before the display's refresh rate has its say): the multiple chosen times the rate the
/// stream really delivers (a game that renders 45 frames a second at "2x" is shown at 90, not at twice the nominal 60).
/// The rate the console is always asked to stream at.
pub const STREAM_HZ: f64 = 60.0;

/// One 30 Hz interval: a pending repeated picture is shown if no distinct picture arrives by this deadline.
const DUPLICATE_DISPLAY_THRESHOLD: Duration = Duration::from_nanos(100_000_000 / 3);
/// The display thread checks pending repeated pictures once per 60 Hz stream interval.
const DISPLAY_TIMER_INTERVAL: Duration = Duration::from_nanos(50_000_000 / 3);

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
    /// Make the interpolated frame with Apple's MetalFX instead of the built-in interpolator (Smooth mode, about 2x only).
    pub metalfx: bool,
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

#[derive(Clone, Debug, Default)]
pub struct RenderStats {
    pub real_frames: u64,
    pub presented: u64,
    pub generated: u64,
    /// Frames replaced in the mailbox before they could be rendered (the renderer was behind).
    pub mailbox_drops: u64,
    pub errors: u64,
    /// Source frame rate as the timeline measures it.
    pub source_fps: f64,
    /// Real frames per second the renderer actually takes in (smoothed); below `source_fps` when it cannot keep up.
    pub processed_fps: f64,
    /// Real frames per second that were different pictures (what the game renders), and the share of arriving frames that were repeats.
    pub content_fps: f64,
    /// The rate being presented (`effective_rate` of the multiple times `content_fps`).
    pub target_hz: f64,
    pub repeat_share: f64,
    /// Present-to-present interval statistics over the last ~2 s, milliseconds.
    pub present_interval_ms: f64,
    pub present_jitter_ms: f64,
    pub present_max_ms: f64,
    /// Time a real frame waited between arriving and first being drawn, milliseconds.
    pub frame_wait_ms: f64,
    pub gpu_ms: f64,
    pub queue_delay_ms: f64,
    pub network_jitter_ms: f64,
    /// How much the smoothing buffer holds Smooth mode's picture back now (0 when it is off or not in Smooth mode).
    pub playout_ms: f64,
    /// Frames are made and presented above the display's refresh rate (the setting is on and the window is not full screen).
    pub uncapped_active: bool,
    pub upscaled: bool,
    /// The height the picture is drawn at while upscaled (0 otherwise).
    pub upscale_height: usize,
    /// The upscaled picture went through the finishing pass (linear light, the sharpness blend, RCAS).
    pub finish_pass: bool,
    /// The MetalFX scaler runs in linear light; this Mac could not make that scaler (the perceptual mode is used instead).
    pub linear_active: bool,
    pub linear_unavailable: bool,
    /// RCAS was applied to the last upscaled picture (the slider is above 0 and the finishing guard has not given the pass up).
    pub rcas_active: bool,
    /// The finishing-pass guard: given up (until the settings change), consecutive too-busy pictures so far, the number that trips it,
    /// and the GPU time per picture above which a picture counts as too busy (70 % of the interval), milliseconds.
    pub finish_guard_off: bool,
    pub finish_guard_slow: u32,
    pub finish_guard_limit: u32,
    pub finish_guard_ms: f64,
    /// The adaptive frame rate: switched on, the rate the settings ask for (before any limit), the limit now in force in fps (0 = none), the
    /// load over the last second at the rate being shown (the share of the time the GPU is busy, and the share of the time between two pictures
    /// that the slowest 5 % of them take), and the seconds it has been too high so far (the rate comes down at
    /// [`crate::ladder::DOWN_AFTER_S`]).
    pub adaptive: bool,
    pub asked_hz: f64,
    pub adaptive_cap_hz: f64,
    pub adaptive_load: f64,
    pub adaptive_peak: f64,
    pub adaptive_over_s: f64,
    /// Command buffers on the GPU and frames waiting in the mailbox right now.
    pub gpu_inflight: usize,
    pub mailbox_len: usize,
    /// The settings the renderer is running with now (after the app's mapping of the user's choices).
    pub settings: RenderSettings,
    pub generating: bool,
    pub camera_active: bool,
    /// The motion search is being given the camera motion the stick commands (it has been learned and is in use).
    pub input_prior_active: bool,
    pub camera_lag_ms: f64,
    pub camera_explained: f64,
    pub drawable_size: (usize, usize),
    /// The rate actually presented (see `effective_rate`) and the display's refresh rate.
    pub effective_hz: f64,
    pub refresh_hz: f64,
    /// Average time from handing a frame to the display until it is shown (Metal's presented time), milliseconds.
    pub present_latency_ms: f64,
    /// When frames actually reached the screen (Metal's presented times), over the last ~2 s: the interval between two frames, how far
    /// it strays, the longest one, and how many were late by more than half an interval (a visible stutter).
    pub shown_interval_ms: f64,
    pub shown_jitter_ms: f64,
    pub shown_max_ms: f64,
    pub shown_late: u32,
    /// Shown intervals in the last ~2 s by length: under 0.8, 0.8-1.25, 1.25-1.75, 1.75-2.5, 2.5-4 and over 4 times the nominal one.
    pub shown_hist: [u32; 6],
    /// The generated frames are being made by MetalFX (see `RenderSettings::metalfx`).
    pub metalfx_active: bool,
    /// This Mac can do MetalFX frame interpolation.
    pub metalfx_available: bool,
    /// The last picture was upscaled by FSR (EASU) instead of the MetalFX scaler.
    pub fsr_active: bool,
    /// The display changes its refresh rate frame by frame (frames are paced by the display); otherwise a rate that does not divide the
    /// refresh rate is paced by our own clock and may tear.
    pub variable_refresh: bool,
    /// The layer's current drawable count (2 windowed, 3 in full screen).
    pub drawables: usize,
}

struct Pending {
    frame: Frame,
    arrived: f64,
}

struct DuplicateDisplay<T> {
    last_timestamp: Instant,
    duplicate_frame: Option<T>,
    duplicate_display: bool,
}

impl<T> DuplicateDisplay<T> {
    fn new(now: Instant) -> Self {
        Self { last_timestamp: now, duplicate_frame: None, duplicate_display: false }
    }

    fn reset(&mut self, now: Instant) {
        self.last_timestamp = now;
        self.duplicate_frame = None;
        self.duplicate_display = false;
    }

    fn on_frame(&mut self, frame: T, duplicate: bool, now: Instant) -> Option<T> {
        if duplicate {
            self.duplicate_frame = Some(frame);
            return None;
        }

        self.duplicate_frame = None;
        if self.duplicate_display {
            self.duplicate_display = false;
        } else {
            self.last_timestamp = now;
        }
        Some(frame)
    }

    fn timer_tick(&mut self, now: Instant) -> Option<T> {
        if now.saturating_duration_since(self.last_timestamp) < DUPLICATE_DISPLAY_THRESHOLD {
            return None;
        }
        let frame = self.duplicate_frame.take()?;
        self.last_timestamp = now;
        self.duplicate_display = true;
        Some(frame)
    }

    fn duplicate_is_due(&self, now: Instant) -> bool {
        self.duplicate_frame.is_some()
            && now.saturating_duration_since(self.last_timestamp) >= DUPLICATE_DISPLAY_THRESHOLD
    }
}

struct Shared {
    /// Decoded frames waiting for the renderer, oldest first, up to `MAILBOX_FRAMES`; overflow drops the oldest (counted in `mailbox_drops`).
    mailbox: Mutex<std::collections::VecDeque<Pending>>,
    wake: Condvar,
    settings: Mutex<RenderSettings>,
    stats: Mutex<RenderStats>,
    stop: AtomicBool,
    /// Clear timeline and history (new session).
    reset: AtomicBool,
    /// The drawable size was set explicitly (no view): do not override it from the layer's bounds.
    headless: Arc<AtomicBool>,
    diag: Mutex<Diag>,
    /// Finds repeated pictures as frames arrive (the decoder's thread), and how many frames arrived / were new pictures in this window.
    gate: Mutex<DupGate>,
    /// Serializes new-frame arrivals with the display thread's periodic duplicate fallback.
    display: Mutex<DuplicateDisplay<Pending>>,
    counts: Mutex<Counts>,
    /// Milliseconds added to every picture's measured GPU time (a test and demo aid: it makes a light stream look like a heavy one, to see
    /// the adaptive frame rate work). Starts from `OPENPS_EXTRA_GPU_MS`; see [`MetalRenderer::set_extra_gpu_ms`].
    extra_gpu_ms: std::sync::atomic::AtomicU64,
}

pub struct MetalRenderer {
    shared: Arc<Shared>,
    join: Option<JoinHandle<()>>,
    layer: usize, // retained CAMetalLayer pointer, handed to the view
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn enqueue_pending(shared: &Shared, mailbox: &mut VecDeque<Pending>, pending: Pending) {
    mailbox.push_back(pending);
    if mailbox.len() > MAILBOX_FRAMES {
        mailbox.pop_front();
        lock(&shared.stats).mailbox_drops += 1;
    }
}

fn next_display_frame<T>(mailbox: &mut VecDeque<T>, display: &mut DuplicateDisplay<T>, now: Instant) -> Option<T> {
    mailbox.pop_front().or_else(|| display.timer_tick(now))
}

impl MetalRenderer {
    pub fn new(look: SharedLook, settings: RenderSettings) -> Result<Self, String> {
        let shared = Arc::new(Shared {
            mailbox: Mutex::new(std::collections::VecDeque::new()),
            wake: Condvar::new(),
            settings: Mutex::new(settings),
            stats: Mutex::new(RenderStats::default()),
            stop: AtomicBool::new(false),
            reset: AtomicBool::new(false),
            headless: Arc::new(AtomicBool::new(false)),
            diag: Mutex::new(Diag::default()),
            gate: Mutex::new(DupGate { prev: vec![], rule: RepeatRule::default() }),
            display: Mutex::new(DuplicateDisplay::new(Instant::now())),
            counts: Mutex::new(Counts::default()),
            extra_gpu_ms: std::sync::atomic::AtomicU64::new(
                std::env::var("OPENPS_EXTRA_GPU_MS")
                    .ok()
                    .and_then(|v| v.trim().parse::<f64>().ok())
                    .filter(|v| v.is_finite() && (0.0..=1000.0).contains(v))
                    .unwrap_or(0.0)
                    .to_bits(),
            ),
        });
        let (tx, rx) = channel::<Result<usize, String>>();
        let sh = shared.clone();
        let join = std::thread::Builder::new()
            .name("openps-render".into())
            .spawn(move || run(sh, look, tx))
            .map_err(|e| e.to_string())?;
        match rx.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(layer)) => Ok(Self { shared, join: Some(join), layer }),
            Ok(Err(e)) => Err(e),
            Err(_) => {
                shared.stop.store(true, Ordering::Relaxed);
                shared.wake.notify_all();
                Err("the Metal renderer did not start".into())
            }
        }
    }

    /// The `CAMetalLayer` to install as a view's layer.
    pub fn layer_ptr(&self) -> *mut c_void {
        self.layer as *mut c_void
    }

    /// Add `ms` to every picture's measured GPU time, as if the filters cost that much more (0 = off). For tests and demonstrations of the
    /// adaptive frame rate, which reacts to that time; nothing is drawn differently. Anything not between 0 and 1000 is ignored.
    pub fn set_extra_gpu_ms(&self, ms: f64) {
        if ms.is_finite() && (0.0..=1000.0).contains(&ms) {
            self.shared.extra_gpu_ms.store(ms.to_bits(), Ordering::Relaxed);
        }
    }

    /// Fix the drawable size (pixels). In the app the view's bounds decide this; headless use (tests) has no view.
    pub fn set_drawable_size(&self, w: usize, h: usize) {
        self.shared.headless.store(true, Ordering::Relaxed);
        let layer = self.layer as *mut CAMetalLayer;
        // SAFETY: the layer is alive as long as `self`; CAMetalLayer's size setter is safe to call from any thread.
        unsafe { (*layer).setDrawableSize(objc2_core_foundation::CGSize { width: w as f64, height: h as f64 }) };
    }

    /// Hand over a decoded frame. Never blocks; replaces an older frame that has not been drawn yet.
    pub fn submit(&self, frame: Frame) {
        let arrived = now();
        let t = Instant::now();
        let mut gate = lock(&self.shared.gate);
        let (repeat, changed) = gate.check(&frame);
        {
            let mut c = lock(&self.shared.counts);
            c.first.get_or_insert(t);
            c.last = Some(t);
            c.seen += 1;
            c.new += u32::from(!repeat);
            if !repeat {
                c.new_times.push_back(t);
                while c.new_times.len() > 64 || c.new_times.front().is_some_and(|x| t.duration_since(*x) > QUICK_WINDOW)
                {
                    c.new_times.pop_front();
                }
            }
        }
        {
            let mut d = lock(&self.shared.diag);
            if let (Some(until), Some(t0)) = (d.until, d.started) {
                d.rows.push((frame.source_id(), t0.elapsed().as_secs_f64(), changed, !repeat, !repeat));
                if Instant::now() >= until || d.rows.len() >= 20_000 {
                    (d.until, d.done) = (None, true);
                }
            }
        }

        let mut mailbox = lock(&self.shared.mailbox);
        let mut display = lock(&self.shared.display);
        let pending = Pending { frame, arrived };
        if let Some(pending) = display.on_frame(pending, repeat, t) {
            enqueue_pending(&self.shared, &mut mailbox, pending);
        }
        drop(display);
        drop(mailbox);
        drop(gate);
        self.shared.wake.notify_one();
    }

    pub fn set_settings(&self, s: RenderSettings) {
        *lock(&self.shared.settings) = s;
        self.shared.wake.notify_one();
    }

    pub fn settings(&self) -> RenderSettings {
        *lock(&self.shared.settings)
    }

    /// New session: forget the old stream's timing, motion and camera model.
    pub fn reset(&self) {
        self.shared.reset.store(true, Ordering::Relaxed);
        let mut gate = lock(&self.shared.gate);
        *gate = DupGate { prev: vec![], rule: RepeatRule::default() };
        lock(&self.shared.mailbox).clear();
        lock(&self.shared.display).reset(Instant::now());
        *lock(&self.shared.counts) = Counts::default();
        drop(gate);
        self.shared.wake.notify_one();
    }

    pub fn stats(&self) -> RenderStats {
        lock(&self.shared.stats).clone()
    }

    /// Start recording every arriving frame (id, arrival, whether its picture changed) for `secs` seconds.
    pub fn start_diag(&self, secs: f64) {
        let mut d = lock(&self.shared.diag);
        *d = Diag {
            until: Some(Instant::now() + Duration::from_secs_f64(secs)),
            started: Some(Instant::now()),
            ..Diag::default()
        };
    }

    /// The recording once it has finished (`None` while it is running or if none was started); taking it clears it.
    pub fn take_diag(&self) -> Option<Vec<DiagRow>> {
        let mut d = lock(&self.shared.diag);
        if d.done {
            d.done = false;
            return Some(std::mem::take(&mut d.rows));
        }
        None
    }
}

impl Drop for MetalRenderer {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        self.shared.wake.notify_all();
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
        // balance the +1 we handed out for the layer
        unsafe { drop(Retained::from_raw(self.layer as *mut CAMetalLayer)) };
    }
}

// ---- the thread ---------------------------------------------------------------------------------------------------------------

/// Textures the renderer owns: a decoded frame is copied into one right away, so the decoder's buffer pool can recycle its
/// memory immediately and the GPU never reads a buffer someone else is overwriting.
struct Ring {
    texs: Vec<Tex>,
    next: usize,
    size: (usize, usize),
}

/// Exposed for tests: copy a decoded BGRA frame into a texture the renderer owns and return it.
#[doc(hidden)]
pub fn frame_to_texture(gpu: &Gpu, frame: &Frame) -> Option<Tex> {
    let cmd = gpu.command_buffer()?;
    let mut ring = None;
    let t = copy_in(gpu, &cmd, &mut ring, frame)?;
    cmd.commit();
    cmd.waitUntilCompleted();
    Some(t)
}

/// Parse the `OPENPS_DRAWABLES` override: only 2 or 3 are valid for `CAMetalLayer` (anything else raises an ObjC exception).
pub fn drawable_override(v: Option<&str>) -> Option<usize> {
    v?.trim().parse().ok().filter(|n| (2..=3).contains(n))
}

/// Largest frame (pixels) the 6-texture ring will hold: 8192 x 8192 x 4 bytes x 6 = 1.5 GiB.
const MAX_RING_PIXELS: usize = 8192 * 8192;

fn make_ring(gpu: &Gpu, w: usize, h: usize) -> Option<Ring> {
    let ok = w >= 1
        && h >= 1
        && w <= crate::gpu::MAX_TEXTURE_DIM
        && h <= crate::gpu::MAX_TEXTURE_DIM
        && w.checked_mul(h).is_some_and(|p| p <= MAX_RING_PIXELS);
    if !ok {
        return None;
    }
    let texs: Option<Vec<Tex>> = (0..6)
        .map(|_| {
            crate::gpu::make_tex(&gpu.dev, MTLPixelFormat::BGRA8Unorm, w, h, MTLTextureUsage::ShaderRead, false).ok()
        })
        .collect();
    Some(Ring { texs: texs?, next: 0, size: (w, h) })
}

/// Copy a decoded BGRA frame into the next ring texture, in `cmd`. `None` if the frame is not Metal-ready.
fn copy_in(
    gpu: &Gpu,
    cmd: &Retained<ProtocolObject<dyn MTLCommandBuffer>>,
    ring: &mut Option<Ring>,
    f: &Frame,
) -> Option<Tex> {
    if f.pixel_format() != openps_video::vt::PIXEL_FORMAT_BGRA {
        return None;
    }
    let surf = NonNull::new(f.io_surface())?;
    let (w, h) = (f.width(), f.height());
    if ring.as_ref().is_none_or(|r| r.size != (w, h)) {
        *ring = make_ring(gpu, w, h);
    }
    let r = ring.as_mut()?;
    let desc = unsafe {
        MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
            MTLPixelFormat::BGRA8Unorm,
            w,
            h,
            false,
        )
    };
    desc.setUsage(MTLTextureUsage::ShaderRead);
    // SAFETY: the pointer is the frame's live IOSurface; `IOSurfaceRef` is the same opaque CoreFoundation type.
    let src = gpu.dev.newTextureWithDescriptor_iosurface_plane(
        &desc,
        unsafe { surf.cast::<objc2_io_surface::IOSurfaceRef>().as_ref() },
        0,
    )?;
    let dst = r.texs[r.next].clone();
    r.next = (r.next + 1) % r.texs.len();
    let blit = cmd.blitCommandEncoder()?;
    unsafe {
        blit.copyFromTexture_sourceSlice_sourceLevel_sourceOrigin_sourceSize_toTexture_destinationSlice_destinationLevel_destinationOrigin(
            &src,
            0,
            0,
            MTLOrigin { x: 0, y: 0, z: 0 },
            MTLSize { width: w, height: h, depth: 1 },
            &dst,
            0,
            0,
            MTLOrigin { x: 0, y: 0, z: 0 },
        );
    }
    blit.endEncoding();
    Some(dst)
}

/// Intervals between frames as the display showed them (written from Metal's presented callback, read by the stats update).
#[derive(Default)]
struct Shown {
    last: Option<f64>,
    intervals: VecDeque<f64>,
}
impl Shown {
    fn push(&mut self, t: f64) {
        if let Some(l) = self.last {
            let dt = (t - l) * 1e3;
            if dt.is_finite() && dt > 0.0 && dt < 500.0 {
                self.intervals.push_back(dt);
                while self.intervals.len() > 240 {
                    self.intervals.pop_front();
                }
            }
        }
        self.last = Some(t);
    }
    /// `(mean, standard deviation, max, late)` where late counts intervals longer than `1.5 x nominal`.
    fn histogram(&self, nominal_ms: f64) -> [u32; 6] {
        let mut h = [0u32; 6];
        for x in &self.intervals {
            let k = x / nominal_ms;
            h[if k < 0.8 {
                0
            } else if k < 1.25 {
                1
            } else if k < 1.75 {
                2
            } else if k < 2.5 {
                3
            } else if k < 4.0 {
                4
            } else {
                5
            }] += 1;
        }
        h
    }

    fn summary(&self, nominal_ms: f64) -> (f64, f64, f64, u32) {
        let n = self.intervals.len() as f64;
        if n < 2.0 {
            return (0.0, 0.0, 0.0, 0);
        }
        let mean = self.intervals.iter().sum::<f64>() / n;
        let var = self.intervals.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / n;
        let late = self.intervals.iter().filter(|x| **x > nominal_ms * 1.5).count() as u32;
        (mean, var.sqrt(), self.intervals.iter().cloned().fold(0.0, f64::max), late)
    }
}

/// Finds repeated pictures. A console that renders fewer frames than the stream carries (a game at 30 on a 60 fps stream) repeats pictures;
/// a repeat adds nothing to interpolate and must not count as a frame the game rendered. A sparse grid of pixels is compared with the last
/// kept picture's (a repeated picture decodes to the same pixels, give or take the odd level; a moving object covers many samples).
struct DupGate {
    prev: Vec<u8>,
    rule: RepeatRule,
}

const GATE_COLS: usize = 160;
const GATE_ROWS: usize = 90;
/// A sample byte that differs by more than this counts as changed, and this many of them make a new picture.
const GATE_LEVEL: u8 = 2;
const GATE_BYTES: usize = 3;

#[link(name = "IOSurface", kind = "framework")]
extern "C" {
    fn IOSurfaceLock(surface: *mut c_void, options: u32, seed: *mut u32) -> i32;
    fn IOSurfaceUnlock(surface: *mut c_void, options: u32, seed: *mut u32) -> i32;
    fn IOSurfaceGetBaseAddress(surface: *mut c_void) -> *mut c_void;
    fn IOSurfaceGetBytesPerRow(surface: *mut c_void) -> usize;
}

/// Decides, picture by picture, whether it is a repeat of the last kept one. A bit-identical picture is. But the console encodes a repeated
/// picture again, and the copy differs from the original by encoder noise that grows with how much is moving (measured on a game at 30 on a
/// 60 fps stream: 10 to 80 bytes in a quiet scene, 200 to 780 while the camera sweeps, against hundreds to tens of thousands for a real new
/// picture). So a picture is a repeat when it changed far less than the last few new pictures did.
///
/// That comparison goes stale when the scene changes: gameplay to a menu or intro, where every real picture changes by a small amount. Left
/// alone, every one of them would look like a repeat of the gameplay's much larger changes and the screen would freeze. So no more than
/// `MAX_REPEATS_IN_A_ROW` pictures in a row are repeats on this ground: the next is a new picture, and the history starts again from it.
#[derive(Default)]
struct RepeatRule {
    recent: Vec<usize>,
    run: usize,
}

const MAX_REPEATS_IN_A_ROW: usize = 3;

impl RepeatRule {
    /// `changed` is the number of sample bytes that differ from the last kept picture (`usize::MAX` = unknown). True for a repeat.
    fn is_repeat(&mut self, changed: usize) -> bool {
        if changed == usize::MAX {
            self.run = 0;
            return false;
        }
        if changed < GATE_BYTES {
            return true; // identical: nothing was drawn, however long that goes on
        }
        let typical = {
            let mut v = self.recent.clone();
            v.sort_unstable();
            v.get(v.len() / 2).copied().unwrap_or(0)
        };
        let looks_like_a_copy =
            self.recent.len() >= 2 && typical >= REPEAT_MIN_TYPICAL && (changed as f64) < REPEAT_RATIO * typical as f64;
        if looks_like_a_copy && self.run < MAX_REPEATS_IN_A_ROW {
            self.run += 1;
            return true;
        }
        // a new picture (also when the scene has gone quiet): it becomes the reference
        if looks_like_a_copy {
            self.recent.clear();
        }
        self.run = 0;
        self.recent.push(changed);
        if self.recent.len() > 5 {
            self.recent.remove(0);
        }
        false
    }
}

/// A picture is a repeat when it changed less than this share of what new pictures change by (the median of the last five)...
const REPEAT_RATIO: f64 = 0.3;
/// ...but only when new pictures change by at least this much (a still scene has nothing to compare against).
const REPEAT_MIN_TYPICAL: usize = 60;

impl DupGate {
    fn sample(f: &Frame) -> Option<Vec<u8>> {
        if f.pixel_format() != openps_video::vt::PIXEL_FORMAT_BGRA {
            return None;
        }
        let s = f.io_surface();
        if s.is_null() {
            return None;
        }
        let (w, h) = (f.width(), f.height());
        if w < GATE_COLS || h < GATE_ROWS {
            return None;
        }
        let mut out = Vec::with_capacity(GATE_COLS * GATE_ROWS * 3);
        // SAFETY: the surface is the frame's live IOSurface; it is locked read-only while its memory is read, within width x height.
        unsafe {
            if IOSurfaceLock(s, 1, std::ptr::null_mut()) != 0 {
                return None;
            }
            let (base, stride) = (IOSurfaceGetBaseAddress(s).cast::<u8>(), IOSurfaceGetBytesPerRow(s));
            if !base.is_null() && stride >= w * 4 {
                for j in 0..GATE_ROWS {
                    let y = (j * 2 + 1) * h / (GATE_ROWS * 2);
                    for i in 0..GATE_COLS {
                        let x = (i * 2 + 1) * w / (GATE_COLS * 2);
                        let p = base.add(y * stride + x * 4);
                        out.extend_from_slice(&[*p, *p.add(1), *p.add(2)]);
                    }
                }
            }
            IOSurfaceUnlock(s, 1, std::ptr::null_mut());
        }
        (out.len() == GATE_COLS * GATE_ROWS * 3).then_some(out)
    }

    /// `(is_repeat, changed sample bytes)` for the next picture; the picture becomes the one compared against next unless it is a repeat.
    fn check(&mut self, f: &Frame) -> (bool, usize) {
        let Some(cur) = Self::sample(f) else { return (false, usize::MAX) };
        let n = if self.prev.len() == cur.len() {
            self.prev.iter().zip(&cur).filter(|(a, b)| a.abs_diff(**b) > GATE_LEVEL).count()
        } else {
            usize::MAX
        };
        let repeat = self.rule.is_repeat(n);
        if !repeat {
            self.prev = cur;
        }
        (repeat, n)
    }
}

/// Frames that arrived since the rate was last measured: how many, how many were new pictures, and when the first and the last came.
#[derive(Default)]
struct Counts {
    seen: u32,
    new: u32,
    first: Option<Instant>,
    last: Option<Instant>,
    /// When the last accepted frames arrived (about the last half second), for a quick read of the rate.
    new_times: std::collections::VecDeque<Instant>,
}

/// The rate the new pictures in `times` came at, if there are enough of them to say; `None` otherwise. Only looks at the last `window`.
fn quick_rate(times: &std::collections::VecDeque<Instant>, now: Instant, window: Duration) -> Option<f64> {
    let recent: Vec<&Instant> = times.iter().filter(|t| now.duration_since(**t) <= window).collect();
    if recent.len() < QUICK_MIN {
        return None;
    }
    let span = recent.last()?.duration_since(**recent.first()?).as_secs_f64();
    let rate = (recent.len() - 1) as f64 / span;
    (span > 0.025 && rate >= QUICK_MIN_RATE).then_some(rate)
}

const QUICK_MIN: usize = 3;
const QUICK_MIN_RATE: f64 = 15.0;
const QUICK_WINDOW: Duration = Duration::from_millis(500);

/// One frame in the diagnostic: `(frame id, seconds since start, changed sample bytes (usize::MAX = unknown), new picture, accepted)`.
pub type DiagRow = (u16, f64, usize, bool, bool);

/// What the frame diagnostic records: one row per frame that arrived.
#[derive(Default)]
struct Diag {
    until: Option<Instant>,
    /// `(frame id, seconds since start, changed sample bytes (usize::MAX = unknown), new picture, accepted)`.
    rows: Vec<DiagRow>,
    started: Option<Instant>,
    done: bool,
}

/// Decoded frames the renderer may have waiting.
const MAILBOX_FRAMES: usize = 3;

struct Pacing {
    last: Option<Instant>,
    intervals: VecDeque<f64>,
}
impl Pacing {
    fn tick(&mut self) {
        let t = Instant::now();
        if let Some(l) = self.last {
            self.intervals.push_back(t.duration_since(l).as_secs_f64() * 1e3);
            while self.intervals.len() > 240 {
                self.intervals.pop_front();
            }
        }
        self.last = Some(t);
    }
    fn summary(&self) -> (f64, f64, f64) {
        let n = self.intervals.len() as f64;
        if n < 2.0 {
            return (0.0, 0.0, 0.0);
        }
        let mean = self.intervals.iter().sum::<f64>() / n;
        let var = self.intervals.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / n;
        (mean, var.sqrt(), self.intervals.iter().cloned().fold(0.0, f64::max))
    }
}

extern "C" {
    fn pthread_set_qos_class_self_np(qos: u32, relative_priority: i32) -> i32;
}

/// The render thread paces frames to a few milliseconds; at the default priority a busy decoder or UI thread can delay it past a refresh.
/// QOS_CLASS_USER_INTERACTIVE (0x21) is what the system gives work the user is directly watching.
fn prioritise_this_thread() {
    // SAFETY: a plain libpthread call on the current thread.
    unsafe {
        pthread_set_qos_class_self_np(0x21, 0);
    }
}

fn run(shared: Arc<Shared>, look: SharedLook, init: std::sync::mpsc::Sender<Result<usize, String>>) {
    prioritise_this_thread();
    let headless = shared.headless.clone();
    let mut gpu = match Gpu::new() {
        Ok(g) => g,
        Err(e) => {
            let _ = init.send(Err(format!("Metal: {e}")));
            return;
        }
    };
    let layer: Retained<CAMetalLayer> = CAMetalLayer::new();
    layer.setDevice(Some(&gpu.dev));
    layer.setPixelFormat(MTLPixelFormat::BGRA8Unorm);
    layer.setFramebufferOnly(true);
    // Two drawables for lowest delay; the loop raises it to three in full screen (see `crate::set_fullscreen`).
    layer.setMaximumDrawableCount(2);
    layer.setDisplaySyncEnabled(true);
    layer.setOpaque(true);
    // This thread has no run loop, so Core Animation would never commit the changes above (the layer would stay in its default,
    // unconfigured state and show nothing). Flush the implicit transaction ourselves.
    objc2_quartz_core::CATransaction::flush();
    let _ = init.send(Ok(Retained::into_raw(layer.clone()) as usize));

    let mut tl = Timeline::default();
    let mut cam = CameraComp::new(look);
    let mut ring: Option<Ring> = None;
    let mut cur_id: Option<u16> = None;
    let mut prev_id: Option<u16> = None;
    let mut cur_arrived = 0.0;
    let mut first_shown = true;
    let mut awaiting: Option<(Cmd, f64, f64, Option<usize>)> = None;
    let mut last_gpu_err_log = 0.0f64;
    // OPENPS_DRAWABLES is a dev knob: read once, only 2 or 3 are valid for CAMetalLayer
    let drawables_override = drawable_override(std::env::var("OPENPS_DRAWABLES").ok().as_deref());
    let mut inflight: VecDeque<Cmd> = VecDeque::new();
    // The rate the stream really delivers, held steady: it only changes when the measurement has stayed more than 6 % away for a second,
    // so the target rate does not wander with every frame the game drops.
    let (mut src_locked, mut src_off_since) = (0.0f64, None::<Instant>);
    // The rate the game renders (distinct pictures per second, counted as they arrive), smoothed.
    let (mut content_ema, mut repeat_share) = (0.0f64, 0.0f64);
    // Display sync is switched off only for a rate the display cannot hold evenly, and not flapped back and forth.
    let (mut free_running, mut free_since) = (false, None::<Instant>);
    let mut pacing = Pacing { last: None, intervals: VecDeque::new() };
    // Average hand-over-to-display time, written by Metal's presented callback (it holds nothing, so it cannot starve the drawable pool).
    let latency_bits = Arc::new(std::sync::atomic::AtomicU64::new(0f64.to_bits()));
    let shown = Arc::new(Mutex::new(Shown::default()));
    let (mut vrr, mut last_vrr_check) = (crate::display_is_variable(), Instant::now());
    let mut next_present = 0.0f64;
    let (mut gpu_ms, mut wait_ms) = (0.0f64, 0.0f64);
    // The upscaler's finishing pass (linear light, RCAS) costs about 2 ms at 4K: when the GPU keeps using most of the time between two displayed
    // pictures, it is given up (the plain upscaler is used) until the upscaling, sharpness or rate settings change.
    let (mut finish_slow, mut finish_off, mut finish_cfg) =
        (0u32, false, (Upscale::Off, 0u16, 0u32, 0u32, 0u32, true, false, false));
    // The adaptive frame rate: the limit the GPU's load calls for, and (when fewer pictures are shown than arrive) the moment the next
    // real picture is due, so the ones in between are dropped evenly.
    let mut ladder = RateLadder::default();
    let mut next_due: Option<Instant> = None;
    let mut s = RenderStats::default();

    while !shared.stop.load(Ordering::Relaxed) {
        // Every iteration gets its own autorelease pool. This thread has no run loop to drain one, and without it the
        // drawables, command buffers and textures it receives are never released (an unpresented drawable would even
        // exhaust the layer's two-drawable limit).
        objc2::rc::autoreleasepool(|_| {
            if shared.reset.swap(false, Ordering::Relaxed) {
                tl.reset();
                cam.reset();
                cur_id = None;
                prev_id = None;
                awaiting = None;
                gpu.reset_stream();
                src_locked = 0.0;
            }
            let settings = *lock(&shared.settings);
            tl.set_buffer(settings.jitter_buffer && settings.mode == FrameMode::Smooth);
            // The rate the game renders: distinct pictures per second, in whatever order and rhythm they arrive (the stream's own rate and
            // the setting are only the starting guess until a second of frames has been seen). A mostly still picture (a menu) has too few
            // changes to say anything: the last good value is kept.
            if src_locked <= 0.0 {
                (src_locked, src_off_since, content_ema) = (STREAM_HZ, None, 0.0);
            }
            // The window is measured over the time the frames actually took to arrive (not over wall time: a stream that has only just
            // started, or paused, must not read as a slow one).
            let window_ready = lock(&shared.counts).first.is_some_and(|t| t.elapsed() >= Duration::from_secs(1));
            if window_ready {
                let c = std::mem::take(&mut *lock(&shared.counts));
                let (seen_count, content_count) = (c.seen, c.new);
                if seen_count >= 8 && tl.stats().frames >= 30 {
                    let share = f64::from(content_count) / f64::from(seen_count);
                    repeat_share = 1.0 - share;
                    if share >= 0.2 {
                        // The share of arriving frames that were new pictures, times the stream's own rate. The share does not depend on when
                        // the frames came (a burst when the stream starts, or after a stall, cannot distort it) and the stream's rate is the
                        // timeline's estimate from the frame ids, which is built to ignore bursts and jitter.
                        let rate = (share / tl.interval()).clamp(5.0, STREAM_HZ);
                        let first = content_ema <= 0.0;
                        content_ema = if first { rate } else { 0.5 * content_ema + 0.5 * rate };
                        if first {
                            src_locked = content_ema.round(); // the first measurement replaces the guess at once
                        } else if (content_ema / src_locked - 1.0).abs() > 0.06 {
                            match src_off_since {
                                None => src_off_since = Some(Instant::now()),
                                Some(t) if t.elapsed() > Duration::from_millis(1500) => {
                                    src_locked = content_ema.round();
                                    src_off_since = None;
                                }
                                _ => {}
                            }
                        } else {
                            src_off_since = None;
                        }
                    }
                }
            }
            // The rate goes up at once and comes down slowly: a game that has been loading (a few pictures a second) and starts to render
            // again would otherwise be held at the low rate for seconds, the loop would run slower than the pictures arrive and real
            // pictures would be dropped. Coming down late costs nothing: a few extra generated frames.
            if let Some(q) = quick_rate(&lock(&shared.counts).new_times, Instant::now(), QUICK_WINDOW) {
                // the console is asked for 60 and sends no more: a reading above that is arrival jitter
                let q = q.min(STREAM_HZ);
                if q > src_locked * 1.12 {
                    src_locked = q.round();
                    content_ema = content_ema.max(q);
                    src_off_since = None;
                }
            }
            let src_fps = src_locked;
            let refresh = crate::main_display_hz();
            // locked to what the menu says (the multiple of the stream's rate, or the display rate), never above what the display can show;
            // with the adaptive frame rate on, also never above what the GPU can hold with every filter running (steps of 5 fps)
            let asked = requested_rate(&settings, src_fps);
            // what is really asked for: never above what the display can show, unless the user lets the renderer run ahead of it (windowed:
            // the compositor shows the newest frame at each refresh; in full screen the frames would only queue up, so the display's rate is kept)
            let uncapped = settings.uncapped && !crate::fullscreen();
            let asked_hz = if refresh >= 1.0 && !uncapped { f64::from(asked).min(refresh) } else { f64::from(asked) };
            let held = if settings.adaptive { ladder.apply(asked) } else { asked };
            let held = settle_held_rate(held, src_fps, refresh, vrr);
            let (eff_hz, vsyncs, capped) = effective_rate_with(held, refresh, uncapped);
            // frames are only generated up to the rate that was selected (and the display can show): at or below the source rate each
            // real frame is simply shown as it arrives
            let wants_more = eff_hz > src_fps * 1.1;
            let generating = wants_more && (eff_hz > src_fps * 1.1 || settings.mode == FrameMode::Smooth);
            let ticking = generating || settings.camera;
            gpu.sharpness = settings.sharpness;
            let cfg = (
                settings.upscale,
                settings.display_hz,
                settings.gen_factor.to_bits(),
                settings.sharpness.to_bits(),
                settings.rcas.to_bits(),
                settings.linear_light,
                settings.fsr,
                settings.adaptive,
            );
            if cfg != finish_cfg {
                (finish_cfg, finish_off, finish_slow) = (cfg, false, 0);
                ladder.reset(); // what the GPU can hold is found out again
            }
            gpu.linear_upscale = settings.linear_light && !finish_off;
            gpu.rcas = if finish_off { 0.0 } else { settings.rcas };
            gpu.saturation = settings.saturation;
            gpu.immersion = settings.immersion;
            // MetalFX makes exactly the midpoint between two frames, so it is the right tool when every generated frame is a midpoint (about 2x).
            // At other multiples some generated frames would come from it and the others from the built-in interpolator, two different looks
            // alternating, which flickers: there the built-in interpolator makes all of them.
            let midpoints_only =
                settings.mode == FrameMode::Smooth && generating && (1.8..=2.3).contains(&(eff_hz / src_fps));
            gpu.fx_use = settings.metalfx && gpu.fx_available && midpoints_only;
            gpu.fsr = settings.fsr;

            // Event-driven when there is nothing to generate: wait for a frame instead of redrawing the same one.
            // Fewer pictures are shown than arrive (the frame rate was lowered below the stream's): the real pictures in between are dropped
            // evenly, each shown one when it is due, so 45 of 60 are kept as three of every four and not as every other one.
            let thin = eff_hz < src_fps * 0.97;
            let take_ok = !thin || next_due.is_none_or(|d| Instant::now() >= d);
            if !thin {
                next_due = None;
            }
            if !ticking {
                let mut m = lock(&shared.mailbox);
                let timeout = if lock(&shared.display).duplicate_frame.is_some() {
                    DISPLAY_TIMER_INTERVAL
                } else {
                    Duration::from_millis(100)
                };
                if m.is_empty() {
                    m = shared.wake.wait_timeout(m, timeout).unwrap_or_else(PoisonError::into_inner).0;
                }
                let waiting = !m.is_empty() && !take_ok;
                drop(m);
                if waiting {
                    let left = next_due.map_or(Duration::ZERO, |d| d.saturating_duration_since(Instant::now()));
                    std::thread::sleep(left.min(Duration::from_millis(2)));
                    return;
                }
                let mailbox = lock(&shared.mailbox);
                let display = lock(&shared.display);
                let has_frame = !mailbox.is_empty();
                let duplicate_due = display.duplicate_is_due(Instant::now());
                if !has_frame && !duplicate_due {
                    return;
                }
            } else if lock(&shared.mailbox).is_empty() && cur_id.is_none() {
                let m = lock(&shared.mailbox);
                let _ = shared.wake.wait_timeout(m, Duration::from_millis(50));
                return;
            }

            // never queue more than two frames on the GPU
            while inflight.front().is_some_and(|c| {
                c.status() == MTLCommandBufferStatus::Completed || c.status() == MTLCommandBufferStatus::Error
            }) {
                if let Some(c) = inflight.pop_front() {
                    if c.status() == MTLCommandBufferStatus::Error {
                        bump_errors(&shared);
                        let mt = unsafe { CACurrentMediaTime() };
                        if mt - last_gpu_err_log >= 1.0 {
                            last_gpu_err_log = mt;
                            match c.error() {
                                Some(e) => eprintln!(
                                    "GPU command buffer failed: code {} {}",
                                    e.code(),
                                    e.localizedDescription()
                                ),
                                None => eprintln!("GPU command buffer failed (no error object)"),
                            }
                        }
                    }
                    let dt = (c.GPUEndTime() - c.GPUStartTime()) * 1e3
                        + f64::from_bits(shared.extra_gpu_ms.load(Ordering::Relaxed));
                    if dt.is_finite() && dt >= 0.0 {
                        gpu_ms += (dt - gpu_ms) * 0.1;
                        if settings.adaptive {
                            // the adaptive frame rate: when the pictures keep the GPU busy for most of the time between two displayed ones,
                            // the rate comes down gently (and back up when there is room), and every filter stays on
                            if ladder.observe(dt, eff_hz, asked_hz) {
                                match ladder.cap() {
                                    Some(c) => eprintln!("the GPU needs {dt:.1} ms per picture: the frame rate is held to {c} fps (asked for {asked_hz:.0})"),
                                    None => eprintln!("the GPU has room again: back to the {asked_hz:.0} fps that was asked for"),
                                }
                            }
                        }
                        // the finishing-pass guard: pictures in a row that kept the GPU busy for most of the time between two displayed ones
                        let interval = 1e3 / eff_hz.max(1.0);
                        if settings.upscale != Upscale::Off && !finish_off && !settings.adaptive {
                            if crate::gpu::finish_pass_too_slow(dt, interval) {
                                finish_slow += 1;
                                if finish_slow >= crate::gpu::FINISH_GUARD_PICTURES {
                                    finish_off = true;
                                    eprintln!("the GPU needs {dt:.1} ms per picture, most of the {interval:.1} ms there is: the upscaler's linear-light and sharpening pass is switched off");
                                }
                            } else {
                                finish_slow = 0;
                            }
                        }
                    }
                }
            }
            if inflight.len() >= 3 {
                std::thread::sleep(Duration::from_micros(300));
                return;
            }

            // camera model: harvest the previous frame's measured motion once the GPU has finished with it
            if let Some((c, t0, t1, slot)) = awaiting.take() {
                match c.status() {
                    MTLCommandBufferStatus::Completed => {
                        let gm = slot.and_then(|k| gpu.global_motion_at(k));
                        if let Some(g) = gm.filter(|g| g.support > 0.5) {
                            cam.observe(t0, t1, (g.x, g.y));
                        }
                    }
                    MTLCommandBufferStatus::Error => {} // dropped (logged when it leaves `inflight`): do not wait for it forever
                    _ => awaiting = Some((c, t0, t1, slot)),
                }
            }

            // Keep the drawable matched to the layer's pixel size (bounds x scale). Core Animation does not do this for a layer
            // that was created off the main thread, and a drawable that is too small is stretched, which is blurry.
            if !headless.load(Ordering::Relaxed) {
                let b = layer.bounds();
                let sc = layer.contentsScale();
                let (w, h) = ((b.size.width * sc).round(), (b.size.height * sc).round());
                let cur = layer.drawableSize();
                if w >= 1.0 && h >= 1.0 && ((cur.width - w).abs() > 0.5 || (cur.height - h).abs() > 0.5) {
                    layer.setDrawableSize(objc2_core_foundation::CGSize { width: w, height: h });
                }
            }
            // Exact-rate pacing (see `effective_rate`): wake shortly before this frame's moment, and ask for it to appear then.
            let t_present = if capped && ticking {
                let interval = 1.0 / eff_hz;
                let mt = unsafe { CACurrentMediaTime() };
                if next_present < mt - interval || next_present > mt + 4.0 * interval {
                    next_present = mt + interval; // first frame, or fell behind: start a fresh schedule
                }
                let wake = next_present - 0.0035;
                if wake > mt {
                    std::thread::sleep(Duration::from_secs_f64(wake - mt));
                }
                Some(next_present)
            } else {
                next_present = 0.0;
                None
            };
            // With variable refresh the display follows the scheduled present times. Without it, a rate that does not divide the refresh
            // rate is paced by our own clock instead of the display's when windowed: frames may tear, but their spacing is even.
            if last_vrr_check.elapsed() > Duration::from_secs(1) {
                vrr = crate::display_is_variable();
                last_vrr_check = Instant::now();
            }
            // Full screen goes straight to the display, which holds a frame handed over "now" for several refreshes (measured: 19.9 ms against
            // 13.1 ms scheduled for a refresh), so there the display's own timing is used; the compositor in a window is what makes free
            // running quicker.
            let want_free =
                capped && ticking && !vrr && !crate::fullscreen() && std::env::var_os("OPENPS_VSYNC").is_none();
            if want_free == free_running {
                free_since = None;
            } else if free_since.get_or_insert_with(Instant::now).elapsed() > Duration::from_secs(1) {
                free_running = want_free; // only a change that has lasted a second counts: flipping the sync mode back and forth flickers
                free_since = None;
            }
            if layer.displaySyncEnabled() == free_running {
                layer.setDisplaySyncEnabled(!free_running);
            }
            let want_drawables = drawables_override.unwrap_or(if crate::fullscreen() { 3 } else { 2 });
            if layer.maximumDrawableCount() != want_drawables {
                layer.setMaximumDrawableCount(want_drawables);
            }
            let Some(drawable) = layer.nextDrawable() else { return };
            let drawable_tex = ProtocolObject::from_retained(drawable.texture());
            let Some(cmd) = gpu.command_buffer() else {
                bump_errors(&shared);
                return;
            };

            // the moment this picture will be on screen (the timeline asks for the content that belongs to it)
            let t_now = match t_present {
                Some(tp) => now() + (tp - unsafe { CACurrentMediaTime() }),
                None => now(),
            };
            let mut got_new = false;
            let next_frame = if take_ok {
                let mut mailbox = lock(&shared.mailbox);
                let mut display = lock(&shared.display);
                next_display_frame(&mut mailbox, &mut display, Instant::now())
            } else {
                None
            };
            if thin && next_frame.is_some() {
                let step = Duration::from_secs_f64(1.0 / eff_hz.max(1.0));
                let at = Instant::now();
                let due = next_due.unwrap_or(at) + step;
                // a due time that has fallen a whole step behind is not owed: start again from now
                next_due = Some(if due + step < at { at } else { due });
            }
            if let Some(p) = next_frame {
                if let Some(tex) = copy_in(&gpu, &cmd, &mut ring, &p.frame) {
                    let id = p.frame.source_id();
                    if tl.on_frame(id, p.arrived) {
                        let motion = ticking || settings.camera;
                        // what the stick says the camera did between the last picture and this one: a starting point for the motion search
                        let prior = cur_id
                            .and_then(|c| Some((tl.content_time(c)?, tl.content_time(id)?)))
                            .and_then(|(t0, t1)| cam.predict(t0, t1));
                        s.input_prior_active = prior.is_some();
                        if motion {
                            gpu.set_input_prior(prior);
                        }
                        if gpu.ingest(&cmd, tex, motion).is_ok() {
                            prev_id = cur_id;
                            cur_id = Some(id);
                            cur_arrived = p.arrived;
                            got_new = true;
                            s.real_frames += 1;
                            if let (Some(pid), true) = (prev_id, awaiting.is_none() && motion) {
                                if let (Some(t0), Some(t1)) = (tl.content_time(pid), tl.content_time(id)) {
                                    awaiting = Some((cmd.clone(), t0, t1, gpu.motion_snapshot()));
                                }
                            }
                        } else {
                            bump_errors(&shared);
                        }
                    }
                } else {
                    bump_errors(&shared); // not BGRA / no IOSurface
                }
            }
            let Some(id) = cur_id else { return };

            let (dw, dh) = (drawable_tex.width(), drawable_tex.height());
            s.drawable_size = (dw, dh);
            let src = gpu_cur_size(&gpu);
            let out = settings.upscale.target_height(src.1).and_then(|h| upscale_target(src, (dw, dh), h));
            let (alpha, extra, tex_res) = match settings.mode {
                FrameMode::Smooth => {
                    let beta = prev_id.map_or(1.0, |p| tl.interp_beta(t_now, p));
                    (beta, (0.0, 0.0), gpu.compose_interp(&cmd, beta, None))
                }
                FrameMode::ZeroLatency => {
                    // the motion field spans `gap` source intervals when frames were dropped on the way: scale it back to one
                    let gap = prev_id.map_or(1, |p| i32::from(id.wrapping_sub(p) as i16).clamp(1, 8)) as f32;
                    let alpha = if generating { tl.alpha(t_now) / gap } else { 0.0 };
                    let mut extra = (0.0f32, 0.0f32);
                    camera_extra(&cam, &tl, id, t_now, alpha, src, settings.camera, &mut extra, &mut s);
                    (alpha, extra, gpu.compose(&cmd, alpha, None, extra))
                }
            };
            let _ = (alpha, extra);
            // the upscaling is a step of its own (MetalFX or FSR), inside this command buffer
            let tex_res = tex_res.and_then(|(t, _)| gpu.upscale(&cmd, t, out));
            let Ok((tex, upscaled)) = tex_res else {
                bump_errors(&shared);
                return;
            };
            // without the upscaler the sharpness setting works as an unsharp mask on the final picture (50 % = untouched)
            gpu.sharpen = if upscaled { 0.0 } else { (settings.sharpness - 0.5) * 1.0 };
            if gpu.present(&cmd, &tex, &drawable_tex).is_err() {
                bump_errors(&shared);
                return;
            }
            let mtl_drawable: &ProtocolObject<dyn MTLDrawable> = ProtocolObject::from_ref(&*drawable);
            if let Some(tp) = t_present {
                if free_running {
                    // not tied to the display's refresh: hand the frame over at its own moment (the GPU needs about `gpu_ms` first)
                    // (the measured GPU time includes waiting for the display when frames queue, so it is not trusted past a few ms: it would
                    // push the hand-over earlier, queue more frames, and measure even longer)
                    let lead = (0.8 + gpu_ms.min(2.5)) / 1e3;
                    wait_until(tp - lead);
                    cmd.presentDrawable(mtl_drawable);
                } else {
                    cmd.presentDrawable_atTime(mtl_drawable, tp);
                }
                next_present += 1.0 / eff_hz;
            } else if vsyncs > 1 {
                // exactly `vsyncs` refresh intervals (a hair under, so rounding cannot push it to one more)
                cmd.presentDrawable_afterMinimumDuration(mtl_drawable, (f64::from(vsyncs) - 0.5) / refresh);
            } else {
                cmd.presentDrawable(mtl_drawable);
            }
            {
                let (bits, t_commit, shown) = (latency_bits.clone(), unsafe { CACurrentMediaTime() }, shown.clone());
                let block = block2::RcBlock::new(move |d: NonNull<ProtocolObject<dyn MTLDrawable>>| {
                    let shown_at = unsafe { d.as_ref().presentedTime() };
                    let ms = (shown_at - t_commit) * 1e3;
                    // Metal reports no presented time for some presentation paths (display sync on, composited window): then the moment
                    // this callback runs, which follows the refresh it was shown on, stands in, so the pacing numbers are never empty
                    let at = if shown_at > 0.0 { shown_at } else { unsafe { CACurrentMediaTime() } };
                    lock(&shown).push(at);
                    if shown_at > 0.0 && ms.is_finite() && (0.0..500.0).contains(&ms) {
                        let old = f64::from_bits(bits.load(Ordering::Relaxed));
                        bits.store((old + (ms - old) * 0.05).to_bits(), Ordering::Relaxed);
                    }
                });
                // SAFETY: Metal copies the block; the pointer is only used for this call.
                unsafe { mtl_drawable.addPresentedHandler(&*block as *const _ as *mut _) };
            }
            cmd.commit();
            inflight.push_back(cmd);

            pacing.tick();
            s.presented += 1;
            if got_new {
                wait_ms += ((now() - cur_arrived) * 1e3 - wait_ms) * 0.1;
                if first_shown {
                    first_shown = false;
                }
            } else {
                s.generated += 1;
            }
            if s.presented % 15 == 0 {
                let t = tl.stats();
                let (mean, sd, max) = pacing.summary();
                s.source_fps = 1.0 / t.interval;
                s.processed_fps = content_ema;
                s.content_fps = src_locked;
                s.target_hz = eff_hz;
                s.repeat_share = repeat_share;
                s.present_interval_ms = mean;
                s.present_jitter_ms = sd;
                s.present_max_ms = max;
                s.gpu_ms = gpu_ms;
                s.frame_wait_ms = wait_ms;
                s.queue_delay_ms = t.queue_delay * 1e3;
                s.network_jitter_ms = t.jitter * 1e3;
                s.playout_ms = tl.playout() * 1e3;
                s.uncapped_active = uncapped;
                s.upscaled = upscaled;
                s.upscale_height = if upscaled { out.map_or(0, |o| o.1) } else { 0 };
                s.generating = generating;
                s.effective_hz = eff_hz;
                s.refresh_hz = refresh;
                s.present_latency_ms = f64::from_bits(latency_bits.load(Ordering::Relaxed));
                s.shown_hist = lock(&shown).histogram(1000.0 / eff_hz);
                (s.shown_interval_ms, s.shown_jitter_ms, s.shown_max_ms, s.shown_late) =
                    lock(&shown).summary(1000.0 / eff_hz);
                s.metalfx_active = gpu.fx_active();
                s.metalfx_available = gpu.fx_available;
                s.fsr_active = gpu.fsr_active();
                s.finish_pass = gpu.finish_pass_active();
                s.linear_active = gpu.linear_active();
                s.linear_unavailable = gpu.linear_unavailable();
                s.rcas_active = s.finish_pass && settings.rcas > 0.0 && !finish_off;
                s.finish_guard_off = finish_off;
                s.finish_guard_slow = finish_slow;
                s.finish_guard_limit = crate::gpu::FINISH_GUARD_PICTURES;
                s.finish_guard_ms = 0.7 * 1e3 / eff_hz.max(1.0);
                s.adaptive = settings.adaptive;
                s.asked_hz = asked_hz;
                s.adaptive_cap_hz = ladder.cap().map_or(0.0, f64::from);
                (s.adaptive_load, s.adaptive_peak) = ladder.load();
                s.adaptive_over_s = ladder.overloaded_for();
                s.gpu_inflight = inflight.len();
                s.mailbox_len = lock(&shared.mailbox).len();
                s.settings = settings;
                s.variable_refresh = vrr;
                s.drawables = layer.maximumDrawableCount();
                if let Some(e) = cam.estimate() {
                    s.camera_lag_ms = e.lag * 1e3;
                    s.camera_explained = e.explained;
                }
                let mut out = lock(&shared.stats);
                let (drops, errors) = (out.mailbox_drops, out.errors);
                *out = s.clone();
                out.mailbox_drops = drops;
                out.errors = errors;
            }
        });
    }
    // drain so nothing outlives the device
    while let Some(c) = inflight.pop_front() {
        c.waitUntilCompleted();
    }
}

/// Errors are published at once: a stream of bad frames never reaches the periodic stats update.
/// Wait until `target` on Metal's clock: sleep most of the way, then spin for the last half millisecond (a sleep alone overshoots by up
/// to a millisecond, which is a visible share of a 7 ms refresh).
fn wait_until(target: f64) {
    let mt = unsafe { CACurrentMediaTime() };
    if target - 0.0005 > mt {
        std::thread::sleep(Duration::from_secs_f64(target - 0.0005 - mt));
    }
    while unsafe { CACurrentMediaTime() } < target {
        std::hint::spin_loop();
    }
}

fn bump_errors(shared: &Shared) {
    lock(&shared.stats).errors += 1;
}

fn gpu_cur_size(gpu: &Gpu) -> (usize, usize) {
    gpu.source_size().unwrap_or((0, 0))
}

/// Camera compensation: replace the extrapolation of global motion by the rotation the stick has commanded.
#[allow(clippy::too_many_arguments)]
fn camera_extra(
    cam: &CameraComp,
    tl: &Timeline,
    id: u16,
    now: f64,
    alpha: f32,
    src: (usize, usize),
    enabled: bool,
    extra: &mut (f32, f32),
    s: &mut RenderStats,
) {
    s.camera_active = false;
    if !enabled {
        return;
    }
    let Some(t_cur) = tl.content_time(id) else { return };
    let Some(shift) = cam.shift(t_cur, now) else { return };
    // a bound on how far the picture may be moved: beyond a few percent the exposed edge becomes visible
    let (lx, ly) = (src.0 as f32 * 0.06, src.1 as f32 * 0.06);
    let sx = shift.0.clamp(-lx, lx);
    let sy = shift.1.clamp(-ly, ly);
    let g = cam.recent_motion();
    *extra = (alpha * g.0 - sx, alpha * g.1 - sy);
    s.camera_active = true;
}

#[cfg(test)]
mod tests {
    use super::{
        effective_rate, next_display_frame, quick_rate, requested_rate, DuplicateDisplay, RenderSettings, RepeatRule,
        DISPLAY_TIMER_INTERVAL, DUPLICATE_DISPLAY_THRESHOLD, MAX_REPEATS_IN_A_ROW, QUICK_WINDOW,
    };
    use std::time::{Duration, Instant};

    #[test]
    fn duplicate_fallback_waits_for_one_30_hz_interval() {
        assert_eq!(DUPLICATE_DISPLAY_THRESHOLD, Duration::from_nanos(100_000_000 / 3));
        assert_eq!(DISPLAY_TIMER_INTERVAL, Duration::from_nanos(50_000_000 / 3));
    }

    #[test]
    fn duplicate_is_buffered_then_released_by_the_timer() {
        let start = Instant::now();
        let mut display = DuplicateDisplay::new(start);
        assert_eq!(display.on_frame(1, false, start), Some(1));
        assert_eq!(display.on_frame(2, true, start + Duration::from_millis(16)), None);
        assert_eq!(display.timer_tick(start + DUPLICATE_DISPLAY_THRESHOLD - Duration::from_nanos(1)), None);
        assert_eq!(display.timer_tick(start + DUPLICATE_DISPLAY_THRESHOLD), Some(2));
        assert!(display.duplicate_display);
    }

    #[test]
    fn a_late_duplicate_is_released_on_the_next_timer_check() {
        let start = Instant::now();
        let mut display = DuplicateDisplay::new(start);
        assert_eq!(display.on_frame(1, false, start), Some(1));
        let late = start + DUPLICATE_DISPLAY_THRESHOLD + Duration::from_millis(5);
        assert_eq!(display.on_frame(2, true, late), None);
        assert_eq!(display.timer_tick(late), Some(2));
    }

    #[test]
    fn real_mailbox_frames_are_selected_before_due_duplicates() {
        let start = Instant::now();
        let mut display = DuplicateDisplay::new(start);
        assert_eq!(display.on_frame(1, false, start), Some(1));
        assert_eq!(display.on_frame(2, true, start + Duration::from_millis(16)), None);
        let mut mailbox = std::collections::VecDeque::from([3]);

        assert_eq!(next_display_frame(&mut mailbox, &mut display, start + DUPLICATE_DISPLAY_THRESHOLD), Some(3));
        assert_eq!(next_display_frame(&mut mailbox, &mut display, start + DUPLICATE_DISPLAY_THRESHOLD), Some(2));
    }

    #[test]
    fn a_new_picture_cancels_the_pending_duplicate() {
        let start = Instant::now();
        let mut display = DuplicateDisplay::new(start);
        assert_eq!(display.on_frame(1, false, start), Some(1));
        assert_eq!(display.on_frame(2, true, start + Duration::from_millis(16)), None);
        assert_eq!(display.on_frame(3, false, start + Duration::from_millis(20)), Some(3));
        assert_eq!(display.timer_tick(start + DUPLICATE_DISPLAY_THRESHOLD), None);
        assert_eq!(display.last_timestamp, start + Duration::from_millis(20));
    }

    #[test]
    fn the_first_picture_after_a_duplicate_fallback_keeps_the_fallback_timestamp() {
        let start = Instant::now();
        let mut display = DuplicateDisplay::new(start);
        assert_eq!(display.on_frame(1, false, start), Some(1));
        assert_eq!(display.on_frame(2, true, start + Duration::from_millis(16)), None);
        let shown_at = start + DUPLICATE_DISPLAY_THRESHOLD;
        assert_eq!(display.timer_tick(shown_at), Some(2));
        assert_eq!(display.on_frame(3, false, shown_at + Duration::from_millis(16)), Some(3));
        assert!(!display.duplicate_display);
        assert_eq!(display.last_timestamp, shown_at);
    }

    #[test]
    fn the_quick_rate_reads_the_last_half_second_and_needs_enough_pictures() {
        let now = Instant::now();
        let at = |ms: u64| now - Duration::from_millis(ms);
        // About 60 pictures a second over the last 160 ms; keep every timestamp at or before `now`.
        let t: std::collections::VecDeque<Instant> = (0..10).map(|i| at(160 - i * 17)).collect();
        let q = quick_rate(&t, now, QUICK_WINDOW).unwrap();
        assert!((q - 59.0).abs() < 4.0, "{q}");
        // a loading screen: a picture every 100 ms is too few to say anything quickly
        let slow: std::collections::VecDeque<Instant> = (0..5).map(|i| at(450 - i * 100)).collect();
        assert_eq!(quick_rate(&slow, now, QUICK_WINDOW), None);
        // pictures that are older than the window do not count
        let old: std::collections::VecDeque<Instant> = (0..20).map(|i| at(900 + i * 17)).collect();
        assert_eq!(quick_rate(&old, now, QUICK_WINDOW), None);
    }

    #[test]
    fn the_quick_rate_detects_a_rising_source_rate_with_five_pictures() {
        let now = Instant::now();
        let times: std::collections::VecDeque<Instant> =
            (0..5).map(|i| now - Duration::from_millis(68 - i * 17)).collect();
        let rate = quick_rate(&times, now, QUICK_WINDOW).unwrap();
        assert!((rate - 59.0).abs() < 4.0, "{rate}");
    }

    #[test]
    fn the_quick_rate_can_raise_pacing_after_three_fast_pictures() {
        let now = Instant::now();
        let times: std::collections::VecDeque<Instant> =
            (0..3).map(|i| now - Duration::from_millis(34 - i * 17)).collect();
        let rate = quick_rate(&times, now, QUICK_WINDOW).unwrap();
        assert!((rate - 59.0).abs() < 4.0, "{rate}");
    }

    /// A recording from a real console (a game at about 30 fps in a 60 fps stream): the changed-byte count of every frame, as the
    /// diagnostic wrote it. The pictures alternate: a new one, then its re-encoded copy.
    #[test]
    fn a_recording_of_a_real_30_in_60_stream_is_read_as_half_repeats() {
        let counts: Vec<usize> = include_str!("../tests/data/frame_diag_30in60.txt")
            .lines()
            .map(|l| l.trim().parse::<i64>().map_or(usize::MAX, |v| if v < 0 { usize::MAX } else { v as usize }))
            .collect();
        assert!(counts.len() > 1000);
        let (mut rule, mut repeats, mut wrong) = (RepeatRule::default(), 0usize, 0usize);
        for (i, &n) in counts.iter().enumerate().skip(1) {
            let rep = rule.is_repeat(n);
            repeats += usize::from(rep);
            // in the recording the copies are the odd frames
            if rep != (i % 2 == 1) {
                wrong += 1;
            }
        }
        let share = repeats as f64 / (counts.len() - 1) as f64;
        assert!((0.44..=0.56).contains(&share), "repeat share {share:.3}");
        assert!((wrong as f64) < 0.05 * counts.len() as f64, "{wrong} frames classified against the alternation");
    }

    #[test]
    fn copies_are_repeats_and_new_pictures_are_not() {
        let mut r = RepeatRule::default();
        for d in [420, 380, 11_000, 650, 510] {
            assert!(!r.is_repeat(d));
        }
        assert!(r.is_repeat(2), "bit-identical");
        assert!(r.is_repeat(40));
        assert!(!r.is_repeat(300), "a quiet new picture");
        assert!(!r.is_repeat(5_000));
        assert!(!r.is_repeat(usize::MAX), "unknown is not a repeat");
        // too little to compare against: a still scene, or the first frames
        let mut still = RepeatRule::default();
        for d in [20, 25, 22, 18, 24, 30] {
            assert!(!still.is_repeat(d));
        }
    }

    /// Gameplay hands over to a menu or an intro: every real picture changes by far less than the gameplay's did. None of them may be
    /// thrown away (that froze the screen and made the frame rate fall), and the rule has to settle on the new level.
    #[test]
    fn a_scene_that_goes_quiet_does_not_freeze_the_picture() {
        let mut r = RepeatRule::default();
        let mut kept_gameplay = 0;
        for i in 0..40 {
            // 30 fps game in a 60 fps stream: a new picture (about 5000), then its copy (about 300)
            if !r.is_repeat(if i % 2 == 0 { 5_000 } else { 300 }) {
                kept_gameplay += 1;
            }
        }
        assert!(
            (20..=21).contains(&kept_gameplay),
            "half of the gameplay frames are copies (the first has nothing to compare with): {kept_gameplay}"
        );
        // the menu: every frame is a new picture with a small change, none of them a copy of the one before
        let menu: Vec<bool> = (0..100).map(|i| r.is_repeat(40 + (i * 7) % 30)).collect();
        let dropped_in_a_row = menu.split(|d| !d).map(<[bool]>::len).max().unwrap_or(0);
        assert!(dropped_in_a_row <= MAX_REPEATS_IN_A_ROW, "{dropped_in_a_row} pictures in a row were thrown away");
        assert!(menu.iter().filter(|d| !**d).count() >= 70, "a menu's pictures are kept");
        assert!(menu[10..].iter().all(|d| !d), "the rule has settled on the quiet level: {menu:?}");
        // and a game that really is at 30 fps in its menu still has its copies removed
        let mut copies = 0;
        for i in 0..40 {
            copies += usize::from(r.is_repeat(if i % 2 == 0 { 400 } else { 20 }));
        }
        assert!(copies >= 15, "copies are found again at the new level: {copies}");
    }

    #[test]
    fn a_multiple_of_the_streams_real_rate_decides_the_rate_when_it_is_set() {
        let rate = |src: f64, f: f32| {
            requested_rate(&RenderSettings { gen_factor: f, display_hz: 144, ..Default::default() }, src)
        };
        assert_eq!(rate(60.0, 1.5), 90);
        assert_eq!(rate(60.0, 2.0), 120);
        assert_eq!(rate(60.0, 2.5), 150);
        assert_eq!(rate(60.0, 3.0), 180);
        assert_eq!(rate(30.0, 2.5), 75);
        assert_eq!(rate(60.0, 1.0), 60, "1x is the stream as it comes");
        // a game that renders 45 frames a second is shown at a multiple of 45, not of the nominal 60
        assert_eq!(rate(45.0, 2.0), 90);
        assert_eq!(rate(45.0, 2.5), 113);
        assert_eq!(rate(45.0, 3.0), 135);
        assert_eq!(rate(60.0, 0.0), 144, "without a multiple the display rate is used");
        // the display's refresh rate is still the ceiling
        assert_eq!(effective_rate(rate(60.0, 3.0), 144.0).0, 144.0);
        assert_eq!(effective_rate(rate(60.0, 2.5), 120.0).0, 120.0);
    }

    #[test]
    fn rates_are_exact_and_only_even_fractions_are_held_for_whole_refreshes() {
        // a 144 Hz panel: what is asked for is what is shown
        assert_eq!(effective_rate(144, 144.0), (144.0, 1, false));
        assert_eq!(effective_rate(120, 144.0), (120.0, 1, true), "120 on 144 Hz is capped at 120, not rounded up");
        assert_eq!(effective_rate(75, 144.0), (75.0, 1, true));
        assert_eq!(effective_rate(60, 144.0), (60.0, 1, true));
        assert_eq!(effective_rate(72, 144.0), (72.0, 2, false), "an even fraction is held for whole refreshes");
        assert_eq!(effective_rate(48, 144.0), (48.0, 3, false));
        // a 120 Hz panel
        assert_eq!(effective_rate(120, 120.0), (120.0, 1, false));
        assert_eq!(effective_rate(60, 120.0), (60.0, 2, false));
        assert_eq!(effective_rate(75, 120.0), (75.0, 1, true));
        assert_eq!(effective_rate(144, 120.0), (120.0, 1, false), "never above the panel");
        // a plain 60 Hz panel
        assert_eq!(effective_rate(60, 60.0), (60.0, 1, false));
        assert_eq!(effective_rate(120, 60.0), (60.0, 1, false));
        assert_eq!(effective_rate(30, 60.0), (30.0, 2, false));
        // unknown or nonsense refresh rates do not panic or divide by zero
        assert_eq!(effective_rate(120, 0.0), (120.0, 1, false));
        assert_eq!(effective_rate(60, f64::NAN), (60.0, 1, false));
        assert_eq!(effective_rate(60, -5.0), (60.0, 1, false));
        for req in 1..=300u16 {
            for refresh in [60.0, 75.0, 120.0, 144.0, 165.0] {
                let (hz, k, capped) = effective_rate(req, refresh);
                assert!(hz <= refresh + 1e-9 && k >= 1, "{req}@{refresh} -> {hz} x {k}");
                assert!(
                    hz <= f64::from(req) * 1.011 || hz >= refresh - 1e-9, // an even fraction within 1 % counts as the asked rate
                    "never faster than asked: {req}@{refresh} -> {hz}"
                );
                if !capped {
                    assert!(
                        (hz * f64::from(k) - refresh).abs() < 1e-6 || (req as f64) >= refresh * 0.99,
                        "{req}@{refresh}"
                    );
                }
            }
        }
    }
}
