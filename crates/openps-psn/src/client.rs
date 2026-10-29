//! High-level PSN session: turn an authorization code into tokens and an account, persist the tokens in a
//! secret store, and keep them fresh.

use crate::config::PsnConfig;
use crate::http::{HttpClient, HttpResponse};
use crate::oauth::{self, Account, PsnError, Tokens};
use crate::store::SecretStore;
use serde::{Deserialize, Serialize};

const TOKENS_KEY: &str = "psn.tokens";
/// Refresh this many seconds before the access token expires.
pub const REFRESH_MARGIN_S: u64 = 120;

#[derive(Serialize, Deserialize)]
struct Stored {
    access_token: String,
    refresh_token: Option<String>,
    /// Unix seconds when the access token stops being valid, if the server said.
    expires_at: Option<u64>,
}

pub struct PsnClient<H: HttpClient, S: SecretStore> {
    cfg: PsnConfig,
    http: H,
    store: S,
}

fn network(e: std::io::Error) -> PsnError {
    PsnError::Network(e.to_string())
}

impl<H: HttpClient, S: SecretStore> PsnClient<H, S> {
    pub fn new(cfg: PsnConfig, http: H, store: S) -> Self {
        Self { cfg, http, store }
    }

    pub fn config(&self) -> &PsnConfig {
        &self.cfg
    }

    fn call(&self, req: &crate::http::HttpRequest) -> Result<HttpResponse, PsnError> {
        self.http.execute(req).map_err(network)
    }

    fn persist(&self, t: &Tokens, now: u64) -> Result<(), PsnError> {
        let s = Stored {
            access_token: t.access_token.clone(),
            refresh_token: t.refresh_token.clone(),
            expires_at: t.expires_in.map(|e| now + e),
        };
        self.store.set(TOKENS_KEY, &serde_json::to_string(&s).expect("serialisable")).map_err(PsnError::Network)
    }

    /// Complete a login: exchange the code, look up the account, persist the tokens. `now` is unix seconds.
    pub fn login(&self, code: &str, now: u64) -> Result<Account, PsnError> {
        let tokens = oauth::parse_tokens(&self.call(&oauth::exchange_code_request(&self.cfg, code))?)?;
        let account = oauth::parse_account(&self.call(&oauth::account_request(&self.cfg, &tokens.access_token))?)?;
        self.persist(&tokens, now)?;
        Ok(account)
    }

