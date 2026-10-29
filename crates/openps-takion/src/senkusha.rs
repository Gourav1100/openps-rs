//! Senkusha: the pre-stream network test on UDP 9297 (spec 02 section 5.4), as a sans-IO state machine.
//!
//! It measures round-trip time and the path MTU in both directions, which the launch spec and the Takion
//! handshake then use. Same chunk framing and INIT/COOKIE steps as the main handshake, but unauthenticated.
//!
//! Sequence (each request is "send, wait for the matching reply, up to 4 sends"): INIT, COOKIE, [PS5: protocol
//! request offering only version 9], Big/Bang (no keys), echo on, 10 RTT probes, echo off, downlink MTU binary
//! search 576..1454, uplink MTU binary search, final client-MTU report, disconnect. All waits use the 100 ms socket
//! timeout, except that the MTU phase switches it to `clamp(5 x RTT, 10, 500)` ms and keeps it.
//!
//! Details taken from CFR output where the other decompiler mis-rendered them: the RTT is the trimmed mean of the
//! probe samples (not of their deviations), and the outlier filter `|x - mean| <= 2 * variance` (variance in ns^2)
//! effectively keeps everything; both are reproduced as written.

use crate::chunk::{chunk_type, class, Chunk};
use crate::proto::*;
use std::collections::VecDeque;

pub const INITIAL_TSN: u32 = 0x4823;
pub const MTU_MIN: i32 = 576;
pub const MTU_MAX: i32 = 1454;
pub const RTT_PROBES: u32 = 10;
pub const RTT_PROBE_LEN: usize = 548;
const SENKUSHA_VERSION_PS4: i32 = 7;

#[derive(Clone, Debug)]
pub struct SenkushaConfig {
    pub ps5: bool,
    /// Socket receive timeout used for every wait until the MTU phase (the app: 100 ms).
    pub io_timeout_s: f64,
    /// Sends per request before giving up on it (the app: 4).
    pub request_sends: u32,
    /// Whole-test attempts (the app: 3).
    pub attempts: u32,
    pub retry_delay_s: f64,
}

