//! The GPU side of the renderer: no window, no layer, only textures in and textures out, so it can be tested by reading
//! pixels back. Frame flow per real frame: luma -> motion search. Per displayed frame: (extrapolate) -> (upscale) -> present.

use crate::shaders::SOURCE;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::NSString;
use objc2_metal::*;
use objc2_metal_fx::*;
use std::ptr::NonNull;

pub type Tex = Retained<ProtocolObject<dyn MTLTexture>>;
pub type Device = Retained<ProtocolObject<dyn MTLDevice>>;
pub type Queue = Retained<ProtocolObject<dyn MTLCommandQueue>>;
pub type Cmd = Retained<ProtocolObject<dyn MTLCommandBuffer>>;

/// Block size of the motion search, in half-resolution pixels.
pub const BLOCK: usize = 8;

#[repr(C)]
struct MeParams {
    radius: i32,
    lambda: f32,
    flat: f32,
    conf_scale: f32,
    lambda_refine: f32,
    /// 1 when `mv_prev` holds the previous pair's motion field.
    temporal: f32,
    pad: [f32; 2],
}

#[repr(C)]
struct BgParams {
    reset: u32,
    pad: [u32; 3],
}

#[repr(C)]
struct Warp2Params {
    alpha: f32,
    tol: f32,
    max_err: f32,
    pad: f32,
    /// Extra offset added to every source position (camera compensation): out(p) = cur(p - alpha*v + extra).
    extra: [f32; 2],
    pad2: [f32; 2],
}

#[repr(C)]
struct InterpParams {
    beta: f32,
    tol: f32,
    max_err: f32,
    pad: f32,
}

#[repr(C)]
struct WarpParams {
    alpha: f32,
    mv_scale: f32,
    max_mv: f32,
    pad: f32,
}

/// Search radius in half-resolution pixels (16 full-resolution pixels per frame interval).
pub const SEARCH_RADIUS: i32 = 8;

/// How far the whole-picture search looks, in quarter-size luma pixels (matches `GLOBAL_R` in the shader): 96 pixels of the picture.
const GLOBAL_RADIUS: i32 = 12;

/// Cost of a longer vector in the block search, per half-resolution pixel of its length, summed over the 64 pixels of a block (0.003 of
/// the brightness range per pixel). A row of identical lines matches at every multiple of their spacing; this keeps the nearest one
/// unless another is clearly better.
const ME_LAMBDA: f32 = 0.1;

/// Cost of a fine block's vector differing from its coarse block's, per half-resolution pixel (the refinement ties otherwise go to
/// whichever line of a repeating pattern was tried first).
const REFINE_LAMBDA: f32 = 0.05;

/// Rounds of `propagate` (odd: the result ends in the scratch field, from which the last step writes the field the later stages read).
const PROPAGATE_PASSES: usize = 7;

/// Largest texture dimension Metal accepts on every Apple GPU; anything above aborts the process in the descriptor.
pub const MAX_TEXTURE_DIM: usize = 16384;

const MV_SNAPS: usize = 8;

