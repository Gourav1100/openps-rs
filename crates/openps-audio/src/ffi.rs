//! Raw AudioToolbox declarations (the surface we use is small enough to audit by eye).
#![allow(non_snake_case, non_camel_case_types, dead_code)]

use std::ffi::c_void;

pub type OSStatus = i32;
pub type AudioConverterRef = *mut c_void;
pub type AudioQueueRef = *mut c_void;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct AudioStreamBasicDescription {
    pub mSampleRate: f64,
    pub mFormatID: u32,
    pub mFormatFlags: u32,
    pub mBytesPerPacket: u32,
    pub mFramesPerPacket: u32,
    pub mBytesPerFrame: u32,
    pub mChannelsPerFrame: u32,
    pub mBitsPerChannel: u32,
    pub mReserved: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct AudioBuffer {
    pub mNumberChannels: u32,
    pub mDataByteSize: u32,
    pub mData: *mut c_void,
}

#[repr(C)]
pub struct AudioBufferList {
    pub mNumberBuffers: u32,
    pub mBuffers: [AudioBuffer; 1],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct AudioStreamPacketDescription {
    pub mStartOffset: i64,
    pub mVariableFramesInPacket: u32,
    pub mDataByteSize: u32,
}

#[repr(C)]
pub struct AudioQueueBuffer {
    pub mAudioDataBytesCapacity: u32,
    pub mAudioData: *mut c_void,
    pub mAudioDataByteSize: u32,
    pub mUserData: *mut c_void,
    pub mPacketDescriptionCapacity: u32,
    pub mPacketDescriptions: *mut c_void,
    pub mPacketDescriptionCount: u32,
}
pub type AudioQueueBufferRef = *mut AudioQueueBuffer;

pub const FORMAT_OPUS: u32 = u32::from_be_bytes(*b"opus");
pub const FORMAT_LPCM: u32 = u32::from_be_bytes(*b"lpcm");
/// kAudioFormatFlagIsSignedInteger | kAudioFormatFlagIsPacked (native little-endian)
pub const PCM_S16_FLAGS: u32 = 4 | 8;
pub const QUEUE_PARAM_VOLUME: u32 = 1;

pub type InputProc = extern "C" fn(
    AudioConverterRef,
    *mut u32,
    *mut AudioBufferList,
    *mut *mut AudioStreamPacketDescription,
    *mut c_void,
) -> OSStatus;

pub type QueueCallback = extern "C" fn(*mut c_void, AudioQueueRef, AudioQueueBufferRef);

extern "C" {
    pub fn AudioConverterNew(
        src: *const AudioStreamBasicDescription,
        dst: *const AudioStreamBasicDescription,
        out: *mut AudioConverterRef,
    ) -> OSStatus;
    pub fn AudioConverterDispose(c: AudioConverterRef) -> OSStatus;
    pub fn AudioConverterReset(c: AudioConverterRef) -> OSStatus;
    pub fn AudioConverterFillComplexBuffer(
        c: AudioConverterRef,
        input: InputProc,
        user: *mut c_void,
        io_packets: *mut u32,
        out: *mut AudioBufferList,
        out_desc: *mut AudioStreamPacketDescription,
    ) -> OSStatus;

    pub fn AudioQueueNewOutput(
        format: *const AudioStreamBasicDescription,
        cb: QueueCallback,
        user: *mut c_void,
        run_loop: *const c_void,
        mode: *const c_void,
        flags: u32,
        out: *mut AudioQueueRef,
    ) -> OSStatus;
    pub fn AudioQueueAllocateBuffer(q: AudioQueueRef, bytes: u32, out: *mut AudioQueueBufferRef) -> OSStatus;
    pub fn AudioQueueEnqueueBuffer(q: AudioQueueRef, b: AudioQueueBufferRef, n: u32, d: *const c_void) -> OSStatus;
    pub fn AudioQueueStart(q: AudioQueueRef, start: *const c_void) -> OSStatus;
    pub fn AudioQueueStop(q: AudioQueueRef, immediate: u8) -> OSStatus;
    pub fn AudioQueueDispose(q: AudioQueueRef, immediate: u8) -> OSStatus;
    pub fn AudioQueueSetParameter(q: AudioQueueRef, param: u32, value: f32) -> OSStatus;
}
