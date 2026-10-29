//! Opus decoding through AudioToolbox's native decoder (no libopus to install or ship).
//! Each call decodes exactly one Opus packet to `samples_per_frame` interleaved 16-bit samples per channel.

use crate::ffi::*;
use crate::header::AudioHeader;
use std::ffi::c_void;
use std::ptr;

#[derive(Debug, PartialEq, Eq)]
pub enum OpusError {
    /// The system refused to create the decoder.
    Create(i32),
    /// Empty or oversized packet.
    BadPacket,
    /// The decoder rejected the packet (corrupt data). The caller drops the frame.
    Decode(i32),
    /// The decoder produced fewer samples than a full frame.
    Short(u32),
}
impl std::fmt::Display for OpusError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for OpusError {}

/// Largest legal Opus packet (RFC 6716: 1275 bytes per frame, up to 48 frames per packet is far beyond what the
/// console sends); anything bigger is not Opus.
pub const MAX_PACKET: usize = 1276 * 3;

const NO_MORE_INPUT: OSStatus = 0x6E6F6D72; // 'nomr': our own marker, returned when the single packet was consumed

pub struct OpusDecoder {
    conv: AudioConverterRef,
    header: AudioHeader,
    /// True once the first packet after creation/reset was decoded: AudioToolbox drops the Opus pre-skip (priming)
    /// samples from that first packet, which therefore comes back short.
    primed: bool,
}
unsafe impl Send for OpusDecoder {}

struct Feed {
    data: *mut c_void,
    len: u32,
    channels: u32,
    desc: AudioStreamPacketDescription,
    given: bool,
}

extern "C" fn input_proc(
    _conv: AudioConverterRef,
    io_packets: *mut u32,
    io_data: *mut AudioBufferList,
    out_desc: *mut *mut AudioStreamPacketDescription,
    user: *mut c_void,
) -> OSStatus {
    // SAFETY: `user` is the `Feed` on the caller's stack for the duration of AudioConverterFillComplexBuffer.
    unsafe {
        let feed = &mut *(user as *mut Feed);
        if feed.given {
            *io_packets = 0;
            return NO_MORE_INPUT;
        }
        feed.given = true;
        *io_packets = 1;
        (*io_data).mNumberBuffers = 1;
        (*io_data).mBuffers[0] =
            AudioBuffer { mNumberChannels: feed.channels, mDataByteSize: feed.len, mData: feed.data };
        if !out_desc.is_null() {
            *out_desc = &mut feed.desc;
        }
    }
    0
}

impl OpusDecoder {
    pub fn new(h: AudioHeader) -> Result<Self, OpusError> {
        let src = AudioStreamBasicDescription {
            mSampleRate: f64::from(h.sample_rate),
            mFormatID: FORMAT_OPUS,
            mFramesPerPacket: h.samples_per_frame,
            mChannelsPerFrame: u32::from(h.channels),
            ..Default::default()
        };
        let dst = pcm_format(&h);
        let mut conv = ptr::null_mut();
        let st = unsafe { AudioConverterNew(&src, &dst, &mut conv) };
        if st != 0 || conv.is_null() {
            return Err(OpusError::Create(st));
        }
        Ok(Self { conv, header: h, primed: false })
    }

    pub fn header(&self) -> AudioHeader {
        self.header
    }

    /// Decode one packet into `out` (cleared first). Appends `samples_per_frame * channels` samples on success.
    pub fn decode(&mut self, packet: &[u8], out: &mut Vec<i16>) -> Result<(), OpusError> {
        if packet.is_empty() || packet.len() > MAX_PACKET {
            return Err(OpusError::BadPacket);
        }
        let n = self.header.frame_samples();
        let start = out.len();
        out.resize(start + n, 0);
        let mut packets = self.header.samples_per_frame;
        let mut list = AudioBufferList {
            mNumberBuffers: 1,
            mBuffers: [AudioBuffer {
                mNumberChannels: u32::from(self.header.channels),
                mDataByteSize: (n * 2) as u32,
                mData: out[start..].as_mut_ptr() as *mut c_void,
            }],
        };
        let mut feed = Feed {
            data: packet.as_ptr() as *mut c_void, // read-only: the decoder never writes to its input
            len: packet.len() as u32,
            channels: u32::from(self.header.channels),
            desc: AudioStreamPacketDescription {
                mStartOffset: 0,
                mVariableFramesInPacket: 0,
                mDataByteSize: packet.len() as u32,
            },
            given: false,
        };
        let st = unsafe {
            AudioConverterFillComplexBuffer(
                self.conv,
                input_proc,
                &mut feed as *mut Feed as *mut c_void,
                &mut packets,
                &mut list,
                ptr::null_mut(),
            )
        };
        if st != 0 && st != NO_MORE_INPUT {
            out.truncate(start);
            return Err(OpusError::Decode(st));
        }
        if packets < self.header.samples_per_frame {
            if self.primed {
                out.truncate(start);
                return Err(OpusError::Short(packets));
            }
            // First packet: the decoder removed the pre-skip. Keep the whole-frame invariant (and the timing) by
            // padding the front with silence, so the real samples stay aligned with the end of the frame.
            let got = packets as usize * self.header.channels as usize;
            out.copy_within(start..start + got, start + n - got);
            out[start..start + n - got].fill(0);
        }
        self.primed = true;
        Ok(())
    }

    /// Forget decoder state (after a long gap) so stale prediction does not leak into the next frame.
    pub fn reset(&mut self) {
        unsafe { AudioConverterReset(self.conv) };
        self.primed = false;
    }
}

impl Drop for OpusDecoder {
    fn drop(&mut self) {
        unsafe { AudioConverterDispose(self.conv) };
    }
}

pub(crate) fn pcm_format(h: &AudioHeader) -> AudioStreamBasicDescription {
    let ch = u32::from(h.channels);
    AudioStreamBasicDescription {
        mSampleRate: f64::from(h.sample_rate),
        mFormatID: FORMAT_LPCM,
        mFormatFlags: PCM_S16_FLAGS,
        mBytesPerPacket: 2 * ch,
        mFramesPerPacket: 1,
        mBytesPerFrame: 2 * ch,
        mChannelsPerFrame: ch,
        mBitsPerChannel: 16,
        mReserved: 0,
    }
}
