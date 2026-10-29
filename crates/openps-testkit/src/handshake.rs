//! Mock console that answers the client handshake (INIT .. OACK). Written from the same spec as the client, so
//! it proves internal consistency, not wire compatibility with a real PlayStation.
use base64::Engine as _;
use openps_crypto::{Context, Direction, RpCipher};
use openps_takion::chunk::{chunk_type, class, Chunk, GMAC_OFFSET, KEY_POS_OFFSET};
use openps_takion::ecdh::{curve_for, Keypair};
use openps_takion::proto::*;

pub const SERVER_TAG: u32 = 0x1000_0001;
pub const HS_KEY: [u8; 16] = [0x42; 16];

pub struct HandshakeConsole {
    pub version: i32,
    pub reject_big: bool,
    pub big_fragments: Vec<u8>,
    pub server_seq: u32,
    pub local_ctx: Option<Context>, // client->server (tag 2): used to verify the client's GMAC
    pub remote_ctx: Option<Context>, // server->client (tag 3): encrypts the stream once the handshake is done
    pub saw: Vec<&'static str>,
    pub launch_json: Option<String>,
    pub server_kp: Option<Keypair>,
    pub gmac_ok: Vec<bool>,
    pub client_seqs: Vec<u32>,
    /// Session key the Big payload must carry (`None` = accept anything).
    pub expect_session_key: Option<String>,
    /// The session cipher the client encrypts the launch spec with.
    pub session_cipher: RpCipher,
    pub bang_sent: bool,
    pub si_sent: bool,
    /// Annex-B parameter sets and size reported in STREAM_INFO (default: a dummy 1280x720 header).
    pub video_header: Option<Vec<u8>>,
    pub resolution: (i32, i32),
    /// The 14-byte audio header reported in STREAM_INFO (default: all zero, as before).
    pub audio_header: Option<Vec<u8>>,
}

impl HandshakeConsole {
    pub fn new(version: i32) -> Self {
        Self {
            version,
            reject_big: false,
            big_fragments: vec![],
            server_seq: SERVER_TAG,
            local_ctx: None,
            remote_ctx: None,
            saw: vec![],
            launch_json: None,
            server_kp: None,
            gmac_ok: vec![],
            client_seqs: vec![],
            expect_session_key: Some("sess-1".into()),
            session_cipher: RpCipher::new([1; 16], [2; 16], [3; 16]),
            bang_sent: false,
            si_sent: false,
            video_header: None,
            resolution: (1280, 720),
            audio_header: None,
        }
    }
    pub fn data(&mut self, m: &TakionMessage) -> Vec<u8> {
        let s = self.server_seq;
        self.server_seq += 1;
        Chunk::data(0xC0FFEE, s, class::GENERAL, m.to_chunk_payload()).encode()
    }

    pub fn verify_gmac(&mut self, raw: &[u8]) {
        if let Some(ctx) = self.local_ctx.as_mut() {
            let key_pos = u32::from_be_bytes(raw[KEY_POS_OFFSET..KEY_POS_OFFSET + 4].try_into().unwrap());
            let mut zeroed = raw.to_vec();
            zeroed[GMAC_OFFSET..GMAC_OFFSET + 8].fill(0); // gmac + keyPos
            let tag = ctx.gmac(key_pos, &zeroed);
            self.gmac_ok.push(tag == raw[GMAC_OFFSET..GMAC_OFFSET + 4]);
        }
    }

    pub fn on_packet(&mut self, raw: &[u8]) -> Vec<Vec<u8>> {
        let c = Chunk::decode(raw).expect("client sends control chunks");
        match c.chunk_type {
            chunk_type::INIT => {
                self.saw.push("init");
                assert_eq!((c.tag, c.sequence, c.class), (0, 0x4823, class::CONTROL));
                assert_eq!(c.payload, vec![0, 100, 0, 100, 0, 0, 0x48, 0x23]);
                let mut p = vec![0u8; 40];
                p[8..12].copy_from_slice(&0x2222_3333u32.to_be_bytes()); // remote init tag
                p[28..].copy_from_slice(&[0xAA; 12]); // cookie
                vec![Chunk::new(0x77, chunk_type::INIT_ACK, 0, SERVER_TAG, 0x1234, p).encode()]
            }
            chunk_type::COOKIE => {
                self.saw.push("cookie");
                assert_eq!((c.tag, c.sequence, c.class), (SERVER_TAG, 0x2222_3333, 0x77));
                let mut want = vec![];
                for v in [SERVER_TAG, 0x1234, SERVER_TAG] {
                    want.extend(v.to_be_bytes());
                }
                want.extend([0xAA; 12]);
                assert_eq!(c.payload, want);
                vec![Chunk::new(0, chunk_type::COOKIE_ACK, 0, 0, 0, vec![]).encode()]
            }
            chunk_type::DATA_ACK => {
                self.saw.push("ack");
                if self.si_sent {
                    self.verify_gmac(raw);
                } // acks after StreamInfo are GMAC-protected
                self.client_seqs.push(c.sequence);
                if self.bang_sent && !self.si_sent {
                    // the Bang was acknowledged: the console sends StreamInfo
                    self.si_sent = true;
                    return vec![self.stream_info()];
                }
                vec![]
            }
            chunk_type::DATA => self.on_data(&c, raw),
            t => panic!("unexpected chunk type {t}"),
        }
    }

