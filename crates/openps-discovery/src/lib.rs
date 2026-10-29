//! Finding and waking consoles on the local network.
//!
//! * Discovery: UDP `SRCH` to port 987 (PS4) / 9302 (PS5); consoles answer with an HTTP-like status line and
//!   `key:value` lines (`host-type`, `host-id`, `host-name`). Status 200 means awake; 620 means standby (the
//!   standby code is from the public protocol; the original app only branches on 200).
//! * Wake: UDP `WAKEUP` carrying the registration key as a decimal `user-credential`.
//! * Registration probe: `SRC2\0` / `SRC3\0` to port 9295 (only the sender address matters).
//!
//! Everything that builds or parses bytes is pure; the socket helpers take explicit ports and timings so tests
//! can run against a fake console on loopback.

use socket2::{Domain, Protocol, Socket, Type};
use std::collections::BTreeMap;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

pub const PS4_PORT: u16 = 987;
pub const PS5_PORT: u16 = 9302;
pub const REGISTRATION_PORT: u16 = 9295;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum DeviceType {
    Ps4,
    Ps5,
}

impl DeviceType {
    pub fn port(self) -> u16 {
        match self {
            DeviceType::Ps4 => PS4_PORT,
            DeviceType::Ps5 => PS5_PORT,
        }
    }

    pub fn protocol_version(self) -> &'static str {
        match self {
            DeviceType::Ps4 => "00020020",
            DeviceType::Ps5 => "00030010",
        }
    }

    /// Value of the `host-type` field.
    pub fn from_host_type(s: &str) -> Option<DeviceType> {
        match s.trim().to_ascii_uppercase().as_str() {
            "PS4" => Some(DeviceType::Ps4),
            "PS5" => Some(DeviceType::Ps5),
            _ => None,
        }
    }

    /// Time to wait for a console to come out of standby (PS5 25 s, PS4 40 s).
    pub fn wake_timeout(self) -> Duration {
        Duration::from_secs(if self == DeviceType::Ps5 { 25 } else { 40 })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Ready,
    Standby,
    Other(u16),
}

impl Status {
    pub fn from_code(c: u16) -> Status {
        match c {
            200 => Status::Ready,
            620 => Status::Standby,
            c => Status::Other(c),
        }
    }
}

// ---- packets ------------------------------------------------------------------------------------------------

/// `SRCH * HTTP/1.1\r\ndevice-discovery-protocol-version:<v>\r\n\r\n`
pub fn search_packet(dt: DeviceType) -> Vec<u8> {
    format!("SRCH * HTTP/1.1\r\ndevice-discovery-protocol-version:{}\r\n\r\n", dt.protocol_version()).into_bytes()
}

#[derive(Debug, PartialEq, Eq)]
pub enum WakeError {
    /// The registration key is not hex, or does not decode to a hex number that fits a signed 64-bit integer.
    BadRegistKey,
}

/// `user-credential` value: the registration key (hex of ASCII hex digits) parsed as a hex integer, printed in
/// decimal (`Long.parseLong(ASCII(unhex(key)), 16)`).
pub fn user_credential(regist_key_hex: &str) -> Result<i64, WakeError> {
    let bytes = hex_to_bytes(regist_key_hex).ok_or(WakeError::BadRegistKey)?;
    let text = String::from_utf8(bytes).map_err(|_| WakeError::BadRegistKey)?;
    if text.starts_with('+') {
        return Err(WakeError::BadRegistKey); // keep strict: only plain digits, optional leading '-'
    }
    i64::from_str_radix(&text, 16).map_err(|_| WakeError::BadRegistKey)
}

/// The wake-up datagram (LF line endings, as the app sends them).
pub fn wake_packet(dt: DeviceType, regist_key_hex: &str) -> Result<Vec<u8>, WakeError> {
    let cred = user_credential(regist_key_hex)?;
    Ok(format!(
        "WAKEUP * HTTP/1.1\nclient-type:vr\nauth-type:R\nmodel:w\napp-type:r\nuser-credential:{cred}\ndevice-discovery-protocol-version:{}\n",
        dt.protocol_version()
    )
    .into_bytes())
}

