//! Hardware video decode through VideoToolbox (macOS). Direct C FFI: the surface is small enough to audit by eye.
//!
//! Ownership rules (CoreFoundation "Create/Copy rule"): everything we create is released exactly once in `Drop`;
//! decoded `CVPixelBuffer`s are retained for the lifetime of a [`Frame`]. The decode callback runs on a VideoToolbox
//! thread and only pushes into a mutex-protected queue owned by the decoder.

use crate::nal::{self, Codec, ParameterSets};
use std::ffi::c_void;
use std::ptr;
use std::sync::{Arc, Mutex};

type OSStatus = i32;
type CFTypeRef = *const c_void;
type CMVideoFormatDescriptionRef = CFTypeRef;
type CMBlockBufferRef = CFTypeRef;
type CMSampleBufferRef = CFTypeRef;
type VTDecompressionSessionRef = CFTypeRef;
type CVImageBufferRef = CFTypeRef;

#[repr(C)]
#[derive(Clone, Copy)]
struct CMTime {
    value: i64,
    timescale: i32,
    flags: u32,
    epoch: i64,
}
const CMTIME_VALID: u32 = 1;
fn cmtime(value: i64) -> CMTime {
    CMTime { value, timescale: 90_000, flags: CMTIME_VALID, epoch: 0 }
}
fn cmtime_invalid() -> CMTime {
    CMTime { value: 0, timescale: 0, flags: 0, epoch: 0 }
}

#[repr(C)]
struct CMSampleTimingInfo {
    duration: CMTime,
    presentation: CMTime,
    decode: CMTime,
}

type DecodeCallback = extern "C" fn(
    refcon: *mut c_void,
    source_frame_refcon: *mut c_void,
    status: OSStatus,
    info_flags: u32,
    image: CVImageBufferRef,
    pts: CMTime,
    duration: CMTime,
);

#[repr(C)]
struct VTDecompressionOutputCallbackRecord {
    callback: DecodeCallback,
    refcon: *mut c_void,
}

