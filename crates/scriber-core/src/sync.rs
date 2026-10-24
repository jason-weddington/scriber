//! Shared state for the app's threads: a lock that carries on past a panic,
//! and a value that loads once, on first use, for every caller.
//!
//! The speech model is the reason [`LoadOnce`] exists. It takes seconds to
//! load, so the app warms it at launch on a worker; a transcription that
//! arrives while that load is still running must wait for it rather than
//! load a second copy, and a load that fails must be tried again next time.

use std::sync::{Mutex, MutexGuard, PoisonError};

/// Locks `m`, carrying on past a panic on another thread that held it: the
/// values the app keeps behind locks stay valid between statements.
pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A value loaded on first use and kept. Callers are served one at a time:
/// one that arrives during a load waits for it and then uses its result.
pub struct LoadOnce<T>(Mutex<Option<T>>);

impl<T> Default for LoadOnce<T> {
    fn default() -> Self {
        Self(Mutex::new(None))
    }
}

impl<T> LoadOnce<T> {
    /// Runs `work` on the value, loading it with `load` first if no earlier
    /// call has loaded it. The value stays loaded whatever `work` returns.
    ///
    /// # Errors
    ///
    /// `load` failed (the value stays unloaded, so the next call loads
    /// again), or `work` failed.
    pub fn with<R, E>(
        &self,
        load: &mut dyn FnMut() -> Result<T, E>,
        work: &mut dyn FnMut(&mut T) -> Result<R, E>,
    ) -> Result<R, E> {
        let mut held = lock(&self.0);
        let value = match &mut *held {
            Some(value) => value,
            empty => empty.insert(load()?),
        };
        work(value)
    }
}