#[derive(Debug)]
pub enum GpuError {
    NoDevice,
    Shader(String),
    Pipeline(String),
    Texture(&'static str),
    NoQueue,
}
impl std::fmt::Display for GpuError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for GpuError {}

pub fn make_tex(
    dev: &Device,
    fmt: MTLPixelFormat,
    w: usize,
    h: usize,
    usage: MTLTextureUsage,
    shared: bool,
) -> Result<Tex, GpuError> {
    if w == 0 || h == 0 {
        return Err(GpuError::Texture("zero texture size"));
    }
    if w > MAX_TEXTURE_DIM || h > MAX_TEXTURE_DIM {
        return Err(GpuError::Texture("texture dimension above the device limit"));
    }
    let d =
        unsafe { MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(fmt, w, h, false) };
    d.setUsage(usage);
    d.setStorageMode(if shared { MTLStorageMode::Shared } else { MTLStorageMode::Private });
    dev.newTextureWithDescriptor(&d).ok_or(GpuError::Texture("texture allocation failed"))
}

/// Apple's MetalFX frame interpolator, fed with our own motion field (a video stream has no engine motion vectors or depth).
struct Fx {
    key: (usize, usize, MTLPixelFormat),
    interpolator: Retained<ProtocolObject<dyn MTLFXFrameInterpolator>>,
    motion: Tex,
    depth: Tex,
    ui: Tex,
    out: Tex,
    /// Frames fed since the history was reset: the interpolator needs about three before its output is a real midpoint.
    frames: usize,
}

struct Scaler {
    key: (usize, usize, usize, usize),
    scaler: Retained<ProtocolObject<dyn MTLFXSpatialScaler>>,
    out: Tex,
    /// Linear-light colour mode: the picture goes in as half floats in `lin_in` and comes out of `out` as half floats.
    linear: bool,
    lin_in: Option<Tex>,
    /// The finished picture (blend and RCAS applied), when a finishing pass is needed.
    post: Tex,
}

/// Whether the GPU is too busy for the upscaler's finishing pass (linear light, RCAS): it uses more than 70 % of the time between two
/// displayed pictures. (At 4K the pass costs about 2 ms, which is a lot at 120 Hz and nothing at 60.)
pub fn finish_pass_too_slow(gpu_ms: f64, interval_ms: f64) -> bool {
    gpu_ms.is_finite() && interval_ms.is_finite() && interval_ms > 0.0 && gpu_ms > 0.7 * interval_ms
}

/// The RCAS lobe scale (2^-stops of FSR's own sharpness control) for a strength in 0..=2 (the slider's percent over 100): 1.0 is stops 2
/// (0.25), where RCAS after the 60 % sharpness blend measured closest to the real picture at every bitrate; 2.0 is stops 1, twice the
/// lobe; 0 is off. Anything that is not a number is off.
pub fn rcas_lobe(strength: f32) -> f32 {
    if strength.is_finite() {
        0.25 * strength.clamp(0.0, 2.0)
    } else {
        0.0
    }
}

/// Consecutive pictures the GPU must be too busy for (see [`finish_pass_too_slow`]) before the finishing pass is given up.
pub const FINISH_GUARD_PICTURES: u32 = 120;

pub struct Gpu {
    pub dev: Device,
    pub queue: Queue,
    luma: Retained<ProtocolObject<dyn MTLComputePipelineState>>,
    motion: Retained<ProtocolObject<dyn MTLComputePipelineState>>,
    warp: Retained<ProtocolObject<dyn MTLComputePipelineState>>,
    reset_stats: Retained<ProtocolObject<dyn MTLComputePipelineState>>,
    refine: Retained<ProtocolObject<dyn MTLComputePipelineState>>,
    down4: Retained<ProtocolObject<dyn MTLComputePipelineState>>,
    global_search: Retained<ProtocolObject<dyn MTLComputePipelineState>>,
    propagate: Retained<ProtocolObject<dyn MTLComputePipelineState>>,
    dominant_motion: Retained<ProtocolObject<dyn MTLComputePipelineState>>,
    adopt_dominant: Retained<ProtocolObject<dyn MTLComputePipelineState>>,
    /// The motion the controller says the camera has made between the last two frames (x, y in half-size pixels, trust 0..1; trust 0 = nothing
    /// learned): set by the renderer before each ingest and handed to the motion search as inline bytes.
    input_prior: [f32; 4],
    /// What the clear blocks agree on (x, y in half-size pixels, their share of all blocks, their number).
    dom_buf: Retained<ProtocolObject<dyn MTLBuffer>>,
    /// The motion of the picture as a whole (x, y in half-resolution pixels, z = 1 when it is a clear win over no motion): the prior of the
    /// block search, found by `global_search` on the quarter-size luma.
    gbuf: Retained<ProtocolObject<dyn MTLBuffer>>,
    bg_update: Retained<ProtocolObject<dyn MTLComputePipelineState>>,
    warp2: Retained<ProtocolObject<dyn MTLComputePipelineState>>,
    interp: Retained<ProtocolObject<dyn MTLComputePipelineState>>,
    motion_to_fx: Retained<ProtocolObject<dyn MTLComputePipelineState>>,
    stats: Retained<ProtocolObject<dyn MTLBuffer>>,
    frame_diff: Retained<ProtocolObject<dyn MTLComputePipelineState>>,
    /// One counter per recent ingest (see `Gpu::diff_slot`): how many sampled pixels changed against the previous frame.
    diff_bufs: Vec<Retained<ProtocolObject<dyn MTLBuffer>>>,
    diff_next: usize,
    last_diff: Option<usize>,
    to_linear: Retained<ProtocolObject<dyn MTLComputePipelineState>>,
    finish_up: Retained<ProtocolObject<dyn MTLComputePipelineState>>,
    fsr_easu: Retained<ProtocolObject<dyn MTLComputePipelineState>>,
    present: Retained<ProtocolObject<dyn MTLRenderPipelineState>>,
    ambient: Retained<ProtocolObject<dyn MTLRenderPipelineState>>,
    glow_down: Retained<ProtocolObject<dyn MTLRenderPipelineState>>,
    /// The picture shrunk to `GLOW_SIZE`: what the immersion glow is made of.
    glow: Tex,
    // per-stream state
    cur: Option<Tex>,
    prev: Option<Tex>,
    mv1: Option<Tex>,
    bg: Option<Tex>,
    luma_cur: Option<Tex>,
    luma_prev: Option<Tex>,
    coarse_cur: Option<Tex>,
    coarse_prev: Option<Tex>,
    mv: Option<Tex>,
    /// The motion field of the previous frame pair: where the lines were going a frame ago is where they go now (see `motion`).
    mv_prev: Option<Tex>,
    /// Scratch for `propagate` (it works from one field into another and back).
    mv_tmp: Option<Tex>,
    /// `mv_prev` is the field of the immediately preceding ingest.
    mv_fresh: bool,
    /// CPU-readable copies of `mv`, one per ingest (ring): a later in-flight command buffer cannot tear a read.
    mv_snaps: Vec<Tex>,
    snap_next: usize,
    last_snap: Option<usize>,
    warp_out: Option<Tex>,
    scaler: Option<Scaler>,
    fx: Option<Fx>,
    /// Frames ingested since the stream (or its size) last changed; motion needs two.
    ingested: usize,
    pub spatial_scaler_available: bool,
    /// MetalFX frame interpolation exists on this Mac (macOS 26+).
    pub fx_available: bool,
    /// Interpolate with MetalFX for the midpoint frame (set by the renderer: only when it is the right tool for the rate).
    pub fx_use: bool,
    /// How much of the MetalFX upscaler's edge contrast to keep: 1 = all of it, 0 = a plain smooth enlargement.
    pub sharpness: f32,
    /// Run the MetalFX scaler in linear light (half floats) instead of its perceptual mode: measured closer to the real picture at every bitrate.
    /// Falls back to the perceptual mode by itself if the Mac cannot create it.
    pub linear_upscale: bool,
    /// RCAS strength on the upscaled picture, 0 (off) ..= 2 (see `rcas_lobe`); independent of `sharpness`.
    pub rcas: f32,
    /// A scratch picture for the FSR upscaler's output (its size follows the target).
    fsr_out: Option<Tex>,
    /// The finished picture of the FSR path (blend and RCAS applied).
    fsr_post: Option<Tex>,
    /// Upscale with AMD's FSR 1 (EASU) instead of MetalFX's spatial scaler.
    pub fsr: bool,
    /// Whether the last upscaled picture was made by FSR (for the stats).
    fsr_used: bool,
    linear_ok: bool,
    /// Whether the last upscaled picture went through the finishing pass (for the stats).
    finish_used: bool,
    /// Colour saturation applied when the picture is presented (1.0 = as the stream is).
    /// Fill the bars around the picture with a soft glow of its colours instead of black.
    pub immersion: bool,
    pub saturation: f32,
    /// Unsharp-mask amount applied when the picture was not upscaled (0 = none, negative softens).
    pub sharpen: f32,
    /// Set by `upscale` for the frame being composed and used by `present`: the picture before the upscaler and how much of the upscaler's
    /// own contrast to keep (the sharpness setting); `None` = show the scaler's output as it is.
    blend: Option<(Tex, f32)>,
    /// Use the first-generation extrapolation (block motion, no hole handling). Kept so tests can compare the two.
    pub legacy_warp: bool,
}

const DIFF_SLOTS: usize = 16;
/// Size of the tiny image the immersion glow is made from.
const GLOW_SIZE: (usize, usize) = (96, 54);
/// How bright the glow is next to the picture, and how far it is blurred (a fraction of the window).
const AMBIENT_DIM: f32 = 0.85;
const AMBIENT_RADIUS: f32 = 0.05;

fn pso(
    dev: &Device,
    lib: &ProtocolObject<dyn MTLLibrary>,
    name: &str,
) -> Result<Retained<ProtocolObject<dyn MTLComputePipelineState>>, GpuError> {
    let f = lib
        .newFunctionWithName(&NSString::from_str(name))
        .ok_or_else(|| GpuError::Shader(format!("missing function {name}")))?;
    dev.newComputePipelineStateWithFunction_error(&f).map_err(|e| GpuError::Pipeline(format!("{name}: {e}")))
}

impl Gpu {
    pub fn new() -> Result<Self, GpuError> {
        let dev = MTLCreateSystemDefaultDevice().ok_or(GpuError::NoDevice)?;
        let queue = dev.newCommandQueue().ok_or(GpuError::NoQueue)?;
        let lib = dev
            .newLibraryWithSource_options_error(&NSString::from_str(SOURCE), None)
            .map_err(|e| GpuError::Shader(e.to_string()))?;
        let rd = MTLRenderPipelineDescriptor::new();
        let vs = lib
            .newFunctionWithName(&NSString::from_str("present_vs"))
            .ok_or_else(|| GpuError::Shader("present_vs".into()))?;
        let fs = lib
            .newFunctionWithName(&NSString::from_str("present_fs"))
            .ok_or_else(|| GpuError::Shader("present_fs".into()))?;
        rd.setVertexFunction(Some(&vs));
        rd.setFragmentFunction(Some(&fs));
        unsafe { rd.colorAttachments().objectAtIndexedSubscript(0).setPixelFormat(MTLPixelFormat::BGRA8Unorm) };
        let present = dev
            .newRenderPipelineStateWithDescriptor_error(&rd)
            .map_err(|e| GpuError::Pipeline(format!("present: {e}")))?;
        let render_pso = |frag: &str| -> Result<Retained<ProtocolObject<dyn MTLRenderPipelineState>>, GpuError> {
            let rd = MTLRenderPipelineDescriptor::new();
            let fs = lib.newFunctionWithName(&NSString::from_str(frag)).ok_or_else(|| GpuError::Shader(frag.into()))?;
            rd.setVertexFunction(Some(&vs));
            rd.setFragmentFunction(Some(&fs));
            unsafe { rd.colorAttachments().objectAtIndexedSubscript(0).setPixelFormat(MTLPixelFormat::BGRA8Unorm) };
            dev.newRenderPipelineStateWithDescriptor_error(&rd).map_err(|e| GpuError::Pipeline(format!("{frag}: {e}")))
        };
        let ambient = render_pso("ambient_fs")?;
        let glow_down = render_pso("glow_down_fs")?;
        let glow = make_tex(
            &dev,
            MTLPixelFormat::BGRA8Unorm,
            GLOW_SIZE.0,
            GLOW_SIZE.1,
            MTLTextureUsage::RenderTarget | MTLTextureUsage::ShaderRead,
            false,
        )?;
        let spatial_scaler_available = unsafe { MTLFXSpatialScalerDescriptor::supportsDevice(&dev) };
        let dev_for_probe = dev.clone();
        let gpu = Self {
            reset_stats: pso(&dev, &lib, "reset_stats")?,
            refine: pso(&dev, &lib, "refine")?,
            down4: pso(&dev, &lib, "down4")?,
            global_search: pso(&dev, &lib, "global_search")?,
            propagate: pso(&dev, &lib, "propagate")?,
            input_prior: [0.0; 4],
            dominant_motion: pso(&dev, &lib, "dominant_motion")?,
            adopt_dominant: pso(&dev, &lib, "adopt_dominant")?,
            dom_buf: dev
                .newBufferWithLength_options(16, MTLResourceOptions::StorageModeShared)
                .ok_or(GpuError::Texture("dominant motion buffer"))?,
            gbuf: dev
                .newBufferWithLength_options(16, MTLResourceOptions::StorageModeShared)
                .ok_or(GpuError::Texture("global motion buffer"))?,
            bg_update: pso(&dev, &lib, "bg_update")?,
            warp2: pso(&dev, &lib, "warp2")?,
            interp: pso(&dev, &lib, "interp")?,
            motion_to_fx: pso(&dev, &lib, "motion_to_fx")?,
            stats: dev
                .newBufferWithLength_options(16, MTLResourceOptions::StorageModeShared)
                .ok_or(GpuError::Texture("stats buffer"))?,
            frame_diff: pso(&dev, &lib, "frame_diff")?,
            diff_bufs: (0..DIFF_SLOTS)
                .map(|_| {
                    dev.newBufferWithLength_options(4, MTLResourceOptions::StorageModeShared)
                        .ok_or(GpuError::Texture("diff buffer"))
                })
                .collect::<Result<_, _>>()?,
            diff_next: 0,
            last_diff: None,
            luma: pso(&dev, &lib, "luma_half")?,
            motion: pso(&dev, &lib, "motion")?,
            warp: pso(&dev, &lib, "warp")?,
            to_linear: pso(&dev, &lib, "to_linear")?,
            finish_up: pso(&dev, &lib, "finish_up")?,
            fsr_easu: pso(&dev, &lib, "fsr_easu")?,
            present,
            ambient,
            glow_down,
            glow,
            dev,
            queue,
            cur: None,
            prev: None,
            mv1: None,
            bg: None,
            luma_cur: None,
            luma_prev: None,
            coarse_cur: None,
            coarse_prev: None,
            mv: None,
            mv_prev: None,
            mv_tmp: None,
            mv_fresh: false,
            mv_snaps: Vec::new(),
            snap_next: 0,
            last_snap: None,
            warp_out: None,
            scaler: None,
            fx: None,
            ingested: 0,
            spatial_scaler_available,
            fx_available: unsafe { MTLFXFrameInterpolatorDescriptor::supportsDevice(&dev_for_probe) },
            fx_use: false,
            sharpness: 1.0,
            linear_upscale: true,
            rcas: 1.0,
            fsr_out: None,
            fsr_post: None,
            fsr: false,
            fsr_used: false,
            linear_ok: true,
            finish_used: false,
            immersion: false,
            saturation: 1.0,
            sharpen: 0.0,
            blend: None,
            legacy_warp: false,
        };
        Ok(gpu)
    }

