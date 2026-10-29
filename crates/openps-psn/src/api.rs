//! The PSN API (as opposed to the Remote Play login): tokens for the "mobile app" client minted from an **npsso**, and the list of the
//! account's PS5 consoles. Remote registration needs a console's `duid` from that list (spec 09, stages 1-2).
//!
//! The npsso is a 60-day credential for the whole PSN account: it is used once to obtain tokens and is **never stored or logged**; only
//! the resulting access/refresh tokens are kept (in the secret store).

use crate::config::ApiConfig;
use crate::http::{HttpClient, HttpRequest, HttpResponse};
use crate::oauth::{url_encode, PsnError};
use crate::store::SecretStore;
use serde::{Deserialize, Serialize};

const TOKENS_KEY: &str = "psn.api_tokens";
pub const CLIENTS_URL: &str = "https://web.np.playstation.com/api/cloudAssistedNavigation/v2/users/me/clients";
/// Refresh this many seconds before the access token expires.
const REFRESH_MARGIN_S: u64 = 120;
const PAGE: usize = 10;
const MAX_PAGES: usize = 10;

#[derive(Debug, PartialEq, Eq)]
pub enum ApiError {
    /// Not signed in to the PSN API (or the tokens are dead): ask for the npsso again.
    NotSignedIn,
    /// The pasted text does not contain an npsso.
    BadNpsso,
    Psn(PsnError),
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiError::NotSignedIn => f.write_str("sign in to PlayStation again (paste a fresh login token)"),
            ApiError::BadNpsso => f.write_str("that does not look like a PlayStation login token (npsso)"),
            ApiError::Psn(e) => write!(f, "{e}"),
        }
    }
}
impl std::error::Error for ApiError {}
impl From<PsnError> for ApiError {
    fn from(e: PsnError) -> Self {
        ApiError::Psn(e)
    }
}

/// What the user pasted -> the npsso. Accepts the bare token, a quoted token, or the whole JSON page
/// (`{"npsso": "...", "expires_in": ...}`) that `ssocookie` shows. Anything else is rejected before it goes near the network.
pub fn extract_npsso(text: &str) -> Option<String> {
    let t = text.trim();
    if t.len() > 4096 {
        return None;
    }
    let candidate = if t.starts_with('{') {
        serde_json::from_str::<serde_json::Value>(t).ok()?.get("npsso")?.as_str()?.trim().to_string()
    } else {
        t.trim_matches(|c| c == '"' || c == '\'').trim().to_string()
    };
    let ok = (32..=128).contains(&candidate.len())
        && candidate.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    ok.then_some(candidate)
}

/// `.../authorize?...` with the npsso as a cookie; the answer is a redirect whose `Location` carries the access code.
pub fn authorize_request(api: &ApiConfig, npsso: &str) -> HttpRequest {
    HttpRequest {
        method: "GET",
        url: format!(
            "{}/authorize?access_type=offline&client_id={}&redirect_uri={}&response_type=code&scope={}",
            api.auth_base,
            url_encode(&api.client_id),
            url_encode(&api.redirect_uri),
            url_encode(&api.scope)
        ),
        headers: vec![("Cookie".into(), format!("npsso={npsso}"))],
        body: vec![],
    }
}

/// `Location` header -> the `code` parameter.
pub fn code_from_location(location: &str) -> Option<String> {
    let (_, q) = location.split_once('?')?;
    q.split('&').find_map(|p| match p.split_once('=') {
        Some(("code", v)) if !v.is_empty() => Some(crate::oauth::url_decode(v)),
        _ => None,
    })
}

pub fn parse_authorize(resp: &HttpResponse) -> Result<String, PsnError> {
    match resp.header("Location").and_then(code_from_location) {
        Some(c) => Ok(c),
        None => match resp.status {
            400 | 401 | 403 => Err(PsnError::Rejected { status: resp.status }),
            // 200 with no redirect, or a redirect without a code (for example to a sign-in page): the npsso is not valid
            200..=399 => Err(PsnError::Rejected { status: resp.status }),
            s => Err(PsnError::Unavailable { status: s }),
        },
    }
}

