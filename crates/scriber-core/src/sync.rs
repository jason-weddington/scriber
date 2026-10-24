//! Shared state for the app's threads: a lock that carries on past a panic,
//! and a value that loads once, on first use, for every caller, and stays
//! until it is closed.
//!
//! The speech model is the reason [`LoadOnce`] exists. It takes seconds to
//! load, so the app warms it at launch on a worker; a transcription that
//! arrives while that load is still running must wait for it rather than
//! load a second copy, and a load that fails must be tried again next time.
//! At shutdown the model is freed with [`LoadOnce::close`], and nothing loads
//! it again.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

/// Locks `m`, carrying on past a panic on another thread that held it: the
/// values the app keeps behind locks stay valid between statements.
pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The refusal of a [`LoadOnce`] that has been closed: its value is freed and
/// is never loaded again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Closed;

/// A value loaded on first use and kept until [`LoadOnce::close`]. Callers
/// are served one at a time: one that arrives during a load waits for it and
/// then uses its result.
pub struct LoadOnce<T> {
    value: Mutex<Option<T>>,
    closed: AtomicBool,
}

impl<T> Default for LoadOnce<T> {
    fn default() -> Self {
        Self {
            value: Mutex::new(None),
            closed: AtomicBool::new(false),
        }
    }
}

impl<T> LoadOnce<T> {
    /// Runs `work` on the value, loading it with `load` first if no earlier
    /// call has loaded it. The value stays loaded whatever `work` returns,
    /// until [`LoadOnce::close`].
    ///
    /// # Errors
    ///
    /// The value has been closed (or is closing), so neither `load` nor
    /// `work` runs; `load` failed (the value stays unloaded, so the next
    /// call loads again); or `work` failed.
    pub fn with<R, E: From<Closed>>(
        &self,
        load: &mut dyn FnMut() -> Result<T, E>,
        work: &mut dyn FnMut(&mut T) -> Result<R, E>,
    ) -> Result<R, E> {
        let mut held = lock(&self.value);
        if self.closed.load(Ordering::SeqCst) {
            return Err(E::from(Closed));
        }
        let value = match &mut *held {
            Some(value) => value,
            empty => empty.insert(load()?),
        };
        work(value)
    }

    /// Frees the value and refuses every later call, including callers
    /// already waiting for it. Waits for a call already running (a load or a
    /// long `work`) to finish first; the value is dropped before this
    /// returns. Returns whether a loaded value was freed: false when nothing
    /// was loaded or it was already closed.
    #[must_use]
    pub fn close(&self) -> bool {
        self.closed.store(true, Ordering::SeqCst);
        let taken = lock(&self.value).take();
        let freed = taken.is_some();
        drop(taken);
        freed
    }
}
