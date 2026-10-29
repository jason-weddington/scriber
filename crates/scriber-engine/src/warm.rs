//! Keeping the cleanup model loaded while the cleanup setting is on: a
//! [`Warmer`] port and the `scriber-warm` thread that loads the wanted model,
//! renews its keep-alive every `keep_warm_every`, and releases a model that is
//! no longer wanted. Every failure is silent: the next tick retries.

use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::Duration;

use scriber_core::model::Settings;
use scriber_core::ports::PortError;
use scriber_core::sync::lock;

/// Loads a model and keeps it loaded, or unloads it (the Ollama adapter).
pub trait Warmer {
    /// Loads `model` and keeps it loaded for the adapter's keep-alive from
    /// now.
    ///
    /// # Errors
    ///
    /// The server cannot be reached, refuses, or is too slow.
    fn warm(&self, model: &str) -> Result<(), PortError>;

    /// Unloads `model` now.
    ///
    /// # Errors
    ///
    /// The server cannot be reached, refuses, or is too slow.
    fn release(&self, model: &str) -> Result<(), PortError>;
}

/// What the keep-warm thread works towards.
#[derive(Default)]
pub(crate) struct WarmState {
    /// The model to keep loaded, or none while cleanup is off.
    wanted: Option<String>,
    /// The model last warmed and not yet released.
    loaded: Option<String>,
    /// Set at shutdown or drop: no warm starts afterwards.
    stopped: bool,
}

/// The state the engine and the keep-warm thread share.
type Shared = Arc<(Mutex<WarmState>, Condvar)>;

/// The engine's side of the keep-warm thread.
pub(crate) struct KeepWarm {
    shared: Shared,
    warmer: Arc<dyn Warmer + Send + Sync>,
}

impl KeepWarm {
    /// Spawns the `scriber-warm` thread over `warmer`, renewing every
    /// `every`. Nothing is wanted until [`Self::refresh`]. A spawn failure
    /// leaves keep-warm off.
    pub(crate) fn start(warmer: Arc<dyn Warmer + Send + Sync>, every: Duration) -> Self {
        let shared: Shared = Arc::default();
        let (thread_shared, thread_warmer) = (Arc::clone(&shared), Arc::clone(&warmer));
        let _spawned = std::thread::Builder::new()
            .name("scriber-warm".to_string())
            .spawn(move || run(&thread_shared, &*thread_warmer, every));
        Self { shared, warmer }
    }

    /// Reads the settings with `read` and wakes the thread when the wanted
    /// model changed; does nothing once stopped.
    pub(crate) fn refresh(&self, read: impl FnOnce() -> Settings) {
        let (mutex, condvar) = &*self.shared;
        let mut state = lock(mutex);
        let wanted = (!state.stopped)
            .then(read)
            .map(|s| s.cleanup.then_some(s.cleanup_model));
        if let Some(wanted) = wanted
            && wanted != state.wanted
        {
            state.wanted = wanted;
            condvar.notify_all();
        }
    }

    /// Stops the thread and releases the loaded model on this thread, best
    /// effort; never waits for the thread.
    pub(crate) fn shutdown(&self) {
        let (mutex, condvar) = &*self.shared;
        let mut state = lock(mutex);
        state.stopped = true;
        let loaded = state.loaded.take();
        condvar.notify_all();
        drop(state);
        if let Some(model) = loaded {
            let _released = self.warmer.release(&model);
        }
    }
}

impl Drop for KeepWarm {
    fn drop(&mut self) {
        let (mutex, condvar) = &*self.shared;
        lock(mutex).stopped = true;
        condvar.notify_all();
    }
}

/// The keep-warm thread: every warmer call is made with the lock dropped.
fn run(shared: &(Mutex<WarmState>, Condvar), warmer: &dyn Warmer, every: Duration) {
    let (mutex, condvar) = shared;
    let mut due = true;
    loop {
        let mut state = lock(mutex);
        if state.stopped {
            return;
        }
        if state.loaded != state.wanted
            && let Some(old) = state.loaded.take()
        {
            drop(state);
            let _released = warmer.release(&old);
            continue;
        }
        if let Some(model) = state.wanted.clone()
            && (due || state.loaded.as_ref() != Some(&model))
        {
            state.loaded = Some(model.clone());
            drop(state);
            let _warmed = warmer.warm(&model);
            due = false;
            let mut after = lock(mutex);
            if after.stopped {
                after.loaded = None;
                drop(after);
                let _released = warmer.release(&model);
                return;
            }
            continue;
        }
        let idle = |s: &mut WarmState| !s.stopped && s.loaded == s.wanted;
        if state.wanted.is_some() {
            let (_state, waited) = condvar
                .wait_timeout_while(state, every, idle)
                .unwrap_or_else(PoisonError::into_inner);
            if waited.timed_out() {
                due = true;
            }
        } else {
            let _state = condvar
                .wait_while(state, idle)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }
}
