//! The running Takion stream (spec 02 sections 7-9) as a sans-IO engine.
//!
//! Feed received UDP datagrams to [`Stream::handle_datagram`]; it returns [`Event`]s (decoded video frames,
//! Opus frames, haptics, console controller output, protocol messages). Datagrams to send accumulate and are
//! drained with [`Stream::poll_transmit`]. Call [`Stream::tick`] regularly (>= every 50 ms) for timers.
//!
//! Intentionally not here: the UDP socket, Opus/H.264 decoding, rendering, and the license-gate timer (excluded).

use crate::audio::{AudioAssembler, AudioUnits, HapticExtractor, OpusFrame, HAPTIC_UNIT_SIZE_THRESHOLD};
use crate::padblob::{self, PadOutput};
use crate::video::{Frame, Outcome, Unit, VideoAssembler};
use openps_crypto::{Context, KeyPosTracker};
use openps_input::state::ControllerState;
use openps_input::trace;
use openps_packets::{
    build_congestion, build_feedback, gmac_key_pos_offsets, packet_type, AudioPacket, FeedbackSeq, HistorySeq,
    PacketType, VideoPacket,
};
use openps_pad::PadFeedback;
use openps_takion::chunk::{chunk_type, class, Chunk, GMAC_OFFSET, KEY_POS_OFFSET};
use openps_takion::handshake::{KeyPosCounter, Session, HEARTBEAT_PAYLOAD};
use openps_takion::proto::{
    msg_type, ControllerConnectionPayload, CorruptFramePayload, DisconnectPayload, TakionMessage,
};
use std::collections::VecDeque;

pub const IDR_MIN_INTERVAL_S: f64 = 0.150;
pub const FIRST_RETRY_S: f64 = 0.066;
pub const RETRY_S: f64 = 0.150;
pub const MAX_RETRIES: u32 = 3;
/// A controller join is retried for much longer than an IDR request: PXPlay gives up after 15 tries (about 2.3 s).
pub const PAD_CONN_MAX_RETRIES: u32 = 15;
pub const CONGESTION_INTERVAL_S: f64 = 1.0;
const STATE_RESEND_S: f64 = 0.1;
pub const MAX_PADS: usize = 4;

#[derive(Clone, Debug)]
pub struct StreamConfig {
    pub ps5: bool,
    /// Treat PS5 audio packets with unit size > 80 as haptics.
    pub haptics: bool,
    /// Count audio packets for congestion only; do not decrypt or decode them.
    pub disable_audio: bool,
    /// `videoFrameBufferSize` (>= 2); the original app's default is 2 (`FEW`).
    pub video_window: usize,
    /// Send congestion packets (bitrate mode AUTO/START).
    pub congestion: bool,
}

impl Default for StreamConfig {
    fn default() -> Self {
        Self { ps5: true, haptics: true, disable_audio: false, video_window: 2, congestion: true }
    }
}

#[derive(Debug, PartialEq)]
pub enum Event {
    Video(Frame),
    Audio(Vec<OpusFrame>),
    Haptic([u8; 120]),
    PadOutput(PadOutput),
    /// Any other protobuf message from the console (INFO, TIMER, CONNECTION_QUALITY, SERVER_SETTINGS, ...).
    Message(Box<TakionMessage>),
    Disconnected(Option<String>),
}

struct Retry {
    seq: u32,
    class: u32,
    payload: Vec<u8>,
    next_at: f64,
    resends: u32,
    max_resends: u32,
}

/// Loss estimate for the congestion packet (spec 7.3): ratio 0 for the first sample, the combined ratio for
/// the second, then each sample's own.
#[derive(Default)]
struct Congestion {
    n: u32,
    r1: u32,
    m1: u32,
}
impl Congestion {
    fn lost(&mut self, received: u32, missed: u32) -> u16 {
        self.n += 1;
        let ratio = match self.n {
            1 => {
                self.r1 = received;
                self.m1 = missed;
                0.0
            }
            2 => {
                let (r, m) = (self.r1 + received, self.m1 + missed);
                if r == 0 {
                    0.0
                } else {
                    m as f64 / r as f64
                }
            }
            _ => {
                if received == 0 {
                    0.0
                } else {
                    missed as f64 / received as f64
                }
            }
        };
        (received as f64 * ratio) as u32 as u16
    }
}

