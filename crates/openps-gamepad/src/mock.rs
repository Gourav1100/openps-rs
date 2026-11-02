//! A scripted pad for tests and the demo.

use crate::{PadEvent, PadSink, PadSource};
use openps_stream::padblob::PadOutput;
use std::sync::{Arc, Mutex, PoisonError};

type Log<T> = Arc<Mutex<Vec<(u32, T)>>>;

#[derive(Clone, Default)]
pub struct MockPad {
    queue: Arc<Mutex<Vec<PadEvent>>>,
    pub applied: Log<PadOutput>,
    pub haptics: Log<[u8; 120]>,
}

impl MockPad {
    pub fn push(&self, e: PadEvent) {
        self.queue.lock().unwrap_or_else(PoisonError::into_inner).push(e);
    }
}

impl PadSource for MockPad {
    fn poll(&mut self) -> Vec<PadEvent> {
        std::mem::take(&mut *self.queue.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

impl PadSink for MockPad {
    fn apply(&self, id: u32, out: &PadOutput) {
        self.applied.lock().unwrap_or_else(PoisonError::into_inner).push((id, out.clone()));
    }
    fn haptic(&self, id: u32, pcm: &[u8; 120]) {
        self.haptics.lock().unwrap_or_else(PoisonError::into_inner).push((id, *pcm));
    }
}
