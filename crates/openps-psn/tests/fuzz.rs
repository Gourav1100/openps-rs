//! The redirect URL and server answers are untrusted input: nothing may panic.
use openps_psn::config::PsnConfig;
use openps_psn::http::HttpResponse;
use openps_psn::oauth::{parse_account, parse_redirect, parse_tokens, url_decode};

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn bytes(&mut self, max: usize) -> Vec<u8> {
        let n = (self.next() % max.max(1) as u64) as usize;
        (0..n).map(|_| self.next() as u8).collect()
    }
}

fn cfg() -> PsnConfig {
    PsnConfig {
        client_id: "c".into(),
        client_secret: "s".into(),
        token_url: "https://a.test/2.0/oauth/token".into(),
        redirect_uri: "https://a.test/r".into(),
        scope: "x y".into(),
    }
}

#[test]
fn redirect_and_decoding_never_panic() {
    let c = cfg();
    let mut r = Rng(11);
    for _ in 0..60_000 {
        let tail = String::from_utf8_lossy(&r.bytes(80)).to_string();
        let _ = url_decode(&tail);
        let _ = parse_redirect(&c, &format!("https://a.test/r?{tail}"));
        let _ = parse_redirect(&c, &tail);
    }
}

#[test]
fn server_answers_never_panic() {
    let mut r = Rng(12);
    let fragments = [
        "{\"access_token\":\"",
        "{\"user_id\":\"",
        "\"expires_in\":",
        "\"refresh_token\":null",
        "}",
        ",",
        "99999999999999999999999",
    ];
    for _ in 0..40_000 {
        let mut body = fragments[(r.next() % fragments.len() as u64) as usize].as_bytes().to_vec();
        body.extend(r.bytes(60));
        if r.next().is_multiple_of(2) {
            body.extend(fragments[(r.next() % fragments.len() as u64) as usize].as_bytes());
        }
        let resp = HttpResponse { status: [200, 400, 500][(r.next() % 3) as usize], headers: vec![], body };
        let _ = parse_tokens(&resp);
        let _ = parse_account(&resp);
    }
}
