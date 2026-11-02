//! Packet transport abstraction. The runtime only needs "send a datagram" and "receive one with a timeout",
//! so production uses UDP and tests use an in-memory pair.

use socket2::{Domain, Protocol, Socket, Type};
use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::time::Duration;

pub trait PacketSink: Send {
    fn send(&mut self, data: &[u8]) -> io::Result<()>;
}

pub trait PacketSource: Send {
    /// Next datagram, or `Ok(None)` if nothing arrived within `timeout`. `Err` is fatal for the link.
    fn recv(&mut self, timeout: Duration) -> io::Result<Option<Vec<u8>>>;
}

pub struct Link {
    pub sink: Box<dyn PacketSink>,
    pub source: Box<dyn PacketSource>,
}

// ---- UDP ---------------------------------------------------------------------------------------------------

/// Receive buffer the original app requests (`SO_RCVBUF`): video bursts must not be dropped by the kernel.
pub const RCVBUF_BYTES: usize = 524_288;
const MAX_DATAGRAM: usize = 65_535;

struct UdpSink(UdpSocket, u64);
struct UdpSource {
    socket: UdpSocket,
    buf: Vec<u8>,
}

/// Errors on a connected UDP socket that say "this datagram did not get through right now" rather than
/// "the link is broken": ICMP port-unreachable (`ECONNREFUSED`), `EINTR`, `ENOBUFS`, and host/network
/// unreachable (Wi-Fi roam). The idle timeout in the runner ends a session that is really dead.
fn is_transient(e: &io::Error) -> bool {
    if matches!(
        e.kind(),
        io::ErrorKind::WouldBlock
            | io::ErrorKind::TimedOut
            | io::ErrorKind::ConnectionRefused
            | io::ErrorKind::Interrupted
    ) {
        return true;
    }
    // (ENETUNREACH, EHOSTUNREACH, ENOBUFS) per platform
    #[cfg(target_os = "macos")]
    const TRANSIENT: [i32; 3] = [51, 65, 55];
    #[cfg(not(target_os = "macos"))]
    const TRANSIENT: [i32; 3] = [101, 113, 105];
    e.raw_os_error().is_some_and(|c| TRANSIENT.contains(&c))
}

impl PacketSink for UdpSink {
    fn send(&mut self, data: &[u8]) -> io::Result<()> {
        match self.0.send(data) {
            Ok(_) => Ok(()),
            // UDP is lossy by contract: the protocol layers retransmit what matters. It is still worth knowing when a datagram never
            // left (a lost button release looks exactly like a delivered one otherwise), so say so, without flooding.
            Err(e) if is_transient(&e) => {
                self.1 += 1;
                if self.1 <= 3 || self.1.is_multiple_of(100) {
                    log::warn!("udp datagram not sent ({e}), {} dropped so far", self.1);
                }
                Ok(())
            }
            Err(e) => Err(e),
        }
    }
}

impl PacketSource for UdpSource {
    fn recv(&mut self, timeout: Duration) -> io::Result<Option<Vec<u8>>> {
        self.socket.set_read_timeout(Some(timeout.max(Duration::from_millis(1))))?;
        match self.socket.recv(&mut self.buf) {
            Ok(n) => Ok(Some(self.buf[..n].to_vec())),
            // Includes ICMP "port unreachable" from an earlier send, which surfaces here on a connected UDP
            // socket; the console may simply not be listening yet.
            Err(e) if is_transient(&e) => Ok(None),
            Err(e) => Err(e),
        }
    }
}

/// A UDP socket connected to the console, with `SO_REUSEADDR` and a large receive buffer.
pub fn udp_link(remote: SocketAddr) -> io::Result<Link> {
    let local: SocketAddr = if remote.is_ipv4() { "0.0.0.0:0".parse().unwrap() } else { "[::]:0".parse().unwrap() };
    let socket = Socket::new(Domain::for_address(remote), Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    socket.set_recv_buffer_size(RCVBUF_BYTES)?;
    socket.bind(&local.into())?;
    socket.connect(&remote.into())?;
    let socket: UdpSocket = socket.into();
    let source = socket.try_clone()?;
    Ok(Link {
        sink: Box::new(UdpSink(socket, 0)),
        source: Box::new(UdpSource { socket: source, buf: vec![0; MAX_DATAGRAM] }),
    })
}

// ---- in-memory ---------------------------------------------------------------------------------------------

struct MemSink(Sender<Vec<u8>>);
struct MemSource(Receiver<Vec<u8>>);

impl PacketSink for MemSink {
    fn send(&mut self, data: &[u8]) -> io::Result<()> {
        self.0.send(data.to_vec()).map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "peer closed"))
    }
}

impl PacketSource for MemSource {
    fn recv(&mut self, timeout: Duration) -> io::Result<Option<Vec<u8>>> {
        match self.0.recv_timeout(timeout) {
            Ok(d) => Ok(Some(d)),
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => Err(io::Error::new(io::ErrorKind::BrokenPipe, "peer closed")),
        }
    }
}

/// A connected pair of in-memory links: what one side sends, the other receives. Used by tests and replay tools.
pub fn mem_pair() -> (Link, Link) {
    let (a_tx, a_rx) = channel();
    let (b_tx, b_rx) = channel();
    (
        Link { sink: Box::new(MemSink(a_tx)), source: Box::new(MemSource(b_rx)) },
        Link { sink: Box::new(MemSink(b_tx)), source: Box::new(MemSource(a_rx)) },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mem_pair_is_duplex_and_reports_a_closed_peer() {
        let (mut a, mut b) = mem_pair();
        a.sink.send(&[1, 2, 3]).unwrap();
        b.sink.send(&[9]).unwrap();
        assert_eq!(b.source.recv(Duration::from_millis(50)).unwrap(), Some(vec![1, 2, 3]));
        assert_eq!(a.source.recv(Duration::from_millis(50)).unwrap(), Some(vec![9]));
        assert_eq!(a.source.recv(Duration::from_millis(5)).unwrap(), None); // timeout
        drop(b);
        assert!(a.sink.send(&[1]).is_err());
        assert!(a.source.recv(Duration::from_millis(5)).is_err());
    }

    #[test]
    fn udp_link_round_trips_over_loopback() {
        let server = UdpSocket::bind("127.0.0.1:0").unwrap();
        server.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let mut link = udp_link(server.local_addr().unwrap()).unwrap();
        link.sink.send(b"hello").unwrap();
        let mut buf = [0u8; 64];
        let (n, from) = server.recv_from(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"hello");
        server.send_to(b"world", from).unwrap();
        assert_eq!(link.source.recv(Duration::from_secs(2)).unwrap(), Some(b"world".to_vec()));
        assert_eq!(link.source.recv(Duration::from_millis(10)).unwrap(), None);
    }
}
