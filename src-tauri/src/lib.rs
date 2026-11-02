//! OpenPS Tauri shell: a thin adapter over `openps-core`. All logic lives in the core; this file only maps
//! commands and events to the web view and chooses production implementations (Keychain, real network).

use openps_core::backend::RealBackend;
use openps_core::events::{AppEvent, EventSink, MediaSink, NullMedia};
use openps_core::store::ConsoleStore;
use openps_core::{Core, CoreBuilder, DiscoveredConsole, PsnService, SessionState, StreamOptions};
use openps_psn::store::SecretStore;
use openps_rp::tables::KeyTables;
use openps_takion::senkusha::SenkushaConfig;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, State};

/// Event channel name the front end listens on.
pub const EVENT: &str = "openps://event";

mod gamemaps;
#[cfg(target_os = "macos")]
mod input;
mod library;
#[cfg(target_os = "macos")]
mod pads;
mod paths;
#[cfg(feature = "demo")]
mod qa;
#[cfg(feature = "psn-live")]
mod remote;

fn demo_requested() -> bool {
    cfg!(feature = "demo") && std::env::var_os("OPENPS_DEMO").is_some()
}
#[cfg(target_os = "macos")]
mod video;

struct TauriSink(AppHandle);

impl EventSink for TauriSink {
    fn emit(&self, event: AppEvent) {
        #[cfg(target_os = "macos")]
        input::on_event(&self.0, &event);
        let _ = self.0.emit(EVENT, event);
    }
}

type CmdResult<T> = Result<T, String>;

/// Reject empty or oversized text before it reaches the core (every command argument that is a string goes through this).
fn bounded(s: &str, max: usize, what: &str) -> CmdResult<()> {
    if s.is_empty() || s.len() > max {
        return Err(format!("not a valid {what}"));
    }
    Ok(())
}

/// Things the user should be told about that happened while starting (the web view may not be listening yet, so they are kept
/// and also returned by `app_info`).
static NOTICES: std::sync::Mutex<Vec<(String, String)>> = std::sync::Mutex::new(Vec::new());

fn notice(app: Option<&AppHandle>, kind: &str, detail: String) {
    log::warn!("{kind}: {detail}");
    if let Some(app) = app {
        let _ = app.emit(EVENT, AppEvent::Control { kind: kind.into(), detail: detail.clone() });
    }
    NOTICES.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push((kind.into(), detail));
}

/// Move a damaged data file aside as `<name>.damaged-<unix time>` (owner-only) so the app can start fresh and nothing is lost.
fn quarantine(path: &Path) -> std::io::Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".damaged-{secs}"));
    let to = path.with_file_name(name);
    std::fs::rename(path, &to)?;
    let _ = std::fs::set_permissions(&to, std::fs::Permissions::from_mode(0o600));
    Ok(to)
}

/// Open a data file; when it is damaged, quarantine it and open a fresh one (a damaged file must never stop the app from starting).
fn open_or_quarantine<T, E: std::fmt::Display>(
    app: Option<&AppHandle>,
    path: &Path,
    what: &str,
    open: impl Fn() -> Result<T, E>,
) -> Result<T, String> {
    match open() {
        Ok(v) => Ok(v),
        Err(first) => {
            let moved =
                quarantine(path).map_err(|e| format!("{what} is damaged ({first}) and could not be set aside: {e}"))?;
            notice(
                app,
                "data_damaged",
                format!(
                    "The {what} could not be read ({first}). It was kept as {} and OpenPS started with an empty one.",
                    moved.display()
                ),
            );
            open().map_err(|e| e.to_string())
        }
    }
}

/// Run a blocking core call off the UI thread.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> CmdResult<T> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(|e| e.to_string())
}

/// Secrets live in a file inside the application's data directory (portable mode; never the OS keychain).
fn secret_store(app: Option<&AppHandle>, data_dir: &Path) -> Result<Arc<dyn SecretStore + Send + Sync>, String> {
    let path = data_dir.join("secrets.json");
    let store = open_or_quarantine(app, &path, "saved sign-in file (secrets.json)", || {
        openps_psn::store::FileStore::open(&path)
    })?;
    Ok(Arc::new(store))
}

/// Key tables: an override from `$OPENPS_TABLES` or `<app data>/tables.json`, else the ones compiled in.
fn load_tables(data_dir: &Path) -> Option<KeyTables> {
    match KeyTables::from_env() {
        Some(Ok(t)) => return Some(t),
        Some(Err(e)) => {
            log::error!("OPENPS_TABLES: {e}");
            return None;
        }
        None => {}
    }
    let file = data_dir.join("tables.json");
    if file.is_file() {
        match KeyTables::load(&file) {
            Ok(t) => return Some(t),
            Err(e) => log::warn!("{}: {e}; using the built-in tables", file.display()),
        }
    }
    Some(KeyTables::builtin())
}

