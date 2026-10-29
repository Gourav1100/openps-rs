//! Presentation through `AVSampleBufferDisplayLayer`: decoded pixel buffers go straight to the compositor
//! (hardware YUV->RGB, no shader toolchain, correct colour handling). Frames are flagged display-immediately: this is
//! a live stream, so presentation timestamps must not delay anything.

use crate::vt::Frame;
use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject};
use objc2::{Encode, Encoding, RefEncode};
use std::ffi::c_void;
use std::ptr;

type CFTypeRef = *const c_void;

extern "C" {
    fn CFRelease(cf: CFTypeRef);
    fn CFArrayGetCount(a: CFTypeRef) -> isize;
    fn CFArrayGetValueAtIndex(a: CFTypeRef, i: isize) -> CFTypeRef;
    fn CFDictionarySetValue(d: CFTypeRef, key: CFTypeRef, value: CFTypeRef);
    static kCFBooleanTrue: CFTypeRef;
    static kCMSampleAttachmentKey_DisplayImmediately: CFTypeRef;
    fn CMVideoFormatDescriptionCreateForImageBuffer(alloc: CFTypeRef, image: CFTypeRef, out: *mut CFTypeRef) -> i32;
    fn CMSampleBufferCreateReadyWithImageBuffer(
        alloc: CFTypeRef,
        image: CFTypeRef,
        format: CFTypeRef,
        timing: *const Timing,
        out: *mut CFTypeRef,
    ) -> i32;
    fn CMSampleBufferGetSampleAttachmentsArray(sb: CFTypeRef, create: u8) -> CFTypeRef;
}

#[repr(C)]
struct Time {
    value: i64,
    timescale: i32,
    flags: u32,
    epoch: i64,
}
#[repr(C)]
struct Timing {
    duration: Time,
    presentation: Time,
    decode: Time,
}

/// `CMSampleBufferRef` as Objective-C sees it: a pointer to the opaque struct `opaqueCMSampleBuffer`.
#[repr(C)]
struct OpaqueSampleBuffer {
    _private: [u8; 0],
}
unsafe impl Encode for OpaqueSampleBuffer {
    const ENCODING: Encoding = Encoding::Struct("opaqueCMSampleBuffer", &[]);
}
unsafe impl RefEncode for OpaqueSampleBuffer {
    const ENCODING_REF: Encoding = Encoding::Pointer(&<Self as Encode>::ENCODING);
}

/// `AVQueuedSampleBufferRenderingStatus`
const STATUS_FAILED: isize = 2;

#[derive(Debug)]
pub enum RenderError {
    NoLayerClass,
    Frame(i32),
    /// The layer entered the failed state; it is flushed and recovers on the next frame, this is informational.
    LayerFailed,
}

pub struct Renderer {
    layer: Retained<AnyObject>,
}
// The layer is only touched through thread-safe AVSampleBufferDisplayLayer entry points (enqueue/flush/status);
// attaching it to a view is the caller's job on the main thread.
unsafe impl Send for Renderer {}
unsafe impl Sync for Renderer {}

impl Renderer {
    pub fn new() -> Result<Self, RenderError> {
        let class = AnyClass::get(c"AVSampleBufferDisplayLayer").ok_or(RenderError::NoLayerClass)?;
        let layer: Retained<AnyObject> = unsafe { msg_send![class, new] };
        // keep the picture's aspect ratio; letterbox the rest. AVLayerVideoGravityResizeAspect == "AVLayerVideoGravityResizeAspect"
        unsafe {
            let ns = objc2_foundation_string("AVLayerVideoGravityResizeAspect");
            let _: () = msg_send![&*layer, setVideoGravity: &*ns];
        }
        Ok(Self { layer })
    }

    /// The `CALayer` to add as a sublayer of the window's content view.
    pub fn layer_ptr(&self) -> *mut c_void {
        Retained::as_ptr(&self.layer) as *mut c_void
    }

    /// Queue one decoded picture for immediate display.
    pub fn show(&self, picture: &Frame) -> Result<(), RenderError> {
        unsafe {
            let mut format = ptr::null();
            let st = CMVideoFormatDescriptionCreateForImageBuffer(ptr::null(), picture.as_ptr(), &mut format);
            if st != 0 || format.is_null() {
                return Err(RenderError::Frame(st));
            }
            let invalid = || Time { value: 0, timescale: 0, flags: 0, epoch: 0 };
            let timing = Timing { duration: invalid(), presentation: invalid(), decode: invalid() };
            let mut sample = ptr::null();
            let st =
                CMSampleBufferCreateReadyWithImageBuffer(ptr::null(), picture.as_ptr(), format, &timing, &mut sample);
            CFRelease(format);
            if st != 0 || sample.is_null() {
                return Err(RenderError::Frame(st));
            }
            let attachments = CMSampleBufferGetSampleAttachmentsArray(sample, 1);
            if !attachments.is_null() && CFArrayGetCount(attachments) > 0 {
                let dict = CFArrayGetValueAtIndex(attachments, 0);
                if !dict.is_null() {
                    CFDictionarySetValue(dict, kCMSampleAttachmentKey_DisplayImmediately, kCFBooleanTrue);
                }
            }
            let failed = self.status() == STATUS_FAILED;
            if failed {
                let _: () = msg_send![&*self.layer, flush]; // clears the failed state so the next frame can display
            } else {
                let _: () = msg_send![&*self.layer, enqueueSampleBuffer: sample as *const OpaqueSampleBuffer];
            }
            CFRelease(sample);
            if failed {
                return Err(RenderError::LayerFailed);
            }
        }
        Ok(())
    }

    pub fn status(&self) -> isize {
        unsafe { msg_send![&*self.layer, status] }
    }

    /// Drop everything queued (reconnect, stream restart).
    pub fn flush(&self) {
        unsafe {
            let _: () = msg_send![&*self.layer, flushAndRemoveImage];
        }
    }
}

/// Minimal NSString creation without pulling in a Foundation binding crate.
fn objc2_foundation_string(s: &str) -> Retained<AnyObject> {
    let class = AnyClass::get(c"NSString").expect("NSString");
    let bytes = s.as_bytes();
    unsafe {
        let alloc: *mut AnyObject = msg_send![class, alloc];
        let obj: *mut AnyObject = msg_send![alloc, initWithBytes: bytes.as_ptr() as *const c_void, length: bytes.len(), encoding: 4usize /* NSUTF8StringEncoding */];
        Retained::from_raw(obj).expect("NSString init")
    }
}
