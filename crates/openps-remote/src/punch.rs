//! The 88-byte hole-punch packets (09b section 3.1). Both sides send REQUESTs to each other's candidate addresses; whoever receives
//! one answers with an ACK that tells the requester what address and port the packet came from (XOR-masked), so the requester
//! learns its own mapped endpoint and that the path works.

use std::net::Ipv4Addr;

pub const SIZE: usize = 88;
pub const T_REQUEST: u32 = 0x0600_0000;
pub const T_ACK: u32 = 0x0700_0000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Packet {
    pub ty: u32,
    /// The sender's 20-byte id (the raw bytes of its base64 `localHashedId`).
    pub own_id: [u8; 20],
    /// The other side's id.
    pub peer_id: [u8; 20],
    pub own_sid: u16,
    pub peer_sid: u16,
    pub nonce: u64,
    /// ACK only: the requester's address and port as seen by the acker, XOR-masked.
    pub mapped_ip: u32,
    pub mapped_port: u16,
}

impl Packet {
    pub fn encode(&self) -> [u8; SIZE] {
        let mut b = [0u8; SIZE];
        b[0..4].copy_from_slice(&self.ty.to_be_bytes());
        b[4..24].copy_from_slice(&self.own_id);
        b[36..56].copy_from_slice(&self.peer_id);
        b[68..70].copy_from_slice(&self.own_sid.to_be_bytes());
        b[70..72].copy_from_slice(&self.peer_sid.to_be_bytes());
        b[72..80].copy_from_slice(&self.nonce.to_be_bytes());
        b[80..84].copy_from_slice(&self.mapped_ip.to_be_bytes());
        b[84..86].copy_from_slice(&self.mapped_port.to_be_bytes());
        b
    }

    /// Anything shorter than 88 bytes is not one of ours; the original does not check ids or nonce of what it receives.
    pub fn parse(b: &[u8]) -> Option<Self> {
        if b.len() < SIZE {
            return None;
        }
        Some(Packet {
            ty: u32::from_be_bytes(b[0..4].try_into().ok()?),
            own_id: b[4..24].try_into().ok()?,
            peer_id: b[36..56].try_into().ok()?,
            own_sid: u16::from_be_bytes([b[68], b[69]]),
            peer_sid: u16::from_be_bytes([b[70], b[71]]),
            nonce: u64::from_be_bytes(b[72..80].try_into().ok()?),
            mapped_ip: u32::from_be_bytes(b[80..84].try_into().ok()?),
            mapped_port: u16::from_be_bytes([b[84], b[85]]),
        })
    }
}

/// Our REQUEST to a console candidate.
pub fn request(own_id: [u8; 20], peer_id: [u8; 20], own_sid: u16, peer_sid: u16, nonce: u64) -> Packet {
    Packet { ty: T_REQUEST, own_id, peer_id, own_sid, peer_sid, nonce, mapped_ip: 0, mapped_port: 0 }
}

/// Our ACK to a REQUEST that came from `from`: tells the requester where it appeared to come from.
pub fn ack_for(req: &Packet, own_id: [u8; 20], own_sid: u16, peer_sid: u16, from: (Ipv4Addr, u16)) -> Packet {
    Packet {
        ty: T_ACK,
        own_id,
        peer_id: req.own_id,
        own_sid,
        peer_sid,
        nonce: req.nonce,
        mapped_ip: ((u32::from(own_sid) << 16) | u32::from(peer_sid)) ^ u32::from(from.0),
        mapped_port: own_sid ^ from.1,
    }
}

/// From the ACK we received: our own address and port as the console saw them. `own_sid` is ours, `peer_sid` the console's.
pub fn mapped_endpoint(ack: &Packet, own_sid: u16, peer_sid: u16) -> (Ipv4Addr, u16) {
    let ip = ack.mapped_ip ^ ((u32::from(peer_sid) << 16) | u32::from(own_sid));
    (Ipv4Addr::from(ip), ack.mapped_port ^ peer_sid)
}

