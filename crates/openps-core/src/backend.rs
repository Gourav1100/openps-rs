//! Every network operation the core needs, behind one trait. Production uses [`RealBackend`]; tests use a fake,
//! so the orchestration (store, state machine, event flow) is testable without a console or a network.

use crate::control::{ControlLink, RealControl};
use openps_discovery::{Console, DeviceType, ScanOptions};
use openps_rp::client::{self, ClientError, OpenParams, Timeouts};
use openps_rp::regist::{RegisterParams, Registered};
use openps_rp::session::StreamingType;
use openps_rp::tables::KeyTables;
use openps_session::link::{udp_link, Link};
use rand_core::{OsRng, RngCore};
use std::io;
use std::net::IpAddr;
use std::time::Duration;

pub trait Backend: Send + Sync {
    /// Search the local network (`targets` empty = broadcast).
    fn scan(&self, targets: &[IpAddr]) -> io::Result<Vec<Console>>;
    /// Send the wake-up packet and wait for the console to come up. `Ok(true)` once it reports ready.
    fn wake(&self, device: DeviceType, ip: IpAddr, regist_key_hex: &str) -> Result<bool, String>;
    fn register(&self, host: &str, params: &RegisterParams) -> Result<Registered, ClientError>;
    fn open_control(
        &self,
        host: &str,
        device: DeviceType,
        version: &str,
        reg: &Registered,
        streaming: StreamingType,
    ) -> Result<Box<dyn ControlLink>, ClientError>;
    /// The two UDP links to the console: (Takion stream, Senkusha network test).
    fn open_links(&self, host: &str) -> io::Result<(Link, Link)>;
}

pub const TAKION_PORT: u16 = 9296;
pub const SENKUSHA_PORT: u16 = 9297;

pub struct RealBackend {
    tables: Option<KeyTables>,
    timeouts: Timeouts,
}

impl RealBackend {
    /// `tables` come from [`KeyTables::from_env`] / `load`; without them pairing and sessions report a clear error.
    pub fn new(tables: Option<KeyTables>) -> Self {
        Self { tables, timeouts: Timeouts::default() }
    }

    fn tables(&self) -> Result<&KeyTables, ClientError> {
        self.tables.as_ref().ok_or_else(|| {
            ClientError::Http(
                "key tables are not configured (set OPENPS_TABLES, see crates/openps-rp/README.md)".into(),
            )
        })
    }
}

fn parse_ip(host: &str) -> io::Result<std::net::SocketAddr> {
    use std::net::ToSocketAddrs;
    (host, 0u16).to_socket_addrs()?.next().ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "unresolvable host"))
}

impl Backend for RealBackend {
    fn scan(&self, targets: &[IpAddr]) -> io::Result<Vec<Console>> {
        let mut opts = ScanOptions::default();
        if !targets.is_empty() {
            opts.targets = targets.to_vec();
        }
        openps_discovery::scan(&opts)
    }

    fn wake(&self, device: DeviceType, ip: IpAddr, regist_key_hex: &str) -> Result<bool, String> {
        openps_discovery::wake(device, ip, regist_key_hex, 10, Duration::from_millis(250), None)
            .map_err(|e| e.to_string())?;
        // The original waits 25 s (PS5) / 40 s (PS4). A real PS5 answers after about 13 s (measured), so 20 s / 30 s leaves a margin and
        // still fails sooner: the user is told to retry instead of staring at a spinner.
        openps_discovery::wait_until_ready(device, ip, device.wake_timeout() * 4 / 5, None).map_err(|e| e.to_string())
    }

    fn register(&self, host: &str, params: &RegisterParams) -> Result<Registered, ClientError> {
        client::register(self.tables()?, host, client::DEFAULT_PORT, params, &mut OsRng, &self.timeouts)
    }

    fn open_control(
        &self,
        host: &str,
        device: DeviceType,
        version: &str,
        reg: &Registered,
        streaming: StreamingType,
    ) -> Result<Box<dyn ControlLink>, ClientError> {
        let p = OpenParams { host, port: client::DEFAULT_PORT, device, version, registration: reg, streaming };
        let mut s = client::open_session(self.tables()?, &p, &mut OsRng, &self.timeouts)?;
        // fallback id (unix seconds plus random text) if the console never announces one, as the original does
        let fallback = || {
            let mut b = [0u8; 32];
            OsRng.fill_bytes(&mut b);
            let secs =
                std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
            format!("{secs}{}", b.iter().map(|x| format!("{x:02x}")).collect::<String>())
        };
        let id = s.wait_for_session_id(Duration::from_secs(3), fallback)?.to_string();
        Ok(Box::new(RealControl::new(s, id)))
    }

    fn open_links(&self, host: &str) -> io::Result<(Link, Link)> {
        let ip = parse_ip(host)?.ip();
        Ok((udp_link((ip, TAKION_PORT).into())?, udp_link((ip, SENKUSHA_PORT).into())?))
    }
}
