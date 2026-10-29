//! Takion protobuf (proto2, every field optional). Field numbers from the decompiled `Takion$*` classes
//! (spec 02 section 2). Unknown fields are skipped on decode. `TakionMessage.type` is the raw enum value.

use prost::Message;

/// `TakionMessage.Type` wire values (spec 02 R8; only the ones the client sends or branches on are named).
pub mod msg_type {
    pub const BIG: i32 = 0;
    pub const BANG: i32 = 1;
    pub const HEARTBEAT: i32 = 3;
    pub const CORRUPT_FRAME: i32 = 5;
    pub const DISCONNECT: i32 = 8;
    pub const SENKUSHA: i32 = 12;
    pub const STREAM_INFO: i32 = 13;
    pub const STREAM_INFO_ACK: i32 = 14;
    pub const CONTROLLER_CONNECTION: i32 = 21;
    pub const IDR_REQUEST: i32 = 25;
    pub const TAKION_PROTOCOL_REQUEST: i32 = 31;
    pub const TAKION_PROTOCOL_REQUEST_ACK: i32 = 32;
    pub const AUDIO_STATE: i32 = 33;
}

#[derive(Clone, PartialEq, Message)]
pub struct TakionMessage {
    #[prost(int32, optional, tag = "1")]
    pub r#type: Option<i32>,
    #[prost(message, optional, tag = "2")]
    pub big: Option<BigPayload>,
    #[prost(message, optional, tag = "3")]
    pub bang: Option<BangPayload>,
    #[prost(message, optional, tag = "4")]
    pub info: Option<InfoPayload>,
    #[prost(message, optional, tag = "5")]
    pub loss: Option<PacketLossPayload>,
    #[prost(message, optional, tag = "6")]
    pub corrupt: Option<CorruptFramePayload>,
    #[prost(message, optional, tag = "9")]
    pub timer: Option<TimerPayload>,
    #[prost(message, optional, tag = "10")]
    pub disconnect: Option<DisconnectPayload>,
    #[prost(message, optional, tag = "12")]
    pub header_request: Option<HeaderRequestPayload>,
    #[prost(message, optional, tag = "14")]
    pub senkusha: Option<SenkushaPayload>,
    #[prost(message, optional, tag = "15")]
    pub stream_info: Option<StreamInfoPayload>,
    #[prost(message, optional, tag = "17")]
    pub connection_quality: Option<ConnectionQualityPayload>,
    #[prost(message, optional, tag = "19")]
    pub play_time_left: Option<PlayTimeLeftPayload>,
    #[prost(message, optional, tag = "20")]
    pub server_message: Option<ServerMessagePayload>,
    #[prost(message, optional, tag = "21")]
    pub fps_change: Option<FpsChangePayload>,
    #[prost(message, optional, tag = "22")]
    pub controller_connection: Option<ControllerConnectionPayload>,
    #[prost(message, optional, tag = "23")]
    pub client_info: Option<ClientInfoPayload>,
    #[prost(message, optional, tag = "28")]
    pub server_settings: Option<ServerSettingsPayload>,
    #[prost(message, optional, tag = "31")]
    pub protocol_request: Option<TakionProtocolRequestPayload>,
    #[prost(message, optional, tag = "32")]
    pub protocol_request_ack: Option<TakionProtocolRequestAckPayload>,
    #[prost(message, optional, tag = "33")]
    pub audio_state: Option<AudioStatePayload>,
}