    pub fn command_buffer(&self) -> Option<Cmd> {
        self.queue.commandBuffer()
    }

    fn dispatch(
        enc: &ProtocolObject<dyn MTLComputeCommandEncoder>,
        p: &ProtocolObject<dyn MTLComputePipelineState>,
        w: usize,
        h: usize,
    ) {
        let tw = p.threadExecutionWidth().max(1);
        let th = (p.maxTotalThreadsPerThreadgroup() / tw).clamp(1, 8);
        enc.dispatchThreads_threadsPerThreadgroup(
            MTLSize { width: w, height: h, depth: 1 },
            MTLSize { width: tw, height: th, depth: 1 },
        );
    }

    /// Tell the motion search what the controller says the camera did between the previous frame and the next one ingested: `motion` in
    /// pixels of the picture (the stick's commanded image motion, see `CameraComp::predict`) and how far to trust it (0..1). `None` clears it.
    pub fn set_input_prior(&mut self, prior: Option<((f32, f32), f32)>) {
        self.input_prior = match prior {
            Some(((x, y), trust)) if x.is_finite() && y.is_finite() && trust > 0.0 => {
                // half-size pixels, and no further than the search could ever use
                [(x * 0.5).clamp(-48.0, 48.0), (y * 0.5).clamp(-48.0, 48.0), trust.clamp(0.0, 1.0), 0.0]
            }
            _ => [0.0; 4],
        };
    }