/// Why PSN sign-in is not available, for the UI (empty when it is). Set once while the core is built.
static PSN_REASON: std::sync::OnceLock<String> = std::sync::OnceLock::new();

#[cfg(feature = "psn-live")]
fn load_psn(data_dir: &Path, secrets: Arc<dyn SecretStore + Send + Sync>) -> Option<Arc<dyn PsnService>> {
    use openps_core::psn::PsnAdapter;
    use openps_psn::http::live::UreqClient;
    use openps_psn::{PsnClient, PsnConfig};
    let cfg = match PsnConfig::locate(data_dir) {
        None => {
            log::info!("PSN credentials: built-in (sign-in will contact Sony)");
            PsnConfig::builtin()
        }
        Some(Ok(c)) => {
            let from = std::env::var_os("OPENPS_PSN").map_or_else(|| data_dir.join("psn.json"), PathBuf::from);
            log::info!("PSN credentials loaded from {} (sign-in will contact Sony)", from.display());
            c
        }
        Some(Err(e)) => {
            let _ = PSN_REASON.set(format!("the PSN credentials could not be read: {e}"));
            log::error!("PSN credentials: {e}");
            return None;
        }
    };
    // the file holds a secret: if it sits loose inside our private data folder, tighten it
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let p = data_dir.join("psn.json");
        if p.is_file() {
            let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
        }
    }
    struct Shared(Arc<dyn SecretStore + Send + Sync>);
    impl SecretStore for Shared {
        fn get(&self, k: &str) -> Option<String> {
            self.0.get(k)
        }
        fn set(&self, k: &str, v: &str) -> Result<(), String> {
            self.0.set(k, v)
        }
        fn delete(&self, k: &str) -> Result<(), String> {
            self.0.delete(k)
        }
    }
    Some(Arc::new(PsnAdapter::new(PsnClient::new(
        cfg,
        UreqClient::new(std::time::Duration::from_secs(20)),
        Shared(secrets),
    ))))
}

#[cfg(not(feature = "psn-live"))]
fn load_psn(_: &Path, _: Arc<dyn SecretStore + Send + Sync>) -> Option<Arc<dyn PsnService>> {
    let _ = PSN_REASON.set("this build cannot contact Sony: rebuild with scripts/install.sh --psn-live".into());
    None // built without `psn-live`: the app never contacts Sony
}

struct MediaParts {
    media: Arc<dyn MediaSink>,
    events: Arc<dyn EventSink>,
    /// Sound control (None where there is no audio output).
    #[cfg(target_os = "macos")]
    audio: Option<Arc<openps_audio::media::AudioOut>>,
    /// The video side (renderer control and statistics).
    #[cfg(target_os = "macos")]
    video: Option<Arc<video::Video>>,
}

#[cfg(target_os = "macos")]
struct VideoHandle(Arc<video::Video>);

/// Choose the media sink and event sink: on macOS decode + render in a view behind the web view (when the main window
/// exists) with sound; elsewhere, or if the video side cannot start, stats only.
fn media_and_events(app: &AppHandle, look: openps_metal::look::SharedLook, settings: &StreamOptions) -> MediaParts {
    let plain: Arc<dyn EventSink> = Arc::new(TauriSink(app.clone()));
    #[cfg(target_os = "macos")]
    {
        if let Some(window) = app.get_webview_window("main") {
            match video::Video::install(&window, look, video::render_settings(settings)) {
                Ok(v) => {
                    let v = Arc::new(v);
                    return MediaParts {
                        media: Arc::new(pads::WithPads(v.media.clone())),
                        events: v.follow_session(app.clone(), plain),
                        audio: Some(v.audio.clone()),
                        video: Some(v),
                    };
                }
                Err(e) => log::error!("video output unavailable: {e}"),
            }
        }
        MediaParts { media: Arc::new(NullMedia), events: plain, audio: None, video: None }
    }
    #[cfg(not(target_os = "macos"))]
    MediaParts { media: Arc::new(NullMedia), events: plain }
}

/// The highest of 144 / 120 / 60 that fits the panel's refresh rate (60 when it is unknown).
fn default_display_fps(refresh_hz: f64) -> u16 {
    [144u16, 120].into_iter().find(|r| refresh_hz >= f64::from(*r) - 1.0).unwrap_or(60)
}

