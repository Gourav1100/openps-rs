//! The application core: everything the UI can ask for, with no UI dependency.

use crate::backend::Backend;
use crate::control::{self, ControlCommand};
use crate::events::{AppEvent, EventSink, MediaSink, StreamStats};
use crate::store::{ConsoleStore, StoreError};
use crate::sync::LockExt;
use crate::types::*;
use openps_discovery::DeviceType;
use openps_psn::store::SecretStore;
use openps_rp::client::ClientError;
use openps_rp::regist::{RegisterError, RegisterParams, Registered};
use openps_rp::session::StreamingType;
use openps_session::{connect as start_session, Commander, ConnectParams, NetworkInfo, NetworkTest, Notice};
use openps_stream::{Event, StreamConfig};
use openps_takion::handshake::{HandshakeConfig, MAX_RESENDS, RESEND_INTERVAL_S};
use openps_takion::launch::LaunchParams;
use openps_takion::senkusha::SenkushaConfig;
use rand_core::{OsRng, RngCore};
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

#[derive(Debug, PartialEq, Eq)]
pub enum CoreError {
    UnknownConsole,
    /// The console's address is not known yet: scan first.
    NoAddress,
    /// PS5 registration needs the PSN account id.
    NeedsPsnLogin,
    PsnNotConfigured,
    AlreadyConnected,
    NotConnected,
    /// Registration or session setup was refused or failed; the text is user-presentable.
    Console(String),
    Network(String),
    Store(String),
}

impl std::fmt::Display for CoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CoreError::UnknownConsole => f.write_str("unknown console"),
            CoreError::NoAddress => f.write_str("the console's address is unknown; scan the network first"),
            CoreError::NeedsPsnLogin => f.write_str("sign in to PSN first: PS5 registration needs your PSN account id"),
            CoreError::PsnNotConfigured => {
                f.write_str("PSN sign-in is not configured (see crates/openps-psn/README.md)")
            }
            CoreError::AlreadyConnected => f.write_str("a session is already running"),
            CoreError::NotConnected => f.write_str("no session is running"),
            CoreError::Console(m) | CoreError::Network(m) | CoreError::Store(m) => f.write_str(m),
        }
    }
}
impl std::error::Error for CoreError {}

impl From<StoreError> for CoreError {
    fn from(e: StoreError) -> Self {
        CoreError::Store(e.0)
    }
}

impl From<ClientError> for CoreError {
    fn from(e: ClientError) -> Self {
        match &e {
            ClientError::Register(RegisterError::Rejected { .. }) | ClientError::Init(_) | ClientError::Ctrl(_) => {
                CoreError::Console(e.to_string())
            }
            _ => CoreError::Network(e.to_string()),
        }
    }
}

/// PSN sign-in as the core needs it; implemented over `openps_psn::PsnClient` (or a fake).
pub trait PsnService: Send + Sync {
    fn authorize_url(&self) -> Result<String, String>;
    /// Finish the login from the URL the web view was redirected to.
    fn complete(&self, redirect_url: &str) -> Result<PsnAccountInfo, String>;
    fn sign_out(&self);
    /// Is this the sign-in redirect (so the shell should hand it to `complete` and stop the navigation)?
    fn is_redirect(&self, _url: &str) -> bool {
        false
    }
}

/// A running (or starting) session. Registered under the `active` lock BEFORE its thread starts, so a session
/// that fails instantly cannot clear an entry that was not stored yet.
struct Active {
    /// Available once the session exists (after the control channel and sockets are open).
    commander: Option<Commander>,
    control: Sender<ControlCommand>,
    stop_control: Arc<AtomicBool>,
}

struct Inner {
    store: Mutex<ConsoleStore>,
    secrets: Arc<dyn SecretStore + Send + Sync>,
    backend: Arc<dyn Backend>,
    psn: Option<Arc<dyn PsnService>>,
    events: Arc<dyn EventSink>,
    media: Arc<dyn MediaSink>,
    state: Mutex<SessionState>,
    active: Mutex<Option<Active>>,
    driver: Mutex<Option<JoinHandle<()>>>,
    /// Timing knobs; tests shorten them.
    settle_s: f64,
    senkusha: SenkushaConfig,
}