    /// A new real frame. Computes its luma and, when `motion` is wanted and a previous frame exists, the motion field.
    pub fn ingest(&mut self, cmd: &Cmd, frame: Tex, motion: bool) -> Result<(), GpuError> {
        let (w, h) = (frame.width(), frame.height());
        let changed = self.cur.as_ref().is_none_or(|c| (c.width(), c.height()) != (w, h));
        if changed {
            self.luma_cur = None;
            self.luma_prev = None;
            self.coarse_cur = None;
            self.coarse_prev = None;
            self.mv = None;
            self.mv_prev = None;
            self.mv_tmp = None;
            self.mv_fresh = false;
            self.mv_snaps.clear();
            self.last_snap = None;
            self.mv1 = None;
            self.bg = None;
            self.prev = None;
            self.warp_out = None;
            self.scaler = None;
            self.fx = None;
            self.ingested = 0;
        }
        let (lw, lh) = (w.div_ceil(2), h.div_ceil(2));
        if self.luma_cur.is_none() {
            let u = MTLTextureUsage::ShaderRead | MTLTextureUsage::ShaderWrite;
            self.luma_cur = Some(make_tex(&self.dev, MTLPixelFormat::R8Unorm, lw, lh, u, false)?);
            self.luma_prev = Some(make_tex(&self.dev, MTLPixelFormat::R8Unorm, lw, lh, u, false)?);
            let (cw, ch) = (lw.div_ceil(4), lh.div_ceil(4));
            self.coarse_cur = Some(make_tex(&self.dev, MTLPixelFormat::R8Unorm, cw, ch, u, false)?);
            self.coarse_prev = Some(make_tex(&self.dev, MTLPixelFormat::R8Unorm, cw, ch, u, false)?);
            self.mv = Some(make_tex(
                &self.dev,
                MTLPixelFormat::RGBA16Float,
                lw.div_ceil(BLOCK),
                lh.div_ceil(BLOCK),
                u,
                false,
            )?);
            self.mv_prev = Some(make_tex(
                &self.dev,
                MTLPixelFormat::RGBA16Float,
                lw.div_ceil(BLOCK),
                lh.div_ceil(BLOCK),
                u,
                false,
            )?);
            self.mv_tmp = Some(make_tex(
                &self.dev,
                MTLPixelFormat::RGBA16Float,
                lw.div_ceil(BLOCK),
                lh.div_ceil(BLOCK),
                u,
                false,
            )?);
            let u2 = MTLTextureUsage::ShaderRead;
            self.mv_snaps = (0..MV_SNAPS)
                .map(|_| {
                    // CPU-readable: the camera estimator reads the motion field
                    make_tex(&self.dev, MTLPixelFormat::RGBA16Float, lw.div_ceil(BLOCK), lh.div_ceil(BLOCK), u2, true)
                })
                .collect::<Result<_, _>>()?;
            self.mv1 = Some(make_tex(
                &self.dev,
                MTLPixelFormat::RGBA16Float,
                lw.div_ceil(BLOCK / 2),
                lh.div_ceil(BLOCK / 2),
                u,
                false,
            )?);
            self.bg = Some(make_tex(&self.dev, MTLPixelFormat::RGBA8Unorm, w, h, u, false)?);
        }
        std::mem::swap(&mut self.luma_cur, &mut self.luma_prev); // last frame's luma becomes "previous"
        std::mem::swap(&mut self.coarse_cur, &mut self.coarse_prev);
        let motion_will_run = motion && self.ingested >= 1;
        let temporal = motion_will_run && self.mv_fresh;
        if motion_will_run {
            std::mem::swap(&mut self.mv, &mut self.mv_prev); // last pair's field becomes the prediction
        }
        self.mv_fresh = motion_will_run;
        let (lc, lp, mv) =
            (self.luma_cur.as_ref().unwrap(), self.luma_prev.as_ref().unwrap(), self.mv.as_ref().unwrap());
        let (mv1, bg) = (self.mv1.as_ref().unwrap(), self.bg.as_ref().unwrap());
        let enc = cmd.computeCommandEncoder().ok_or(GpuError::Texture("compute encoder"))?;
        enc.setComputePipelineState(&self.luma);
        unsafe {
            enc.setTexture_atIndex(Some(&frame), 0);
            enc.setTexture_atIndex(Some(lc), 1);
        }
        Self::dispatch(&enc, &self.luma, lw, lh);
        let (cc, cp) = (self.coarse_cur.as_ref().unwrap(), self.coarse_prev.as_ref().unwrap());
        enc.setComputePipelineState(&self.down4);
        unsafe {
            enc.setTexture_atIndex(Some(lc), 0);
            enc.setTexture_atIndex(Some(cc), 1);
        }
        Self::dispatch(&enc, &self.down4, cc.width(), cc.height());
        enc.setComputePipelineState(&self.reset_stats);
        unsafe { enc.setBuffer_offset_atIndex(Some(&self.stats), 0, 0) };
        Self::dispatch(&enc, &self.reset_stats, 1, 1);
        self.last_diff = None;
        if self.ingested >= 1 {
            let k = self.diff_next;
            self.diff_next = (k + 1) % DIFF_SLOTS;
            // SAFETY: shared-storage buffer of 4 bytes; the slot's previous use finished long ago (16 ingests, at most 3 in flight)
            unsafe { *(self.diff_bufs[k].contents().as_ptr().cast::<u32>()) = 0 };
            enc.setComputePipelineState(&self.frame_diff);
            unsafe {
                enc.setTexture_atIndex(Some(lc), 0);
                enc.setTexture_atIndex(Some(lp), 1);
                enc.setBuffer_offset_atIndex(Some(&self.diff_bufs[k]), 0, 0);
            }
            Self::dispatch(&enc, &self.frame_diff, lw.div_ceil(2), lh.div_ceil(2));
            self.last_diff = Some(k);
        }
        let motion_run = motion && self.ingested >= 1;
        if motion_run {
            let p = MeParams {
                radius: SEARCH_RADIUS,
                lambda: ME_LAMBDA,
                flat: 0.5 / 255.0,
                conf_scale: 12.0,
                lambda_refine: REFINE_LAMBDA,
                temporal: if temporal { 1.0 } else { 0.0 },
                pad: [0.0; 2],
            };
            enc.setComputePipelineState(&self.global_search);
            unsafe {
                enc.setTexture_atIndex(Some(cc), 0);
                enc.setTexture_atIndex(Some(cp), 1);
                enc.setBuffer_offset_atIndex(Some(&self.gbuf), 0, 0);
            }
            let side = 2 * GLOBAL_RADIUS as usize + 1;
            enc.dispatchThreadgroups_threadsPerThreadgroup(
                MTLSize { width: 1, height: 1, depth: 1 },
                MTLSize { width: side, height: side, depth: 1 },
            );
            enc.setComputePipelineState(&self.motion);
            unsafe {
                enc.setTexture_atIndex(Some(lc), 0);
                enc.setTexture_atIndex(Some(lp), 1);
                enc.setTexture_atIndex(Some(mv), 2);
                enc.setBytes_length_atIndex(NonNull::from(&p).cast(), std::mem::size_of::<MeParams>(), 0);
                enc.setBuffer_offset_atIndex(Some(&self.stats), 0, 1);
                enc.setBuffer_offset_atIndex(Some(&self.gbuf), 0, 2);
                enc.setBytes_length_atIndex(
                    NonNull::from(&self.input_prior).cast(),
                    std::mem::size_of::<[f32; 4]>(),
                    3,
                );
                enc.setTexture_atIndex(Some(self.mv_prev.as_ref().unwrap()), 3);
            }
            Self::dispatch(&enc, &self.motion, lw.div_ceil(BLOCK), lh.div_ceil(BLOCK));
            // ambiguous blocks take the vector of the clear ones around them, one block further each pass
            let tmp = self.mv_tmp.as_ref().unwrap();
            enc.setComputePipelineState(&self.propagate);
            for pass in 0..PROPAGATE_PASSES {
                let (from, to) = if pass % 2 == 0 { (mv, tmp) } else { (tmp, mv) };
                unsafe {
                    enc.setTexture_atIndex(Some(lc), 0);
                    enc.setTexture_atIndex(Some(lp), 1);
                    enc.setTexture_atIndex(Some(from), 2);
                    enc.setTexture_atIndex(Some(to), 3);
                }
                Self::dispatch(&enc, &self.propagate, lw.div_ceil(BLOCK), lh.div_ceil(BLOCK));
            }
            // what is still ambiguous (an expanse with nothing to match) takes the motion most of the clear blocks have; the passes above
            // are an odd number, so their result is in `tmp` and this last step writes `mv`
            enc.setComputePipelineState(&self.dominant_motion);
            unsafe {
                enc.setTexture_atIndex(Some(tmp), 0);
                enc.setBuffer_offset_atIndex(Some(&self.dom_buf), 0, 0);
            }
            enc.dispatchThreadgroups_threadsPerThreadgroup(
                MTLSize { width: 1, height: 1, depth: 1 },
                MTLSize { width: 1024, height: 1, depth: 1 },
            );
            enc.setComputePipelineState(&self.adopt_dominant);
            unsafe {
                enc.setTexture_atIndex(Some(lc), 0);
                enc.setTexture_atIndex(Some(lp), 1);
                enc.setTexture_atIndex(Some(tmp), 2);
                enc.setTexture_atIndex(Some(mv), 3);
                enc.setBuffer_offset_atIndex(Some(&self.dom_buf), 0, 0);
                enc.setBytes_length_atIndex(
                    NonNull::from(&self.input_prior).cast(),
                    std::mem::size_of::<[f32; 4]>(),
                    1,
                );
            }
            Self::dispatch(&enc, &self.adopt_dominant, lw.div_ceil(BLOCK), lh.div_ceil(BLOCK));
            enc.setComputePipelineState(&self.refine);
            unsafe {
                enc.setTexture_atIndex(Some(lc), 0);
                enc.setTexture_atIndex(Some(lp), 1);
                enc.setTexture_atIndex(Some(mv), 2);
                enc.setTexture_atIndex(Some(mv1), 3);
                enc.setBytes_length_atIndex(NonNull::from(&p).cast(), std::mem::size_of::<MeParams>(), 0);
            }
            Self::dispatch(&enc, &self.refine, lw.div_ceil(BLOCK / 2), lh.div_ceil(BLOCK / 2));
            let bp = BgParams { reset: u32::from(self.ingested == 0), pad: [0; 3] };
            enc.setComputePipelineState(&self.bg_update);
            unsafe {
                enc.setTexture_atIndex(Some(&frame), 0);
                enc.setTexture_atIndex(Some(mv1), 1);
                enc.setTexture_atIndex(Some(bg), 2);
                enc.setBytes_length_atIndex(NonNull::from(&bp).cast(), std::mem::size_of::<BgParams>(), 0);
                enc.setBuffer_offset_atIndex(Some(&self.stats), 0, 1);
            }
            Self::dispatch(&enc, &self.bg_update, w, h);
        } else if self.ingested == 0 {
            // first frame: seed the background memory
            let bp = BgParams { reset: 1, pad: [0; 3] };
            enc.setComputePipelineState(&self.bg_update);
            unsafe {
                enc.setTexture_atIndex(Some(&frame), 0);
                enc.setTexture_atIndex(Some(mv1), 1);
                enc.setTexture_atIndex(Some(bg), 2);
                enc.setBytes_length_atIndex(NonNull::from(&bp).cast(), std::mem::size_of::<BgParams>(), 0);
                enc.setBuffer_offset_atIndex(Some(&self.stats), 0, 1);
            }
            Self::dispatch(&enc, &self.bg_update, w, h);
        }
        enc.endEncoding();
        self.input_prior = [0.0; 4]; // for the one ingest it was set for (the bytes were copied when encoded)
        if motion_run {
            let k = self.snap_next % self.mv_snaps.len().max(1);
            if let (Some(snap), Some(blit)) = (self.mv_snaps.get(k), cmd.blitCommandEncoder()) {
                let sz = MTLSize { width: snap.width(), height: snap.height(), depth: 1 };
                let o = MTLOrigin { x: 0, y: 0, z: 0 };
                unsafe {
                    blit.copyFromTexture_sourceSlice_sourceLevel_sourceOrigin_sourceSize_toTexture_destinationSlice_destinationLevel_destinationOrigin(mv, 0, 0, o, sz, snap, 0, 0, o);
                }
                blit.endEncoding();
                self.last_snap = Some(k);
                self.snap_next = k + 1;
            }
        }
        self.prev = self.cur.take();
        self.cur = Some(frame);
        self.ingested += 1;
        if self.fx_use && self.fx_available && motion && self.ingested >= 2 {
            if let Err(e) = self.fx_interpolate(cmd) {
                self.fx = None; // fall back to our own interpolation rather than fail the frame
                self.fx_use = false;
                eprintln!("MetalFX frame interpolation unavailable: {e}");
            }
        }
        Ok(())
    }

    /// Dominant image motion between the last two real frames, in full-resolution pixels per source frame, and the share of the
    /// image it explains (0...1). Read from the coarse motion field; call after the command buffer that ingested the frame
    /// has completed. `None` until two frames exist.
    pub fn global_motion(&self) -> Option<GlobalMotion> {
        self.global_motion_at(self.last_snap?)
    }

    /// Slot of the change counter of the most recent ingest (`None` for the first frame), and how to read it once that ingest has finished:
    /// the share of sampled pixels that changed, 0..1.
    pub fn diff_slot(&self) -> Option<(usize, usize)> {
        let (w, h) = self.luma_cur.as_ref().map(|t| (t.width().div_ceil(2), t.height().div_ceil(2)))?;
        self.last_diff.map(|k| (k, (w * h).max(1)))
    }

