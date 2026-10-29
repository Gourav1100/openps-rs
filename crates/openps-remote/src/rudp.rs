//! The small reliable-UDP layer the register request rides on (09b section 3.2-3.3). A datagram is a chain of chunks:
//! `u16 len (0xC000 | 8 + data)`, `24 4F 24 4F`, `u16 type`, data. All integers big-endian. No checksum.

use std::io;
use std::time::{Duration, Instant};

pub const T_INIT: u16 = 0x8030;
pub const T_COOKIE: u16 = 0x9030;
pub const T_HEAD: u16 = 0x2030;
pub const T_FINAL: u16 = 0x2430;
pub const T_DATA: u16 = 0x0230;

/// Constants the console expects inside INIT and COOKIE chunks (meaning unknown; copied from the original).
const INIT_MID: [u8; 6] = [0x0b, 0x01, 0x01, 0x00, 0x01, 0x00];
const INIT_END: [u8; 2] = [0x05, 0x82];
const FINAL_END: [u8; 2] = [0x00, 0x92];

/// Largest chunk payload the 12-bit length field can describe.
pub const MAX_CHUNK_DATA: usize = 0x0FFF - 8;

/// One chunk (also used by test consoles).
/// The length field has 12 bits: data above [`MAX_CHUNK_DATA`] bytes is cut (it could not be read back anyway).
pub fn chunk(ty: u16, data: &[u8]) -> Vec<u8> {
    let data: &[u8] = if data.len() < 4 { &[0, 0, 0, 0] } else { data.get(..MAX_CHUNK_DATA).unwrap_or(data) };
    let mut out = Vec::with_capacity(8 + data.len());
    out.extend(((0xC000u16) | ((8 + data.len()) as u16 & 0x0FFF)).to_be_bytes());
    out.extend([0x24, 0x4F, 0x24, 0x4F]);
    out.extend(ty.to_be_bytes());
    out.extend(data);
    out
}

fn hello(seq: u16, tag: u32) -> Vec<u8> {
    let mut d = seq.to_be_bytes().to_vec();
    d.extend(INIT_MID);
    d.extend(tag.to_be_bytes());
    d.extend(INIT_END);
    d
}

/// First datagram: opens the association.
pub fn init(seq: u16, tag: u32) -> Vec<u8> {
    chunk(T_INIT, &hello(seq, tag))
}

/// Second datagram: echoes the cookie the console answered the INIT with.
pub fn cookie(seq: u16, tag: u32, cookie: &[u8]) -> Vec<u8> {
    let mut d = hello(seq, tag);
    d.extend(cookie);
    chunk(T_COOKIE, &d)
}

/// Third datagram: carries the request (two chunks: a small head, then `seq` and the request bytes).
pub fn request(seq: u16, n3: u16, req: &[u8]) -> Vec<u8> {
    let mut head = seq.to_be_bytes().to_vec();
    head.extend(n3.to_be_bytes());
    let mut out = chunk(T_HEAD, &head);
    let mut data = seq.to_be_bytes().to_vec();
    data.extend(req);
    out.extend(chunk(T_DATA, &data));
    out
}

/// Last datagram: acknowledges the answer.
pub fn fin(seq: u16, n3: u16) -> Vec<u8> {
    let mut d = seq.to_be_bytes().to_vec();
    d.extend(n3.to_be_bytes());
    d.extend(FINAL_END);
    chunk(T_FINAL, &d)
}

/// The first chunk of a datagram (the original never reads later ones): its type and data.
#[derive(Debug, PartialEq, Eq)]
pub struct Chunk {
    pub ty: u16,
    pub data: Vec<u8>,
}

pub fn parse_first_chunk(dg: &[u8]) -> Option<Chunk> {
    if dg.len() < 8 {
        return None;
    }
    let len = (u16::from_be_bytes([dg[0], dg[1]]) & 0x0FFF) as usize;
    let ty = u16::from_be_bytes([dg[6], dg[7]]);
    let data = if len > 8 { dg[8..dg.len().min(len)].to_vec() } else { vec![] };
    Some(Chunk { ty, data })
}

// What the original tests for in each reply: only the high byte (or low nibble) of the type byte at offset 6.
pub fn is_init_reply(dg: &[u8]) -> bool {
    dg.len() >= 8 && dg[6] == 0xD0
}
pub fn is_cookie_reply(dg: &[u8]) -> bool {
    dg.len() >= 8 && dg[6] == 0xA0
}
pub fn is_data_reply(dg: &[u8]) -> bool {
    dg.len() >= 8 && matches!(dg[6] & 0x0F, 2 | 6)
}
pub fn is_final_ack(dg: &[u8]) -> bool {
    dg.len() >= 8 && dg[6] == 0xC0
}

/// A datagram path: where the register exchange sends and receives.
pub trait Wire {
    fn send(&mut self, datagram: &[u8]) -> io::Result<()>;
    /// One datagram, or `None` after `timeout`.
    fn recv(&mut self, timeout: Duration) -> io::Result<Option<Vec<u8>>>;
}

/// Cancel flag and overall deadline of an exchange, checked after every datagram step.
#[derive(Clone, Copy, Default)]
pub struct Limits<'a> {
    pub cancel: Option<&'a std::sync::atomic::AtomicBool>,
    pub deadline: Option<Instant>,
}

impl Limits<'_> {
    fn check(&self) -> Result<(), RudpError> {
        if self.cancel.is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed)) {
            return Err(RudpError::Cancelled);
        }
        if self.deadline.is_some_and(|d| Instant::now() >= d) {
            return Err(RudpError::TimedOut);
        }
        Ok(())
    }
}