impl Default for SenkushaConfig {
    fn default() -> Self {
        Self { ps5: true, io_timeout_s: 0.1, request_sends: 4, attempts: 3, retry_delay_s: 0.1 }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SenkushaResult {
    pub rtt_ms: i32,
    pub mtu_in: i32,
    pub mtu_out: i32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    Running,
    Complete(SenkushaResult),
    Failed(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Match {
    InitAck,
    CookieAck,
    ProtoAck,
    Bang,
    AckOf(u32),
    ClientMtuReply,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Await {
    Control(Match),
    /// An audio-type datagram (echo of one of our probes).
    Audio,
    /// A video-type datagram (the console's MTU probe).
    Video,
    /// A control message carrying an MTU command (the console's own probing).
    MtuMessage,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Init,
    Cookie,
    Proto,
    Big,
    EchoOn,
    Rtt,
    EchoOff,
    MtuDown,
    Drain,
    ClientMtuOn,
    MtuUp,
    ClientMtuOff,
    Closing,
}

/// A binary search over MTU sizes, exactly as the original runs it: the loop runs while `hi > lo`, the first
/// candidate is given, success raises `lo`, failure lowers `hi`, the next candidate is the midpoint.
#[derive(Clone, Copy, Debug)]
struct Search {
    lo: i32,
    hi: i32,
    cand: i32,
    best: i32,
}

impl Search {
    fn new(first: i32, best: i32) -> Self {
        Self { lo: MTU_MIN, hi: MTU_MAX, cand: first, best }
    }
    fn active(&self) -> bool {
        self.hi > self.lo
    }
    fn record(&mut self, ok: bool) {
        if ok {
            self.best = self.cand;
            self.lo = self.cand + 1;
        } else {
            self.hi = self.cand - 1;
        }
        self.cand = self.lo + (self.hi - self.lo) / 2;
    }
}

/// An outbound `SenkushaAudioMessage` probe: 19-byte audio header (type 3, unit index in the high bits of the
/// word, 2048 total units, codec 0xFF) padded to `total_len`, with `rand` written at offset 4 of the padding.
pub fn probe_packet(unit_index: u32, total_len: usize, rand: u32) -> Vec<u8> {
    let mut p = vec![0u8; 19];
    p[0] = 3;
    let word: u32 = ((2048 - 1) << 10) | (unit_index & 0xFFFF) << 21;
    p[5..9].copy_from_slice(&word.to_be_bytes());
    p[9] = 0xFF;
    let mut pad = vec![0u8; total_len.saturating_sub(p.len())];
    if pad.len() >= 8 {
        pad[4..8].copy_from_slice(&rand.to_be_bytes());
    }
    p.extend(pad);
    p
}

/// RTT in whole milliseconds from per-probe samples in nanoseconds (see module docs for the exact rule).
pub fn rtt_ms(samples_ns: &[i64]) -> Option<i32> {
    if samples_ns.is_empty() {
        return None;
    }
    let n = samples_ns.len() as i64;
    let mean = samples_ns.iter().sum::<i64>() / n;
    let var = samples_ns.iter().map(|x| ((x - mean) as f64).powi(2) as i64).sum::<i64>() / n;
    let mut kept: Vec<i64> = samples_ns.iter().copied().filter(|x| (x - mean).abs() <= 2 * var).collect();
    kept.sort_unstable();
    if kept.len() > 2 {
        kept.remove(0);
        kept.pop();
    }
    if kept.is_empty() {
        return None;
    }
    Some((kept.iter().sum::<i64>() / kept.len() as i64 / 1_000_000) as i32)
}

/// Timeout for the MTU probes: 5 x RTT, 10 ms if that is below 5, never above 500.
pub fn probe_timeout_ms(rtt_ms: i32) -> i32 {
    match rtt_ms * 5 {
        v if v < 5 => 10,
        v if v > 500 => 500,
        v => v,
    }
}

pub struct Senkusha {
    cfg: SenkushaConfig,
    rand: Box<dyn FnMut() -> u32 + Send>,
    state: State,
    step: Step,
    attempt: u32,
    start_at: Option<f64>,
    // context
    tag: u32,
    next_tsn: u32,
    version: i32,
    rtt: i32,
    mtu_in: i32,
    mtu_out: i32,
    io_timeout: f64,
    // current request
    out: VecDeque<Vec<u8>>,
    awaiting: Await,
    last: Vec<Vec<u8>>,
    sends: u32,
    deadline: Option<f64>,
    // per-step state
    probe_idx: u32,
    probe_sent_at: f64,
    samples: Vec<i64>,
    echo_off_attempt: u32,
    search: Search,
    mtu_cmd_id: i32,
    down_attempt: u32,
    down_waiting_video: bool,
    up_i: u32,
    drain_count: u32,
}

impl Senkusha {
    pub fn new(cfg: SenkushaConfig, rand: impl FnMut() -> u32 + Send + 'static) -> Self {
        let io = cfg.io_timeout_s;
        Self {
            cfg,
            rand: Box::new(rand),
            state: State::Running,
            step: Step::Init,
            attempt: 0,
            start_at: Some(0.0),
            tag: 0,
            next_tsn: INITIAL_TSN,
            version: SENKUSHA_VERSION_PS4,
            rtt: 0,
            mtu_in: MTU_MAX,
            mtu_out: MTU_MAX,
            io_timeout: io,
            out: VecDeque::new(),
            awaiting: Await::Control(Match::InitAck),
            last: vec![],
            sends: 0,
            deadline: None,
            probe_idx: 0,
            probe_sent_at: 0.0,
            samples: vec![],
            echo_off_attempt: 0,
            search: Search::new(MTU_MAX, MTU_MAX),
            mtu_cmd_id: 0,
            down_attempt: 0,
            down_waiting_video: false,
            up_i: 0,
            drain_count: 0,
        }
    }

    pub fn state(&self) -> &State {
        &self.state
    }

    pub fn result(&self) -> Option<&SenkushaResult> {
        if let State::Complete(r) = &self.state {
            Some(r)
        } else {
            None
        }
    }

    fn running(&self) -> bool {
        self.state == State::Running
    }

    // ---- driving ----------------------------------------------------------------------------------------------
    pub fn poll_transmit(&mut self, now: f64) -> Option<Vec<u8>> {
        if self.running() {
            if let Some(t) = self.start_at {
                if now >= t {
                    self.start_at = None;
                    self.begin_attempt(now);
                }
            }
        }
        self.out.pop_front()
    }

    pub fn poll_timeout(&self) -> Option<f64> {
        if !self.running() {
            return None;
        }
        self.start_at.or(self.deadline)
    }

    pub fn handle_timeout(&mut self, now: f64) {
        if !self.running() || self.start_at.is_some() {
            return;
        }
        let Some(d) = self.deadline else { return };
        if now < d {
            return;
        }
        self.on_timeout(now);
    }

    // ---- request helpers -----------------------------------------------------------------------------------------
    fn tsn(&mut self) -> u32 {
        let v = self.next_tsn;
        self.next_tsn = self.next_tsn.wrapping_add(1);
        v
    }

    fn start_request(&mut self, now: f64, packets: Vec<Vec<u8>>, awaiting: Await) {
        self.out.extend(packets.iter().cloned());
        self.last = packets;
        self.awaiting = awaiting;
        self.sends = 1;
        self.deadline = Some(now + self.io_timeout);
    }

    /// Resend the current request; false once its sends are used up.
    fn resend(&mut self, now: f64) -> bool {
        if self.sends >= self.cfg.request_sends {
            return false;
        }
        self.sends += 1;
        self.out.extend(self.last.iter().cloned());
        self.deadline = Some(now + self.io_timeout);
        true
    }

    fn send_data(&mut self, class: u32, m: &TakionMessage) -> (u32, Vec<u8>) {
        let seq = self.tsn();
        (seq, Chunk::data(self.tag, seq, class, m.to_chunk_payload()).encode())
    }

    fn senkusha_msg(&mut self, p: SenkushaPayload) -> (u32, Vec<u8>) {
        let mut m = TakionMessage::of_type(msg_type::SENKUSHA);
        m.senkusha = Some(p);
        self.send_data(class::SENKUSHA_ECHO, &m)
    }

    fn ack_of(&self, c: &Chunk) -> Vec<u8> {
        Chunk::ack(self.tag, c.sequence).encode()
    }

    // ---- attempts --------------------------------------------------------------------------------------------------
    fn begin_attempt(&mut self, now: f64) {
        self.tag = 0;
        self.next_tsn = INITIAL_TSN;
        self.version = SENKUSHA_VERSION_PS4;
        self.io_timeout = self.cfg.io_timeout_s;
        self.samples.clear();
        self.step = Step::Init;
        let init =
            Chunk::new(0, chunk_type::INIT, 0, INITIAL_TSN, class::CONTROL, vec![0, 100, 0, 100, 0, 0, 0x48, 0x23]);
        self.start_request(now, vec![init.encode()], Await::Control(Match::InitAck));
    }

    /// This attempt failed: say goodbye if a session exists, then retry after a pause or give up.
    fn fail_attempt(&mut self, now: f64, why: &str) {
        if self.tag != 0 {
            let mut m = TakionMessage::of_type(msg_type::DISCONNECT);
            m.disconnect = Some(DisconnectPayload { reason: Some("Client Disconnecting".into()), extended_info: None });
            let (_, p) = self.send_data(class::GENERAL, &m);
            self.out.push_back(p); // best effort, not waited for
        }
        self.attempt += 1;
        self.deadline = None;
        if self.attempt >= self.cfg.attempts {
            self.state = State::Failed(format!("senkusha failed after {} attempts: {why}", self.attempt));
        } else {
            self.start_at = Some(now + self.cfg.retry_delay_s);
        }
    }

    fn finish(&mut self, now: f64) {
        self.step = Step::Closing;
        let mut m = TakionMessage::of_type(msg_type::DISCONNECT);
        m.disconnect = Some(DisconnectPayload { reason: Some("Client Disconnecting".into()), extended_info: None });
        let (seq, p) = self.send_data(class::GENERAL, &m);
        self.start_request(now, vec![p], Await::Control(Match::AckOf(seq)));
    }

    fn complete(&mut self) {
        self.deadline = None;
        self.state = State::Complete(SenkushaResult { rtt_ms: self.rtt, mtu_in: self.mtu_in, mtu_out: self.mtu_out });
    }

    // ---- timeouts ------------------------------------------------------------------------------------------------------
    fn on_timeout(&mut self, now: f64) {
        match self.step {
            Step::Init | Step::Cookie | Step::Proto | Step::Big | Step::EchoOn | Step::ClientMtuOn => {
                if !self.resend(now) {
                    self.fail_attempt(now, &format!("no answer in step {:?}", self.step));
                }
            }
            Step::Rtt => {
                if !self.resend(now) {
                    self.next_rtt_probe(now); // this probe is lost: move on
                }
            }
            Step::EchoOff => {
                if !self.resend(now) {
                    self.echo_off_attempt += 1;
                    if self.echo_off_attempt < 3 {
                        self.send_echo(now, false);
                    } else {
                        self.fail_attempt(now, "echo off was never acknowledged");
                    }
                }
            }
            Step::MtuDown => {
                if self.down_waiting_video {
                    // the single wait for the console's probe timed out: next of 3 attempts, else this size fails
                    self.down_attempt += 1;
                    if self.down_attempt < 3 {
                        self.send_mtu_command(now);
                    } else {
                        self.search.record(false);
                        self.down_step(now);
                    }
                } else if !self.resend(now) {
                    self.fail_attempt(now, "MTU command was never acknowledged");
                }
            }
            Step::Drain => self.after_drain(now),
            Step::MtuUp => {
                if !self.resend(now) {
                    self.up_i += 1;
                    if self.up_i < 3 {
                        self.send_up_probe(now);
                    } else {
                        self.search.record(false);
                        self.up_step(now);
                    }
                }
            }
            Step::ClientMtuOff => {
                if !self.resend(now) {
                    self.finish(now); // the app only warns here
                }
            }
            Step::Closing => {
                if !self.resend(now) {
                    self.complete(); // the goodbye is best effort
                }
            }
        }
    }

    // ---- incoming ----------------------------------------------------------------------------------------------------------
    pub fn handle_datagram(&mut self, now: f64, data: &[u8]) {
        if !self.running() || self.start_at.is_some() || data.is_empty() {
            return;
        }
        match self.awaiting {
            Await::Audio => {
                if data[0] == 3 {
                    self.on_audio_reply(now);
                }
            }
            Await::Video => {
                if data[0] == 2 {
                    self.on_video_probe(now);
                }
            }
            Await::Control(m) => {
                if let Some(c) = Chunk::decode(data) {
                    self.on_control(now, m, &c);
                }
            }
            Await::MtuMessage => {
                if let Some(c) = Chunk::decode(data) {
                    if c.chunk_type == chunk_type::DATA
                        && c.flags == 1
                        && TakionMessage::parse(&c.payload).and_then(|m| m.senkusha).is_some_and(|s| s.mtu.is_some())
                    {
                        let ack = self.ack_of(&c);
                        self.out.push_back(ack);
                        self.drain_count += 1;
                        if self.drain_count > 20 {
                            self.after_drain(now);
                        } else {
                            self.deadline = Some(now + self.io_timeout);
                        }
                    }
                }
            }
        }
    }

    fn on_control(&mut self, now: f64, m: Match, c: &Chunk) {
        let size = c.wire_size();
        match m {
            Match::InitAck => {
                if c.chunk_type != chunk_type::INIT_ACK
                    || c.flags != 0
                    || !(40..=60).contains(&size)
                    || c.payload.len() < 28
                {
                    return;
                }
                let l3 = u32::from_be_bytes([c.payload[8], c.payload[9], c.payload[10], c.payload[11]]);
                let mut payload = vec![];
                payload.extend(c.sequence.to_be_bytes());
                payload.extend(c.class.to_be_bytes());
                payload.extend(c.sequence.to_be_bytes());
                payload.extend(&c.payload[28..]);
                self.tag = c.sequence;
                let cookie = Chunk::new(c.sequence, chunk_type::COOKIE, 0, l3, c.tag, payload);
                self.step = Step::Cookie;
                self.start_request(now, vec![cookie.encode()], Await::Control(Match::CookieAck));
            }
            Match::CookieAck => {
                if c.chunk_type != chunk_type::COOKIE_ACK || c.flags != 0 {
                    return;
                }
                if self.cfg.ps5 {
                    let mut m = TakionMessage::of_type(msg_type::TAKION_PROTOCOL_REQUEST);
                    m.protocol_request = Some(TakionProtocolRequestPayload { supported_takion_versions: vec![9] });
                    let (_, p) = self.send_data(class::PROTOCOL_REQUEST, &m);
                    self.step = Step::Proto;
                    self.start_request(now, vec![p], Await::Control(Match::ProtoAck));
                } else {
                    self.send_big(now);
                }
            }
            Match::ProtoAck => {
                let Some(m) = self.data_message(c, 18..=25) else { return };
                let Some(v) = m.protocol_request_ack.and_then(|a| a.takion_protocol_version) else { return };
                if m.r#type != Some(msg_type::TAKION_PROTOCOL_REQUEST_ACK) {
                    return;
                }
                self.version = v;
                self.out.push_back(self.ack_of(c));
                self.send_big(now);
            }
            Match::Bang => {
                let Some(m) = self.data_message(c, 11..=49) else { return };
                let Some(b) = m.bang else { return };
                // keyless Bang: server version and key-accepted present, no ECDH material
                if m.r#type != Some(msg_type::BANG)
                    || b.server_version.is_none()
                    || b.encrypted_key_accepted.is_none()
                    || b.ecdh_pub_key.as_ref().is_some_and(|k| !k.is_empty())
                    || b.ecdh_sig.as_ref().is_some_and(|k| !k.is_empty())
                {
                    return;
                }
                self.out.push_back(self.ack_of(c));
                self.send_echo(now, true);
            }
            Match::AckOf(seq) => {
                if c.chunk_type != chunk_type::DATA_ACK || c.sequence != seq {
                    return;
                }
                self.on_acked(now);
            }
            Match::ClientMtuReply => {
                let is_reply = c.chunk_type == chunk_type::DATA
                    && c.flags == 1
                    && TakionMessage::parse(&c.payload)
                        .and_then(|m| m.senkusha)
                        .is_some_and(|s| s.client_mtu.is_some());
                if !is_reply {
                    return;
                }
                self.out.push_back(self.ack_of(c));
                self.start_up_search(now);
            }
        }
    }

    fn data_message(&self, c: &Chunk, sizes: std::ops::RangeInclusive<usize>) -> Option<TakionMessage> {
        if c.chunk_type != chunk_type::DATA || c.flags != 1 || !sizes.contains(&c.wire_size()) {
            return None;
        }
        TakionMessage::parse(&c.payload)
    }

    /// The request we were waiting on was acknowledged: move to the next stage of the current step.
    fn on_acked(&mut self, now: f64) {
        match self.step {
            Step::EchoOn => self.start_rtt(now),
            Step::EchoOff => self.start_down_search(now),
            Step::MtuDown => {
                // MtuCommand acknowledged: now wait for the console's video-type probe (one read, no resend)
                self.down_waiting_video = true;
                self.awaiting = Await::Video;
                self.io_timeout = f64::from(probe_timeout_ms(self.rtt)) / 1000.0;
                self.deadline = Some(now + self.io_timeout);
            }
            Step::ClientMtuOff => self.finish(now),
            Step::Closing => self.complete(),
            _ => {}
        }
    }

    // ---- stages ---------------------------------------------------------------------------------------------------------------
    fn send_big(&mut self, now: f64) {
        let mut m = TakionMessage::of_type(msg_type::BIG);
        m.big = Some(BigPayload {
            client_version: Some(self.version),
            session_key: Some(String::new()),
            launch_spec: Some(vec![]),
            encrypted_key: Some(vec![]),
            ecdh_pub_key: None,
            ecdh_sig: None,
        });
        let (_, p) = self.send_data(class::GENERAL, &m);
        self.step = Step::Big;
        self.start_request(now, vec![p], Await::Control(Match::Bang));
    }

    fn send_echo(&mut self, now: f64, on: bool) {
        let (seq, p) = self.senkusha_msg(SenkushaPayload {
            command: Some(0),
            echo: Some(EchoCommand { state: Some(on) }),
            ..Default::default()
        });
        self.step = if on { Step::EchoOn } else { Step::EchoOff };
        self.start_request(now, vec![p], Await::Control(Match::AckOf(seq)));
    }

    fn start_rtt(&mut self, now: f64) {
        self.step = Step::Rtt;
        self.probe_idx = 0;
        self.samples.clear();
        self.send_rtt_probe(now);
    }

    fn send_rtt_probe(&mut self, now: f64) {
        let r = (self.rand)();
        let p = probe_packet(self.probe_idx, RTT_PROBE_LEN, r);
        self.probe_sent_at = now;
        self.start_request(now, vec![p], Await::Audio);
    }

    fn on_audio_reply(&mut self, now: f64) {
        match self.step {
            Step::Rtt => {
                self.samples.push(((now - self.probe_sent_at) * 1e9) as i64);
                self.next_rtt_probe(now);
            }
            Step::MtuUp => {
                self.search.record(true);
                self.up_step(now);
            }
            _ => {}
        }
    }

    fn next_rtt_probe(&mut self, now: f64) {
        self.probe_idx += 1;
        if self.probe_idx < RTT_PROBES {
            self.send_rtt_probe(now);
            return;
        }
        match rtt_ms(&self.samples) {
            Some(ms) => {
                self.rtt = ms;
                self.echo_off_attempt = 0;
                self.send_echo(now, false);
            }
            None => self.fail_attempt(now, "no echo probe was answered"),
        }
    }

    // downlink MTU: the console sends a datagram of the requested size; arrival proves the path carries it
    fn start_down_search(&mut self, now: f64) {
        self.step = Step::MtuDown;
        self.search = Search::new(MTU_MAX, MTU_MAX);
        self.mtu_cmd_id = 0;
        self.down_step(now);
    }

    fn down_step(&mut self, now: f64) {
        if !self.search.active() {
            self.mtu_in = self.search.best;
            self.drain_count = 0;
            self.step = Step::Drain;
            self.awaiting = Await::MtuMessage;
            self.deadline = Some(now + self.io_timeout);
            return;
        }
        self.down_attempt = 0;
        self.send_mtu_command(now);
    }

    fn send_mtu_command(&mut self, now: f64) {
        self.mtu_cmd_id += 1;
        let cmd = MtuCommand {
            id: Some(self.mtu_cmd_id),
            mtu_req: Some(self.search.cand),
            num: Some(1),
            ..Default::default()
        };
        let (seq, p) = self.senkusha_msg(SenkushaPayload { command: Some(1), mtu: Some(cmd), ..Default::default() });
        self.down_waiting_video = false;
        self.start_request(now, vec![p], Await::Control(Match::AckOf(seq)));
    }

    fn on_video_probe(&mut self, now: f64) {
        if self.step == Step::MtuDown && self.down_waiting_video {
            self.search.record(true);
            self.down_step(now);
        }
    }

    fn after_drain(&mut self, now: f64) {
        // uplink search needs a sane downlink result
        if !(MTU_MIN..=MTU_MAX).contains(&self.mtu_in) {
            self.fail_attempt(now, "downlink MTU out of range");
            return;
        }
        let cm = ClientMtuCommand {
            id: Some(1),
            mtu_req: Some(self.mtu_in),
            state: Some(true),
            mtu_down: Some(self.mtu_in),
        };
        let (seq, p) =
            self.senkusha_msg(SenkushaPayload { command: Some(4), client_mtu: Some(cm), ..Default::default() });
        let _ = seq;
        self.step = Step::ClientMtuOn;
        self.start_request(now, vec![p], Await::Control(Match::ClientMtuReply));
    }

    // uplink MTU: we send datagrams of the candidate size; any echo proves the console received it
    fn start_up_search(&mut self, now: f64) {
        self.step = Step::MtuUp;
        self.search = Search::new(self.mtu_in, self.mtu_in);
        self.up_step(now);
    }

    fn up_step(&mut self, now: f64) {
        if !self.search.active() {
            self.mtu_out = self.search.best;
            let cm = ClientMtuCommand {
                id: Some(1),
                mtu_req: Some(self.mtu_out),
                state: Some(false),
                mtu_down: Some(self.mtu_in),
            };
            let (seq, p) =
                self.senkusha_msg(SenkushaPayload { command: Some(4), client_mtu: Some(cm), ..Default::default() });
            self.step = Step::ClientMtuOff;
            self.start_request(now, vec![p], Await::Control(Match::AckOf(seq)));
            return;
        }
        self.up_i = 0;
        self.send_up_probe(now);
    }

    fn send_up_probe(&mut self, now: f64) {
        let r = (self.rand)();
        let len = (self.search.cand - 28).max(19) as usize;
        let p = probe_packet(self.up_i, len, r);
        self.io_timeout = f64::from(probe_timeout_ms(self.rtt)) / 1000.0;
        self.start_request(now, vec![p], Await::Audio);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_packet_layout() {
        let p = probe_packet(3, 548, 0xDEADBEEF);
        assert_eq!(p.len(), 548);
        assert_eq!(p[0], 3);
        assert_eq!(&p[1..5], &[0, 0, 0, 0]); // packet id and frame id
        let word = u32::from_be_bytes(p[5..9].try_into().unwrap());
        assert_eq!((word >> 21, (word >> 10) & 0x7FF, word & 0x3FF), (3, 2047, 0)); // unit 3 of 2048, no FEC
        assert_eq!(p[9], 0xFF);
        assert!(p[10..19].iter().all(|b| *b == 0));
        assert_eq!(&p[19 + 4..19 + 8], &0xDEADBEEFu32.to_be_bytes()); // random int at offset 4 of the padding
        assert!(p[19..23].iter().all(|b| *b == 0) && p[27..].iter().all(|b| *b == 0));
        assert_eq!(probe_packet(0, 576 - 28, 1).len(), 548); // smallest uplink probe is the same size
        assert_eq!(probe_packet(0, 10, 1).len(), 19); // never shorter than the header
    }

    #[test]
    fn rtt_is_the_trimmed_mean_in_whole_milliseconds() {
        let ms = |v: i64| v * 1_000_000;
        assert_eq!(rtt_ms(&[]), None);
        assert_eq!(rtt_ms(&[ms(7)]), Some(7));
        assert_eq!(rtt_ms(&[ms(4), ms(6)]), Some(5)); // two samples: no trimming
                                                      // 10 samples: extremes (1 and 99) are trimmed, the rest average to 5
        let mut s: Vec<i64> = (0..8).map(|_| ms(5)).collect();
        s.push(ms(1));
        s.push(ms(99));
        assert_eq!(rtt_ms(&s), Some(5));
        assert_eq!(rtt_ms(&[ms(2) + 900_000]), Some(2)); // floors, like the integer division in the original
        assert_eq!(rtt_ms(&[300_000, 400_000, 200_000]), Some(0)); // sub-millisecond rounds to 0
    }

    #[test]
    fn probe_timeout_clamps() {
        // 5 x RTT; a result below 5 becomes 10 ms; never above 500 ms
        for (rtt, want) in [(0, 10), (1, 5), (2, 10), (20, 100), (100, 500), (101, 500), (5000, 500)] {
            assert_eq!(probe_timeout_ms(rtt), want, "rtt {rtt}");
        }
    }

    #[test]
    fn mtu_search_follows_the_originals_loop() {
        // path carries up to 1200: simulate the search and check the visited candidates and the answer
        let (mut s, path) = (Search::new(MTU_MAX, MTU_MAX), 1200);
        let mut visited = vec![];
        while s.active() {
            visited.push(s.cand);
            s.record(s.cand <= path);
        }
        assert_eq!(s.best, 1200);
        assert_eq!(visited[0], 1454); // always starts at the maximum
        assert!(visited.len() <= 12);
        // nothing works: the best stays at the initial 1454 (the original never lowers it)
        let mut s = Search::new(MTU_MAX, MTU_MAX);
        while s.active() {
            s.record(false);
        }
        assert_eq!(s.best, 1454);
        // everything works: ends at the maximum
        let mut s = Search::new(1000, 1000);
        while s.active() {
            s.record(true);
        }
        assert_eq!(s.best, 1453); // the loop stops when lo == hi, so the last value is never probed
    }
}
