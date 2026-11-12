//! The detector thread: takes a decoded picture now and then, runs the model, and keeps the filter's scene on the [`SceneLink`].
//!
//! The video path only calls [`SceneService::offer`], which does nothing unless detection is on and the last picture is half a second
//! old; everything else (loading the model, running it, the filter) happens here, off the decode and input threads.

use crate::encoder::{self, Encoder};
use crate::filter::{Params, SceneFilter};
use crate::probe::Probe;
use crate::{find_models_dir, PROBE_FILE};
use openps_input::scene::{Action, DetectorState, SceneLink};
use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

/// How often a picture is looked at. The model takes about 5 ms; the filter is made for pictures seconds apart.
pub const PICTURE_INTERVAL_S: f64 = 0.5;

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRetain(cf: *const c_void) -> *const c_void;
    fn CFRelease(cf: *const c_void);
}

extern "C" {
    fn pthread_set_qos_class_self_np(qos_class: u32, relative_priority: i32) -> i32;
}
const QOS_CLASS_UTILITY: u32 = 0x11;
const QOS_CLASS_DEFAULT: u32 = 0x15;

/// A retained `CVPixelBuffer`.
struct Picture {
    buffer: *const c_void,
    /// When it was taken, on the link's clock.
    at: f64,
}

// CoreVideo buffers are reference counted and safe to hand across threads.
unsafe impl Send for Picture {}

impl Drop for Picture {
    fn drop(&mut self) {
        unsafe { CFRelease(self.buffer) };
    }
}

struct Shared {
    link: Arc<SceneLink>,
    cache_dir: PathBuf,
    models: Option<PathBuf>,
    slot: Mutex<Option<Picture>>,
    wake: Condvar,
    stop: AtomicBool,
    /// Bits of the link time of the last picture taken.
    last_offer: AtomicU64,
    /// The thread is still loading the model (which cannot be interrupted).
    loading: AtomicBool,
    /// The model loaded.
    ready: AtomicBool,
}

/// Called on the detector's thread once the model is loaded.
type OnLoaded = Box<dyn FnOnce() + Send>;

pub struct SceneService {
    shared: Arc<Shared>,
    join: Mutex<Option<JoinHandle<()>>>,
}

impl SceneService {
    /// Start the thread. The model is looked for in the usual places ([`find_models_dir`]) and compiled into `cache_dir` the first time.
    pub fn start(link: Arc<SceneLink>, cache_dir: PathBuf) -> SceneService {
        Self::start_with(link, cache_dir, find_models_dir())
    }

    pub fn start_with(link: Arc<SceneLink>, cache_dir: PathBuf, models: Option<PathBuf>) -> SceneService {
        Self::spawn(link, cache_dir, models, None)
    }

    /// [`SceneService::start`], calling `on_loaded` on the detector's thread once the model is loaded (never when it cannot be).
    pub fn start_notifying(
        link: Arc<SceneLink>,
        cache_dir: PathBuf,
        on_loaded: impl FnOnce() + Send + 'static,
    ) -> SceneService {
        Self::spawn(link, cache_dir, find_models_dir(), Some(Box::new(on_loaded)))
    }

    /// Whether the model has loaded (so a page that opened after the notice can still be told).
    pub fn model_ready(&self) -> bool {
        self.shared.ready.load(Ordering::Relaxed)
    }

    fn spawn(
        link: Arc<SceneLink>,
        cache_dir: PathBuf,
        models: Option<PathBuf>,
        on_loaded: Option<OnLoaded>,
    ) -> SceneService {
        let shared = Arc::new(Shared {
            link,
            cache_dir,
            models,
            slot: Mutex::new(None),
            wake: Condvar::new(),
            stop: AtomicBool::new(false),
            last_offer: AtomicU64::new(f64::NEG_INFINITY.to_bits()),
            loading: AtomicBool::new(true),
            ready: AtomicBool::new(false),
        });
        let s = shared.clone();
        let join = std::thread::Builder::new()
            .name("openps-scene".into())
            .spawn(move || run(&s, on_loaded))
            .expect("a thread");
        SceneService { shared, join: Mutex::new(Some(join)) }
    }

    /// A decoded picture (`CVPixelBufferRef`) for the detector. Cheap, and called for every picture: it keeps one every
    /// [`PICTURE_INTERVAL_S`] while detection is on and ignores the rest.
    ///
    /// # Safety
    /// `buffer` must be a valid `CVPixelBufferRef`.
    pub unsafe fn offer(&self, buffer: *const c_void) {
        let s = &self.shared;
        if buffer.is_null() || !s.link.running() {
            return;
        }
        let now = s.link.now();
        if now - f64::from_bits(s.last_offer.load(Ordering::Relaxed)) < PICTURE_INTERVAL_S {
            return;
        }
        s.last_offer.store(now.to_bits(), Ordering::Relaxed);
        let picture = Picture { buffer: CFRetain(buffer), at: now };
        *s.slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(picture); // a picture still waiting is replaced
        s.wake.notify_one();
    }

