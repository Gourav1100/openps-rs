//! Authorization-code flow building blocks: URLs, requests and response parsing. Pure functions; nothing here
//! performs I/O.

use crate::config::PsnConfig;
use crate::http::{HttpRequest, HttpResponse};
use base64::Engine as _;
use rand_core::RngCore;
use serde::Deserialize;

pub const DUID_PREFIX: &str = "0000000700410080";

/// Percent-encode everything except RFC 3986 unreserved characters.
pub fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn hex_val(c: u8) -> Option<u8> {
    (c as char).to_digit(16).map(|d| d as u8)
}

pub fn url_decode(s: &str) -> String {
    let b = s.as_bytes();
    let (mut out, mut i) = (Vec::with_capacity(b.len()), 0);
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                // decode from bytes, never by slicing the &str: a multibyte character after '%' is not a char boundary
                if let (Some(h), Some(l)) = (hex_val(b[i + 1]), hex_val(b[i + 2])) {
                    out.push(h << 4 | l);
                    i += 3;
                    continue;
                }
                out.push(b'%');
                i += 1;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Device id sent with the authorize request: a fixed prefix plus 16 random bytes in hex.
pub fn new_duid(rng: &mut impl RngCore) -> String {
    let mut b = [0u8; 16];
    rng.fill_bytes(&mut b);
    format!("{DUID_PREFIX}{}", b.iter().map(|x| format!("{x:02x}")).collect::<String>())
}

/// The URL to open in a browser or web view. The user signs in and is redirected to `redirect_uri?code=...`.
pub fn authorize_url(cfg: &PsnConfig, duid: &str) -> String {
    format!(
        "{}?service_entity=urn:service-entity:psn&response_type=code&client_id={}&redirect_uri={}&duid={}&scope={}&layout_type=popup&smcid=remoteplay&prompt=always&PlatformPrivacyWs1=minimal&",
        cfg.authorize_url_base(),
        url_encode(&cfg.client_id),
        cfg.redirect_uri,
        duid,
        cfg.scope.replace(' ', "%20")
    )
}

#[derive(Debug, PartialEq, Eq)]
pub enum RedirectOutcome {
    Code(String),
    /// The user declined (`error=access_denied`) or the redirect carried an error.
    Denied(String),
    /// Not our redirect, or no code in it.
    NotRedirect,
}

/// Is `url` our redirect target? The redirect URI must match as a whole URL prefix, ending at a path/query/fragment
/// boundary: a bare string prefix would let `https://host.evil.test/` pass for a redirect URI of `https://host`.
pub fn is_redirect(cfg: &PsnConfig, url: &str) -> bool {
    let base = cfg.redirect_uri.as_str();
    match url.strip_prefix(base) {
        None => false,
        Some(rest) => base.ends_with('/') || rest.is_empty() || rest.starts_with(['?', '#', '/']),
    }
}

/// Inspect a URL the web view is about to navigate to.
pub fn parse_redirect(cfg: &PsnConfig, url: &str) -> RedirectOutcome {
    if !is_redirect(cfg, url) {
        return RedirectOutcome::NotRedirect;
    }
    let query = url.split_once('?').map(|(_, q)| q).unwrap_or("");
    let query = query.split('#').next().unwrap_or("");
    let mut code = None;
    for pair in query.split('&') {
        match pair.split_once('=') {
            Some(("code", v)) if !v.is_empty() => code = Some(url_decode(v)),
            Some(("error", v)) => return RedirectOutcome::Denied(url_decode(v)),
            _ => {}
        }
    }
    code.map(RedirectOutcome::Code).unwrap_or(RedirectOutcome::NotRedirect)
}

fn basic_auth(cfg: &PsnConfig) -> String {
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("{}:{}", cfg.client_id, cfg.client_secret))
    )
}

fn form(pairs: &[(&str, &str)]) -> Vec<u8> {
    pairs.iter().map(|(k, v)| format!("{}={}", url_encode(k), url_encode(v))).collect::<Vec<_>>().join("&").into_bytes()
}

fn token_request(cfg: &PsnConfig, body: Vec<u8>) -> HttpRequest {
    HttpRequest {
        method: "POST",
        url: cfg.token_url.clone(),
        headers: vec![
            ("Authorization".into(), basic_auth(cfg)),
            ("Content-Type".into(), "application/x-www-form-urlencoded".into()),
            ("Accept".into(), "application/json".into()),
        ],
        body,
    }
}

