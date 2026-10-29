//! The real push WebSocket (feature `live`): TLS with the WebPKI root set, a read timeout per call, and the 5-second ping the original
//! sends. Never compiled into default builds and never used by tests against Sony.
//!
//! The HTTP upgrade is done here, not by the WebSocket library: the original's HTTP stack sends `Sec-WebSocket-Protocol: np-pushpacket`
//! and does not care whether the server echoes it, whereas tungstenite fails the connection if it does not. Only the framing is
//! delegated to tungstenite (`from_raw_socket`).

use crate::session::{PushConnector, PushSocket};
use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tungstenite::handshake::{client::generate_key, derive_accept_key};
use tungstenite::protocol::Role;
use tungstenite::{Message, WebSocket};

pub struct LiveConnector;

fn other(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(e.to_string())
}

fn tls_config() -> Arc<rustls::ClientConfig> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    // Sony's push server omits the intermediate certificate: see `tls` (the same fix the original app uses)
    let verifier = crate::tls::AiaVerifier::new(Arc::new(roots), provider.clone(), Arc::new(crate::tls::http_fetch))
        .expect("the root store is not empty");
    Arc::new(
        rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("the ring provider supports the default protocol versions")
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(verifier))
            .with_no_client_auth(),
    )
}

/// A TCP stream with an optional total deadline (the handshake): before every read or write the socket timeout is set to what is left,
/// so a server that drips bytes cannot hold the connection beyond the deadline.
struct Tcp {
    s: TcpStream,
    deadline: Option<Instant>,
}

impl Tcp {
    fn arm(&self) -> io::Result<()> {
        if let Some(d) = self.deadline {
            let left = d.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "the push handshake took too long"));
            }
            self.s.set_read_timeout(Some(left))?;
            self.s.set_write_timeout(Some(left))?;
        }
        Ok(())
    }
}
impl Read for Tcp {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        self.arm()?;
        self.s.read(b)
    }
}
impl Write for Tcp {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.arm()?;
        self.s.write(b)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.s.flush()
    }
}

/// Handshake budget (connect excluded: it has its own timeout) and the largest push message / frame accepted (PSN sends a few KiB).
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_PUSH_MESSAGE: usize = 256 * 1024;

enum Conn {
    Plain(Tcp),
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, Tcp>>),
}

impl Conn {
    fn tcp(&mut self) -> &mut Tcp {
        match self {
            Conn::Plain(s) => s,
            Conn::Tls(s) => &mut s.sock,
        }
    }
}
impl Read for Conn {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        match self {
            Conn::Plain(s) => s.read(b),
            Conn::Tls(s) => s.read(b),
        }
    }
}
impl Write for Conn {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        match self {
            Conn::Plain(s) => s.write(b),
            Conn::Tls(s) => s.write(b),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            Conn::Plain(s) => s.flush(),
            Conn::Tls(s) => s.flush(),
        }
    }
}

/// `GET path` with the upgrade headers plus `headers`; reads exactly the response head (no byte past it) and checks 101 + the accept key.
fn upgrade(c: &mut Conn, host: &str, path: &str, headers: &[(String, String)]) -> io::Result<()> {
    let clean = |s: &str| !s.contains(['\r', '\n']);
    if !clean(path) || !clean(host) || headers.iter().any(|(k, v)| !clean(k) || !clean(v) || k.is_empty()) {
        return Err(other("a header contains a line break"));
    }
    let key = generate_key();
    let mut req = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: {key}\r\n");
    for (k, v) in headers {
        if !k.eq_ignore_ascii_case("host") {
            req.push_str(&format!("{k}: {v}\r\n"));
        }
    }
    req.push_str("\r\n");
    c.write_all(req.as_bytes())?;
    c.flush()?;
    let mut head = Vec::with_capacity(512);
    let mut b = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() > 16 * 1024 {
            return Err(other("the push server's answer is too long"));
        }
        if c.read(&mut b)? == 0 {
            return Err(other("the push server closed the connection during the handshake"));
        }
        head.push(b[0]);
    }
    let text = String::from_utf8_lossy(&head);
    let mut lines = text.split("\r\n");
    let status: u16 = lines.next().and_then(|l| l.split_whitespace().nth(1)).and_then(|s| s.parse().ok()).unwrap_or(0);
    if status != 101 {
        return Err(other(format!("the push server refused the connection (HTTP {status})")));
    }
    let accept = lines.find_map(|l| {
        l.split_once(':')
            .filter(|(k, _)| k.eq_ignore_ascii_case("sec-websocket-accept"))
            .map(|(_, v)| v.trim().to_string())
    });
    if accept.as_deref() != Some(derive_accept_key(key.as_bytes()).as_str()) {
        return Err(other("the push server's handshake answer is not valid"));
    }
    Ok(())
}

