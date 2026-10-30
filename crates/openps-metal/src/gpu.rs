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
}

pub struct Gpu {
    pub dev: Device,
    pub queue: Queue,
    luma: Retained<ProtocolObject<dyn MTLComputePipelineState>>,
    motion: Retained<ProtocolObject<dyn MTLComputePipelineState>>,
    warp: Retained<ProtocolObject<dyn MTLComputePipelineState>>,
    reset_stats: Retained<ProtocolObject<dyn MTLComputePipelineState>>,
    refine: Retained<ProtocolObject<dyn MTLComputePipelineState>>,
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
    present: Retained<ProtocolObject<dyn MTLRenderPipelineState>>,
    // per-stream state
    cur: Option<Tex>,
    prev: Option<Tex>,
    mv1: Option<Tex>,
    bg: Option<Tex>,
    luma_cur: Option<Tex>,
    luma_prev: Option<Tex>,
    mv: Option<Tex>,
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
    /// Colour saturation applied when the picture is presented (1.0 = as the stream is).
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
        let spatial_scaler_available = unsafe { MTLFXSpatialScalerDescriptor::supportsDevice(&dev) };
        let dev_for_probe = dev.clone();
        Ok(Self {
            reset_stats: pso(&dev, &lib, "reset_stats")?,
            refine: pso(&dev, &lib, "refine")?,
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
            present,
            dev,
            queue,
            cur: None,
            prev: None,
            mv1: None,
            bg: None,
            luma_cur: None,
            luma_prev: None,
            mv: None,
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
            saturation: 1.0,
            sharpen: 0.0,
            blend: None,
            legacy_warp: false,
        })
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

    /// A new real frame. Computes its luma and, when `motion` is wanted and a previous frame exists, the motion field.
    pub fn ingest(&mut self, cmd: &Cmd, frame: Tex, motion: bool) -> Result<(), GpuError> {
        let (w, h) = (frame.width(), frame.height());
        let changed = self.cur.as_ref().is_none_or(|c| (c.width(), c.height()) != (w, h));
        if changed {
            self.luma_cur = None;
            self.luma_prev = None;
            self.mv = None;
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
            self.mv = Some(make_tex(
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
            let p = MeParams { radius: SEARCH_RADIUS, lambda: 0.4 / 255.0, flat: 2.0 / 255.0, conf_scale: 12.0 };
            enc.setComputePipelineState(&self.motion);
            unsafe {
                enc.setTexture_atIndex(Some(lc), 0);
                enc.setTexture_atIndex(Some(lp), 1);
                enc.setTexture_atIndex(Some(mv), 2);
                enc.setBytes_length_atIndex(NonNull::from(&p).cast(), std::mem::size_of::<MeParams>(), 0);
                enc.setBuffer_offset_atIndex(Some(&self.stats), 0, 1);
            }
            Self::dispatch(&enc, &self.motion, lw.div_ceil(BLOCK), lh.div_ceil(BLOCK));
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
        self.mv = None;
        self.mv1 = None;
        self.bg = None;
        self.warp_out = None;
        self.scaler = None;
        self.fx = None;
        self.ingested = 0;
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

    fn upscale(&mut self, cmd: &Cmd, shown: Tex, out: Option<(usize, usize)>) -> Result<(Tex, bool), GpuError> {
        self.blend = None;
        let (w, h) = (shown.width(), shown.height());
        if let Some((ow, oh)) = out {
            if self.spatial_scaler_available && ow > w && oh > h {
                let key = (w, h, ow, oh);
                if self.scaler.as_ref().is_none_or(|s| s.key != key) {
                    self.scaler = Some(self.make_scaler(key)?);
                }
                let s = self.scaler.as_ref().unwrap();
                unsafe {
                    s.scaler.setColorTexture(Some(&shown));
                    s.scaler.setOutputTexture(Some(&s.out));
                    s.scaler.encodeToCommandBuffer(cmd);
                }
                let k = self.sharpness.clamp(0.0, 2.0);
                if (k - 1.0).abs() > 0.005 {
                    self.blend = Some((shown.clone(), k)); // blended in the present pass
                }
                return Ok((s.out.clone(), true));
            }
        }
        Ok((shown, false))
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

    fn make_scaler(&self, key: (usize, usize, usize, usize)) -> Result<Scaler, GpuError> {
        let d = unsafe { MTLFXSpatialScalerDescriptor::new() };
        unsafe {
            d.setInputWidth(key.0);
            d.setInputHeight(key.1);
            d.setOutputWidth(key.2);
            d.setOutputHeight(key.3);
            d.setColorTextureFormat(MTLPixelFormat::BGRA8Unorm);
            d.setOutputTextureFormat(MTLPixelFormat::BGRA8Unorm);
            d.setColorProcessingMode(MTLFXSpatialScalerColorProcessingMode::Perceptual);
        }
        let scaler =
            unsafe { d.newSpatialScalerWithDevice(&self.dev) }.ok_or(GpuError::Pipeline("MetalFX scaler".into()))?;
        let out = make_tex(
            &self.dev,
            MTLPixelFormat::BGRA8Unorm,
            key.2,
            key.3,
            MTLTextureUsage::RenderTarget | MTLTextureUsage::ShaderRead,
            false,
        )?;
        Ok(Scaler { key, scaler, out })
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
        let enc = cmd.renderCommandEncoderWithDescriptor(&pass).ok_or(GpuError::Texture("render encoder"))?;
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

/// Aspect-fit `(w, h)` into `(tw, th)`: returns (x, y, width, height) and the scale factor.
pub fn fit(w: f64, h: f64, tw: f64, th: f64) -> ((f64, f64, f64, f64), f64) {
    if w <= 0.0 || h <= 0.0 || tw <= 0.0 || th <= 0.0 {
        return ((0.0, 0.0, tw.max(0.0), th.max(0.0)), 1.0);
    }
    let s = (tw / w).min(th / h);
    let (fw, fh) = (w * s, h * s);
    (((tw - fw) / 2.0, (th - fh) / 2.0, fw, fh), s)
}

/// Size to upscale to: the video aspect-fitted into the drawable, never above the chosen mode's height, and `None`
/// when that is not larger than the source (nothing to gain).
pub fn upscale_target(src: (usize, usize), drawable: (usize, usize), mode_height: usize) -> Option<(usize, usize)> {
    if src.0 == 0 || src.1 == 0 || drawable.0 == 0 || drawable.1 == 0 {
        return None;
    }
    let (r, _) = fit(src.0 as f64, src.1 as f64, drawable.0 as f64, drawable.1 as f64);
    let (mut ow, mut oh) = (r.2.round() as usize, r.3.round() as usize);
    if oh > mode_height && mode_height > 0 {
        ow = (ow as f64 * mode_height as f64 / oh as f64).round() as usize;
        oh = mode_height;
    }
    let (ow, oh) = (ow & !1, oh & !1); // even sizes
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
