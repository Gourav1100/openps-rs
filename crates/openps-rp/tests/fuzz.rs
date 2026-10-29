//! Deterministic fuzzing of everything in this crate (and discovery) that parses bytes from the network or from a
//! console reply: none of it may panic, hang, or allocate based on an attacker-controlled length.
use openps_crypto::RpCipher;
use openps_rp::ctrl::{CtrlRx, Inbound};
use openps_rp::http::{parse_response, NoLength, Response};
use openps_rp::regist::{parse_header_block, parse_response as parse_regist};
use openps_rp::session::parse_init_response;

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
    fn bytes(&mut self, max: usize) -> Vec<u8> {
        let n = self.below(max);
        (0..n).map(|_| self.next() as u8).collect()
    }
}

#[test]
fn http_response_parser_never_panics() {
    let mut r = Rng(1);
    let heads: [&[u8]; 4] =
        [b"HTTP/1.1 200 OK\r\n", b"HTTP/1.1 403 X\r\nContent-Length: ", b"HTTP/1.1 200 OK\r\nRP-Nonce: ", b""];
    for _ in 0..60_000 {
        let mut b = heads[r.below(4)].to_vec();
        b.extend(r.bytes(300));
        if r.below(2) == 0 {
            b.extend(b"\r\n\r\n");
            b.extend(r.bytes(100));
        }
        for nl in [NoLength::Empty, NoLength::UntilClose] {
            let _ = parse_response(&b, nl, false);
            let _ = parse_response(&b, nl, true);
        }
    }
}

#[test]
fn control_frame_reader_never_panics_and_survives_any_split() {
    let mut r = Rng(2);
    for _ in 0..3000 {
        let mut rx = CtrlRx::new(RpCipher::new([1; 16], [2; 16], [3; 16]), r.below(100) as u32);
        for _ in 0..30 {
            // mix valid-looking headers with noise
            let mut chunk = vec![];
            if r.below(2) == 0 {
                chunk.extend(((r.below(40)) as u32).to_be_bytes());
                chunk.extend((r.next() as u16).to_be_bytes());
                chunk.extend([0, 0]);
            }
            chunk.extend(r.bytes(60));
            rx.feed(&chunk);
            for _ in 0..8 {
                match rx.next_message() {
                    Ok(Some(_)) => {}
                    Ok(None) | Err(_) => break,
                }
            }
        }
    }
}

#[test]
fn inbound_message_parser_handles_every_type_with_any_payload() {
    let mut r = Rng(3);
    let types = [1u16, 4, 5, 10, 22, 51, 97, 100, 254, 32776, 32777, 32872, 0, 65535];
    for _ in 0..40_000 {
        let ty = types[r.below(types.len())];
        let p = r.bytes(40);
        let m = Inbound::parse(ty, &p);
        let _ = m.plausible();
    }
}

#[test]
fn header_blocks_and_discovery_responses_never_panic() {
    let mut r = Rng(4);
    for _ in 0..50_000 {
        let b = r.bytes(200);
        let _ = parse_header_block(&b);
        let _ = openps_discovery::parse_response(&b);
        let mut prefixed = b"HTTP/1.1 200 Ok\r\n".to_vec();
        prefixed.extend(&b);
        if let Some(resp) = openps_discovery::parse_response(&prefixed) {
            let _ = openps_discovery::console_from_response(&resp, "127.0.0.1".parse().unwrap());
        }
    }
}

#[test]
fn registration_and_init_replies_never_panic_on_garbage() {
    let mut r = Rng(5);
    let cipher = RpCipher::new([9; 16], [8; 16], [7; 16]);
    for _ in 0..30_000 {
        let resp = Response {
            status: [200, 403, 500, 0][r.below(4)],
            headers: vec![
                ("RP-Nonce".into(), String::from_utf8_lossy(&r.bytes(40)).to_string()),
                ("RP-Version".into(), "10.0".into()),
            ],
            body: r.bytes(400),
            ..Default::default()
        };
        let _ = parse_regist(&cipher, &resp);
        let _ = parse_init_response(&resp);
    }
}