pub struct Stream {
    cfg: StreamConfig,
    t20: bool,
    version: i32,
    tag: u32,
    next_tsn: u32,
    local: Context,
    remote: Context,
    key_pos: KeyPosCounter,
    video_pos: KeyPosTracker,
    audio_pos: KeyPosTracker,
    haptic_pos: KeyPosTracker,
    video: VideoAssembler,
    audio: AudioAssembler,
    haptic: HapticExtractor,
    out: VecDeque<Vec<u8>>,
    pads: Vec<Option<PadFeedback>>,
    feedback_seq: FeedbackSeq,
    history_seq: HistorySeq,
    idr: Option<Retry>,
    last_idr_at: f64,
    pad_conn: [Option<Retry>; MAX_PADS],
    congestion: Congestion,
    next_congestion_at: f64,
    /// Last tick time, and when each pad's state packet was last sent (a lost one must not leave a stick stuck).
    clock: f64,
    state_sent_at: [f64; MAX_PADS],
    started_at: Option<f64>,
    recv_seq: SeqWindow,
    /// Sequences of the history packets that were traced when built (only used with `OPENPS_INPUT_TRACE`), so the session can
    /// log what the socket did with exactly those and not with the idle repeats.
    traced_history: VecDeque<u16>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HistoryCause {
    Change,
    Idle,
}

/// Sliding window of the last 64 received DATA sequence numbers (32-bit wrapping) for de-duplication.
#[derive(Default)]
struct SeqWindow {
    hi: Option<u32>,
    mask: u64, // bit i set = `hi - i` was seen
}

impl SeqWindow {
    /// Record `seq`; false if it was already seen. A sequence more than 63 behind the window is processed
    /// (not recorded) rather than suppressed, so a bogus jump can never silence the real stream.
    fn is_new(&mut self, seq: u32) -> bool {
        let Some(hi) = self.hi else {
            self.hi = Some(seq);
            self.mask = 1;
            return true;
        };
        let d = seq.wrapping_sub(hi) as i32;
        if d > 0 {
            self.mask = if d >= 64 { 1 } else { (self.mask << d) | 1 };
            self.hi = Some(seq);
            true
        } else {
            let back = d.unsigned_abs();
            if back >= 64 {
                return true;
            }
            let bit = 1u64 << back;
            let fresh = self.mask & bit == 0;
            self.mask |= bit;
            fresh
        }
    }
}

impl Stream {
    pub fn new(session: Session, cfg: StreamConfig) -> Self {
        let t20 = session.takion_version >= 20;
        let video = VideoAssembler::new(cfg.video_window);
        Self {
            cfg,
            t20,
            version: session.takion_version,
            tag: session.remote_tag,
            next_tsn: session.next_tsn,
            local: session.local,
            remote: session.remote,
            key_pos: session.key_pos,
            video_pos: KeyPosTracker::default(),
            audio_pos: KeyPosTracker::default(),
            haptic_pos: KeyPosTracker::default(),
            video,
            audio: AudioAssembler::new(),
            haptic: HapticExtractor::new(),
            out: VecDeque::new(),
            pads: (0..MAX_PADS).map(|_| None).collect(),
            feedback_seq: FeedbackSeq::default(),
            history_seq: HistorySeq::default(),
            idr: None,
            last_idr_at: f64::NEG_INFINITY,
            pad_conn: [None, None, None, None],
            congestion: Congestion::default(),
            next_congestion_at: 0.0,
            clock: 0.0,
            state_sent_at: [0.0; MAX_PADS],
            started_at: None,
            recv_seq: SeqWindow::default(),
            traced_history: VecDeque::new(),
        }
    }

    pub fn poll_transmit(&mut self) -> Option<Vec<u8>> {
        self.out.pop_front()
    }
    pub fn takion_version(&self) -> i32 {
        self.version
    }

