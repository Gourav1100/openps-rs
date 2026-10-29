//! `MediaSink` for audio: configures from the console's stream info, decodes Opus frames and plays them.
//! Everything else the sink is offered is ignored here (chain it behind a video sink or the other way round).

use crate::header::AudioHeader;
use crate::opus::OpusDecoder;
use crate::player::{Player, PlayerConfig, PlayerStats};
use openps_core::events::MediaSink;
use openps_core::sync::LockExt;
use openps_stream::audio::OpusFrame;
use openps_takion::proto::StreamInfoPayload;
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::Mutex;

/// Where decoded PCM goes. The real implementation is [`Player`]; tests substitute a recorder.
pub trait PcmOut: Send + Sync {
    fn push(&self, samples: &[i16]);
    fn set_volume(&self, v: f32);
    fn flush(&self) {}
    fn stats(&self) -> PlayerStats {
        PlayerStats::default()
    }
}

impl PcmOut for Player {
    fn push(&self, samples: &[i16]) {
        Player::push(self, samples)
    }
    fn set_volume(&self, v: f32) {
        Player::set_volume(self, v)
    }
    fn flush(&self) {
        Player::flush(self)
    }
    fn stats(&self) -> PlayerStats {
        Player::stats(self)
    }
}

pub type OutputFactory = Box<dyn Fn(&AudioHeader) -> Result<Box<dyn PcmOut>, String> + Send + Sync>;

/// Opus errors in a row after which the decoder is reset (the original tolerates 30 before giving up).
pub const MAX_CONSECUTIVE_ERRORS: u32 = 30;

struct Active {
    dec: OpusDecoder,
    out: Box<dyn PcmOut>,
    scratch: Vec<i16>,
    consecutive_errors: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AudioCounters {
    pub decoded: u64,
    pub errors: u64,
    pub resets: u64,
    pub ignored_before_config: u64,
}

pub struct AudioOut {
    factory: OutputFactory,
    active: Mutex<Option<Active>>,
    volume: AtomicU8, // 0..=100
    decoded: AtomicU64,
    errors: AtomicU64,
    resets: AtomicU64,
    ignored: AtomicU64,
    last_problem: Mutex<Option<String>>,
}

impl AudioOut {
    /// Real playback through the default output device.
    pub fn system() -> Self {
        Self::with_output(Box::new(|h| {
            Player::new(*h, PlayerConfig::default()).map(|p| Box::new(p) as Box<dyn PcmOut>).map_err(|e| e.to_string())
        }))
    }

    pub fn with_output(factory: OutputFactory) -> Self {
        Self {
            factory,
            active: Mutex::new(None),
            volume: AtomicU8::new(100),
            decoded: AtomicU64::new(0),
            errors: AtomicU64::new(0),
            resets: AtomicU64::new(0),
            ignored: AtomicU64::new(0),
            last_problem: Mutex::new(None),
        }
    }

    /// 0..=100; values above 100 are clamped. Applies immediately and to later sessions.
    pub fn set_volume(&self, percent: u8) {
        let p = percent.min(100);
        self.volume.store(p, Ordering::Relaxed);
        if let Some(a) = self.active.lock_ok().as_ref() {
            a.out.set_volume(f32::from(p) / 100.0);
        }
    }

    pub fn volume(&self) -> u8 {
        self.volume.load(Ordering::Relaxed)
    }

    pub fn counters(&self) -> AudioCounters {
        AudioCounters {
            decoded: self.decoded.load(Ordering::Relaxed),
            errors: self.errors.load(Ordering::Relaxed),
            resets: self.resets.load(Ordering::Relaxed),
            ignored_before_config: self.ignored.load(Ordering::Relaxed),
        }
    }

    pub fn player_stats(&self) -> Option<PlayerStats> {
        self.active.lock_ok().as_ref().map(|a| a.out.stats())
    }

    /// Why audio is not working, if it is not (shown to the user instead of silently having no sound).
    pub fn problem(&self) -> Option<String> {
        self.last_problem.lock_ok().clone()
    }

    fn fail(&self, msg: String) {
        *self.last_problem.lock_ok() = Some(msg);
    }
}

impl MediaSink for AudioOut {
    fn stream_info(&self, info: &StreamInfoPayload) {
        // new session: nothing carries over
        *self.active.lock_ok() = None;
        *self.last_problem.lock_ok() = None;
        let Some(raw) = info.audio_header.as_deref() else {
            return self.fail("the console sent no audio description".into());
        };
        let header = match AudioHeader::parse(raw) {
            Ok(h) => h,
            Err(e) => return self.fail(format!("audio format not supported: {e}")),
        };
        let dec = match OpusDecoder::new(header) {
            Ok(d) => d,
            Err(e) => return self.fail(format!("cannot start the audio decoder: {e}")),
        };
        let out = match (self.factory)(&header) {
            Ok(o) => o,
            Err(e) => return self.fail(format!("cannot open the audio output: {e}")),
        };
        out.set_volume(f32::from(self.volume()) / 100.0);
        *self.active.lock_ok() = Some(Active { dec, out, scratch: Vec::new(), consecutive_errors: 0 });
    }

    fn audio(&self, frames: Vec<OpusFrame>) {
        let mut guard = self.active.lock_ok();
        let Some(a) = guard.as_mut() else {
            self.ignored.fetch_add(frames.len() as u64, Ordering::Relaxed);
            return;
        };
        for f in &frames {
            a.scratch.clear();
            match a.dec.decode(&f.data, &mut a.scratch) {
                Ok(()) => {
                    a.consecutive_errors = 0;
                    self.decoded.fetch_add(1, Ordering::Relaxed);
                    a.out.push(&a.scratch);
                }
                Err(_) => {
                    self.errors.fetch_add(1, Ordering::Relaxed);
                    a.consecutive_errors += 1;
                    if a.consecutive_errors >= MAX_CONSECUTIVE_ERRORS {
                        // a long run of bad frames: stale decoder state is likelier the cause than the data
                        a.dec.reset();
                        a.consecutive_errors = 0;
                        self.resets.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        }
    }
}
