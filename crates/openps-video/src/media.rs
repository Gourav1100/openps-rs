//! `MediaSink` that decodes video and hands pictures to a [`PictureSink`] (the renderer). Audio, haptics and pad
//! output are forwarded untouched to an optional inner sink.

use crate::pipeline::Pipeline;
use crate::vt::Frame as Picture;
use crate::vt::OutputFormat;
use openps_core::events::MediaSink;
use openps_core::sync::LockExt;
use openps_stream::audio::OpusFrame;
use openps_stream::padblob::PadOutput;
use openps_stream::video::Frame;
use openps_takion::proto::StreamInfoPayload;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

pub trait PictureSink: Send + Sync {
    fn picture(&self, picture: Picture);
    /// The format is unusable (no parameter sets, decoder refused them). Called at most once per session.
    fn unavailable(&self, _reason: &str) {}
}

pub struct DecodingMedia {
    pipeline: Mutex<Pipeline>,
    pictures: Arc<dyn PictureSink>,
    rest: Option<Arc<dyn MediaSink>>,
    keyframe: AtomicBool,
    /// Deliver BGRA (for the Metal renderer) instead of the native format.
    bgra: AtomicBool,
}

impl DecodingMedia {
    pub fn new(pictures: Arc<dyn PictureSink>, rest: Option<Arc<dyn MediaSink>>) -> Self {
        Self {
            pipeline: Mutex::new(Pipeline::new()),
            pictures,
            rest,
            keyframe: AtomicBool::new(false),
            bgra: AtomicBool::new(false),
        }
    }

    /// Choose the decoder's output format for the next session.
    pub fn set_bgra(&self, on: bool) {
        self.bgra.store(on, Ordering::Relaxed);
    }

    pub fn counters(&self) -> crate::pipeline::Counters {
        self.pipeline.lock_ok().counters
    }
}

impl MediaSink for DecodingMedia {
    fn stream_info(&self, info: &StreamInfoPayload) {
        // a new session always starts with a fresh decoder: nothing carries over from the previous stream
        let mut p = self.pipeline.lock_ok();
        *p = Pipeline::new();
        p.set_output(if self.bgra.load(Ordering::Relaxed) { OutputFormat::Bgra } else { OutputFormat::Native });
        if let Err(e) = p.configure(info) {
            self.pictures.unavailable(&format!("cannot start video decoder: {e}"));
        }
        drop(p);
        if let Some(r) = &self.rest {
            r.stream_info(info);
        }
    }

    fn video(&self, frame: Frame) {
        let push = self.pipeline.lock_ok().push(&frame);
        if push.request_keyframe {
            self.keyframe.store(true, Ordering::Release);
        }
        for p in push.pictures {
            self.pictures.picture(p);
        }
    }

    fn take_keyframe_request(&self) -> bool {
        self.keyframe.swap(false, Ordering::AcqRel)
    }

    fn pictures_shown(&self) -> Option<u64> {
        Some(self.counters().decoded)
    }

    fn audio(&self, frames: Vec<OpusFrame>) {
        if let Some(r) = &self.rest {
            r.audio(frames);
        }
    }
    fn haptic(&self, frame: [u8; 120]) {
        if let Some(r) = &self.rest {
            r.haptic(frame);
        }
    }
    fn pad_output(&self, out: PadOutput) {
        if let Some(r) = &self.rest {
            r.pad_output(out);
        }
    }
}
