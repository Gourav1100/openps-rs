pub use openps_input_host::cursor::{Cursor, NullCursor};

#[cfg(target_os = "macos")]
pub use mac::MacCursor;

#[cfg(target_os = "macos")]
mod mac {
    use super::Cursor;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CGPoint {
        x: f64,
        y: f64,
    }
    extern "C" {
        fn CGAssociateMouseAndMouseCursorPosition(connected: u32) -> i32;
        fn CGGetLastMouseDelta(dx: *mut i32, dy: *mut i32);
        fn CGWarpMouseCursorPosition(p: CGPoint) -> i32;
        fn CGDisplayHideCursor(display: u32) -> i32;
        fn CGDisplayShowCursor(display: u32) -> i32;
    }

    #[derive(Default)]
    pub struct MacCursor {
        engaged: bool,
        want_hide: bool,
        hidden: bool,
    }

    impl MacCursor {
        pub fn new() -> Self {
            Self::default()
        }
        fn flush_delta() -> (f64, f64) {
            let (mut dx, mut dy) = (0i32, 0i32);
            unsafe { CGGetLastMouseDelta(&mut dx, &mut dy) };
            (f64::from(dx), f64::from(dy))
        }
    }

    impl Cursor for MacCursor {
        fn engage(&mut self, center: Option<(f64, f64)>) {
            if let Some((x, y)) = center {
                unsafe { CGWarpMouseCursorPosition(CGPoint { x, y }) };
            }
            unsafe { CGAssociateMouseAndMouseCursorPosition(0) };
            Self::flush_delta(); // discard movement from before capture, or the first frame lurches
            self.engaged = true;
            if self.want_hide && !self.hidden {
                unsafe { CGDisplayHideCursor(0) }; // hide/show calls nest, so keep them balanced
                self.hidden = true;
            }
        }
        fn recenter(&mut self, (x, y): (f64, f64)) {
            if self.engaged {
                unsafe { CGWarpMouseCursorPosition(CGPoint { x, y }) };
            }
        }
        fn release(&mut self) {
            unsafe { CGAssociateMouseAndMouseCursorPosition(1) };
            if self.hidden {
                unsafe { CGDisplayShowCursor(0) };
                self.hidden = false;
            }
            self.engaged = false;
        }
        fn set_hide(&mut self, hide: bool) {
            self.want_hide = hide;
            if !hide && self.hidden {
                unsafe { CGDisplayShowCursor(0) };
                self.hidden = false;
            } else if hide && self.engaged && !self.hidden {
                unsafe { CGDisplayHideCursor(0) };
                self.hidden = true;
            }
        }
        fn delta(&mut self) -> (f64, f64) {
            Self::flush_delta()
        }
    }

    impl Drop for MacCursor {
        fn drop(&mut self) {
            self.release();
        }
    }
}
