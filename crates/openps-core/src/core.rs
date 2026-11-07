//! The application core: everything the UI can ask for, with no UI dependency.

use crate::backend::Backend;
use crate::capture::VideoCapture;
use crate::control::{self, ControlCommand};
use crate::events::{AppEvent, EventSink, MediaSink, StreamStats};
use crate::picture;
use crate::store::{ConsoleStore, StoreError};
use crate::sync::LockExt;
use crate::types::*;
use openps_discovery::DeviceType;
use openps_psn::api::PsnConsole;
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
    /// Like [`sign_out`](Self::sign_out) but reports a failure to delete the stored login. Implementors that can fail
    /// should override this; the default cannot fail.
    fn try_sign_out(&self) -> Result<(), String> {
        self.sign_out();
        Ok(())
    }
    /// Is this the sign-in redirect (so the shell should hand it to `complete` and stop the navigation)?
    fn is_redirect(&self, _url: &str) -> bool {
        false
    }

    // ---- the PSN API (console list for remote registration, spec 09) ----------------------------------------------------------
    /// Can this service use the PSN API (are the API credentials present)?
    fn api_available(&self) -> bool {
        false
    }
    /// Sign in to the PSN API with what the user pasted (an npsso). The npsso is not kept.
    fn api_sign_in(&self, _pasted: &str) -> Result<(), String> {
        Err("the PSN API is not available".into())
    }
    fn api_signed_in(&self) -> bool {
        false
    }
    fn api_sign_out(&self) {}
    /// The account's PS5 consoles as PSN lists them.
    fn api_consoles(&self) -> Result<Vec<PsnConsole>, String> {
        Err("the PSN API is not available".into())
    }
    /// The account's games with trophy progress, and the overall trophy summary.
    fn api_library(&self) -> Result<openps_psn::library::Library, String> {
        Err("the PSN API is not available".into())
    }
    /// Ask the console to start a title (PSN launch command). Returns the command id.
    fn api_launch(&self, _duid: &str, _title_id: &str) -> Result<String, String> {
        Err("the PSN API is not available".into())
    }
    fn api_command_status(&self, _command_id: &str) -> Result<openps_psn::library::CommandStatus, String> {
        Err("the PSN API is not available".into())
    }
    /// One cover image (bytes, MIME type). Only Sony's image hosts are allowed.
    fn api_art(&self, _url: &str) -> Result<(Vec<u8>, String), String> {
        Err("the PSN API is not available".into())
    }
    /// A valid access token of the Remote Play login (needed to register over the internet).
    fn remote_access_token(&self) -> Result<String, String> {
        Err("sign in to PlayStation first".into())
    }
}

/// What a remote registration needs from the application.
pub struct RemoteRequest {
    pub access_token: String,
    pub account_id: u64,
    /// The console's id in PSN (from the console list).
    pub duid: String,
}

/// Registers a console over the internet through PSN (spec 09). Implemented where the key tables and the network clients are available.
pub trait RemoteRegistrar: Send + Sync {
    /// Blocks until done. `progress` receives short plain-language steps; `cancel` is polled.
    fn register(
        &self,
        req: RemoteRequest,
        cancel: Arc<AtomicBool>,
        progress: &dyn Fn(&str),
    ) -> Result<Registered, String>;
}

/// A running (or starting) session. Registered under the `active` lock BEFORE its thread starts, so a session
/// that fails instantly cannot clear an entry that was not stored yet.
struct Active {
    /// Normalised id of the console this session talks to.
    console: String,
    /// Available once the session exists (after the control channel and sockets are open).
    commander: Option<Commander>,
    control: Sender<ControlCommand>,
    stop_control: Arc<AtomicBool>,
    /// The user (or the app exit) asked to end this session before it had a commander. Survives the Auto-bitrate retry.
    cancel: Arc<AtomicBool>,
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
    /// What Auto bitrate may ask for, given the console's address. Replaceable so tests do not depend on this machine's network.
    auto_probe: Mutex<AutoProbe>,
    remote: Mutex<Option<Arc<dyn RemoteRegistrar>>>,
    /// Consoles (by address) that failed while Auto asked for a raised bitrate: they get the default from then on, until restart.
    auto_failed: Mutex<std::collections::HashSet<String>>,
    remote_cancel: Mutex<Option<Arc<AtomicBool>>>,
    /// How long a connected session may show no picture before it is failed.
    first_picture_timeout: Mutex<Duration>,
}