    pub fn read_diff(&self, slot: usize, sampled: usize) -> f32 {
        // SAFETY: see `ingest`; the caller read this only after the command buffer completed
        let n = unsafe { *(self.diff_bufs[slot].contents().as_ptr().cast::<u32>()) };
        n as f32 / sampled as f32
    }

    /// The coarse motion field of the latest ingest (8x8 blocks of the half-size picture, so 16 pixels): per block the motion in pixels of the
    /// picture, the confidence and the share of certainty (1 = clear, 0 = a look-alike matches as well), and the width of the field. For
    /// tests and tools.
    #[doc(hidden)]
    pub fn coarse_motion_field(&self) -> Option<(Vec<[f32; 4]>, usize)> {
        let mv = self.mv_snaps.get(self.last_snap?)?;
        let (w, h) = (mv.width(), mv.height());
        let mut raw = vec![0u16; w * h * 4];
        unsafe {
            mv.getBytes_bytesPerRow_fromRegion_mipmapLevel(
                NonNull::new(raw.as_mut_ptr().cast())?,
                w * 8,
                MTLRegion { origin: MTLOrigin { x: 0, y: 0, z: 0 }, size: MTLSize { width: w, height: h, depth: 1 } },
                0,
            );
        }
        Some((raw.as_chunks::<4>().0.iter().map(|p| p.map(half_to_f32)).collect(), w))
    }

    /// Ring slot holding the motion field of the most recent ingest (see `global_motion_at`).
    pub fn motion_snapshot(&self) -> Option<usize> {
        self.last_snap
    }

    /// Like `global_motion` but for a specific ingest's snapshot, immune to later ingests still running on the GPU.
    pub fn global_motion_at(&self, slot: usize) -> Option<GlobalMotion> {
        if self.ingested < 2 {
            return None;
        }
        let mv = self.mv_snaps.get(slot)?;
        let (w, h) = (mv.width(), mv.height());
        let mut raw = vec![0u16; w * h * 4];
        unsafe {
            mv.getBytes_bytesPerRow_fromRegion_mipmapLevel(
                NonNull::new(raw.as_mut_ptr().cast())?,
                w * 8,
                MTLRegion { origin: MTLOrigin { x: 0, y: 0, z: 0 }, size: MTLSize { width: w, height: h, depth: 1 } },
                0,
            );
        }
        let blocks: Vec<(f32, f32, f32)> =
            raw.as_chunks::<4>().0.iter().map(|p| (half_to_f32(p[0]), half_to_f32(p[1]), half_to_f32(p[2]))).collect();
        Some(dominant_motion(&blocks))
    }

    /// Size of the newest source frame.
    pub fn source_size(&self) -> Option<(usize, usize)> {
        self.cur.as_ref().map(|c| (c.width(), c.height()))
    }

    /// Forget everything about the previous stream (new session).
    pub fn reset_stream(&mut self) {
        self.cur = None;
        self.prev = None;
        self.luma_cur = None;
        self.luma_prev = None;
        self.coarse_cur = None;
        self.coarse_prev = None;
        self.mv = None;
        self.mv_prev = None;
        self.mv_tmp = None;
        self.mv_fresh = false;
        self.mv1 = None;
        self.bg = None;
        self.warp_out = None;
        self.scaler = None;
        self.fx = None;
        self.ingested = 0;
        self.fsr_out = None;
        self.fsr_post = None;
    }

    /// Whether `compose` can extrapolate yet (it needs the motion between two real frames).
    pub fn motion_ready(&self) -> bool {
        self.ingested >= 2
    }

    /// The picture to show: the newest real frame advanced by `alpha` frame intervals (0 = the frame itself), upscaled to
    /// `out` if that is larger than the source and MetalFX is available. Returns the texture and whether it was upscaled.
    pub fn compose(
        &mut self,
        cmd: &Cmd,
        alpha: f32,
        out: Option<(usize, usize)>,
        extra: (f32, f32),
    ) -> Result<(Tex, bool), GpuError> {
        let cur = self.cur.clone().ok_or(GpuError::Texture("no frame yet"))?;
        let (w, h) = (cur.width(), cur.height());
        let mut shown = cur.clone();
        let moved = extra.0.abs() > 0.02 || extra.1.abs() > 0.02;
        if (alpha > 0.002 || moved) && self.motion_ready() {
            if self.warp_out.is_none() {
                let u = MTLTextureUsage::ShaderRead | MTLTextureUsage::ShaderWrite;
                self.warp_out = Some(make_tex(&self.dev, MTLPixelFormat::BGRA8Unorm, w, h, u, false)?);
            }
            let wo = self.warp_out.clone().unwrap();
            let enc = cmd.computeCommandEncoder().ok_or(GpuError::Texture("compute encoder"))?;
            if let (false, Some(prev)) = (self.legacy_warp, self.prev.clone()) {
                let p = Warp2Params {
                    alpha: alpha.clamp(0.0, 1.5),
                    tol: 2.5,
                    max_err: 0.22,
                    pad: 0.0,
                    extra: [extra.0, extra.1],
                    pad2: [0.0; 2],
                };
                enc.setComputePipelineState(&self.warp2);
                unsafe {
                    enc.setTexture_atIndex(Some(&cur), 0);
                    enc.setTexture_atIndex(Some(&prev), 1);
                    enc.setTexture_atIndex(Some(self.mv1.as_ref().unwrap()), 2);
                    enc.setTexture_atIndex(Some(self.bg.as_ref().unwrap()), 3);
                    enc.setTexture_atIndex(Some(&wo), 4);
                    enc.setBytes_length_atIndex(NonNull::from(&p).cast(), std::mem::size_of::<Warp2Params>(), 0);
                    enc.setBuffer_offset_atIndex(Some(&self.stats), 0, 1);
                }
                Self::dispatch(&enc, &self.warp2, w, h);
            } else {
                let p = WarpParams { alpha: alpha.clamp(0.0, 1.5), mv_scale: 2.0, max_mv: 40.0, pad: 0.0 };
                enc.setComputePipelineState(&self.warp);
                unsafe {
                    enc.setTexture_atIndex(Some(&cur), 0);
                    enc.setTexture_atIndex(Some(self.mv.as_ref().unwrap()), 1);
                    enc.setTexture_atIndex(Some(&wo), 2);
                    enc.setBytes_length_atIndex(NonNull::from(&p).cast(), std::mem::size_of::<WarpParams>(), 0);
                }
                Self::dispatch(&enc, &self.warp, w, h);
            }
            enc.endEncoding();
            shown = wo;
        }
        self.upscale(cmd, shown, out)
    }

    /// Upscale `shown` to `out` with MetalFX when that is larger than the picture. The scaler runs in linear light and a finishing pass
    /// (back to sRGB, the sharpness blend, RCAS) makes the picture; when neither is wanted the blend is left to the present pass.
    pub fn upscale(&mut self, cmd: &Cmd, shown: Tex, out: Option<(usize, usize)>) -> Result<(Tex, bool), GpuError> {
        self.blend = None;
        self.finish_used = false;
        self.fsr_used = false;
        let (w, h) = (shown.width(), shown.height());
        if let Some((ow, oh)) = out {
            if self.fsr && ow > w && oh > h {
                return self.upscale_fsr(cmd, shown, (ow, oh));
            }
            if self.spatial_scaler_available && ow > w && oh > h {
                let key = (w, h, ow, oh);
                let want_linear = self.linear_upscale && self.linear_ok;
                if self.scaler.as_ref().is_none_or(|s| s.key != key || s.linear != want_linear) {
                    let s = self.make_scaler(key, want_linear)?;
                    self.linear_ok = self.linear_ok && (s.linear || !want_linear);
                    self.scaler = Some(s);
                }
                let s = self.scaler.as_ref().unwrap();
                let k = self.sharpness.clamp(0.0, 2.0);
                let rcas = rcas_lobe(self.rcas);
                if let Some(lin) = &s.lin_in {
                    let enc = cmd.computeCommandEncoder().ok_or(GpuError::Texture("compute encoder"))?;
                    enc.setComputePipelineState(&self.to_linear);
                    unsafe {
                        enc.setTexture_atIndex(Some(&shown), 0);
                        enc.setTexture_atIndex(Some(lin), 1);
                    }
                    Self::dispatch(&enc, &self.to_linear, w, h);
                    enc.endEncoding();
                }
                unsafe {
                    s.scaler.setColorTexture(Some(s.lin_in.as_ref().unwrap_or(&shown)));
                    s.scaler.setOutputTexture(Some(&s.out));
                    s.scaler.encodeToCommandBuffer(cmd);
                }
                if s.linear || rcas > 0.0 {
                    self.encode_finish(cmd, &s.out, &shown, &s.post, k, rcas, s.linear)?;
                    self.finish_used = true;
                    return Ok((s.post.clone(), true));
                }
                if (k - 1.0).abs() > 0.005 {
                    self.blend = Some((shown.clone(), k)); // blended in the present pass
                }
                return Ok((s.out.clone(), true));
            }
        }
        Ok((shown, false))
    }

