//! Mac exit check, never run by the gate: opens the engine on a local model,
//! waits for the warm-up to load it (a Metal context on macOS), shuts the
//! engine down and then calls `exit(0)` with the engine still alive. Without
//! the shutdown, whisper.cpp's Metal device, a C++ static freed at exit,
//! aborts the process (exit status 134). `scripts/exit-check.sh` runs this
//! from the repo root and fails unless it exits 0:
//!
//! ```sh
//! cargo run --release -p scriber-engine --example exit_check -- <model.bin> [--no-shutdown]
//! ```

use std::io::Write;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use scriber_engine::{Engine, EngineConfig, Event, ModelSource};

/// How long the warm-up may take to report.
const WARM_UP: Duration = Duration::from_secs(300);

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let usage = "usage: exit_check <model.bin> [--no-shutdown]";
    let model = args.next().ok_or(usage)?;
    let shutdown = match args.next().as_deref() {
        None => true,
        Some("--no-shutdown") => false,
        Some(_) => return Err(usage.into()),
    };
    let data = tempfile::TempDir::new()?;
    let mut config = EngineConfig::new(data.path());
    config.model = ModelSource::Local(model.into());
    let (tx, rx) = mpsc::channel();
    let tx = Mutex::new(tx);
    // Forward to a channel: never call the engine from the callback.
    let events = Arc::new(move |e: Event| {
        let _sent = tx.lock().unwrap_or_else(PoisonError::into_inner).send(e);
    });
    let engine = Engine::open(config, events);

    // The warm-up is the only job queued for a present local model, so the
    // first state reported is its outcome.
    let started = Instant::now();
    let warm = loop {
        let left = WARM_UP.saturating_sub(started.elapsed());
        match rx.recv_timeout(left) {
            Ok(Event::State(s)) => break s,
            Ok(Event::Level(_)) => {}
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => {
                return Err("exit_check: warm-up did not report within 300 s".into());
            }
        }
    };
    if let Some(e) = warm.model_error {
        return Err(format!("exit_check: the model did not load: {e}").into());
    }

    let mut out = std::io::stdout();
    let (called, freed_model, paused_recording, shutdown_ms) = if shutdown {
        let timer = Instant::now();
        let report = engine.shutdown();
        let ms = timer.elapsed().as_millis();
        if !report.freed_model {
            return Err("exit_check: shutdown freed no model".into());
        }
        ("called", report.freed_model, report.paused_recording, ms)
    } else {
        ("skipped", false, false, 0)
    };
    writeln!(
        out,
        "exit_check: shutdown={called} freed_model={freed_model} paused_recording={paused_recording} shutdown_ms={shutdown_ms}"
    )?;
    out.flush()?;
    exit_with_the_engine_alive(&engine)
}

/// Exits 0 while `engine` (and its job threads) are still alive.
#[expect(
    clippy::disallowed_methods,
    clippy::exit,
    reason = "the check is that exit() with the engine alive does not abort"
)]
fn exit_with_the_engine_alive(_engine: &Engine) -> ! {
    std::process::exit(0)
}