    pub fn on_data(&mut self, c: &Chunk, raw: &[u8]) -> Vec<Vec<u8>> {
        // Big may arrive fragmented: flags 0 slices are buffered; the last (flags 1) completes the message.
        if c.class == class::GENERAL {
            let mut bytes = vec![];
            if let Some(k) = c.payload_kind {
                bytes.push(k);
            }
            bytes.extend(&c.payload);
            if c.flags == 0 {
                self.big_fragments.extend(bytes);
                return vec![];
            }
            self.big_fragments.extend(bytes);
            let whole = std::mem::take(&mut self.big_fragments);
            return self.on_message(c, raw, &whole[1..]);
        }
        let body: Vec<u8> = c.payload.clone();
        self.on_message(c, raw, &body)
    }

    pub fn on_message(&mut self, c: &Chunk, raw: &[u8], body: &[u8]) -> Vec<Vec<u8>> {
        // client StreamInfo / OACK payloads are fixed strings that begin with a protobuf message
        if c.class == class::STREAM_INFO {
            self.verify_gmac(raw);
            self.client_seqs.push(c.sequence);
            let m = TakionMessage::parse(body).expect("client message parses");
            self.saw.push(if m.r#type == Some(msg_type::STREAM_INFO) { "client-stream-info" } else { "oack" });
            return vec![Chunk::ack(0xC0FFEE, c.sequence).encode()];
        }
        let m = TakionMessage::parse(body).expect("client message parses");
        match m.r#type {
            Some(msg_type::TAKION_PROTOCOL_REQUEST) => {
                self.saw.push("protocol-request");
                let v = m.protocol_request.unwrap().supported_takion_versions;
                assert_eq!(v, vec![9, 10, 11, 12, 13, 14]);
                assert_eq!(c.class, class::PROTOCOL_REQUEST);
                let mut a = TakionMessage::of_type(msg_type::TAKION_PROTOCOL_REQUEST_ACK);
                a.protocol_request_ack =
                    Some(TakionProtocolRequestAckPayload { takion_protocol_version: Some(self.version) });
                vec![self.data(&a)]
            }
            Some(msg_type::BIG) => {
                self.saw.push("big");
                let big = m.big.unwrap();
                assert_eq!(big.client_version, Some(self.version));
                if let Some(want) = &self.expect_session_key {
                    assert_eq!(big.session_key.as_deref(), Some(want.as_str()));
                }
                assert_eq!(big.encrypted_key, Some(vec![]));
                // decrypt the launch spec with the session keystream and check the handshake key travels inside it
                let ct = base64::engine::general_purpose::STANDARD.decode(big.launch_spec.unwrap()).unwrap();
                let mut ks = vec![0u8; ct.len()];
                self.session_cipher.encrypt(&mut ks, 0);
                let json = String::from_utf8(ct.iter().zip(&ks).map(|(a, b)| a ^ b).collect()).unwrap();
                // learn the handshake key the way a real console does: it travels inside the launch spec
                let hs_key: [u8; 16] = json
                    .split("\"handshakeKey\":\"")
                    .nth(1)
                    .and_then(|r| r.split('"').next())
                    .and_then(|b64| base64::engine::general_purpose::STANDARD.decode(b64).ok())
                    .and_then(|v| v.try_into().ok())
                    .expect("launch spec carries a 16-byte handshake key");
                self.launch_json = Some(json);
                // ecdh_sig = HMAC-SHA256(handshakeKey, pub)
                assert_eq!(
                    big.ecdh_sig.unwrap(),
                    openps_takion::ecdh::ecdh_sig(&hs_key, big.ecdh_pub_key.as_ref().unwrap()).to_vec()
                );
                let client_pub = big.ecdh_pub_key.unwrap();
                let kp = Keypair::generate(curve_for(self.version));
                let secret = kp.shared_secret(&client_pub, self.version).unwrap();
                self.local_ctx = Some(Context::new(Direction::Local, &hs_key, &secret));
                self.remote_ctx = Some(Context::new(Direction::Remote, &hs_key, &secret));
                let mut bang = TakionMessage::of_type(msg_type::BANG);
                bang.bang = Some(BangPayload {
                    server_version: Some(self.version),
                    token: Some(1),
                    encrypted_key_accepted: Some(!self.reject_big),
                    version_accepted: Some(true),
                    ecdh_pub_key: Some(kp.public_bytes()),
                    ecdh_sig: Some(vec![9; 32]),
                    ..Default::default()
                });
                self.server_kp = Some(kp);
                self.bang_sent = true;
                vec![self.data(&bang)]
            }
            t => panic!("unexpected message {t:?}"),
        }
    }

    pub fn stream_info(&mut self) -> Vec<u8> {
        let mut m = TakionMessage::of_type(msg_type::STREAM_INFO);
        m.stream_info = Some(StreamInfoPayload {
            resolution: vec![ResolutionPayload {
                width: Some(self.resolution.0),
                height: Some(self.resolution.1),
                video_header: Some(self.video_header.clone().unwrap_or_else(|| {
                    vec![
                        0, 0, 0, 1, 0x67, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21,
                        22, 23, 24, 25, 26, 27, 28,
                    ]
                })),
            }],
            audio_header: Some(self.audio_header.clone().unwrap_or_else(|| vec![0; 14])),
            ..Default::default()
        });
        self.data(&m)
    }
}

impl HandshakeConsole {
    /// True once the client's OACK has been acknowledged (the handshake is over from the console's side).
    pub fn done(&self) -> bool {
        self.saw.contains(&"oack")
    }

    /// Continue as a streaming console using the keys negotiated during this handshake.
    pub fn into_stream_console(self) -> Option<crate::stream::StreamConsole> {
        Some(crate::stream::StreamConsole::from_contexts(self.remote_ctx?, self.local_ctx?, self.server_seq))
    }
}
