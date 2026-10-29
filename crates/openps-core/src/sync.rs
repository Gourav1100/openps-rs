//! Locking that survives a panic elsewhere.
//!
//! `Mutex::lock().unwrap()` turns one panic (which the core now contains) into a permanent failure: the poisoned lock
//! makes every later caller panic as well, so the whole application stops working until restarted. The data behind
//! these locks is plain state that stays usable after a panic, so recover it instead.

use std::sync::{Mutex, MutexGuard, PoisonError};

pub trait LockExt<T> {
    fn lock_ok(&self) -> MutexGuard<'_, T>;
}

impl<T> LockExt<T> for Mutex<T> {
    fn lock_ok(&self) -> MutexGuard<'_, T> {
        self.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn a_poisoned_lock_is_recovered_not_propagated() {
        let m = Arc::new(Mutex::new(vec![1, 2, 3]));
        let m2 = m.clone();
        let _ = std::thread::spawn(move || {
            let _g = m2.lock().unwrap();
            panic!("die while holding the lock");
        })
        .join();
        assert!(m.lock().is_err(), "precondition: the mutex is poisoned");
        assert_eq!(*m.lock_ok(), vec![1, 2, 3]);
        m.lock_ok().push(4); // and it stays usable
        assert_eq!(m.lock_ok().len(), 4);
    }
}
