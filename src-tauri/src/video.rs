//! macOS video output: a decoding media sink feeding an `AVSampleBufferDisplayLayer` that lives in its own view,
//! stacked above the web view and shown only while a session is connected.

use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{Encode, Encoding};
use openps_audio::media::AudioOut;
use openps_core::events::{AppEvent, EventSink, MediaSink};
use openps_core::SessionState;
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

struct Output(Arc<Renderer>);
impl PictureSink for Output {
    fn picture(&self, picture: Frame) {
        if let Err(e) = self.0.show(&picture) {
            log::warn!("render: {e:?}");
        }
    }
    fn unavailable(&self, reason: &str) {
        log::error!("video: {reason}");
    }
}

/// Everything the app needs from the video side.
pub struct Video {
    pub media: Arc<dyn MediaSink>,
    /// Sound output; also the handle for volume and status.
    pub audio: Arc<AudioOut>,
    view: MainThreadView,
    renderer: Arc<Renderer>,
}

impl Video {
    /// Create the renderer and its view inside `window`. Must run on the main thread (Tauri's `setup` does).
    pub fn install(window: &WebviewWindow) -> Result<Self, String> {
        let renderer = Arc::new(Renderer::new().map_err(|e| format!("{e:?}"))?);
        let ns_window = window.ns_window().map_err(|e| e.to_string())? as *mut AnyObject;
        let view = unsafe {
            let content: *mut AnyObject = msg_send![ns_window, contentView];
            if content.is_null() {
                return Err("window has no content view".into());
            }
            let bounds: Rect = msg_send![content, bounds];
            let class = objc2::runtime::AnyClass::get(c"NSView").ok_or("NSView missing")?;
            let alloc: *mut AnyObject = msg_send![class, alloc];
            let view: Retained<AnyObject> =
                Retained::from_raw(msg_send![alloc, initWithFrame: bounds]).ok_or("view init")?;
            let _: () = msg_send![&*view, setAutoresizingMask: 18usize]; // width | height sizable
            let _: () = msg_send![&*view, setWantsLayer: true];
            let _: () = msg_send![&*view, setLayer: renderer.layer_ptr() as *mut AnyObject];
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
        let media: Arc<dyn MediaSink> =
            Arc::new(DecodingMedia::new(Arc::new(Output(renderer.clone())), Some(audio.clone() as Arc<dyn MediaSink>)));
        Ok(Video { media, audio, view: MainThreadView(view), renderer })
    }

    /// Wrap an event sink so the video view appears when a session connects and disappears when it ends.
    pub fn follow_session(&self, app: AppHandle, inner: Arc<dyn EventSink>) -> Arc<dyn EventSink> {
        Arc::new(Following { app, inner, view: self.view, renderer: self.renderer.clone() })
    }
}

struct Following {
    app: AppHandle,
    inner: Arc<dyn EventSink>,
    view: MainThreadView,
    renderer: Arc<Renderer>,
}

impl EventSink for Following {
    fn emit(&self, event: AppEvent) {
        if let AppEvent::Session(s) = &event {
            let show = match s {
                SessionState::Connected { .. } => Some(true),
                SessionState::Closed { .. } | SessionState::Failed { .. } | SessionState::Idle => Some(false),
                _ => None,
            };
            if let Some(show) = show {
                if !show {
                    self.renderer.flush(); // the last picture of a finished session must not linger
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