extern "C" {
    fn CFRelease(cf: CFTypeRef);
    fn CFRetain(cf: CFTypeRef) -> CFTypeRef;
    fn CFDictionaryCreate(
        alloc: CFTypeRef,
        keys: *const CFTypeRef,
        values: *const CFTypeRef,
        n: isize,
        key_callbacks: *const c_void,
        value_callbacks: *const c_void,
    ) -> CFTypeRef;
    fn CFNumberCreate(alloc: CFTypeRef, the_type: isize, value: *const c_void) -> CFTypeRef;
    static kCFTypeDictionaryKeyCallBacks: u8;
    static kCFTypeDictionaryValueCallBacks: u8;
    static kCFBooleanTrue: CFTypeRef;
    static kCVPixelBufferPixelFormatTypeKey: CFTypeRef;
    static kCVPixelBufferMetalCompatibilityKey: CFTypeRef;
    fn CVPixelBufferGetIOSurface(b: CVImageBufferRef) -> *mut c_void;

    fn CMVideoFormatDescriptionCreateFromH264ParameterSets(
        alloc: CFTypeRef,
        count: usize,
        pointers: *const *const u8,
        sizes: *const usize,
        nal_header_length: i32,
        out: *mut CMVideoFormatDescriptionRef,
    ) -> OSStatus;
    fn CMVideoFormatDescriptionCreateFromHEVCParameterSets(
        alloc: CFTypeRef,
        count: usize,
        pointers: *const *const u8,
        sizes: *const usize,
        nal_header_length: i32,
        extensions: CFTypeRef,
        out: *mut CMVideoFormatDescriptionRef,
    ) -> OSStatus;
    fn CMBlockBufferCreateWithMemoryBlock(
        alloc: CFTypeRef,
        memory: *mut c_void,
        length: usize,
        block_alloc: CFTypeRef,
        custom: *const c_void,
        offset: usize,
        data_length: usize,
        flags: u32,
        out: *mut CMBlockBufferRef,
    ) -> OSStatus;
    fn CMBlockBufferReplaceDataBytes(src: *const c_void, dst: CMBlockBufferRef, offset: usize, len: usize) -> OSStatus;
    fn CMSampleBufferCreate(
        alloc: CFTypeRef,
        data: CMBlockBufferRef,
        ready: u8,
        make_ready: *const c_void,
        refcon: *const c_void,
        format: CMVideoFormatDescriptionRef,
        num_samples: isize,
        num_timing: isize,
        timing: *const CMSampleTimingInfo,
        num_sizes: isize,
        sizes: *const usize,
        out: *mut CMSampleBufferRef,
    ) -> OSStatus;

    fn VTDecompressionSessionCreate(
        alloc: CFTypeRef,
        format: CMVideoFormatDescriptionRef,
        decoder_spec: CFTypeRef,
        image_attrs: CFTypeRef,
        callback: *const VTDecompressionOutputCallbackRecord,
        out: *mut VTDecompressionSessionRef,
    ) -> OSStatus;
    fn VTDecompressionSessionDecodeFrame(
        session: VTDecompressionSessionRef,
        sample: CMSampleBufferRef,
        flags: u32,
        frame_refcon: *mut c_void,
        info_flags: *mut u32,
    ) -> OSStatus;
    fn VTDecompressionSessionWaitForAsynchronousFrames(session: VTDecompressionSessionRef) -> OSStatus;
    fn VTDecompressionSessionInvalidate(session: VTDecompressionSessionRef);

    fn CVPixelBufferGetWidth(b: CVImageBufferRef) -> usize;
    fn CVPixelBufferGetHeight(b: CVImageBufferRef) -> usize;
    fn CVPixelBufferGetPixelFormatType(b: CVImageBufferRef) -> u32;
    fn CVPixelBufferGetPlaneCount(b: CVImageBufferRef) -> usize;
    fn CVPixelBufferGetBaseAddress(b: CVImageBufferRef) -> *const u8;
    fn CVPixelBufferGetBytesPerRow(b: CVImageBufferRef) -> usize;
    fn CVPixelBufferLockBaseAddress(b: CVImageBufferRef, flags: u64) -> i32;
    fn CVPixelBufferUnlockBaseAddress(b: CVImageBufferRef, flags: u64) -> i32;
    fn CVPixelBufferGetBaseAddressOfPlane(b: CVImageBufferRef, plane: usize) -> *const u8;
    fn CVPixelBufferGetBytesPerRowOfPlane(b: CVImageBufferRef, plane: usize) -> usize;
    fn CVPixelBufferGetHeightOfPlane(b: CVImageBufferRef, plane: usize) -> usize;
}

const READ_ONLY: u64 = 1; // kCVPixelBufferLock_ReadOnly
/// `420v`: 8-bit 4:2:0 bi-planar, video range (what the console encodes).
/// `BGRA`: 8-bit, converted by the decoder hardware (what the Metal renderer reads).
pub const PIXEL_FORMAT_BGRA: u32 = 0x4247_5241;
pub const PIXEL_FORMAT_NV12_VIDEO: u32 = 0x3432_3076;
/// `420f`: full range variant.
pub const PIXEL_FORMAT_NV12_FULL: u32 = 0x3432_3066;
/// `x420`: 10-bit 4:2:0 bi-planar video range (HEVC Main10 / HDR).
pub const PIXEL_FORMAT_P010: u32 = 0x7834_3230;

/// What the decoder should deliver.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OutputFormat {
    /// The decoder's own format (NV12 video range): cheapest, for the system compositor.
    #[default]
    Native,
    /// 32-bit BGRA, converted in hardware and Metal-compatible: for the Metal renderer.
    Bgra,
}

#[derive(Debug)]
pub enum Error {
    /// Parameter sets missing or rejected by the system.
    ParameterSets(OSStatus),
    /// Decoder session could not be created (unsupported profile, no hardware, ...).
    Session(OSStatus),
    Decode(OSStatus),
    /// The access unit holds no picture data.
    Empty,
    /// Decoding cannot start before a keyframe.
    NeedKeyframe,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}

/// A decoded picture. Cheap to move; holds a retained `CVPixelBuffer`.
pub struct Frame {
    buffer: CVImageBufferRef,
    pub pts: i64,
}
// A retained CVPixelBuffer is thread-safe to hand across threads (CoreVideo buffers are reference counted).
unsafe impl Send for Frame {}

