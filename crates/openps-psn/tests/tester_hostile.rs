//! Independent tester suite for openps-psn: hostile server answers, token expiry edge cases, refresh races, art-URL
//! allow-list bypass attempts, pagination loops, secret-store corruption. Every test uses a scripted fake HTTP client
//! (no network). Tests marked `#[ignore = "BUG T04-xx ..."]` demonstrate a confirmed defect: un-ignore after the fix.
use openps_psn::api::{self, Api};
use openps_psn::client::PsnClient;
use openps_psn::config::{ApiConfig, PsnConfig};
use openps_psn::http::{HttpClient, HttpRequest, HttpResponse};
use openps_psn::library;
use openps_psn::oauth::{self, PsnError};
use openps_psn::store::{FileStore, MemoryStore, SecretStore};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

fn cfg() -> PsnConfig {
    PsnConfig {
        client_id: "c".into(),
        client_secret: "s".into(),
        token_url: "https://auth.example.test/2.0/oauth/token".into(),
        redirect_uri: "https://example.test/redirect".into(),
        scope: "a b".into(),
        api: None,
    }
}

fn api_cfg() -> ApiConfig {
    ApiConfig {
        client_id: "m".into(),
        basic_auth: "Basic eDp5".into(),
        redirect_uri: "com.example://r".into(),
        scope: "s".into(),
        auth_base: "https://auth.example.test/api/authz/v3/oauth".into(),
    }
}

fn resp(status: u16, body: &str) -> HttpResponse {
    HttpResponse { status, headers: vec![], body: body.as_bytes().to_vec() }
}

/// Scripted HTTP: queued replies, every request recorded.
#[derive(Default)]
struct Script {
    replies: RefCell<VecDeque<std::io::Result<HttpResponse>>>,
    seen: RefCell<Vec<HttpRequest>>,
}
impl Script {
    fn with(r: Vec<HttpResponse>) -> Self {
        Self { replies: RefCell::new(r.into_iter().map(Ok).collect()), seen: RefCell::default() }
    }
}
impl HttpClient for Script {
    fn execute(&self, req: &HttpRequest) -> std::io::Result<HttpResponse> {
        self.seen.borrow_mut().push(req.clone());
        self.replies.borrow_mut().pop_front().unwrap_or_else(|| Err(std::io::Error::other("script exhausted")))
    }
}