/// Send up to 10 times; after each send read up to 5 datagrams (100 ms each) until `want` accepts one.
fn exchange(
    w: &mut dyn Wire,
    out: &[u8],
    want: impl Fn(&[u8]) -> bool,
    step: Duration,
    lim: &Limits,
) -> Result<Option<Vec<u8>>, RudpError> {
    for _ in 0..10 {
        lim.check()?;
        w.send(out)?;
        for _ in 0..5 {
            lim.check()?;
            if let Some(dg) = w.recv(step)? {
                if dg.len() >= 8 && want(&dg) {
                    return Ok(Some(dg));
                }
            }
        }
    }
    lim.check()?;
    Ok(None)
}

/// Why the exchange did not produce an answer.
#[derive(Debug, PartialEq, Eq)]
pub enum RudpError {
    NoInitReply,
    NoCookieReply,
    NoAnswer,
    /// The cancel flag was raised.
    Cancelled,
    /// The overall deadline passed.
    TimedOut,
    Malformed(&'static str),
    Io(String),
}

impl std::fmt::Display for RudpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RudpError::NoInitReply => f.write_str("the console did not answer the connection request"),
            RudpError::NoCookieReply => f.write_str("the console did not complete the connection"),
            RudpError::NoAnswer => f.write_str("the console did not answer the registration"),
            RudpError::Cancelled => f.write_str("cancelled"),
            RudpError::TimedOut => f.write_str("the console did not answer in time"),
            RudpError::Malformed(w) => write!(f, "unexpected answer from the console: {w}"),
            RudpError::Io(e) => write!(f, "network error: {e}"),
        }
    }
}

impl From<io::Error> for RudpError {
    fn from(e: io::Error) -> Self {
        RudpError::Io(e.to_string())
    }
}

/// The complete exchange of 09b 3.3: INIT, COOKIE, REQUEST, FINAL. Returns the answer's bytes (HTTP-like text).
/// `seq0` and `tag` are random per attempt (the original: seq in 511..24575, tag in 32768..i32::MAX).
pub fn register(
    w: &mut dyn Wire,
    request_bytes: &[u8],
    seq0: u16,
    tag: u32,
    step: Duration,
) -> Result<Vec<u8>, RudpError> {
    register_until(w, request_bytes, seq0, tag, step, &Limits::default())
}

/// [`register`] that stops with `Cancelled` / `TimedOut` as soon as the limits say so (checked after every datagram step).
pub fn register_until(
    w: &mut dyn Wire,
    request_bytes: &[u8],
    seq0: u16,
    tag: u32,
    step: Duration,
    lim: &Limits,
) -> Result<Vec<u8>, RudpError> {
    let r = exchange(w, &init(seq0, tag), is_init_reply, step, lim)?.ok_or(RudpError::NoInitReply)?;
    let c = parse_first_chunk(&r).ok_or(RudpError::Malformed("short init reply"))?;
    if c.data.len() <= 8 {
        return Err(RudpError::Malformed("init reply has no cookie"));
    }
    let cookie_bytes = c.data[8..].to_vec();

    let r =
        exchange(w, &cookie(seq0, tag, &cookie_bytes), is_cookie_reply, step, lim)?.ok_or(RudpError::NoCookieReply)?;
    let c = parse_first_chunk(&r).ok_or(RudpError::Malformed("short cookie reply"))?;
    if c.data.len() < 2 {
        return Err(RudpError::Malformed("cookie reply too small"));
    }
    let n6 = u16::from_be_bytes([c.data[0], c.data[1]]).wrapping_add(1);

    let seq = seq0.wrapping_add(1);
    let r = exchange(w, &request(seq, n6, request_bytes), is_data_reply, step, lim)?.ok_or(RudpError::NoAnswer)?;
    let c = parse_first_chunk(&r).ok_or(RudpError::Malformed("short answer"))?;
    if c.data.len() < 2 {
        return Err(RudpError::Malformed("answer too small"));
    }
    let n7 = u16::from_be_bytes([c.data[0], c.data[1]]).wrapping_add(1);
    let payload = c.data[2..].to_vec();

    // acknowledge; the original waits for a 0xC0 reply but goes on without one, so we send once and wait for one step at most
    let seq = seq.wrapping_add(1);
    if w.send(&fin(seq, n7)).is_ok() {
        let _ = w.recv(step);
    }
    Ok(payload)
}

/// A UDP socket as a [`Wire`] toward one peer.
pub struct UdpWire {
    pub socket: std::net::UdpSocket,
    pub peer: std::net::SocketAddr,
}

impl Wire for UdpWire {
    fn send(&mut self, datagram: &[u8]) -> io::Result<()> {
        self.socket.send_to(datagram, self.peer).map(|_| ())
    }
    fn recv(&mut self, timeout: Duration) -> io::Result<Option<Vec<u8>>> {
        // datagrams from anyone but the peer are dropped, and do not extend the wait
        let end = Instant::now() + timeout;
        let mut buf = vec![0u8; 32767];
        let mut first = true;
        loop {
            let left = if first { timeout } else { end.saturating_duration_since(Instant::now()) };
            if !first && left.is_zero() {
                return Ok(None);
            }
            first = false;
            self.socket.set_read_timeout(Some(left))?;
            match self.socket.recv_from(&mut buf) {
                Ok((n, from)) if from == self.peer => {
                    buf.truncate(n);
                    return Ok(Some(buf));
                }
                Ok(_) => {}
                Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => return Ok(None),
                Err(e) => return Err(e),
            }
        }
    }
}
