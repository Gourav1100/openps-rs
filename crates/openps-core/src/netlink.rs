//! How much bandwidth "Auto" may ask for. The console's own encoder decides what it really sends, but the launch specification
//! tells it how much the client can take (`bwKbpsSent`). Asking for a fixed 6-15 Mbit/s wastes a gigabit LAN, so for a console on the
//! SAME network segment this looks at the link that reaches it: wired Ethernet speed, or the Wi-Fi rate, and asks for a large share.
//! A console behind a router (or an unknown link) keeps the conservative default: never ask more than the path is known to carry.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Highest request ever made (100 Mbit/s: the most the original accepts; a real PS5 ended the stream when asked for 800).
pub const MAX_AUTO_KBPS: i32 = openps_takion::launch::MAX_AUTO_KBPS;
/// Share of a wired link's speed to ask for (headroom for the console's own traffic, ACKs and bursts).
const WIRED_SHARE: f64 = 0.8;
/// Share of a Wi-Fi PHY rate to ask for: real throughput is roughly half the negotiated rate.
const WIFI_SHARE: f64 = 0.5;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Medium {
    Wired,
    WiFi,
    Unknown,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Link {
    pub interface: String,
    pub medium: Medium,
    /// Negotiated rate in Mbit/s, if it could be read.
    pub mbps: Option<u32>,
    /// The console is on the same network segment (no gateway in between).
    pub direct: bool,
}

/// Bitrate to request for `link`, in kbit/s, or `None` to keep the resolution-based default.
pub fn auto_kbps(link: &Link) -> Option<i32> {
    if !link.direct {
        return None;
    }
    let mbps = f64::from(link.mbps.filter(|m| *m > 0)?);
    let share = match link.medium {
        Medium::Wired => WIRED_SHARE,
        Medium::WiFi => WIFI_SHARE,
        Medium::Unknown => return None,
    };
    let kbps = (mbps * 1000.0 * share).floor();
    Some(kbps.clamp(0.0, f64::from(MAX_AUTO_KBPS)) as i32)
}

// ---- parsing (pure, tested on real output) -------------------------------------------------------------------------------------

/// `route -n get <ip>` -> (interface, has a gateway).
pub fn parse_route(out: &str) -> Option<(String, bool)> {
    let field = |name: &str| {
        out.lines().find_map(|l| l.trim().strip_prefix(name).map(|v| v.trim_start_matches(':').trim().to_string()))
    };
    let iface = field("interface")?;
    (!iface.is_empty() && iface.len() <= 16 && iface.bytes().all(|b| b.is_ascii_alphanumeric())).then_some(())?;
    Some((iface, field("gateway").is_some()))
}

/// `ifconfig <if>` media line -> Mbit/s, e.g. `media: autoselect (1000baseT <full-duplex>)`, `(10Gbase-T ...)`, `(2500Base-T ...)`.
pub fn parse_media_mbps(out: &str) -> Option<u32> {
    let line = out.lines().find(|l| l.trim_start().starts_with("media:"))?;
    let inner = line.split('(').nth(1)?;
    let word = inner.split([' ', '<', ')']).next()?.to_ascii_lowercase();
    let (num, unit) = match word.find("gbase") {
        Some(i) => (&word[..i], 1000),
        None => (&word[..word.find("base")?], 1),
    };
    num.parse::<u32>().ok().and_then(|n| n.checked_mul(unit)).filter(|m| *m > 0)
}

/// `networksetup -listallhardwareports` -> is `iface` the Wi-Fi port?
pub fn is_wifi_port(out: &str, iface: &str) -> bool {
    let mut port = "";
    for l in out.lines() {
        if let Some(p) = l.strip_prefix("Hardware Port:") {
            port = p.trim();
        } else if let Some(d) = l.strip_prefix("Device:") {
            if d.trim() == iface {
                return port.eq_ignore_ascii_case("wi-fi") || port.eq_ignore_ascii_case("airport");
            }
        }
    }
    false
}

/// `system_profiler SPAirPortDataType -json` -> current transmit rate (Mbit/s) of `iface`.
pub fn parse_wifi_rate(json: &str, iface: &str) -> Option<u32> {
    fn walk(v: &serde_json::Value, iface: &str) -> Option<u32> {
        match v {
            serde_json::Value::Object(m) => {
                if m.get("_name").and_then(|n| n.as_str()) == Some(iface) {
                    let r = m.get("spairport_current_network_information")?.get("spairport_network_rate")?;
                    return r.as_u64().map(|x| x as u32).or_else(|| r.as_str().and_then(|s| s.trim().parse().ok()));
                }
                m.values().find_map(|x| walk(x, iface))
            }
            serde_json::Value::Array(a) => a.iter().find_map(|x| walk(x, iface)),
            _ => None,
        }
    }
    walk(&serde_json::from_str(json).ok()?, iface).filter(|r| *r > 0)
}

// ---- probing (macOS) --------------------------------------------------------------------------------------------------------------

/// Run a system tool with fixed arguments and a time limit; `None` on any problem.
fn run(program: &str, args: &[&str], limit: Duration) -> Option<String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut pipe = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut out = String::new();
        std::io::Read::read_to_string(&mut pipe, &mut out).ok().map(|_| out)
    });
    let start = Instant::now();
    loop {
        match child.try_wait().ok()? {
            Some(_) => break,
            None if start.elapsed() > limit => {
                let _ = child.kill();
                let _ = child.wait();
                return None; // the reader ends when the pipe closes
            }
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    reader.join().ok()?
}

/// The link that reaches `host` (an IP address). Only fixed system tools are run; `host` was validated by the caller.
pub fn probe(host: &str) -> Option<Link> {
    if host.parse::<std::net::IpAddr>().is_err() {
        return None; // a hostname could resolve anywhere: do not guess
    }
    let (interface, gateway) = parse_route(&run("/sbin/route", &["-n", "get", host], Duration::from_secs(2))?)?;
    let direct = !gateway;
    let ports = run("/usr/sbin/networksetup", &["-listallhardwareports"], Duration::from_secs(2)).unwrap_or_default();
    if is_wifi_port(&ports, &interface) {
        let mbps = run("/usr/sbin/system_profiler", &["SPAirPortDataType", "-json"], Duration::from_secs(6))
            .and_then(|j| parse_wifi_rate(&j, &interface));
        return Some(Link { interface, medium: Medium::WiFi, mbps, direct });
    }
    let mbps = run("/sbin/ifconfig", &[&interface], Duration::from_secs(2)).and_then(|o| parse_media_mbps(&o));
    let medium = if mbps.is_some() { Medium::Wired } else { Medium::Unknown };
    Some(Link { interface, medium, mbps, direct })
}

/// The bitrate Auto should request for `host`, with a log line saying why.
pub fn auto_kbps_for(host: &str) -> Option<i32> {
    let link = match probe(host) {
        Some(l) => l,
        None => {
            log::info!("auto bitrate: could not read the link to {host}; using the default");
            return None;
        }
    };
    let k = auto_kbps(&link);
    log::info!(
        "auto bitrate: {} via {} ({:?}, {} Mbit/s, {}): {}",
        host,
        link.interface,
        link.medium,
        link.mbps.map_or("unknown".into(), |m| m.to_string()),
        if link.direct { "same network" } else { "through a router" },
        k.map_or("default".into(), |k| format!("asking for {k} kbit/s"))
    );
    k
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROUTE_DIRECT: &str = "   route to: 192.168.1.22\ndestination: 192.168.1.0\n       mask: 255.255.255.0\n  interface: en0\n      flags: <UP,DONE,CLONING,STATIC>\n recvpipe  sendpipe  ssthresh  rtt,msec    rttvar  hopcount      mtu     expire\n       0         0         0         0         0         0      1500      -391\n";
    const ROUTE_GATEWAY: &str = "   route to: 8.8.8.8\ndestination: default\n       mask: default\n    gateway: 192.168.1.1\n  interface: en0\n      flags: <UP,GATEWAY,DONE,STATIC,PRCLONING,GLOBAL>\n";
    const IFCONFIG_GIGE: &str = "en5: flags=8863<UP,BROADCAST,SMART,RUNNING,SIMPLEX,MULTICAST> mtu 1500\n\tether 00:11:22:33:44:55\n\tmedia: autoselect (1000baseT <full-duplex,flow-control>)\n\tstatus: active\n";
    const PORTS: &str = "Hardware Port: Ethernet Adapter (en5)\nDevice: en5\nEthernet Address: 00:11:22:33:44:55\n\nHardware Port: Wi-Fi\nDevice: en0\nEthernet Address: aa:bb:cc:dd:ee:ff\n";

    #[test]
    fn route_output_gives_the_interface_and_whether_a_router_is_in_between() {
        assert_eq!(parse_route(ROUTE_DIRECT), Some(("en0".into(), false)));
        assert_eq!(parse_route(ROUTE_GATEWAY), Some(("en0".into(), true)));
        assert_eq!(parse_route(""), None);
        assert_eq!(parse_route("interface: ../../etc\n"), None, "an interface name is only letters and digits");
    }

    #[test]
    fn media_lines_in_every_common_form() {
        assert_eq!(parse_media_mbps(IFCONFIG_GIGE), Some(1000));
        for (line, want) in [
            ("media: autoselect (100baseTX <full-duplex>)", Some(100)),
            ("media: autoselect (2500Base-T <full-duplex>)", Some(2500)),
            ("media: autoselect (10Gbase-T <full-duplex>)", Some(10_000)),
            ("media: autoselect (10baseT/UTP <half-duplex>)", Some(10)),
            ("media: autoselect", None),
            ("media: autoselect (none)", None),
            ("status: active", None),
        ] {
            assert_eq!(parse_media_mbps(line), want, "{line}");
        }
    }

    #[test]
    fn wifi_ports_are_recognised_by_name() {
        assert!(is_wifi_port(PORTS, "en0"));
        assert!(!is_wifi_port(PORTS, "en5"));
        assert!(!is_wifi_port(PORTS, "en9"));
    }

    #[test]
    fn the_wifi_rate_is_found_in_system_profiler_json() {
        let j = r#"{"SPAirPortDataType":[{"spairport_airport_interfaces":[{"_name":"en0","spairport_current_network_information":{"_name":"home","spairport_network_rate":866,"spairport_network_channel":"36"}},{"_name":"awdl0"}]}]}"#;
        assert_eq!(parse_wifi_rate(j, "en0"), Some(866));
        assert_eq!(parse_wifi_rate(j, "awdl0"), None);
        assert_eq!(
            parse_wifi_rate(
                r#"{"x":[{"_name":"en0","spairport_current_network_information":{"spairport_network_rate":"1200"}}]}"#,
                "en0"
            ),
            Some(1200)
        );
        assert_eq!(parse_wifi_rate("not json", "en0"), None);
    }

    fn link(medium: Medium, mbps: Option<u32>, direct: bool) -> Link {
        Link { interface: "en0".into(), medium, mbps, direct }
    }

    #[test]
    fn policy_asks_for_a_large_share_of_a_direct_link_and_never_more_than_the_cap() {
        assert_eq!(MAX_AUTO_KBPS, 100_000);
        assert_eq!(auto_kbps(&link(Medium::Wired, Some(1000), true)), Some(MAX_AUTO_KBPS), "gigabit: capped");
        assert_eq!(auto_kbps(&link(Medium::Wired, Some(100), true)), Some(80_000), "fast ethernet: 80% of the link");
        assert_eq!(auto_kbps(&link(Medium::Wired, Some(10_000), true)), Some(MAX_AUTO_KBPS));
        assert_eq!(auto_kbps(&link(Medium::WiFi, Some(120), true)), Some(60_000), "half of a Wi-Fi rate");
        assert_eq!(auto_kbps(&link(Medium::WiFi, Some(4000), true)), Some(MAX_AUTO_KBPS));
    }

    #[test]
    fn policy_stays_conservative_when_the_path_is_not_known_to_carry_it() {
        assert_eq!(auto_kbps(&link(Medium::Wired, Some(1000), false)), None, "through a router");
        assert_eq!(auto_kbps(&link(Medium::Wired, None, true)), None);
        assert_eq!(auto_kbps(&link(Medium::WiFi, None, true)), None);
        assert_eq!(auto_kbps(&link(Medium::Unknown, Some(1000), true)), None);
    }

    #[test]
    fn hostnames_and_garbage_are_never_probed() {
        assert!(probe("example.com").is_none());
        assert!(probe("1.2.3.4; reboot").is_none());
        assert!(probe("").is_none());
    }

    #[test]
    fn a_real_probe_of_the_loopback_address_does_not_hang_or_panic() {
        let t = Instant::now();
        let _ = probe("127.0.0.1");
        assert!(t.elapsed() < Duration::from_secs(10));
    }
}