    /// True once for each history packet whose construction was traced; the session uses it to trace the same packet's send.
    pub fn take_traced_history(&mut self, seq: u16) -> bool {
        match self.traced_history.iter().position(|s| *s == seq) {
            Some(i) => self.traced_history.remove(i).is_some(),
            None => false,
        }
    }

    // ---- outbound primitives ------------------------------------------------------------------
    fn tsn(&mut self) -> u32 {
        let v = self.next_tsn;
        self.next_tsn = self.next_tsn.wrapping_add(1);
        v
    }

    /// Control packet with GMAC computed first (gmac/keyPos zero in the AAD) and keyPos written afterwards.
    fn protect_control(&mut self, c: &Chunk) -> Vec<u8> {
        let kp = self.key_pos.allocate();
        let mut p = c.encode();
        let tag = self.local.gmac(kp, &p);
        p[GMAC_OFFSET..GMAC_OFFSET + 4].copy_from_slice(&tag);
        p[KEY_POS_OFFSET..KEY_POS_OFFSET + 4].copy_from_slice(&kp.to_be_bytes());
        p
    }

    fn send_data(&mut self, class: u32, payload: Vec<u8>) -> u32 {
        let seq = self.tsn();
        let p = self.protect_control(&Chunk::data(self.tag, seq, class, payload));
        self.out.push_back(p);
        seq
    }

    fn send_message(&mut self, class: u32, m: &TakionMessage) -> u32 {
        self.send_data(class, m.to_chunk_payload())
    }

    fn send_ack(&mut self, seq: u32) {
        let p = self.protect_control(&Chunk::ack(self.tag, seq));
        self.out.push_back(p);
    }

    // ---- inbound ---------------------------------------------------------------------------------------
    pub fn handle_datagram(&mut self, now: f64, data: &[u8]) -> Vec<Event> {
        if self.started_at.is_none() {
            self.started_at = Some(now);
            self.next_congestion_at = now + CONGESTION_INTERVAL_S;
        }
        if data.is_empty() {
            return vec![];
        }
        match packet_type(data[0]) {
            PacketType::Control => self.on_control(now, data),
            PacketType::Video => self.on_video(now, data),
            PacketType::Audio => self.on_audio(data),
            _ => vec![],
        }
    }

    fn on_control(&mut self, _now: f64, data: &[u8]) -> Vec<Event> {
        let Some(c) = Chunk::decode(data) else { return vec![] };
        let mut ev = vec![];
        match c.chunk_type {
            chunk_type::DATA => {
                self.send_ack(c.sequence); // every received DATA chunk is acknowledged
                if !self.recv_seq.is_new(c.sequence) {
                    return ev; // retransmission of a chunk already processed
                }
                if c.flags != 1 {
                    return ev;
                }
                match c.payload_kind {
                    Some(0) => {
                        let Some(m) = TakionMessage::parse(&c.payload) else { return ev };
                        match m.r#type {
                            Some(msg_type::HEARTBEAT) => {
                                self.send_data(class::GENERAL, HEARTBEAT_PAYLOAD.to_vec());
                            }
                            Some(msg_type::DISCONNECT) => {
                                ev.push(Event::Disconnected(m.disconnect.and_then(|d| d.reason)))
                            }
                            _ => ev.push(Event::Message(Box::new(m))),
                        }
                    }
                    Some(k @ (7 | 9 | 11)) => {
                        ev.extend(padblob::parse(k, &c.payload).into_iter().map(Event::PadOutput))
                    }
                    _ => {}
                }
            }
            chunk_type::DATA_ACK => {
                if self.idr.as_ref().is_some_and(|r| r.seq == c.sequence) {
                    self.idr = None;
                }
                for slot in self.pad_conn.iter_mut() {
                    if slot.as_ref().is_some_and(|r| r.seq == c.sequence) {
                        *slot = None;
                    }
                }
            }
            _ => {}
        }
        ev
    }

