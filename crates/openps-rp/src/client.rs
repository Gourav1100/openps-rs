//! Blocking client for the local-network flow: pair with a console (`register`) and open a session
//! (`open_session`). Both talk plain HTTP to the console on port 9295; the session keeps its TCP connection for
//! the control channel.

use crate::ctrl::{CtrlRx, CtrlTx, FrameError, Inbound};
use crate::http::{self, NoLength, Request};
use crate::regist::{self, RegisterError, RegisterParams, Registered};
use crate::session::{self, CtrlInfo, CtrlParams, CtrlResponseError, CtrlState, InitError, Retry, StreamingType};
use crate::tables::KeyTables;
use openps_crypto::RpCipher;
use openps_discovery::{DeviceType, REGISTRATION_PORT};
use rand_core::RngCore;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct Timeouts {
    pub connect: Duration,
    /// Per read/write on the socket (the app uses 20 s).
    pub io: Duration,
    /// Pause before retrying a failed init (the app uses 2 s).
    pub retry_delay: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self { connect: Duration::from_secs(5), io: Duration::from_secs(20), retry_delay: Duration::from_secs(2) }
    }
}

#[derive(Debug)]
pub enum ClientError {
    Io(io::Error),
    Http(String),
    Register(RegisterError),
    Init(InitError),
    Ctrl(CtrlResponseError),
    BadKey,
    Frame(FrameError),
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientError::Io(e) => write!(f, "network error: {e}"),
            ClientError::Http(e) => write!(f, "bad HTTP response: {e}"),
            ClientError::Register(RegisterError::Rejected { status, reason }) => {
                write!(f, "registration rejected (HTTP {status}): {}", regist::reason_text(reason))
            }
            ClientError::Register(e) => write!(f, "registration failed: {e:?}"),
            ClientError::Init(e) => write!(f, "session init failed: {e:?}"),
            ClientError::Ctrl(e) => write!(f, "control channel refused: {e:?}"),
            ClientError::BadKey => write!(f, "stored registration keys are invalid"),
            ClientError::Frame(e) => write!(f, "control channel protocol error: {e:?}"),
        }
    }
}
impl std::error::Error for ClientError {}

impl From<io::Error> for ClientError {
    fn from(e: io::Error) -> Self {
        ClientError::Io(e)
    }
}

fn connect(addr: SocketAddr, t: &Timeouts) -> io::Result<TcpStream> {
    let s = TcpStream::connect_timeout(&addr, t.connect)?;
    s.set_read_timeout(Some(t.io))?;
    s.set_write_timeout(Some(t.io))?;
    s.set_nodelay(true)?;
    Ok(s)
}

fn resolve(host: &str, port: u16) -> io::Result<SocketAddr> {
    (host, port).to_socket_addrs()?.next().ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no address"))
}

fn exchange(stream: &mut TcpStream, req: &Request, nl: NoLength) -> Result<(http::Response, Vec<u8>), ClientError> {
    http::exchange(stream, req, nl).map_err(|e| {
        if e.kind() == io::ErrorKind::InvalidData {
            ClientError::Http(e.to_string())
        } else {
            ClientError::Io(e)
        }
    })
}

/// Pair with a console using the PIN it displays. `port` is 9295 in production.
pub fn register(
    t: &KeyTables,
    host: &str,
    port: u16,
    p: &RegisterParams,
    rng: &mut impl RngCore,
    timeouts: &Timeouts,
) -> Result<Registered, ClientError> {
    let built = regist::build_request(t, p, host, rng);
    let mut stream = connect(resolve(host, port)?, timeouts)?;
    let (resp, _) = exchange(&mut stream, &built.request, NoLength::UntilClose)?;
    regist::parse_response(&built.cipher, &resp).map_err(ClientError::Register)
}

/// An open session: the control channel plus what the Takion handshake needs from it.
pub struct ControlSession {
    pub stream: TcpStream,
    pub tx: CtrlTx,
    pub rx: CtrlRx,
    pub info: CtrlInfo,
    /// Protocol version actually used (init may have downgraded it).
    pub version: String,
    /// Session cipher: its counter-0 keystream encrypts the Takion launch spec.
    pub cipher: RpCipher,
    /// Session id announced on the control channel (ctrl frame 51), once seen.
    pub session_id: Option<String>,
    /// Inbound messages seen while waiting for the session id.
    pub pending: Vec<Inbound>,
}

pub struct OpenParams<'a> {
    pub host: &'a str,
    pub port: u16,
    pub device: DeviceType,
    pub version: &'a str,
    pub registration: &'a Registered,
    pub streaming: StreamingType,
}

