//! Just enough HTTP/1.1 for the console's session endpoints: build a request, parse a response incrementally,
//! and run one exchange over any `Read + Write` stream. The ctrl endpoint keeps the connection open and starts
//! sending binary frames right after the headers, so the exchange hands back any bytes read past the response.

use std::io::{self, Read, Write};
use std::time::Instant;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    pub fn new(method: &str, path: &str) -> Self {
        Self { method: method.into(), path: path.into(), ..Default::default() }
    }

    pub fn header(mut self, name: &str, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    pub fn body(mut self, body: Vec<u8>) -> Self {
        self.body = body;
        self
    }

    pub fn get_header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    /// Serialise. CR, LF and NUL are stripped from every spliced string so a hostile host/value cannot inject
    /// header lines.
    pub fn to_bytes(&self) -> Vec<u8> {
        let clean = |s: &str| -> String { s.chars().filter(|c| !matches!(c, '\r' | '\n' | '\0')).collect() };
        let mut out = format!("{} {} HTTP/1.1\r\n", clean(&self.method), clean(&self.path)).into_bytes();
        for (k, v) in &self.headers {
            out.extend(format!("{}: {}\r\n", clean(k), clean(v)).bytes());
        }
        if !self.body.is_empty() && self.get_header("Content-Length").is_none() {
            out.extend(format!("Content-Length: {}\r\n", self.body.len()).bytes());
        }
        out.extend(b"\r\n");
        out.extend(&self.body);
        out
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub reason: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// What to do when a response has no `Content-Length`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoLength {
    /// Read until the peer closes (one-shot `Connection: close` requests).
    UntilClose,
    /// The body is empty and whatever follows is something else (the ctrl channel's binary frames).
    Empty,
}

#[derive(Debug, PartialEq, Eq)]
pub enum HttpError {
    BadStatusLine,
    HeadersTooLarge,
    BodyTooLarge,
}

const MAX_HEAD: usize = 16 * 1024;
const MAX_BODY: usize = 1024 * 1024;

/// Try to parse a complete response from `buf`. `Ok(None)` = need more bytes. On success returns the response
/// and how many bytes of `buf` it used. With [`NoLength::UntilClose`] and no length, completion needs `eof`.
pub fn parse_response(buf: &[u8], no_length: NoLength, eof: bool) -> Result<Option<(Response, usize)>, HttpError> {
    let Some(head_end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
        return if buf.len() > MAX_HEAD { Err(HttpError::HeadersTooLarge) } else { Ok(None) };
    };
    let head = String::from_utf8_lossy(&buf[..head_end]);
    let mut lines = head.split("\r\n");
    let status_line = lines.next().ok_or(HttpError::BadStatusLine)?;
    let mut parts = status_line.splitn(3, ' ');
    if !parts.next().is_some_and(|v| v.starts_with("HTTP/")) {
        return Err(HttpError::BadStatusLine);
    }
    let status: u16 = parts.next().and_then(|c| c.parse().ok()).ok_or(HttpError::BadStatusLine)?;
    let reason = parts.next().unwrap_or("").to_string();
    let headers: Vec<(String, String)> =
        lines.filter_map(|l| l.split_once(':').map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))).collect();
    let mut resp = Response { status, reason, headers, body: vec![] };
    let body_start = head_end + 4;
    let avail = &buf[body_start..];
    match resp.header("Content-Length").and_then(|v| v.parse::<usize>().ok()) {
        Some(n) if n > MAX_BODY => Err(HttpError::BodyTooLarge),
        Some(n) if avail.len() >= n => {
            resp.body = avail[..n].to_vec();
            Ok(Some((resp, body_start + n)))
        }
        Some(_) => Ok(None),
        None => match no_length {
            NoLength::Empty => Ok(Some((resp, body_start))),
            NoLength::UntilClose if eof => {
                resp.body = avail.to_vec();
                Ok(Some((resp, buf.len())))
            }
            NoLength::UntilClose if avail.len() > MAX_BODY => Err(HttpError::BodyTooLarge),
            NoLength::UntilClose => Ok(None),
        },
    }
}

/// Send `req` and read one response. Returns it plus any bytes received after it.
pub fn exchange<S: Read + Write>(
    stream: &mut S,
    req: &Request,
    no_length: NoLength,
) -> io::Result<(Response, Vec<u8>)> {
    exchange_until(stream, req, no_length, None, |_, _| Ok(()))
}

