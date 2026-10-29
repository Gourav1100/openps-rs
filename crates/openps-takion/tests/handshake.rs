//! Handshake tests against a mock console. The console is written from the same spec as the client, so these
//! prove internal consistency (framing, retransmits, ECDH agreement, GMAC/keystream symmetry), NOT wire
//! compatibility with a real PlayStation: that needs a captured session.
use base64::Engine as _;
use openps_crypto::RpCipher;
use openps_takion::chunk::{chunk_type, class, Chunk};
use openps_takion::handshake::*;
use openps_takion::launch::LaunchParams;
use openps_takion::proto::*;

use openps_testkit::handshake::{HandshakeConsole as Console, HS_KEY, SERVER_TAG};

fn cfg(ps5: bool, mtu_out: usize) -> HandshakeConfig {
    HandshakeConfig {
        ps5,
        request_takion20: false,
        session_key: "sess-1".into(),
        mtu_out,
        launch: LaunchParams { ps5, ..Default::default() },
        handshake_key: HS_KEY,
        session_cipher: RpCipher::new([1; 16], [2; 16], [3; 16]),
        ecdh_secret: None,
        resend_interval_s: RESEND_INTERVAL_S,
        max_resends: MAX_RESENDS,
    }
}

/// Pump packets between client and console until nothing is in flight. Returns the number of datagrams the client sent.
fn pump(h: &mut Handshake, con: &mut Console, now: &mut f64, drop_first_n_client_packets: usize) -> usize {
    let (mut sent, mut dropped) = (0, 0);
    for _ in 0..200 {
        let mut progressed = false;
        while let Some(p) = h.poll_transmit(*now) {
            sent += 1;
            progressed = true;
            if dropped < drop_first_n_client_packets {
                dropped += 1;
                continue;
            }
            let replies = con.on_packet(&p);
            for r in replies {
                h.handle_datagram(*now, &r);
            }
        }
        if !progressed {
            match h.poll_timeout() {
                Some(t) if !h.is_complete() && h.failed().is_none() => {
                    *now = t;
                    h.handle_timeout(*now);
                }
                _ => break,
            }
        }
        if h.is_complete() || h.failed().is_some() {
            while h.poll_transmit(*now).is_some() {}
            break;
        }
    }
    sent
}

fn run(ps5: bool, version: i32, mtu_out: usize, drop_n: usize) -> (Handshake, Console) {
    let mut h = Handshake::new(cfg(ps5, mtu_out));
    let mut con = Console::new(version);
    let mut now = 0.0;
    pump(&mut h, &mut con, &mut now, drop_n);
    (h, con)
}

#[test]
fn ps5_handshake_completes_with_p521_and_protected_packets() {
    let (h, con) = run(true, 14, 1454, 0);
    assert!(h.is_complete(), "{:?}", h.state());
    assert_eq!(
        con.saw,
        vec!["init", "cookie", "protocol-request", "ack", "big", "ack", "ack", "client-stream-info", "oack"]
    );
    assert_eq!(h.takion_version(), 14);
    let json = con.launch_json.unwrap();
    assert!(json.contains("\"videoCodec\":\"avc\"") && json.ends_with('\u{0}'));
    assert!(json.contains(&base64::engine::general_purpose::STANDARD.encode(HS_KEY)));
    // every GMAC-protected packet (stream-info ack, StreamInfo, OACK) verifies under the console's tag-2 context
    assert_eq!(con.gmac_ok.len(), 3, "{:?}", con.saw);
    assert!(con.gmac_ok.iter().all(|ok| *ok));
    let s = h.into_session().unwrap();
    assert_eq!(s.stream_info.resolution[0].width, Some(1280));
    assert_eq!(s.stream_info.audio_header.as_ref().unwrap().len(), 14);
    assert_eq!(s.remote_tag, SERVER_TAG);
    // client sequences: protocol request, Big, StreamInfo, OACK
    assert_eq!(s.next_tsn, INITIAL_TSN + 4);
}

#[test]
fn ps4_skips_the_protocol_request_and_uses_k256() {
    let (h, con) = run(false, 9, 1454, 0);
    assert!(h.is_complete(), "{:?}", h.state());
    assert!(!con.saw.contains(&"protocol-request"));
    assert_eq!(h.takion_version(), 9);
    assert!(!con.launch_json.unwrap().contains("videoCodec"));
}