    fn on_video(&mut self, now: f64, data: &[u8]) -> Vec<Event> {
        let Ok(p) = VideoPacket::parse(data, self.t20) else { return vec![] };
        let mut payload = p.payload.to_vec();
        let off = self.video_pos.offset(p.key_pos);
        self.remote.ctr_xor(off, &mut payload);
        let before = self.video.cursor();
        let unit = Unit {
            frame_id: p.frame_id,
            unit_index: p.unit_index,
            total_units: p.total_units,
            fec_units: p.fec_units,
            adaptive_index: p.adaptive_stream_index,
            payload: &payload,
        };
        let outcome = self.video.push(&unit);
        let after = self.video.cursor();
        if outcome == Outcome::DeliveredWithGap && after > 0 {
            log::info!("video loss: frames {before}..{after} skipped at {now:.3} s, CORRUPT_FRAME sent");
            self.send_corrupt_frame(before, after);
        }
        if self.video.take_resync() {
            // the frame-id space no longer matches the console's: start over and ask for a keyframe
            self.request_idr(now);
        }
        let mut ev = vec![];
        while let Some(f) = self.video.pop_frame() {
            ev.push(Event::Video(f));
        }
        ev
    }

    fn on_audio(&mut self, data: &[u8]) -> Vec<Event> {
        let Ok(p) = AudioPacket::parse(data, self.t20) else { return vec![] };
        let is_haptic = self.cfg.ps5 && self.cfg.haptics && p.unit_size as usize > HAPTIC_UNIT_SIZE_THRESHOLD;
        if !is_haptic && self.cfg.disable_audio {
            self.audio.note_unit_ids(p.frame_id, p.source_units);
            return vec![];
        } // counted for congestion only, never decrypted or decoded
        let mut payload = p.payload.to_vec();
        let off = if is_haptic { self.haptic_pos.offset(p.key_pos) } else { self.audio_pos.offset(p.key_pos) };
        self.remote.ctr_xor(off, &mut payload);
        let u = AudioUnits {
            frame_id: p.frame_id,
            total_units: p.total_units,
            source_units: p.source_units,
            redundant_units: p.redundant_units,
            unit_size: p.unit_size as usize,
            payload: &payload,
        };
        if is_haptic {
            return self.haptic.push(&u).map(Event::Haptic).into_iter().collect();
        }
        let frames = self.audio.push(&u);
        if frames.is_empty() {
            vec![]
        } else {
            vec![Event::Audio(frames)]
        }
    }

    // ---- client requests -----------------------------------------------------------------------------------
    fn send_corrupt_frame(&mut self, start: i32, end: i32) {
        let mut m = TakionMessage::of_type(msg_type::CORRUPT_FRAME);
        m.corrupt = Some(CorruptFramePayload { start: Some(start), end: Some(end) });
        self.send_message(class::CORRUPT_FRAME, &m); // no retry/ack tracking
    }

    /// Ask for a keyframe. At most one per 150 ms unless the previous one was acknowledged; retried at
    /// 66 ms then every 150 ms, up to 3 resends, until its DATA_ACK arrives.
    pub fn request_idr(&mut self, now: f64) {
        if self.idr.is_some() && now - self.last_idr_at < IDR_MIN_INTERVAL_S {
            return;
        }
        self.last_idr_at = now;
        let payload = TakionMessage::of_type(msg_type::IDR_REQUEST).to_chunk_payload();
        let seq = self.send_data(class::GENERAL, payload.clone());
        self.idr = Some(Retry {
            seq,
            class: class::GENERAL,
            payload,
            next_at: now + FIRST_RETRY_S,
            resends: 0,
            max_resends: MAX_RETRIES,
        });
    }

    /// Announce a pad joining (controller type 6) or leaving (0); retried like the IDR request until acked.
    pub fn set_pad_connected(&mut self, now: f64, pad: usize, connected: bool) {
        if pad >= MAX_PADS {
            return;
        }
        let mut m = TakionMessage::of_type(msg_type::CONTROLLER_CONNECTION);
        m.controller_connection = Some(ControllerConnectionPayload {
            controller_id: Some(pad as i32),
            connected: Some(connected),
            controller_type: Some(if connected { 6 } else { 0 }),
        });
        let payload = m.to_chunk_payload();
        let seq = self.send_data(class::GENERAL, payload.clone());
        self.pad_conn[pad] = Some(Retry {
            seq,
            class: class::GENERAL,
            payload,
            next_at: now + FIRST_RETRY_S,
            resends: 0,
            max_resends: PAD_CONN_MAX_RETRIES,
        });
        let p = self.pads[pad].get_or_insert_with(|| PadFeedback::new(pad as u8));
        p.presence(connected);
    }

