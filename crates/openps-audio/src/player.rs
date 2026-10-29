//! Playback through an `AudioQueue` fed from a jitter buffer.
//!
//! The queue's callback pulls PCM from a ring. Policy:
//! * **Prefill**: output is silent until `prefill_ms` of audio has accumulated, so playback does not start with an
//!   immediate underrun.
//! * **Underrun**: if the ring runs dry mid-stream the rest of the buffer is silence, the event is counted, and the
//!   player re-enters prefill (rather than stuttering at the edge of emptiness).
//! * **Overrun**: if the ring grows beyond `max_ms` (the console's clock runs faster than ours, or the app stalled),
//!   the oldest audio is dropped down to `target_ms`. Latency is bounded; one audible skip beats ever-growing delay.

use crate::ffi::*;
use crate::header::AudioHeader;
use crate::opus::pcm_format;
use openps_core::sync::LockExt;
use std::collections::VecDeque;
use std::ffi::c_void;
use std::ptr;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, Debug)]
pub struct PlayerConfig {
    /// Audio to accumulate before starting (and after an underrun).
    pub prefill_ms: u32,
    /// Trim point when the ring overruns.
    pub target_ms: u32,
    /// Ring size above which we trim.
    pub max_ms: u32,
    /// Length of each queue buffer.
    pub buffer_ms: u32,
    pub buffers: u32,
}

