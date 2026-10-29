//! A pretend console for demos and end-to-end tests. Everything above the network edge is the real application:
//! the core, the session runtime, the handshake and stream engines, FEC recovery, decode and rendering. This backend
//! only replaces discovery, HTTP pairing and the UDP sockets, and plays a built-in 720p clip (HEVC or H.264) in a loop
//! from the "console" side, paced in real time, with optional packet loss to exercise FEC and keyframe recovery.

use openps_core::backend::Backend;
use openps_core::control::{ControlCommand, ControlLink};
use openps_crypto::RpCipher;
use openps_discovery::{Console, DeviceType, Status};
use openps_rp::client::ClientError;
use openps_rp::ctrl::Inbound;
use openps_rp::regist::{RegisterParams, Registered};
use openps_rp::session::{CtrlInfo, StreamingType};
use openps_session::link::{mem_pair, Link};
use openps_takion::chunk::{chunk_type, class, Chunk};
use openps_takion::proto::{msg_type, TakionMessage};
use openps_testkit::handshake::HandshakeConsole;
use openps_testkit::senkusha::SenkushaConsole;
use openps_testkit::stream::encode_frame;
use openps_video::nal::{self, Codec};
use std::io;
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const MAC: &str = "AA:BB:CC:DD:EE:01";
const HEVC: &[u8] = include_bytes!("../assets/demo_hevc.annexb");
const H264: &[u8] = include_bytes!("../assets/demo_h264.annexb");
const AUDIO: &[u8] = include_bytes!("../assets/demo_audio.opus");
pub const WIDTH: i32 = 1280;
pub const HEIGHT: i32 = 720;
const FEC_UNITS: usize = 2;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DemoConfig {
    pub codec: Codec,
    pub fps: u32,
    /// Every Nth frame loses one unit (FEC repairs it). 0 = never.
    pub recoverable_loss_every: u32,
    /// Every Nth frame loses more units than FEC can repair (the client must wait for the next keyframe). 0 = never.
    pub fatal_loss_every: u32,
    /// Stream a quiet looping chord (Opus, 10 ms frames, one redundant copy per packet; loss mode also drops audio packets).
    pub audio: bool,
}

impl Default for DemoConfig {
    fn default() -> Self {
        Self { codec: Codec::Hevc, fps: 30, recoverable_loss_every: 0, fatal_loss_every: 0, audio: true }
    }
}

impl DemoConfig {
    /// `OPENPS_DEMO=hevc|h264`, `OPENPS_DEMO_LOSS=1` (both loss kinds on), `OPENPS_DEMO_AUDIO=0` (no sound). `None` when demo mode is not requested.
    pub fn from_env() -> Option<Self> {
        let mode = std::env::var("OPENPS_DEMO").ok()?;
        let codec = match mode.to_ascii_lowercase().as_str() {
            "h264" | "avc" => Codec::H264,
            _ => Codec::Hevc,
        };
        let lossy = std::env::var("OPENPS_DEMO_LOSS").is_ok_and(|v| v == "1");
        Some(Self {
            codec,
            fps: std::env::var("OPENPS_DEMO_FPS")
                .ok()
                .and_then(|v| v.parse().ok())
                .filter(|f| (1..=120).contains(f))
                .unwrap_or(30),
            recoverable_loss_every: if lossy { 7 } else { 0 },
            fatal_loss_every: if lossy { 97 } else { 0 },
            audio: std::env::var("OPENPS_DEMO_AUDIO").map_or(true, |v| v != "0"),
        })
    }
}

struct DemoControl;

impl ControlLink for DemoControl {
    fn version(&self) -> String {
        "1.0".into()
    }
    fn session_id(&self) -> String {
        "demo-session".into()
    }
    fn cipher(&self) -> RpCipher {
        RpCipher::new([1; 16], [2; 16], [3; 16]) // the mock handshake console decrypts launch specs with this
    }
    fn info(&self) -> CtrlInfo {
        CtrlInfo { server_type: Some(1), full_hd: true, prohibited: false }
    }
    fn take_pending(&mut self) -> Vec<Inbound> {
        vec![Inbound::DisplayStatus { can_display: true }]
    }
    fn poll(&mut self, timeout: Duration) -> io::Result<Vec<Inbound>> {
        std::thread::sleep(timeout.min(Duration::from_millis(20)));
        Ok(vec![])
    }
    fn send(&mut self, _: ControlCommand) -> io::Result<()> {
        Ok(())
    }
}

pub struct DemoBackend {
    cfg: DemoConfig,
    stop: Arc<AtomicBool>,
    threads: Mutex<Vec<std::thread::JoinHandle<()>>>,
}

