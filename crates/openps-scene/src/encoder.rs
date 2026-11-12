//! The Core ML image encoders. They run through Vision, which takes the decoded picture as it is (NV12 or BGRA, any size) and scales it
//! to the model's input (`tools/scene-context/scenelab/pictures.py` has the same table).
//!
//! One frame gives two pictures, both the whole frame squashed to the model's input; their embeddings, joined in [`layout`] order, are
//! what the probe reads:
//! - `whole`: MobileCLIP-S1, 256 x 256
//! - `backbone`: DINOv2-B/14, 392 x 224
//!
//! The models ship as `.mlpackage`s; Core ML needs them compiled, which happens once and is kept in the cache folder.

use objc2::rc::Retained;
use objc2::AnyThread;
use objc2_core_ml::MLModel;
use objc2_core_video::CVPixelBuffer;
use objc2_foundation::{NSArray, NSDictionary, NSString, NSURL};
use objc2_vision::{
    VNCoreMLFeatureValueObservation, VNCoreMLModel, VNCoreMLRequest, VNImageCropAndScaleOption, VNImageRequestHandler,
    VNRequest,
};
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

const CLIP_DIM: usize = 512;
const BACKBONE_DIM: usize = 1536;

/// The pictures and the length of each embedding, in the order they are joined (the probe's `layout`).
pub fn layout() -> [(&'static str, usize); 2] {
    [("whole", CLIP_DIM), ("backbone", BACKBONE_DIM)]
}

/// The length of the joined embedding.
pub const DIM: usize = CLIP_DIM + BACKBONE_DIM;

pub struct Encoder {
    passes: Vec<(Retained<VNCoreMLRequest>, usize)>,
}

// Used from one thread at a time (the detector's); Vision objects are not touched from anywhere else.
unsafe impl Send for Encoder {}

fn url(path: &Path) -> Retained<NSURL> {
    NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()))
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)? {
        let e = e?;
        let target = to.join(e.file_name());
        if e.file_type()?.is_dir() {
            copy_dir(&e.path(), &target)?;
        } else {
            std::fs::copy(e.path(), target)?;
        }
    }
    Ok(())
}

/// A name that changes when the package does (its newest file time and size), so a new model is compiled again.
fn package_stamp(package: &Path) -> String {
    fn walk(p: &Path, newest: &mut u64, size: &mut u64) {
        let Ok(rd) = std::fs::read_dir(p) else { return };
        for e in rd.flatten() {
            let Ok(m) = e.metadata() else { continue };
            if m.is_dir() {
                walk(&e.path(), newest, size);
            } else {
                *size += m.len();
                let t = m.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs());
                *newest = (*newest).max(t);
            }
        }
    }
    let (mut newest, mut size) = (0, 0);
    walk(package, &mut newest, &mut size);
    format!("{newest:x}-{size:x}")
}

/// The note of a load that was given up on request (the app is closing).
pub const CANCELLED: &str = "cancelled";