/// `SRC2\0` (PS4) / `SRC3\0` (PS5) to the registration port.
pub fn registration_probe(dt: DeviceType) -> &'static [u8] {
    match dt {
        DeviceType::Ps4 => b"SRC2\0",
        DeviceType::Ps5 => b"SRC3\0",
    }
}

fn hex_to_bytes(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) || !s.is_ascii() {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok()).collect()
}

// ---- responses ----------------------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Response {
    pub code: u16,
    pub reason: String,
    /// Field names are lower-cased.
    pub fields: BTreeMap<String, String>,
}

/// Parse `HTTP/1.1 <code> <reason>` followed by `key:value` lines. Returns None for anything else.
pub fn parse_response(data: &[u8]) -> Option<Response> {
    let text = std::str::from_utf8(data).ok()?;
    let mut lines = text.split("\r\n").flat_map(|l| l.split('\n'));
    let status = lines.next()?;
    let mut parts = status.splitn(3, ' ');
    if !parts.next()?.starts_with("HTTP/") {
        return None;
    }
    let code: u16 = parts.next()?.trim().parse().ok()?;
    let reason = parts.next().unwrap_or("").trim().to_string();
    let mut fields = BTreeMap::new();
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            fields.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    Some(Response { code, reason, fields })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Console {
    pub device: DeviceType,
    pub host_id: String,
    pub host_name: String,
    pub status: Status,
    pub addr: IpAddr,
}

/// Build a [`Console`] from a discovery response. Requires a known `host-type` and a `host-id`.
pub fn console_from_response(resp: &Response, from: IpAddr) -> Option<Console> {
    Some(Console {
        device: DeviceType::from_host_type(resp.fields.get("host-type")?)?,
        host_id: resp.fields.get("host-id")?.clone(),
        host_name: resp.fields.get("host-name").cloned().unwrap_or_default(),
        status: Status::from_code(resp.code),
        addr: from,
    })
}

// ---- sockets ------------------------------------------------------------------------------------------------

fn broadcast_socket() -> io::Result<UdpSocket> {
    let s = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    s.set_reuse_address(true)?;
    s.set_broadcast(true)?;
    s.bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)).into())?;
    Ok(s.into())
}

#[derive(Clone, Debug)]
pub struct ScanOptions {
    /// Where to send searches: `255.255.255.255` for broadcast, or specific console addresses.
    pub targets: Vec<IpAddr>,
    pub devices: Vec<DeviceType>,
    /// How long to collect answers after the last send.
    pub listen: Duration,
    /// Searches per (target, device); the app sends 3 for direct probes.
    pub sends: u32,
    pub send_interval: Duration,
    /// Override the destination port (tests only); `None` uses each device's standard port.
    pub port_override: Option<u16>,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            targets: vec![IpAddr::V4(Ipv4Addr::BROADCAST)],
            devices: vec![DeviceType::Ps4, DeviceType::Ps5],
            listen: Duration::from_secs(2),
            sends: 3,
            send_interval: Duration::from_millis(100),
            port_override: None,
        }
    }
}

/// Send searches and collect distinct consoles (by `host-id`; a later answer from the same console replaces the
/// earlier one so a status change is visible).
pub fn scan(opts: &ScanOptions) -> io::Result<Vec<Console>> {
    let sock = broadcast_socket()?;
    let mut found: BTreeMap<String, Console> = BTreeMap::new();
    let mut buf = [0u8; 2048];
    let mut collect = |sock: &UdpSocket, until: Instant, found: &mut BTreeMap<String, Console>| -> io::Result<()> {
        loop {
            let now = Instant::now();
            if now >= until {
                return Ok(());
            }
            sock.set_read_timeout(Some((until - now).max(Duration::from_millis(1))))?;
            match sock.recv_from(&mut buf) {
                Ok((n, from)) => {
                    if let Some(c) = parse_response(&buf[..n]).and_then(|r| console_from_response(&r, from.ip())) {
                        found.insert(c.host_id.clone(), c);
                    }
                }
                Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => return Ok(()),
                Err(e) if e.kind() == io::ErrorKind::ConnectionRefused => {}
                Err(e) => return Err(e),
            }
        }
    };
    for round in 0..opts.sends.max(1) {
        for dt in &opts.devices {
            for target in &opts.targets {
                let dest = SocketAddr::new(*target, opts.port_override.unwrap_or(dt.port()));
                // an unreachable target must not abort the whole scan
                let _ = sock.send_to(&search_packet(*dt), dest);
            }
        }
        let wait = if round + 1 < opts.sends.max(1) { opts.send_interval } else { opts.listen };
        collect(&sock, Instant::now() + wait, &mut found)?;
    }
    Ok(found.into_values().collect())
}