impl DemoBackend {
    pub fn new(cfg: DemoConfig) -> Self {
        Self { cfg, stop: Arc::new(AtomicBool::new(false)), threads: Mutex::new(vec![]) }
    }

    fn clip(&self) -> &'static [u8] {
        // dev aid: `OPENPS_DEMO_CLIP=<file.annexb>` streams your own H.264 clip instead (read once, kept for the process lifetime)
        if let Some(path) = std::env::var_os("OPENPS_DEMO_CLIP") {
            static CUSTOM: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
            if let Some(b) = CUSTOM.get_or_init(|| std::fs::read(path).unwrap_or_default()).as_slice().into() {
                // a file that is not Annex-B video has no access units: use the built-in clip instead of dividing by zero later
                if !b.is_empty() && !nal::access_units(self.cfg.codec, b).is_empty() {
                    return b;
                }
            }
        }
        match self.cfg.codec {
            Codec::Hevc => HEVC,
            Codec::H264 => H264,
        }
    }
}

impl Drop for DemoBackend {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for t in self.threads.lock().unwrap_or_else(|e| e.into_inner()).drain(..) {
            let _ = t.join();
        }
    }
}

impl Backend for DemoBackend {
    fn scan(&self, _: &[IpAddr]) -> io::Result<Vec<Console>> {
        Ok(vec![Console {
            device: DeviceType::Ps5,
            host_id: MAC.replace(':', "").to_lowercase(),
            host_name: "Demo PS5".into(),
            status: Status::Ready,
            addr: "127.0.0.1".parse().unwrap(),
        }])
    }
    fn wake(&self, _: DeviceType, _: IpAddr, _: &str) -> Result<bool, String> {
        Ok(true)
    }
    fn register(&self, _: &str, p: &RegisterParams) -> Result<Registered, ClientError> {
        if p.pin == 0 {
            // lets testers see the rejection path
            return Err(ClientError::Http("the console rejected the PIN (demo: 00000000 is always wrong)".into()));
        }
        Ok(Registered {
            device: p.device,
            mac: MAC.into(),
            nickname: "Demo PS5".into(),
            regist_key: "demo-regist-key".into(),
            rp_key: "demo-rp-key".into(),
            ap_name: "demo".into(),
        })
    }
    fn open_control(
        &self,
        _: &str,
        _: DeviceType,
        _: &str,
        _: &Registered,
        _: StreamingType,
    ) -> Result<Box<dyn ControlLink>, ClientError> {
        Ok(Box::new(DemoControl))
    }
    fn open_links(&self, _: &str) -> io::Result<(Link, Link)> {
        let (stream_client, stream_server) = mem_pair();
        let (test_client, test_server) = mem_pair();
        let (cfg, stop, clip) = (self.cfg, self.stop.clone(), self.clip());
        let mut t = self.threads.lock().unwrap_or_else(|e| e.into_inner());
        t.retain(|h| !h.is_finished()); // do not accumulate one handle per past session
        t.push(std::thread::spawn(move || serve_stream(stream_server, cfg, clip, stop)));
        let stop = self.stop.clone();
        t.push(std::thread::spawn(move || serve_senkusha(test_server, stop)));
        Ok((stream_client, test_client))
    }
}

/// Split one access unit into wire units (source + parity) the way the console does: at most ~1200 bytes per unit and
/// never more than 200 source units (GF(256) limit with room for parity).
fn packetise(au: &[u8]) -> Vec<Vec<u8>> {
    let k = au.len().div_ceil(1200).clamp(1, 200);
    encode_frame(au, k, FEC_UNITS)
}