    /// Upscale with AMD's FSR 1: EASU into a half-float picture, then the same finishing pass as the MetalFX picture gets (the blend with
    /// the plain enlargement, then RCAS). Needs only the Metal compute pipeline, so it is there on every Mac that has the GPU renderer.
    fn upscale_fsr(&mut self, cmd: &Cmd, shown: Tex, (ow, oh): (usize, usize)) -> Result<(Tex, bool), GpuError> {
        let (w, h) = (shown.width(), shown.height());
        let u = MTLTextureUsage::ShaderRead | MTLTextureUsage::ShaderWrite;
        if self.fsr_out.as_ref().is_none_or(|t| (t.width(), t.height()) != (ow, oh)) {
            self.fsr_out = Some(make_tex(&self.dev, MTLPixelFormat::RGBA16Float, ow, oh, u, false)?);
            self.fsr_post = Some(make_tex(&self.dev, MTLPixelFormat::BGRA8Unorm, ow, oh, u, false)?);
        }
        let (up, post) = (self.fsr_out.clone().unwrap(), self.fsr_post.clone().unwrap());
        let enc = cmd.computeCommandEncoder().ok_or(GpuError::Texture("compute encoder"))?;
        enc.setComputePipelineState(&self.fsr_easu);
        let params = [w as f32 / ow as f32, h as f32 / oh as f32, 0.0f32, 0.0];
        unsafe {
            enc.setTexture_atIndex(Some(&shown), 0);
            enc.setTexture_atIndex(Some(&up), 1);
            enc.setBytes_length_atIndex(NonNull::from(&params).cast(), std::mem::size_of::<[f32; 4]>(), 0);
        }
        Self::dispatch(&enc, &self.fsr_easu, ow, oh);
        enc.endEncoding();
        self.fsr_used = true;
        let k = self.sharpness.clamp(0.0, 2.0);
        let rcas = rcas_lobe(self.rcas);
        if rcas > 0.0 {
            self.encode_finish(cmd, &up, &shown, &post, k, rcas, false)?;
            self.finish_used = true;
            return Ok((post, true));
        }
        if (k - 1.0).abs() > 0.005 {
            self.blend = Some((shown, k)); // blended in the present pass
        }
        Ok((up, true))
    }

    /// Whether the last upscaled picture was made by FSR.
    pub fn fsr_active(&self) -> bool {
        self.fsr_used
    }

    /// Smooth mode: the picture at fraction `beta` (0 = previous real frame, 1 = newest) between the last two real frames.
    /// Needs motion; before that (or when `beta` is at an end) it returns the nearer real frame. Costs one frame of latency
    /// by construction: it can only show what lies between two frames that already exist.
    pub fn compose_interp(
        &mut self,
        cmd: &Cmd,
        beta: f32,
        out: Option<(usize, usize)>,
    ) -> Result<(Tex, bool), GpuError> {
        let cur = self.cur.clone().ok_or(GpuError::Texture("no frame yet"))?;
        let beta = beta.clamp(0.0, 1.0);
        let mut shown = cur.clone();
        if let (Some(prev), true) = (self.prev.clone(), self.motion_ready()) {
            let mid = if self.fx_use && (beta - 0.5).abs() <= 0.17 { self.fx_midpoint() } else { None };
            if beta >= 0.999 {
                shown = cur.clone();
            } else if beta <= 0.001 {
                shown = prev;
            } else if let Some(m) = mid {
                shown = m; // the picture halfway between the last two real frames, from MetalFX
            } else {
                let (w, h) = (cur.width(), cur.height());
                if self.warp_out.is_none() {
                    let u = MTLTextureUsage::ShaderRead | MTLTextureUsage::ShaderWrite;
                    self.warp_out = Some(make_tex(&self.dev, MTLPixelFormat::BGRA8Unorm, w, h, u, false)?);
                }
                let wo = self.warp_out.clone().unwrap();
                let p = InterpParams { beta, tol: 2.5, max_err: 0.22, pad: 0.0 };
                let enc = cmd.computeCommandEncoder().ok_or(GpuError::Texture("compute encoder"))?;
                enc.setComputePipelineState(&self.interp);
                unsafe {
                    enc.setTexture_atIndex(Some(&cur), 0);
                    enc.setTexture_atIndex(Some(&prev), 1);
                    enc.setTexture_atIndex(Some(self.mv1.as_ref().unwrap()), 2);
                    enc.setTexture_atIndex(Some(&wo), 3);
                    enc.setBytes_length_atIndex(NonNull::from(&p).cast(), std::mem::size_of::<InterpParams>(), 0);
                    enc.setBuffer_offset_atIndex(Some(&self.stats), 0, 1);
                }
                Self::dispatch(&enc, &self.interp, w, h);
                enc.endEncoding();
                shown = wo;
            }
        }
        self.upscale(cmd, shown, out)
    }

    /// Whether the last upscaled picture went through the finishing pass (linear light, the blend, RCAS).
    pub fn finish_pass_active(&self) -> bool {
        self.finish_used
    }

    /// The MetalFX scaler in use runs in linear light (false: its perceptual mode, or no scaler yet).
    pub fn linear_active(&self) -> bool {
        self.scaler.as_ref().is_some_and(|s| s.linear)
    }

    /// Linear light was asked for but this Mac could not create that scaler, so the perceptual mode is used.
    pub fn linear_unavailable(&self) -> bool {
        !self.linear_ok
    }

    /// Feed the interpolator the last two real frames and our motion field; its output is the picture halfway between them.
    fn fx_interpolate(&mut self, cmd: &Cmd) -> Result<(), GpuError> {
        let (Some(cur), Some(prev), Some(mv1)) = (self.cur.clone(), self.prev.clone(), self.mv1.clone()) else {
            return Ok(());
        };
        let (w, h, fmt) = (cur.width(), cur.height(), cur.pixelFormat());
        if self.fx.as_ref().is_none_or(|f| f.key != (w, h, fmt)) {
            let d = unsafe { MTLFXFrameInterpolatorDescriptor::new() };
            unsafe {
                d.setColorTextureFormat(fmt);
                d.setOutputTextureFormat(MTLPixelFormat::BGRA8Unorm);
                d.setDepthTextureFormat(MTLPixelFormat::R32Float);
                d.setMotionTextureFormat(MTLPixelFormat::RG16Float);
                d.setUITextureFormat(MTLPixelFormat::BGRA8Unorm);
                d.setInputWidth(w);
                d.setInputHeight(h);
                d.setOutputWidth(w);
                d.setOutputHeight(h);
            }
            let interpolator = unsafe { d.newFrameInterpolatorWithDevice(&self.dev) }
                .ok_or(GpuError::Pipeline("MetalFX frame interpolator".into()))?;
            let u = MTLTextureUsage::ShaderRead | MTLTextureUsage::ShaderWrite | MTLTextureUsage::RenderTarget;
            let motion = make_tex(&self.dev, MTLPixelFormat::RG16Float, w, h, u, false)?;
            // no scene depth in a video: a flat plane. Not used for the motion; only the interpolator's occlusion logic reads it.
            let depth = make_tex(&self.dev, MTLPixelFormat::R32Float, w, h, MTLTextureUsage::ShaderRead, true)?;
            let flat = vec![0.5f32; w * h];
            unsafe {
                depth.replaceRegion_mipmapLevel_withBytes_bytesPerRow(
                    MTLRegion {
                        origin: MTLOrigin { x: 0, y: 0, z: 0 },
                        size: MTLSize { width: w, height: h, depth: 1 },
                    },
                    0,
                    NonNull::new(flat.as_ptr() as *mut _).unwrap(),
                    w * 4,
                );
            }
            let ui = make_tex(&self.dev, MTLPixelFormat::BGRA8Unorm, w, h, MTLTextureUsage::ShaderRead, true)?; // transparent: no overlay
            let out = make_tex(&self.dev, MTLPixelFormat::BGRA8Unorm, w, h, u, false)?;
            self.fx = Some(Fx { key: (w, h, fmt), interpolator, motion, depth, ui, out, frames: 0 });
        }
        let fx = self.fx.as_mut().unwrap();
        let enc = cmd.computeCommandEncoder().ok_or(GpuError::Texture("compute encoder"))?;
        enc.setComputePipelineState(&self.motion_to_fx);
        unsafe {
            enc.setTexture_atIndex(Some(&mv1), 0);
            enc.setTexture_atIndex(Some(&fx.motion), 1);
        }
        Self::dispatch(&enc, &self.motion_to_fx, w, h);
        enc.endEncoding();
        let i = &fx.interpolator;
        unsafe {
            i.setColorTexture(Some(&cur));
            i.setPrevColorTexture(Some(&prev));
            i.setMotionTexture(Some(&fx.motion));
            i.setDepthTexture(Some(&fx.depth));
            i.setUITexture(Some(&fx.ui));
            i.setOutputTexture(Some(&fx.out));
            i.setMotionVectorScaleX(1.0);
            i.setMotionVectorScaleY(1.0);
            i.setDeltaTime(1.0 / 60.0);
            i.setNearPlane(0.1);
            i.setFarPlane(100.0);
            i.setFieldOfView(60.0_f32.to_radians());
            i.setAspectRatio(w as f32 / h as f32);
            i.setIsUITextureComposited(false);
            i.setShouldResetHistory(fx.frames == 0);
            i.encodeToCommandBuffer(cmd);
        }
        fx.frames += 1;
        Ok(())
    }

