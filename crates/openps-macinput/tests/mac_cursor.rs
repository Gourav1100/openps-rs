//! Touches the real cursor (freezes and hides it for ~0.3 s), so it is not part of the normal run:
//! `cargo test -p openps-macinput --test mac_cursor -- --ignored`
#![cfg(target_os = "macos")]
use openps_macinput::cursor::MacCursor;
use openps_macinput::Cursor;
use std::time::Duration;

#[test]
#[ignore = "briefly freezes and hides the real mouse cursor"]
fn engage_reads_deltas_and_release_always_restores_the_cursor() {
    let mut c = MacCursor::new();
    c.set_hide(true);
    c.engage(None);
    std::thread::sleep(Duration::from_millis(150));
    let (dx, dy) = c.delta();
    assert!(dx.is_finite() && dy.is_finite());
    c.recenter((0.0, 0.0)); // must not panic while engaged
    std::thread::sleep(Duration::from_millis(150));
    c.release();
    c.release(); // idempotent: hide/show stay balanced
                 // dropping while engaged must also restore it
    let mut d = MacCursor::new();
    d.engage(None);
    drop(d);
}