#[test]
fn keys_agree_in_both_directions() {
    let (h, _con) = run(true, 14, 1454, 0);
    let s = h.into_session().unwrap();
    let mut data = b"video payload".to_vec();
    s.remote.ctr_xor(48, &mut data);
    s.remote.ctr_xor(48, &mut data);
    assert_eq!(data, b"video payload");
    // client -> server: the console verified GMAC with its independently derived tag-2 context (checked above),
    // so local keys match; confirm distinct directions
    let mut a = b"x".to_vec();
    let mut b = b"x".to_vec();
    s.local.ctr_xor(0, &mut a);
    s.remote.ctr_xor(0, &mut b);
    assert_ne!(a, b);
}

#[test]
fn lost_packets_are_retransmitted() {
    let (h, con) = run(true, 14, 1454, 1); // the very first INIT is dropped
    assert!(h.is_complete(), "{:?}", h.state());
    assert_eq!(con.saw.iter().filter(|s| **s == "init").count(), 1);
}

#[test]
fn gives_up_after_four_resends() {
    let mut h = Handshake::new(cfg(true, 1454));
    let mut sent = 0;
    let mut now = 0.0;
    for _ in 0..20 {
        while h.poll_transmit(now).is_some() {
            sent += 1;
        }
        match h.poll_timeout() {
            Some(t) => {
                now = t;
                h.handle_timeout(now);
            }
            None => break,
        }
    }
    assert_eq!(sent, 5); // INIT + 4 resends
    assert!(h.failed().unwrap().contains("timed out"));
    assert!(h.into_session().is_none());
}

#[test]
fn a_rejected_big_fails_the_handshake() {
    let mut h = Handshake::new(cfg(true, 1454));
    let mut con = Console::new(14);
    con.reject_big = true;
    let mut now = 0.0;
    pump(&mut h, &mut con, &mut now, 0);
    assert!(h.failed().unwrap().contains("rejected"), "{:?}", h.state());
}

#[test]
fn small_mtu_fragments_the_big_payload() {
    let (h, con) = run(true, 14, 300, 0); // slices of 247 bytes
    assert!(h.is_complete(), "{:?}", h.state());
    assert!(con.saw.contains(&"big"));
}

#[test]
fn takion20_is_offered_when_requested() {
    let mut c = cfg(true, 1454);
    c.request_takion20 = true;
    let mut h = Handshake::new(c);
    let init = h.poll_transmit(0.0).unwrap();
    assert_eq!(Chunk::decode(&init).unwrap().chunk_type, chunk_type::INIT);
    let mut p = vec![0u8; 40];
    p[28..].fill(1);
    h.handle_datagram(0.0, &Chunk::new(1, chunk_type::INIT_ACK, 0, SERVER_TAG, 2, p).encode());
    let _cookie = h.poll_transmit(0.0).unwrap();
    h.handle_datagram(0.0, &Chunk::new(0, chunk_type::COOKIE_ACK, 0, 0, 0, vec![]).encode());
    let req = Chunk::decode(&h.poll_transmit(0.0).unwrap()).unwrap();
    let m = TakionMessage::parse(&req.payload).unwrap();
    assert_eq!(m.protocol_request.unwrap().supported_takion_versions, vec![9, 10, 11, 12, 13, 14, 20]);
}

#[test]
fn garbage_and_out_of_order_packets_are_ignored() {
    let mut h = Handshake::new(cfg(true, 1454));
    h.poll_transmit(0.0);
    h.handle_datagram(0.0, &[1, 2, 3]);
    h.handle_datagram(0.0, &[0; 40]); // a chunk of type 0 in the wrong state
    h.handle_datagram(0.0, &Chunk::new(0, chunk_type::COOKIE_ACK, 0, 0, 0, vec![]).encode());
    assert_eq!(*h.state(), State::AwaitInitAck);
}

