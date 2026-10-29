//! Mock console for the Senkusha network test (UDP 9297): INIT/COOKIE, optional protocol request, Big/Bang without
//! keys, echo probes, and MTU probing. Path MTUs are simulated by dropping datagrams that exceed them, the way a
//! real path drops oversized packets.

use openps_takion::chunk::{chunk_type, class, Chunk};
use openps_takion::proto::*;

pub const SERVER_TAG: u32 = 0x2000_0001;

pub struct SenkushaConsole {
    pub ps5: bool,
    /// Largest IP packet that reaches the client (downlink) / the console (uplink).
    pub path_mtu_down: usize,
    pub path_mtu_up: usize,
    /// Ignore the first N echo probes (simulates loss).
    pub drop_first_probes: usize,
    pub refuse_bang: bool,
    pub seq: u32,
    pub log: Vec<String>,
    pub client_mtu_commands: Vec<(i32, i32, bool)>,
    pub probes_seen: usize,
    /// Length of every echo/MTU probe the client sent.
    pub probe_sizes: Vec<usize>,
    /// Requested size of every downlink MTU probe.
    pub mtu_requests: Vec<usize>,
    pub disconnected: bool,
}

impl SenkushaConsole {
    pub fn new(ps5: bool, path_mtu_down: usize, path_mtu_up: usize) -> Self {
        Self {
            ps5,
            path_mtu_down,
            path_mtu_up,
            drop_first_probes: 0,
            refuse_bang: false,
            seq: SERVER_TAG,
            log: vec![],
            client_mtu_commands: vec![],
            probes_seen: 0,
            probe_sizes: vec![],
            mtu_requests: vec![],
            disconnected: false,
        }
    }

    fn data(&mut self, class: u32, m: &TakionMessage) -> Vec<u8> {
        let s = self.seq;
        self.seq += 1;
        Chunk::data(0xABCD, s, class, m.to_chunk_payload()).encode()
    }

    fn ack(&self, c: &Chunk) -> Vec<u8> {
        Chunk::ack(0xABCD, c.sequence).encode()
    }

    /// React to one datagram from the client; returns datagrams to send back.
    pub fn on_packet(&mut self, raw: &[u8]) -> Vec<Vec<u8>> {
        if raw.is_empty() {
            return vec![];
        }
        match raw[0] & 0x0F {
            3 => return self.on_audio_probe(raw),
            0 => {}
            _ => return vec![],
        }
        let Some(c) = Chunk::decode(raw) else { return vec![] };
        match c.chunk_type {
            chunk_type::INIT => {
                self.log.push("init".into());
                let mut p = vec![0u8; 40];
                p[8..12].copy_from_slice(&0x3333_4444u32.to_be_bytes());
                p[28..].fill(0xCC);
                vec![Chunk::new(0x66, chunk_type::INIT_ACK, 0, SERVER_TAG, 0x4321, p).encode()]
            }
            chunk_type::COOKIE => {
                self.log.push("cookie".into());
                vec![Chunk::new(0, chunk_type::COOKIE_ACK, 0, 0, 0, vec![]).encode()]
            }
            chunk_type::DATA_ACK => {
                self.log.push("ack".into());
                vec![]
            }
            chunk_type::DATA => self.on_data(&c),
            _ => vec![],
        }
    }

    fn on_audio_probe(&mut self, raw: &[u8]) -> Vec<Vec<u8>> {
        self.probes_seen += 1;
        self.probe_sizes.push(raw.len());
        if raw.len() + 28 > self.path_mtu_up || self.probes_seen <= self.drop_first_probes {
            return vec![]; // lost on the way
        }
        // echo as an audio-type packet of the same size
        let mut echo = raw.to_vec();
        echo[0] = 3;
        vec![echo]
    }

    fn on_data(&mut self, c: &Chunk) -> Vec<Vec<u8>> {
        let Some(m) = TakionMessage::parse(&c.payload) else { return vec![self.ack(c)] };
        match m.r#type {
            Some(msg_type::TAKION_PROTOCOL_REQUEST) => {
                self.log.push("protocol-request".into());
                let mut a = TakionMessage::of_type(msg_type::TAKION_PROTOCOL_REQUEST_ACK);
                a.protocol_request_ack = Some(TakionProtocolRequestAckPayload { takion_protocol_version: Some(9) });
                vec![self.data(class::GENERAL, &a)]
            }
            Some(msg_type::BIG) => {
                self.log.push("big".into());
                let big = m.big.unwrap_or_default();
                assert_eq!(big.session_key.as_deref(), Some(""), "senkusha sends an empty session key");
                if self.refuse_bang {
                    return vec![];
                }
                let mut bang = TakionMessage::of_type(msg_type::BANG);
                bang.bang = Some(BangPayload {
                    server_version: Some(if self.ps5 { 9 } else { 7 }),
                    encrypted_key_accepted: Some(true),
                    version_accepted: Some(true),
                    ..Default::default()
                });
                vec![self.data(class::GENERAL, &bang)]
            }
            Some(msg_type::SENKUSHA) => self.on_senkusha(c, m.senkusha.unwrap_or_default()),
            Some(msg_type::DISCONNECT) => {
                self.log.push("disconnect".into());
                self.disconnected = true;
                vec![self.ack(c)]
            }
            _ => vec![self.ack(c)],
        }
    }

    fn on_senkusha(&mut self, c: &Chunk, s: SenkushaPayload) -> Vec<Vec<u8>> {
        let mut out = vec![self.ack(c)];
        match s.command {
            Some(0) => self.log.push(format!("echo:{}", s.echo.and_then(|e| e.state).unwrap_or(false))),
            Some(1) => {
                let req = s.mtu.and_then(|m| m.mtu_req).unwrap_or(0) as usize;
                self.log.push(format!("mtu:{req}"));
                self.mtu_requests.push(req);
                // the console answers with a video-type packet of that IP size; oversized ones never arrive
                if req <= self.path_mtu_down && req > 28 {
                    let mut v = vec![0u8; req - 28];
                    v[0] = 2;
                    out.push(v);
                }
            }
            Some(4) => {
                let cm = s.client_mtu.unwrap_or_default();
                self.client_mtu_commands.push((
                    cm.mtu_req.unwrap_or(0),
                    cm.mtu_down.unwrap_or(0),
                    cm.state.unwrap_or(false),
                ));
                self.log.push(format!("client-mtu:{}", cm.state.unwrap_or(false)));
                // the console confirms with a client-MTU message of its own
                let mut reply = TakionMessage::of_type(msg_type::SENKUSHA);
                reply.senkusha = Some(SenkushaPayload { command: Some(4), client_mtu: Some(cm), ..Default::default() });
                out.push(self.data(class::SENKUSHA_ECHO, &reply));
            }
            _ => {}
        }
        out
    }
}
