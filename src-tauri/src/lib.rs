//! openPS Tauri shell: a thin adapter over `openps-core`. All logic lives in the core; this file only maps
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

mod paths;
#[cfg(feature = "demo")]
mod qa;

fn demo_requested() -> bool {
    cfg!(feature = "demo") && std::env::var_os("OPENPS_DEMO").is_some()
}
#[cfg(target_os = "macos")]
mod video;

struct TauriSink(AppHandle);

impl EventSink for TauriSink {
    fn emit(&self, event: AppEvent) {
        let _ = self.0.emit(EVENT, event);
    }
}

type CmdResult<T> = Result<T, String>;

/// Run a blocking core call off the UI thread.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> CmdResult<T> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(|e| e.to_string())
}

/// Secrets live in a file inside the application's data directory (portable mode; never the OS keychain).
fn secret_store(data_dir: &Path) -> Result<Arc<dyn SecretStore + Send + Sync>, String> {
    Ok(Arc::new(openps_psn::store::FileStore::open(data_dir.join("secrets.json"))?))
}

/// Key tables: `$OPENPS_TABLES`, else `<app data>/tables.json`. Missing tables are not fatal; pairing and sessions
/// then report a clear error.
fn load_tables(data_dir: &Path) -> Option<KeyTables> {
    match KeyTables::from_env() {
        Some(Ok(t)) => Some(t),
        Some(Err(e)) => {
            log::error!("OPENPS_TABLES: {e}");
            None
        }
        None => KeyTables::load(&data_dir.join("tables.json")).ok(),
    }
}