/// Send the wake-up packet `count` times, `interval` apart. Returns once sent; use [`wait_until_ready`] after.
pub fn wake(
    dt: DeviceType,
    target: IpAddr,
    regist_key_hex: &str,
    count: u32,
    interval: Duration,
    port_override: Option<u16>,
) -> Result<(), io::Error> {
    let packet = wake_packet(dt, regist_key_hex)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid registration key"))?;
    let sock = broadcast_socket()?;
    let dest = SocketAddr::new(target, port_override.unwrap_or(dt.port()));
    for i in 0..count {
        sock.send_to(&packet, dest)?;
        if i + 1 < count {
            std::thread::sleep(interval);
        }
    }
    Ok(())
}

/// Poll a specific console until it reports Ready, up to `timeout`.
pub fn wait_until_ready(
    dt: DeviceType,
    target: IpAddr,
    timeout: Duration,
    port_override: Option<u16>,
) -> io::Result<bool> {
    let end = Instant::now() + timeout;
    while Instant::now() < end {
        let opts = ScanOptions {
            targets: vec![target],
            devices: vec![dt],
            listen: Duration::from_millis(400),
            sends: 1,
            port_override,
            ..Default::default()
        };
        if scan(&opts)?.iter().any(|c| c.status == Status::Ready && c.device == dt) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Wait until `until` for any datagram and return its sender.
fn listen_until(sock: &UdpSocket, until: Instant) -> io::Result<Option<SocketAddr>> {
    let mut buf = [0u8; 2048];
    while let Some(left) = until.checked_duration_since(Instant::now()).filter(|d| !d.is_zero()) {
        sock.set_read_timeout(Some(left.max(Duration::from_millis(1))))?;
        match sock.recv_from(&mut buf) {
            Ok((_, from)) => return Ok(Some(from)),
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => return Ok(None),
            Err(e) if e.kind() == io::ErrorKind::ConnectionRefused => {} // ICMP from an earlier send: keep listening
            Err(e) => return Err(e),
        }
    }
    Ok(None)
}

/// Registration probe: send `SRC2`/`SRC3` up to `sends` times, listening `interval` after each, then until
/// `timeout`. Returns the address of whoever answers (only the sender matters).
pub fn probe_registration(
    dt: DeviceType,
    target: IpAddr,
    sends: u32,
    interval: Duration,
    timeout: Duration,
    port: u16,
) -> io::Result<Option<SocketAddr>> {
    let sock = broadcast_socket()?;
    let dest = SocketAddr::new(target, port);
    let end = Instant::now() + timeout;
    for _ in 0..sends.max(1) {
        let _ = sock.send_to(registration_probe(dt), dest);
        if let Some(from) = listen_until(&sock, (Instant::now() + interval).min(end))? {
            return Ok(Some(from));
        }
    }
    listen_until(&sock, end)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    const LO: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

    #[test]
    fn search_packets_match_the_apps_bytes() {
        assert_eq!(
            search_packet(DeviceType::Ps4),
            b"SRCH * HTTP/1.1\r\ndevice-discovery-protocol-version:00020020\r\n\r\n"
        );
        assert_eq!(
            search_packet(DeviceType::Ps5),
            b"SRCH * HTTP/1.1\r\ndevice-discovery-protocol-version:00030010\r\n\r\n"
        );
        assert_eq!((DeviceType::Ps4.port(), DeviceType::Ps5.port()), (987, 9302));
        assert_eq!(registration_probe(DeviceType::Ps4), b"SRC2\0");
        assert_eq!(registration_probe(DeviceType::Ps5), b"SRC3\0");
    }

    #[test]
    fn user_credential_is_the_decimal_of_the_hex_digits() {
        // "0000000a" as ASCII hex digits -> 0x0000000a = 10
        let key: String = "0000000a".bytes().map(|b| format!("{b:02x}")).collect();
        assert_eq!(user_credential(&key), Ok(10));
        let key: String = "ffffffff".bytes().map(|b| format!("{b:02x}")).collect();
        assert_eq!(user_credential(&key), Ok(4_294_967_295));
        assert_eq!(user_credential("zz"), Err(WakeError::BadRegistKey));
        assert_eq!(user_credential("3"), Err(WakeError::BadRegistKey)); // odd length
        let not_hex: String = "hello!".bytes().map(|b| format!("{b:02x}")).collect();
        assert_eq!(user_credential(&not_hex), Err(WakeError::BadRegistKey));
        let too_big: String = "ffffffffffffffff".bytes().map(|b| format!("{b:02x}")).collect(); // overflows i64 like Long.parseLong
        assert_eq!(user_credential(&too_big), Err(WakeError::BadRegistKey));
    }

    #[test]
    fn wake_packet_layout() {
        let key: String = "00000064".bytes().map(|b| format!("{b:02x}")).collect();
        let p = String::from_utf8(wake_packet(DeviceType::Ps5, &key).unwrap()).unwrap();
        assert_eq!(
            p,
            "WAKEUP * HTTP/1.1\nclient-type:vr\nauth-type:R\nmodel:w\napp-type:r\nuser-credential:100\ndevice-discovery-protocol-version:00030010\n"
        );
        assert!(wake_packet(DeviceType::Ps4, "xx").is_err());
    }

    #[test]
    fn response_parsing() {
        let r = parse_response(b"HTTP/1.1 200 Ok\r\nhost-id:AABBCCDDEEFF\r\nhost-type:PS5\r\nhost-name:Living Room\r\nhost-request-port:997\r\n\r\n").unwrap();
        assert_eq!((r.code, r.reason.as_str()), (200, "Ok"));
        let c = console_from_response(&r, LO).unwrap();
        assert_eq!(
            (c.device, c.host_id.as_str(), c.host_name.as_str(), c.status),
            (DeviceType::Ps5, "AABBCCDDEEFF", "Living Room", Status::Ready)
        );
        let standby = parse_response(b"HTTP/1.1 620 Server Standby\nhost-id:1\nhost-type:ps4\nhost-name:x\n").unwrap();
        assert_eq!(console_from_response(&standby, LO).unwrap().status, Status::Standby);
        assert_eq!(Status::from_code(503), Status::Other(503));
        // missing mandatory fields / not HTTP / garbage
        assert!(console_from_response(&parse_response(b"HTTP/1.1 200 Ok\r\nhost-type:PS5\r\n").unwrap(), LO).is_none());
        assert!(console_from_response(
            &parse_response(b"HTTP/1.1 200 Ok\r\nhost-id:1\r\nhost-type:XBOX\r\n").unwrap(),
            LO
        )
        .is_none());
        assert!(parse_response(b"SRCH * HTTP/1.1\r\n").is_none());
        assert!(parse_response(&[0xFF, 0xFE]).is_none());
        assert!(parse_response(b"HTTP/1.1 abc\r\n").is_none());
    }

    /// A fake console: answers SRCH/WAKEUP on a loopback UDP socket.
    struct Fake {
        port: u16,
        stop: Arc<AtomicBool>,
        handle: Option<std::thread::JoinHandle<Vec<String>>>,
    }

    impl Fake {
        fn start(device: &'static str, id: &'static str, standby_until_wake: bool) -> Fake {
            let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
            sock.set_read_timeout(Some(Duration::from_millis(20))).unwrap();
            let port = sock.local_addr().unwrap().port();
            let stop = Arc::new(AtomicBool::new(false));
            let s2 = stop.clone();
            let handle = std::thread::spawn(move || {
                let mut seen = vec![];
                let mut awake = !standby_until_wake;
                let mut buf = [0u8; 2048];
                while !s2.load(Ordering::Relaxed) {
                    if let Ok((n, from)) = sock.recv_from(&mut buf) {
                        let text = String::from_utf8_lossy(&buf[..n]).to_string();
                        if text.starts_with("SRCH") {
                            let (code, reason) = if awake { (200, "Ok") } else { (620, "Server Standby") };
                            let r = format!("HTTP/1.1 {code} {reason}\r\nhost-id:{id}\r\nhost-type:{device}\r\nhost-name:Fake {id}\r\n\r\n");
                            sock.send_to(r.as_bytes(), from).unwrap();
                        } else if text.starts_with("WAKEUP") {
                            awake = true;
                        } else if text.starts_with("SRC") {
                            sock.send_to(b"RESP", from).unwrap();
                        }
                        seen.push(text);
                    }
                }
                seen
            });
            Fake { port, stop, handle: Some(handle) }
        }
        fn finish(mut self) -> Vec<String> {
            self.stop.store(true, Ordering::Relaxed);
            self.handle.take().unwrap().join().unwrap()
        }
    }

    #[test]
    fn scan_finds_consoles_and_deduplicates_repeated_answers() {
        let fake = Fake::start("PS5", "ID1", true); // standby, never woken
        let opts = ScanOptions {
            targets: vec![LO],
            devices: vec![DeviceType::Ps5],
            listen: Duration::from_millis(200),
            sends: 3,
            send_interval: Duration::from_millis(30),
            port_override: Some(fake.port),
        };
        let found = scan(&opts).unwrap();
        let seen = fake.finish();
        assert_eq!(found.len(), 1); // three searches, one console
        assert_eq!(
            (found[0].host_id.as_str(), found[0].status, found[0].device),
            ("ID1", Status::Standby, DeviceType::Ps5)
        );
        assert_eq!(seen.len(), 3);
        assert!(seen.iter().all(|s| s == "SRCH * HTTP/1.1\r\ndevice-discovery-protocol-version:00030010\r\n\r\n"));
    }

    #[test]
    fn scan_with_no_console_returns_empty_without_error() {
        let silent = UdpSocket::bind("127.0.0.1:0").unwrap();
        let opts = ScanOptions {
            targets: vec![LO],
            devices: vec![DeviceType::Ps4],
            listen: Duration::from_millis(80),
            sends: 1,
            port_override: Some(silent.local_addr().unwrap().port()),
            ..Default::default()
        };
        assert!(scan(&opts).unwrap().is_empty());
    }

    #[test]
    fn wake_then_wait_until_ready() {
        let fake = Fake::start("PS5", "ID2", true); // standby until it hears WAKEUP
        let key: String = "0000002a".bytes().map(|b| format!("{b:02x}")).collect();
        assert!(!wait_until_ready(DeviceType::Ps5, LO, Duration::from_millis(300), Some(fake.port)).unwrap());
        wake(DeviceType::Ps5, LO, &key, 3, Duration::from_millis(10), Some(fake.port)).unwrap();
        assert!(wait_until_ready(DeviceType::Ps5, LO, Duration::from_secs(3), Some(fake.port)).unwrap());
        let seen = fake.finish();
        let wakes: Vec<&String> = seen.iter().filter(|s| s.starts_with("WAKEUP")).collect();
        assert_eq!(wakes.len(), 3);
        assert!(wakes[0].contains("user-credential:42\n")); // 0x2a
    }

    #[test]
    fn wake_rejects_a_bad_registration_key_before_sending() {
        let e = wake(DeviceType::Ps4, LO, "nothex", 1, Duration::ZERO, Some(9)).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn registration_probe_reports_the_responder() {
        let fake = Fake::start("PS4", "ID3", false);
        let r =
            probe_registration(DeviceType::Ps4, LO, 2, Duration::from_millis(50), Duration::from_secs(1), fake.port)
                .unwrap();
        assert_eq!(r.map(|a| a.ip()), Some(LO));
        let seen = fake.finish();
        assert_eq!(seen[0], "SRC2\0");
        let nobody = UdpSocket::bind("127.0.0.1:0").unwrap();
        let none = probe_registration(
            DeviceType::Ps5,
            LO,
            1,
            Duration::from_millis(20),
            Duration::from_millis(60),
            nobody.local_addr().unwrap().port(),
        )
        .unwrap();
        assert_eq!(none, None);
    }
}
