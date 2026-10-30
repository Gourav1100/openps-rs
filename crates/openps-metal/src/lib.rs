pub mod camera;
pub mod gpu;
pub mod look;
#[cfg(target_os = "macos")]
pub mod renderer;
pub mod shaders;
pub mod timeline;

#[cfg(target_os = "macos")]
pub use renderer::{
    effective_rate, now, requested_rate, FrameMode, MetalRenderer, RenderSettings, RenderStats, Upscale,
};

/// The refresh rate of the display the window is on (milli-Hz; 0 = not reported yet). Set by the app whenever the window moves,
/// resizes or enters full screen: the main display is not necessarily the one the stream is shown on.
static WINDOW_REFRESH_MILLI_HZ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Whether the window is in full screen. Full screen needs a third drawable (the compositor keeps one for an extra refresh, so with
/// two the renderer is locked at half rate); windowed play is a whole refresh quicker with two (measured: 15.5 ms vs 23.9 ms from
/// hand-over to display at 120 Hz). The renderer follows this flag.
static FULLSCREEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_fullscreen(on: bool) {
    FULLSCREEN.store(on, std::sync::atomic::Ordering::Relaxed);
}

pub(crate) fn fullscreen() -> bool {
    FULLSCREEN.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn set_window_refresh_hz(hz: f64) {
    let v = if hz.is_finite() && (1.0..=1000.0).contains(&hz) { (hz * 1000.0).round() as u32 } else { 0 };
    WINDOW_REFRESH_MILLI_HZ.store(v, std::sync::atomic::Ordering::Relaxed);
}

/// Refresh rate to pace against: the window's display if known, else the main display (0 if unknown).
pub fn main_display_hz() -> f64 {
    let w = WINDOW_REFRESH_MILLI_HZ.load(std::sync::atomic::Ordering::Relaxed);
    if w > 0 {
        return f64::from(w) / 1000.0;
    }
    system_main_display_hz()
}

/// Can the main display change its refresh rate frame by frame (variable refresh / Adaptive Sync, switched on in System Settings)?
/// Then a frame scheduled for a moment is shown at that moment whatever the rate. Such a display reports a longest refresh interval
/// well above its shortest one; a fixed one reports the same for both.
#[cfg(target_os = "macos")]
pub fn display_is_variable() -> bool {
    use objc2::runtime::{AnyClass, AnyObject};
    let Some(class) = AnyClass::get(c"NSScreen") else { return false };
    // SAFETY: `mainScreen`, `minimumRefreshInterval` and `maximumRefreshInterval` are plain getters (the latter two need macOS 12 and 14;
    // a screen that lacks them answers false).
    unsafe {
        let screen: *mut AnyObject = objc2::msg_send![class, mainScreen];
        if screen.is_null() {
            return false;
        }
        let has_max: bool = objc2::msg_send![screen, respondsToSelector: objc2::sel!(maximumRefreshInterval)];
        if !has_max {
            return false;
        }
        let min: f64 = objc2::msg_send![screen, minimumRefreshInterval];
        let max: f64 = objc2::msg_send![screen, maximumRefreshInterval];
        min > 0.0 && max > min * 1.05
    }
}

#[cfg(target_os = "macos")]
fn system_main_display_hz() -> f64 {
    extern "C" {
        fn CGMainDisplayID() -> u32;
        fn CGDisplayCopyDisplayMode(d: u32) -> *const std::ffi::c_void;
        fn CGDisplayModeGetRefreshRate(m: *const std::ffi::c_void) -> f64;
        fn CGDisplayModeRelease(m: *const std::ffi::c_void);
    }
    unsafe {
        let m = CGDisplayCopyDisplayMode(CGMainDisplayID());
        if m.is_null() {
            return 0.0;
        }
        let hz = CGDisplayModeGetRefreshRate(m);
        CGDisplayModeRelease(m);
        hz
    }
}

#[cfg(not(target_os = "macos"))]
fn system_main_display_hz() -> f64 {
    0.0
}