#[cfg(feature = "psn-live")]
fn load_psn(secrets: Arc<dyn SecretStore + Send + Sync>) -> Option<Arc<dyn PsnService>> {
    use openps_core::psn::PsnAdapter;
    use openps_psn::http::live::UreqClient;
    use openps_psn::{PsnClient, PsnConfig};
    let cfg = match PsnConfig::from_env()? {
        Ok(c) => c,
        Err(e) => {
            log::error!("OPENPS_PSN: {e}");
            return None;
        }
    };
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
fn load_psn(_: Arc<dyn SecretStore + Send + Sync>) -> Option<Arc<dyn PsnService>> {
    None // built without `psn-live`: the app never contacts Sony
}

struct MediaParts {
    media: Arc<dyn MediaSink>,
    events: Arc<dyn EventSink>,
    /// Sound control (None where there is no audio output).
    #[cfg(target_os = "macos")]
    audio: Option<Arc<openps_audio::media::AudioOut>>,
}

/// Choose the media sink and event sink: on macOS decode + render in a view behind the web view (when the main window
/// exists) with sound; elsewhere, or if the video side cannot start, stats only.
fn media_and_events(app: &AppHandle) -> MediaParts {
    let plain: Arc<dyn EventSink> = Arc::new(TauriSink(app.clone()));
    #[cfg(target_os = "macos")]
    {
        if let Some(window) = app.get_webview_window("main") {
            match video::Video::install(&window) {
                Ok(v) => {
                    return MediaParts {
                        media: v.media.clone(),
                        events: v.follow_session(app.clone(), plain),
                        audio: Some(v.audio.clone()),
                    }
                }
                Err(e) => log::error!("video output unavailable: {e}"),
            }
        }
        MediaParts { media: Arc::new(NullMedia), events: plain, audio: None }
    }
    #[cfg(not(target_os = "macos"))]
    MediaParts { media: Arc::new(NullMedia), events: plain }
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
    (Arc::new(RealBackend::new(load_tables(data_dir))), load_psn(secrets))
}

fn build_core(app: &AppHandle, data_dir: PathBuf) -> Result<Core, String> {
    paths::ensure(&data_dir).map_err(|e| format!("{}: {e}", data_dir.display()))?;
    let store = ConsoleStore::open(data_dir.join("state.json")).map_err(|e| e.to_string())?;
    let secrets = secret_store(&data_dir)?;
    let parts = media_and_events(app);
    #[cfg(target_os = "macos")]
    if let Some(a) = &parts.audio {
        // the saved volume applies from the first session on
        a.set_volume(store.settings().map_or(100, |s| s.volume));
        app.manage(AudioHandle(a.clone()));
    }
    let (media, events) = (parts.media, parts.events);
    let (backend, psn) = backend_and_psn(&data_dir, secrets.clone());
    Ok(Core::new(CoreBuilder {
        store,
        secrets: secrets.clone(),
        backend,
        psn,
        events,
        media,
        settle_s: 2.5,
        senkusha: SenkushaConfig::default(),
    }))
}

// ---- commands ---------------------------------------------------------------------------------------------------------------
#[cfg(target_os = "macos")]
struct AudioHandle(Arc<openps_audio::media::AudioOut>);

/// Where this run keeps its data; shown in the About panel so users can see (and delete) everything the app stores.
struct DataDir(PathBuf);

#[tauri::command]
fn app_info(dir: State<'_, DataDir>) -> serde_json::Value {
    #[cfg(target_os = "macos")]
    let hevc = openps_video::vt::hardware_decode_supported(openps_video::nal::Codec::Hevc);
    #[cfg(not(target_os = "macos"))]
    let hevc = false;
    serde_json::json!({
      "name": "openPS",
      "version": env!("CARGO_PKG_VERSION"),
      "psn": cfg!(feature = "psn-live") || demo_requested(),
      "demo": demo_requested(),
      "hevc": hevc,
      "data_dir": dir.0.display().to_string(),
    })
}

/// Saved settings, or sensible defaults (HEVC when the hardware can decode it).
#[tauri::command]
fn get_settings(core: State<'_, Core>) -> StreamOptions {
    core.settings().unwrap_or_else(default_stream_options)
}

#[tauri::command]
fn set_settings(app: AppHandle, core: State<'_, Core>, settings: StreamOptions) -> CmdResult<()> {
    let volume = settings.volume;
    core.set_settings(settings).map_err(|e| e.to_string())?;
    apply_volume(&app, volume);
    Ok(())
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
    let core = core.inner().clone();
    blocking(move || core.register(&host, device, &pin, version.as_deref()).map_err(|e| e.to_string())).await?
}

#[tauri::command]
fn forget(core: State<'_, Core>, id: String) -> CmdResult<()> {
    core.forget(&id).map_err(|e| e.to_string())
}

#[tauri::command]
fn connect(core: State<'_, Core>, id: String, options: Option<StreamOptions>) -> CmdResult<()> {
    let opts = options.or_else(|| core.settings()).unwrap_or_else(default_stream_options);
    core.connect(&id, opts).map_err(|e| e.to_string())
}

#[tauri::command]
fn disconnect(core: State<'_, Core>) -> CmdResult<()> {
    core.disconnect().map_err(|e| e.to_string())
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
            app_info,
            get_settings,
            set_settings,
            set_volume,
            audio_status,
            list_consoles,
            scan,
            wake,
            register,
            forget,
            connect,
            disconnect,
            session_state,
            request_keyframe,
            psn_account,
            psn_sign_out,
            psn_sign_in
        ])
        .setup(|app| {
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            let dir = paths::data_dir(std::env::var("OPENPS_HOME").ok().as_deref(), &exe);
            paths::ensure(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
            app.handle().plugin(
                tauri_plugin_log::Builder::default()
                    .clear_targets()
                    .target(tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::Folder {
                        path: dir.join("logs"),
                        file_name: Some("openps".into()),
                    }))
                    .target(tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::Stdout))
                    .level(log::LevelFilter::Info)
                    .build(),
            )?;
            // `incognito`: the web view keeps no cookies, cache or local storage on disk.
            tauri::WebviewWindowBuilder::new(app, "main", tauri::WebviewUrl::App("index.html".into()))
                .title("openPS")
                .inner_size(1180.0, 760.0)
                .min_inner_size(360.0, 480.0)
                .incognito(true)
                .build()?;
            let dir = if demo_requested() { dir.join("demo") } else { dir };
            app.manage(DataDir(dir.clone()));
            let core = build_core(app.handle(), dir).map_err(|e| format!("cannot start: {e}"))?;
            app.manage(core);
            #[cfg(feature = "demo")]
            qa::start(app.handle().clone());
            Ok(())
        })
        .on_window_event(|window, event| {
            // closing the main window ends any running session first, so the console is told goodbye
            if let tauri::WindowEvent::CloseRequested { .. } = event {
                if window.label() == "main" {
                    if let Some(core) = window.try_state::<Core>() {
                        core.shutdown();
                    }
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error while building tauri application");
}