#[derive(Clone)]
pub struct Core {
    inner: Arc<Inner>,
}

pub struct CoreBuilder {
    pub store: ConsoleStore,
    pub secrets: Arc<dyn SecretStore + Send + Sync>,
    pub backend: Arc<dyn Backend>,
    pub psn: Option<Arc<dyn PsnService>>,
    pub events: Arc<dyn EventSink>,
    pub media: Arc<dyn MediaSink>,
    pub settle_s: f64,
    pub senkusha: SenkushaConfig,
}

fn rand16() -> [u8; 16] {
    let mut b = [0u8; 16];
    OsRng.fill_bytes(&mut b);
    b
}

impl Core {
    pub fn new(b: CoreBuilder) -> Self {
        Core {
            inner: Arc::new(Inner {
                store: Mutex::new(b.store),
                secrets: b.secrets,
                backend: b.backend,
                psn: b.psn,
                events: b.events,
                media: b.media,
                state: Mutex::new(SessionState::Idle),
                active: Mutex::new(None),
                driver: Mutex::new(None),
                settle_s: b.settle_s,
                senkusha: b.senkusha,
            }),
        }
    }

    pub fn session_state(&self) -> SessionState {
        self.inner.state.lock_ok().clone()
    }

    fn set_state(&self, s: SessionState) {
        *self.inner.state.lock_ok() = s.clone();
        self.inner.events.emit(AppEvent::Session(s));
    }

    // ---- consoles ----------------------------------------------------------------------------------------------------
    /// The user's saved stream settings, if they ever changed them.
    pub fn settings(&self) -> Option<StreamOptions> {
        self.inner.store.lock_ok().settings().cloned()
    }

    pub fn set_settings(&self, o: StreamOptions) -> Result<(), CoreError> {
        o.validate().map_err(CoreError::Console)?;
        self.inner.store.lock_ok().set_settings(o).map_err(|e| CoreError::Store(e.to_string()))
    }

    pub fn consoles(&self) -> Vec<ConsoleRecord> {
        self.inner.store.lock_ok().consoles().to_vec()
    }

    pub fn forget(&self, id: &str) -> Result<(), CoreError> {
        let removed = self.inner.store.lock_ok().remove(id)?;
        if !removed {
            return Err(CoreError::UnknownConsole);
        }
        ConsoleStore::delete_secrets(&*self.inner.secrets, id)?;
        self.inner.events.emit(AppEvent::ConsolesChanged);
        Ok(())
    }

    /// Search the network and mark consoles that are already paired. Updates stored addresses.
    pub fn scan(&self, targets: &[IpAddr]) -> Result<Vec<DiscoveredConsole>, CoreError> {
        let found = self.inner.backend.scan(targets).map_err(|e| CoreError::Network(e.to_string()))?;
        let mut store = self.inner.store.lock_ok();
        let mut out = vec![];
        let mut changed = false;
        for c in found {
            let id = normalise_id(&c.host_id);
            let known = store.get(&id).is_some();
            if known {
                store.set_host(&id, &c.addr.to_string())?;
                changed = true;
            }
            out.push(DiscoveredConsole {
                host_id: c.host_id,
                name: clean_text(&c.host_name, MAX_NAME_CHARS),
                device: c.device.into(),
                status: status_text(c.status).into(),
                addr: c.addr.to_string(),
                registered_id: known.then_some(id),
            });
        }
        drop(store);
        if changed {
            self.inner.events.emit(AppEvent::ConsolesChanged);
        }
        Ok(out)
    }

    /// Wake a paired console and wait for it. `Ok(true)` = it is up.
    pub fn wake(&self, id: &str) -> Result<bool, CoreError> {
        let (rec, secrets) = self.load(id)?;
        let host = rec.host.clone().ok_or(CoreError::NoAddress)?;
        let ip: IpAddr = host.parse().map_err(|_| CoreError::NoAddress)?;
        self.inner.backend.wake(rec.device.into(), ip, &secrets.regist_key).map_err(CoreError::Network)
    }

