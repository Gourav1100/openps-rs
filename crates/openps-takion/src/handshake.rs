//! Client-side Takion handshake over UDP 9296 as a sans-IO state machine (spec 02 section 5.2).
//!
//! Drive it with `poll_transmit` (datagrams to send), `handle_datagram` (datagrams received), and
//! `poll_timeout`/`handle_timeout` (every step is "send, wait for the reply, up to 4 resends, 1 s apart").
//! The caller owns the socket and the clock, and retries the whole handshake up to 3 times like the app.
//!
//! Deviations from the original are limited to: the 3 post-protocol-request "drain" reads are skipped, and
//! DATA_ACKs always echo the sequence of the chunk being acknowledged.

use crate::chunk::{chunk_type, class, Chunk, GMAC_OFFSET, KEY_POS_OFFSET};
use crate::ecdh::{curve_for, ecdh_sig, Keypair};
use crate::launch::LaunchParams;
use crate::proto::{msg_type, BigPayload, StreamInfoPayload, TakionMessage, TakionProtocolRequestPayload};
use base64::Engine as _;
use openps_crypto::{Context, Direction, RpCipher};
use std::collections::VecDeque;

pub const RESEND_INTERVAL_S: f64 = 1.0;
pub const MAX_RESENDS: u32 = 4;
pub const INITIAL_TSN: u32 = 0x4823;
/// DATA payload the client sends to announce its uplink (microphone) audio header: 1 ch, 16 bit, 48 kHz, 480 samples.
pub const STREAM_INFO_PAYLOAD: [u8; 21] = [
    0x00, 0x08, 0x0d, 0x7a, 0x10, 0x12, 0x0e, 0x01, 0x10, 0x00, 0x00, 0xbb, 0x80, 0x00, 0x00, 0x01, 0xe0, 0x00, 0x00,
    0x00, 0x01,
];
pub const STREAM_INFO_OACK_PAYLOAD: [u8; 3] = [0x00, 0x08, 0x0e];
pub const HEARTBEAT_PAYLOAD: [u8; 3] = [0x00, 0x08, 0x03];

pub struct HandshakeConfig {
    pub ps5: bool,
    /// Offer Takion version 20 in the protocol request.
    pub request_takion20: bool,
    /// Session id from the ctrl frame, sent as `BigPayload.sessionKey`.
    pub session_key: String,
    /// Outbound MTU; Big packets larger than `mtu_out - 28` are fragmented into `mtu_out - 53` byte slices.
    pub mtu_out: usize,
    pub launch: LaunchParams,
    pub handshake_key: [u8; 16],
    /// Session cipher (`gc`): its counter-0 keystream encrypts the launch spec.
    pub session_cipher: RpCipher,
    /// Fixed ECDH secret (tests/replay); `None` generates a random key for the negotiated curve.
    pub ecdh_secret: Option<Vec<u8>>,
    /// Seconds between resends of the current step (the app uses [`RESEND_INTERVAL_S`]).
    pub resend_interval_s: f64,
    /// Resends before the handshake fails (the app uses [`MAX_RESENDS`]).
    pub max_resends: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    AwaitInitAck,
    AwaitCookieAck,
    AwaitProtocolAck,
    AwaitBang,
    AwaitStreamInfo,
    AwaitStreamInfoAck,
    AwaitOackAck,
    Complete,
    Failed(String),
}

/// Everything the stream phase needs from the handshake.
pub struct Session {
    pub remote_tag: u32,
    pub next_tsn: u32,
    pub takion_version: i32,
    pub stream_info: StreamInfoPayload,
    /// Client -> server (tag 2): feedback, mic audio, control GMAC.
    pub local: Context,
    /// Server -> client (tag 3): video/audio/haptic decrypt.
    pub remote: Context,
    pub key_pos: KeyPosCounter,
}

/// Outbound `keyPos` allocator (`ga.c()`): hands out the current value and advances by 16, wrapping at 2^32.
#[derive(Clone, Copy, Debug, Default)]
pub struct KeyPosCounter {
    next: u64,
}
impl KeyPosCounter {
    pub fn allocate(&mut self) -> u32 {
        let v = if self.next + 16 > 0x1_0000_0000 {
            self.next = 0;
            0
        } else {
            self.next
        };
        self.next += 16;
        v as u32
    }
}