fn form(pairs: &[(&str, &str)]) -> Vec<u8> {
    pairs.iter().map(|(k, v)| format!("{}={}", url_encode(k), url_encode(v))).collect::<Vec<_>>().join("&").into_bytes()
}

fn token_post(api: &ApiConfig, body: Vec<u8>) -> HttpRequest {
    HttpRequest {
        method: "POST",
        url: format!("{}/token", api.auth_base),
        headers: vec![
            ("Authorization".into(), api.basic_auth.clone()),
            ("Content-Type".into(), "application/x-www-form-urlencoded".into()),
        ],
        body,
    }
}

pub fn exchange_request(api: &ApiConfig, code: &str) -> HttpRequest {
    token_post(
        api,
        form(&[
            ("code", code),
            ("redirect_uri", &api.redirect_uri),
            ("grant_type", "authorization_code"),
            ("token_format", "jwt"),
        ]),
    )
}

pub fn refresh_request(api: &ApiConfig, refresh_token: &str) -> HttpRequest {
    token_post(
        api,
        form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("scope", &api.scope),
            ("token_format", "jwt"),
        ]),
    )
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiTokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
    /// Unix seconds, when the server said how long the access token lives.
    pub expires_at: Option<u64>,
}

impl std::fmt::Debug for ApiTokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ApiTokens(<redacted>)")
    }
}

#[derive(Deserialize)]
struct TokenJson {
    access_token: Option<String>,
    refresh_token: Option<String>,
    expires_in: Option<u64>,
}

pub(crate) fn check(resp: &HttpResponse) -> Result<(), PsnError> {
    match resp.status {
        200..=299 => Ok(()),
        400 | 401 | 403 => Err(PsnError::Rejected { status: resp.status }),
        s => Err(PsnError::Unavailable { status: s }),
    }
}

pub fn parse_tokens(resp: &HttpResponse, now: u64) -> Result<ApiTokens, PsnError> {
    check(resp)?;
    let j: TokenJson =
        serde_json::from_slice(&resp.body).map_err(|_| PsnError::BadResponse("token response is not JSON"))?;
    let access_token = j.access_token.filter(|t| !t.is_empty()).ok_or(PsnError::BadResponse("no access token"))?;
    Ok(ApiTokens {
        access_token,
        refresh_token: j.refresh_token.filter(|t| !t.is_empty()),
        expires_at: j.expires_in.map(|e| now.saturating_add(e)),
    })
}

/// One of the account's consoles as the PSN API lists it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PsnConsole {
    /// The console's id in PSN: what remote registration addresses it by.
    pub duid: String,
    pub name: String,
    pub platform: String,
    pub updated: Option<String>,
}

#[derive(Deserialize)]
struct ClientsJson {
    clients: Option<Vec<ClientJson>>,
}
#[derive(Deserialize)]
struct ClientJson {
    duid: Option<String>,
    platform: Option<String>,
    #[serde(rename = "updatedDateTime")]
    updated: Option<String>,
    device: Option<DeviceJson>,
}
#[derive(Deserialize)]
struct DeviceJson {
    name: Option<String>,
    #[serde(rename = "updatedDateTime")]
    updated: Option<String>,
}

pub fn clients_request(access_token: &str, offset: usize) -> HttpRequest {
    HttpRequest {
        method: "GET",
        url: format!("{CLIENTS_URL}?platform=PS5&includeFields=device&limit={PAGE}&offset={offset}"),
        headers: vec![
            ("Accept-Language".into(), "ja-JP".into()),
            ("User-Agent".into(), "RpNetHttpUtilImpl".into()),
            ("Authorization".into(), format!("Bearer {access_token}")),
        ],
        body: vec![],
    }
}

/// Names come from the network and end up on screen: keep them short and printable.
fn clean(s: &str, max: usize) -> String {
    crate::oauth::clean_text(s, max)
}