// ---- the driver -----------------------------------------------------------------------------------------------------------------------

use crate::signaling::{Candidate, PunchResult};
use std::net::{SocketAddr, SocketAddrV4, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Who we are in the exchange.
#[derive(Clone, Debug)]
pub struct Ids {
    pub own_id: [u8; 20],
    pub peer_id: [u8; 20],
    pub own_sid: u16,
    pub peer_sid: u16,
}

/// Candidate addresses come from the network: only plain unicast IPv4 with a real port is ever probed (never loopback, unspecified,
/// broadcast, multicast or link-local), and no more than 16 of them.
pub fn probe_targets(cands: &[Candidate], allow_loopback: bool) -> Vec<SocketAddrV4> {
    let ok = |ip: &Ipv4Addr, port: i64| {
        (1..=65535).contains(&port)
            && !((ip.is_loopback() && !allow_loopback)
                || ip.is_unspecified()
                || ip.is_broadcast()
                || ip.is_multicast()
                || ip.is_link_local())
    };
    let mut out: Vec<SocketAddrV4> = vec![];
    for c in cands {
        let mut add = |addr: &str, port: i64| {
            if let Ok(ip) = addr.trim().parse::<Ipv4Addr>() {
                if ok(&ip, port) {
                    let a = SocketAddrV4::new(ip, port as u16);
                    if !out.contains(&a) {
                        out.push(a);
                    }
                }
            }
        };
        add(&c.addr, c.port);
        if c.mapped_addr != "0.0.0.0" || c.mapped_port != 0 {
            add(&c.mapped_addr, c.mapped_port);
        }
    }
    out.truncate(16);
    out
}

/// Try to reach the console: send REQUESTs to its candidates (5 times, 5 ms apart, up to 3 rounds), answer any REQUEST it sends us
/// with an ACK, and take the first valid ACK as success (09b 3.1; it must echo our nonce and come from a probed host). `socket` is the SAME socket the register exchange will use.
pub fn hole_punch(
    socket: &UdpSocket,
    cands: &[Candidate],
    ids: &Ids,
    nonce: u64,
    cancel: &AtomicBool,
    round_wait: Duration,
    allow_loopback: bool,
) -> Option<PunchResult> {
    punch_targets(socket, &probe_targets(cands, allow_loopback), cands, ids, nonce, cancel, round_wait)
}

/// The exchange itself, over targets that were already vetted.
pub(crate) fn punch_targets(
    socket: &UdpSocket,
    targets: &[SocketAddrV4],
    cands: &[Candidate],
    ids: &Ids,
    nonce: u64,
    cancel: &AtomicBool,
    round_wait: Duration,
) -> Option<PunchResult> {
    if targets.is_empty() {
        return None;
    }
    let req = request(ids.own_id, ids.peer_id, ids.own_sid, ids.peer_sid, nonce).encode();
    let _ = socket.set_read_timeout(Some(Duration::from_millis(20)));
    let mut buf = [0u8; 2048];
    for _round in 0..3 {
        for _ in 0..5 {
            for t in targets {
                let _ = socket.send_to(&req, SocketAddr::V4(*t));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let until = Instant::now() + round_wait;
        while Instant::now() < until {
            if cancel.load(Ordering::Relaxed) {
                return None;
            }
            let Ok((n, from)) = socket.recv_from(&mut buf) else { continue };
            let (Some(p), SocketAddr::V4(from)) = (Packet::parse(&buf[..n]), from) else { continue };
            match p.ty {
                T_REQUEST => {
                    let ack = ack_for(&p, ids.own_id, ids.own_sid, ids.peer_sid, (*from.ip(), from.port()));
                    let _ = socket.send_to(&ack.encode(), SocketAddr::V4(from));
                }
                // only an ACK that echoes our nonce, comes from a host we probed and names us as the receiver is the console's
                T_ACK if p.nonce == nonce && p.peer_id == ids.own_id && targets.iter().any(|t| t.ip() == from.ip()) => {
                    let (mip, mport) = mapped_endpoint(&p, ids.own_sid, ids.peer_sid);
                    let kind = cands
                        .iter()
                        .rev()
                        .find(|c| c.addr.trim() == from.ip().to_string())
                        .map_or("DERIVED", |c| c.kind.as_str());
                    return Some(PunchResult {
                        kind: kind.to_string(),
                        console_ip: from.ip().to_string(),
                        console_port: from.port(),
                        mapped_ip: mip.to_string(),
                        mapped_port: mport,
                    });
                }
                _ => {}
            }
        }
    }
    None
}

/// The local address the OS would use to reach `peer` (a throw-away connected UDP socket); the register request carries it in `HOST`.
pub fn local_ip_toward(peer: SocketAddr) -> Option<Ipv4Addr> {
    let s = UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect(peer).ok()?;
    match s.local_addr().ok()? {
        SocketAddr::V4(a) => Some(*a.ip()),
        SocketAddr::V6(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packets_round_trip_and_the_mapped_endpoint_is_recovered() {
        let (a_id, b_id) = ([1u8; 20], [2u8; 20]);
        let r = request(a_id, b_id, 5123, 9001, 0x0012_3456_789a);
        let wire = r.encode();
        assert_eq!(wire.len(), 88);
        assert_eq!(&wire[0..4], &[6, 0, 0, 0]);
        assert_eq!(Packet::parse(&wire), Some(r.clone()));
        // the console (sid 9001) answers a request that appeared to come from 192.168.1.10:40000
        let ack = ack_for(&r, b_id, 9001, 5123, (Ipv4Addr::new(192, 168, 1, 10), 40000));
        assert_eq!(&ack.encode()[0..4], &[7, 0, 0, 0]);
        assert_eq!(ack.nonce, r.nonce, "an ACK echoes the request's nonce");
        let got = Packet::parse(&ack.encode()).unwrap();
        assert_eq!(mapped_endpoint(&got, 5123, 9001), (Ipv4Addr::new(192, 168, 1, 10), 40000));
        assert_eq!(Packet::parse(&wire[..87]), None, "short packets are not ours");
    }

    #[test]
    fn only_sane_unicast_targets_are_ever_probed() {
        let c = |addr: &str, port: i64, mapped: &str, mport: i64| Candidate {
            kind: "LOCAL".into(),
            addr: addr.into(),
            mapped_addr: mapped.into(),
            port,
            mapped_port: mport,
        };
        let t = probe_targets(
            &[
                c("192.168.1.22", 9304, "0.0.0.0", 0),
                c("192.168.1.22", 9304, "0.0.0.0", 0), // duplicate
                c("127.0.0.1", 9304, "0.0.0.0", 0),
                c("0.0.0.0", 9304, "0.0.0.0", 0),
                c("255.255.255.255", 9304, "0.0.0.0", 0),
                c("224.0.0.1", 9304, "0.0.0.0", 0),
                c("169.254.1.1", 9304, "0.0.0.0", 0),
                c("10.0.0.5", 0, "0.0.0.0", 0),
                c("10.0.0.5", 70000, "0.0.0.0", 0),
                c("not an ip", 9304, "0.0.0.0", 0),
                c("203.0.113.9", 9304, "198.51.100.1", 5000), // both the address and its mapping
            ],
            false,
        );
        assert_eq!(
            t,
            vec![
                SocketAddrV4::new(Ipv4Addr::new(192, 168, 1, 22), 9304),
                SocketAddrV4::new(Ipv4Addr::new(203, 0, 113, 9), 9304),
                SocketAddrV4::new(Ipv4Addr::new(198, 51, 100, 1), 5000)
            ]
        );
        let many: Vec<Candidate> = (1..=60).map(|i| c(&format!("10.0.0.{i}"), 1000, "0.0.0.0", 0)).collect();
        assert_eq!(probe_targets(&many, false).len(), 16);
    }

    /// A console-like peer: answers the first REQUEST with an ACK, and sends one REQUEST of its own first (as a real console does).
    fn console(
        console_id: [u8; 20],
        sid: u16,
        our_sid: u16,
    ) -> (std::net::SocketAddrV4, std::thread::JoinHandle<bool>) {
        let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        let SocketAddr::V4(addr) = sock.local_addr().unwrap() else { unreachable!() };
        let h = std::thread::spawn(move || {
            sock.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut b = [0u8; 256];
            let Ok((n, from)) = sock.recv_from(&mut b) else { return false };
            let (Some(p), SocketAddr::V4(f)) = (Packet::parse(&b[..n]), from) else { return false };
            let ack = ack_for(&p, console_id, sid, our_sid, (*f.ip(), f.port()));
            sock.send_to(&ack.encode(), from).is_ok()
        });
        (addr, h)
    }

    #[test]
    fn the_punch_finds_the_console_and_learns_our_own_mapped_endpoint() {
        let ours = UdpSocket::bind("127.0.0.1:0").unwrap();
        let our_port = ours.local_addr().unwrap().port();
        let (console_id, our_id) = ([8u8; 20], [7u8; 20]);
        let (caddr, h) = console(console_id, 9001, 5123);
        let cand = Candidate {
            kind: "LOCAL".into(),
            addr: caddr.ip().to_string(),
            mapped_addr: "0.0.0.0".into(),
            port: caddr.port() as i64,
            mapped_port: 0,
        };
        let ids = Ids { own_id: our_id, peer_id: console_id, own_sid: 5123, peer_sid: 9001 };
        let r = punch_targets(&ours, &[caddr], &[cand], &ids, 42, &AtomicBool::new(false), Duration::from_millis(300))
            .expect("punched");
        assert_eq!((r.kind.as_str(), r.console_ip.as_str(), r.console_port), ("LOCAL", "127.0.0.1", caddr.port()));
        assert_eq!(
            (r.mapped_ip.as_str(), r.mapped_port),
            ("127.0.0.1", our_port),
            "the console told us where we appeared to be"
        );
        assert!(h.join().unwrap());
    }

    #[test]
    fn a_silent_console_gives_up_and_cancel_stops_it_at_once() {
        let ours = UdpSocket::bind("127.0.0.1:0").unwrap();
        let silent = UdpSocket::bind("127.0.0.1:0").unwrap(); // never answers
        let SocketAddr::V4(addr) = silent.local_addr().unwrap() else { unreachable!() };
        let ids = Ids { own_id: [1; 20], peer_id: [2; 20], own_sid: 1, peer_sid: 2 };
        let t = Instant::now();
        assert!(
            punch_targets(&ours, &[addr], &[], &ids, 1, &AtomicBool::new(false), Duration::from_millis(60)).is_none()
        );
        assert!(t.elapsed() < Duration::from_secs(3));
        let cancelled = AtomicBool::new(true);
        let t = Instant::now();
        assert!(punch_targets(&ours, &[addr], &[], &ids, 1, &cancelled, Duration::from_secs(30)).is_none());
        assert!(t.elapsed() < Duration::from_secs(2), "cancel is honoured within a round, not after the wait");
    }

    #[test]
    fn public_entry_refuses_loopback_candidates() {
        let ours = UdpSocket::bind("127.0.0.1:0").unwrap();
        let cand = Candidate {
            kind: "LOCAL".into(),
            addr: "127.0.0.1".into(),
            mapped_addr: "0.0.0.0".into(),
            port: 9,
            mapped_port: 0,
        };
        let ids = Ids { own_id: [1; 20], peer_id: [2; 20], own_sid: 1, peer_sid: 2 };
        assert!(
            hole_punch(&ours, &[cand], &ids, 1, &AtomicBool::new(false), Duration::from_millis(30), false).is_none()
        );
    }
}