fn init_with_retries(p: &OpenParams, timeouts: &Timeouts) -> Result<([u8; 16], String), ClientError> {
    let addr = resolve(p.host, p.port)?;
    let mut version = p.version.to_string();
    let (mut again_left, mut io_left) = (2u32, 5u32);
    loop {
        let result: Result<[u8; 16], ClientError> = (|| {
            let mut s = connect(addr, timeouts)?;
            let (resp, _) = exchange(
                &mut s,
                &session::init_request(p.host, &p.registration.regist_key, &version),
                NoLength::UntilClose,
            )?;
            session::parse_init_response(&resp).map_err(ClientError::Init)
        })();
        match result {
            Ok(nonce) => return Ok((nonce, version)),
            Err(ClientError::Io(_)) if io_left > 0 => {
                io_left -= 1;
                std::thread::sleep(timeouts.retry_delay);
            }
            Err(ClientError::Init(e)) => match e.retry(&version) {
                Retry::Downgrade(v) => {
                    version = v.to_string();
                    std::thread::sleep(timeouts.retry_delay);
                }
                Retry::Again { max } if again_left > 0 && max > 0 => {
                    again_left -= 1;
                    std::thread::sleep(timeouts.retry_delay);
                }
                _ => return Err(ClientError::Init(e)),
            },
            Err(e) => return Err(e),
        }
    }
}

/// `GET init`, derive the session cipher, `GET ctrl`, and return the live control channel.
pub fn open_session(
    t: &KeyTables,
    p: &OpenParams,
    rng: &mut impl RngCore,
    timeouts: &Timeouts,
) -> Result<ControlSession, ClientError> {
    let (nonce, version) = init_with_retries(p, timeouts)?;
    let cp = CtrlParams {
        host: p.host,
        version: &version,
        device: p.device,
        regist_key_hex: &p.registration.regist_key,
        rp_key_hex: &p.registration.rp_key,
        streaming: p.streaming,
    };
    let (req, mut state) = session::ctrl_request(t, &cp, &nonce, rng).map_err(|_| ClientError::BadKey)?;
    let mut stream = connect(resolve(p.host, p.port)?, timeouts)?;
    let (resp, leftover) = exchange(&mut stream, &req, NoLength::Empty)?; // binary frames follow the headers
    let info = session::parse_ctrl_response(&mut state, p.device, &resp).map_err(ClientError::Ctrl)?;
    let CtrlState { cipher, out_counter, in_counter } = state;
    let mut rx = CtrlRx::new(cipher.clone(), in_counter);
    rx.feed(&leftover);
    Ok(ControlSession {
        stream,
        tx: CtrlTx::new(cipher.clone(), out_counter),
        rx,
        info,
        version,
        cipher,
        session_id: None,
        pending: vec![],
    })
}

impl ControlSession {
    /// Read frames until the session id arrives (or `timeout`). Keepalives are answered; other messages are kept
    /// in `pending`. Falls back to `fallback` (e.g. unix seconds plus random text, as the app does) on timeout.
    pub fn wait_for_session_id(
        &mut self,
        timeout: Duration,
        fallback: impl FnOnce() -> String,
    ) -> Result<&str, ClientError> {
        let end = Instant::now() + timeout;
        let mut buf = [0u8; 2048];
        while self.session_id.is_none() {
            while let Some(m) = self.rx.next_message().map_err(ClientError::Frame)? {
                match m {
                    Inbound::SessionId(id) => self.session_id = Some(id),
                    Inbound::Keepalive => self.stream.write_all(&crate::ctrl::KEEPALIVE_REPLY)?,
                    other => self.pending.push(other),
                }
            }
            if self.session_id.is_some() {
                break;
            }
            let now = Instant::now();
            if now >= end {
                self.session_id = Some(fallback());
                break;
            }
            self.stream.set_read_timeout(Some((end - now).max(Duration::from_millis(1))))?;
            match self.stream.read(&mut buf) {
                Ok(0) => {
                    return Err(ClientError::Io(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "console closed the control channel",
                    )))
                }
                Ok(n) => self.rx.feed(&buf[..n]),
                Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(self.session_id.as_deref().unwrap_or(""))
    }

    /// Send a frame built by `tx` (e.g. `session.send(|tx| tx.pad_join(1, 6))`).
    pub fn send(&mut self, build: impl FnOnce(&mut CtrlTx) -> Vec<u8>) -> io::Result<()> {
        let f = build(&mut self.tx);
        self.stream.write_all(&f)
    }
}

pub const DEFAULT_PORT: u16 = REGISTRATION_PORT;