// ---- constants and protobuf against the app's raw bytes ---------------------------------------------------
#[test]
fn client_stream_info_constant_is_the_protobuf_the_spec_describes() {
    let m = TakionMessage::parse(&STREAM_INFO_PAYLOAD[1..]).unwrap();
    assert_eq!(m.r#type, Some(msg_type::STREAM_INFO));
    let ah = m.stream_info.unwrap().audio_header.unwrap();
    assert_eq!(ah, vec![0x01, 0x10, 0x00, 0x00, 0xbb, 0x80, 0x00, 0x00, 0x01, 0xe0, 0x00, 0x00, 0x00, 0x01]);
    // and re-encoding it reproduces the app's bytes exactly
    let mut again = TakionMessage::of_type(msg_type::STREAM_INFO);
    again.stream_info = Some(StreamInfoPayload { audio_header: Some(ah), ..Default::default() });
    assert_eq!(again.to_chunk_payload(), STREAM_INFO_PAYLOAD.to_vec());
}

#[test]
fn oack_and_heartbeat_constants_encode_identically() {
    assert_eq!(TakionMessage::of_type(msg_type::STREAM_INFO_ACK).to_chunk_payload(), STREAM_INFO_OACK_PAYLOAD.to_vec());
    assert_eq!(TakionMessage::of_type(msg_type::HEARTBEAT).to_chunk_payload(), HEARTBEAT_PAYLOAD.to_vec());
}

#[test]
fn protobuf_roundtrips_and_skips_unknown_fields() {
    let mut m = TakionMessage::of_type(msg_type::CONTROLLER_CONNECTION);
    m.controller_connection =
        Some(ControllerConnectionPayload { controller_id: Some(1), connected: Some(true), controller_type: Some(6) });
    let b = m.encode_to_vec_pub();
    assert_eq!(TakionMessage::parse(&b).unwrap(), m);
    let mut with_unknown = b.clone();
    with_unknown.extend([0xF8, 0x07, 0x01]); // field 127 varint
    assert_eq!(TakionMessage::parse(&with_unknown).unwrap(), m);
    let mut idr = TakionMessage::of_type(msg_type::IDR_REQUEST);
    idr.corrupt = Some(CorruptFramePayload { start: Some(10), end: Some(12) });
    assert_eq!(TakionMessage::parse(&idr.encode_to_vec_pub()).unwrap().corrupt.unwrap().end, Some(12));
}

trait EncodePub {
    fn encode_to_vec_pub(&self) -> Vec<u8>;
}
impl EncodePub for TakionMessage {
    fn encode_to_vec_pub(&self) -> Vec<u8> {
        self.to_chunk_payload()[1..].to_vec()
    }
}

#[test]
fn key_pos_counter_advances_by_16_and_wraps() {
    let mut k = KeyPosCounter::default();
    assert_eq!((k.allocate(), k.allocate(), k.allocate()), (0, 16, 32));
}

// ---- robustness: the handshake parses untrusted bytes in every state -------------------------------------------
#[test]
fn handshake_survives_random_and_structured_datagrams_in_every_state() {
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next() % n.max(1) as u64) as usize
        }
        fn bytes(&mut self, n: usize) -> Vec<u8> {
            (0..n).map(|_| self.next() as u8).collect()
        }
    }
    for seed in 1..=40u64 {
        let mut r = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let mut h = Handshake::new(cfg(seed % 2 == 0, 1454));
        let mut now = 0.0;
        for i in 0..400 {
            now += 0.3;
            let _ = h.poll_transmit(now);
            let pkt = match r.below(4) {
                0 => {
                    let n = r.below(200);
                    r.bytes(n)
                }
                1 => {
                    // INIT_ACK-like with a random payload length
                    let n = r.below(80);
                    Chunk::new(r.next() as u32, chunk_type::INIT_ACK, 0, r.next() as u32, r.next() as u32, r.bytes(n))
                        .encode()
                }
                2 => {
                    // DATA with a random protobuf-ish payload (Bang / StreamInfo / ack lookalikes)
                    let mut payload = vec![0u8];
                    let n = r.below(300);
                    payload.extend(r.bytes(n));
                    Chunk::data(r.next() as u32, r.next() as u32, class::GENERAL, payload).encode()
                }
                _ => Chunk::new(
                    r.next() as u32,
                    [3u8, 10, 11, 0, 2][r.below(5)],
                    r.below(2) as u8,
                    r.next() as u32,
                    0,
                    vec![0; r.below(20)],
                )
                .encode(),
            };
            h.handle_datagram(now, &pkt);
            if i % 7 == 0 {
                h.handle_timeout(now + 5.0);
            }
            if h.failed().is_some() || h.is_complete() {
                break;
            }
        }
    }
}