    fn load(&self, id: &str) -> Result<(ConsoleRecord, ConsoleSecrets), CoreError> {
        let rec = self.inner.store.lock_ok().get(id).cloned().ok_or(CoreError::UnknownConsole)?;
        let secrets = ConsoleStore::get_secrets(&*self.inner.secrets, &rec.id)
            .ok_or_else(|| CoreError::Store("stored registration keys are missing; pair the console again".into()))?;
        Ok((rec, secrets))
    }

    // ---- PSN --------------------------------------------------------------------------------------------------------------
    pub fn psn_account(&self) -> Option<PsnAccountInfo> {
        self.inner.store.lock_ok().psn_account().cloned()
    }

    pub fn psn_authorize_url(&self) -> Result<String, CoreError> {
        self.inner.psn.as_ref().ok_or(CoreError::PsnNotConfigured)?.authorize_url().map_err(CoreError::Network)
    }

    /// Whether `url` is the PSN sign-in redirect. Anything else, even a URL containing `code=`, is ordinary navigation.
    pub fn psn_is_redirect(&self, url: &str) -> bool {
        self.inner.psn.as_ref().is_some_and(|p| p.is_redirect(url))
    }

    pub fn psn_complete(&self, redirect_url: &str) -> Result<PsnAccountInfo, CoreError> {
        let acct = self
            .inner
            .psn
            .as_ref()
            .ok_or(CoreError::PsnNotConfigured)?
            .complete(redirect_url)
            .map_err(CoreError::Console)?;
        self.inner.store.lock_ok().set_psn_account(Some(acct.clone()))?;
        self.inner.events.emit(AppEvent::PsnChanged { signed_in: true });
        Ok(acct)
    }

    pub fn psn_sign_out(&self) -> Result<(), CoreError> {
        if let Some(p) = &self.inner.psn {
            p.sign_out();
        }
        self.inner.store.lock_ok().set_psn_account(None)?;
        self.inner.events.emit(AppEvent::PsnChanged { signed_in: false });
        Ok(())
    }

    // ---- pairing --------------------------------------------------------------------------------------------------------------
    /// Pair with the console at `host` using the PIN it shows. PS5 (and PS4 firmware 7+) need the PSN account id.
    pub fn register(
        &self,
        host: &str,
        device: Device,
        pin: &str,
        version: Option<&str>,
    ) -> Result<ConsoleRecord, CoreError> {
        let pin_num = parse_pin(pin).map_err(CoreError::Console)?;
        validate_host(host).map_err(CoreError::Console)?;
        if let Some(v) = version {
            validate_version(v).map_err(CoreError::Console)?;
        }
        let version = version.unwrap_or(device.default_version()).to_string();
        let account = self.psn_account();
        let (account_id, online_id) = match (&account, version.as_str()) {
            (_, "8.0") => (0, account.as_ref().map(|a| a.online_id.clone()).ok_or(CoreError::NeedsPsnLogin)?),
            (Some(a), _) => (
                a.account_id.parse::<u64>().map_err(|_| CoreError::Store("stored PSN account id is invalid".into()))?,
                a.online_id.clone(),
            ),
            (None, _) => return Err(CoreError::NeedsPsnLogin),
        };
        let params = RegisterParams {
            version: &version,
            device: device.into(),
            pin: pin_num,
            account_id,
            online_id: &online_id,
        };
        let reg: Registered = self.inner.backend.register(host, &params)?;
        let rec = ConsoleRecord {
            id: normalise_id(&reg.mac),
            device: reg.device.into(),
            nickname: clean_text(&reg.nickname, MAX_NAME_CHARS),
            ap_name: clean_text(&reg.ap_name, MAX_NAME_CHARS),
            version,
            host: Some(host.to_string()),
        };
        ConsoleStore::put_secrets(
            &*self.inner.secrets,
            &rec.id,
            &ConsoleSecrets { regist_key: reg.regist_key, rp_key: reg.rp_key },
        )?;
        self.inner.store.lock_ok().upsert(rec.clone())?;
        self.inner.events.emit(AppEvent::ConsolesChanged);
        Ok(rec)
    }