impl Default for PlayerConfig {
    fn default() -> Self {
        // the original's default output buffer is 100 ms; be a little tighter, remote play is interactive
        Self { prefill_ms: 40, target_ms: 60, max_ms: 150, buffer_ms: 20, buffers: 3 }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlayerStats {
    pub underruns: u64,
    pub overruns: u64,
    /// Samples (all channels) handed to the device so far, silence included.
    pub samples_out: u64,
    /// Samples currently waiting in the ring.
    pub buffered: u64,
}

#[derive(Debug)]
pub enum PlayerError {
    Create(i32),
    Buffer(i32),
    Start(i32),
}
impl std::fmt::Display for PlayerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for PlayerError {}

struct Ring {
    buf: VecDeque<i16>,
    /// False while prefilling.
    started: bool,
}

struct Shared {
    ring: Mutex<Ring>,
    prefill: usize,
    target: usize,
    max: usize,
    underruns: AtomicU64,
    overruns: AtomicU64,
    out: AtomicU64,
}

pub struct Player {
    queue: AudioQueueRef,
    shared: *const Shared, // Arc::into_raw; reclaimed in Drop after the queue is disposed
    volume: AtomicU32,
}
unsafe impl Send for Player {}
unsafe impl Sync for Player {}

extern "C" fn on_buffer(user: *mut c_void, queue: AudioQueueRef, buf: AudioQueueBufferRef) {
    // SAFETY: `user` is Arc::into_raw of a Shared that outlives the queue (Drop disposes the queue first).
    let sh = unsafe { &*(user as *const Shared) };
    let (data, cap_bytes) = unsafe { ((*buf).mAudioData as *mut i16, (*buf).mAudioDataBytesCapacity as usize) };
    let want = cap_bytes / 2;
    let out = unsafe { std::slice::from_raw_parts_mut(data, want) };
    let mut filled = 0;
    // never block the real-time thread: if the producer holds the lock right now, play silence this round
    if let Ok(mut r) = sh.ring.try_lock() {
        if !r.started && r.buf.len() >= sh.prefill {
            r.started = true;
        }
        if r.started {
            let n = want.min(r.buf.len());
            for (o, s) in out.iter_mut().zip(r.buf.drain(..n)) {
                *o = s;
            }
            filled = n;
            if n < want {
                // the device needed more than we had: that is an underrun. (Running dry exactly at a buffer boundary is
                // not one yet; the next callback finds the ring empty and counts it then.)
                r.started = false;
                sh.underruns.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
    out[filled..].fill(0);
    sh.out.fetch_add(want as u64, Ordering::Relaxed);
    unsafe {
        (*buf).mAudioDataByteSize = cap_bytes as u32;
        AudioQueueEnqueueBuffer(queue, buf, 0, ptr::null());
    }
}

impl Player {
    pub fn new(h: AudioHeader, cfg: PlayerConfig) -> Result<Self, PlayerError> {
        let per_ms = h.sample_rate as usize * h.channels as usize / 1000;
        // sanitise: prefill <= target <= max, and never a zero-length device buffer
        let cfg = {
            let target_ms = cfg.target_ms.max(1);
            let max_ms = cfg.max_ms.max(target_ms);
            PlayerConfig {
                prefill_ms: cfg.prefill_ms.min(target_ms),
                target_ms,
                max_ms,
                buffer_ms: cfg.buffer_ms.max(1),
                ..cfg
            }
        };
        let shared = Arc::new(Shared {
            ring: Mutex::new(Ring { buf: VecDeque::new(), started: false }),
            prefill: cfg.prefill_ms as usize * per_ms,
            target: cfg.target_ms as usize * per_ms,
            max: cfg.max_ms.max(cfg.target_ms) as usize * per_ms,
            underruns: AtomicU64::new(0),
            overruns: AtomicU64::new(0),
            out: AtomicU64::new(0),
        });
        let raw = Arc::into_raw(shared);
        let fmt = pcm_format(&h);
        let mut q: AudioQueueRef = ptr::null_mut();
        let st =
            unsafe { AudioQueueNewOutput(&fmt, on_buffer, raw as *mut c_void, ptr::null(), ptr::null(), 0, &mut q) };
        if st != 0 || q.is_null() {
            drop(unsafe { Arc::from_raw(raw) });
            return Err(PlayerError::Create(st));
        }
        let me = Player { queue: q, shared: raw, volume: AtomicU32::new(1.0f32.to_bits()) };
        let bytes = (cfg.buffer_ms as usize * per_ms * 2) as u32;
        for _ in 0..cfg.buffers.max(2) {
            let mut b: AudioQueueBufferRef = ptr::null_mut();
            let st = unsafe { AudioQueueAllocateBuffer(q, bytes, &mut b) };
            if st != 0 || b.is_null() {
                return Err(PlayerError::Buffer(st)); // Drop disposes the queue and the shared state
            }
            // prime with silence through the same path as steady state
            on_buffer(raw as *mut c_void, q, b);
        }
        let st = unsafe { AudioQueueStart(q, ptr::null()) };
        if st != 0 {
            return Err(PlayerError::Start(st));
        }
        Ok(me)
    }

    fn shared(&self) -> &Shared {
        unsafe { &*self.shared }
    }

    /// Queue decoded interleaved samples for playback.
    pub fn push(&self, samples: &[i16]) {
        let sh = self.shared();
        let mut r = sh.ring.lock_ok();
        r.buf.extend(samples.iter().copied());
        if r.buf.len() > sh.max {
            let drop_n = r.buf.len() - sh.target;
            r.buf.drain(..drop_n);
            sh.overruns.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// 0.0 (silent) ..= 1.0.
    pub fn set_volume(&self, v: f32) {
        let v = if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 };
        self.volume.store(v.to_bits(), Ordering::Relaxed);
        unsafe { AudioQueueSetParameter(self.queue, QUEUE_PARAM_VOLUME, v) };
    }

    pub fn volume(&self) -> f32 {
        f32::from_bits(self.volume.load(Ordering::Relaxed))
    }

    /// Drop everything queued (after a reconnect or a long stall).
    pub fn flush(&self) {
        let mut r = self.shared().ring.lock_ok();
        r.buf.clear();
        r.started = false;
    }

    pub fn stats(&self) -> PlayerStats {
        let sh = self.shared();
        PlayerStats {
            underruns: sh.underruns.load(Ordering::Relaxed),
            overruns: sh.overruns.load(Ordering::Relaxed),
            samples_out: sh.out.load(Ordering::Relaxed),
            buffered: sh.ring.lock_ok().buf.len() as u64,
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        unsafe {
            AudioQueueStop(self.queue, 1);
            AudioQueueDispose(self.queue, 1); // synchronous: no callback runs after this returns
            drop(Arc::from_raw(self.shared));
        }
    }
}