/// One page of the list. Clients without a `duid` are unusable and dropped.
pub fn parse_clients(resp: &HttpResponse) -> Result<Vec<PsnConsole>, PsnError> {
    check(resp)?;
    let j: ClientsJson =
        serde_json::from_slice(&resp.body).map_err(|_| PsnError::BadResponse("console list is not JSON"))?;
    Ok(j.clients
        .unwrap_or_default()
        .into_iter()
        .filter_map(|c| {
            let duid = c
                .duid
                .as_deref()
                .map(str::trim)
                .filter(|d| !d.is_empty() && d.len() <= 128 && d.is_ascii())?
                .to_string();
            let name = c.device.as_ref().and_then(|d| d.name.clone()).map(|n| clean(&n, 64)).filter(|n| !n.is_empty());
            let platform = clean(c.platform.as_deref().unwrap_or("PS5"), 16);
            Some(PsnConsole {
                updated: c.device.and_then(|d| d.updated).or(c.updated).map(|u| clean(&u, 40)),
                name: name.unwrap_or_else(|| platform.clone()),
                platform,
                duid,
            })
        })
        .collect())
}

/// The PSN API session: tokens in the secret store, minted from an npsso.
pub struct Api<'a, H: HttpClient + ?Sized, S: SecretStore + ?Sized> {
    pub cfg: &'a ApiConfig,
    pub http: &'a H,
    pub store: &'a S,
}