impl PushConnector for LiveConnector {
    fn connect(&self, url: &str, headers: &[(String, String)]) -> io::Result<Box<dyn PushSocket>> {
        let (tls, rest) = match url.split_once("://") {
            Some(("wss", r)) => (true, r),
            Some(("ws", r)) => (false, r), // production only ever builds wss:// (see signaling::websocket_url); ws:// is for tests
            _ => return Err(other("not a WebSocket URL")),
        };
        let (authority, path) = rest.split_once('/').map_or((rest, "/".to_string()), |(a, p)| (a, format!("/{p}")));
        let (host, port) = match authority.rsplit_once(':') {
            Some((h, p)) => (h, p.parse::<u16>().map_err(|_| other("bad port"))?),
            None => (authority, if tls { 443 } else { 80 }),
        };
        let addr = (host, port).to_socket_addrs()?.next().ok_or_else(|| other("unresolvable push host"))?;
        let tcp = TcpStream::connect_timeout(&addr, Duration::from_secs(10))?;
        tcp.set_nodelay(true).ok();
        let tcp = Tcp { s: tcp, deadline: Some(Instant::now() + HANDSHAKE_TIMEOUT) };
        let mut conn = if tls {
            let name = rustls::pki_types::ServerName::try_from(host.to_string()).map_err(|_| other("bad host name"))?;
            let c = rustls::ClientConnection::new(tls_config(), name).map_err(other)?;
            Conn::Tls(Box::new(rustls::StreamOwned::new(c, tcp)))
        } else {
            Conn::Plain(tcp)
        };
        upgrade(&mut conn, authority, &path, headers)?;
        conn.tcp().deadline = None; // the handshake is over: reads now use the per-call timeout of `recv`
        let config = tungstenite::protocol::WebSocketConfig::default()
            .max_message_size(Some(MAX_PUSH_MESSAGE))
            .max_frame_size(Some(MAX_PUSH_MESSAGE));
        Ok(Box::new(LiveSocket {
            ws: WebSocket::from_raw_socket(conn, Role::Client, Some(config)),
            last_ping: Instant::now(),
        }))
    }
}

struct LiveSocket {
    ws: WebSocket<Conn>,
    last_ping: Instant,
}