impl HttpClient for &Script {
    fn execute(&self, req: &HttpRequest) -> std::io::Result<HttpResponse> {
        (**self).execute(req)
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// Token expiry arithmetic
// ---------------------------------------------------------------------------------------------------------------------

const ACCOUNT: &str = r#"{"online_id":"P","user_id":"42"}"#;

#[test]
fn expires_in_zero_means_refresh_on_next_use_and_a_huge_but_safe_value_never_refreshes() {
    let net = Script::with(vec![
        resp(200, r#"{"access_token":"A","refresh_token":"R","expires_in":0}"#),
        resp(200, ACCOUNT),
        resp(200, r#"{"access_token":"B","refresh_token":"R2","expires_in":1000000000000}"#),
    ]);
    let c = PsnClient::new(cfg(), &net, MemoryStore::default());
    c.login("x", 1000).unwrap();
    assert_eq!(c.access_token(1000).unwrap().as_deref(), Some("B"), "expires_in=0 -> already inside the margin");
    assert_eq!(c.access_token(1_000_000).unwrap().as_deref(), Some("B"), "1e12 s lifetime: no further refresh");
    assert_eq!(net.seen.borrow().len(), 3);
}

#[test]
fn clock_going_backwards_does_not_refresh_or_panic() {
    let net = Script::with(vec![
        resp(200, r#"{"access_token":"A","refresh_token":"R","expires_in":3600}"#),
        resp(200, ACCOUNT),
    ]);
    let c = PsnClient::new(cfg(), &net, MemoryStore::default());
    c.login("x", 10_000).unwrap();
    // wall clock jumped back by a day (or unix_now() fell back to 0): token is still considered valid
    assert_eq!(c.access_token(0).unwrap().as_deref(), Some("A"));
    assert_eq!(net.seen.borrow().len(), 2);
}

#[test]
fn the_margin_boundary_is_inclusive() {
    let net = Script::with(vec![
        resp(200, r#"{"access_token":"A","refresh_token":"R","expires_in":1000}"#),
        resp(200, ACCOUNT),
        resp(200, r#"{"access_token":"B","expires_in":1000}"#),
    ]);
    let c = PsnClient::new(cfg(), &net, MemoryStore::default());
    c.login("x", 0).unwrap();
    assert_eq!(c.access_token(1000 - 120 - 1).unwrap().as_deref(), Some("A"));
    assert_eq!(c.access_token(1000 - 120).unwrap().as_deref(), Some("B"));
}

#[test]
fn a_hostile_expires_in_must_not_panic_the_login() {
    let body = r#"{"access_token":"A","refresh_token":"R","expires_in":18446744073709551615}"#;
    let r = std::panic::catch_unwind(|| {
        let net = Script::with(vec![resp(200, body), resp(200, ACCOUNT)]);
        let c = PsnClient::new(cfg(), &net, MemoryStore::default());
        let _ = c.login("x", 1_800_000_000);
        // and the API-login path
        let r = api::parse_tokens(&resp(200, body), 1_800_000_000);
        r.is_ok()
    });
    assert!(r.is_ok(), "panicked on a server-controlled number");
}

// ---------------------------------------------------------------------------------------------------------------------
// Refresh race (two callers share one stored refresh token; the server rotates it)
// ---------------------------------------------------------------------------------------------------------------------

/// HTTP fake that, during the FIRST refresh, lets a second caller run a complete refresh (as a second thread would),
/// then answers the first caller as the server would once the refresh token was rotated: 400.
struct Racy {
    client: RefCell<Option<std::rc::Weak<PsnClient<RcHttp, Shared>>>>,
    refreshes: RefCell<u32>,
    nested: RefCell<bool>,
}
struct RcHttp(Rc<Racy>);
#[derive(Clone)]
struct Shared(Rc<MemoryStoreShared>);
struct MemoryStoreShared(MemoryStore);
impl SecretStore for Shared {
    fn get(&self, k: &str) -> Option<String> {
        self.0 .0.get(k)
    }
    fn set(&self, k: &str, v: &str) -> Result<(), String> {
        self.0 .0.set(k, v)
    }
    fn delete(&self, k: &str) -> Result<(), String> {
        self.0 .0.delete(k)
    }
}
impl HttpClient for RcHttp {
    fn execute(&self, req: &HttpRequest) -> std::io::Result<HttpResponse> {
        let body = String::from_utf8_lossy(&req.body).to_string();
        if req.url.ends_with("/token") && body.contains("grant_type=authorization_code") {
            return Ok(resp(200, r#"{"access_token":"A0","refresh_token":"R0","expires_in":10}"#));
        }
        if req.method == "GET" {
            return Ok(resp(200, ACCOUNT));
        }
        // a refresh request
        let n = {
            let mut r = self.0.refreshes.borrow_mut();
            *r += 1;
            *r
        };
        if n == 1 {
            // second caller runs to completion while the first one is "on the wire"
            *self.0.nested.borrow_mut() = true;
            let c = self.0.client.borrow().as_ref().unwrap().upgrade().unwrap();
            let t = c.access_token(1000).unwrap();
            assert_eq!(t.as_deref(), Some("A-new"), "the second caller won the rotation");
            // the first caller's request used the old refresh token, which the server has now rotated away
            return Ok(resp(400, r#"{"error":"invalid_grant"}"#));
        }
        Ok(resp(200, r#"{"access_token":"A-new","refresh_token":"R-new","expires_in":3600}"#))
    }
}

#[test]
fn a_losing_concurrent_refresh_must_not_delete_the_winners_tokens() {
    let racy = Rc::new(Racy { client: RefCell::new(None), refreshes: RefCell::new(0), nested: RefCell::new(false) });
    let store = Shared(Rc::new(MemoryStoreShared(MemoryStore::default())));
    let c = Rc::new(PsnClient::new(cfg(), RcHttp(racy.clone()), store));
    *racy.client.borrow_mut() = Some(Rc::downgrade(&c));
    c.login("code", 0).unwrap();
    // token expires in 10 s: both callers want to refresh
    let _ = c.access_token(1000);
    assert!(*racy.nested.borrow());
    assert!(c.is_logged_in(), "the winner's new tokens were deleted by the loser's logout");
}

// ---------------------------------------------------------------------------------------------------------------------
// Persist failure after a successful refresh
// ---------------------------------------------------------------------------------------------------------------------

struct FailingSet(MemoryStore, std::cell::Cell<bool>);
impl SecretStore for FailingSet {
    fn get(&self, k: &str) -> Option<String> {
        self.0.get(k)
    }
    fn set(&self, k: &str, v: &str) -> Result<(), String> {
        if self.1.get() {
            Err("disk full".into())
        } else {
            self.0.set(k, v)
        }
    }
    fn delete(&self, k: &str) -> Result<(), String> {
        self.0.delete(k)
    }
}

#[test]
fn characterisation_a_failed_save_after_refresh_loses_the_fresh_token_but_keeps_the_old_login() {
    let net = Script::with(vec![
        resp(200, r#"{"access_token":"A","refresh_token":"R","expires_in":10}"#),
        resp(200, ACCOUNT),
        resp(200, r#"{"access_token":"B","refresh_token":"R2","expires_in":3600}"#),
    ]);
    let st = FailingSet(MemoryStore::default(), std::cell::Cell::new(false));
    let c = PsnClient::new(cfg(), &net, st);
    c.login("x", 0).unwrap();
    c.secrets().get("psn.tokens").unwrap();
    // make the store fail from now on
    // (FailingSet is moved into the client, so flip through the accessor)
    // SAFETY-free: the flag is a Cell reachable through secrets()
    // `secrets()` returns &S
    let flag = &c.secrets().1;
    flag.set(true);
    let r = c.access_token(1000);
    assert!(matches!(r, Err(PsnError::Network(ref m)) if m.contains("disk full")), "{r:?}");
    // the rotated refresh token (R2) is now only in the server's records: the stored R may be dead
    assert!(c.secrets().get("psn.tokens").unwrap().contains("\"R\""));
}

#[test]
fn a_403_on_refresh_keeps_the_login() {
    // 403 can come from a captive portal / WAF / rate limiter, not only from an invalid refresh token
    let net = Script::with(vec![
        resp(200, r#"{"access_token":"A","refresh_token":"R","expires_in":10}"#),
        resp(200, ACCOUNT),
        resp(403, "<html>blocked</html>"),
    ]);
    let c = PsnClient::new(cfg(), &net, MemoryStore::default());
    c.login("x", 0).unwrap();
    assert_eq!(c.access_token(1000), Err(PsnError::Unavailable { status: 403 }));
    assert!(c.is_logged_in(), "a 403 may come from a portal or WAF: do not sign the user out");
}

#[test]
fn a_corrupt_stored_token_blob_is_not_reported_as_logged_in() {
    let st = MemoryStore::default();
    st.set("psn.tokens", "{not json").unwrap();
    let c = PsnClient::new(cfg(), Script::default(), st);
    assert!(!c.is_logged_in(), "is_logged_in parses the blob");
    assert_eq!(c.access_token(0).unwrap(), None);
}

// ---------------------------------------------------------------------------------------------------------------------
// Hostile JSON
// ---------------------------------------------------------------------------------------------------------------------

#[test]
fn deeply_nested_and_huge_json_is_an_error_not_a_crash() {
    let deep = format!("{}{}", "[".repeat(200_000), "]".repeat(200_000));
    assert!(oauth::parse_tokens(&resp(200, &deep)).is_err());
    assert!(oauth::parse_account(&resp(200, &deep)).is_err());
    assert!(api::parse_clients(&resp(200, &deep)).is_err());
    assert!(library::parse_games(&resp(200, &deep)).is_err());
    assert!(library::parse_trophy_titles(&resp(200, &deep)).is_err());
    assert!(library::parse_installed(&resp(200, &deep)).is_err());
    assert!(library::parse_status(&resp(200, &deep)).is_err());
    let big = format!(r#"{{"access_token":"{}"}}"#, "A".repeat(8 * 1024 * 1024));
    assert_eq!(oauth::parse_tokens(&resp(200, &big)).unwrap().access_token.len(), 8 * 1024 * 1024);
}

#[test]
fn wrongly_typed_fields_are_handled() {
    for body in [
        r#"{"access_token":5}"#,
        r#"{"access_token":"a","expires_in":-1}"#,
        r#"{"access_token":"a","expires_in":1.5}"#,
        r#"{"access_token":"a","expires_in":"3600"}"#,
        r#"[]"#,
        r#"null"#,
        "",
    ] {
        assert!(oauth::parse_tokens(&resp(200, body)).is_err(), "{body}");
    }
    let (g, next) = library::parse_games(&resp(
        200,
        r#"{"titles":[{"name":5},{"name":"ok","media":{"images":"x"},"playDuration":7,"titleId":9},null,[],{"name":"  \u0007 "}],"nextOffset":-3}"#,
    ))
    .unwrap();
    assert_eq!(g.len(), 1);
    assert_eq!(next, None);
    let s = library::parse_summary(&resp(
        200,
        r#"{"trophyLevel":-1,"progress":9999999999999,"earnedTrophies":{"gold":99999999999999,"bronze":-1,"silver":1.5}}"#,
    ))
    .unwrap();
    assert_eq!((s.level, s.progress, s.earned.gold, s.earned.bronze, s.earned.silver), (0, 100, 100_000, 0, 0));
    assert!(s.earned.total() <= 400_000);
}

#[test]
fn a_huge_play_duration_must_not_panic() {
    assert!(std::panic::catch_unwind(|| library::parse_duration_minutes("PT18446744073709551615H")).is_ok());
    let body = r#"{"titles":[{"name":"G","playDuration":"PT18446744073709551615H"}]}"#;
    assert!(std::panic::catch_unwind(|| library::parse_games(&resp(200, body))).is_ok());
}

#[test]
fn duration_parser_edge_cases() {
    assert_eq!(library::parse_duration_minutes("PT3H12M5S"), Some(192));
    assert_eq!(library::parse_duration_minutes("PT0S"), Some(0));
    assert_eq!(library::parse_duration_minutes("PT"), Some(0));
    assert_eq!(library::parse_duration_minutes("PT5"), Some(0), "trailing digits without a unit are ignored");
    assert_eq!(library::parse_duration_minutes("P1DT2H"), None, "days are not supported");
    assert_eq!(library::parse_duration_minutes("PT-5M"), None);
    assert_eq!(library::parse_duration_minutes("PTH"), None);
    assert_eq!(library::parse_duration_minutes("PT99999999999999999999H"), None, "does not fit u64");
    assert_eq!(library::parse_duration_minutes("PT1000000000000M"), Some(10_000_000), "clamped");
}

#[test]
fn console_and_game_names_must_not_keep_bidi_overrides() {
    let c = api::parse_clients(&resp(
        200,
        r#"{"clients":[{"duid":"d1","platform":"PS5","device":{"name":"Living\u202eRoom\u200b"}}]}"#,
    ))
    .unwrap();
    assert!(!c[0].name.contains('\u{202e}') && !c[0].name.contains('\u{200b}'), "{:?}", c[0].name);
    let (g, _) = library::parse_games(&resp(200, r#"{"titles":[{"name":"Game\u202e"}]}"#)).unwrap();
    assert!(!g[0].name.contains('\u{202e}'));
}

#[test]
fn console_list_cleaning_and_duid_validation() {
    let long = "x".repeat(300);
    let body = format!(
        r#"{{"clients":[{{"duid":"{long}"}},{{"duid":"  "}},{{"duid":"d\u00e9"}},{{"duid":"ok1","device":{{"name":"{long}"}}}},{{"platform":"PS5"}},{{"duid":"ok2","device":{{"name":"a\nb\u0000c"}}}}]}}"#
    );
    let v = api::parse_clients(&resp(200, &body)).unwrap();
    assert_eq!(v.iter().map(|c| c.duid.as_str()).collect::<Vec<_>>(), ["ok1", "ok2"]);
    assert_eq!(v[0].name.chars().count(), 64);
    assert_eq!(v[1].name, "abc");
}

// ---------------------------------------------------------------------------------------------------------------------
// npsso handling
// ---------------------------------------------------------------------------------------------------------------------

#[test]
fn npsso_extraction_boundaries_and_injection() {
    let ok = |n: usize| "a".repeat(n);
    assert_eq!(api::extract_npsso(&ok(31)), None);
    assert_eq!(api::extract_npsso(&ok(32)).as_deref(), Some(ok(32).as_str()));
    assert_eq!(api::extract_npsso(&ok(128)).as_deref(), Some(ok(128).as_str()));
    assert_eq!(api::extract_npsso(&ok(129)), None);
    assert_eq!(api::extract_npsso(&ok(5000)), None);
    assert_eq!(api::extract_npsso(&format!("\"{}\"", ok(64))).as_deref(), Some(ok(64).as_str()));
    assert_eq!(
        api::extract_npsso(&format!(r#"{{"npsso":"{}","expires_in":5}}"#, ok(64))).as_deref(),
        Some(ok(64).as_str())
    );
    // anything that could break out of a Cookie header is refused
    for bad in ["a;b=c", "a\r\nX-Evil: 1", "a b", "a%0d%0ab", "a,b"] {
        let t = format!("{}{bad}{}", ok(40), ok(10));
        assert_eq!(api::extract_npsso(&t), None, "{bad:?}");
        let j = serde_json::json!({ "npsso": t }).to_string();
        assert_eq!(api::extract_npsso(&j), None, "{bad:?}");
    }
    assert_eq!(api::extract_npsso(r#"{"npsso":5}"#), None);
    assert_eq!(api::extract_npsso("{"), None);
    // a rejected paste never reaches the network
    let net = Script::default();
    let st = MemoryStore::default();
    let a = cfg_api();
    let api = Api { cfg: &a, http: &net, store: &st };
    assert!(api.sign_in("short", 0).is_err());
    assert!(net.seen.borrow().is_empty());
}

fn cfg_api() -> ApiConfig {
    api_cfg()
}

#[test]
fn the_npsso_is_only_sent_as_a_cookie_and_never_stored() {
    let npsso = "n".repeat(64);
    let net = Script::with(vec![
        HttpResponse {
            status: 302,
            headers: vec![("Location".into(), "com.example://r?code=CODE%3D1".into())],
            body: vec![],
        },
        resp(200, r#"{"access_token":"AT","refresh_token":"RT","expires_in":3600}"#),
    ]);
    let (a, st) = (api_cfg(), MemoryStore::default());
    let api = Api { cfg: &a, http: &net, store: &st };
    api.sign_in(&npsso, 100).unwrap();
    let seen = net.seen.borrow();
    assert_eq!(seen[0].header("Cookie"), Some(format!("npsso={npsso}").as_str()));
    assert!(!seen[0].url.contains(&npsso) && !String::from_utf8_lossy(&seen[1].body).contains(&npsso));
    assert!(seen[1].header("Cookie").is_none());
    assert!(String::from_utf8_lossy(&seen[1].body).contains("code=CODE%3D1"));
    let stored = st.get("psn.api_tokens").unwrap();
    assert!(!stored.contains(&npsso));
}

#[test]
fn authorize_answers_are_classified() {
    let loc = |s: u16, l: Option<&str>| HttpResponse {
        status: s,
        headers: l.map(|l| vec![("location".into(), l.into())]).unwrap_or_default(),
        body: vec![],
    };
    assert_eq!(api::parse_authorize(&loc(302, Some("x://r?code=abc&cid=1"))).unwrap(), "abc");
    assert_eq!(
        api::parse_authorize(&loc(302, Some("x://r?error=login_required"))),
        Err(PsnError::Rejected { status: 302 })
    );
    assert_eq!(api::parse_authorize(&loc(302, None)), Err(PsnError::Rejected { status: 302 }));
    assert_eq!(api::parse_authorize(&loc(503, None)), Err(PsnError::Unavailable { status: 503 }));
    assert_eq!(api::parse_authorize(&loc(429, None)), Err(PsnError::Unavailable { status: 429 }));
    assert_eq!(api::code_from_location("x://r?code="), None);
    assert_eq!(api::code_from_location("no-query"), None);
}

// ---------------------------------------------------------------------------------------------------------------------
// Art URL allow-list
// ---------------------------------------------------------------------------------------------------------------------

#[test]
fn art_url_allow_list_rejects_the_usual_bypasses() {
    for bad in [
        "http://image.api.playstation.com/a.png",
        "https://evil.test/a.png",
        "https://playstation.com.evil.test/a.png",
        "https://evilplaystation.com/a.png",
        "https://image.api.playstation.com@evil.test/a.png",
        "https://evil.test@image.api.playstation.com/a.png",
        "https://image.api.playstation.com:8443/a.png",
        "https://image.api.playstation.com.evil.test/a.png",
        "https://evil.test/?x=.playstation.com",
        "https://evil.test#.playstation.com",
        "https://evil.test?.playstation.com/",
        "https://image.api.playstation.com./a.png", // trailing dot: refused (conservative)
        "https://image.api.playstation.com%2f@evil.test/",
        "HTTPS://evil.test/a.png",
        "//image.api.playstation.com/a.png",
        "file:///etc/passwd",
        "https://",
        "https:///a.png",
        "https://127.0.0.1/a.png",
        "https://[::1]/a.png",
        "https://localhost/a.png",
        "javascript:alert(1)",
        "",
        " https://image.api.playstation.com/a.png",
        "https://playstation.com/a.png", // apex is not a sub-domain: refused
    ] {
        assert!(!library::art_url_allowed(bad), "must be refused: {bad:?}");
    }
    for good in [
        "https://image.api.playstation.com/cdn/x.png",
        "https://IMAGE.API.PLAYSTATION.COM/a.png",
        "https://x.playstation.net/a",
        "https://a.b.sonyentertainmentnetwork.com/a.jpg?x=1",
    ] {
        assert!(library::art_url_allowed(good), "{good:?}");
    }
    assert!(!library::art_url_allowed(&format!("https://image.api.playstation.com/{}", "a".repeat(2100))));
}

#[test]
fn art_url_with_a_backslash_must_not_pass_the_allow_list() {
    for bad in ["https://evil.test\\.playstation.com/x.png", "https://evil.test\t\\.playstation.com/x.png"] {
        assert!(!library::art_url_allowed(bad), "{bad:?}");
    }
}

#[test]
fn art_url_parser_differential_cases_are_rejected() {
    for bad in [
        "https://evil.test%5c.playstation.com/x.png",
        "https://evil.test%2f.playstation.com/x.png",
        "https://image.api.playstation.com@evil.test/x.png",
        "https://evil.test@image.api.playstation.com/x.png",
        "https://image.api.playstation.com:8443/x.png",
        "https://image.api.playstation.com:/x.png",
        "https://.playstation.com/x.png",
        "https://a..playstation.com/x.png",
        "https://image.api.playstation.com./x.png",
        "https://image.api.playstation.com\\@evil.test/x.png",
        "https://im\u{e4}ge.api.playstation.com/x.png",
        "https://image.api.playstation.com\n.evil.test/x.png",
        "https://[::1]/x.png",
        "https://127.0.0.1/x.png",
        "https:///image.api.playstation.com/x.png",
        "https://image.api.playstation.com.evil.test/x.png",
    ] {
        assert!(!library::art_url_allowed(bad), "{bad:?}");
    }
    assert!(library::art_url_allowed("https://image.api.playstation.com/a/b.png?w=1#f"));
    assert!(library::art_url_allowed("https://IMAGE.API.PLAYSTATION.COM/a.png"));
}

#[test]
fn two_threads_updating_one_file_store_path_lose_nothing() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("secrets.json");
    let hs: Vec<_> = (0..4)
        .map(|t| {
            let p = p.clone();
            std::thread::spawn(move || {
                let s = FileStore::open(&p).unwrap(); // separate instances, like app + import tool
                (0..15).for_each(|i| s.set(&format!("k{t}-{i}"), "v").unwrap());
            })
        })
        .collect();
    hs.into_iter().for_each(|h| h.join().unwrap());
    assert_eq!(FileStore::open(&p).unwrap().get("k3-14").as_deref(), Some("v"));
    let m: std::collections::HashMap<String, String> = serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
    assert_eq!(m.len(), 60);
}

#[test]
fn a_file_store_sees_another_instances_changes() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("secrets.json");
    let (a, b) = (FileStore::open(&p).unwrap(), FileStore::open(&p).unwrap());
    a.set("k", "1").unwrap();
    assert_eq!(b.get("k").as_deref(), Some("1"));
    b.delete("k").unwrap();
    assert_eq!(a.get("k"), None);
}

#[test]
fn overflowing_expiry_saturates() {
    let body = r#"{"access_token":"A","refresh_token":"R","expires_in":18446744073709551615}"#;
    let t = api::parse_tokens(&resp(200, body), u64::MAX - 5).unwrap();
    assert_eq!(t.expires_at, Some(u64::MAX));
    let net = Script::with(vec![resp(200, body), resp(200, ACCOUNT)]);
    let c = PsnClient::new(cfg(), &net, MemoryStore::default());
    c.login("x", u64::MAX - 5).unwrap();
    assert_eq!(c.access_token(u64::MAX - 1000).unwrap().as_deref(), Some("A"));
}

#[test]
fn a_game_with_a_huge_play_duration_is_clamped() {
    assert_eq!(library::parse_duration_minutes("PT18446744073709551615H"), Some(10_000_000));
}

#[test]
fn degenerate_config_urls_are_rejected() {
    let mut c = cfg();
    c.token_url = "https://".into();
    assert!(c.validate().is_err());
}

fn art(status: u16, ctype: &str, body: Vec<u8>) -> HttpResponse {
    HttpResponse { status, headers: vec![("Content-Type".into(), ctype.into())], body }
}

#[test]
fn fetch_art_limits_type_size_and_status() {
    let (a, st) = (api_cfg(), MemoryStore::default());
    let url = "https://image.api.playstation.com/a.png";
    let run = |r: HttpResponse| {
        let net = Script::with(vec![r]);
        let api = Api { cfg: &a, http: &net, store: &st };
        let out = api.fetch_art(url);
        let seen = net.seen.borrow().clone();
        (out, seen)
    };
    let (ok, seen) = run(art(200, "image/png", vec![1; 10]));
    assert_eq!(ok.unwrap().1, "image/png");
    assert_eq!(seen[0].url, "https://image.api.playstation.com/a.png?w=400");
    assert!(seen[0].header("Authorization").is_none(), "the bearer token is never sent to the image host");
    assert!(run(art(200, "image/svg+xml", vec![1; 10])).0.is_err());
    assert!(run(art(200, "text/html", b"<script>".to_vec())).0.is_err());
    assert!(run(art(200, "image/png", vec![])).0.is_err());
    assert!(run(art(200, "image/png", vec![0; library::MAX_ART_BYTES + 1])).0.is_err());
    assert!(run(art(200, "image/png", vec![0; library::MAX_ART_BYTES])).0.is_ok());
    assert!(run(art(302, "image/png", vec![1])).0.is_err(), "redirects are not followed");
    assert!(run(art(404, "image/png", vec![1])).0.is_err());
    let net = Script::default();
    let api = Api { cfg: &a, http: &net, store: &st };
    assert!(api.fetch_art("https://evil.test/a.png").is_err());
    assert!(net.seen.borrow().is_empty(), "a refused host is never contacted");
}

#[test]
fn characterisation_art_bytes_are_trusted_by_content_type_only() {
    let (a, st) = (api_cfg(), MemoryStore::default());
    let net = Script::with(vec![art(200, "image/png", b"<html>not a png</html>".to_vec())]);
    let api = Api { cfg: &a, http: &net, store: &st };
    assert!(
        api.fetch_art("https://image.api.playstation.com/a.png").is_ok(),
        "no magic-byte check: the decoder must be robust"
    );
}

// ---------------------------------------------------------------------------------------------------------------------
// Pagination loops
// ---------------------------------------------------------------------------------------------------------------------

/// Always answers with the same 10 distinct-looking clients incremented per request: a server that never ends.
struct Endless(RefCell<u32>);
impl HttpClient for Endless {
    fn execute(&self, req: &HttpRequest) -> std::io::Result<HttpResponse> {
        *self.0.borrow_mut() += 1;
        if req.url.contains("/clients") {
            let n = *self.0.borrow();
            let items: Vec<String> = (0..10).map(|i| format!(r#"{{"duid":"d{n}-{i}","platform":"PS5"}}"#)).collect();
            return Ok(resp(200, &format!(r#"{{"clients":[{}]}}"#, items.join(","))));
        }
        Ok(resp(200, "{}"))
    }
}

fn signed_in_store() -> MemoryStore {
    let st = MemoryStore::default();
    st.set("psn.api_tokens", r#"{"access_token":"AT","refresh_token":"RT","expires_at":null}"#).unwrap();
    st
}

#[test]
fn a_never_ending_console_list_stops_after_the_page_cap() {
    let (a, st, net) = (api_cfg(), signed_in_store(), Endless(RefCell::new(0)));
    let api = Api { cfg: &a, http: &net, store: &st };
    let v = api.consoles(0).unwrap();
    assert_eq!(*net.0.borrow(), 10, "MAX_PAGES requests, not more");
    assert_eq!(v.len(), 100);
}

/// Library server whose `nextOffset` misbehaves.
struct Loopy {
    next: Box<dyn Fn(u32) -> serde_json::Value>,
    hits: RefCell<u32>,
}
impl HttpClient for Loopy {
    fn execute(&self, req: &HttpRequest) -> std::io::Result<HttpResponse> {
        if req.url.contains("trophyTitles") || req.url.contains("gamelist") {
            let n = {
                *self.hits.borrow_mut() += 1;
                *self.hits.borrow()
            };
            let key = if req.url.contains("trophyTitles") { "trophyTitles" } else { "titles" };
            let name_key = if key == "titles" { "name" } else { "trophyTitleName" };
            let mut j = serde_json::json!({ key: [{ name_key: format!("Game {n}") }] });
            if let serde_json::Value::Object(m) = (self.next)(n) {
                j.as_object_mut().unwrap().extend(m);
            }
            return Ok(resp(200, &j.to_string()));
        }
        Ok(resp(200, "{}"))
    }
}

#[test]
fn library_pagination_with_bad_next_offsets_terminates() {
    #[allow(clippy::type_complexity)]
    let cases: Vec<(&str, Box<dyn Fn(u32) -> serde_json::Value>, u32)> = vec![
        ("same offset", Box::new(|_| serde_json::json!({"nextOffset": 0})), 2),
        ("always growing", Box::new(|n| serde_json::json!({"nextOffset": n * 100})), 16),
        ("decreasing", Box::new(|n| serde_json::json!({"nextOffset": 1000 - n})), 4),
        ("huge", Box::new(|_| serde_json::json!({"nextOffset": u64::MAX})), 4),
        ("negative", Box::new(|_| serde_json::json!({"nextOffset": -1})), 2),
        ("absent", Box::new(|_| serde_json::json!({})), 2),
    ];
    for (name, next, _) in cases {
        let (a, st) = (api_cfg(), signed_in_store());
        let net = Loopy { next, hits: RefCell::new(0) };
        let api = Api { cfg: &a, http: &net, store: &st };
        let lib = api.library(0).unwrap();
        assert!(*net.hits.borrow() <= 16, "{name}: {} requests", net.hits.borrow());
        assert!(lib.games.len() <= 16, "{name}");
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// merge()
// ---------------------------------------------------------------------------------------------------------------------

fn game(name: &str, platform: &str, played: Option<&str>, progress: Option<u8>) -> library::Game {
    library::Game {
        title_id: None,
        name: name.into(),
        platform: platform.into(),
        image_url: None,
        last_played: played.map(String::from),
        play_minutes: None,
        trophies: progress.map(|p| library::GameTrophies {
            progress: p,
            earned: Default::default(),
            total: Default::default(),
        }),
    }
}

#[test]
fn merge_orders_played_first_newest_first_then_leftover_trophy_titles() {
    let played = vec![
        game("Old", "PS5", Some("2020-01-01T00:00:00Z"), None),
        game("New", "PS5", Some("2025-01-01T00:00:00Z"), None),
        game("Never", "PS5", None, None),
    ];
    let trophies = vec![
        game("NEW", "PS5", Some("x"), Some(50)),
        game("Other", "PS4", Some("2019"), Some(10)),
        game("Other2", "PS4", Some("2022"), Some(10)),
    ];
    let m = library::merge(played, trophies);
    let names: Vec<_> = m.iter().map(|g| g.name.as_str()).collect();
    assert_eq!(names, ["New", "Old", "Never", "Other2", "Other"]);
    assert_eq!(m[0].trophies.as_ref().unwrap().progress, 50, "joined by normalised name");
    assert_eq!(library::merge(vec![], vec![]).len(), 0);
}

#[test]
fn merge_must_not_attach_ps4_trophies_to_the_ps5_version() {
    let played = vec![game("Game", "PS5", Some("2025"), None)];
    let trophies = vec![game("Game", "PS4", Some("2019"), Some(10)), game("Game", "PS5", Some("2025"), Some(90))];
    let m = library::merge(played, trophies);
    let ps5 = m.iter().find(|g| g.platform == "PS5").unwrap();
    assert_eq!(ps5.trophies.as_ref().map(|t| t.progress), Some(90));
}

#[test]
fn merge_must_not_drop_trophies_when_a_title_is_listed_twice() {
    let played = vec![game("Game", "PS5", Some("2020"), None), game("Game", "PS5", Some("2025"), None)];
    let trophies = vec![game("Game", "PS5", Some("2025"), Some(77))];
    let m = library::merge(played, trophies);
    assert_eq!(m.len(), 1);
    assert_eq!(m[0].trophies.as_ref().map(|t| t.progress), Some(77));
}

#[test]
fn characterisation_names_without_letters_or_digits_all_collapse_into_one_tile() {
    let played = vec![game("™", "PS5", Some("2025"), None), game("®", "PS5", Some("2024"), None)];
    assert_eq!(library::merge(played, vec![]).len(), 1);
}

// ---------------------------------------------------------------------------------------------------------------------
// Installed titles / launch
// ---------------------------------------------------------------------------------------------------------------------

#[test]
fn installed_titles_are_sanitised_and_deduplicated() {
    let body =
        serde_json::json!({"clients":[{"duid":"d1","device":{"name":"PS5"},"systemData":{"installedTitles":{"titles":[
            {"npTitleId":"ppsa01234_00","comment":"A","lastAccessDateTime":"2025","size":18446744073709551615u64},
            {"npTitleId":"PPSA01234","comment":"A dup","lastAccessDateTime":"2024"},
            {"npTitleId":"bad id!","comment":"x"},
            {"npTitleId":"../../etc","comment":"x"},
            {"npTitleId":"PPSA99999999999999999999999999"},
            {"titleId":"CUSA00001","displayLocationSpace":"media"},
            {"npTitleId":5}
        ]}}}]})
        .to_string();
    let v = library::parse_installed(&resp(200, &body)).unwrap();
    let ids: Vec<_> = v[0].installed.iter().map(|t| t.title_id.as_str()).collect();
    assert_eq!(ids, ["PPSA01234", "CUSA00001"]);
    assert_eq!(v[0].installed[0].size_bytes, u64::MAX);
    assert_eq!(v[0].installed[1].kind, "media");
}

#[test]
fn launch_request_is_json_escaped_and_normalised() {
    let r = library::launch_request("T", "PROSPERO", "du\"id", "ppsa01234_00");
    let j: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
    assert_eq!(j["commandDetail"]["duid"], "du\"id");
    assert_eq!(j["commandDetail"]["parameters"]["titleIds"][0], "PPSA01234");
    let s = library::status_request("T", "PROSPERO", "a&b=c d");
    assert!(s.url.contains("commandIds=a%26b%3Dc%20d"));
    let (a, st) = (api_cfg(), signed_in_store());
    let net = Script::default();
    let api = Api { cfg: &a, http: &net, store: &st };
    assert!(api.launch_title(0, "", "d", "t").is_err());
    assert!(api.launch_title(0, "PROSPERO", "d", "  ").is_err());
    assert!(net.seen.borrow().is_empty());
}

#[test]
fn status_request_must_encode_the_platform() {
    let s = library::status_request("T", "PRO&x=1", "c");
    assert!(!s.url.contains("&x=1"));
}

// ---------------------------------------------------------------------------------------------------------------------
// 401/403 handling
// ---------------------------------------------------------------------------------------------------------------------

#[test]
fn a_rejected_access_token_is_refreshed_once_then_the_console_list_signs_out() {
    let net = Script::with(vec![
        resp(401, ""),
        resp(200, r#"{"access_token":"AT2","refresh_token":"RT2","expires_in":3600}"#),
        resp(401, ""),
    ]);
    let (a, st) = (api_cfg(), signed_in_store());
    let api = Api { cfg: &a, http: &net, store: &st };
    assert_eq!(api.consoles(0), Err(api::ApiError::NotSignedIn));
    assert!(!api.is_signed_in());
    assert_eq!(net.seen.borrow().len(), 3);
}

#[test]
fn a_401_with_no_refresh_token_signs_out_even_for_the_library_endpoints() {
    // library.rs says a login that "may not be allowed to read this endpoint keeps the sign-in", but with no refresh
    // token the first 401 already wipes it (api.rs:299-301)
    let st = MemoryStore::default();
    st.set("psn.api_tokens", r#"{"access_token":"AT","refresh_token":null,"expires_at":null}"#).unwrap();
    let net = Script::with(vec![resp(401, "")]);
    let a = api_cfg();
    let api = Api { cfg: &a, http: &net, store: &st };
    assert!(api.launch_title(0, "PROSPERO", "d", "t").is_err());
    assert!(!api.is_signed_in());
}

// ---------------------------------------------------------------------------------------------------------------------
// Secret stores
// ---------------------------------------------------------------------------------------------------------------------

#[test]
fn file_store_rejects_wrongly_typed_and_truncated_files_and_leaves_them_alone() {
    let d = tempfile::tempdir().unwrap();
    for (i, bad) in [&b"{\"a\":1}"[..], b"[]", b"null", b"{\"a\":\"b\"", b"\xff\xfe", b""].iter().enumerate() {
        let p = d.path().join(format!("s{i}.json"));
        std::fs::write(&p, bad).unwrap();
        assert!(FileStore::open(&p).is_err(), "{bad:?}");
        assert_eq!(std::fs::read(&p).unwrap(), *bad);
    }
    assert!(FileStore::open(d.path()).is_err(), "a directory is not a store");
}

#[test]
fn file_store_survives_a_leftover_partial_temp_file_and_unicode_keys() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("secrets.json");
    std::fs::write(p.with_extension("tmp"), b"{\"half").unwrap();
    let s = FileStore::open(&p).unwrap();
    s.set("k\u{0}\u{1F3AE}", &"v".repeat(1_000_000)).unwrap();
    assert_eq!(FileStore::open(&p).unwrap().get("k\u{0}\u{1F3AE}").unwrap().len(), 1_000_000);
}

#[test]
fn file_store_a_poisoned_lock_is_recovered() {
    let d = tempfile::tempdir().unwrap();
    let s = std::sync::Arc::new(FileStore::open(d.path().join("s.json")).unwrap());
    let s2 = s.clone();
    let _ = std::thread::spawn(move || {
        let _ = s2.set("a", "1");
        panic!("boom");
    })
    .join();
    s.set("b", "2").unwrap();
    assert_eq!(s.get("a").as_deref(), Some("1"));
}

#[test]
fn two_file_store_instances_must_not_lose_each_others_entries() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("secrets.json");
    let app = FileStore::open(&p).unwrap();
    let tool = FileStore::open(&p).unwrap();
    tool.set("console/AA", "imported").unwrap();
    app.set("psn.tokens", "tokens").unwrap(); // the app's snapshot did not know about console/AA
    let again = FileStore::open(&p).unwrap();
    assert_eq!(again.get("console/AA").as_deref(), Some("imported"), "the imported keys were erased");
}

#[test]
fn a_stale_loose_temp_file_must_not_make_the_secrets_file_world_readable() {
    use std::os::unix::fs::PermissionsExt;
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("secrets.json");
    let tmp = p.with_extension("tmp");
    std::fs::write(&tmp, b"x").unwrap();
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o644)).unwrap();
    let s = FileStore::open(&p).unwrap();
    s.set("k", "secret").unwrap();
    assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
}

#[test]
fn a_symlinked_temp_file_must_not_receive_the_secrets() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("secrets.json");
    let target = d.path().join("elsewhere.txt");
    std::fs::write(&target, b"original").unwrap();
    std::os::unix::fs::symlink(&target, p.with_extension("tmp")).unwrap();
    let s = FileStore::open(&p).unwrap();
    s.set("k", "SECRET-VALUE").unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), b"original");
}

// ---------------------------------------------------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------------------------------------------------

#[test]
fn config_validation_boundaries() {
    let good = cfg().to_json();
    assert!(PsnConfig::from_json(&good).is_ok());
    for (from, to) in [
        ("https://auth.example.test/2.0/oauth/token", "http://auth.example.test/2.0/oauth/token"),
        ("https://auth.example.test/2.0/oauth/token", "https://auth.example.test/2.0/oauth/token/"),
        ("https://auth.example.test/2.0/oauth/token", "  "),
        ("\"client_secret\": \"s\"", "\"client_secret\": \"\""),
    ] {
        assert!(PsnConfig::from_json(&good.replace(from, to)).is_err(), "{to}");
    }
    assert!(PsnConfig::from_json("{}").is_err());
    assert!(PsnConfig::from_json(&"[".repeat(100_000)).is_err());
    let dbg = format!("{:?}", {
        let mut c = cfg();
        c.api = Some(api_cfg());
        c
    });
    assert!(!dbg.contains("client_secret: \"s\"") && !dbg.contains("eDp5"), "{dbg}");
}

#[test]
fn config_validation_must_reject_degenerate_urls() {
    let mut c = cfg();
    c.token_url = "https://".into();
    assert!(c.validate().is_err());
    let mut c = cfg();
    let mut a = api_cfg();
    a.auth_base = "https://auth.example.test/oauth/".into();
    c.api = Some(a);
    assert!(c.validate().is_err());
}

#[test]
fn redirect_matching_rejects_look_alikes() {
    let c = cfg();
    for u in [
        "https://example.test/redirect.evil/?code=1",
        "https://example.test/redirectx?code=1",
        "http://example.test/redirect?code=1",
        "HTTPS://EXAMPLE.TEST/redirect?code=1",
        "https://example.test:443/redirect?code=1",
        "https://example.test/./redirect?code=1",
        "https://example.test/redirect%3Fcode=1",
    ] {
        assert!(!oauth::is_redirect(&c, u), "{u}");
    }
    assert_eq!(
        oauth::parse_redirect(&c, "https://example.test/redirect?error=access_denied&code=1"),
        oauth::RedirectOutcome::Denied("access_denied".into())
    );
    // an error that comes after the code still wins
    assert_eq!(
        oauth::parse_redirect(&c, "https://example.test/redirect?code=1&error=x"),
        oauth::RedirectOutcome::Denied("x".into())
    );
}