/// HEVC when this machine can decode it in hardware (the console then sends the better-quality stream), H.264 otherwise.
/// PS4 consoles are always H.264 regardless (see `Core::connect`).
fn default_stream_options() -> StreamOptions {
    #[allow(unused_mut)]
    let mut o = StreamOptions::default();
    #[cfg(target_os = "macos")]
    {
        o.hevc = openps_video::vt::preferred_codec() == openps_video::nal::Codec::Hevc;
    }
    // generated frames are what Smooth and camera prediction are for: default to the highest supported rate the panel can show
    o.display_fps = default_display_fps(display_hz());
    o
}

type BackendAndPsn = (Arc<dyn openps_core::backend::Backend>, Option<Arc<dyn PsnService>>);

/// The real network backend, or (with the `demo` feature and `OPENPS_DEMO` set) the built-in pretend console.
fn backend_and_psn(data_dir: &Path, secrets: Arc<dyn SecretStore + Send + Sync>) -> BackendAndPsn {
    #[cfg(feature = "demo")]
    if let Some(cfg) = openps_demo::DemoConfig::from_env() {
        log::warn!("DEMO MODE: talking to a built-in pretend console, not a real one ({cfg:?})");
        return (Arc::new(openps_demo::DemoBackend::new(cfg)), Some(Arc::new(openps_demo::DemoPsn)));
    }
    (Arc::new(RealBackend::new(load_tables(data_dir))), load_psn(data_dir, secrets))
}

fn build_core(app: &AppHandle, data_dir: PathBuf, look: openps_metal::look::SharedLook) -> Result<Core, String> {
    paths::ensure(&data_dir).map_err(|e| format!("{}: {e}", data_dir.display()))?;
    let state_path = data_dir.join("state.json");
    let store = open_or_quarantine(Some(app), &state_path, "saved consoles and settings (state.json)", || {
        ConsoleStore::open(&state_path)
    })?;
    let secrets = secret_store(Some(app), &data_dir)?;
    let saved = store.settings().cloned().unwrap_or_else(default_stream_options);
    let parts = media_and_events(app, look, &saved);
    #[cfg(target_os = "macos")]
    if let Some(v) = &parts.video {
        app.manage(VideoHandle(v.clone()));
    }
    #[cfg(target_os = "macos")]
    if let Some(a) = &parts.audio {
        // the saved volume applies from the first session on
        a.set_volume(store.settings().map_or(100, |s| s.volume));
        app.manage(AudioHandle(a.clone()));
    }
    let (media, events) = (parts.media, parts.events);
    let (backend, psn) = backend_and_psn(&data_dir, secrets.clone());
    let core = Core::new(CoreBuilder {
        store,
        secrets: secrets.clone(),
        backend,
        psn,
        events,
        media,
        settle_s: 6.0,
        senkusha: SenkushaConfig::default(),
    });
    install_remote_registrar(&core, &data_dir);
    Ok(core)
}

/// Registering over the internet: the real thing with `psn-live` and the key tables, a pretend one in demo mode, otherwise not offered.
fn install_remote_registrar(core: &Core, data_dir: &Path) {
    let _ = data_dir;
    #[cfg(feature = "demo")]
    if demo_requested() {
        core.set_remote_registrar(Arc::new(openps_demo::DemoRegistrar));
        return;
    }
    #[cfg(feature = "psn-live")]
    match load_tables(data_dir) {
        Some(t) => core.set_remote_registrar(Arc::new(remote::LiveRegistrar::new(t))),
        None => log::warn!("registering over the internet is unavailable: the key tables are not configured"),
    }
    let _ = core;
}

// ---- commands ---------------------------------------------------------------------------------------------------------------
#[cfg(target_os = "macos")]
struct AudioHandle(Arc<openps_audio::media::AudioOut>);

/// Where this run keeps its data; shown in the About panel so users can see (and delete) everything the app stores.
struct DataDir(PathBuf);

#[tauri::command]
fn app_info(dir: State<'_, DataDir>, core: State<'_, Core>) -> serde_json::Value {
    #[cfg(target_os = "macos")]
    let hevc = openps_video::vt::hardware_decode_supported(openps_video::nal::Codec::Hevc);
    #[cfg(not(target_os = "macos"))]
    let hevc = false;
    serde_json::json!({
      "name": "OpenPS",
      "version": env!("CARGO_PKG_VERSION"),
      "psn": core.has_psn(),
      "psn_reason": PSN_REASON.get().cloned().unwrap_or_default(),
      "demo": demo_requested(),
      "hevc": hevc,
      "data_dir": dir.0.display().to_string(),
      "display_hz": display_hz(),
      "metal": metal_available(),
      "metalfx": metalfx_framegen_available(),
      "notices": NOTICES.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
          .iter().map(|(k, d)| serde_json::json!({ "kind": k, "detail": d })).collect::<Vec<_>>(),
    })
}

