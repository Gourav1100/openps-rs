//! The engine thread: ticks the Con2PC engine at its own rate and applies messages from the UI thread.

use crate::cursor::Cursor;
use crate::hub::{InputHub, Msg};
use crate::platform::{HubPlatform, Output};
use openps_input::engine::{Engine, Hotkey, UiEvent};
use openps_input::scene::SceneLink;
use openps_input::Config;
use std::path::PathBuf;
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub struct InputRunner {
    hub: Arc<InputHub>,
    tx: Sender<Msg>,
    join: Option<JoinHandle<()>>,
}

pub struct RunnerConfig {
    pub config: Config,
    /// Where overlay edits are saved.
    pub config_path: Option<PathBuf>,
    /// Start with mapping on (the stream window is dedicated, so there is nothing to lose the keyboard from).
    pub enabled: bool,
}

impl InputRunner {
    pub fn start(
        hub: Arc<InputHub>,
        cfg: RunnerConfig,
        cursor: Box<dyn Cursor>,
        out: Box<dyn Output>,
    ) -> Result<Self, String> {
        Self::start_with_scene(hub, cfg, cursor, out, None)
    }

    /// Like `start`, with the link the scene detector shares with the engine (the layer in, the actions out).
    pub fn start_with_scene(
        hub: Arc<InputHub>,
        cfg: RunnerConfig,
        cursor: Box<dyn Cursor>,
        out: Box<dyn Output>,
        scene: Option<Arc<SceneLink>>,
    ) -> Result<Self, String> {
        let (tx, rx) = channel::<Msg>();
        let (ready_tx, ready_rx) = channel::<Result<(), String>>();
        hub.attach(tx.clone());
        let hub2 = hub.clone();
        let join = std::thread::Builder::new()
            .name("openps-input".into())
            .spawn(move || {
                // the engine is built on its own thread: it never has to be Send
                let platform = HubPlatform::new(hub2.clone(), cursor, out);
                let mut engine = match Engine::new(cfg.config, platform, false) {
                    Ok(e) => e,
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                        return;
                    }
                };
                if let Some(p) = cfg.config_path {
                    engine = engine.with_config_path(p);
                }
                if let Some(link) = scene {
                    engine = engine.with_scene(link);
                }
                let t0 = Instant::now();
                engine.start(0.0);
                if cfg.enabled {
                    engine.apply_enabled(true);
                }
                let _ = ready_tx.send(Ok(()));
                loop {
                    engine.tick(t0.elapsed().as_secs_f64());
                    let wait = Duration::from_secs_f64(engine.period().clamp(0.001, 0.25));
                    match rx.recv_timeout(wait) {
                        Ok(Msg::Stop) | Err(RecvTimeoutError::Disconnected) => break,
                        Ok(first) => {
                            for m in std::iter::once(first).chain(rx.try_iter()) {
                                match m {
                                    Msg::Press(inp, held) => {
                                        engine.on_press(inp, &held);
                                    }
                                    Msg::Ui(e) => engine.push_ui_event(e),
                                    Msg::Hotkey(h) => engine.request(h),
                                    Msg::Wake => {}
                                    Msg::Stop => {
                                        engine.shutdown();
                                        return;
                                    }
                                }
                            }
                        }
                        Err(RecvTimeoutError::Timeout) => {}
                    }
                }
                engine.shutdown(); // releases the cursor and sends a neutral pad
            })
            .map_err(|e| e.to_string())?;
        match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(())) => Ok(Self { hub, tx, join: Some(join) }),
            Ok(Err(e)) => {
                hub.detach();
                Err(e)
            }
            Err(_) => {
                hub.detach();
                Err("the input engine did not start".into())
            }
        }
    }

    pub fn hotkey(&self, h: Hotkey) {
        let _ = self.tx.send(Msg::Hotkey(h));
    }
    pub fn ui_event(&self, e: UiEvent) {
        let _ = self.tx.send(Msg::Ui(e));
    }

    /// Stop the engine and wait for it: the cursor is released and a neutral pad sent before this returns.
    pub fn stop(mut self) {
        self.finish();
    }

    fn finish(&mut self) {
        self.hub.detach();
        let _ = self.tx.send(Msg::Stop);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
        self.hub.release_all();
    }
}

impl Drop for InputRunner {
    fn drop(&mut self) {
        self.finish();
    }
}
