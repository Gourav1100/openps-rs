//! Only with `--features live`. Talks to 127.0.0.1 port 1 (connection refused) and nothing else: never Sony.
#![cfg(feature = "live")]
use openps_psn::http::live::UreqClient;
use openps_psn::http::{HttpClient, HttpRequest};
use std::time::Duration;

#[test]
fn a_transport_error_must_not_contain_the_request_url_path() {
    let c = UreqClient::new(Duration::from_secs(2));
    // same shape as oauth::account_request: `<token_url>/<access_token>`
    let req = HttpRequest {
        method: "GET",
        url: "http://127.0.0.1:1/2.0/oauth/token/SECRET-ACCESS-TOKEN".into(),
        headers: vec![],
        body: vec![],
    };
    let err = c.execute(&req).unwrap_err().to_string();
    assert!(!err.contains("SECRET-ACCESS-TOKEN"), "error text leaks the token: {err}");
}

#[test]
fn a_refused_connection_is_an_error_not_a_hang() {
    let c = UreqClient::new(Duration::from_secs(2));
    let req = HttpRequest { method: "GET", url: "http://127.0.0.1:1/".into(), headers: vec![], body: vec![] };
    let t = std::time::Instant::now();
    assert!(c.execute(&req).is_err());
    assert!(t.elapsed() < Duration::from_secs(5));
}