    // ---- sessions ---------------------------------------------------------------------------------------------------------------
    /// Start streaming from a paired console. Returns immediately; progress arrives as [`AppEvent::Session`].
    pub fn connect(&self, id: &str, opts: StreamOptions) -> Result<(), CoreError> {
        opts.validate().map_err(CoreError::Console)?;
        let mut active = self.inner.active.lock_ok();
        if active.is_some() {
            return Err(CoreError::AlreadyConnected);
        }
        let (rec, secrets) = self.load(id)?;
        let host = rec.host.clone().ok_or(CoreError::NoAddress)?;
        validate_host(&host).map_err(CoreError::Console)?; // the state file could have been edited by hand
        self.set_state(SessionState::Connecting);

        let (ctl_tx, ctl_rx) = channel::<ControlCommand>();
        let stop_control = Arc::new(AtomicBool::new(false));
        // register first: the driver's teardown takes this entry, and it needs the `active` lock we still hold
        *active = Some(Active { commander: None, control: ctl_tx, stop_control: stop_control.clone() });
        let core = self.clone();
        match std::thread::Builder::new().name("openps-session-driver".into()).spawn(move || {
            // Anything that panics on this thread (a bug in a decoder or renderer, say) must still end the session
            // visibly; otherwise the UI shows "connected" forever and every later connect is refused.
            let guard = core.clone();
            let run = std::panic::AssertUnwindSafe(|| core.drive(rec, secrets, host, opts, ctl_rx, stop_control));
            if let Err(p) = std::panic::catch_unwind(run) {
                guard.abort_after_panic(&panic_text(&p));
            }
        }) {
            Ok(t) => {
                *self.inner.driver.lock_ok() = Some(t);
                Ok(())
            }
            Err(e) => {
                *active = None;
                drop(active);
                self.set_state(SessionState::Idle);
                Err(CoreError::Network(e.to_string()))
            }
        }
    }

    /// Last-resort cleanup after a panic on the session thread: report the failure, stop whatever is still running and
    /// free the session slot so the user can connect again.
    fn abort_after_panic(&self, why: &str) {
        log::error!("session thread panicked: {why}");
        if let Some(a) = self.inner.active.lock_ok().take() {
            a.stop_control.store(true, Ordering::Relaxed);
            if let Some(c) = a.commander {
                c.disconnect(); // tells the console goodbye and lets the session threads finish
            }
        }
        self.set_state(SessionState::Failed { reason: "internal error while handling the stream".into() });
    }