    /// The MetalFX midpoint, once its history is warm.
    fn fx_midpoint(&self) -> Option<Tex> {
        self.fx.as_ref().filter(|f| f.frames >= 4).map(|f| f.out.clone())
    }

    /// Whether the last midpoint came from MetalFX (for the stats).
    pub fn fx_active(&self) -> bool {
        self.fx_use && self.fx.as_ref().is_some_and(|f| f.frames >= 4)
    }

    /// The finishing pass: `up` (the upscaler's picture, in linear light when `linear`) blended with the plain enlargement of `plain` by `k`,
    /// then RCAS with lobe scale `rcas`, into `post`. The kernel works in 16x16 tiles (see its comment), so the grid is a whole number of them.
    #[allow(clippy::too_many_arguments)]
    fn encode_finish(
        &self,
        cmd: &Cmd,
        up: &Tex,
        plain: &Tex,
        post: &Tex,
        k: f32,
        rcas: f32,
        linear: bool,
    ) -> Result<(), GpuError> {
        let enc = cmd.computeCommandEncoder().ok_or(GpuError::Texture("compute encoder"))?;
        enc.setComputePipelineState(&self.finish_up);
        let params = [k, rcas, if linear { 1.0f32 } else { 0.0 }, 0.0];
        unsafe {
            enc.setTexture_atIndex(Some(up), 0);
            enc.setTexture_atIndex(Some(plain), 1);
            enc.setTexture_atIndex(Some(post), 2);
            enc.setBytes_length_atIndex(NonNull::from(&params).cast(), std::mem::size_of::<[f32; 4]>(), 0);
        }
        // whole threadgroups, so a group at the edge is not partial: every thread of a group loads its part of the shared tile
        enc.dispatchThreadgroups_threadsPerThreadgroup(
            MTLSize { width: post.width().div_ceil(16), height: post.height().div_ceil(16), depth: 1 },
            MTLSize { width: 16, height: 16, depth: 1 },
        );
        enc.endEncoding();
        Ok(())
    }

    fn make_scaler(&self, key: (usize, usize, usize, usize), linear: bool) -> Result<Scaler, GpuError> {
        let build = |linear: bool| {
            let fmt = if linear { MTLPixelFormat::RGBA16Float } else { MTLPixelFormat::BGRA8Unorm };
            let d = unsafe { MTLFXSpatialScalerDescriptor::new() };
            unsafe {
                d.setInputWidth(key.0);
                d.setInputHeight(key.1);
                d.setOutputWidth(key.2);
                d.setOutputHeight(key.3);
                d.setColorTextureFormat(fmt);
                d.setOutputTextureFormat(fmt);
                d.setColorProcessingMode(if linear {
                    MTLFXSpatialScalerColorProcessingMode::Linear
                } else {
                    MTLFXSpatialScalerColorProcessingMode::Perceptual
                });
            }
            unsafe { d.newSpatialScalerWithDevice(&self.dev) }.map(|s| (s, fmt))
        };
        let (scaler, fmt, linear) = match (linear, build(linear)) {
            (_, Some((s, f))) => (s, f, linear),
            (true, None) => {
                let (s, f) = build(false).ok_or(GpuError::Pipeline("MetalFX scaler".into()))?;
                (s, f, false)
            }
            (false, None) => return Err(GpuError::Pipeline("MetalFX scaler".into())),
        };
        let out =
            make_tex(&self.dev, fmt, key.2, key.3, MTLTextureUsage::RenderTarget | MTLTextureUsage::ShaderRead, false)?;
        let lin_in = if linear {
            Some(make_tex(
                &self.dev,
                MTLPixelFormat::RGBA16Float,
                key.0,
                key.1,
                MTLTextureUsage::ShaderRead | MTLTextureUsage::ShaderWrite,
                false,
            )?)
        } else {
            None
        };
        let post = make_tex(
            &self.dev,
            MTLPixelFormat::BGRA8Unorm,
            key.2,
            key.3,
            MTLTextureUsage::ShaderRead | MTLTextureUsage::ShaderWrite,
            false,
        )?;
        Ok(Scaler { key, scaler, out, linear, lin_in, post })
    }