impl Frame {
    pub fn width(&self) -> usize {
        unsafe { CVPixelBufferGetWidth(self.buffer) }
    }
    pub fn height(&self) -> usize {
        unsafe { CVPixelBufferGetHeight(self.buffer) }
    }
    /// The id the source frame carried (the pipeline encodes it as `id * 1500` in the presentation time).
    pub fn source_id(&self) -> u16 {
        (self.pts / 1500) as u16
    }
    /// The backing `IOSurface` (for zero-copy Metal textures), valid while the frame lives. Null if there is none.
    pub fn io_surface(&self) -> *mut c_void {
        unsafe { CVPixelBufferGetIOSurface(self.buffer) }
    }
    pub fn pixel_format(&self) -> u32 {
        unsafe { CVPixelBufferGetPixelFormatType(self.buffer) }
    }
    /// Raw `CVPixelBufferRef` for the renderer. Valid while the frame lives.
    pub fn as_ptr(&self) -> *const c_void {
        self.buffer
    }
    /// Copy the planes out tightly packed (no row padding): Y plane then interleaved CbCr (NV12) for 8-bit.
    pub fn copy_planes(&self) -> Vec<u8> {
        let mut out = vec![];
        unsafe {
            if CVPixelBufferLockBaseAddress(self.buffer, READ_ONLY) != 0 {
                return out;
            }
            let bytes_per_sample = if self.pixel_format() == PIXEL_FORMAT_P010 { 2 } else { 1 };
            if CVPixelBufferGetPlaneCount(self.buffer) == 0 {
                // a non-planar format such as BGRA: one block of rows
                let base = CVPixelBufferGetBaseAddress(self.buffer);
                let stride = CVPixelBufferGetBytesPerRow(self.buffer);
                let row_bytes = (self.width() * 4).min(stride);
                if !base.is_null() {
                    for r in 0..self.height() {
                        out.extend_from_slice(std::slice::from_raw_parts(base.add(r * stride), row_bytes));
                    }
                }
                CVPixelBufferUnlockBaseAddress(self.buffer, READ_ONLY);
                return out;
            }
            for p in 0..CVPixelBufferGetPlaneCount(self.buffer) {
                let base = CVPixelBufferGetBaseAddressOfPlane(self.buffer, p);
                if base.is_null() {
                    continue;
                }
                let stride = CVPixelBufferGetBytesPerRowOfPlane(self.buffer, p);
                let rows = CVPixelBufferGetHeightOfPlane(self.buffer, p);
                // luma and interleaved chroma rows are both `width` samples; never read past the stride the system reports
                let row_bytes = (self.width() * bytes_per_sample).min(stride);
                for r in 0..rows {
                    out.extend_from_slice(std::slice::from_raw_parts(base.add(r * stride), row_bytes));
                }
            }
            CVPixelBufferUnlockBaseAddress(self.buffer, READ_ONLY);
        }
        out
    }
}
impl Drop for Frame {
    fn drop(&mut self) {
        unsafe { CFRelease(self.buffer) }
    }
}

type Queue = Arc<Mutex<Vec<Result<Frame, OSStatus>>>>;

extern "C" fn on_frame(
    refcon: *mut c_void,
    _frame_refcon: *mut c_void,
    status: OSStatus,
    _info: u32,
    image: CVImageBufferRef,
    pts: CMTime,
    _dur: CMTime,
) {
    // SAFETY: refcon is `Arc::as_ptr` of the queue owned by the decoder, which outlives the session.
    let queue = unsafe { &*(refcon as *const Mutex<Vec<Result<Frame, OSStatus>>>) };
    let item = if status == 0 && !image.is_null() {
        Ok(Frame { buffer: unsafe { CFRetain(image) }, pts: pts.value })
    } else {
        Err(status)
    };
    if let Ok(mut q) = queue.lock() {
        q.push(item);
    }
}

pub struct Decoder {
    codec: Codec,
    params: ParameterSets,
    format: CMVideoFormatDescriptionRef,
    session: VTDecompressionSessionRef,
    queue: Queue,
    output: OutputFormat,
    /// True once a keyframe has been decoded with the current session; P frames before that are refused.
    synced: bool,
}
unsafe impl Send for Decoder {}

