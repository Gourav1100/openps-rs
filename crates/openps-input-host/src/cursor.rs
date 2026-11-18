//! Mouse capture. While mapping is live the cursor is frozen in place and hidden, and the mouse is read as relative
//! movement. `CGAssociateMouseAndMouseCursorPosition` and `CGGetLastMouseDelta` need no special permission.

/// Everything is released again by `release()` and by `Drop`: a frozen, invisible cursor is the worst possible failure.
pub trait Cursor: Send {
    fn engage(&mut self, center: Option<(f64, f64)>);
    fn recenter(&mut self, _center: (f64, f64)) {}
    fn release(&mut self);
    fn set_hide(&mut self, hide: bool);
    /// Mouse movement since the previous call.
    fn delta(&mut self) -> (f64, f64);
}

/// No real cursor (tests, other platforms).
#[derive(Default)]
pub struct NullCursor;
impl Cursor for NullCursor {
    fn engage(&mut self, _: Option<(f64, f64)>) {}
    fn release(&mut self) {}
    fn set_hide(&mut self, _: bool) {}
    fn delta(&mut self) -> (f64, f64) {
        (0.0, 0.0)
    }
}

