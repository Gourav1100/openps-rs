//! macOS video output: a decoding media sink feeding an `AVSampleBufferDisplayLayer` that lives in its own view,
//! stacked above the web view and shown only while a session is connected.

use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{Encode, Encoding};
use openps_audio::media::AudioOut;
use openps_core::events::{AppEvent, EventSink, MediaSink};
use openps_core::{SessionState, StreamOptions};
use openps_metal::look::SharedLook;
use openps_metal::{FrameMode, MetalRenderer, RenderSettings, Upscale};
use openps_video::media::{DecodingMedia, PictureSink};
use openps_video::render::Renderer;
use openps_video::vt::Frame;
use std::sync::Arc;
use tauri::{AppHandle, WebviewWindow};

#[repr(C)]
#[derive(Clone, Copy)]
struct Point {
    x: f64,
    y: f64,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct Size {
    w: f64,
    h: f64,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct Rect {
    origin: Point,
    size: Size,
}
unsafe impl Encode for Point {
    const ENCODING: Encoding = Encoding::Struct("CGPoint", &[Encoding::Double, Encoding::Double]);
}
unsafe impl Encode for Size {
    const ENCODING: Encoding = Encoding::Struct("CGSize", &[Encoding::Double, Encoding::Double]);
}
unsafe impl Encode for Rect {
    const ENCODING: Encoding = Encoding::Struct("CGRect", &[Point::ENCODING, Size::ENCODING]);
}

/// Raw view pointer that may only be dereferenced on the main thread (we only do that inside `run_on_main_thread`).
#[derive(Clone, Copy)]
struct MainThreadView(*mut AnyObject);
unsafe impl Send for MainThreadView {}
unsafe impl Sync for MainThreadView {}

/// The layer the next view will host (one video view per app).
static HOSTED_LAYER: std::sync::atomic::AtomicPtr<AnyObject> = std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());

/// An `NSView` whose backing layer is the renderer's layer. This is the canonical way to host a `CAMetalLayer`: AppKit then owns
/// the layer for real and keeps its frame, scale and drawable size in step with the view (setting `layer` by hand does not).
fn layer_view_class() -> &'static objc2::runtime::AnyClass {
    use objc2::runtime::{AnyClass, Bool, ClassBuilder, Sel};
    use std::sync::atomic::Ordering;
    use std::sync::OnceLock;
    static CLASS: OnceLock<&'static AnyClass> = OnceLock::new();

    extern "C-unwind" fn make_backing_layer(_this: *mut AnyObject, _cmd: Sel) -> *mut AnyObject {
        // `make...` methods return an owned (+1) object
        unsafe { objc2::ffi::objc_retain(HOSTED_LAYER.load(Ordering::Acquire).cast()).cast() }
    }
    extern "C-unwind" fn wants_update_layer(_this: *mut AnyObject, _cmd: Sel) -> Bool {
        Bool::YES
    }
    CLASS.get_or_init(|| {
        let sup = AnyClass::get(c"NSView").expect("NSView");
        let mut b = ClassBuilder::new(c"OpenPSLayerView", sup).expect("class name is free");
        unsafe {
            b.add_method(
                objc2::sel!(makeBackingLayer),
                make_backing_layer as extern "C-unwind" fn(*mut AnyObject, Sel) -> *mut AnyObject,
            );
            b.add_method(
                objc2::sel!(wantsUpdateLayer),
                wants_update_layer as extern "C-unwind" fn(*mut AnyObject, Sel) -> Bool,
            );
        }
        b.register()
    })
}

/// Where decoded pictures go: the Metal pipeline (upscaling, frame generation, lowest latency) or, if Metal cannot start, the
/// system's sample-buffer layer.
#[derive(Clone)]
enum Backend {
    Metal(Arc<MetalRenderer>),
    System(Arc<Renderer>),
}