/// Destination image attributes asking for BGRA, Metal-compatible buffers (null for the decoder's native format).
fn destination_attributes(output: OutputFormat) -> CFTypeRef {
    if output == OutputFormat::Native {
        return ptr::null();
    }
    unsafe {
        let fmt = PIXEL_FORMAT_BGRA as i32;
        let num = CFNumberCreate(ptr::null(), 3 /* kCFNumberSInt32Type */, &fmt as *const i32 as *const c_void);
        let keys = [kCVPixelBufferPixelFormatTypeKey, kCVPixelBufferMetalCompatibilityKey];
        let values = [num, kCFBooleanTrue];
        let d = CFDictionaryCreate(
            ptr::null(),
            keys.as_ptr(),
            values.as_ptr(),
            2,
            &kCFTypeDictionaryKeyCallBacks as *const u8 as *const c_void,
            &kCFTypeDictionaryValueCallBacks as *const u8 as *const c_void,
        );
        CFRelease(num); // the dictionary holds its own reference
        d
    }
}

fn format_description(codec: Codec, p: &ParameterSets) -> Result<CMVideoFormatDescriptionRef, Error> {
    if !p.complete(codec) {
        return Err(Error::ParameterSets(-1));
    }
    let sets = p.ordered(codec);
    let ptrs: Vec<*const u8> = sets.iter().map(|s| s.as_ptr()).collect();
    let sizes: Vec<usize> = sets.iter().map(|s| s.len()).collect();
    let mut out = ptr::null();
    let st = unsafe {
        match codec {
            Codec::H264 => CMVideoFormatDescriptionCreateFromH264ParameterSets(
                ptr::null(),
                ptrs.len(),
                ptrs.as_ptr(),
                sizes.as_ptr(),
                4,
                &mut out,
            ),
            Codec::Hevc => CMVideoFormatDescriptionCreateFromHEVCParameterSets(
                ptr::null(),
                ptrs.len(),
                ptrs.as_ptr(),
                sizes.as_ptr(),
                4,
                ptr::null(),
                &mut out,
            ),
        }
    };
    if st != 0 || out.is_null() {
        return Err(Error::ParameterSets(st));
    }
    Ok(out)
}

impl Decoder {
    pub fn new(codec: Codec, params: ParameterSets) -> Result<Self, Error> {
        Self::new_with(codec, params, OutputFormat::Native)
    }

    pub fn new_with(codec: Codec, params: ParameterSets, output: OutputFormat) -> Result<Self, Error> {
        let queue: Queue = Arc::new(Mutex::new(vec![]));
        let (format, session) = Self::make_session(codec, &params, &queue, output)?;
        Ok(Decoder { codec, params, format, session, queue, output, synced: false })
    }

    fn make_session(
        codec: Codec,
        params: &ParameterSets,
        queue: &Queue,
        output: OutputFormat,
    ) -> Result<(CMVideoFormatDescriptionRef, VTDecompressionSessionRef), Error> {
        let format = format_description(codec, params)?;
        let attrs = destination_attributes(output);
        let record =
            VTDecompressionOutputCallbackRecord { callback: on_frame, refcon: Arc::as_ptr(queue) as *mut c_void };
        let mut session = ptr::null();
        let st =
            unsafe { VTDecompressionSessionCreate(ptr::null(), format, ptr::null(), attrs, &record, &mut session) };
        if !attrs.is_null() {
            unsafe { CFRelease(attrs) };
        }
        if st != 0 || session.is_null() {
            unsafe { CFRelease(format) };
            return Err(Error::Session(st));
        }
        Ok((format, session))
    }

    pub fn codec(&self) -> Codec {
        self.codec
    }

    fn teardown(&mut self) {
        unsafe {
            VTDecompressionSessionWaitForAsynchronousFrames(self.session);
            VTDecompressionSessionInvalidate(self.session);
            CFRelease(self.session);
            CFRelease(self.format);
        }
    }

    /// Decode one Annex-B access unit. Parameter sets that differ from the current ones (resolution change,
    /// encoder restart) rebuild the session first. Returns the pictures produced (usually one).
    pub fn decode_annex_b(&mut self, annex_b: &[u8], pts: i64) -> Result<Vec<Frame>, Error> {
        let au = nal::analyse(self.codec, annex_b);
        if !au.in_band.sps.is_empty() && !au.in_band.pps.is_empty() && changed(&self.params, &au.in_band) {
            let mut merged = self.params.clone();
            merged.sps = au.in_band.sps.clone();
            merged.pps = au.in_band.pps.clone();
            if !au.in_band.vps.is_empty() {
                merged.vps = au.in_band.vps.clone();
            }
            let (format, session) = Self::make_session(self.codec, &merged, &self.queue, self.output)?;
            self.teardown();
            self.format = format;
            self.session = session;
            self.params = merged;
            self.synced = false;
        }
        if au.avcc.is_empty() {
            return Err(Error::Empty);
        }
        if !self.synced && !au.keyframe {
            return Err(Error::NeedKeyframe);
        }
        self.decode_avcc(&au.avcc, pts)?;
        self.synced = true;
        self.take()
    }