    /// The session thread: sets up, runs and tears down one session, reporting every step.
    fn drive(
        &self,
        rec: ConsoleRecord,
        secrets: ConsoleSecrets,
        host: String,
        opts: StreamOptions,
        ctl_rx: std::sync::mpsc::Receiver<ControlCommand>,
        stop_control: Arc<AtomicBool>,
    ) {
        let end = |core: &Core, s: SessionState| {
            core.set_state(s);
            if let Some(a) = core.inner.active.lock_ok().take() {
                a.stop_control.store(true, Ordering::Relaxed);
            }
        };
        let device: DeviceType = rec.device.into();
        let reg = Registered {
            device,
            mac: rec.id.clone(),
            nickname: rec.nickname.clone(),
            regist_key: secrets.regist_key,
            rp_key: secrets.rp_key,
            ap_name: rec.ap_name.clone(),
        };
        let streaming = match (rec.device, opts.hevc, opts.hdr) {
            (Device::Ps5, true, true) => StreamingType::HevcHdr,
            (Device::Ps5, true, false) => StreamingType::HevcSdr,
            _ => StreamingType::Avc,
        };
        let control = match self.inner.backend.open_control(&host, device, &rec.version, &reg, streaming) {
            Ok(c) => c,
            Err(e) => return end(self, SessionState::Failed { reason: CoreError::from(e).to_string() }),
        };
        if control.info().prohibited {
            return end(self, SessionState::Failed { reason: "the console currently does not allow streaming".into() });
        }
        let (session_id, cipher, version_used) = (control.session_id(), control.cipher(), control.version());
        if version_used != rec.version {
            let _ = self.inner.store.lock_ok().upsert(ConsoleRecord { version: version_used, ..rec.clone() });
            // remember what worked
        }
        let (stream_link, test_link) = match self.inner.backend.open_links(&host) {
            Ok(l) => l,
            Err(e) => return end(self, SessionState::Failed { reason: format!("cannot open network sockets: {e}") }),
        };

        let ps5 = rec.device == Device::Ps5;
        let (o, sid) = (opts.clone(), session_id);
        let mut params = ConnectParams::new(
            move |info: &NetworkInfo| HandshakeConfig {
                ps5,
                request_takion20: false,
                session_key: sid.clone(),
                mtu_out: info.mtu_out as usize,
                launch: LaunchParams {
                    width: o.width,
                    height: o.height,
                    fps: o.fps,
                    bitrate: o.bitrate,
                    mtu_in: info.mtu_in,
                    rtt_ms: info.rtt_ms,
                    ps5,
                    hevc: o.hevc,
                    hdr: o.hdr,
                },
                handshake_key: rand16(),
                session_cipher: cipher.clone(), // every attempt needs its own copy
                ecdh_secret: None,
                resend_interval_s: RESEND_INTERVAL_S,
                max_resends: MAX_RESENDS,
            },
            StreamConfig { ps5, ..Default::default() },
        );
        params.network_test = Some(NetworkTest {
            link: test_link,
            config: SenkushaConfig { ps5, ..self.inner.senkusha.clone() },
            settle_s: self.inner.settle_s,
        });
        let handle = start_session(stream_link, params);
        if let Some(a) = self.inner.active.lock_ok().as_mut() {
            a.commander = Some(handle.commander());
        }

        // control channel worker
        let events = self.inner.events.clone();
        let ctl = std::thread::Builder::new().name("openps-control".into()).spawn({
            let stop = stop_control.clone();
            move || {
                if let Some(e) = control::run_worker(control, ctl_rx, stop, events.clone()) {
                    events.emit(AppEvent::Control { kind: "control_closed".into(), detail: e.to_string() });
                }
            }
        });

        let mut counters = Counters::new();
        let commander = handle.commander(); // once, not per notice: video arrives at frame rate
        let terminal = loop {
            let mut next = handle.recv_timeout(Duration::from_millis(100));
            if next.is_none() && handle.has_stopped() {
                // A session thread exited. Normally its final notice is still queued; if not, it died unexpectedly and
                // waiting longer would hang the UI on "connected" (and spin, as the closed channel returns at once).
                match handle.try_recv() {
                    Some(n) => next = Some(n),
                    None => break SessionState::Failed { reason: "the session stopped unexpectedly".into() },
                }
            }
            if let Some(n) = next {
                // the media sinks (decode, render, audio) run inside this call: contain their panics
                let step = std::panic::AssertUnwindSafe(|| self.on_notice(n, &commander, &mut counters));
                match std::panic::catch_unwind(step) {
                    Ok(Some(t)) => break t,
                    Ok(None) => {}
                    Err(p) => {
                        log::error!("media handling panicked: {}", panic_text(&p));
                        commander.disconnect();
                        break SessionState::Failed { reason: "internal error while handling the stream".into() };
                    }
                }
            }
            counters.maybe_emit(&*self.inner.events, &handle);
        };
        handle.join();
        stop_control.store(true, Ordering::Relaxed);
        if let Ok(t) = ctl {
            let _ = t.join();
        }
        end(self, terminal);
    }