enum Last {
    Plain(Vec<Vec<u8>>),
    Protected(Chunk),
}

pub struct Handshake {
    cfg: HandshakeConfig,
    state: State,
    remote_tag: u32,
    next_tsn: u32,
    version: i32,
    keypair: Option<Keypair>,
    local: Option<Context>,
    remote: Option<Context>,
    key_pos: KeyPosCounter,
    stream_info: Option<StreamInfoPayload>,
    out: VecDeque<Vec<u8>>,
    last: Last,
    pending_seq: u32,
    deadline: Option<f64>,
    resends: u32,
    started: bool,
}

impl Handshake {
    pub fn new(cfg: HandshakeConfig) -> Self {
        Self {
            cfg,
            state: State::AwaitInitAck,
            remote_tag: 0,
            next_tsn: INITIAL_TSN,
            version: 9,
            keypair: None,
            local: None,
            remote: None,
            key_pos: KeyPosCounter::default(),
            stream_info: None,
            out: VecDeque::new(),
            last: Last::Plain(vec![]),
            pending_seq: 0,
            deadline: None,
            resends: 0,
            started: false,
        }
    }

    pub fn state(&self) -> &State {
        &self.state
    }
    pub fn takion_version(&self) -> i32 {
        self.version
    }
    pub fn is_complete(&self) -> bool {
        self.state == State::Complete
    }
    pub fn failed(&self) -> Option<&str> {
        if let State::Failed(s) = &self.state {
            Some(s)
        } else {
            None
        }
    }

    fn tsn(&mut self) -> u32 {
        let v = self.next_tsn;
        self.next_tsn = self.next_tsn.wrapping_add(1);
        v
    }

    /// Start (queue INIT) if not started; then pop the next datagram to send.
    pub fn poll_transmit(&mut self, now: f64) -> Option<Vec<u8>> {
        if !self.started {
            self.started = true;
            let init =
                Chunk::new(0, chunk_type::INIT, 0, INITIAL_TSN, class::CONTROL, vec![0, 100, 0, 100, 0, 0, 0x48, 0x23]);
            self.send(now, vec![init.encode()]);
        }
        self.out.pop_front()
    }

    pub fn poll_timeout(&self) -> Option<f64> {
        if matches!(self.state, State::Complete | State::Failed(_)) {
            None
        } else {
            self.deadline
        }
    }

    pub fn handle_timeout(&mut self, now: f64) {
        let Some(d) = self.deadline else { return };
        if now < d || matches!(self.state, State::Complete | State::Failed(_)) {
            return;
        }
        if self.resends >= self.cfg.max_resends {
            self.fail(format!("timed out in {:?}", self.state));
            return;
        }
        self.resends += 1;
        self.deadline = Some(now + self.cfg.resend_interval_s);
        match &self.last {
            Last::Plain(p) => self.out.extend(p.clone()),
            Last::Protected(c) => {
                let c = c.clone();
                let p = self.protect(&c);
                self.out.push_back(p);
            }
        }
    }

    fn send(&mut self, now: f64, packets: Vec<Vec<u8>>) {
        self.out.extend(packets.iter().cloned());
        self.last = Last::Plain(packets);
        self.deadline = Some(now + self.cfg.resend_interval_s);
        self.resends = 0;
    }

    fn send_protected(&mut self, now: f64, c: Chunk) {
        let p = self.protect(&c);
        self.out.push_back(p);
        self.last = Last::Protected(c);
        self.deadline = Some(now + self.cfg.resend_interval_s);
        self.resends = 0;
    }

    /// Serialise with GMAC and keyPos filled: the GMAC is computed first over a packet whose gmac/keyPos are zero,
    /// then both fields are written (control packets, spec 6.5).
    fn protect(&mut self, c: &Chunk) -> Vec<u8> {
        let key_pos = self.key_pos.allocate();
        let mut p = c.encode();
        let tag = self.local.as_mut().expect("keys exist before protected sends").gmac(key_pos, &p);
        p[GMAC_OFFSET..GMAC_OFFSET + 4].copy_from_slice(&tag);
        p[KEY_POS_OFFSET..KEY_POS_OFFSET + 4].copy_from_slice(&key_pos.to_be_bytes());
        p
    }

    fn fail(&mut self, why: String) {
        self.state = State::Failed(why);
        self.deadline = None;
    }