/// Text from the network that ends up on screen: no control, bidi-override, zero-width or line-separator characters,
/// at most `max` characters, trimmed.
pub(crate) fn clean_text(s: &str, max: usize) -> String {
    let hidden = |c: char| {
        c.is_control()
            || matches!(c, '\u{00AD}' | '\u{061C}' | '\u{200B}'..='\u{200F}' | '\u{2028}'..='\u{202E}' | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{2069}' | '\u{FEFF}')
    };
    s.chars()
        .filter(|c| !hidden(*c))
        .collect::<String>()
        .trim()
        .chars()
        .take(max)
        .collect::<String>()
        .trim()
        .to_string()
}

pub fn exchange_code_request(cfg: &PsnConfig, code: &str) -> HttpRequest {
    token_request(
        cfg,
        form(&[("grant_type", "authorization_code"), ("code", code), ("redirect_uri", &cfg.redirect_uri)]),
    )
}

pub fn refresh_request(cfg: &PsnConfig, refresh_token: &str) -> HttpRequest {
    token_request(
        cfg,
        form(&[("grant_type", "refresh_token"), ("refresh_token", refresh_token), ("scope", &cfg.scope)]),
    )
}

/// `GET <token_url>/<access_token>` returns the account behind a token.
pub fn account_request(cfg: &PsnConfig, access_token: &str) -> HttpRequest {
    HttpRequest {
        method: "GET",
        url: format!("{}/{}", cfg.token_url, url_encode(access_token)),
        headers: vec![("Authorization".into(), basic_auth(cfg)), ("Accept".into(), "application/json".into())],
        body: vec![],
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
    /// Lifetime in seconds as reported by the server.
    pub expires_in: Option<u64>,
}

impl std::fmt::Debug for Tokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tokens")
            .field("access_token", &"<redacted>")
            .field("refresh_token", &self.refresh_token.as_ref().map(|_| "<redacted>"))
            .field("expires_in", &self.expires_in)
            .finish()
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum PsnError {
    /// The server refused (bad/expired code or token, wrong credentials). Retrying will not help.
    Rejected { status: u16 },
    /// Server trouble or an unusable answer; the caller may retry later.
    Unavailable { status: u16 },
    /// The answer was not what the protocol says.
    BadResponse(&'static str),
    /// Transport failure (no network, TLS, timeout).
    Network(String),
}

impl std::fmt::Display for PsnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PsnError::Rejected { status } => write!(f, "PSN refused the request (HTTP {status})"),
            PsnError::Unavailable { status } => write!(f, "PSN is unavailable (HTTP {status})"),
            PsnError::BadResponse(w) => write!(f, "unexpected PSN response: {w}"),
            PsnError::Network(e) => write!(f, "network error: {e}"),
        }
    }
}
impl std::error::Error for PsnError {}

/// 400, 401 and 403 are a stated refusal; everything else non-2xx is treated as temporary.
fn check(resp: &HttpResponse) -> Result<(), PsnError> {
    match resp.status {
        200..=299 => Ok(()),
        400 | 401 | 403 => Err(PsnError::Rejected { status: resp.status }),
        s => Err(PsnError::Unavailable { status: s }),
    }
}

#[derive(Deserialize)]
struct TokenJson {
    access_token: Option<String>,
    refresh_token: Option<String>,
    expires_in: Option<u64>,
}

pub fn parse_tokens(resp: &HttpResponse) -> Result<Tokens, PsnError> {
    check(resp)?;
    let j: TokenJson =
        serde_json::from_slice(&resp.body).map_err(|_| PsnError::BadResponse("token response is not JSON"))?;
    let access_token = j.access_token.filter(|t| !t.is_empty()).ok_or(PsnError::BadResponse("no access token"))?;
    Ok(Tokens { access_token, refresh_token: j.refresh_token.filter(|t| !t.is_empty()), expires_in: j.expires_in })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    pub online_id: String,
    /// PSN account id: an unsigned 64-bit integer (sent little-endian during console registration).
    pub account_id: u64,
}

/// Unsigned decimal, like `Long.parseUnsignedLong`; zero means "not set" and is rejected.
pub fn parse_account_id(s: &str) -> Option<u64> {
    s.trim().parse::<u64>().ok().filter(|v| *v != 0)
}