struct Output(Backend, Option<Arc<openps_scene::service::SceneService>>);
impl PictureSink for Output {
    fn picture(&self, picture: Frame) {
        if let Some(scene) = &self.1 {
            // the scene detector keeps one picture in a while, and only while it is switched on
            unsafe { scene.offer(picture.as_ptr()) };
        }
        match &self.0 {
            Backend::Metal(m) => m.submit(picture),
            Backend::System(r) => {
                if let Err(e) = r.show(&picture) {
                    log::warn!("render: {e:?}");
                }
            }
        }
    }
    fn unavailable(&self, reason: &str) {
        log::error!("video: {reason}");
    }
}

/// The highest rate asked of the renderer when the generation multiple is not used: the display's own refresh rate caps it.
pub const AUTO_RATE_HZ: u16 = 240;

/// The render settings a set of stream options asks for. A switched-off filter becomes the value that leaves the picture as it was:
/// sharpness blend 100 % with an upscaler (the picture as it draws it) or 50 % without (no unsharp mask), adaptive sharpening 0,
/// saturation 100 %. Frame generation is the chosen multiple of the stream's rate (1.0 when off), the target that the display's refresh
/// rate caps and the adaptive frame rate lowers; the adaptive frame rate and the smoothing buffer are always on.
pub fn render_settings(o: &StreamOptions) -> RenderSettings {
    let upscale = Upscale::from_factor(o.upscale_factor());
    let plain_sharpness = if upscale == Upscale::Off { 0.5 } else { 1.0 };
    let tenths = if o.frame_gen == 0 { 20 } else { o.frame_gen.clamp(10, 30) };
    RenderSettings {
        upscale,
        display_hz: AUTO_RATE_HZ,
        gen_factor: f32::from(tenths) / 10.0,
        mode: if o.frame_mode == "smooth" { FrameMode::Smooth } else { FrameMode::ZeroLatency },
        camera: o.camera_comp,
        // the engine upscales (MetalFX's scaler or FSR); the interpolator setting decides who makes the generated frames
        metalfx: o.interpolator != "builtin",
        fx_fallback: o.interpolator == "dynamic",
        fsr: o.engine == "fsr",
        linear_light: o.linear_light,
        adaptive: true,
        jitter_buffer: true,
        uncapped: false,
        sharpness: if o.sharpness_on { f32::from(o.sharpness.min(200)) / 100.0 } else { plain_sharpness },
        rcas: if o.rcas_on { f32::from(o.rcas.min(200)) / 100.0 } else { 0.0 },
        saturation: if o.saturation_on { f32::from(o.saturation.min(200)) / 100.0 } else { 1.0 },
        immersion: o.immersion,
    }
}

/// Everything the app needs from the video side.
pub struct Video {
    pub media: Arc<dyn MediaSink>,
    /// Sound output; also the handle for volume and status.
    pub audio: Arc<AudioOut>,
    view: MainThreadView,
    backend: Backend,
}

