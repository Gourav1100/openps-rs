//! Registering a PS5 over the internet through PSN (spec 09): the live registrar, which needs the key tables, Sony's servers and the
//! real WebSocket. Compiled only with the `psn-live` feature.

use openps_core::{RemoteRegistrar, RemoteRequest};
use openps_discovery::DeviceType;
use openps_remote::session::{parse_ifconfig_ipv4, register, Params, Progress, RemoteError, Step};
use openps_remote::signaling::AcceptStyle;
use openps_remote::ws::LiveConnector;
use openps_rp::regist::Registered;
use openps_rp::tables::KeyTables;
use rand_core::OsRng;
use std::net::{Ipv4Addr, UdpSocket};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

pub struct LiveRegistrar {
    tables: KeyTables,
}

impl LiveRegistrar {
    pub fn new(tables: KeyTables) -> Self {
        Self { tables }
    }
}

/// Plain-language text for each step.
fn step_text(s: Step) -> &'static str {
    match s {
        Step::Connecting => "Connecting to PlayStation Network",
        Step::Session => "Asking your console to join",
        Step::WaitingForConsole => "Waiting for your console to answer",
        Step::Reaching => "Reaching your console",
        Step::Registering => "Registering",
    }
}

struct Steps<'a>(&'a dyn Fn(&str));
impl Progress for Steps<'_> {
    fn step(&self, s: Step) {
        (self.0)(step_text(s));
    }
}

/// What to tell the user (the technical detail is in the log).
fn explain(e: &RemoteError) -> String {
    match e {
        RemoteError::Cancelled => "Registration cancelled.".into(),
        RemoteError::TimedOut => "Your console did not answer through PlayStation Network. Make sure it is on (or in rest mode with \"Stay connected to the internet\" enabled), that Remote Play is enabled, and that it is signed in to your account.".into(),
        other => other.to_string(),
    }
}

fn local_ips() -> Vec<Ipv4Addr> {
    let out = std::process::Command::new("/sbin/ifconfig").stdin(std::process::Stdio::null()).output().ok();
    let mut v = out.map(|o| parse_ifconfig_ipv4(&String::from_utf8_lossy(&o.stdout))).unwrap_or_default();
    if v.is_empty() {
        // no ifconfig: the address the OS would use toward the internet
        if let Some(a) = openps_remote::punch::local_ip_toward("1.1.1.1:80".parse().expect("literal")) {
            v.push(a);
        }
    }
    v
}

/// The public address, for the second attempt only (it contacts a third-party service, like the original does on every attempt).
fn public_ip(http: &dyn openps_psn::http::HttpClient) -> Option<String> {
    let req = openps_psn::http::HttpRequest {
        method: "GET",
        url: "https://api.ipify.org".into(),
        headers: vec![],
        body: vec![],
    };
    let r = http.execute(&req).ok().filter(|r| r.status == 200)?;
    String::from_utf8_lossy(&r.body).trim().parse::<Ipv4Addr>().ok().map(|a| a.to_string())
}

impl RemoteRegistrar for LiveRegistrar {
    fn register(
        &self,
        req: RemoteRequest,
        cancel: Arc<AtomicBool>,
        progress: &dyn Fn(&str),
    ) -> Result<Registered, String> {
        let http = openps_psn::http::live::UreqClient::new(Duration::from_secs(20));
        let ips = local_ips();
        log::info!("remote registration: {} local address(es)", ips.len());
        let mut last = String::new();
        for attempt in 0..2 {
            let public = if attempt == 1 {
                progress("Trying again with your public address");
                public_ip(&http)
            } else {
                None
            };
            let socket = UdpSocket::bind("0.0.0.0:0").map_err(|e| format!("could not open a network port: {e}"))?;
            let params = Params {
                tables: &self.tables,
                access_token: req.access_token.clone(),
                account_id: req.account_id,
                duid: req.duid.clone(),
                local_ips: ips.clone(),
                public_ip: public,
                socket,
                accept_style: AcceptStyle::Original,
                allow_loopback: false,
                timeout: Duration::from_secs(150),
                cancel: cancel.clone(),
                round_wait: Duration::from_secs(2),
                step_wait: Duration::from_millis(100),
            };
            match register(&params, &http, &LiveConnector, &mut OsRng, &Steps(progress)) {
                Ok(r) => {
                    return Ok(Registered {
                        device: DeviceType::Ps5,
                        mac: r.mac,
                        nickname: r.nickname,
                        regist_key: r.regist_key,
                        rp_key: r.rp_key,
                        ap_name: "PS5".into(),
                    })
                }
                // the console could not be reached directly: the second attempt also advertises the public address
                Err(RemoteError::Console(m)) if attempt == 0 && m.contains("could not reach your console") => {
                    log::warn!("remote registration, attempt 1: {m}");
                    last = m;
                }
                Err(e) => {
                    if let RemoteError::Unreadable(detail) = &e {
                        log::warn!("remote registration: unreadable console message (redacted): {detail}");
                    }
                    log::warn!("remote registration failed: {e}");
                    return Err(explain(&e));
                }
            }
        }
        Err(last)
    }
}