#[derive(Deserialize)]
struct AccountJson {
    online_id: Option<String>,
    user_id: Option<String>,
}

pub fn parse_account(resp: &HttpResponse) -> Result<Account, PsnError> {
    check(resp)?;
    let j: AccountJson =
        serde_json::from_slice(&resp.body).map_err(|_| PsnError::BadResponse("account response is not JSON"))?;
    let account_id =
        j.user_id.as_deref().and_then(parse_account_id).ok_or(PsnError::BadResponse("no usable account id"))?;
    Ok(Account { online_id: j.online_id.unwrap_or_default(), account_id })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::test_config;

    #[test]
    fn url_encoding_roundtrip() {
        assert_eq!(url_encode("a b&c=d/é~-._"), "a%20b%26c%3Dd%2F%C3%A9~-._");
        assert_eq!(url_decode(&url_encode("a b&c=d/é+")), "a b&c=d/é+");
        assert_eq!(url_decode("x+y%20z%"), "x y z%"); // '+' is a space; a dangling % is kept
        assert_eq!(url_decode("%zz"), "%zz");
        // regression (found by fuzzing): '%' followed by a multibyte character must not panic
        assert_eq!(url_decode("%é1"), "%é1");
        assert_eq!(url_decode("a%\u{1F600}"), "a%\u{1F600}");
    }

    struct Fixed(u8);
    impl RngCore for Fixed {
        fn next_u32(&mut self) -> u32 {
            self.0 as u32
        }
        fn next_u64(&mut self) -> u64 {
            self.0 as u64
        }
        fn fill_bytes(&mut self, d: &mut [u8]) {
            d.fill(self.0)
        }
        fn try_fill_bytes(&mut self, d: &mut [u8]) -> Result<(), rand_core::Error> {
            self.fill_bytes(d);
            Ok(())
        }
    }

    #[test]
    fn authorize_url_has_the_expected_parameters() {
        let duid = new_duid(&mut Fixed(0xAB));
        assert_eq!(duid, format!("{DUID_PREFIX}{}", "ab".repeat(16)));
        let u = authorize_url(&test_config(), &duid);
        assert!(u.starts_with("https://auth.example.test/2.0/oauth/authorize?service_entity=urn:service-entity:psn&response_type=code&client_id=test-client&"));
        assert!(u.contains("redirect_uri=https://example.test/redirect&"));
        assert!(u.contains(&format!("duid={duid}&")));
        assert!(u.contains("scope=psn:clientapp%20psn:other&"));
        assert!(!u.contains("test-secret")); // the secret never goes in a browser URL
    }

    #[test]
    fn redirect_parsing() {
        let c = test_config();
        assert_eq!(
            parse_redirect(&c, "https://example.test/redirect?code=abc%2Bdef&x=1"),
            RedirectOutcome::Code("abc+def".into())
        );
        assert_eq!(
            parse_redirect(&c, "https://example.test/redirect?error=access_denied"),
            RedirectOutcome::Denied("access_denied".into())
        );
        assert_eq!(parse_redirect(&c, "https://example.test/redirect"), RedirectOutcome::NotRedirect);
        assert_eq!(parse_redirect(&c, "https://example.test/redirect?code="), RedirectOutcome::NotRedirect);
        assert_eq!(parse_redirect(&c, "https://evil.test/redirect?code=abc"), RedirectOutcome::NotRedirect); // other hosts never yield a code
        assert_eq!(
            parse_redirect(&c, "https://example.test/redirect?code=zz#frag"),
            RedirectOutcome::Code("zz".into())
        );
    }

    #[test]
    fn the_redirect_must_match_at_a_boundary_not_just_as_a_string_prefix() {
        let mut c = test_config();
        c.redirect_uri = "https://example.test".into(); // a redirect URI without a path
        for bad in [
            "https://example.test.evil.test/?code=abc",
            "https://example.testing/?code=abc",
            "https://example.test@evil.test/?code=abc",
            "https://example.test:8443/?code=abc",
        ] {
            assert!(!is_redirect(&c, bad), "{bad}");
            assert_eq!(parse_redirect(&c, bad), RedirectOutcome::NotRedirect, "{bad}");
        }
        for good in [
            "https://example.test",
            "https://example.test/?code=a",
            "https://example.test?code=a",
            "https://example.test#x",
        ] {
            assert!(is_redirect(&c, good), "{good}");
        }
        // ordinary pages whose URL merely contains the text `code=` are not the redirect
        let c = test_config();
        for page in [
            "https://auth.example.test/signin?error_code=5&x=1",
            "https://auth.example.test/signin?country_code=US",
            "https://auth.example.test/redirectX?code=abc",
        ] {
            assert!(!is_redirect(&c, page), "{page}");
        }
    }

    #[test]
    fn token_requests() {
        let c = test_config();
        let r = exchange_code_request(&c, "co de&1");
        assert_eq!((r.method, r.url.as_str()), ("POST", "https://auth.example.test/2.0/oauth/token"));
        assert_eq!(r.header("Authorization"), Some("Basic dGVzdC1jbGllbnQ6dGVzdC1zZWNyZXQ=")); // base64("test-client:test-secret")
        assert_eq!(r.header("Content-Type"), Some("application/x-www-form-urlencoded"));
        assert_eq!(
            String::from_utf8(r.body).unwrap(),
            "grant_type=authorization_code&code=co%20de%261&redirect_uri=https%3A%2F%2Fexample.test%2Fredirect"
        );
        let r = refresh_request(&c, "rt");
        assert_eq!(
            String::from_utf8(r.body).unwrap(),
            "grant_type=refresh_token&refresh_token=rt&scope=psn%3Aclientapp%20psn%3Aother"
        );
        let a = account_request(&c, "tok/en");
        assert_eq!((a.method, a.url.as_str()), ("GET", "https://auth.example.test/2.0/oauth/token/tok%2Fen"));
        assert!(a.header("Authorization").unwrap().starts_with("Basic "));
    }

    fn resp(status: u16, body: &str) -> HttpResponse {
        HttpResponse { status, headers: vec![], body: body.as_bytes().to_vec() }
    }

    #[test]
    fn token_response_parsing_and_error_classes() {
        let t = parse_tokens(&resp(200, r#"{"access_token":"A","refresh_token":"R","expires_in":3599,"scope":"x"}"#))
            .unwrap();
        assert_eq!((t.access_token.as_str(), t.refresh_token.as_deref(), t.expires_in), ("A", Some("R"), Some(3599)));
        let t = parse_tokens(&resp(200, r#"{"access_token":"A","refresh_token":""}"#)).unwrap();
        assert_eq!((t.refresh_token, t.expires_in), (None, None));
        assert_eq!(parse_tokens(&resp(200, "{}")), Err(PsnError::BadResponse("no access token")));
        assert_eq!(parse_tokens(&resp(200, "nope")), Err(PsnError::BadResponse("token response is not JSON")));
        for s in [400, 401, 403] {
            assert_eq!(parse_tokens(&resp(s, "")), Err(PsnError::Rejected { status: s }));
        }
        for s in [429, 500, 503, 302] {
            assert_eq!(parse_tokens(&resp(s, "")), Err(PsnError::Unavailable { status: s }));
        }
        let shown = format!(
            "{:?}",
            Tokens { access_token: "SECRET-A".into(), refresh_token: Some("SECRET-R".into()), expires_in: Some(1) }
        );
        assert!(!shown.contains("SECRET"));
    }

    #[test]
    fn account_parsing_handles_the_full_unsigned_range() {
        let a = parse_account(&resp(200, r#"{"online_id":"Player_1","user_id":"18446744073709551615"}"#)).unwrap(); // u64::MAX
        assert_eq!((a.online_id.as_str(), a.account_id), ("Player_1", u64::MAX));
        assert_eq!(parse_account(&resp(200, r#"{"user_id":"0"}"#)), Err(PsnError::BadResponse("no usable account id")));
        assert_eq!(
            parse_account(&resp(200, r#"{"user_id":"-5"}"#)),
            Err(PsnError::BadResponse("no usable account id"))
        );
        assert_eq!(
            parse_account(&resp(200, r#"{"user_id":"abc"}"#)),
            Err(PsnError::BadResponse("no usable account id"))
        );
        assert_eq!(
            parse_account(&resp(200, r#"{"online_id":"x"}"#)),
            Err(PsnError::BadResponse("no usable account id"))
        );
        assert_eq!(parse_account_id(" 42 "), Some(42));
    }
}