impl PushSocket for LiveSocket {
    fn recv(&mut self, timeout: Duration) -> io::Result<Option<String>> {
        if self.last_ping.elapsed() >= Duration::from_secs(5) {
            self.last_ping = Instant::now();
            self.ws.send(Message::Ping(Vec::new().into())).map_err(other)?;
        }
        self.ws.get_mut().tcp().s.set_read_timeout(Some(timeout.max(Duration::from_millis(1))))?;
        match self.ws.read() {
            Ok(Message::Text(t)) => Ok(Some(t.as_str().to_string())),
            Ok(Message::Close(_)) => {
                Err(io::Error::new(io::ErrorKind::ConnectionAborted, "the push server closed the connection"))
            }
            Ok(_) => Ok(None), // pings are answered by the library; binary frames are ignored, as the original does
            Err(tungstenite::Error::Io(e))
                if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) =>
            {
                Ok(None)
            }
            Err(e) => Err(other(e)),
        }
    }

    fn close(&mut self) {
        let _ = self.ws.close(None);
        let _ = self.ws.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    /// A local WebSocket server (tungstenite's server side): records the handshake headers, sends text frames and a binary one, then
    /// either closes or lingers. `echo_protocol` says whether it answers the sub-protocol the way a strict server would.
    fn server(
        frames: Vec<&'static str>,
        close_after: bool,
        echo_protocol: bool,
    ) -> (u16, std::thread::JoinHandle<Vec<(String, String)>>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let h = std::thread::spawn(move || {
            let (s, _) = l.accept().unwrap();
            let mut seen: Vec<(String, String)> = vec![];
            #[allow(clippy::result_large_err)]
            let cb = |req: &tungstenite::handshake::server::Request,
                      mut resp: tungstenite::handshake::server::Response| {
                for (k, v) in req.headers() {
                    seen.push((k.as_str().to_string(), v.to_str().unwrap_or("").to_string()));
                }
                if echo_protocol {
                    resp.headers_mut()
                        .insert("Sec-WebSocket-Protocol", tungstenite::http::HeaderValue::from_static("np-pushpacket"));
                }
                Ok(resp)
            };
            let mut ws = tungstenite::accept_hdr(s, cb).unwrap();
            for f in frames {
                ws.send(Message::text(f)).unwrap();
            }
            ws.send(Message::Binary(vec![1, 2, 3].into())).unwrap();
            std::thread::sleep(Duration::from_millis(if close_after { 300 } else { 600 }));
            if close_after {
                let _ = ws.close(None);
                let _ = ws.flush();
            }
            seen
        });
        (port, h)
    }

    fn hdr(k: &str, v: &str) -> (String, String) {
        (k.to_string(), v.to_string())
    }

    fn read_n(s: &mut Box<dyn PushSocket>, n: usize) -> Vec<String> {
        let mut got = vec![];
        let end = Instant::now() + Duration::from_secs(3);
        while got.len() < n && Instant::now() < end {
            if let Some(m) = s.recv(Duration::from_millis(100)).unwrap() {
                got.push(m);
            }
        }
        got
    }

    #[test]
    fn connects_sends_the_headers_reads_text_frames_and_ignores_binary() {
        let (port, h) = server(vec!["{\"a\":1}", "{\"b\":2}"], false, true);
        let mut s = LiveConnector
            .connect(
                &format!("ws://127.0.0.1:{port}/np/pushNotification"),
                &[
                    hdr("Sec-WebSocket-Protocol", "np-pushpacket"),
                    hdr("X-PSN-APP-TYPE", "REMOTE_PLAY"),
                    hdr("Authorization", "Bearer T"),
                ],
            )
            .unwrap();
        assert_eq!(read_n(&mut s, 2), vec!["{\"a\":1}", "{\"b\":2}"], "binary frames are skipped");
        let t = Instant::now();
        assert_eq!(s.recv(Duration::from_millis(80)).unwrap(), None, "a quiet socket times out as None");
        assert!(t.elapsed() < Duration::from_secs(2));
        s.close();
        let seen = h.join().unwrap();
        let find = |k: &str| seen.iter().find(|(n, _)| n.eq_ignore_ascii_case(k)).map(|(_, v)| v.as_str());
        assert_eq!(
            (find("x-psn-app-type"), find("authorization"), find("sec-websocket-protocol")),
            (Some("REMOTE_PLAY"), Some("Bearer T"), Some("np-pushpacket"))
        );
        assert_eq!(find("sec-websocket-version"), Some("13"));
    }

    /// The reason the handshake is hand-rolled: a server that does NOT echo the sub-protocol must still work (the original's stack accepts it).
    #[test]
    fn a_server_that_does_not_echo_the_subprotocol_is_still_accepted() {
        let (port, h) = server(vec!["{\"x\":1}"], false, false);
        let mut s = LiveConnector
            .connect(&format!("ws://127.0.0.1:{port}/p"), &[hdr("Sec-WebSocket-Protocol", "np-pushpacket")])
            .unwrap();
        assert_eq!(read_n(&mut s, 1), vec!["{\"x\":1}"]);
        s.close();
        let _ = h.join();
    }

    #[test]
    fn a_server_that_closes_is_an_error_not_a_hang() {
        let (port, h) = server(vec![], true, false);
        let mut s = LiveConnector.connect(&format!("ws://127.0.0.1:{port}/x"), &[]).unwrap();
        let end = Instant::now() + Duration::from_secs(5);
        let mut err = None;
        while Instant::now() < end {
            if let Err(e) = s.recv(Duration::from_millis(100)) {
                err = Some(e);
                break;
            }
        }
        assert!(err.is_some(), "the close was reported");
        let _ = h.join();
    }

    #[test]
    fn a_refusing_or_bogus_server_is_an_error_with_the_status() {
        for (answer, want) in [
            ("HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n", "HTTP 403"),
            (
                "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nSec-WebSocket-Accept: wrong\r\n\r\n",
                "not valid",
            ),
            ("garbage", "closed the connection"),
        ] {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = l.local_addr().unwrap().port();
            let h = std::thread::spawn(move || {
                let (mut s, _) = l.accept().unwrap();
                let mut b = [0u8; 2048];
                let _ = s.read(&mut b);
                let _ = s.write_all(answer.as_bytes());
            });
            let e = LiveConnector.connect(&format!("ws://127.0.0.1:{port}/x"), &[]).err().expect("must fail");
            assert!(e.to_string().contains(want), "{answer:?} -> {e}");
            h.join().unwrap();
        }
    }

    #[test]
    fn bad_input_is_rejected_before_any_network_use() {
        assert!(LiveConnector.connect("not a url", &[]).is_err());
        assert!(LiveConnector.connect("http://x/y", &[]).is_err());
        assert!(
            LiveConnector.connect("wss://push.example.net/x", &[hdr("A", "b\r\nInjected: 1")]).is_err(),
            "no header injection"
        );
        assert!(LiveConnector.connect("ws://127.0.0.1:1/x", &[]).is_err(), "nothing listening: an error, quickly");
    }

    #[test]
    fn the_tls_configuration_builds_with_an_explicit_provider() {
        let _ = tls_config(); // no process-wide crypto provider is needed
    }
}