/// Like [`exchange`] with a total `deadline`: before every read `set_read_timeout` is told how long is left, and
/// the call fails with `TimedOut` once the deadline has passed (a peer dripping bytes cannot hold it forever).
pub fn exchange_until<S: Read + Write>(
    stream: &mut S,
    req: &Request,
    no_length: NoLength,
    deadline: Option<Instant>,
    set_read_timeout: impl Fn(&mut S, std::time::Duration) -> io::Result<()>,
) -> io::Result<(Response, Vec<u8>)> {
    stream.write_all(&req.to_bytes())?;
    stream.flush()?;
    let mut buf = vec![];
    let mut chunk = [0u8; 4096];
    loop {
        if let Some((resp, used)) = parse_response(&buf, no_length, false)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("{e:?}")))?
        {
            return Ok((resp, buf[used..].to_vec()));
        }
        if let Some(d) = deadline {
            let left = d.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "console HTTP exchange exceeded its total deadline",
                ));
            }
            set_read_timeout(stream, left)?;
        }
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            // peer closed: a body without a length ends here
            return match parse_response(&buf, no_length, true)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("{e:?}")))?
            {
                Some((resp, used)) => Ok((resp, buf[used..].to_vec())),
                None => Err(io::Error::new(io::ErrorKind::UnexpectedEof, "connection closed mid-response")),
            };
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_serialisation() {
        let r =
            Request::new("POST", "/x").header("Host", "1.2.3.4:9295").header("Connection", "close").body(vec![1, 2, 3]);
        assert_eq!(
            r.to_bytes(),
            b"POST /x HTTP/1.1\r\nHost: 1.2.3.4:9295\r\nConnection: close\r\nContent-Length: 3\r\n\r\n\x01\x02\x03"
                .to_vec()
        );
        let g = Request::new("GET", "/y").header("Content-Length", "0");
        assert_eq!(g.to_bytes(), b"GET /y HTTP/1.1\r\nContent-Length: 0\r\n\r\n".to_vec());
    }

    #[test]
    fn incremental_parsing_with_content_length() {
        let full = b"HTTP/1.1 200 OK\r\nRP-Nonce: abc\r\ncontent-length: 4\r\n\r\nbodyEXTRA";
        for cut in 0..full.len() - 5 {
            assert_eq!(parse_response(&full[..cut], NoLength::Empty, false).unwrap(), None, "cut {cut}");
        }
        let (r, used) = parse_response(full, NoLength::Empty, false).unwrap().unwrap();
        assert_eq!(
            (r.status, r.header("rp-nonce"), r.body.as_slice(), used),
            (200, Some("abc"), &b"body"[..], full.len() - 5)
        );
        assert!(r.ok());
    }

    #[test]
    fn missing_length_policies() {
        let head = b"HTTP/1.1 200 OK\r\nRP-Server-Type: x\r\n\r\n\x00\x00\x00\x00\x01\xfe\x00\x00";
        let (r, used) = parse_response(head, NoLength::Empty, false).unwrap().unwrap();
        assert!(r.body.is_empty());
        assert_eq!(&head[used..], b"\x00\x00\x00\x00\x01\xfe\x00\x00"); // ctrl frames are left for the caller
        assert_eq!(parse_response(head, NoLength::UntilClose, false).unwrap(), None);
        let (r, _) = parse_response(head, NoLength::UntilClose, true).unwrap().unwrap();
        assert_eq!(r.body.len(), 8);
    }

    #[test]
    fn malformed_and_oversized_input() {
        assert_eq!(parse_response(b"FOO 200 OK\r\n\r\n", NoLength::Empty, false), Err(HttpError::BadStatusLine));
        assert_eq!(parse_response(b"HTTP/1.1 abc OK\r\n\r\n", NoLength::Empty, false), Err(HttpError::BadStatusLine));
        assert_eq!(parse_response(&vec![b'a'; MAX_HEAD + 1], NoLength::Empty, false), Err(HttpError::HeadersTooLarge));
        assert_eq!(
            parse_response(b"HTTP/1.1 200 OK\r\nContent-Length: 99999999\r\n\r\n", NoLength::Empty, false),
            Err(HttpError::BodyTooLarge)
        );
    }

    #[test]
    fn exchange_over_a_stream_returns_leftover_bytes() {
        struct Duplex {
            input: std::io::Cursor<Vec<u8>>,
            written: Vec<u8>,
        }
        impl Read for Duplex {
            fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
                self.input.read(b)
            }
        }
        impl Write for Duplex {
            fn write(&mut self, b: &[u8]) -> io::Result<usize> {
                self.written.extend_from_slice(b);
                Ok(b.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut d = Duplex {
            input: std::io::Cursor::new(b"HTTP/1.1 200 OK\r\nA: b\r\n\r\n\x01\x02".to_vec()),
            written: vec![],
        };
        let (r, rest) = exchange(&mut d, &Request::new("GET", "/"), NoLength::Empty).unwrap();
        assert_eq!((r.header("a"), rest), (Some("b"), vec![1, 2]));
        assert!(d.written.starts_with(b"GET / HTTP/1.1"));
        // closed before the headers finish
        let mut d = Duplex { input: std::io::Cursor::new(b"HTTP/1.1 200".to_vec()), written: vec![] };
        assert_eq!(
            exchange(&mut d, &Request::new("GET", "/"), NoLength::Empty).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
    }
}