impl Video {
    /// Create the renderer and its view inside `window`. Must run on the main thread (Tauri's `setup` does).
    pub fn install(
        window: &WebviewWindow,
        look: SharedLook,
        settings: RenderSettings,
        scene: Option<Arc<openps_scene::service::SceneService>>,
    ) -> Result<Self, String> {
        let want_system = std::env::var("OPENPS_RENDERER").is_ok_and(|v| v.eq_ignore_ascii_case("av"));
        let backend = match (want_system, MetalRenderer::new(look, settings)) {
            (false, Ok(m)) => Backend::Metal(Arc::new(m)),
            (_, m) => {
                if let Err(e) = &m {
                    log::warn!("Metal renderer unavailable ({e}); using the system layer");
                }
                Backend::System(Arc::new(Renderer::new().map_err(|e| format!("{e:?}"))?))
            }
        };
        let layer_ptr = match &backend {
            Backend::Metal(m) => m.layer_ptr(),
            Backend::System(r) => r.layer_ptr(),
        };
        let ns_window = window.ns_window().map_err(|e| e.to_string())? as *mut AnyObject;
        let view = unsafe {
            let content: *mut AnyObject = msg_send![ns_window, contentView];
            if content.is_null() {
                return Err("window has no content view".into());
            }
            let bounds: Rect = msg_send![content, bounds];
            HOSTED_LAYER.store(layer_ptr as *mut AnyObject, std::sync::atomic::Ordering::Release);
            let class = layer_view_class();
            let alloc: *mut AnyObject = msg_send![class, alloc];
            let view: Retained<AnyObject> =
                Retained::from_raw(msg_send![alloc, initWithFrame: bounds]).ok_or("view init")?;
            let _: () = msg_send![&*view, setAutoresizingMask: 18usize]; // width | height sizable
                                                                         // wantsLayer makes AppKit ask the view for its backing layer: ours. From then on AppKit keeps its frame, scale and
                                                                         // drawable size in step with the view.
            let _: () = msg_send![&*view, setWantsLayer: true];
            let _: () = msg_send![&*view, setHidden: true];
            // NSWindowBelow (-1) relative to nothing = behind every sibling, i.e. behind the web view: the page draws its
            // controls over the picture, and shows the picture wherever the page itself is transparent.
            let below: isize = -1;
            let no_view: *mut AnyObject = std::ptr::null_mut();
            let _: () = msg_send![content, addSubview: &*view, positioned: below, relativeTo: no_view];
            let ns_color = objc2::runtime::AnyClass::get(c"NSColor").ok_or("NSColor missing")?;
            let black: *mut AnyObject = msg_send![ns_color, blackColor];
            let _: () = msg_send![ns_window, setBackgroundColor: black]; // letterbox bars are black, not the desktop
            Retained::into_raw(view) // owned by the content view now; our raw copy is for show/hide only
        };
        // The web view must not paint an opaque background of its own, or it would hide the video view behind it.
        window
            .with_webview(|wv| unsafe {
                let web = wv.inner() as *mut AnyObject;
                let ns_number = objc2::runtime::AnyClass::get(c"NSNumber").expect("NSNumber");
                let no: *mut AnyObject = msg_send![ns_number, numberWithBool: false];
                let key = crate::video::nsstring("drawsBackground");
                let _: () = msg_send![web, setValue: no, forKey: &*key];
            })
            .map_err(|e| e.to_string())?;
        let audio = Arc::new(AudioOut::system());
        let decoding = Arc::new(DecodingMedia::new(
            Arc::new(Output(backend.clone(), scene)),
            Some(audio.clone() as Arc<dyn MediaSink>),
        ));
        decoding.set_bgra(matches!(backend, Backend::Metal(_))); // the Metal pipeline reads BGRA
        let media: Arc<dyn MediaSink> = decoding;
        Ok(Video { media, audio, view: MainThreadView(view), backend })
    }

    /// Live settings change (upscaling, display rate, frame mode, camera compensation).
    pub fn apply(&self, o: &StreamOptions) {
        if let Backend::Metal(m) = &self.backend {
            m.set_settings(render_settings(o));
        }
    }