/// `host -> kbit/s to request`, or `None` for the resolution default.
type AutoProbe = Arc<dyn Fn(&str) -> Option<i32> + Send + Sync>;

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
                auto_probe: Mutex::new(Arc::new(crate::netlink::auto_kbps_for)),
                remote: Mutex::new(None),
                auto_failed: Mutex::new(Default::default()),
                remote_cancel: Mutex::new(None),
                first_picture_timeout: Mutex::new(picture::GIVE_UP_AFTER),
            }),
        }
    }

    /// Replace how Auto bitrate decides what to ask for (tests; embedders with better knowledge of the network).
    pub fn set_auto_bitrate_probe(&self, f: impl Fn(&str) -> Option<i32> + Send + Sync + 'static) {
        *self.inner.auto_probe.lock_ok() = Arc::new(f);
    }

    /// How long a connected session may show no picture before it fails (tests shorten it).
    pub fn set_first_picture_timeout(&self, t: Duration) {
        *self.inner.first_picture_timeout.lock_ok() = t;
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
        self.inner.store.lock_ok().settings().cloned().map(StreamOptions::migrated)
    }

    pub fn set_settings(&self, o: StreamOptions) -> Result<(), CoreError> {
        o.validate().map_err(CoreError::Console)?;
        let o = o.migrated();
        self.inner.store.lock_ok().set_settings(o).map_err(|e| CoreError::Store(e.to_string()))
    }

    pub fn consoles(&self) -> Vec<ConsoleRecord> {
        self.inner.store.lock_ok().consoles().to_vec()
    }

    pub fn forget(&self, id: &str) -> Result<(), CoreError> {
        let old = self.inner.store.lock_ok().get(id).cloned().ok_or(CoreError::UnknownConsole)?;
        if !self.inner.store.lock_ok().remove(id)? {
            return Err(CoreError::UnknownConsole);
        }
        if let Err(e) = ConsoleStore::delete_secrets(&*self.inner.secrets, id) {
            // keep the console (and its keys) rather than leave keys nobody can reach
            let _ = self.inner.store.lock_ok().upsert(old);
            return Err(e.into());
        }
        self.inner.events.emit(AppEvent::ConsolesChanged);
        Ok(())
    }

    /// Search the network and mark consoles that are already paired. Updates stored addresses.
    pub fn scan(&self, targets: &[IpAddr]) -> Result<Vec<DiscoveredConsole>, CoreError> {
        let found = self.inner.backend.scan(targets).map_err(|e| CoreError::Network(e.to_string()))?;
        let mut out = vec![];
        let mut updates = vec![];
        let store = self.inner.store.lock_ok();
        for c in found {
            let id = normalise_id(&c.host_id);
            let known = store.get(&id).is_some();
            if known {
                updates.push((id.clone(), c.addr.to_string()));
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
        let changed = !updates.is_empty();
        if changed {
            self.inner.store.lock_ok().set_hosts(&updates)?;
            self.inner.events.emit(AppEvent::ConsolesChanged);
        }
        Ok(out)
    }

    /// Wake a paired console and wait for it. `Ok(true)` = it is up.
    pub fn wake(&self, id: &str) -> Result<bool, CoreError> {
        let (rec, secrets) = self.load(id)?;
        let host = rec.host.clone().ok_or(CoreError::NoAddress)?;
        let ip: IpAddr = canonical_host(&host).parse().map_err(|_| CoreError::NoAddress)?;
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

    fn psn_service(&self) -> Result<&Arc<dyn PsnService>, CoreError> {
        self.inner.psn.as_ref().ok_or(CoreError::PsnNotConfigured)
    }

    /// Whether the PSN API (console list) can be used, and whether the user is signed in to it.
    pub fn psn_api_status(&self) -> (bool, bool) {
        self.inner.psn.as_ref().map_or((false, false), |p| (p.api_available(), p.api_signed_in()))
    }

    pub fn psn_api_sign_in(&self, pasted: &str) -> Result<(), CoreError> {
        self.psn_service()?.api_sign_in(pasted).map_err(CoreError::Console)?;
        // tell the page: the games shelf and the console list depend on this login, however it was started
        self.inner.events.emit(AppEvent::PsnChanged { signed_in: self.inner.store.lock_ok().psn_account().is_some() });
        Ok(())
    }

    pub fn psn_library(&self) -> Result<openps_psn::library::Library, CoreError> {
        self.psn_service()?.api_library().map_err(CoreError::Console)
    }

    pub fn psn_launch(&self, duid: &str, title_id: &str) -> Result<String, CoreError> {
        self.psn_service()?.api_launch(duid, title_id).map_err(CoreError::Console)
    }

    pub fn psn_command_status(&self, command_id: &str) -> Result<openps_psn::library::CommandStatus, CoreError> {
        self.psn_service()?.api_command_status(command_id).map_err(CoreError::Console)
    }

    pub fn psn_art(&self, url: &str) -> Result<(Vec<u8>, String), CoreError> {
        self.psn_service()?.api_art(url).map_err(CoreError::Console)
    }

    pub fn psn_api_sign_out(&self) {
        if let Some(p) = &self.inner.psn {
            p.api_sign_out();
        }
    }

    pub fn psn_consoles(&self) -> Result<Vec<PsnConsole>, CoreError> {
        self.psn_service()?.api_consoles().map_err(CoreError::Console)
    }

    pub fn psn_sign_out(&self) -> Result<(), CoreError> {
        if let Some(p) = &self.inner.psn {
            let result = p.try_sign_out();
            p.api_sign_out();
            if let Err(e) = result {
                // the stored login is still there: keep the account shown so the UI does not claim a sign-out that failed
                return Err(CoreError::Console(format!("could not remove the saved PlayStation login: {e}")));
            }
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
        log::info!(
            "registering {:?} at {host} (protocol {version}, PSN account {})",
            device,
            if account_id != 0 { "present" } else { "absent" }
        );
        let reg: Registered = self.inner.backend.register(host, &params).map_err(|e| {
            log::warn!("registration at {host} failed: {e}");
            e
        })?;
        log::info!("registered {:?} ({})", reg.device, reg.mac);
        self.save_registration(reg, version, Some(canonical_host(host)))
    }

    /// Keep a registration: the record in the state file, the keys in the secret store.
    fn save_registration(
        &self,
        reg: Registered,
        version: String,
        host: Option<String>,
    ) -> Result<ConsoleRecord, CoreError> {
        let id = normalise_id(&reg.mac);
        if id.len() != 12 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(CoreError::Console("the console did not report a usable id".into()));
        }
        let rec = ConsoleRecord {
            id,
            device: reg.device.into(),
            nickname: clean_text(&reg.nickname, MAX_NAME_CHARS),
            ap_name: clean_text(&reg.ap_name, MAX_NAME_CHARS),
            version,
            host,
        };
        let keys = ConsoleSecrets { regist_key: reg.regist_key, rp_key: reg.rp_key };
        self.inner.store.lock_ok().upsert_with_secrets(rec.clone(), &*self.inner.secrets, &keys)?;
        self.inner.events.emit(AppEvent::ConsolesChanged);
        Ok(rec)
    }

    // ---- registration over the internet (spec 09) ---------------------------------------------------------------------------------
    /// Install what registers consoles over the internet (needs the key tables and the live network clients).
    pub fn set_remote_registrar(&self, r: Arc<dyn RemoteRegistrar>) {
        *self.inner.remote.lock_ok() = Some(r);
    }

    pub fn remote_available(&self) -> bool {
        self.inner.remote.lock_ok().is_some()
    }

    /// Register the PS5 with PSN id `duid` over the internet. Blocks; progress arrives as `remote_step` control events. Only one runs at
    /// a time.
    pub fn register_remote(&self, duid: &str) -> Result<ConsoleRecord, CoreError> {
        let registrar =
            self.inner.remote.lock_ok().clone().ok_or_else(|| {
                CoreError::Console("registering over the internet is not available in this build".into())
            })?;
        if duid.is_empty() || duid.len() > 128 || !duid.is_ascii() || duid.bytes().any(|b| !b.is_ascii_graphic()) {
            return Err(CoreError::Console("that is not a console id".into()));
        }
        let psn = self.psn_service()?.clone();
        let account = self.psn_account().ok_or(CoreError::NeedsPsnLogin)?;
        let account_id = account
            .account_id
            .parse::<u64>()
            .map_err(|_| CoreError::Store("stored PSN account id is invalid".into()))?;
        let token = psn.remote_access_token().map_err(CoreError::Console)?;
        let cancel = Arc::new(AtomicBool::new(false));
        {
            let mut slot = self.inner.remote_cancel.lock_ok();
            if slot.is_some() {
                return Err(CoreError::Console("a registration is already running".into()));
            }
            *slot = Some(cancel.clone());
        }
        let events = self.inner.events.clone();
        let progress =
            move |step: &str| events.emit(AppEvent::Control { kind: "remote_step".into(), detail: step.into() });
        log::info!("registering over the internet (PSN account present)");
        // frees the slot even if the registrar panics
        struct Slot<'a>(&'a Mutex<Option<Arc<AtomicBool>>>);
        impl Drop for Slot<'_> {
            fn drop(&mut self) {
                *self.0.lock_ok() = None;
            }
        }
        let result = {
            let _slot = Slot(&self.inner.remote_cancel);
            registrar.register(
                RemoteRequest { access_token: token, account_id, duid: duid.to_string() },
                cancel,
                &progress,
            )
        };
        let reg = result.map_err(|e| {
            log::warn!("registration over the internet failed: {e}");
            CoreError::Console(e)
        })?;
        log::info!("registered over the internet: {:?} ({})", reg.device, reg.mac);
        self.save_registration(reg, "1.0".into(), None)
    }

    /// Stop a running remote registration.
    pub fn cancel_remote(&self) {
        if let Some(c) = self.inner.remote_cancel.lock_ok().as_ref() {
            c.store(true, Ordering::Relaxed);
        }
    }

    // ---- sessions ---------------------------------------------------------------------------------------------------------------
    /// Start streaming from a paired console. Returns immediately; progress arrives as [`AppEvent::Session`].
    /// Whether PSN sign-in is available (credentials loaded, or the demo fake).
    pub fn has_psn(&self) -> bool {
        self.inner.psn.is_some()
    }

    /// The console the running session belongs to (None when idle).
    pub fn active_console(&self) -> Option<String> {
        self.inner.active.lock_ok().as_ref().map(|a| a.console.clone())
    }

    pub fn connect(&self, id: &str, opts: StreamOptions) -> Result<(), CoreError> {
        opts.validate().map_err(CoreError::Console)?;
        if self.inner.active.lock_ok().is_some() {
            return Err(CoreError::AlreadyConnected);
        }
        // may touch the Keychain: not under the `active` lock
        let (rec, secrets) = self.load(id)?;
        let host = canonical_host(rec.host.as_deref().ok_or(CoreError::NoAddress)?);
        validate_host(&host).map_err(CoreError::Console)?; // the state file could have been edited by hand
        if validate_version(&rec.version).is_err() {
            return Err(CoreError::Console("the stored protocol version is not valid; pair the console again".into()));
        }

        let (ctl_tx, ctl_rx) = channel::<ControlCommand>();
        let stop_control = Arc::new(AtomicBool::new(false));
        let cancel = Arc::new(AtomicBool::new(false));
        {
            // register first: the driver's teardown takes this entry. Only the slot is touched under the lock.
            let mut active = self.inner.active.lock_ok();
            if active.is_some() {
                return Err(CoreError::AlreadyConnected);
            }
            *active = Some(Active {
                console: rec.id.clone(),
                commander: None,
                control: ctl_tx,
                stop_control: stop_control.clone(),
                cancel: cancel.clone(),
            });
        }
        // no lock is held while the sink runs; the driver has not started yet, so `Connecting` is always the first state
        self.set_state(SessionState::Connecting);
        let core = self.clone();
        match std::thread::Builder::new().name("openps-session-driver".into()).spawn(move || {
            // Anything that panics on this thread (a bug in a decoder or renderer, say) must still end the session
            // visibly; otherwise the UI shows "connected" forever and every later connect is refused.
            let guard = core.clone();
            let run =
                std::panic::AssertUnwindSafe(|| core.drive(rec, secrets, host, opts, ctl_rx, stop_control, cancel));
            if let Err(p) = std::panic::catch_unwind(run) {
                guard.abort_after_panic(&panic_text(&p));
            }
        }) {
            Ok(t) => {
                *self.inner.driver.lock_ok() = Some(t);
                Ok(())
            }
            Err(e) => {
                *self.inner.active.lock_ok() = None;
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

    /// The session thread. A session that fails BEFORE it connects while Auto asked for a large bitrate is tried once more with the
    /// conservative default: the console's reaction to a very large request is not known, and a failure must not be the user's problem.
    #[allow(clippy::too_many_arguments)]
    fn drive(
        &self,
        rec: ConsoleRecord,
        secrets: ConsoleSecrets,
        host: String,
        opts: StreamOptions,
        ctl_rx: std::sync::mpsc::Receiver<ControlCommand>,
        stop_control: Arc<AtomicBool>,
        cancel: Arc<AtomicBool>,
    ) {
        let auto = if opts.bitrate == -1 && !self.inner.auto_failed.lock_ok().contains(&host) {
            let probe = self.inner.auto_probe.lock_ok().clone();
            probe(&host)
        } else {
            None
        };
        let (mut terminal, up) = self.drive_once(
            rec.clone(),
            secrets.clone(),
            host.clone(),
            opts.clone(),
            ctl_rx,
            &stop_control,
            &cancel,
            auto,
        );
        let decoder_failed = matches!(&terminal, SessionState::Failed { reason } if reason == picture::DECODER_REASON);
        if decoder_failed && (opts.hevc || opts.hdr) {
            // The decoder could not show HEVC (or HDR): H.264 is the one format every Mac decodes.
            log::warn!("{}; retrying with H.264", picture::DECODER_REASON);
            let h264 = StreamOptions { hevc: false, hdr: false, ..opts };
            terminal = self.restart(rec, secrets, host, h264, &stop_control, &cancel, auto);
        } else if let (Some(why), Some(kbps)) = (needs_default_bitrate(&terminal, up), auto) {
            let detail = match &terminal {
                SessionState::Failed { reason } => reason.clone(),
                SessionState::Closed { reason } => reason.clone().unwrap_or_default(),
                _ => String::new(),
            };
            log::warn!("{why} ({detail}) while asking for {kbps} kbit/s; retrying with the default bitrate and not raising it for this console again until OpenPS restarts");
            self.inner.auto_failed.lock_ok().insert(host.clone());
            terminal = self.restart(rec, secrets, host, opts, &stop_control, &cancel, None);
        }
        // free the slot BEFORE announcing the end: a handler that reacts by connecting again must be allowed to
        if let Some(a) = self.inner.active.lock_ok().take() {
            a.stop_control.store(true, Ordering::Relaxed);
        }
        self.set_state(terminal);
    }

    /// Another attempt after one ended: waits for the console to let go of the session that just ended (a cancel request ends the wait
    /// and closes quietly), then runs again with a fresh control channel.
    #[allow(clippy::too_many_arguments)]
    fn restart(
        &self,
        rec: ConsoleRecord,
        secrets: ConsoleSecrets,
        host: String,
        opts: StreamOptions,
        stop_control: &Arc<AtomicBool>,
        cancel: &Arc<AtomicBool>,
        auto_kbps: Option<i32>,
    ) -> SessionState {
        let end = Instant::now() + Duration::from_secs_f64(self.inner.settle_s.clamp(0.0, 60.0));
        while Instant::now() < end && !cancel.load(Ordering::Relaxed) {
            std::thread::sleep((end - Instant::now()).min(Duration::from_millis(20)));
        }
        if cancel.load(Ordering::Relaxed) {
            return SessionState::Closed { reason: None };
        }
        let (tx, rx) = channel::<ControlCommand>();
        if let Some(a) = self.inner.active.lock_ok().as_mut() {
            a.control = tx; // the previous attempt's control channel is gone
            a.commander = None;
        }
        stop_control.store(false, Ordering::Relaxed);
        self.set_state(SessionState::Connecting);
        self.drive_once(rec, secrets, host, opts, rx, stop_control, cancel, auto_kbps).0
    }

    /// One attempt: sets up, runs and tears down a session, reporting every step. Returns how it ended and, if it ever connected, how
    /// long the session ran after that.
    #[allow(clippy::too_many_arguments)]
    fn drive_once(
        &self,
        rec: ConsoleRecord,
        secrets: ConsoleSecrets,
        host: String,
        opts: StreamOptions,
        ctl_rx: std::sync::mpsc::Receiver<ControlCommand>,
        stop_control: &Arc<AtomicBool>,
        cancel: &Arc<AtomicBool>,
        auto_kbps: Option<i32>,
    ) -> (SessionState, Option<Duration>) {
        let end = |_: &Core, s: SessionState| (s, None);
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
        let (o, sid) = (opts.clone().migrated(), session_id); // always 60 fps
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
                    auto_kbps,
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
            StreamConfig { ps5, congestion: sends_congestion(opts.bitrate), ..Default::default() },
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
        if cancel.load(Ordering::Relaxed) {
            handle.commander().disconnect(); // asked to stop while the session was still being set up
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
        let mut capture = VideoCapture::from_env();
        let mut watch = picture::PictureWatch::new(*self.inner.first_picture_timeout.lock_ok());
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
                let step = std::panic::AssertUnwindSafe(|| self.on_notice(n, &commander, &mut counters, &mut capture));
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
            if let Some(up) = counters.connected.map(|t| t.elapsed()) {
                match watch.check(up, self.inner.media.pictures_shown(), counters.video_seen) {
                    picture::Watch::Fine => {}
                    picture::Watch::Slow => {
                        log::warn!("connected {up:?} ago and still no picture (video seen: {})", counters.video_seen);
                    }
                    picture::Watch::GiveUp(reason) => {
                        log::error!("giving up: {reason}");
                        commander.disconnect();
                        break SessionState::Failed { reason: reason.into() };
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
        (terminal, counters.connected.map(|t| t.elapsed()))
    }

    /// Map one session notice to state/media. Returns the final state when the session is over.
    fn on_notice(
        &self,
        n: Notice,
        commander: &Commander,
        counters: &mut Counters,
        capture: &mut Option<VideoCapture>,
    ) -> Option<SessionState> {
        match n {
            Notice::NetworkTested(_) => self.set_state(SessionState::TestingNetwork),
            Notice::Handshaking { attempt } => self.set_state(SessionState::Handshaking { attempt }),
            Notice::Connected { takion_version, stream_info } => {
                counters.connected.get_or_insert_with(Instant::now);
                if let Some(c) = capture {
                    for h in stream_info.resolution.iter().filter_map(|r| r.video_header.as_deref()) {
                        c.parameter_sets(h);
                    }
                }
                self.inner.media.stream_info(&stream_info);
                self.set_state(SessionState::Connected { takion_version });
                commander.send(openps_session::Command::PadConnected { pad: 0, connected: true });
                // announce the session pad
            }
            Notice::Event(e) => match e {
                Event::Video(f) => {
                    counters.video_seen = true;
                    if let Some(c) = capture {
                        c.frame(&f);
                    }
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

    /// State of a physical pad in console slot `pad` (slot 0 is shared with the key map; only one of them writes at a time).
    pub fn set_pad_slot(&self, pad: usize, state: openps_input::state::ControllerState) -> Result<(), CoreError> {
        self.commander()?.set_pad(pad, state);
        Ok(())
    }

    pub fn set_pad_connected(&self, pad: usize, connected: bool) -> Result<(), CoreError> {
        self.commander()?.send(openps_session::Command::PadConnected { pad, connected });
        Ok(())
    }

    pub fn set_pad_sensors(
        &self,
        pad: usize,
        gyro: [f32; 3],
        accel: [f32; 3],
        orientation: [f32; 4],
    ) -> Result<(), CoreError> {
        self.commander()?.send(openps_session::Command::Sensors { pad, gyro, accel, orientation });
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
        let commander = {
            let g = self.inner.active.lock_ok();
            let a = g.as_ref().ok_or(CoreError::NotConnected)?;
            if a.commander.is_none() {
                // still setting up (or between the attempts of the Auto retry): remember the request, the driver honours it
                a.cancel.store(true, Ordering::Relaxed);
                a.stop_control.store(true, Ordering::Relaxed);
            }
            a.commander.clone()
        };
        if let Some(c) = commander {
            c.disconnect();
        }
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

    /// Stop everything. Waits up to 5 s for the session thread; if a backend call is still blocked after that, the thread is
    /// left to finish on its own (it frees the session slot when it does) instead of hanging the app's exit.
    pub fn shutdown(&self) {
        let _ = self.disconnect(); // idle: nothing to do
        if self.wait_idle(Duration::from_secs(5)) {
            if let Some(t) = self.inner.driver.lock_ok().take() {
                let _ = t.join();
            }
        } else {
            drop(self.inner.driver.lock_ok().take()); // detach
        }
    }
}

/// The original app reports congestion to the console only while the bitrate is automatic (-1); with a fixed bitrate the console has
/// nothing to adapt, so the packets are left out.
fn sends_congestion(bitrate: i32) -> bool {
    bitrate == -1
}

/// A session that ends within this long after connecting, on the console's initiative, did not really start.
const EARLY_END: Duration = Duration::from_secs(20);

/// Whether a session that asked for a raised Auto bitrate should be tried again with the default, and why: it failed before connecting,
/// or the console ended it (or it failed) shortly after connecting. A session the user ended (`Closed` without a reason) never qualifies.
fn needs_default_bitrate(terminal: &SessionState, up: Option<Duration>) -> Option<&'static str> {
    match (terminal, up) {
        (SessionState::Failed { .. }, None) => Some("the session failed before connecting"),
        (SessionState::Failed { .. } | SessionState::Closed { reason: Some(_) }, Some(d)) if d < EARLY_END => {
            Some("the console ended the session shortly after it started")
        }
        _ => None,
    }
}

struct Counters {
    /// When the session reached `Connected` (the first time).
    connected: Option<Instant>,
    /// Whether any video frame has arrived this session.
    video_seen: bool,
    frames: u64,
    bytes: u64,
    since: Instant,
}

impl Counters {
    fn new() -> Self {
        Self { connected: None, video_seen: false, frames: 0, bytes: 0, since: Instant::now() }
    }

    /// Start a new one-second sample; what describes the session as a whole stays.
    fn next_sample(&mut self) {
        *self = Self { connected: self.connected, video_seen: self.video_seen, ..Self::new() };
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
            datagrams_out: s.datagrams_out,
        }));
        self.next_sample();
    }
}

/// Best-effort text of a panic payload.
fn panic_text(p: &Box<dyn std::any::Any + Send>) -> String {
    p.downcast_ref::<&str>()
        .map(|s| (*s).to_string())
        .or_else(|| p.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".into())
}

#[cfg(test)]
mod retry_tests {
    use super::*;
    use std::time::Duration;

    fn failed() -> SessionState {
        SessionState::Failed { reason: "x".into() }
    }
    fn closed(r: Option<&str>) -> SessionState {
        SessionState::Closed { reason: r.map(String::from) }
    }

    #[test]
    fn a_raised_bitrate_is_given_up_when_the_session_fails_early_but_not_when_the_user_ends_it() {
        let secs = |n| Some(Duration::from_secs(n));
        assert!(needs_default_bitrate(&failed(), None).is_some(), "never connected");
        assert!(needs_default_bitrate(&failed(), secs(3)).is_some(), "failed 3 s after connecting");
        assert!(
            needs_default_bitrate(&closed(Some("video send timeout")), secs(5)).is_some(),
            "the console ended it early (the real case)"
        );
        assert!(needs_default_bitrate(&closed(Some("video send timeout")), secs(19)).is_some());
        // later is not this problem; and a session the user ended is never retried
        assert!(needs_default_bitrate(&closed(Some("video send timeout")), secs(20)).is_none());
        assert!(needs_default_bitrate(&failed(), secs(600)).is_none());
        assert!(needs_default_bitrate(&closed(None), secs(2)).is_none(), "the user pressed Disconnect");
        assert!(needs_default_bitrate(&closed(None), None).is_none(), "cancelled while connecting");
        assert!(needs_default_bitrate(&SessionState::Idle, secs(1)).is_none());
    }

    #[test]
    fn congestion_is_reported_only_for_an_automatic_bitrate() {
        assert!(sends_congestion(-1));
        assert!(!sends_congestion(10_000));
        assert!(!sends_congestion(0));
    }

    #[test]
    fn a_new_sample_keeps_what_describes_the_whole_session() {
        let mut c = Counters::new();
        c.connected = Some(Instant::now());
        c.video_seen = true;
        c.frames = 60;
        c.bytes = 1000;
        c.next_sample();
        assert!(c.connected.is_some(), "the session is still connected: its uptime must survive the sample");
        assert!(c.video_seen);
        assert_eq!((c.frames, c.bytes), (0, 0));
    }
}