fn serve_stream(mut link: Link, cfg: DemoConfig, clip: &'static [u8], stop: Arc<AtomicBool>) {
    let mut con = HandshakeConsole::new(14);
    con.expect_session_key = Some("demo-session".into());
    con.video_header = Some(nal::header(cfg.codec, clip));
    con.resolution = (WIDTH, HEIGHT);
    let audio_frames = openps_testkit::ogg::opus_packets(AUDIO).unwrap_or_default();
    con.audio_header = Some(audio_header());
    let deadline = Instant::now() + Duration::from_secs(15);
    while !con.done() && Instant::now() < deadline && !stop.load(Ordering::Relaxed) {
        if let Ok(Some(d)) = link.source.recv(Duration::from_millis(20)) {
            for r in con.on_packet(&d) {
                if link.sink.send(&r).is_err() {
                    return;
                }
            }
        }
    }
    let Some(mut sc) = con.into_stream_console() else { return };
    let units = nal::access_units(cfg.codec, clip);
    if units.is_empty() {
        return; // nothing to play (cannot happen with the built-in clips)
    }
    let pace = Duration::from_secs_f64(1.0 / f64::from(cfg.fps.max(1)));
    let audio_pace = Duration::from_millis(10);
    let (mut next_video, mut next_audio) = (Instant::now(), Instant::now());
    let mut id: u16 = 0;
    let mut n: u64 = 0;
    let mut a_n: u64 = 0;
    let loop_audio = audio_frames.len().min(300); // 3 s: matches the video loop
    while !stop.load(Ordering::Relaxed) {
        let now = Instant::now();
        if now >= next_video {
            let au = &units[(n as usize) % units.len()];
            let wire = packetise(au);
            let total = wire.len() as u16;
            let fec = FEC_UNITS as u16;
            let n32 = n as u32 + 1;
            let drop_fatal = cfg.fatal_loss_every > 0 && n32.is_multiple_of(cfg.fatal_loss_every);
            let drop_one = cfg.recoverable_loss_every > 0 && n32.is_multiple_of(cfg.recoverable_loss_every);
            for (i, u) in wire.iter().enumerate() {
                let lost = (drop_fatal && i < FEC_UNITS + 1) || (drop_one && i == 0);
                if lost {
                    continue;
                }
                if link.sink.send(&sc.video(14, id, i as u16, total, fec, u)).is_err() {
                    return;
                }
            }
            id = id.wrapping_add(1);
            n += 1;
            next_video += pace;
            if Instant::now() > next_video + Duration::from_millis(250) {
                next_video = Instant::now(); // fell behind (debugger, suspended laptop): do not burst to catch up
            }
        }
        if cfg.audio && loop_audio > 0 && now >= next_audio {
            let cur = &audio_frames[(a_n as usize) % loop_audio];
            let prev = &audio_frames[(a_n.saturating_sub(1) as usize) % loop_audio];
            // one new frame plus a redundant copy of the previous one (wire layout per spec 02 8.2). With loss on, some
            // packets are dropped: the client's assembler treats a redundant copy behind a newer frame as stale, so a
            // lost packet is a gap, as in the original.
            let lost = cfg.recoverable_loss_every > 0 && (a_n + 1).is_multiple_of(13);
            if !lost && link.sink.send(&sc.audio(a_n as u16, &[cur, prev], 1, 1, cur.len())).is_err() {
                return;
            }
            a_n += 1;
            next_audio += audio_pace;
            if Instant::now() > next_audio + Duration::from_millis(250) {
                next_audio = Instant::now();
            }
        }
        // sleep until the next due event, absorbing whatever the client sends (feedback, acks, keyframe requests)
        let due = if cfg.audio { next_video.min(next_audio) } else { next_video };
        let now = Instant::now();
        if due > now {
            match link.source.recv(due - now) {
                Err(_) => return, // client gone
                Ok(Some(d)) if is_idr_request(&d) => {
                    // a real console answers a keyframe request promptly: ack it and restart the clip at its IDR
                    let ack = Chunk::new(0, chunk_type::DATA_ACK, 0, request_seq(&d), class::CONTROL, vec![0; 4]);
                    if link.sink.send(&ack.encode()).is_err() {
                        return;
                    }
                    let len = units.len() as u64;
                    n = n.div_ceil(len) * len;
                }
                Ok(_) => {}
            }
        }
    }
}

/// The 14-byte header the console announces: stereo, 16-bit, 48 kHz, 480 samples (10 ms).
pub fn audio_header() -> Vec<u8> {
    let mut b = vec![2u8, 16];
    b.extend(48000u32.to_be_bytes());
    b.extend(480u32.to_be_bytes());
    b.extend(1u32.to_be_bytes());
    b
}