    /// Draw `tex` letterboxed (aspect preserved) into `target`.
    pub fn present(
        &self,
        cmd: &Cmd,
        tex: &ProtocolObject<dyn MTLTexture>,
        target: &ProtocolObject<dyn MTLTexture>,
    ) -> Result<(), GpuError> {
        let pass = MTLRenderPassDescriptor::new();
        unsafe {
            let a = pass.colorAttachments().objectAtIndexedSubscript(0);
            a.setTexture(Some(target));
            a.setLoadAction(MTLLoadAction::Clear);
            a.setStoreAction(MTLStoreAction::Store);
            a.setClearColor(MTLClearColor { red: 0.0, green: 0.0, blue: 0.0, alpha: 1.0 });
        }
        let (tw, th) = (target.width() as f64, target.height() as f64);
        let (r, v) = fit(tex.width() as f64, tex.height() as f64, tw, th);
        // a picture that fills the drawable needs no clear (and no write of the black that would be overwritten)
        if r.0.abs() < 0.5 && r.1.abs() < 0.5 && (r.2 - tw).abs() < 0.5 && (r.3 - th).abs() < 0.5 {
            unsafe { pass.colorAttachments().objectAtIndexedSubscript(0).setLoadAction(MTLLoadAction::DontCare) };
        }
        let bars = bar_rects(r, (tw, th));
        let glowing = self.immersion && !bars.is_empty();
        if glowing {
            // the picture shrunk to a tiny image, which the bars are then made from
            let gp = MTLRenderPassDescriptor::new();
            unsafe {
                let a = gp.colorAttachments().objectAtIndexedSubscript(0);
                a.setTexture(Some(&self.glow));
                a.setLoadAction(MTLLoadAction::DontCare);
                a.setStoreAction(MTLStoreAction::Store);
            }
            let ge = cmd.renderCommandEncoderWithDescriptor(&gp).ok_or(GpuError::Texture("render encoder"))?;
            ge.setRenderPipelineState(&self.glow_down);
            unsafe {
                ge.setFragmentTexture_atIndex(Some(tex), 0);
                let cell = [1.0 / GLOW_SIZE.0 as f32, 1.0 / GLOW_SIZE.1 as f32];
                ge.setFragmentBytes_length_atIndex(NonNull::from(&cell).cast(), std::mem::size_of::<[f32; 2]>(), 0);
                ge.drawPrimitives_vertexStart_vertexCount(MTLPrimitiveType::Triangle, 0, 3);
            }
            ge.endEncoding();
        }
        let enc = cmd.renderCommandEncoderWithDescriptor(&pass).ok_or(GpuError::Texture("render encoder"))?;
        if glowing {
            enc.setRenderPipelineState(&self.ambient);
            enc.setViewport(MTLViewport { originX: 0.0, originY: 0.0, width: tw, height: th, znear: 0.0, zfar: 1.0 });
            unsafe {
                enc.setFragmentTexture_atIndex(Some(&self.glow), 0);
                let params = [tw as f32, th as f32, AMBIENT_DIM, AMBIENT_RADIUS];
                enc.setFragmentBytes_length_atIndex(NonNull::from(&params).cast(), std::mem::size_of::<[f32; 4]>(), 0);
            }
            for (x, y, w, h) in &bars {
                enc.setScissorRect(MTLScissorRect {
                    x: *x as usize,
                    y: *y as usize,
                    width: *w as usize,
                    height: *h as usize,
                });
                unsafe { enc.drawPrimitives_vertexStart_vertexCount(MTLPrimitiveType::Triangle, 0, 3) };
            }
            enc.setScissorRect(MTLScissorRect { x: 0, y: 0, width: tw as usize, height: th as usize });
        }
        enc.setRenderPipelineState(&self.present);
        enc.setViewport(MTLViewport { originX: r.0, originY: r.1, width: r.2, height: r.3, znear: 0.0, zfar: 1.0 });
        unsafe {
            enc.setFragmentTexture_atIndex(Some(tex), 0);
            let (plain, blend, use_blend) = match &self.blend {
                Some((p, k)) => (p.as_ref(), *k, 1.0f32),
                None => (tex, 1.0, 0.0),
            };
            enc.setFragmentTexture_atIndex(Some(plain), 1);
            let sat = if self.saturation.is_finite() { self.saturation.clamp(0.0, 2.0) } else { 1.0 };
            let sharp = if self.sharpen.is_finite() { self.sharpen.clamp(-1.0, 3.0) } else { 0.0 };
            let params = [r.2 as f32, r.3 as f32, sat, sharp, blend, use_blend, 0.0, 0.0];
            enc.setFragmentBytes_length_atIndex(NonNull::from(&params).cast(), std::mem::size_of::<[f32; 8]>(), 0);
            enc.drawPrimitives_vertexStart_vertexCount(MTLPrimitiveType::Triangle, 0, 3);
        }
        enc.endEncoding();
        let _ = v;
        Ok(())
    }
}

/// The parts of a `target`-sized window not covered by the picture rectangle `r` (x, y, width, height), as whole-pixel scissor rectangles
/// (x, y, width, height): left and right of a pillarboxed picture, above and below a letterboxed one. They overlap the picture by a
/// pixel so a fractional edge leaves no black line (the picture is drawn over them).
pub fn bar_rects(r: (f64, f64, f64, f64), target: (f64, f64)) -> Vec<(u32, u32, u32, u32)> {
    let (tw, th) = (target.0.floor() as i64, target.1.floor() as i64);
    let mut out = vec![];
    let (left, right) = (r.0.ceil() as i64 + 1, (r.0 + r.2).floor() as i64 - 1);
    let (top, bottom) = (r.1.ceil() as i64 + 1, (r.1 + r.3).floor() as i64 - 1);
    if r.0 > 0.5 {
        out.push((0, 0, left.clamp(0, tw) as u32, th as u32));
        if right < tw {
            out.push((right.max(0) as u32, 0, (tw - right.max(0)) as u32, th as u32));
        }
    } else if r.1 > 0.5 {
        out.push((0, 0, tw as u32, top.clamp(0, th) as u32));
        if bottom < th {
            out.push((0, bottom.max(0) as u32, tw as u32, (th - bottom.max(0)) as u32));
        }
    }
    out
}

/// Aspect-fit `(w, h)` into `(tw, th)`: returns (x, y, width, height) and the scale factor.
pub fn fit(w: f64, h: f64, tw: f64, th: f64) -> ((f64, f64, f64, f64), f64) {
    if w <= 0.0 || h <= 0.0 || tw <= 0.0 || th <= 0.0 {
        return ((0.0, 0.0, tw.max(0.0), th.max(0.0)), 1.0);
    }
    let s = (tw / w).min(th / h);
    let (fw, fh) = (w * s, h * s);
    (((tw - fw) / 2.0, (th - fh) / 2.0, fw, fh), s)
}

/// Size to upscale to: the chosen multiple of the stream (`mode_height` tall, the video's aspect), whatever the window is. A window
/// smaller than that gets the picture scaled down to fit, which looks better than upscaling to the window's own size (it is
/// supersampled). `None` when that is not larger than the video, when there is no window yet, or when the GPU could not make it.
pub fn upscale_target(src: (usize, usize), drawable: (usize, usize), mode_height: usize) -> Option<(usize, usize)> {
    if src.0 == 0 || src.1 == 0 || drawable.0 == 0 || drawable.1 == 0 || mode_height == 0 {
        return None;
    }
    let ow = (src.0 as f64 * mode_height as f64 / src.1 as f64).round() as usize;
    let (ow, oh) = (ow & !1, mode_height & !1); // even sizes
    (ow > src.0 && oh > src.1 && ow <= MAX_TEXTURE_DIM && oh <= MAX_TEXTURE_DIM).then_some((ow, oh))
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GlobalMotion {
    /// Full-resolution pixels per source frame.
    pub x: f32,
    pub y: f32,
    /// Fraction of confident blocks that agree with this motion.
    pub support: f32,
}

pub(crate) fn half_to_f32(h: u16) -> f32 {
    let (s, e, m) = ((h >> 15) & 1, (h >> 10) & 0x1F, h & 0x3FF);
    let v = match e {
        0 => f32::from(m) * 2f32.powi(-24),
        31 => {
            if m == 0 {
                f32::INFINITY
            } else {
                f32::NAN
            }
        }
        _ => (1.0 + f32::from(m) / 1024.0) * 2f32.powi(i32::from(e) - 15),
    };
    if s == 1 {
        -v
    } else {
        v
    }
}

/// The motion most blocks agree on: a confidence-weighted mean over blocks, re-done twice keeping only blocks close to the
/// current estimate, so a moving object does not drag the camera estimate with it. Vectors are in half-resolution pixels.
pub(crate) fn dominant_motion(blocks: &[(f32, f32, f32)]) -> GlobalMotion {
    let usable: Vec<&(f32, f32, f32)> =
        blocks.iter().filter(|b| b.2 > 0.5 && b.0.is_finite() && b.1.is_finite()).collect();
    if usable.is_empty() {
        return GlobalMotion { x: 0.0, y: 0.0, support: 0.0 };
    }
    let mean = |set: &[&(f32, f32, f32)]| {
        let w: f32 = set.iter().map(|b| b.2).sum();
        if w <= 0.0 {
            return (0.0, 0.0);
        }
        (set.iter().map(|b| b.0 * b.2).sum::<f32>() / w, set.iter().map(|b| b.1 * b.2).sum::<f32>() / w)
    };
    let mut m = mean(&usable);
    let mut inliers: Vec<&(f32, f32, f32)> = usable.clone();
    for radius in [6.0f32, 2.0, 1.0] {
        inliers =
            usable.iter().copied().filter(|b| ((b.0 - m.0).powi(2) + (b.1 - m.1).powi(2)).sqrt() <= radius).collect();
        if inliers.is_empty() {
            break;
        }
        m = mean(&inliers);
    }
    GlobalMotion { x: m.0 * 2.0, y: m.1 * 2.0, support: inliers.len() as f32 / usable.len() as f32 }
}

#[cfg(test)]
mod unit {
    use super::*;

    #[test]
    fn half_floats_decode() {
        assert_eq!(half_to_f32(0x3C00), 1.0);
        assert_eq!(half_to_f32(0xC000), -2.0);
        assert_eq!(half_to_f32(0x0000), 0.0);
        assert!((half_to_f32(0x3555) - 0.333_251_95).abs() < 1e-6);
        assert!(half_to_f32(0x7C00).is_infinite());
        assert!(half_to_f32(0x7E00).is_nan());
        assert!(half_to_f32(0x0001) > 0.0 && half_to_f32(0x0001) < 1e-6);
    }

    #[test]
    fn the_dominant_motion_ignores_a_moving_object_and_unreliable_blocks() {
        let mut b: Vec<(f32, f32, f32)> = vec![(4.0, -1.0, 1.0); 100]; // the camera pan
        b.extend(vec![(20.0, 6.0, 1.0); 15]); // an object moving differently
        b.extend(vec![(-8.0, 8.0, 0.1); 30]); // unreliable blocks
        let g = dominant_motion(&b);
        assert!((g.x - 8.0).abs() < 0.3 && (g.y + 2.0).abs() < 0.3, "{g:?}");
        assert!(g.support > 0.8);
        assert_eq!(dominant_motion(&[]).support, 0.0);
        assert_eq!(dominant_motion(&[(f32::NAN, 0.0, 1.0)]).support, 0.0);
    }
}