    pub fn handle_datagram(&mut self, now: f64, data: &[u8]) {
        let Some(c) = Chunk::decode(data) else { return };
        let size = c.wire_size();
        let msg =
            || if c.flags == 1 && c.chunk_type == chunk_type::DATA { TakionMessage::parse(&c.payload) } else { None };
        match self.state.clone() {
            State::AwaitInitAck => {
                if c.chunk_type != chunk_type::INIT_ACK || !(40..=60).contains(&size) || c.payload.len() < 28 {
                    return;
                }
                let l3 = u32::from_be_bytes([c.payload[8], c.payload[9], c.payload[10], c.payload[11]]);
                let cookie = &c.payload[28..];
                let mut payload = vec![];
                payload.extend(c.sequence.to_be_bytes());
                payload.extend(c.class.to_be_bytes());
                payload.extend(c.sequence.to_be_bytes());
                payload.extend(cookie);
                self.remote_tag = c.sequence;
                let cookie_chunk = Chunk::new(c.sequence, chunk_type::COOKIE, 0, l3, c.tag, payload);
                self.send(now, vec![cookie_chunk.encode()]);
                self.state = State::AwaitCookieAck;
            }
            State::AwaitCookieAck => {
                if c.chunk_type != chunk_type::COOKIE_ACK || c.flags != 0 {
                    return;
                }
                if self.cfg.ps5 {
                    self.send_protocol_request(now);
                } else {
                    self.send_big(now);
                }
            }
            State::AwaitProtocolAck => {
                let Some(m) = msg() else { return };
                if !(19..24).contains(&size) || m.r#type != Some(msg_type::TAKION_PROTOCOL_REQUEST_ACK) {
                    return;
                }
                let Some(v) = m.protocol_request_ack.and_then(|a| a.takion_protocol_version) else { return };
                self.version = v;
                self.out.push_back(Chunk::ack(self.remote_tag, c.sequence).encode());
                self.send_big(now);
            }
            State::AwaitBang => {
                let Some(m) = msg() else { return };
                if !(101..400).contains(&size) || m.r#type != Some(msg_type::BANG) {
                    return;
                }
                let Some(bang) = m.bang else { return };
                let pub_key = bang.ecdh_pub_key.clone().unwrap_or_default();
                if bang.version_accepted != Some(true)
                    || bang.encrypted_key_accepted != Some(true)
                    || pub_key.is_empty()
                    || bang.ecdh_sig.as_ref().is_none_or(|s| s.is_empty())
                {
                    return self.fail("console rejected the Big payload (version/key not accepted)".into());
                }
                // The original never verifies `ecdh_sig`; neither do we.
                let ack = Chunk::ack(self.remote_tag, c.sequence).encode();
                let Some(kp) = self.keypair.as_ref() else { return };
                let secret = match kp.shared_secret(&pub_key, self.version) {
                    Ok(s) => s,
                    Err(_) => return self.fail("bad console ECDH key".into()),
                };
                self.local = Some(Context::new(Direction::Local, &self.cfg.handshake_key, &secret));
                self.remote = Some(Context::new(Direction::Remote, &self.cfg.handshake_key, &secret));
                self.send(now, vec![ack]); // resent if the console repeats the Bang
                self.state = State::AwaitStreamInfo;
            }
            State::AwaitStreamInfo => {
                let Some(m) = msg() else { return };
                if m.r#type == Some(msg_type::BANG) {
                    // our ack was lost: acknowledge again
                    self.out.push_back(Chunk::ack(self.remote_tag, c.sequence).encode());
                    return;
                }
                if !(50..=400).contains(&size) || m.r#type != Some(msg_type::STREAM_INFO) {
                    return;
                }
                let Some(si) = m.stream_info else { return };
                if si.resolution.is_empty() || si.audio_header.as_ref().is_none_or(|h| h.len() != 14) {
                    return;
                }
                self.stream_info = Some(si);
                let p = self.protect(&Chunk::ack(self.remote_tag, c.sequence)); // first GMAC-protected packet
                self.out.push_back(p);
                let seq = self.tsn();
                self.pending_seq = seq;
                let info = Chunk::data(self.remote_tag, seq, class::STREAM_INFO, STREAM_INFO_PAYLOAD.to_vec());
                self.send_protected(now, info);
                self.state = State::AwaitStreamInfoAck;
            }
            State::AwaitStreamInfoAck => {
                if c.chunk_type != chunk_type::DATA_ACK || c.sequence != self.pending_seq {
                    return;
                }
                let seq = self.tsn();
                self.pending_seq = seq;
                let oack = Chunk::data(self.remote_tag, seq, class::STREAM_INFO, STREAM_INFO_OACK_PAYLOAD.to_vec());
                self.send_protected(now, oack);
                self.state = State::AwaitOackAck;
            }
            State::AwaitOackAck => {
                if c.chunk_type != chunk_type::DATA_ACK || c.sequence != self.pending_seq {
                    return;
                }
                self.deadline = None;
                self.state = State::Complete;
            }
            State::Complete | State::Failed(_) => {}
        }
    }

    fn send_protocol_request(&mut self, now: f64) {
        let mut versions = vec![9, 10, 11, 12, 13, 14];
        if self.cfg.request_takion20 {
            versions.push(20);
        }
        let mut m = TakionMessage::of_type(msg_type::TAKION_PROTOCOL_REQUEST);
        m.protocol_request = Some(TakionProtocolRequestPayload { supported_takion_versions: versions });
        let seq = self.tsn();
        let c = Chunk::data(self.remote_tag, seq, class::PROTOCOL_REQUEST, m.to_chunk_payload());
        self.send(now, vec![c.encode()]);
        self.state = State::AwaitProtocolAck;
    }

    fn send_big(&mut self, now: f64) {
        let curve = curve_for(self.version);
        let kp = match &self.cfg.ecdh_secret {
            Some(s) => match Keypair::from_secret(curve, s) {
                Ok(k) => k,
                Err(_) => return self.fail("bad configured ECDH secret".into()),
            },
            None => Keypair::generate(curve),
        };
        let pub_key = kp.public_bytes();
        let sig = ecdh_sig(&self.cfg.handshake_key, &pub_key);
        self.keypair = Some(kp);

        let key_b64 = base64::engine::general_purpose::STANDARD.encode(self.cfg.handshake_key);
        let json = self.cfg.launch.build(&key_b64).into_bytes();
        let mut ks = vec![0u8; json.len()];
        self.cfg.session_cipher.encrypt(&mut ks, 0); // keystream = encryption of zeros at counter 0
        let xored: Vec<u8> = json.iter().zip(&ks).map(|(a, b)| a ^ b).collect();
        let launch_spec = base64::engine::general_purpose::STANDARD.encode(xored).into_bytes();

        let mut m = TakionMessage::of_type(msg_type::BIG);
        m.big = Some(BigPayload {
            client_version: Some(self.version),
            session_key: Some(self.cfg.session_key.clone()),
            launch_spec: Some(launch_spec),
            encrypted_key: Some(vec![]),
            ecdh_pub_key: Some(pub_key),
            ecdh_sig: Some(sig.to_vec()),
        });
        let payload = m.to_chunk_payload();
        let mut packets = vec![];
        if payload.len() + 25 + 28 <= self.cfg.mtu_out {
            let seq = self.tsn();
            packets.push(Chunk::data(self.remote_tag, seq, class::GENERAL, payload).encode());
        } else {
            let slice = self.cfg.mtu_out.saturating_sub(53).max(1);
            let n = payload.chunks(slice).count();
            for (i, part) in payload.chunks(slice).enumerate() {
                let seq = self.tsn();
                let mut c = Chunk::new(
                    self.remote_tag,
                    chunk_type::DATA,
                    if i + 1 == n { 1 } else { 0 },
                    seq,
                    class::GENERAL,
                    part.to_vec(),
                );
                c.payload_kind = None; // slices of the already-prefixed payload go out raw
                packets.push(c.encode());
            }
        }
        self.send(now, packets);
        self.state = State::AwaitBang;
    }

    /// The finished session. Only valid once [`State::Complete`].
    pub fn into_session(self) -> Option<Session> {
        if self.state != State::Complete {
            return None;
        }
        Some(Session {
            remote_tag: self.remote_tag,
            next_tsn: self.next_tsn,
            takion_version: self.version,
            stream_info: self.stream_info?,
            local: self.local?,
            remote: self.remote?,
            key_pos: self.key_pos,
        })
    }
}