    /// Stop the thread and wait for it. A load that is under way is told to give up at its next step, but one compile cannot be
    /// interrupted, so it is not waited for: the thread ends by itself and the app's exit is not held up.
    pub fn stop(&self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        self.shared.wake.notify_all();
        let join = self.join.lock().unwrap_or_else(PoisonError::into_inner).take();
        if let Some(j) = join.filter(|_| !self.shared.loading.load(Ordering::Relaxed)) {
            let _ = j.join();
        }
        self.shared.slot.lock().unwrap_or_else(PoisonError::into_inner).take();
    }
}

impl Drop for SceneService {
    fn drop(&mut self) {
        self.stop();
    }
}

struct Loaded {
    encoder: Encoder,
    probe: Probe,
}

fn load(s: &Shared) -> Result<Loaded, String> {
    let dir = s.models.as_ref().ok_or("The scene model is not installed (assets/models).")?;
    let probe = Probe::from_json(
        &std::fs::read_to_string(dir.join(PROBE_FILE)).map_err(|e| format!("The scene probe cannot be read: {e}"))?,
    )?;
    let encoder = Encoder::load_until(dir, &s.cache_dir, &|| s.stop.load(Ordering::Relaxed))?;
    let layout = encoder::layout();
    if probe.dim() != encoder::DIM || probe.layout().iter().map(String::as_str).ne(layout.iter().map(|p| p.0)) {
        return Err(
            "The scene probe was made for other pictures than the scene models read; reinstall the models.".into()
        );
    }
    Ok(Loaded { encoder, probe })
}

fn run(s: &Shared, on_loaded: Option<OnLoaded>) {
    let link = &s.link;
    // loaded as the app starts, so switching detection on (F10 in a profile that uses it) finds the model ready; at low priority and on
    // this thread only, so the window, the stream and the input never wait for it
    unsafe { pthread_set_qos_class_self_np(QOS_CLASS_UTILITY, 0) };
    let (loaded, failure) = match load(s) {
        Ok(l) => (Some(l), None),
        Err(_) if s.stop.load(Ordering::Relaxed) => {
            s.loading.store(false, Ordering::Relaxed);
            return; // closing: the load was given up on, which is no failure
        }
        Err(e) => {
            log::error!("scene detection: {e}");
            (None, Some(e))
        }
    };
    unsafe { pthread_set_qos_class_self_np(QOS_CLASS_DEFAULT, 0) };
    s.loading.store(false, Ordering::Relaxed);
    if loaded.is_some() {
        s.ready.store(true, Ordering::Relaxed);
        if let Some(notify) = on_loaded {
            notify();
        }
    }
    let mut filter = SceneFilter::new(Params::default());
    let mut was_running = false;
    loop {
        let picture = {
            let mut slot = s.slot.lock().unwrap_or_else(PoisonError::into_inner);
            if slot.is_none() && !s.stop.load(Ordering::Relaxed) {
                slot = s.wake.wait_timeout(slot, Duration::from_millis(250)).unwrap_or_else(PoisonError::into_inner).0;
            }
            if s.stop.load(Ordering::Relaxed) {
                return;
            }
            slot.take()
        };
        if !link.running() {
            if was_running {
                was_running = false;
                filter.reset();
            }
            continue; // a picture taken just before it was switched off, or while a controller drives, is dropped
        }
        if !was_running {
            was_running = true;
            filter.reset();
            if let Some(e) = &failure {
                link.publish_state(DetectorState::Unavailable, e);
            }
        }
        let (Some(l), Some(picture)) = (loaded.as_ref(), picture) else { continue };
        let embedding = match unsafe { l.encoder.embed_pixel_buffer(picture.buffer) } {
            Ok(e) if e.len() == l.probe.dim() => e,
            Ok(e) => {
                log::warn!("scene detection: the model gave {} numbers, the probe reads {}", e.len(), l.probe.dim());
                continue;
            }
            Err(e) => {
                log::warn!("scene detection: {e}");
                continue;
            }
        };
        for a in Action::ALL {
            filter.set_available(a, link.is_labeled(a));
        }
        filter.set_allowed(link.allowed_contexts());
        for (t, a) in link.drain_actions() {
            filter.push_action(t, a);
        }
        let estimate = filter.update(picture.at, l.probe.context(&embedding), l.probe.view(&embedding));
        if link.running() {
            link.publish_scene(estimate.context, estimate.view, estimate.confidence);
            link.publish_state(DetectorState::Running, "");
        }
    }
}
