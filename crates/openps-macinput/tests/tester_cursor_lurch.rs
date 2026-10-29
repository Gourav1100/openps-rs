//! Touches the real cursor. By hand: cargo test -p openps-macinput --test tester_cursor_lurch -- --ignored --nocapture
//! Measures what the mouse delta reads right after the cursor is captured and warped to the window centre, with the cursor far away.
#![cfg(target_os = "macos")]
use openps_macinput::cursor::MacCursor;
use openps_macinput::Cursor;
use std::time::Duration;

#[test]
#[ignore = "moves the real mouse cursor for about a second"]
fn deltas_right_after_capture_are_small() {
    let mut c = MacCursor::new();
    c.engage(Some((900.0, 600.0))); // warp from wherever the pointer is
    let mut seen = vec![];
    for _ in 0..40 {
        std::thread::sleep(Duration::from_millis(2));
        seen.push(c.delta());
    }
    c.release();
    let worst = seen.iter().map(|(x, y)| x.abs().max(y.abs())).fold(0.0, f64::max);
    eprintln!("largest delta in the first 80 ms after capture: {worst}; first reads: {:?}", &seen[..6]);
    assert!(worst < 30.0, "the capture itself moved the stick: {seen:?}");
}