fn display_hz() -> f64 {
    #[cfg(target_os = "macos")]
    return openps_metal::main_display_hz();
    #[cfg(not(target_os = "macos"))]
    0.0
}

/// Whether the Metal pipeline (MetalFX upscaling) can run here.
/// MetalFX frame interpolation (macOS 26+) can be used on this Mac.
fn metalfx_framegen_available() -> bool {
    #[cfg(target_os = "macos")]
    {
        static OK: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *OK.get_or_init(|| openps_metal::gpu::Gpu::new().is_ok_and(|g| g.fx_available))
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

fn metal_available() -> bool {
    #[cfg(target_os = "macos")]
    {
        static OK: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *OK.get_or_init(|| openps_metal::gpu::Gpu::new().is_ok_and(|g| g.spatial_scaler_available))
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// Saved settings, or sensible defaults (HEVC when the hardware can decode it).
#[tauri::command]
fn get_settings(core: State<'_, Core>) -> StreamOptions {
    core.settings().unwrap_or_else(default_stream_options)
}

#[tauri::command]
fn set_settings(app: AppHandle, core: State<'_, Core>, settings: StreamOptions) -> CmdResult<()> {
    let volume = settings.volume;
    core.set_settings(settings.clone()).map_err(|e| e.to_string())?;
    apply_volume(&app, volume);
    apply_video(&app, &settings);
    Ok(())
}

fn apply_video(app: &AppHandle, o: &StreamOptions) {
    #[cfg(target_os = "macos")]
    if let Some(v) = app.try_state::<VideoHandle>() {
        v.0.apply(o);
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (app, o);
}

/// Records the frames the console sends for `seconds` seconds (10..=120) and reports how many were new pictures: the way to see what a game
/// really renders while the stream runs at a fixed rate.
#[tauri::command]
async fn frame_diagnostic(app: AppHandle, seconds: f64) -> CmdResult<String> {
    #[cfg(target_os = "macos")]
    if let Some(v) = app.try_state::<VideoHandle>() {
        let dir = app.state::<DataDir>().0.join("logs");
        let v = v.0.clone();
        let secs = if seconds.is_finite() { seconds.clamp(10.0, 120.0) } else { 30.0 };
        return tauri::async_runtime::spawn_blocking(move || v.frame_diagnostic(&dir, secs))
            .await
            .map_err(|e| e.to_string())?;
    }
    let _ = (&app, seconds);
    Err("not available".into())
}

/// Renderer statistics for the HUD: frame pacing, generated frames, latency contributors.
#[tauri::command]
fn render_stats(app: AppHandle) -> serde_json::Value {
    #[cfg(target_os = "macos")]
    if let Some(v) = app.try_state::<VideoHandle>() {
        return v.0.stats();
    }
    let _ = &app;
    serde_json::json!({ "renderer": "none" })
}

fn apply_volume(app: &AppHandle, percent: u8) {
    #[cfg(target_os = "macos")]
    if let Some(a) = app.try_state::<AudioHandle>() {
        a.0.set_volume(percent);
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (app, percent);
}

/// Change the volume right now without saving it (the HUD's mute button).
#[tauri::command]
fn set_volume(app: AppHandle, percent: u8) -> CmdResult<()> {
    if percent > 100 {
        return Err(format!("volume {percent} is out of range (0 to 100)"));
    }
    apply_volume(&app, percent);
    Ok(())
}

/// Whether sound works, and why not if it does not.
#[tauri::command]
fn audio_status(app: AppHandle) -> serde_json::Value {
    #[cfg(target_os = "macos")]
    if let Some(a) = app.try_state::<AudioHandle>() {
        let c = a.0.counters();
        let p = a.0.player_stats().unwrap_or_default();
        return serde_json::json!({
            "available": true, "problem": a.0.problem(), "decoded": c.decoded, "errors": c.errors,
            "underruns": p.underruns, "overruns": p.overruns, "volume": a.0.volume(),
        });
    }
    let _ = &app;
    serde_json::json!({ "available": false, "problem": "audio output is not available on this platform" })
}

#[tauri::command]
fn list_consoles(core: State<'_, Core>) -> Vec<openps_core::ConsoleRecord> {
    core.consoles()
}

#[tauri::command]
async fn scan(core: State<'_, Core>) -> CmdResult<Vec<DiscoveredConsole>> {
    let core = core.inner().clone();
    blocking(move || core.scan(&[]).map_err(|e| e.to_string())).await?
}

#[tauri::command]
async fn wake(core: State<'_, Core>, id: String) -> CmdResult<bool> {
    bounded(&id, 128, "console")?;
    let core = core.inner().clone();
    blocking(move || core.wake(&id).map_err(|e| e.to_string())).await?
}

#[tauri::command]
async fn register(
    core: State<'_, Core>,
    host: String,
    device: openps_core::Device,
    pin: String,
    version: Option<String>,
) -> CmdResult<openps_core::ConsoleRecord> {
    bounded(&host, 253, "console address")?;
    bounded(&pin, 32, "PIN")?;
    if let Some(v) = &version {
        bounded(v, 16, "version")?;
    }
    let core = core.inner().clone();
    blocking(move || core.register(&host, device, &pin, version.as_deref()).map_err(|e| e.to_string())).await?
}

#[tauri::command]
fn forget(core: State<'_, Core>, id: String) -> CmdResult<()> {
    bounded(&id, 128, "console")?;
    core.forget(&id).map_err(|e| e.to_string())
}

#[tauri::command]
fn connect(app: AppHandle, core: State<'_, Core>, id: String, options: Option<StreamOptions>) -> CmdResult<()> {
    bounded(&id, 128, "console")?;
    let opts = options.or_else(|| core.settings()).unwrap_or_else(default_stream_options);
    // the renderer must never see options the core would refuse
    opts.validate()?;
    apply_video(&app, &opts);
    core.connect(&id, opts).map_err(|e| e.to_string())
}

#[tauri::command]
fn disconnect(core: State<'_, Core>) -> CmdResult<()> {
    core.disconnect().map_err(|e| e.to_string())
}

/// Puts the console into rest mode (the only remote power-down Remote Play has), then ends the session.
#[tauri::command]
async fn rest_console(core: State<'_, Core>) -> CmdResult<()> {
    let core = core.inner().clone();
    blocking(move || {
        core.control(openps_core::control::ControlCommand::RestMode).map_err(|e| e.to_string())?;
        // give the frame a moment to leave before the connection is torn down
        std::thread::sleep(std::time::Duration::from_millis(400));
        core.disconnect().map_err(|e| e.to_string())
    })
    .await?
}

#[tauri::command]
fn active_console(core: State<'_, Core>) -> Option<String> {
    core.active_console()
}

#[tauri::command]
fn session_state(core: State<'_, Core>) -> SessionState {
    core.session_state()
}

#[tauri::command]
fn request_keyframe(core: State<'_, Core>) -> CmdResult<()> {
    core.request_keyframe().map_err(|e| e.to_string())
}

#[tauri::command]
fn psn_account(core: State<'_, Core>) -> Option<openps_core::PsnAccountInfo> {
    core.psn_account()
}

#[derive(serde::Serialize)]
struct PsnApiStatus {
    available: bool,
    signed_in: bool,
    /// Can a console be registered over the internet (key tables present, live build or demo)?
    remote: bool,
}

/// Can the PSN API (console list) be used here, and is the user signed in to it?
#[tauri::command]
fn psn_api_status(core: State<'_, Core>) -> PsnApiStatus {
    let (available, signed_in) = core.psn_api_status();
    PsnApiStatus { available, signed_in, remote: core.remote_available() }
}

/// Sign in to the PSN API with the pasted login token (npsso). It is used once and not kept; it is never logged.
#[tauri::command]
async fn psn_api_sign_in(core: State<'_, Core>, npsso: String) -> CmdResult<()> {
    bounded(npsso.trim(), 256, "login token")?;
    let core = core.inner().clone();
    blocking(move || core.psn_api_sign_in(&npsso).map_err(|e| e.to_string())).await?
}

/// Register the PS5 with this PSN id over the internet. Blocks (off the UI thread) for up to a few minutes; progress arrives as
/// `remote_step` events.
#[tauri::command]
async fn psn_register_remote(core: State<'_, Core>, duid: String) -> CmdResult<openps_core::ConsoleRecord> {
    if !library::plain_id(&duid, 128) {
        return Err("not a valid console".into());
    }
    let core = core.inner().clone();
    blocking(move || core.register_remote(&duid).map_err(|e| e.to_string())).await?
}

#[tauri::command]
fn psn_register_cancel(core: State<'_, Core>) {
    core.cancel_remote();
}

#[tauri::command]
fn psn_api_sign_out(core: State<'_, Core>) {
    core.psn_api_sign_out();
}

/// The account's PS5 consoles as PSN lists them.
#[tauri::command]
async fn psn_consoles(core: State<'_, Core>) -> CmdResult<Vec<openps_core::PsnConsole>> {
    let core = core.inner().clone();
    blocking(move || core.psn_consoles().map_err(|e| e.to_string())).await?
}

#[tauri::command]
fn psn_sign_out(core: State<'_, Core>) -> CmdResult<()> {
    core.psn_sign_out().map_err(|e| e.to_string())
}

/// Open the PSN sign-in page in its own window and finish the login when it redirects back.
#[tauri::command]
fn psn_sign_in(app: AppHandle, core: State<'_, Core>) -> CmdResult<()> {
    use tauri::{WebviewUrl, WebviewWindowBuilder};
    let url = core.psn_authorize_url().map_err(|e| e.to_string())?;
    if url.starts_with("demo:") {
        // demo mode: no browser window, no network; sign in instantly
        return core.psn_complete(&url).map(|_| ()).map_err(|e| e.to_string());
    }
    let parsed: tauri::Url = url.parse().map_err(|e| format!("bad sign-in URL: {e}"))?;
    let (core, handle) = (core.inner().clone(), app.clone());
    WebviewWindowBuilder::new(&app, "psn-login", WebviewUrl::External(parsed))
        .title("Sign in to PlayStation Network")
        .incognito(true) // no cookies or cache are written anywhere
        .inner_size(520.0, 720.0)
        .on_navigation(move |u| {
            let s = u.as_str().to_string();
            // Only the real redirect is ours; a URL that merely contains "code=" (error_code=, country_code=, ...) is
            // ordinary navigation inside Sony's pages and must not be touched.
            if core.psn_is_redirect(&s) {
                let (core, handle) = (core.clone(), handle.clone());
                std::thread::spawn(move || {
                    let outcome = core.psn_complete(&s);
                    if let Err(e) = &outcome {
                        log::warn!("PSN sign-in: {e}");
                        // tell the main window, so the user is not left wondering why nothing happened
                        let _ =
                            handle.emit(EVENT, AppEvent::Control { kind: "psn_error".into(), detail: e.to_string() });
                    }
                    // The same login left the account's session token in this private window. Take it for the console
                    // list so nothing has to be pasted; it goes straight to the API sign-in, is never logged, never stored.
                    if outcome.is_ok() {
                        let (avail, signed) = core.psn_api_status();
                        if avail && !signed {
                            if let Some(token) = handle.get_webview_window("psn-login").and_then(|w| {
                                let url: tauri::Url = "https://ca.account.sony.com/".parse().ok()?;
                                w.cookies_for_url(url)
                                    .ok()?
                                    .into_iter()
                                    .find(|c| c.name() == "npsso")
                                    .map(|c| c.value().to_string())
                            }) {
                                match core.psn_api_sign_in(&token) {
                                    Ok(()) => {
                                        let _ = handle.emit(EVENT, AppEvent::PsnChanged { signed_in: true });
                                    }
                                    Err(e) => log::warn!("PSN console-list sign-in: {e}"),
                                }
                            }
                        }
                    }
                    // success or failure, the sign-in window has done its job
                    if let Some(w) = handle.get_webview_window("psn-login") {
                        let _ = w.close();
                    }
                });
                return false; // never load the redirect target
            }
            // The sign-in page is HTTPS. Refuse everything else (http, file:, data:, javascript:, custom schemes).
            u.scheme() == "https" || s == "about:blank"
        })
        .build()
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            #[cfg(feature = "demo")]
            qa::qa_result,
            #[cfg(feature = "demo")]
            qa::qa_resize,
            #[cfg(feature = "demo")]
            qa::qa_close,
            #[cfg(feature = "demo")]
            qa::qa_focus,
            #[cfg(feature = "demo")]
            qa::qa_last_pad,
            app_info,
            #[cfg(target_os = "macos")]
            input::input_key,
            #[cfg(target_os = "macos")]
            input::input_button,
            input::ps_button,
            #[cfg(target_os = "macos")]
            input::input_wheel,
            #[cfg(target_os = "macos")]
            input::input_toggle,
            #[cfg(target_os = "macos")]
            input::input_overlay,
            #[cfg(target_os = "macos")]
            input::input_ui,
            #[cfg(target_os = "macos")]
            input::input_config_info,
            #[cfg(target_os = "macos")]
            input::input_config_reveal,
            #[cfg(target_os = "macos")]
            input::input_config_reset,
            #[cfg(target_os = "macos")]
            pads::controller_status,
            #[cfg(target_os = "macos")]
            pads::controller_set_deadzone,
            get_settings,
            render_stats,
            input::input_profiles,
            input::migrate_game_maps,
            frame_diagnostic,
            set_settings,
            set_volume,
            audio_status,
            list_consoles,
            scan,
            wake,
            rest_console,
            register,
            forget,
            connect,
            disconnect,
            session_state,
            active_console,
            request_keyframe,
            psn_account,
            psn_sign_out,
            psn_api_status,
            psn_api_sign_in,
            psn_api_sign_out,
            psn_register_remote,
            psn_register_cancel,
            library::psn_library,
            library::game_art,
            library::launch_game,
            library::launch_status,
            gamemaps::game_maps,
            gamemaps::set_game_map,
            input::input_profile_names,
            input::input_select_profile,
            psn_consoles,
            psn_sign_in
        ])
        .setup(|app| {
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            // Everything the process creates (including files made by the log plugin when it rotates) is owner-only.
            #[cfg(target_os = "macos")]
            {
                extern "C" {
                    fn umask(mask: u16) -> u16;
                }
                // SAFETY: umask(2) only sets the process file-creation mask; it cannot fail.
                unsafe {
                    umask(0o077);
                }
            }
            let env_home = std::env::var("OPENPS_HOME").ok();
            let mut dir = paths::data_dir(env_home.as_deref(), &exe);
            if let Err(first) = paths::ensure(&dir) {
                // a read-only or foreign-owned app bundle: fall back to the user's Application Support folder
                let alt = if env_home.as_deref().is_some_and(|p| !p.trim().is_empty()) {
                    None
                } else {
                    paths::fallback_dir(std::env::var("HOME").ok().as_deref())
                };
                match alt {
                    Some(alt) if paths::ensure(&alt).is_ok() => {
                        eprintln!("data folder {} is not writable ({first}); using {}", dir.display(), alt.display());
                        dir = alt;
                    }
                    _ => return Err(format!("cannot create {}: {first}", dir.display()).into()),
                }
            }
            // logs are private too: a 0700 folder and a 0600 file (the log plugin would otherwise create them world-readable)
            #[cfg(unix)]
            {
                use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
                let logs = dir.join("logs");
                let _ = std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&logs);
                let _ = std::fs::set_permissions(&logs, std::fs::Permissions::from_mode(0o700));
                let file = logs.join("openps.log");
                let _ = std::fs::OpenOptions::new().append(true).create(true).mode(0o600).open(&file);
                let _ = std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600));
            }
            let mut log = tauri_plugin_log::Builder::default()
                .clear_targets()
                .target(tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::Folder {
                    path: dir.join("logs"),
                    file_name: Some("openps".into()),
                }))
                .target(tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::Stdout))
                .level(log::LevelFilter::Info);
            if openps_input::trace::enabled() {
                // the default (40 KB, keep one) would rotate the start of a reproduction away within seconds
                log = log.max_file_size(100_000_000).rotation_strategy(tauri_plugin_log::RotationStrategy::KeepAll);
            }
            app.handle().plugin(log.build())?;
            // `incognito`: the web view keeps no cookies, cache or local storage on disk.
            tauri::WebviewWindowBuilder::new(app, "main", tauri::WebviewUrl::App("index.html".into()))
                .title("OpenPS")
                .inner_size(1180.0, 760.0)
                .min_inner_size(360.0, 480.0)
                .incognito(true)
                .build()?;
            let dir = if demo_requested() { dir.join("demo") } else { dir };
            app.manage(DataDir(dir.clone()));
            let look = openps_metal::look::SharedLook::new(); // what we told the console the stick does; the renderer reads it
            #[cfg(target_os = "macos")]
            {
                app.manage(input::InputState::new(&dir, look.clone()));
                app.manage(pads::PadState::new(look.clone()));
            }
            let core = build_core(app.handle(), dir, look).map_err(|e| format!("cannot start: {e}"))?;
            app.manage(core);
            // The input hub ignores input until it is told the window is focused: the window may already be key now, and then no
            // `Focused` event will ever arrive for it.
            #[cfg(target_os = "macos")]
            if let Some(w) = app.get_webview_window("main") {
                input::set_focus(app.handle(), w.is_focused().unwrap_or(false));
                pads::set_focus(app.handle(), w.is_focused().unwrap_or(false));
            }
            #[cfg(target_os = "macos")]
            pads::init(app.handle());
            // Window events can arrive before a full-screen transition has finished, so the renderer's idea of "full screen" (and of the
            // display's refresh rate) is also re-read twice a second. Cheap, and it always converges. The timer thread only posts
            // to the main thread: NSWindow and NSScreen may be used from there only.
            #[cfg(target_os = "macos")]
            {
                let handle = app.handle().clone();
                std::thread::spawn(move || loop {
                    std::thread::sleep(std::time::Duration::from_millis(500));
                    let h = handle.clone();
                    if handle
                        .run_on_main_thread(move || {
                            if let Some(w) = h.get_webview_window("main") {
                                video::sync_refresh(&w.as_ref().window());
                            }
                        })
                        .is_err()
                    {
                        break; // the event loop is gone: the app is exiting
                    }
                });
            }
            #[cfg(feature = "demo")]
            qa::start(app.handle().clone());
            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() != "main" {
                return;
            }
            match event {
                // closing the main window ends any running session first, so the console is told goodbye
                tauri::WindowEvent::CloseRequested { api, .. } => {
                    #[cfg(target_os = "macos")]
                    input::shutdown(window.app_handle()); // hand the mouse back before anything else
                                                          // The shutdown can wait on the network, so it never runs on the main thread: hold the window open, say goodbye
                                                          // on a worker, and quit when it is done or after a bounded wait.
                    static CLOSING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
                    api.prevent_close();
                    if !CLOSING.swap(true, std::sync::atomic::Ordering::SeqCst) {
                        let app = window.app_handle().clone();
                        std::thread::spawn(move || {
                            let (tx, rx) = std::sync::mpsc::channel::<()>();
                            if let Some(core) = app.try_state::<Core>() {
                                let core = core.inner().clone();
                                std::thread::spawn(move || {
                                    core.shutdown();
                                    let _ = tx.send(());
                                });
                            } else {
                                let _ = tx.send(());
                            }
                            if rx.recv_timeout(std::time::Duration::from_secs(8)).is_err() {
                                log::warn!("shutdown is taking too long; quitting anyway");
                            }
                            app.exit(0);
                        });
                    }
                }
                #[cfg(target_os = "macos")]
                tauri::WindowEvent::Focused(f) => {
                    input::set_focus(window.app_handle(), *f);
                    pads::set_focus(window.app_handle(), *f);
                }
                #[cfg(target_os = "macos")]
                tauri::WindowEvent::Moved(_)
                | tauri::WindowEvent::Resized(_)
                | tauri::WindowEvent::ScaleFactorChanged { .. } => {
                    video::sync_refresh(window);
                    input::update_bounds(window.app_handle())
                }
                _ => {}
            }
        })
        .run(tauri::generate_context!())
        .expect("error while building tauri application");
}

