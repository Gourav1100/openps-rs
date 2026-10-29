//! The whole remote registration, driven against traits for HTTP, the push socket and UDP so that it can be tested end to end against
//! a fake PSN and a fake console. The sequence is 09b section 1/5; each step names the spec section it follows.

use crate::keys::{
    attempt_secrets, build_register_request, decode_custom_data, parse_register_response, KeyError, RemoteRegistered,
};
use crate::punch::{self, local_ip_toward, Ids};
use crate::rudp::{self, UdpWire};
use crate::signaling::*;
use base64::Engine as _;
use openps_psn::http::{HttpClient, HttpRequest, HttpResponse};
use openps_rp::tables::KeyTables;
use rand_core::RngCore;
use std::io;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// A connected push WebSocket: text messages in, nothing out (everything we say goes over HTTPS).
pub trait PushSocket {
    /// The next text message, or `None` after `timeout`. `Err` when the socket is closed or broken.
    fn recv(&mut self, timeout: Duration) -> io::Result<Option<String>>;
    fn close(&mut self) {}
}

pub trait PushConnector {
    fn connect(&self, url: &str, headers: &[(String, String)]) -> io::Result<Box<dyn PushSocket>>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Looking up the push server and opening the connection.
    Connecting,
    /// Creating the PSN session and asking the console to join.
    Session,
    /// Waiting for the console to answer through PSN.
    WaitingForConsole,
    /// Reaching the console directly over the network.
    Reaching,
    /// Exchanging the registration with the console.
    Registering,
}

pub trait Progress {
    fn step(&self, s: Step);
}
pub struct NoProgress;
impl Progress for NoProgress {
    fn step(&self, _: Step) {}
}

#[derive(Debug, PartialEq, Eq)]
pub enum RemoteError {
    Cancelled,
    TimedOut,
    /// PSN refused or failed a request.
    Psn(String),
    /// The console (or the path to it) did not do what the exchange needs.
    Console(String),
    Key(KeyError),
    /// A message from the console that could not be read: the redacted text is for the log, not for the user.
    Unreadable(String),
}

impl std::fmt::Display for RemoteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RemoteError::Cancelled => f.write_str("cancelled"),
            RemoteError::TimedOut => f.write_str("the console did not answer in time"),
            RemoteError::Psn(e) => write!(f, "PlayStation Network: {e}"),
            RemoteError::Console(e) => f.write_str(e),
            RemoteError::Key(e) => write!(f, "{e}"),
            RemoteError::Unreadable(_) => {
                f.write_str("an unreadable message from the console (details are in the log)")
            }
        }
    }
}
impl std::error::Error for RemoteError {}

pub struct Params<'a> {
    pub tables: &'a KeyTables,
    /// The Remote Play login's access token.
    pub access_token: String,
    /// The PSN account id (unsigned 64-bit).
    pub account_id: u64,
    /// The console's id in PSN, from the console list.
    pub duid: String,
    /// Our IPv4 addresses, advertised as LOCAL candidates.
    pub local_ips: Vec<Ipv4Addr>,
    /// A public address to advertise as STATIC (optional).
    pub public_ip: Option<String>,
    /// Bound UDP socket (port != 0): used for the hole punch and the register exchange.
    pub socket: UdpSocket,
    pub accept_style: AcceptStyle,
    /// Probe loopback candidates too (tests only: a real console is never on loopback).
    pub allow_loopback: bool,
    pub timeout: Duration,
    pub cancel: Arc<AtomicBool>,
    /// How long the hole punch waits per round, and each register datagram step (shortened by tests).
    pub round_wait: Duration,
    pub step_wait: Duration,
}

fn bearer(token: &str) -> (String, String) {
    ("Authorization".into(), format!("Bearer {token}"))
}

fn check(resp: &HttpResponse, what: &str) -> Result<(), RemoteError> {
    match resp.status {
        200..=299 => Ok(()),
        401 | 403 => Err(RemoteError::Psn(format!(
            "{what}: your PlayStation sign-in was refused (HTTP {}). Sign in again.",
            resp.status
        ))),
        s => Err(RemoteError::Psn(format!("{what} failed (HTTP {s})"))),
    }
}

