//! What the core tells the UI, and where media goes.

use crate::sync::LockExt;
use crate::types::SessionState;
use openps_stream::audio::OpusFrame;
use openps_stream::padblob::PadOutput;
use openps_stream::video::Frame;
use serde::Serialize;
use std::sync::Mutex;

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AppEvent {
    ConsolesChanged,
    Session(SessionState),
    /// A message from the console on the control channel (pad join results, display status, ...).
    Control {
        kind: String,
        detail: String,
    },
    /// Once per second while streaming.
    Stats(StreamStats),
    PsnChanged {
        signed_in: bool,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct StreamStats {
    pub fps: f64,
    pub kbps: f64,
    pub video_frames_dropped: u64,
    pub datagrams_in: u64,
}

pub trait EventSink: Send + Sync {
    fn emit(&self, event: AppEvent);
}

/// Receives decoded-stream data. The decoder/renderer implements this; until it exists the front end only sees stats.
pub trait MediaSink: Send + Sync {
    /// The console's stream description (codec parameter sets, audio header), once per session before any media.
    fn stream_info(&self, _info: &openps_takion::proto::StreamInfoPayload) {}
    /// Polled after each video frame: the sink cannot show pictures until a keyframe (loss, decoder reset).
    fn take_keyframe_request(&self) -> bool {
        false
    }
    fn video(&self, _frame: Frame) {}
    fn audio(&self, _frames: Vec<OpusFrame>) {}
    fn haptic(&self, _frame: [u8; 120]) {}
    fn pad_output(&self, _out: PadOutput) {}
}

/// Discards media.
pub struct NullMedia;
impl MediaSink for NullMedia {}

/// Collects events (tests).
#[derive(Default)]
pub struct VecSink(pub Mutex<Vec<AppEvent>>);

impl EventSink for VecSink {
    fn emit(&self, event: AppEvent) {
        self.0.lock_ok().push(event);
    }
}

impl VecSink {
    pub fn events(&self) -> Vec<AppEvent> {
        self.0.lock_ok().clone()
    }
    pub fn states(&self) -> Vec<SessionState> {
        self.events().into_iter().filter_map(|e| if let AppEvent::Session(s) = e { Some(s) } else { None }).collect()
    }
}