    /// Map one session notice to state/media. Returns the final state when the session is over.
    fn on_notice(&self, n: Notice, commander: &Commander, counters: &mut Counters) -> Option<SessionState> {
        match n {
            Notice::NetworkTested(_) => self.set_state(SessionState::TestingNetwork),
            Notice::Handshaking { attempt } => self.set_state(SessionState::Handshaking { attempt }),
            Notice::Connected { takion_version, stream_info } => {
                self.inner.media.stream_info(&stream_info);
                self.set_state(SessionState::Connected { takion_version });
                commander.send(openps_session::Command::PadConnected { pad: 0, connected: true });
                // announce the session pad
            }
            Notice::Event(e) => match e {
                Event::Video(f) => {
                    counters.frames += 1;
                    counters.bytes += f.data.len() as u64;
                    self.inner.media.video(f);
                    if self.inner.media.take_keyframe_request() {
                        commander.request_idr();
                    }
                }
                Event::Audio(a) => self.inner.media.audio(a),
                Event::Haptic(h) => self.inner.media.haptic(h),
                Event::PadOutput(p) => self.inner.media.pad_output(p),
                Event::Message(_) | Event::Disconnected(_) => {}
            },
            Notice::Failed(reason) => return Some(SessionState::Failed { reason }),
            Notice::Closed(reason) => return Some(SessionState::Closed { reason }),
        }
        None
    }

    /// The running session's command port; `NotConnected` until the session exists.
    fn commander(&self) -> Result<Commander, CoreError> {
        self.inner.active.lock_ok().as_ref().and_then(|a| a.commander.clone()).ok_or(CoreError::NotConnected)
    }

    pub fn set_pad(&self, state: openps_input::state::ControllerState) -> Result<(), CoreError> {
        self.commander()?.set_pad(0, state);
        Ok(())
    }

    pub fn request_keyframe(&self) -> Result<(), CoreError> {
        self.commander()?.request_idr();
        Ok(())
    }

    pub fn control(&self, cmd: ControlCommand) -> Result<(), CoreError> {
        let g = self.inner.active.lock_ok();
        g.as_ref().ok_or(CoreError::NotConnected)?.control.send(cmd).map_err(|_| CoreError::NotConnected)
    }

    pub fn disconnect(&self) -> Result<(), CoreError> {
        self.commander()?.disconnect();
        Ok(())
    }

    /// Wait for the session thread to finish (after `disconnect`, or at app exit). Returns false on timeout.
    pub fn wait_idle(&self, timeout: Duration) -> bool {
        let end = Instant::now() + timeout;
        loop {
            let done = self.inner.active.lock_ok().is_none();
            if done {
                return true;
            }
            if Instant::now() >= end {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Stop everything and join the session thread.
    pub fn shutdown(&self) {
        if self.disconnect().is_err() {
            // no commander yet: the session is still setting up; stop its control worker so it winds down
            if let Some(a) = self.inner.active.lock_ok().as_ref() {
                a.stop_control.store(true, Ordering::Relaxed);
            }
        }
        self.wait_idle(Duration::from_secs(5));
        if let Some(t) = self.inner.driver.lock_ok().take() {
            let _ = t.join();
        }
    }
}

struct Counters {
    frames: u64,
    bytes: u64,
    since: Instant,
}

impl Counters {
    fn new() -> Self {
        Self { frames: 0, bytes: 0, since: Instant::now() }
    }

    fn maybe_emit(&mut self, events: &dyn EventSink, handle: &openps_session::Handle) {
        let dt = self.since.elapsed().as_secs_f64();
        if dt < 1.0 {
            return;
        }
        let s = handle.stats();
        events.emit(AppEvent::Stats(StreamStats {
            fps: self.frames as f64 / dt,
            kbps: self.bytes as f64 * 8.0 / 1000.0 / dt,
            video_frames_dropped: s.video_frames_dropped,
            datagrams_in: s.datagrams_in,
        }));
        *self = Counters::new();
    }
}

/// Best-effort text of a panic payload.
fn panic_text(p: &Box<dyn std::any::Any + Send>) -> String {
    p.downcast_ref::<&str>()
        .map(|s| (*s).to_string())
        .or_else(|| p.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".into())
}
