//! The only way this crate touches the network. Production code plugs in a real client (feature `live`); tests
//! plug in a scripted fake, so no test or default build can reach Sony's servers.

use std::io;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpRequest {
    pub method: &'static str,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl HttpRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl HttpResponse {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

pub trait HttpClient {
    /// Perform one request. HTTP error statuses are `Ok`; only transport failures are `Err`.
    fn execute(&self, req: &HttpRequest) -> io::Result<HttpResponse>;
}

#[cfg(feature = "live")]
pub mod live {
    use super::*;
    use std::time::Duration;

    /// Real HTTPS client. Only compiled with `--features live`; never used by tests.
    pub struct UreqClient {
        agent: ureq::Agent,
    }

    impl UreqClient {
        pub fn new(timeout: Duration) -> Self {
            Self { agent: ureq::AgentBuilder::new().timeout(timeout).redirects(0).build() }
        }
    }

    impl HttpClient for UreqClient {
        fn execute(&self, req: &HttpRequest) -> io::Result<HttpResponse> {
            let mut r = self.agent.request(req.method, &req.url);
            for (k, v) in &req.headers {
                r = r.set(k, v);
            }
            let result = if req.body.is_empty() { r.call() } else { r.send_bytes(&req.body) };
            let resp = match result {
                Ok(resp) => resp,
                Err(ureq::Error::Status(_, resp)) => resp, // error statuses are data, not failures
                Err(ureq::Error::Transport(t)) => return Err(io::Error::other(t.to_string())),
            };
            let status = resp.status();
            let headers = resp
                .headers_names()
                .into_iter()
                .filter_map(|n| resp.header(&n).map(|v| (n.clone(), v.to_string())))
                .collect();
            let mut body = vec![];
            io::Read::read_to_end(&mut resp.into_reader().take(1 << 20), &mut body)?;
            Ok(HttpResponse { status, headers, body })
        }
    }

    use std::io::Read as _;
}