impl<H: HttpClient + ?Sized, S: SecretStore + ?Sized> Api<'_, H, S> {
    pub(crate) fn call(&self, req: &HttpRequest) -> Result<HttpResponse, PsnError> {
        self.http.execute(req).map_err(|e| PsnError::Network(e.to_string()))
    }

    fn save(&self, t: &ApiTokens) -> Result<(), ApiError> {
        self.store
            .set(TOKENS_KEY, &serde_json::to_string(t).expect("serialisable"))
            .map_err(|e| ApiError::Psn(PsnError::Network(e)))
    }

    fn load(&self) -> Option<ApiTokens> {
        self.store.get(TOKENS_KEY).and_then(|j| serde_json::from_str(&j).ok())
    }

    pub fn is_signed_in(&self) -> bool {
        self.load().is_some()
    }

    pub fn sign_out(&self) {
        let _ = self.store.delete(TOKENS_KEY);
    }

    /// Mint tokens from what the user pasted. The npsso is used for one request and dropped.
    pub fn sign_in(&self, pasted: &str, now: u64) -> Result<(), ApiError> {
        let npsso = extract_npsso(pasted).ok_or(ApiError::BadNpsso)?;
        let code = parse_authorize(&self.call(&authorize_request(self.cfg, &npsso))?)?;
        let tokens = parse_tokens(&self.call(&exchange_request(self.cfg, &code))?, now)?;
        self.save(&tokens)
    }

    /// A usable access token, refreshing first when it is about to expire.
    pub(crate) fn access_token(&self, now: u64, force_refresh: bool) -> Result<String, ApiError> {
        let t = self.load().ok_or(ApiError::NotSignedIn)?;
        let expiring = t.expires_at.is_some_and(|e| now.saturating_add(REFRESH_MARGIN_S) >= e);
        if !expiring && !force_refresh {
            return Ok(t.access_token);
        }
        let Some(rt) = t.refresh_token.clone() else {
            self.sign_out();
            return Err(ApiError::NotSignedIn);
        };
        match parse_tokens(&self.call(&refresh_request(self.cfg, &rt))?, now) {
            Ok(mut n) => {
                n.refresh_token.get_or_insert(rt); // some servers do not rotate it
                self.save(&n)?;
                Ok(n.access_token)
            }
            // A 403 is as likely a portal/WAF/rate limiter as a dead token: keep the login.
            Err(PsnError::Rejected { status: 403 }) => Err(PsnError::Unavailable { status: 403 }.into()),
            Err(PsnError::Rejected { .. }) => {
                // Compare-and-delete: if a concurrent refresh already rotated the token, keep the winner's login.
                if let Some(cur) = self.load() {
                    if cur.refresh_token.as_deref() != Some(rt.as_str()) {
                        return Ok(cur.access_token);
                    }
                }
                self.sign_out(); // dead: forget it so the UI asks again
                Err(ApiError::NotSignedIn)
            }
            Err(e) => Err(e.into()),
        }
    }

    /// The account's PS5 consoles. An access token that the server turns down is refreshed once, then the sign-in is dropped.
    pub fn consoles(&self, now: u64) -> Result<Vec<PsnConsole>, ApiError> {
        let mut token = self.access_token(now, false)?;
        let mut refreshed = false;
        let mut out: Vec<PsnConsole> = vec![];
        let mut offset = 0;
        for _ in 0..MAX_PAGES {
            let resp = self.call(&clients_request(&token, offset))?;
            if matches!(resp.status, 401 | 403) {
                if refreshed {
                    self.sign_out();
                    return Err(ApiError::NotSignedIn);
                }
                token = self.access_token(now, true)?;
                refreshed = true;
                continue; // same page again
            }
            let raw_len =
                serde_json::from_slice::<ClientsJson>(&resp.body).ok().and_then(|j| j.clients).map_or(0, |c| c.len());
            let page = parse_clients(&resp)?;
            let before = out.len();
            for c in page {
                if !out.iter().any(|x| x.duid == c.duid) {
                    out.push(c);
                }
            }
            if raw_len < PAGE || out.len() == before {
                break;
            }
            offset += raw_len;
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MemoryStore;
    use std::cell::RefCell;
    use std::collections::VecDeque;

    fn api_cfg() -> ApiConfig {
        ApiConfig {
            client_id: "mobile-client".into(),
            basic_auth: "Basic dGVzdDp0ZXN0".into(),
            redirect_uri: "com.example.app://redirect".into(),
            scope: "psn:mobile.v2.core psn:clientapp".into(),
            auth_base: "https://auth.example.test/api/authz/v3/oauth".into(),
        }
    }

    struct Script {
        replies: RefCell<VecDeque<std::io::Result<HttpResponse>>>,
        seen: RefCell<Vec<HttpRequest>>,
    }
    impl Script {
        fn new(r: Vec<std::io::Result<HttpResponse>>) -> Self {
            Self { replies: RefCell::new(r.into()), seen: RefCell::new(vec![]) }
        }
    }
    impl HttpClient for Script {
        fn execute(&self, req: &HttpRequest) -> std::io::Result<HttpResponse> {
            self.seen.borrow_mut().push(req.clone());
            self.replies.borrow_mut().pop_front().expect("unexpected extra request")
        }
    }
    fn resp(status: u16, headers: &[(&str, &str)], body: &str) -> std::io::Result<HttpResponse> {
        Ok(HttpResponse {
            status,
            headers: headers.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            body: body.as_bytes().to_vec(),
        })
    }

    const NPSSO: &str = "AbCdEfGhIjKlMnOpQrStUvWxYz0123456789AbCdEfGhIjKlMnOpQrStUvWxYz01";
    const TOKENS: &str = r#"{"access_token":"AT1","refresh_token":"RT1","expires_in":3600}"#;
    const CLIENTS: &str = r#"{"clients":[{"duid":"0000000700410080a0112233445566778899aabbccddeeff","platform":"PS5","updatedDateTime":"2026-10-20T10:00:00Z","device":{"language":"en","name":"PS5-591","updatedDateTime":"2026-10-22T09:00:00Z"}}]}"#;

    #[test]
    fn the_npsso_is_found_in_what_the_user_pasted() {
        assert_eq!(extract_npsso(NPSSO).as_deref(), Some(NPSSO));
        assert_eq!(extract_npsso(&format!("  \"{NPSSO}\"\n")).as_deref(), Some(NPSSO));
        assert_eq!(
            extract_npsso(&format!("{{\n  \"npsso\": \"{NPSSO}\",\n  \"expires_in\": 5183973\n}}")).as_deref(),
            Some(NPSSO)
        );
        for bad in [
            "",
            "short",
            "{}",
            r#"{"npsso":5}"#,
            "not a token with spaces and symbols !!!!!!!!!!!!!!!!",
            &"x".repeat(5000),
            "{\"npsso\":\"a b\"}",
        ] {
            assert_eq!(extract_npsso(bad), None, "{bad:.30}");
        }
    }

    #[test]
    fn sign_in_follows_the_original_request_shapes_and_never_stores_the_npsso() {
        let http = Script::new(vec![
            resp(302, &[("Location", "com.example.app://redirect/?code=ACODE&cid=1")], ""),
            resp(200, &[], TOKENS),
        ]);
        let store = MemoryStore::default();
        let cfg = api_cfg();
        let api = Api { cfg: &cfg, http: &http, store: &store };
        api.sign_in(NPSSO, 1000).unwrap();
        let seen = http.seen.borrow();
        let a = &seen[0];
        assert_eq!(a.method, "GET");
        assert!(a.url.starts_with("https://auth.example.test/api/authz/v3/oauth/authorize?access_type=offline&client_id=mobile-client&redirect_uri=com.example.app%3A%2F%2Fredirect&response_type=code&scope=psn%3Amobile.v2.core%20psn%3Aclientapp"), "{}", a.url);
        assert_eq!(a.header("Cookie"), Some(format!("npsso={NPSSO}").as_str()));
        let t = &seen[1];
        assert_eq!((t.method, t.url.as_str()), ("POST", "https://auth.example.test/api/authz/v3/oauth/token"));
        assert_eq!(t.header("Authorization"), Some("Basic dGVzdDp0ZXN0"));
        assert_eq!(t.header("Content-Type"), Some("application/x-www-form-urlencoded"));
        assert_eq!(
            String::from_utf8_lossy(&t.body),
            "code=ACODE&redirect_uri=com.example.app%3A%2F%2Fredirect&grant_type=authorization_code&token_format=jwt"
        );
        // the only thing stored is the tokens
        let stored = store.get(TOKENS_KEY).unwrap();
        assert!(stored.contains("AT1") && stored.contains("RT1") && !stored.contains(NPSSO));
        assert!(!format!("{:?}", api.load().unwrap()).contains("AT1"), "tokens never print");
    }

    #[test]
    fn bad_input_and_refusals_are_clear_errors_and_store_nothing() {
        let cfg = api_cfg();
        let store = MemoryStore::default();
        let http = Script::new(vec![]);
        assert_eq!(Api { cfg: &cfg, http: &http, store: &store }.sign_in("nope", 0), Err(ApiError::BadNpsso)); // no request made
        for (reply, want) in [
            (resp(302, &[("Location", "https://x/signin")], ""), ApiError::Psn(PsnError::Rejected { status: 302 })), // not signed in: no code
            (resp(401, &[], ""), ApiError::Psn(PsnError::Rejected { status: 401 })),
            (resp(503, &[], ""), ApiError::Psn(PsnError::Unavailable { status: 503 })),
        ] {
            let http = Script::new(vec![reply]);
            assert_eq!(Api { cfg: &cfg, http: &http, store: &store }.sign_in(NPSSO, 0), Err(want));
        }
        assert!(store.get(TOKENS_KEY).is_none());
    }

    #[test]
    fn the_console_list_is_parsed_cleaned_and_paged() {
        let cfg = api_cfg();
        let store = MemoryStore::default();
        store.set(TOKENS_KEY, r#"{"access_token":"AT1","refresh_token":"RT1","expires_at":99999}"#).unwrap();
        let http = Script::new(vec![resp(200, &[], CLIENTS)]);
        let list = Api { cfg: &cfg, http: &http, store: &store }.consoles(1000).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!((list[0].name.as_str(), list[0].platform.as_str()), ("PS5-591", "PS5"));
        assert_eq!(list[0].duid, "0000000700410080a0112233445566778899aabbccddeeff");
        let r = &http.seen.borrow()[0];
        assert_eq!(r.url, format!("{CLIENTS_URL}?platform=PS5&includeFields=device&limit=10&offset=0"));
        assert_eq!(r.header("Authorization"), Some("Bearer AT1"));
        // hostile content: control characters stripped, clients without a usable duid dropped, duplicates merged
        let hostile = r#"{"clients":[{"duid":"","device":{"name":"x"}},{"duid":"abc","device":{"name":"Bad\u0000\n\u001bName"}},{"duid":"abc","device":{"name":"dup"}},{"duid":"é","device":{"name":"non-ascii id"}},{"platform":"PS5"}]}"#;
        let http = Script::new(vec![resp(200, &[], hostile)]);
        let list = Api { cfg: &cfg, http: &http, store: &store }.consoles(1000).unwrap();
        assert_eq!(
            list.iter().map(|c| (c.duid.as_str(), c.name.as_str())).collect::<Vec<_>>(),
            vec![("abc", "BadName")]
        );
        // a full page asks for the next one
        let page: String =
            (0..10).map(|i| format!(r#"{{"duid":"d{i}","device":{{"name":"n{i}"}}}}"#)).collect::<Vec<_>>().join(",");
        let http = Script::new(vec![
            resp(200, &[], &format!(r#"{{"clients":[{page}]}}"#)),
            resp(200, &[], r#"{"clients":[]}"#),
        ]);
        assert_eq!(Api { cfg: &cfg, http: &http, store: &store }.consoles(1000).unwrap().len(), 10);
        assert!(http.seen.borrow()[1].url.ends_with("offset=10"));
        // junk
        for bad in ["", "not json", "[]"] {
            let http = Script::new(vec![resp(200, &[], bad)]);
            assert!(Api { cfg: &cfg, http: &http, store: &store }.consoles(1000).is_err(), "{bad}");
        }
    }

    #[test]
    fn an_expired_token_is_refreshed_and_a_dead_one_drops_the_sign_in() {
        let cfg = api_cfg();
        let store = MemoryStore::default();
        store.set(TOKENS_KEY, r#"{"access_token":"OLD","refresh_token":"RT1","expires_at":1000}"#).unwrap();
        // expired by the clock: refresh first, then the list
        let http =
            Script::new(vec![resp(200, &[], r#"{"access_token":"NEW","expires_in":3600}"#), resp(200, &[], CLIENTS)]);
        Api { cfg: &cfg, http: &http, store: &store }.consoles(5000).unwrap();
        let seen = http.seen.borrow();
        assert!(String::from_utf8_lossy(&seen[0].body).starts_with("grant_type=refresh_token&refresh_token=RT1&scope="));
        assert_eq!(seen[1].header("Authorization"), Some("Bearer NEW"));
        assert!(
            store.get(TOKENS_KEY).unwrap().contains("RT1"),
            "the refresh token is kept when the server does not rotate it"
        );
        drop(seen);
        // the server turns the token down: one refresh, then the page again
        let http = Script::new(vec![
            resp(401, &[], ""),
            resp(200, &[], r#"{"access_token":"NEW2","expires_in":3600}"#),
            resp(200, &[], CLIENTS),
        ]);
        assert_eq!(Api { cfg: &cfg, http: &http, store: &store }.consoles(5000).unwrap().len(), 1);
        // refused again after refreshing, or refresh refused: signed out
        let http =
            Script::new(vec![resp(401, &[], ""), resp(200, &[], r#"{"access_token":"N3"}"#), resp(401, &[], "")]);
        assert_eq!(Api { cfg: &cfg, http: &http, store: &store }.consoles(5000), Err(ApiError::NotSignedIn));
        assert!(store.get(TOKENS_KEY).is_none());
        store.set(TOKENS_KEY, r#"{"access_token":"A","refresh_token":"R","expires_at":1}"#).unwrap();
        let http = Script::new(vec![resp(400, &[], "")]);
        assert_eq!(Api { cfg: &cfg, http: &http, store: &store }.consoles(5000), Err(ApiError::NotSignedIn));
        assert!(!Api { cfg: &cfg, http: &http, store: &store }.is_signed_in());
        // never signed in: no request at all
        let http = Script::new(vec![]);
        assert_eq!(Api { cfg: &cfg, http: &http, store: &store }.consoles(5000), Err(ApiError::NotSignedIn));
    }

    #[test]
    fn transport_failures_are_network_errors() {
        let cfg = api_cfg();
        let store = MemoryStore::default();
        let http = Script::new(vec![Err(std::io::Error::other("no route"))]);
        assert!(matches!(
            Api { cfg: &cfg, http: &http, store: &store }.sign_in(NPSSO, 0),
            Err(ApiError::Psn(PsnError::Network(_)))
        ));
    }
}