    /// A valid access token, refreshing (and persisting) first if it is about to expire. `None` if not logged in.
    pub fn access_token(&self, now: u64) -> Result<Option<String>, PsnError> {
        let Some(stored) = self.store.get(TOKENS_KEY).and_then(|j| serde_json::from_str::<Stored>(&j).ok()) else {
            return Ok(None);
        };
        let expiring = stored.expires_at.is_some_and(|e| now + REFRESH_MARGIN_S >= e);
        if !expiring {
            return Ok(Some(stored.access_token));
        }
        let Some(rt) = stored.refresh_token else { return Ok(None) }; // expired and not refreshable: log in again
        match oauth::parse_tokens(&self.call(&oauth::refresh_request(&self.cfg, &rt))?) {
            Ok(mut t) => {
                if t.refresh_token.is_none() {
                    t.refresh_token = Some(rt); // some servers do not rotate the refresh token
                }
                self.persist(&t, now)?;
                Ok(Some(t.access_token))
            }
            Err(PsnError::Rejected { .. }) => {
                self.logout()?; // the refresh token is dead: forget it so the UI asks for a new login
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    pub fn is_logged_in(&self) -> bool {
        self.store.get(TOKENS_KEY).is_some()
    }

    pub fn logout(&self) -> Result<(), PsnError> {
        self.store.delete(TOKENS_KEY).map_err(PsnError::Network)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::test_config;
    use crate::http::{HttpRequest, HttpResponse};
    use crate::store::MemoryStore;
    use std::cell::RefCell;
    use std::collections::VecDeque;

    /// Scripted HTTP: returns queued responses and records every request. Never touches a network.
    struct Script {
        replies: RefCell<VecDeque<std::io::Result<HttpResponse>>>,
        seen: RefCell<Vec<HttpRequest>>,
    }

    impl Script {
        fn new(replies: Vec<std::io::Result<HttpResponse>>) -> Self {
            Self { replies: RefCell::new(replies.into()), seen: RefCell::new(vec![]) }
        }
    }

    impl HttpClient for &Script {
        fn execute(&self, req: &HttpRequest) -> std::io::Result<HttpResponse> {
            self.seen.borrow_mut().push(req.clone());
            self.replies.borrow_mut().pop_front().expect("unexpected extra request")
        }
    }

    fn ok(body: &str) -> std::io::Result<HttpResponse> {
        Ok(HttpResponse { status: 200, headers: vec![], body: body.as_bytes().to_vec() })
    }
    fn status(s: u16) -> std::io::Result<HttpResponse> {
        Ok(HttpResponse { status: s, headers: vec![], body: vec![] })
    }

    const TOKENS: &str = r#"{"access_token":"AT1","refresh_token":"RT1","expires_in":3600}"#;
    const ACCOUNT: &str = r#"{"online_id":"Player_1","user_id":"1234567890123456789"}"#;

    #[test]
    fn login_exchanges_the_code_looks_up_the_account_and_persists_tokens() {
        let net = Script::new(vec![ok(TOKENS), ok(ACCOUNT)]);
        let c = PsnClient::new(test_config(), &net, MemoryStore::default());
        assert!(!c.is_logged_in());
        let a = c.login("the-code", 1000).unwrap();
        assert_eq!((a.online_id.as_str(), a.account_id), ("Player_1", 1_234_567_890_123_456_789));
        let seen = net.seen.borrow();
        assert_eq!(seen.len(), 2);
        assert!(String::from_utf8_lossy(&seen[0].body).contains("code=the-code"));
        assert_eq!(seen[1].url, "https://auth.example.test/2.0/oauth/token/AT1");
        assert!(c.is_logged_in());
        drop(seen);
        // within the validity window the stored token is returned without any network call
        assert_eq!(c.access_token(1000 + 3000).unwrap().as_deref(), Some("AT1"));
        assert_eq!(net.seen.borrow().len(), 2);
    }

    #[test]
    fn a_failed_account_lookup_persists_nothing() {
        let net = Script::new(vec![ok(TOKENS), status(500)]);
        let c = PsnClient::new(test_config(), &net, MemoryStore::default());
        assert_eq!(c.login("x", 0), Err(PsnError::Unavailable { status: 500 }));
        assert!(!c.is_logged_in());
        let net = Script::new(vec![status(400)]);
        let c = PsnClient::new(test_config(), &net, MemoryStore::default());
        assert_eq!(c.login("bad", 0), Err(PsnError::Rejected { status: 400 }));
        let net = Script::new(vec![Err(std::io::Error::other("offline"))]);
        let c = PsnClient::new(test_config(), &net, MemoryStore::default());
        assert!(matches!(c.login("x", 0), Err(PsnError::Network(m)) if m.contains("offline")));
    }

    #[test]
    fn tokens_are_refreshed_before_they_expire_and_the_refresh_token_survives() {
        // the first refresh response carries no new refresh token; the second does
        let net = Script::new(vec![
            ok(TOKENS),
            ok(ACCOUNT),
            ok(r#"{"access_token":"AT2","expires_in":3600}"#),
            ok(r#"{"access_token":"AT3","refresh_token":"RT3","expires_in":3600}"#),
        ]);
        let c = PsnClient::new(test_config(), &net, MemoryStore::default());
        c.login("x", 0).unwrap();
        // 3600 s validity, 120 s margin: at t=3500 the token is inside the margin
        assert_eq!(c.access_token(3500).unwrap().as_deref(), Some("AT2"));
        assert!(String::from_utf8_lossy(&net.seen.borrow()[2].body)
            .starts_with("grant_type=refresh_token&refresh_token=RT1"));
        // the server did not rotate the refresh token, so RT1 must still be in the store for the next refresh
        assert_eq!(c.access_token(3500 + 3500).unwrap().as_deref(), Some("AT3"));
        assert!(String::from_utf8_lossy(&net.seen.borrow()[3].body)
            .starts_with("grant_type=refresh_token&refresh_token=RT1"));
        // and now the rotated token is the one kept
        assert_eq!(c.access_token(7000 + 100).unwrap().as_deref(), Some("AT3"));
        assert_eq!(net.seen.borrow().len(), 4);
    }

    #[test]
    fn a_dead_refresh_token_logs_the_user_out() {
        let net = Script::new(vec![ok(TOKENS), ok(ACCOUNT), status(400)]);
        let c = PsnClient::new(test_config(), &net, MemoryStore::default());
        c.login("x", 0).unwrap();
        assert_eq!(c.access_token(4000).unwrap(), None); // refused: forgotten
        assert!(!c.is_logged_in());
        assert_eq!(c.access_token(4001).unwrap(), None); // and no further network calls
        assert_eq!(net.seen.borrow().len(), 3);
    }

    #[test]
    fn transient_refresh_errors_keep_the_login() {
        let net = Script::new(vec![ok(TOKENS), ok(ACCOUNT), status(503)]);
        let c = PsnClient::new(test_config(), &net, MemoryStore::default());
        c.login("x", 0).unwrap();
        assert_eq!(c.access_token(4000), Err(PsnError::Unavailable { status: 503 }));
        assert!(c.is_logged_in()); // a server hiccup must not discard credentials
    }

    #[test]
    fn expired_without_a_refresh_token_means_login_again() {
        let net = Script::new(vec![ok(r#"{"access_token":"A","expires_in":100}"#), ok(ACCOUNT)]);
        let c = PsnClient::new(test_config(), &net, MemoryStore::default());
        c.login("x", 0).unwrap();
        assert_eq!(c.access_token(500).unwrap(), None);
        assert_eq!(net.seen.borrow().len(), 2); // no refresh attempted
    }

    #[test]
    fn logout_forgets_everything() {
        let net = Script::new(vec![ok(TOKENS), ok(ACCOUNT)]);
        let c = PsnClient::new(test_config(), &net, MemoryStore::default());
        c.login("x", 0).unwrap();
        c.logout().unwrap();
        assert_eq!(c.access_token(0).unwrap(), None);
    }

    #[test]
    fn no_expiry_information_means_the_token_is_used_as_is() {
        let net = Script::new(vec![ok(r#"{"access_token":"A"}"#), ok(ACCOUNT)]);
        let c = PsnClient::new(test_config(), &net, MemoryStore::default());
        c.login("x", 0).unwrap();
        assert_eq!(c.access_token(u64::MAX / 2).unwrap().as_deref(), Some("A"));
    }
}
