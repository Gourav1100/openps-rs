//! Glue between the stream's reassembled access units and the decoder: picks the codec from the stream header,
//! rebuilds on format changes, and turns decode trouble into rate-limited keyframe requests.

use crate::nal::{self, Codec, ParameterSets};
use crate::vt::{Decoder, Error, Frame, OutputFormat};
use openps_stream::video::Frame as AccessUnit;
use openps_takion::proto::StreamInfoPayload;
use std::time::{Duration, Instant};

/// Minimum spacing between keyframe requests; the console needs a moment to produce one and a flood of requests
/// makes it send several large IDRs back to back.
pub const KEYFRAME_REQUEST_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Default)]
pub struct Push {
    pub pictures: Vec<Frame>,
    /// The pipeline cannot show anything until the next keyframe; the caller should ask the console for one.
    pub request_keyframe: bool,
}

#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Counters {
    pub decoded: u64,
    pub dropped: u64,
    pub keyframe_requests: u64,
}

pub struct Pipeline {
    decoder: Option<Decoder>,
    codec: Option<Codec>,
    last_request: Option<Instant>,
    last_id: Option<u16>,
    output: OutputFormat,
    pub counters: Counters,
}

impl Default for Pipeline {
    fn default() -> Self {
        Self::new()
    }
}

impl Pipeline {
    pub fn new() -> Self {
        Pipeline {
            decoder: None,
            codec: None,
            last_request: None,
            last_id: None,
            output: OutputFormat::Native,
            counters: Counters::default(),
        }
    }

    /// What the decoder delivers (takes effect at the next `configure`).
    pub fn set_output(&mut self, output: OutputFormat) {
        self.output = output;
    }

    pub fn codec(&self) -> Option<Codec> {
        self.codec
    }

    /// Configure from the console's stream info. The first resolution entry carrying a header is used (the console
    /// lists the adaptive-quality variants with identical codec). Returns the codec found.
    pub fn configure(&mut self, info: &StreamInfoPayload) -> Result<Codec, Error> {
        let header = info
            .resolution
            .iter()
            .find_map(|r| r.video_header.as_deref().filter(|h| !h.is_empty()))
            .ok_or(Error::ParameterSets(-1))?;
        self.configure_header(header)
    }

    pub fn configure_header(&mut self, header: &[u8]) -> Result<Codec, Error> {
        let codec = nal::detect_codec(header).ok_or(Error::ParameterSets(-2))?;
        let params = ParameterSets::from_annex_b(codec, header);
        self.decoder = Some(Decoder::new_with(codec, params, self.output)?);
        self.codec = Some(codec);
        self.last_id = None;
        Ok(codec)
    }

    pub fn push(&mut self, unit: &AccessUnit) -> Push {
        let Some(dec) = self.decoder.as_mut() else {
            self.counters.dropped += 1;
            return Push::default();
        };
        // A gap in frame ids means the stream layer skipped a frame: a delta frame after it would reference a picture we
        // never decoded. VideoToolbox may decode that without complaint and show corruption, so do not even try.
        let gap = self.last_id.is_some_and(|last| unit.id != last.wrapping_add(1));
        self.last_id = Some(unit.id);
        if gap && !nal::analyse(dec.codec(), &unit.data).keyframe {
            dec.reset();
        }
        let r = dec.decode_annex_b(&unit.data, i64::from(unit.id) * 1500);
        match r {
            Ok(pictures) => {
                self.counters.decoded += pictures.len() as u64;
                Push { pictures, request_keyframe: false }
            }
            Err(e) => {
                self.counters.dropped += 1;
                if matches!(e, Error::Decode(_)) {
                    dec.reset();
                }
                // `Empty` is harmless (a unit without pictures); everything else means we are blind until an IDR.
                let want = !matches!(e, Error::Empty) && self.keyframe_due();
                Push { pictures: vec![], request_keyframe: want }
            }
        }
    }

    /// Tell the pipeline frames were lost upstream (the stream reported a gap): wait for a keyframe.
    pub fn on_loss(&mut self) -> bool {
        if let Some(d) = self.decoder.as_mut() {
            d.reset();
        }
        self.keyframe_due()
    }

    fn keyframe_due(&mut self) -> bool {
        let now = Instant::now();
        if self.last_request.is_some_and(|t| now.duration_since(t) < KEYFRAME_REQUEST_INTERVAL) {
            return false;
        }
        self.last_request = Some(now);
        self.counters.keyframe_requests += 1;
        true
    }
}