    fn decode_avcc(&mut self, avcc: &[u8], pts: i64) -> Result<(), Error> {
        unsafe {
            let mut block = ptr::null();
            // Let CoreMedia allocate and own the memory, then copy in: no lifetime coupling to `avcc`.
            let st = CMBlockBufferCreateWithMemoryBlock(
                ptr::null(),
                ptr::null_mut(),
                avcc.len(),
                ptr::null(),
                ptr::null(),
                0,
                avcc.len(),
                1, /* AssureMemoryNow */
                &mut block,
            );
            if st != 0 || block.is_null() {
                return Err(Error::Decode(st));
            }
            let st = CMBlockBufferReplaceDataBytes(avcc.as_ptr() as *const c_void, block, 0, avcc.len());
            if st != 0 {
                CFRelease(block);
                return Err(Error::Decode(st));
            }
            let timing =
                CMSampleTimingInfo { duration: cmtime_invalid(), presentation: cmtime(pts), decode: cmtime_invalid() };
            let size = avcc.len();
            let mut sample = ptr::null();
            let st = CMSampleBufferCreate(
                ptr::null(),
                block,
                1,
                ptr::null(),
                ptr::null(),
                self.format,
                1,
                1,
                &timing,
                1,
                &size,
                &mut sample,
            );
            CFRelease(block);
            if st != 0 || sample.is_null() {
                return Err(Error::Decode(st));
            }
            let st = VTDecompressionSessionDecodeFrame(self.session, sample, 0, ptr::null_mut(), ptr::null_mut());
            CFRelease(sample);
            if st != 0 {
                return Err(Error::Decode(st));
            }
            let st = VTDecompressionSessionWaitForAsynchronousFrames(self.session);
            if st != 0 {
                return Err(Error::Decode(st));
            }
        }
        Ok(())
    }

    fn take(&mut self) -> Result<Vec<Frame>, Error> {
        let items = std::mem::take(&mut *self.queue.lock().unwrap_or_else(|e| e.into_inner()));
        let mut frames = vec![];
        let mut first_err = None;
        for i in items {
            match i {
                Ok(f) => frames.push(f),
                Err(st) => {
                    self.synced = false; // a failed picture breaks the reference chain: wait for the next keyframe
                    first_err.get_or_insert(st);
                }
            }
        }
        // good frames already collected are not thrown away together with a bad one
        match first_err {
            Some(st) if frames.is_empty() => Err(Error::Decode(st)),
            _ => Ok(frames),
        }
    }

    /// Forget decoder state after packet loss so only a keyframe restarts output.
    pub fn reset(&mut self) {
        self.synced = false;
    }
}

fn changed(old: &ParameterSets, new: &ParameterSets) -> bool {
    old.sps != new.sps || old.pps != new.pps || (!new.vps.is_empty() && old.vps != new.vps)
}

impl Drop for Decoder {
    fn drop(&mut self) {
        self.teardown();
    }
}

extern "C" {
    fn VTIsHardwareDecodeSupported(codec_type: u32) -> u8;
}

/// Whether this Mac has a hardware decoder for the codec. HEVC is preferred when it does, H.264 otherwise.
pub fn hardware_decode_supported(codec: Codec) -> bool {
    let fourcc = match codec {
        Codec::H264 => u32::from_be_bytes(*b"avc1"),
        Codec::Hevc => u32::from_be_bytes(*b"hvc1"),
    };
    unsafe { VTIsHardwareDecodeSupported(fourcc) != 0 }
}

/// The codec to ask the console for: HEVC when hardware decode exists, else H.264.
pub fn preferred_codec() -> Codec {
    if hardware_decode_supported(Codec::Hevc) {
        Codec::Hevc
    } else {
        Codec::H264
    }
}