#[cfg(test)]
mod default_tests {
    use super::default_display_fps;

    #[test]
    fn the_default_display_rate_follows_the_panel() {
        assert_eq!(default_display_fps(0.0), 60); // unknown
        assert_eq!(default_display_fps(60.0), 60);
        assert_eq!(default_display_fps(75.0), 60);
        assert_eq!(default_display_fps(119.88), 120);
        assert_eq!(default_display_fps(120.0), 120);
        assert_eq!(default_display_fps(143.96), 144);
        assert_eq!(default_display_fps(165.0), 144);
    }
}

#[cfg(test)]
mod damaged_file_tests {
    use super::*;

    #[test]
    fn a_damaged_state_file_is_set_aside_and_the_app_starts_fresh() {
        use std::os::unix::fs::PermissionsExt;
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("state.json");
        for (i, junk) in [&b""[..], b"{ truncated", b"<<<<<<< HEAD", b"[1,2]"].into_iter().enumerate() {
            std::fs::write(&p, junk).unwrap();
            let store = open_or_quarantine(None, &p, "state", || ConsoleStore::open(&p)).expect("must start");
            assert!(store.settings().is_none());
            let kept: Vec<_> = std::fs::read_dir(t.path())
                .unwrap()
                .filter_map(Result::ok)
                .filter(|e| e.file_name().to_string_lossy().starts_with("state.json.damaged-"))
                .collect();
            assert!(!kept.is_empty(), "case {i}: the damaged file must be kept");
            for k in &kept {
                assert!(std::fs::read(k.path()).unwrap().len() <= 16);
                assert_eq!(std::fs::metadata(k.path()).unwrap().permissions().mode() & 0o777, 0o600);
            }
            let _ = std::fs::remove_file(&p);
        }
    }

    #[test]
    fn a_damaged_secrets_file_is_set_aside_too() {
        let t = tempfile::tempdir().unwrap();
        std::fs::write(t.path().join("secrets.json"), b"not json").unwrap();
        assert!(secret_store(None, t.path()).is_ok());
        assert!(std::fs::read_dir(t.path())
            .unwrap()
            .filter_map(Result::ok)
            .any(|e| e.file_name().to_string_lossy().starts_with("secrets.json.damaged-")));
    }

    #[test]
    fn text_arguments_are_bounded() {
        assert!(bounded("", 8, "x").is_err());
        assert!(bounded(&"a".repeat(9), 8, "x").is_err());
        assert!(bounded("ok", 8, "x").is_ok());
    }
}