    /// 3x DISCONNECT with the app's reason string.
    pub fn disconnect(&mut self) {
        let mut m = TakionMessage::of_type(msg_type::DISCONNECT);
        m.disconnect = Some(DisconnectPayload { reason: Some("Client Disconnecting".into()), extended_info: None });
        for _ in 0..3 {
            self.send_message(class::GENERAL, &m);
        }
    }

    // ---- controller feedback ---------------------------------------------------------------------------------
    /// Feed the newest pad state. Sends a FEEDBACK packet when sticks/sensors changed and a FEEDBACK_HISTORY
    /// packet when new button/touch events are queued.
    pub fn update_pad(&mut self, now: f64, pad: usize, st: &ControllerState) {
        if pad >= MAX_PADS {
            return;
        }
        let p = self.pads[pad].get_or_insert_with(|| PadFeedback::new(pad as u8));
        let u = p.update(st, now);
        if trace::enabled() {
            log::info!(
                "input_trace t={} stage=feedback_encode pad={pad} state_changed={} history_changed={} {}",
                trace::now_us(),
                u.state_changed,
                u.history_changed,
                trace::brief(st)
            );
        }
        if u.state_changed {
            self.send_feedback_state(pad);
        }
        if u.history_changed {
            self.send_feedback_history(now, pad, HistoryCause::Change);
        }
    }

    /// Sensor data from a physical pad (gyro rad/s, accel g, orientation quaternion).
    pub fn update_sensors(&mut self, pad: usize, gyro: [f32; 3], accel: [f32; 3], orientation: [f32; 4]) {
        if pad >= MAX_PADS {
            return;
        }
        self.pads[pad].get_or_insert_with(|| PadFeedback::new(pad as u8)).set_sensors(gyro, accel, orientation);
        self.send_feedback_state(pad);
    }

    /// Protects and queues one feedback packet; returns what identifies it on the wire (keyPos, length, GMAC).
    fn send_feedback(&mut self, kind: u8, seq: u16, payload: Vec<u8>) -> (u32, usize, [u8; 4]) {
        let ptype = if kind == 6 { PacketType::Feedback } else { PacketType::FeedbackHistory };
        let (gmac_off, kp_off) = gmac_key_pos_offsets(ptype, self.t20).expect("feedback offsets");
        let hdr = if self.t20 { 20 } else { 12 };
        let kp = self.key_pos.allocate();
        let mut p = build_feedback(kind, seq, self.t20, &payload);
        self.local.ctr_xor(kp as u64 + 16, &mut p[hdr..]); // payload encrypted at keyPos + 16
        p[kp_off..kp_off + 4].copy_from_slice(&kp.to_be_bytes()); // keyPos is part of the GMAC input here
        let tag = self.local.gmac(kp, &p);
        p[gmac_off..gmac_off + 4].copy_from_slice(&tag);
        let len = p.len();
        self.out.push_back(p);
        (kp, len, tag)
    }

    fn send_feedback_state(&mut self, pad: usize) {
        let v = self.version as u32;
        self.state_sent_at[pad] = self.clock;
        let payload = self.pads[pad].as_mut().unwrap().state_payload(v);
        let seq = self.feedback_seq.next_seq();
        self.send_feedback(6, seq, payload);
    }

