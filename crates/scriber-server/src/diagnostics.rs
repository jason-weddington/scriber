//! Sanitized structured diagnostics for `scriber-server`.
//!
//! Every diagnostic event carries only generated connection identifiers,
//! fixed reason codes, counts, bind/auth metadata and shutdown report flags.
//! Secrets (`SCRIBER_TOKEN`), raw client frames, PCM payloads and transcript
//! text are structurally excluded from every variant.

use std::io::Write;
use std::sync::Arc;
#[cfg(test)]
use std::sync::{Mutex, PoisonError};

use serde::Serialize;

/// Bounded per-connection audio frame counters reported on close.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub(crate) struct AudioCounts {
    /// Owner PCM16 frames pushed to `StreamRecorder` while recording.
    pub(crate) accepted_frames: u64,
    /// Total 16 kHz mono samples accepted into `StreamRecorder`.
    pub(crate) accepted_samples: u64,
    /// Valid PCM16 frames dropped because the connection did not own recording.
    pub(crate) dropped_nonowner_frames: u64,
    /// Valid PCM16 frames dropped because the session was not in `recording`.
    pub(crate) dropped_out_of_phase_frames: u64,
    /// Authenticated binary frames rejected for having an odd byte length.
    pub(crate) invalid_odd_frames: u64,
}

/// One sanitized structured diagnostic event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub(crate) enum DiagnosticEvent {
    /// The server bound its listener, opened the engine and is ready to serve.
    StartupReady {
        /// Actual bound socket address.
        bind: String,
        /// `"token"` or `"no_token"`.
        auth_mode: &'static str,
        /// Whether `SCRIBER_WEB_DIR` exists as a directory at startup.
        static_available: bool,
    },
    /// Startup or serving failed at a fixed stage.
    StartupFailed {
        /// `"config"`, `"bind"`, `"model_folder"`, or `"serve"`.
        stage: &'static str,
        /// Fixed machine-readable reason code.
        reason: &'static str,
    },
    /// WebSocket authentication outcome for one connection.
    Auth {
        /// Generated connection identifier.
        conn_id: u64,
        /// `"accepted"` or `"rejected"`.
        outcome: &'static str,
        /// Fixed machine-readable reason code.
        reason: &'static str,
    },
    /// Recording ownership state transition.
    Ownership {
        /// Generated connection identifier.
        conn_id: u64,
        /// `"granted"`, `"cleared"`, or `"released"`.
        outcome: &'static str,
        /// Action or `"disconnect"` that caused the change.
        action: &'static str,
        /// Resulting session phase name.
        phase: &'static str,
    },
    /// A subscriber queue overflowed and forced disconnect.
    SubscriberLag {
        /// Generated connection identifier.
        conn_id: u64,
        /// Fixed reason code (`"subscriber_lag"`).
        reason: &'static str,
        /// Bounded subscriber queue capacity.
        queue_capacity: usize,
    },
    /// Terminal outcome of a connection's ordered writer task.
    WriterOutcome {
        /// Generated connection identifier.
        conn_id: u64,
        /// Fixed machine-readable reason code.
        reason: &'static str,
    },
    /// Terminal summary emitted when a WebSocket connection finishes.
    ConnectionClosed {
        /// Generated connection identifier.
        conn_id: u64,
        /// Fixed machine-readable close reason code.
        reason: &'static str,
        /// Bounded audio frame counts for the connection's lifetime.
        audio: AudioCounts,
    },
    /// Graceful shutdown outcome from `Engine::shutdown`.
    Shutdown {
        /// `"ctrl_c"`, `"sigterm"`, `"signal"`, or `"serve_error"`.
        trigger: &'static str,
        /// `ShutdownReport::first`.
        first: bool,
        /// `ShutdownReport::paused_recording`.
        paused_recording: bool,
        /// `ShutdownReport::freed_model`.
        freed_model: bool,
    },
}

/// Nonblocking sink for [`DiagnosticEvent`]s.
#[derive(Clone)]
pub(crate) struct Diagnostics {
    sink: Arc<dyn Fn(&DiagnosticEvent) + Send + Sync>,
}

impl Diagnostics {
    /// Emits compact JSON lines to `stderr`.
    #[must_use]
    pub(crate) fn stderr() -> Self {
        Self::new(move |event| write_json_line(&mut std::io::stderr(), event))
    }

    /// Emits compact JSON lines to an arbitrary writer (used in tests).
    #[cfg(test)]
    #[must_use]
    pub(crate) fn to_writer<W: Write + Send + 'static>(writer: Arc<Mutex<W>>) -> Self {
        Self::new(move |event| {
            let mut guard = writer.lock().unwrap_or_else(PoisonError::into_inner);
            write_json_line(&mut *guard, event);
        })
    }

    /// Records events in memory for assertions.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn recording() -> (Self, Arc<Mutex<Vec<DiagnosticEvent>>>) {
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&events);
        let diag = Self::new(move |event| {
            sink.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(event.clone());
        });
        (diag, events)
    }

    /// Constructs a sink from a nonblocking callback.
    #[must_use]
    pub(crate) fn new(sink: impl Fn(&DiagnosticEvent) + Send + Sync + 'static) -> Self {
        Self {
            sink: Arc::new(sink),
        }
    }

    /// Emits one structured diagnostic event.
    pub(crate) fn emit(&self, event: &DiagnosticEvent) {
        (self.sink)(event);
    }
}

/// Serializes `event` as one compact JSON line to `writer`, ignoring I/O errors.
fn write_json_line<W: Write>(writer: &mut W, event: &DiagnosticEvent) {
    if let Ok(mut line) = serde_json::to_string(event) {
        line.push('\n');
        let _written = writer.write_all(line.as_bytes());
    }
}
