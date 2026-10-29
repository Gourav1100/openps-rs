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

struct Output(Backend);
impl PictureSink for Output {
    fn picture(&self, picture: Frame) {
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

/// The render settings a set of stream options asks for.
pub fn render_settings(o: &StreamOptions) -> RenderSettings {
    RenderSettings {
        upscale: match o.upscale.as_str() {
            "1440p" => Upscale::Qhd,
            "2160p" => Upscale::Uhd,
            _ => Upscale::Off,
        },
        display_hz: if (30..=240).contains(&o.display_fps) { o.display_fps } else { 60 },
        gen_factor: if o.frame_gen > 0 { f32::from(o.frame_gen) / 10.0 } else { 0.0 },
        mode: if o.frame_mode == "smooth" { FrameMode::Smooth } else { FrameMode::ZeroLatency },
        camera: o.camera_comp,
        metalfx: o.framegen == "metalfx",
        sharpness: f32::from(o.sharpness.min(200)) / 100.0,
        saturation: f32::from(o.saturation.min(200)) / 100.0,
        prompts: o.show_prompts,
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
    pub fn install(window: &WebviewWindow, look: SharedLook, settings: RenderSettings) -> Result<Self, String> {
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
        let decoding =
            Arc::new(DecodingMedia::new(Arc::new(Output(backend.clone())), Some(audio.clone() as Arc<dyn MediaSink>)));
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

    /// Save the plane the last prompt scan read (`prompt-plane-WxH.gray`) next to the detections (`prompt-scan.txt`) in `dir`, at most
    /// every ten seconds, so a wrong or missing keycap can be reproduced from the files.
    pub fn dump_prompt_scan(&self, dir: &std::path::Path) {
        use std::sync::Mutex;
        static LAST: Mutex<Option<std::time::Instant>> = Mutex::new(None);
        let Backend::Metal(m) = &self.backend else { return };
        let mut last = LAST.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if last.is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(10)) {
            return;
        }
        let Some(plane) = m.prompt_plane() else { return };
        *last = Some(std::time::Instant::now());
        let (data, w, h) = (&plane.0, plane.1, plane.2);
        let (hits, _, scans) = m.prompts();
        let text: String = hits
            .iter()
            .map(|d| format!("{:?} x={:.4} y={:.4} w={:.4} h={:.4} score={:.2}\n", d.kind, d.x, d.y, d.w, d.h, d.score))
            .collect();
        let _ = std::fs::write(dir.join(format!("prompt-plane-{w}x{h}.gray")), data);
        let _ = std::fs::write(dir.join("prompt-scan.txt"), format!("scan {scans}, plane {w}x{h}\n{text}"));
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

    /// Push the prompts to the page the moment they change (instead of the page asking): a thread watches the renderer's change counter
    /// and sends `{type: "prompts", ...}` on the app's event channel, with the video view's rectangle (it changes when the window does).
    pub fn watch_prompts(self: &Arc<Self>, app: AppHandle) {
        use tauri::{Emitter, Manager};
        let me = self.clone();
        let _ = std::thread::Builder::new().name("openps-prompt-events".into()).spawn(move || {
            let Backend::Metal(m) = &me.backend else { return };
            let (mut last, mut view, mut asked) = (u64::MAX, None::<[f64; 4]>, std::time::Instant::now());
            let mut had_hits = false;
            loop {
                std::thread::sleep(std::time::Duration::from_millis(16));
                let v = m.prompt_version();
                let mut view_changed = false;
                // the window's rectangle is only worth asking for while something is drawn (or has just changed)
                if v != last || (had_hits && asked.elapsed() > std::time::Duration::from_millis(300)) {
                    if let Some(w) = app.get_webview_window("main") {
                        if let Some(r) = me.view_rect(&w) {
                            view_changed = view != Some(r);
                            view = Some(r);
                        }
                    }
                    asked = std::time::Instant::now();
                }
                if v != last || view_changed {
                    last = v;
                    let mut out = me.prompt_hits();
                    had_hits = out["hits"].as_array().is_some_and(|h| !h.is_empty());
                    out["type"] = "prompts".into();
                    if let Some(r) = view {
                        out["view"] = serde_json::json!(r);
                    }
                    let _ = app.emit(crate::EVENT, out);
                }
            }
        });
    }

    /// How the video view sits against the web view, in points: `[left offset, bottom offset, width, height]` (the view's left and
    /// bottom edges relative to the web view's, then its size). The video fills the window's whole content area, but the page's own
    /// coordinates start below the title bar, so the page cannot assume the two share a top-left corner when it draws over the picture;
    /// it works the top out from the bottom edge and its own viewport height.
    pub fn view_rect(&self, window: &tauri::WebviewWindow) -> Option<[f64; 4]> {
        let (tx, rx) = std::sync::mpsc::channel();
        let view = self.view;
        window
            .with_webview(move |wv| {
                // SAFETY: runs on the main thread; both views are alive (the video view is owned by the content view).
                let view = view; // capture the whole (Send) wrapper, not just its pointer field
                let r = unsafe {
                    let web = wv.inner() as *mut AnyObject;
                    let vf: Rect = msg_send![view.0, frame];
                    let wf: Rect = msg_send![web, frame];
                    [vf.origin.x - wf.origin.x, wf.origin.y - vf.origin.y, vf.size.w, vf.size.h]
                };
                let _ = tx.send(r);
            })
            .ok()?;
        rx.recv_timeout(std::time::Duration::from_millis(500)).ok()
    }

    /// Controller button prompts found in the picture: `{aspect, scans, hits:[{kind,x,y,w,h}]}` (positions are 0..1 of the picture).
    pub fn prompt_hits(&self) -> serde_json::Value {
        match &self.backend {
            Backend::Metal(m) => {
                let (hits, aspect, scans) = m.prompts();
                // L3 and R3 are recognised (so they are not taken for something else) but get no keycap
                let hits: Vec<_> = hits
                    .iter()
                    .filter(|d| !d.kind.is_silent())
                    .map(|d| serde_json::json!({ "id": d.id, "kind": d.kind.control(), "x": d.x, "y": d.y, "w": d.w, "h": d.h }))
                    .collect();
                serde_json::json!({ "aspect": aspect, "scans": scans, "hits": hits })
            }
            Backend::System(_) => serde_json::json!({ "aspect": 1.78, "scans": 0, "hits": [] }),
        }
    }

    /// Renderer statistics as JSON (for the HUD and diagnostics).
    pub fn stats(&self) -> serde_json::Value {
        match &self.backend {
            Backend::System(_) => serde_json::json!({ "renderer": "system" }),
            Backend::Metal(m) => {
                let s = m.stats();
                serde_json::json!({
                    "renderer": "metal",
                    "real_frames": s.real_frames, "presented": s.presented, "generated": s.generated,
                    "mailbox_drops": s.mailbox_drops, "errors": s.errors,
                    "source_fps": s.source_fps, "content_fps": s.content_fps, "repeat_share": s.repeat_share, "target_hz": s.target_hz, "processed_fps": s.processed_fps,
                    "present_interval_ms": s.present_interval_ms, "present_jitter_ms": s.present_jitter_ms,
                    "present_max_ms": s.present_max_ms, "shown_interval_ms": s.shown_interval_ms, "shown_jitter_ms": s.shown_jitter_ms, "shown_max_ms": s.shown_max_ms, "shown_late": s.shown_late, "shown_hist": s.shown_hist, "frame_wait_ms": s.frame_wait_ms, "gpu_ms": s.gpu_ms,
                    "queue_delay_ms": s.queue_delay_ms, "network_jitter_ms": s.network_jitter_ms,
                    "upscaled": s.upscaled, "generating": s.generating,
                    "camera_active": s.camera_active, "camera_lag_ms": s.camera_lag_ms, "camera_explained": s.camera_explained,
                    "drawable": [s.drawable_size.0, s.drawable_size.1],
                    "effective_hz": s.effective_hz, "refresh_hz": s.refresh_hz, "present_latency_ms": s.present_latency_ms, "metalfx_active": s.metalfx_active, "metalfx_available": s.metalfx_available, "variable_refresh": s.variable_refresh,  "drawables": s.drawables,
                })
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