    /// Record every arriving frame for `secs` seconds and write `frame-diag.csv` (id, seconds, changed sample bytes, kept) to `dir`; returns
    /// a one-line summary: how many frames the console sent per second, how many were different pictures, and how many ids were skipped.
    pub fn frame_diagnostic(&self, dir: &std::path::Path, secs: f64) -> Result<String, String> {
        let Backend::Metal(m) = &self.backend else { return Err("needs the Metal renderer".into()) };
        m.start_diag(secs);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs_f64(secs + 3.0);
        let rows = loop {
            if let Some(r) = m.take_diag() {
                break r;
            }
            if std::time::Instant::now() > deadline {
                return Err("no frames arrived".into());
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        };
        if rows.len() < 2 {
            return Err("no frames arrived".into());
        }
        let span = (rows[rows.len() - 1].1 - rows[0].1).max(0.001);
        let kept = rows.iter().filter(|r| r.3).count();
        let accepted = rows.iter().filter(|r| r.4).count();
        let gaps =
            rows.windows(2).filter(|w| w[1].0.wrapping_sub(w[0].0) > 1 && w[1].0.wrapping_sub(w[0].0) < 1000).count();
        let mut csv = String::from("id,seconds,changed_bytes,new_picture,accepted\n");
        for r in &rows {
            csv += &format!(
                "{},{:.4},{},{},{}\n",
                r.0,
                r.1,
                if r.2 == usize::MAX { -1 } else { r.2 as i64 },
                u8::from(r.3),
                u8::from(r.4)
            );
        }
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let path = dir.join("frame-diag.csv");
        std::fs::write(&path, csv).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(format!(
            "{:.1} frames/s arrived, {:.1} new pictures/s, {:.1} accepted/s, {:.0}% repeats, {} id gaps in {:.0} s (saved to logs/frame-diag.csv)",
            rows.len() as f64 / span,
            kept as f64 / span,
            accepted as f64 / span,
            100.0 * (rows.len() - kept) as f64 / rows.len() as f64,
            gaps,
            span
        ))
    }

    /// Renderer statistics as JSON (for the HUD and diagnostics).
    pub fn stats(&self) -> serde_json::Value {
        match &self.backend {
            Backend::System(_) => serde_json::json!({ "renderer": "system" }),
            Backend::Metal(m) => {
                let s = m.stats();
                let mut stats = serde_json::json!({
                    "renderer": "metal",
                    "real_frames": s.real_frames, "presented": s.presented, "generated": s.generated,
                    "mailbox_drops": s.mailbox_drops, "errors": s.errors,
                    "source_fps": s.source_fps, "content_fps": s.content_fps, "repeat_share": s.repeat_share, "target_hz": s.target_hz, "processed_fps": s.processed_fps,
                    "present_interval_ms": s.present_interval_ms, "present_jitter_ms": s.present_jitter_ms,
                    "present_max_ms": s.present_max_ms, "shown_interval_ms": s.shown_interval_ms, "shown_jitter_ms": s.shown_jitter_ms, "shown_max_ms": s.shown_max_ms, "shown_late": s.shown_late, "shown_hist": s.shown_hist, "frame_wait_ms": s.frame_wait_ms, "gpu_ms": s.gpu_ms,
                    "queue_delay_ms": s.queue_delay_ms, "network_jitter_ms": s.network_jitter_ms, "playout_ms": s.playout_ms, "uncapped_active": s.uncapped_active,
                    "upscaled": s.upscaled, "upscale_height": s.upscale_height, "generating": s.generating,
                    "camera_active": s.camera_active, "camera_lag_ms": s.camera_lag_ms, "camera_explained": s.camera_explained, "input_prior_active": s.input_prior_active,
                    "drawable": [s.drawable_size.0, s.drawable_size.1],
                    "effective_hz": s.effective_hz, "refresh_hz": s.refresh_hz, "present_latency_ms": s.present_latency_ms, "metalfx_active": s.metalfx_active, "metalfx_available": s.metalfx_available, "fsr_active": s.fsr_active, "variable_refresh": s.variable_refresh,  "drawables": s.drawables,
                });
                // more outside the macro, which has a recursion limit on the number of keys: what the debug bar shows
                let c = s.settings;
                stats["finish_pass"] = serde_json::Value::Bool(s.finish_pass);
                stats["linear_active"] = serde_json::Value::Bool(s.linear_active);
                stats["linear_unavailable"] = serde_json::Value::Bool(s.linear_unavailable);
                stats["rcas_active"] = serde_json::Value::Bool(s.rcas_active);
                stats["metalfx_fallback"] = serde_json::Value::Bool(s.metalfx_fallback);
                stats["gpu_inflight"] = s.gpu_inflight.into();
                stats["mailbox_len"] = s.mailbox_len.into();
                stats["finish_guard"] = serde_json::json!({
                    "off": s.finish_guard_off, "slow": s.finish_guard_slow, "limit": s.finish_guard_limit, "ms": s.finish_guard_ms,
                });
                stats["adaptive"] = serde_json::json!({
                    "on": s.adaptive, "asked_hz": s.asked_hz, "cap_hz": s.adaptive_cap_hz, "load": s.adaptive_load, "peak": s.adaptive_peak,
                    "over_s": s.adaptive_over_s, "down_after_s": openps_metal::ladder::DOWN_AFTER_S,
                    "heavy_mean": openps_metal::ladder::HEAVY_MEAN, "heavy_peak": openps_metal::ladder::HEAVY_PEAK,
                    "step_hz": openps_metal::ladder::STEP_HZ,
                });
                stats["config"] = serde_json::json!({
                    "upscale_factor": c.upscale.factor(), "display_hz": c.display_hz, "gen_factor": c.gen_factor,
                    "smooth": c.mode == FrameMode::Smooth, "camera": c.camera, "metalfx": c.metalfx, "fx_fallback": c.fx_fallback, "fsr": c.fsr,
                    "linear_light": c.linear_light, "adaptive": c.adaptive,
                    "sharpness": c.sharpness, "rcas": c.rcas, "saturation": c.saturation, "immersion": c.immersion,
                });
                stats
            }
        }
    }

    /// Wrap an event sink so the video view appears when a session connects and disappears when it ends.
    pub fn follow_session(&self, app: AppHandle, inner: Arc<dyn EventSink>) -> Arc<dyn EventSink> {
        Arc::new(Following { app, inner, view: self.view, backend: self.backend.clone() })
    }
}

struct Following {
    app: AppHandle,
    inner: Arc<dyn EventSink>,
    view: MainThreadView,
    backend: Backend,
}

impl EventSink for Following {
    fn emit(&self, event: AppEvent) {
        if let AppEvent::Session(s) = &event {
            let show = match s {
                SessionState::Connected { .. } => Some(true),
                SessionState::Closed { .. } | SessionState::Failed { .. } | SessionState::Idle => Some(false),
                _ => None,
            };
            if matches!(s, SessionState::Connecting) {
                if let Backend::Metal(m) = &self.backend {
                    m.reset(); // a new session: forget the old stream's timing and motion
                }
            }
            if let Some(show) = show {
                if !show {
                    match &self.backend {
                        Backend::System(r) => r.flush(), // the last picture of a finished session must not linger
                        Backend::Metal(m) => m.reset(),
                    }
                }
                let view = self.view;
                let _ = self.app.run_on_main_thread(move || unsafe {
                    let v = view;
                    let _: () = msg_send![v.0, setHidden: !show];
                });
            }
        }
        self.inner.emit(event);
    }
}

/// NSString from a Rust string (UTF-8, no Foundation binding crate needed).
pub(crate) fn nsstring(s: &str) -> Retained<AnyObject> {
    let class = objc2::runtime::AnyClass::get(c"NSString").expect("NSString");
    unsafe {
        let alloc: *mut AnyObject = msg_send![class, alloc];
        let obj: *mut AnyObject =
            msg_send![alloc, initWithBytes: s.as_ptr() as *const std::ffi::c_void, length: s.len(), encoding: 4usize];
        Retained::from_raw(obj).expect("NSString init")
    }
}

/// Tell the renderer which refresh rate the window's own display runs at (not necessarily the main display's). Call from the main
/// thread or any window event handler (AppKit's `NSWindow`/`NSScreen` are main-thread only; background callers must go through
/// `run_on_main_thread`); harmless when the window has no screen yet. A failed fullscreen query keeps the last known value.
pub fn sync_refresh(window: &tauri::Window) {
    static LAST_FULLSCREEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if let Ok(fs) = window.is_fullscreen() {
        LAST_FULLSCREEN.store(fs, std::sync::atomic::Ordering::Relaxed);
    }
    openps_metal::set_fullscreen(LAST_FULLSCREEN.load(std::sync::atomic::Ordering::Relaxed));
    let Ok(ns_window) = window.ns_window() else { return };
    let ns_window = ns_window as *mut AnyObject;
    unsafe {
        let screen: *mut AnyObject = msg_send![ns_window, screen];
        if screen.is_null() {
            return;
        }
        let fps: isize = msg_send![screen, maximumFramesPerSecond];
        openps_metal::set_window_refresh_hz(fps as f64);
    }
}