fn is_idr_request(d: &[u8]) -> bool {
    d.first().is_some_and(|b| b & 0x0F == 0)
        && Chunk::decode(d).is_some_and(|c| {
            c.flags == 1 && TakionMessage::parse(&c.payload).is_some_and(|m| m.r#type == Some(msg_type::IDR_REQUEST))
        })
}

fn request_seq(d: &[u8]) -> u32 {
    Chunk::decode(d).map_or(0, |c| c.sequence)
}

fn serve_senkusha(mut link: Link, stop: Arc<AtomicBool>) {
    let mut con = SenkushaConsole::new(true, 1400, 1300);
    while !stop.load(Ordering::Relaxed) {
        match link.source.recv(Duration::from_millis(20)) {
            Ok(Some(d)) => {
                for r in con.on_packet(&d) {
                    if link.sink.send(&r).is_err() {
                        return;
                    }
                }
            }
            Ok(None) => {}
            Err(_) => return,
        }
    }
}

/// Offline stand-in for PSN sign-in: "signs in" instantly as a made-up player. Contacts nothing.
pub struct DemoPsn;

pub const DEMO_SIGN_IN_URL: &str = "demo://sign-in?code=demo";

impl openps_core::PsnService for DemoPsn {
    fn authorize_url(&self) -> Result<String, String> {
        Ok(DEMO_SIGN_IN_URL.into())
    }
    fn complete(&self, url: &str) -> Result<openps_core::PsnAccountInfo, String> {
        if !url.contains("code=") {
            return Err("sign-in was cancelled".into());
        }
        Ok(openps_core::PsnAccountInfo { online_id: "DemoPlayer".into(), account_id: "1234567890123456".into() })
    }
    fn sign_out(&self) {}

    // the PSN API, faked: any well-formed npsso "signs in", and the account owns one demo console
    fn api_available(&self) -> bool {
        true
    }
    fn api_sign_in(&self, pasted: &str) -> Result<(), String> {
        openps_psn::api::extract_npsso(pasted)
            .ok_or_else(|| "that does not look like a PlayStation login token (npsso)".to_string())?;
        DEMO_API_SIGNED_IN.store(true, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }
    fn remote_access_token(&self) -> Result<String, String> {
        Ok("demo-token".into())
    }
    fn api_signed_in(&self) -> bool {
        DEMO_API_SIGNED_IN.load(std::sync::atomic::Ordering::Relaxed)
    }
    fn api_sign_out(&self) {
        DEMO_API_SIGNED_IN.store(false, std::sync::atomic::Ordering::Relaxed);
    }
    fn api_library(&self) -> Result<openps_psn::library::Library, String> {
        Ok(demo_library())
    }
    fn api_launch(&self, _duid: &str, title_id: &str) -> Result<String, String> {
        if title_id.is_empty() {
            return Err("no title".into());
        }
        Ok(format!("demo-{title_id}"))
    }
    fn api_command_status(&self, command_id: &str) -> Result<openps_psn::library::CommandStatus, String> {
        Ok(openps_psn::library::CommandStatus {
            status: "completed".into(),
            result: "success".into(),
            action: "launchTitle".into(),
            action_state: if command_id.starts_with("demo-") { "done".into() } else { String::new() },
        })
    }
    fn api_art(&self, url: &str) -> Result<(Vec<u8>, String), String> {
        Ok(demo_art(url))
    }
    fn api_consoles(&self) -> Result<Vec<openps_core::PsnConsole>, String> {
        Ok(vec![openps_core::PsnConsole {
            duid: "00000007004100800000000000000000000000000000demo".into(),
            name: "Demo PS5".into(),
            platform: "PS5".into(),
            updated: Some("2026-10-22T09:00:00Z".into()),
        }])
    }
}

/// name, platform, trophy progress %, earned and total counts (platinum, gold, silver, bronze)
type DemoGame = (&'static str, &'static str, u8, [u32; 4], [u32; 4]);
const DEMO_GAMES: &[DemoGame] = &[
    ("Ghost of Yōtei", "PS5", 64, [1, 6, 11, 28], [1, 8, 14, 33]),
    ("Astro Bot", "PS5", 100, [1, 9, 12, 20], [1, 9, 12, 20]),
    ("Gran Turismo 7", "PS5", 38, [0, 4, 15, 40], [1, 9, 24, 66]),
    ("Spider-Man 2", "PS5", 77, [1, 7, 12, 30], [1, 8, 14, 33]),
    ("Elden Ring", "PS5", 12, [0, 1, 4, 18], [1, 11, 20, 40]),
    ("God of War Ragnarök", "PS5", 55, [0, 5, 14, 29], [1, 9, 28, 36]),
    ("Returnal", "PS5", 25, [0, 2, 9, 22], [1, 6, 13, 32]),
    ("Horizon Forbidden West", "PS5", 90, [1, 10, 14, 30], [1, 10, 14, 33]),
    ("Hades", "PS5", 0, [0, 0, 0, 0], [1, 9, 15, 26]),
    ("The Last of Us Part I", "PS5", 41, [0, 3, 9, 25], [1, 8, 16, 38]),
    ("Marvel’s Spider-Man", "PS4", 100, [1, 9, 15, 31], [1, 9, 15, 31]),
    ("Bloodborne", "PS4", 18, [0, 1, 5, 14], [1, 9, 12, 27]),
];

fn demo_library() -> openps_psn::library::Library {
    use openps_psn::library::{Game, GameTrophies, Library, TrophyCounts, TrophySummary};
    let c = |a: [u32; 4]| TrophyCounts { platinum: a[0], gold: a[1], silver: a[2], bronze: a[3] };
    let games = DEMO_GAMES
        .iter()
        .enumerate()
        .map(|(i, (name, platform, progress, earned, total))| Game {
            title_id: Some(format!("PPSA{:05}", 1000 + i)),
            name: (*name).into(),
            platform: (*platform).into(),
            image_url: Some(format!("https://image.api.playstation.com/demo/{i}.png")),
            last_played: Some(format!("2026-10-{:02}T09:00:00Z", 22 - i.min(20))),
            play_minutes: Some(60 + 37 * i as u32),
            trophies: (*progress > 0 || i % 3 == 0).then(|| GameTrophies {
                progress: *progress,
                earned: c(*earned),
                total: c(*total),
            }),
        })
        .collect();
    let installed = (0..DEMO_GAMES.len())
        .filter(|i| i % 4 != 3)
        .map(|i| openps_psn::library::InstalledTitle {
            title_id: format!("PPSA{:05}", 1000 + i),
            name: DEMO_GAMES[i].0.into(),
            platform: DEMO_GAMES[i].1.into(),
            kind: "game".into(),
            size_bytes: 20_000_000_000 + i as u64 * 1_000_000_000,
            last_access: Some(format!("2026-10-{:02}T09:00:00Z", 22 - i.min(20))),
        })
        .chain(std::iter::once(openps_psn::library::InstalledTitle {
            title_id: "PPSA99999".into(),
            name: "Never Played Yet".into(),
            platform: "PS5".into(),
            kind: "game".into(),
            size_bytes: 5_000_000_000,
            last_access: Some("2026-01-01T00:00:00Z".into()),
        }))
        .collect();
    Library {
        consoles: vec![openps_psn::library::ConsoleTitles {
            duid: "00000007004100800000000000000000000000000000demo".into(),
            name: "Demo PS5".into(),
            installed,
            updated: None,
        }],
        games,
        summary: Some(TrophySummary {
            level: 214,
            progress: 61,
            earned: TrophyCounts { platinum: 9, gold: 96, silver: 241, bronze: 612 },
        }),
    }
}

/// A generated cover so the demo shows the real layout without contacting anything.
fn demo_art(url: &str) -> (Vec<u8>, String) {
    let i: usize = url.rsplit('/').next().and_then(|f| f.split('.').next()).and_then(|n| n.parse().ok()).unwrap_or(0);
    let name = DEMO_GAMES.get(i).map_or("?", |g| g.0);
    let initials: String = name.split_whitespace().filter_map(|w| w.chars().next()).take(2).collect();
    let hue = (i * 47) % 360;
    let svg = format!(
        "<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 256 256'><defs><linearGradient id='g' x1='0' y1='0' x2='1' y2='1'><stop offset='0' stop-color='hsl({hue},70%,45%)'/><stop offset='1' stop-color='hsl({},60%,18%)'/></linearGradient></defs><rect width='256' height='256' fill='url(#g)'/><text x='128' y='158' font-family='Helvetica,Arial,sans-serif' font-size='96' font-weight='700' text-anchor='middle' fill='rgba(255,255,255,.88)'>{initials}</text></svg>",
        (hue + 40) % 360
    );
    (svg.into_bytes(), "image/svg+xml".into())
}

static DEMO_API_SIGNED_IN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Offline stand-in for registering over the internet: walks through the same steps with short pauses and "registers" a made-up
/// console. Contacts nothing.
pub struct DemoRegistrar;

impl openps_core::RemoteRegistrar for DemoRegistrar {
    fn register(
        &self,
        req: openps_core::RemoteRequest,
        cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
        progress: &dyn Fn(&str),
    ) -> Result<openps_rp::regist::Registered, String> {
        for step in
            ["Connecting to PlayStation Network", "Asking your console to join", "Reaching your console", "Registering"]
        {
            progress(step);
            for _ in 0..10 {
                if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                    return Err("cancelled".into());
                }
                std::thread::sleep(std::time::Duration::from_millis(40));
            }
        }
        if req.duid.contains("fail") {
            return Err(
                "could not reach your console over the network. It must be on and reachable from this computer.".into(),
            );
        }
        Ok(openps_rp::regist::Registered {
            device: openps_discovery::DeviceType::Ps5,
            mac: "AABBCCDDEE02".into(),
            nickname: "Demo PS5 (internet)".into(),
            regist_key: "3a3b3c3d3e3f4041".into(),
            rp_key: "00112233445566778899aabbccddeeff".into(),
            ap_name: "PS5".into(),
        })
    }
}
