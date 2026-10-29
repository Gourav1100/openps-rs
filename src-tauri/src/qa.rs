//! QA harness, compiled only with the `demo` feature and active only when `OPENPS_QA` names a directory.
//! A tester writes JavaScript to `<dir>/cmd.js`; the app evaluates it in the main window and the script may report back
//! with `window.__TAURI__.core.invoke("qa_result", { text })`, which lands in `<dir>/result.txt`.
//! This exists to drive the real UI in end-to-end tests without OS accessibility permissions.

use std::path::PathBuf;
use std::time::Duration;
use tauri::{AppHandle, Manager};

fn dir() -> Option<PathBuf> {
    std::env::var_os("OPENPS_QA").map(PathBuf::from)
}

#[tauri::command]
pub fn qa_result(text: String) -> Result<(), String> {
    let dir = dir().ok_or("QA harness is not enabled")?;
    std::fs::write(dir.join("result.txt"), text).map_err(|e| e.to_string())
}

/// Resize the main window (logical pixels) so responsive layouts can be checked at real sizes.
#[tauri::command]
pub fn qa_resize(app: AppHandle, width: f64, height: f64) -> Result<(), String> {
    let w = app.get_webview_window("main").ok_or("no window")?;
    w.set_size(tauri::LogicalSize::new(width, height)).map_err(|e| e.to_string())
}

/// Close the main window the way the user's close button does (goes through the close-requested handler).
#[tauri::command]
pub fn qa_close(app: AppHandle) -> Result<(), String> {
    app.get_webview_window("main").ok_or("no window")?.close().map_err(|e| e.to_string())
}

/// Bring the window to the front (measurements of frame pacing are only meaningful for a visible window).
#[tauri::command]
pub fn qa_focus(app: AppHandle) -> Result<(), String> {
    let w = app.get_webview_window("main").ok_or("no window")?;
    w.show().map_err(|e| e.to_string())?;
    w.set_focus().map_err(|e| e.to_string())
}

pub fn start(app: AppHandle) {
    let Some(dir) = dir() else { return };
    log::warn!("QA harness active: watching {}", dir.display());
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_millis(150));
        let cmd = dir.join("cmd.js");
        let Ok(script) = std::fs::read_to_string(&cmd) else { continue };
        let _ = std::fs::remove_file(&cmd);
        let _ = std::fs::remove_file(dir.join("result.txt"));
        if let Some(w) = app.get_webview_window("main") {
            let _ = w.eval(&script);
        }
    });
}

/// The pad state the key map last produced, as text.
#[tauri::command]
pub fn qa_last_pad() -> String {
    crate::input::last_pad_debug()
}