fn call(http: &dyn HttpClient, req: &HttpRequest, what: &str) -> Result<HttpResponse, RemoteError> {
    let resp = http.execute(req).map_err(|e| RemoteError::Psn(format!("{what}: {e}")))?;
    check(&resp, what)?;
    Ok(resp)
}

fn json_request(method: &'static str, url: &str, token: &str, body: String) -> HttpRequest {
    HttpRequest {
        method,
        url: url.to_string(),
        headers: vec![
            bearer(token),
            ("Content-Type".into(), "application/json; charset=utf-8".into()),
            ("User-Agent".into(), "RpNetHttpUtilImpl".into()),
        ],
        body: body.into_bytes(),
    }
}

fn ids_from(b64: &str) -> [u8; 20] {
    let mut out = [0u8; 20];
    if let Ok(v) = base64::engine::general_purpose::STANDARD.decode(b64.trim()) {
        for (o, x) in out.iter_mut().zip(v) {
            *o = x;
        }
    }
    out
}

/// Run the registration. Returns the console's registration, or why it did not work. Always leaves the PSN session.
pub fn register<R: RngCore>(
    p: &Params,
    http: &dyn HttpClient,
    push: &dyn PushConnector,
    rng: &mut R,
    progress: &dyn Progress,
) -> Result<RemoteRegistered, RemoteError> {
    let mut leave: Option<String> = None;
    let mut ws: Option<Box<dyn PushSocket>> = None;
    let result = run(p, http, push, rng, progress, &mut ws, &mut leave);
    // cleanup, whatever happened: leave the session, close the socket
    if let Some(sid) = leave {
        let req = HttpRequest {
            method: "DELETE",
            url: leave_session_url(&sid),
            headers: vec![bearer(&p.access_token)],
            body: vec![],
        };
        let _ = http.execute(&req);
    }
    if let Some(mut w) = ws {
        w.close();
    }
    result
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum State {
    WaitCreated,
    WaitOffer,
    Punching,
}

#[allow(clippy::too_many_lines)]
fn run<R: RngCore>(
    p: &Params,
    http: &dyn HttpClient,
    push: &dyn PushConnector,
    rng: &mut R,
    progress: &dyn Progress,
    ws_slot: &mut Option<Box<dyn PushSocket>>,
    leave: &mut Option<String>,
) -> Result<RemoteRegistered, RemoteError> {
    let deadline = Instant::now() + p.timeout;
    let cancelled = || p.cancel.load(Ordering::Relaxed);
    let my_port = p.socket.local_addr().map_err(|e| RemoteError::Console(format!("no UDP socket: {e}")))?.port();
    if my_port == 0 {
        return Err(RemoteError::Console("no UDP port".into()));
    }

    // per-attempt values (09b section 0)
    let (secret_i, secret_j) = attempt_secrets(rng);
    let context_id = uuid_v4(rng);
    let mut hid = [0u8; 20];
    rng.fill_bytes(&mut hid);
    let my_hashed_id = base64::engine::general_purpose::STANDARD.encode(hid);
    let my_sid = 5000 + (rng.next_u32() % 20000) as u16;

    // 1. push server, 2. WebSocket
    progress.step(Step::Connecting);
    let resp = call(
        http,
        &HttpRequest {
            method: "GET",
            url: PUSH_SERVER_URL.into(),
            headers: vec![bearer(&p.access_token)],
            body: vec![],
        },
        "push server lookup",
    )?;
    #[derive(serde::Deserialize)]
    struct Addr {
        fqdn: Option<String>,
    }
    let fqdn = serde_json::from_slice::<Addr>(&resp.body).ok().and_then(|a| a.fqdn).unwrap_or_default();
    let url = websocket_url(&fqdn).ok_or_else(|| RemoteError::Psn("the push server address is not usable".into()))?;
    let mut headers: Vec<(String, String)> = WS_HEADERS.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    headers.push(bearer(&p.access_token));
    headers.push(("X-PSN-RECONNECTION".into(), "false".into()));
    *ws_slot =
        Some(push.connect(&url, &headers).map_err(|e| RemoteError::Psn(format!("push connection to {fqdn}: {e}")))?);
    let ws = ws_slot.as_mut().expect("just set");

    // 3. create the session
    progress.step(Step::Session);
    let r = call(
        http,
        &json_request("POST", CREATE_SESSION_URL, &p.access_token, create_session_body(&context_id)),
        "creating the session",
    )?;
    let answer = serde_json::from_slice::<serde_json::Value>(&r.body)
        .map_err(|_| RemoteError::Psn("creating the session: the answer is not JSON".into()))?;
    // the answer names the session: remember it at once so that it is deleted on every exit path, even if the push
    // notification never arrives
    let mut session_id = String::new();
    if let Some(id) = answer["remotePlaySessions"][0]["sessionId"].as_str().filter(|i| valid_session_id(i)) {
        session_id = id.to_string();
        *leave = Some(session_id.clone());
    }
    let mut state = State::WaitCreated;
    let mut l: Option<[u8; 16]> = None;
    let mut offered = false;
    let mut peer: Option<(Ids, Vec<Candidate>, i64)> = None; // ids, console candidates, console sid
    let stop = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel::<Option<crate::signaling::PunchResult>>();
    let mut punched: Option<crate::signaling::PunchResult> = None;
    let mut accepted_ack = false; // the console's RESULT reqId 2

    let send_msg = |http: &dyn HttpClient, sid: &str, payload: &str| -> Result<(), RemoteError> {
        let body = session_message_body(payload, p.account_id, &p.duid);
        call(http, &json_request("POST", &session_message_url(sid), &p.access_token, body), "sending a session message")
            .map(|_| ())
    };

    let outcome = (|| -> Result<RemoteRegistered, RemoteError> {
        loop {
            if cancelled() {
                return Err(RemoteError::Cancelled);
            }
            if Instant::now() >= deadline {
                return Err(RemoteError::TimedOut);
            }
            // the hole punch finished?
            if let Ok(r) = rx.try_recv() {
                match r {
                    Some(res) => {
                        let (ids, _, peer_sid) = peer.as_ref().expect("punch only runs after an offer");
                        let accept = accept_payload(2, ids.own_sid, *peer_sid, 0, &res, p.account_id, p.accept_style);
                        send_msg(http, &session_id, &accept)?;
                        punched = Some(res);
                    }
                    None => return Err(RemoteError::Console("could not reach your console over the network. It must be on and reachable from this computer.".into())),
                }
            }
            // everything needed for the register exchange is there
            if accepted_ack {
                if let (Some(res), Some(l)) = (&punched, &l) {
                    progress.step(Step::Registering);
                    let console = SocketAddr::new(
                        res.console_ip.parse().map_err(|_| RemoteError::Console("bad console address".into()))?,
                        res.console_port,
                    );
                    let host = local_ip_toward(console)
                        .ok_or_else(|| RemoteError::Console("no local address toward the console".into()))?;
                    let mut body480 = [0u8; 480];
                    rng.fill_bytes(&mut body480);
                    let mut nonce = [0u8; 16];
                    rng.fill_bytes(&mut nonce);
                    let (req, cipher) = build_register_request(p.tables, l, &body480, nonce, p.account_id, host);
                    let seq0 = 511 + (rng.next_u32() % 24064) as u16;
                    let tag = 32768 + (rng.next_u32() % (i32::MAX as u32 - 32768));
                    let sock = p.socket.try_clone().map_err(|e| RemoteError::Console(format!("UDP socket: {e}")))?;
                    let mut wire = UdpWire { socket: sock, peer: console };
                    let lim = rudp::Limits { cancel: Some(&p.cancel), deadline: Some(deadline) };
                    let answer =
                        rudp::register_until(&mut wire, &req, seq0, tag, p.step_wait, &lim).map_err(|e| match e {
                            rudp::RudpError::Cancelled => RemoteError::Cancelled,
                            rudp::RudpError::TimedOut => RemoteError::TimedOut,
                            e => RemoteError::Console(e.to_string()),
                        })?;
                    return parse_register_response(&cipher, &answer).map_err(RemoteError::Key);
                }
            }

            let Some(text) = ws
                .recv(Duration::from_millis(200))
                .map_err(|e| RemoteError::Psn(format!("the push connection closed: {e}")))?
            else {
                continue;
            };
            let Some(msg) = WsMessage::parse(&text) else { continue };
            if !msg.is_for_us(&session_id, &context_id) {
                continue;
            }
            match msg.data_type() {
                N_SESSION_DELETED | N_MEMBER_DELETED => return Err(RemoteError::Psn("PSN closed the session".into())),
                N_CUSTOM_DATA => {
                    let c = msg.custom_data1();
                    if !c.is_empty() {
                        l = Some(decode_custom_data(p.tables, &secret_i, &secret_j, c).map_err(RemoteError::Key)?);
                    }
                }
                N_CREATED if state == State::WaitCreated => {
                    session_id = msg.session_id().to_string();
                    *leave = Some(session_id.clone());
                    let body = join_command_body(&p.duid, p.account_id, &session_id, &secret_i, &secret_j);
                    call(
                        http,
                        &HttpRequest {
                            method: "POST",
                            url: COMMANDS_URL.into(),
                            headers: json_request("POST", COMMANDS_URL, &p.access_token, String::new()).headers,
                            body: body.into_bytes(),
                        },
                        "asking the console to join",
                    )?;
                    progress.step(Step::WaitingForConsole);
                    state = State::WaitOffer;
                }
                N_MESSAGE => {
                    let Some(payload) = msg.payload().and_then(EndpointPayload::parse) else {
                        return Err(RemoteError::Unreadable(redact(msg.payload().unwrap_or(""))));
                    };
                    let req_id = payload.req_id.unwrap_or(0);
                    match payload.action() {
                        "OFFER" if state != State::WaitCreated => {
                            send_msg(http, &session_id, &result_payload(req_id))?;
                            if offered {
                                continue;
                            }
                            let Some(cr) = payload.conn_request else {
                                return Err(RemoteError::Console(
                                    "the console's offer has no connection details".into(),
                                ));
                            };
                            if cr.candidate.is_empty() {
                                return Err(RemoteError::Console("the console's offer has no addresses".into()));
                            }
                            offered = true;
                            let peer_sid = cr.sid.unwrap_or(0);
                            let ids = Ids {
                                own_id: hid,
                                peer_id: ids_from(cr.local_hashed_id.as_deref().unwrap_or("")),
                                own_sid: my_sid,
                                peer_sid: (peer_sid & 0xFFFF) as u16,
                            };
                            let mut cands: Vec<String> =
                                p.local_ips.iter().map(|ip| local_candidate(&ip.to_string(), my_port)).collect();
                            if let Some(ip) = &p.public_ip {
                                cands.push(static_candidate(ip, my_port));
                            }
                            send_msg(
                                http,
                                &session_id,
                                &offer_payload(1, my_sid, 0, &cands, p.account_id, &my_hashed_id),
                            )?;
                            state = State::Punching;
                            progress.step(Step::Reaching);
                            let (sock, c2, ids2, tx2, stop2) = (
                                p.socket.try_clone().map_err(|e| RemoteError::Console(format!("UDP socket: {e}")))?,
                                cr.candidate.clone(),
                                ids.clone(),
                                tx.clone(),
                                stop.clone(),
                            );
                            let (wait, lb) = (p.round_wait, p.allow_loopback);
                            let nonce = rng.next_u64() & 0xFF_FFFF_FFFF; // 40 random bits, the range the original uses
                            std::thread::spawn(move || {
                                let _ = tx2.send(punch::hole_punch(&sock, &c2, &ids2, nonce, &stop2, wait, lb));
                            });
                            peer = Some((ids, cr.candidate, peer_sid));
                        }
                        "ACCEPT" if state == State::Punching => send_msg(http, &session_id, &result_payload(req_id))?,
                        "RESULT" => {
                            if payload.error.unwrap_or(0) != 0 {
                                return Err(RemoteError::Console("the console refused the connection request".into()));
                            }
                            if state == State::Punching && req_id == 2 {
                                accepted_ack = true;
                            }
                        }
                        "TERMINATE" => {
                            return Err(RemoteError::Console("the console ended the connection request".into()))
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    })();
    stop.store(true, Ordering::Relaxed);
    outcome
}

fn uuid_v4<R: RngCore>(rng: &mut R) -> String {
    let mut b = [0u8; 16];
    rng.fill_bytes(&mut b);
    b[6] = (b[6] & 0x0F) | 0x40;
    b[8] = (b[8] & 0x3F) | 0x80;
    let h: Vec<String> = b.iter().map(|x| format!("{x:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        h[0..4].concat(),
        h[4..6].concat(),
        h[6..8].concat(),
        h[8..10].concat(),
        h[10..16].concat()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixed(u8);
    impl RngCore for Fixed {
        fn next_u32(&mut self) -> u32 {
            self.0 = self.0.wrapping_add(1);
            u32::from(self.0)
        }
        fn next_u64(&mut self) -> u64 {
            u64::from(self.next_u32())
        }
        fn fill_bytes(&mut self, d: &mut [u8]) {
            for x in d {
                self.0 = self.0.wrapping_add(7);
                *x = self.0;
            }
        }
        fn try_fill_bytes(&mut self, d: &mut [u8]) -> Result<(), rand_core::Error> {
            self.fill_bytes(d);
            Ok(())
        }
    }

    #[test]
    fn uuid_is_version_4_and_formatted() {
        let u = uuid_v4(&mut Fixed(1));
        assert_eq!(u.len(), 36);
        assert_eq!(&u[14..15], "4");
        assert!(matches!(&u[19..20], "8" | "9" | "a" | "b"));
        assert_eq!(u.matches('-').count(), 4);
    }

    #[test]
    fn peer_ids_are_padded_or_truncated_to_twenty_bytes_and_garbage_is_zero() {
        let b64 = |v: &[u8]| base64::engine::general_purpose::STANDARD.encode(v);
        assert_eq!(ids_from(&b64(&[9u8; 20])), [9u8; 20]);
        assert_eq!(&ids_from(&b64(&[9u8; 5]))[..6], &[9, 9, 9, 9, 9, 0]);
        assert_eq!(ids_from(&b64(&[9u8; 40])), [9u8; 20]);
        assert_eq!(ids_from("!!!not base64"), [0u8; 20]);
    }
}

/// Our IPv4 addresses from `ifconfig` output: every `inet` address that is not loopback or link-local (advertised as LOCAL candidates).
pub fn parse_ifconfig_ipv4(out: &str) -> Vec<Ipv4Addr> {
    let mut v: Vec<Ipv4Addr> = vec![];
    for l in out.lines() {
        let mut w = l.split_whitespace();
        if w.next() == Some("inet") {
            if let Some(ip) = w.next().and_then(|a| a.parse::<Ipv4Addr>().ok()) {
                if !ip.is_loopback() && !ip.is_link_local() && !ip.is_unspecified() && !v.contains(&ip) {
                    v.push(ip);
                }
            }
        }
    }
    v
}

#[cfg(test)]
mod net_tests {
    use super::*;

    #[test]
    fn ifconfig_addresses_are_found_and_loopback_and_link_local_are_not() {
        let out = "lo0: flags=8049<UP,LOOPBACK,RUNNING,MULTICAST> mtu 16384\n\tinet 127.0.0.1 netmask 0xff000000\n\tinet6 ::1 prefixlen 128\nen0: flags=8863<UP> mtu 1500\n\tinet 192.168.1.10 netmask 0xffffff00 broadcast 192.168.1.255\n\tinet6 fe80::1 prefixlen 64\nen1: flags=8863<UP>\n\tinet 169.254.7.7 netmask 0xffff0000\nutun0: flags=8051<UP>\n\tinet 10.8.0.2 --> 10.8.0.1 netmask 0xffffffff\nen0b: x\n\tinet 192.168.1.10 netmask 0xffffff00\n\tinet notanip\n";
        assert_eq!(parse_ifconfig_ipv4(out), vec![Ipv4Addr::new(192, 168, 1, 10), Ipv4Addr::new(10, 8, 0, 2)]);
        assert!(parse_ifconfig_ipv4("").is_empty());
    }
}