/// The compiled model for `package`, compiling it into `cache` the first time (and whenever the package changes). `cancel` is asked before
/// the compile, which cannot be interrupted once it has begun.
fn compiled(package: &Path, cache: &Path, cancel: &dyn Fn() -> bool) -> Result<PathBuf, String> {
    if package.extension().is_some_and(|e| e == "mlmodelc") {
        return Ok(package.to_path_buf());
    }
    let stem = package.file_stem().map_or_else(|| "scene-model".into(), |s| s.to_string_lossy().into_owned());
    let name = format!("{stem}-{}.mlmodelc", package_stamp(package));
    let target = cache.join(&name);
    if target.is_dir() {
        return Ok(target);
    }
    if cancel() {
        return Err(CANCELLED.into());
    }
    std::fs::create_dir_all(cache).map_err(|e| format!("cannot create {}: {e}", cache.display()))?;
    #[allow(deprecated)] // the asynchronous variant needs a run loop; this runs once, on the detector's own thread
    let temp = unsafe { MLModel::compileModelAtURL_error(&url(package)) }
        .map_err(|e| format!("the scene model could not be compiled: {}", e.localizedDescription()))?;
    let temp = temp.path().map(|p| PathBuf::from(p.to_string())).ok_or("the compiled scene model has no path")?;
    // the compiler's folder may be on another volume than the cache: move when possible, copy otherwise. A copy is made under another
    // name and renamed when whole, so quitting in the middle of it never leaves a half model that the next start would trust.
    if std::fs::rename(&temp, &target).is_err() {
        let partial = cache.join(format!("{name}.partial"));
        let _ = std::fs::remove_dir_all(&partial);
        copy_dir(&temp, &partial)
            .and_then(|()| std::fs::rename(&partial, &target))
            .map_err(|e| format!("cannot store the compiled scene model: {e}"))?;
        let _ = std::fs::remove_dir_all(&temp);
    }
    // older compiled copies are of no use any more
    if let Ok(rd) = std::fs::read_dir(cache) {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            if n.starts_with(&format!("{stem}-")) && n.ends_with(".mlmodelc") && n != name {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }
    Ok(target)
}

fn vision_model(package: &Path, cache: &Path, cancel: &dyn Fn() -> bool) -> Result<Retained<VNCoreMLModel>, String> {
    let path = compiled(package, cache, cancel)?;
    let model = unsafe { MLModel::modelWithContentsOfURL_error(&url(&path)) }
        .map_err(|e| format!("the scene model could not be opened: {}", e.localizedDescription()))?;
    unsafe { VNCoreMLModel::modelForMLModel_error(&model) }
        .map_err(|e| format!("the scene model is not usable: {}", e.localizedDescription()))
}

impl Encoder {
    /// Load both models from `models` (compiling them into `cache` the first time).
    pub fn load(models: &Path, cache: &Path) -> Result<Encoder, String> {
        Self::load_until(models, cache, &|| false)
    }

    /// [`Encoder::load`], giving up with [`CANCELLED`] between its steps as soon as `cancel` says so.
    pub fn load_until(models: &Path, cache: &Path, cancel: &dyn Fn() -> bool) -> Result<Encoder, String> {
        let mut passes = vec![];
        for (package, dim) in [(crate::ENCODER_PACKAGE, CLIP_DIM), (crate::BACKBONE_PACKAGE, BACKBONE_DIM)] {
            if cancel() {
                return Err(CANCELLED.into());
            }
            let model = vision_model(&models.join(package), cache, cancel)?;
            let request = unsafe { VNCoreMLRequest::initWithModel(VNCoreMLRequest::alloc(), &model) };
            unsafe { request.setImageCropAndScaleOption(VNImageCropAndScaleOption::ScaleFill) };
            passes.push((request, dim));
        }
        Ok(Encoder { passes })
    }

    fn run(&self, handler: &VNImageRequestHandler) -> Result<Vec<f32>, String> {
        let requests: Vec<&VNRequest> = self.passes.iter().map(|(r, _)| -> &VNRequest { r }).collect();
        let list = NSArray::from_slice(&requests);
        handler
            .performRequests_error(&list)
            .map_err(|e| format!("scene model failed: {}", e.localizedDescription()))?;
        let mut out = Vec::with_capacity(DIM);
        for (request, dim) in &self.passes {
            let results = unsafe { request.results() }.ok_or("the scene model returned nothing")?;
            let first = results.firstObject().ok_or("the scene model returned nothing")?;
            let obs = first
                .downcast::<VNCoreMLFeatureValueObservation>()
                .map_err(|_| "the scene model returned something else than numbers")?;
            let array = unsafe { obs.featureValue().multiArrayValue() }.ok_or("the scene model returned no array")?;
            let n = unsafe { array.count() };
            if n as usize != *dim {
                return Err(format!("the scene model returned {n} numbers, expected {dim}"));
            }
            out.extend((0..n).map(|i| unsafe { array.objectAtIndexedSubscript(i).floatValue() }));
        }
        Ok(out)
    }

    /// The embedding of a decoded picture (`CVPixelBufferRef`, which must stay valid for the call).
    ///
    /// # Safety
    /// `pixel_buffer` must be a valid `CVPixelBufferRef`.
    pub unsafe fn embed_pixel_buffer(&self, pixel_buffer: *const c_void) -> Result<Vec<f32>, String> {
        if pixel_buffer.is_null() {
            return Err("no picture".into());
        }
        let buffer: &CVPixelBuffer = &*pixel_buffer.cast::<CVPixelBuffer>();
        let handler = VNImageRequestHandler::initWithCVPixelBuffer_options(
            VNImageRequestHandler::alloc(),
            buffer,
            &NSDictionary::new(),
        );
        self.run(&handler)
    }

    /// The embedding of an image file (for checks and tools).
    pub fn embed_file(&self, path: &Path) -> Result<Vec<f32>, String> {
        let handler = unsafe {
            VNImageRequestHandler::initWithURL_options(VNImageRequestHandler::alloc(), &url(path), &NSDictionary::new())
        };
        self.run(&handler)
    }
}