impl TakionMessage {
    pub fn of_type(t: i32) -> Self {
        Self { r#type: Some(t), ..Default::default() }
    }
    /// `0x00` (payload kind: protobuf) + the serialised message, as carried in a DATA chunk.
    pub fn to_chunk_payload(&self) -> Vec<u8> {
        let mut v = vec![0u8];
        v.extend(self.encode_to_vec());
        v
    }
    /// Parse a protobuf payload (without the leading kind byte).
    pub fn parse(b: &[u8]) -> Option<Self> {
        Self::decode(b).ok()
    }
}

#[derive(Clone, PartialEq, Message)]
pub struct BigPayload {
    #[prost(int32, optional, tag = "1")]
    pub client_version: Option<i32>,
    #[prost(string, optional, tag = "2")]
    pub session_key: Option<String>,
    #[prost(bytes = "vec", optional, tag = "3")]
    pub launch_spec: Option<Vec<u8>>,
    #[prost(bytes = "vec", optional, tag = "4")]
    pub encrypted_key: Option<Vec<u8>>,
    #[prost(bytes = "vec", optional, tag = "5")]
    pub ecdh_pub_key: Option<Vec<u8>>,
    #[prost(bytes = "vec", optional, tag = "6")]
    pub ecdh_sig: Option<Vec<u8>>,
}

#[derive(Clone, PartialEq, Message)]
pub struct EventCode {
    #[prost(string, optional, tag = "1")]
    pub event_code: Option<String>,
    #[prost(string, optional, tag = "2")]
    pub name: Option<String>,
    #[prost(string, optional, tag = "3")]
    pub description: Option<String>,
}

#[derive(Clone, PartialEq, Message)]
pub struct BangPayload {
    #[prost(int32, optional, tag = "1")]
    pub server_version: Option<i32>,
    #[prost(int32, optional, tag = "2")]
    pub token: Option<i32>,
    #[prost(bool, optional, tag = "3")]
    pub encrypted_key_accepted: Option<bool>,
    #[prost(bool, optional, tag = "4")]
    pub version_accepted: Option<bool>,
    #[prost(string, optional, tag = "5")]
    pub session_key: Option<String>,
    #[prost(message, optional, tag = "6")]
    pub extended_info: Option<EventCode>,
    #[prost(string, optional, tag = "7")]
    pub server_version_string: Option<String>,
    #[prost(bytes = "vec", optional, tag = "8")]
    pub ecdh_pub_key: Option<Vec<u8>>,
    #[prost(bytes = "vec", optional, tag = "9")]
    pub ecdh_sig: Option<Vec<u8>>,
}

#[derive(Clone, PartialEq, Message)]
pub struct InfoPayload {
    #[prost(int32, optional, tag = "1")]
    pub effective_bw: Option<i32>,
    #[prost(int32, optional, tag = "2")]
    pub bitrate: Option<i32>,
    #[prost(int32, optional, tag = "3")]
    pub min_bitrate: Option<i32>,
    #[prost(int32, optional, tag = "4")]
    pub target_bitrate: Option<i32>,
    #[prost(int32, optional, tag = "5")]
    pub corrupt_frame_freq: Option<i32>,
    #[prost(int32, optional, tag = "6")]
    pub mtu: Option<i32>,
    #[prost(int32, optional, tag = "7")]
    pub fps: Option<i32>,
    #[prost(int32, optional, tag = "8")]
    pub monitor_interval: Option<i32>,
    #[prost(int32, optional, tag = "9")]
    pub ext_overhead: Option<i32>,
    #[prost(int32, optional, tag = "10")]
    pub int_overhead: Option<i32>,
    #[prost(int32, optional, tag = "11")]
    pub timeout_interval: Option<i32>,
    #[prost(int32, optional, tag = "12")]
    pub min_resume_quality: Option<i32>,
    #[prost(int32, optional, tag = "13")]
    pub min_fps: Option<i32>,
}

#[derive(Clone, PartialEq, Message)]
pub struct PacketLossPayload {
    #[prost(int32, optional, tag = "1")]
    pub start: Option<i32>,
    #[prost(int32, optional, tag = "2")]
    pub end: Option<i32>,
    #[prost(int32, optional, tag = "3")]
    pub packets_received: Option<i32>,
}

#[derive(Clone, PartialEq, Message)]
pub struct CorruptFramePayload {
    #[prost(int32, optional, tag = "1")]
    pub start: Option<i32>,
    #[prost(int32, optional, tag = "2")]
    pub end: Option<i32>,
}

#[derive(Clone, PartialEq, Message)]
pub struct TimerPayload {
    #[prost(int32, optional, tag = "1")]
    pub seconds_left: Option<i32>,
    #[prost(int32, optional, tag = "2")]
    pub total_seconds: Option<i32>,
}

#[derive(Clone, PartialEq, Message)]
pub struct DisconnectPayload {
    #[prost(string, optional, tag = "1")]
    pub reason: Option<String>,
    #[prost(message, optional, tag = "2")]
    pub extended_info: Option<EventCode>,
}

#[derive(Clone, PartialEq, Message)]
pub struct HeaderRequestPayload {
    #[prost(int32, optional, tag = "1")]
    pub r#type: Option<i32>,
}

#[derive(Clone, PartialEq, Message)]
pub struct ResolutionPayload {
    #[prost(int32, optional, tag = "1")]
    pub width: Option<i32>,
    #[prost(int32, optional, tag = "2")]
    pub height: Option<i32>,
    /// Annex-B parameter sets (VPS/SPS/PPS or SPS/PPS) with 00 00 00 01 start codes.
    #[prost(bytes = "vec", optional, tag = "3")]
    pub video_header: Option<Vec<u8>>,
}

#[derive(Clone, PartialEq, Message)]
pub struct AudioChannelPayload {
    #[prost(int32, optional, tag = "1")]
    pub audio_channel_type: Option<i32>,
    #[prost(bytes = "vec", optional, tag = "2")]
    pub audio_header: Option<Vec<u8>>,
    #[prost(bool, optional, tag = "3")]
    pub is_raw_pcm: Option<bool>,
}

#[derive(Clone, PartialEq, Message)]
pub struct StreamInfoPayload {
    #[prost(message, repeated, tag = "1")]
    pub resolution: Vec<ResolutionPayload>,
    /// 14 bytes, see spec 02 section 8.1.
    #[prost(bytes = "vec", optional, tag = "2")]
    pub audio_header: Option<Vec<u8>>,
    #[prost(int32, optional, tag = "3")]
    pub start_timeout: Option<i32>,
    #[prost(int32, optional, tag = "4")]
    pub afk_timeout: Option<i32>,
    #[prost(int32, optional, tag = "5")]
    pub afk_timeout_disconnect: Option<i32>,
    #[prost(int32, optional, tag = "6")]
    pub congestion_control_interval: Option<i32>,
    #[prost(message, repeated, tag = "7")]
    pub audio_channels: Vec<AudioChannelPayload>,
}

#[derive(Clone, PartialEq, Message)]
pub struct AudioStatePayload {
    #[prost(int32, optional, tag = "1")]
    pub audio_state_type: Option<i32>,
    #[prost(bytes = "vec", optional, tag = "2")]
    pub audio_state_data: Option<Vec<u8>>,
}

#[derive(Clone, PartialEq, Message)]
pub struct ConnectionQualityPayload {
    #[prost(int32, optional, tag = "1")]
    pub target_bitrate: Option<i32>,
    #[prost(int32, optional, tag = "2")]
    pub upstream_bitrate: Option<i32>,
    #[prost(float, optional, tag = "3")]
    pub upstream_loss: Option<f32>,
    #[prost(bool, optional, tag = "4")]
    pub disable_upstream_audio: Option<bool>,
    #[prost(double, optional, tag = "5")]
    pub rtt: Option<f64>,
    #[prost(int64, optional, tag = "6")]
    pub loss: Option<i64>,
    #[prost(double, optional, tag = "7")]
    pub loss_percent: Option<f64>,
}

#[derive(Clone, PartialEq, Message)]
pub struct PlayTimeLeftPayload {
    #[prost(int32, optional, tag = "1")]
    pub timeleft: Option<i32>,
    #[prost(string, optional, tag = "2")]
    pub reason: Option<String>,
}

#[derive(Clone, PartialEq, Message)]
pub struct ServerMessagePayload {
    #[prost(string, optional, tag = "1")]
    pub payload: Option<String>,
}

#[derive(Clone, PartialEq, Message)]
pub struct FpsChangePayload {
    #[prost(int32, optional, tag = "1")]
    pub fps: Option<i32>,
}

/// `controller_type`: client sends 6 on connect and 0 on disconnect/leave (spec 02 section 2.2).
#[derive(Clone, PartialEq, Message)]
pub struct ControllerConnectionPayload {
    #[prost(int32, optional, tag = "1")]
    pub controller_id: Option<i32>,
    #[prost(bool, optional, tag = "2")]
    pub connected: Option<bool>,
    #[prost(int32, optional, tag = "3")]
    pub controller_type: Option<i32>,
}

#[derive(Clone, PartialEq, Message)]
pub struct ClientInfoPayload {
    #[prost(string, optional, tag = "1")]
    pub session_key: Option<String>,
    #[prost(int32, optional, tag = "2")]
    pub gcm_tag: Option<i32>,
    #[prost(int32, optional, tag = "3")]
    pub key_pos: Option<i32>,
}

#[derive(Clone, PartialEq, Message)]
pub struct ServerSettingsPayload {
    #[prost(int32, optional, tag = "1")]
    pub idr_period: Option<i32>,
    #[prost(bool, optional, tag = "2")]
    pub send_periodic_timestamp: Option<bool>,
    #[prost(bool, optional, tag = "3")]
    pub enable_smooth_playback: Option<bool>,
}

#[derive(Clone, PartialEq, Message)]
pub struct TakionProtocolRequestPayload {
    #[prost(int32, repeated, packed = "false", tag = "1")]
    pub supported_takion_versions: Vec<i32>,
}

#[derive(Clone, PartialEq, Message)]
pub struct TakionProtocolRequestAckPayload {
    #[prost(int32, optional, tag = "1")]
    pub takion_protocol_version: Option<i32>,
}

// ---- Senkusha (UDP 9297 network test) ----
#[derive(Clone, PartialEq, Message)]
pub struct EchoCommand {
    #[prost(bool, optional, tag = "1")]
    pub state: Option<bool>,
}

#[derive(Clone, PartialEq, Message)]
pub struct MtuCommand {
    #[prost(int32, optional, tag = "1")]
    pub id: Option<i32>,
    #[prost(int32, optional, tag = "2")]
    pub mtu_req: Option<i32>,
    #[prost(int32, optional, tag = "3")]
    pub mtu_sent: Option<i32>,
    #[prost(int32, optional, tag = "4")]
    pub num: Option<i32>,
    #[prost(int32, optional, tag = "5")]
    pub send_delay: Option<i32>,
    #[prost(int32, optional, tag = "6")]
    pub delta: Option<i32>,
}

#[derive(Clone, PartialEq, Message)]
pub struct BandwidthCommand {
    #[prost(int32, optional, tag = "1")]
    pub id: Option<i32>,
    #[prost(int32, optional, tag = "2")]
    pub time: Option<i32>,
    #[prost(int32, optional, tag = "3")]
    pub bandwidth: Option<i32>,
    #[prost(int32, optional, tag = "4")]
    pub fps: Option<i32>,
    #[prost(int32, optional, tag = "5")]
    pub mtu: Option<i32>,
    #[prost(int32, optional, tag = "6")]
    pub nalu_count: Option<i32>,
    #[prost(int32, optional, tag = "7")]
    pub total_sent: Option<i32>,
    #[prost(bool, optional, tag = "8")]
    pub upstream: Option<bool>,
    #[prost(double, optional, tag = "9")]
    pub loss: Option<f64>,
}

#[derive(Clone, PartialEq, Message)]
pub struct ClientMtuCommand {
    #[prost(int32, optional, tag = "1")]
    pub id: Option<i32>,
    #[prost(int32, optional, tag = "2")]
    pub mtu_req: Option<i32>,
    #[prost(bool, optional, tag = "3")]
    pub state: Option<bool>,
    #[prost(int32, optional, tag = "4")]
    pub mtu_down: Option<i32>,
}

/// `command`: 0 = ECHO, 1 = MTU, 3 = BANDWIDTH, 4 = CLIENT_MTU (spec 02 section 2.4).
#[derive(Clone, PartialEq, Message)]
pub struct SenkushaPayload {
    #[prost(int32, optional, tag = "1")]
    pub command: Option<i32>,
    #[prost(message, optional, tag = "2")]
    pub echo: Option<EchoCommand>,
    #[prost(message, optional, tag = "3")]
    pub mtu: Option<MtuCommand>,
    #[prost(message, optional, tag = "4")]
    pub bandwidth: Option<BandwidthCommand>,
    #[prost(message, optional, tag = "5")]
    pub client_mtu: Option<ClientMtuCommand>,
}