    /// Why a history packet is being sent. Only `Change` is traced: the idle repeat would fill the log with identical
    /// lines ten times a second and rotate the interesting part away.
    fn send_feedback_history(&mut self, now: f64, pad: usize, cause: HistoryCause) {
        let packets = if cause == HistoryCause::Change {
            // The console applies one new event per history packet: send one packet per event, oldest first.
            self.pads[pad].as_mut().unwrap().take_history_packets(now)
        } else {
            vec![self.pads[pad].as_mut().unwrap().history_payload(now)]
        };
        for payload in packets.into_iter().filter(|p| !p.is_empty()) {
            self.send_history_packet(pad, payload, cause);
        }
    }

    fn send_history_packet(&mut self, pad: usize, payload: Vec<u8>, cause: HistoryCause) {
        let seq = self.history_seq.next_seq();
        let traced = trace::enabled() && cause == HistoryCause::Change;
        let hex = traced.then(|| payload.iter().map(|b| format!("{b:02x}")).collect::<String>());
        let (key_pos, len, gmac) = self.send_feedback(1, seq, payload);
        if let Some(hex) = hex {
            if self.traced_history.len() >= 64 {
                self.traced_history.pop_front();
            }
            self.traced_history.push_back(seq);
            log::info!(
                "input_trace t={} stage=wire_protect kind=history cause={cause:?} pad={pad} seq={seq} key_pos={key_pos} len={len} gmac={:02x}{:02x}{:02x}{:02x} payload={hex}",
                trace::now_us(),
                gmac[0],
                gmac[1],
                gmac[2],
                gmac[3]
            );
        }
    }

    // ---- timers ---------------------------------------------------------------------------------------------------
    pub fn tick(&mut self, now: f64) {
        // retries (IDR request, controller connection)
        let mut due: Vec<(u32, u32, Vec<u8>)> = vec![];
        for r in self.idr.iter_mut().chain(self.pad_conn.iter_mut().flatten()) {
            if now >= r.next_at {
                if r.resends >= r.max_resends {
                    r.seq = u32::MAX;
                    continue;
                }
                r.resends += 1;
                r.next_at = now + RETRY_S;
                due.push((r.seq, r.class, r.payload.clone()));
            }
        }
        if self.idr.as_ref().is_some_and(|r| r.seq == u32::MAX) {
            self.idr = None;
        }
        for slot in self.pad_conn.iter_mut() {
            if slot.as_ref().is_some_and(|r| r.seq == u32::MAX) {
                *slot = None;
            }
        }
        for (seq, class, payload) in due {
            let p = self.protect_control(&Chunk::data(self.tag, seq, class, payload)); // same sequence, fresh keyPos
            self.out.push_back(p);
        }
        self.clock = now;
        // the state packet is repeated while idle, like the history, so one lost datagram cannot leave a stick stuck
        for pad in 0..MAX_PADS {
            if self.pads[pad].is_some() && now - self.state_sent_at[pad] >= STATE_RESEND_S {
                self.send_feedback_state(pad);
            }
        }
        // history self-heals every 100 ms
        for pad in 0..MAX_PADS {
            if self.pads[pad].as_ref().is_some_and(|p| p.history_due(now)) {
                self.send_feedback_history(now, pad, HistoryCause::Idle);
            }
        }
        // congestion report once per second
        if self.started_at.is_some() && now >= self.next_congestion_at {
            self.next_congestion_at = now + CONGESTION_INTERVAL_S;
            let (vr, vm) = self.video.poll_telemetry(now).unwrap_or((0, 0));
            let (ar, am) = self.audio.take_sample();
            if self.cfg.congestion {
                let received = vr + ar;
                let lost = self.congestion.lost(received, vm + am);
                self.send_congestion(received as u16, lost);
            }
        }
    }

    fn send_congestion(&mut self, received: u16, lost: u16) {
        let (gmac_off, kp_off) = gmac_key_pos_offsets(PacketType::Congestion, self.t20).unwrap();
        let mut p = build_congestion(received, lost).to_vec();
        let kp = self.key_pos.allocate();
        let tag = self.local.gmac(kp, &p); // keyPos is zero in the GMAC input; payload is not encrypted
        p[gmac_off..gmac_off + 4].copy_from_slice(&tag);
        p[kp_off..kp_off + 4].copy_from_slice(&kp.to_be_bytes());
        self.out.push_back(p);
    }
}
