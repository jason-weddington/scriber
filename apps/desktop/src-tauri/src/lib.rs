//! The Scriber desktop app: a Tauri 2 shell around `scriber-core`. The real
//! ports live here (cpal, whisper-rs on Metal, the data folder, the clipboard,
//! a transcription queue); each command forwards to the core. The bridge
//! contract is `docs/design.md`.
#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_clipboard_manager::ClipboardExt;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

use scriber_core::dictation::{Dictation, Job};
use scriber_core::ports::Transcriber;
use scriber_core::ports::{Captured, Clipboard, Files, JobQueue, PortError, Ports, Recorder};
use scriber_core::store::MODEL;

/// Event name for a snapshot that changed in the background.
pub const DICTATION_EVENT: &str = "dictation-changed";

fn fail(e: impl std::fmt::Display) -> PortError {
    PortError(e.to_string())
}

/// Locks, carrying on past a panic elsewhere: every value here stays valid.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// While recording: cpal's stream, the samples it delivered, channels, rate.
type Capture = (cpal::Stream, Arc<Mutex<Vec<f32>>>, u16, u32);

impl Recorder for App {
    fn start(&self) -> Result<(), PortError> {
        let (host, none) = (cpal::default_host(), fail("there is no input device"));
        let device = host.default_input_device().ok_or(none)?;
        let config = device.default_input_config().map_err(fail)?;
        let (channels, rate) = (config.channels(), config.sample_rate());
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&buffer);
        let deliver = move |data: &[f32], _: &_| lock(&sink).extend_from_slice(data);
        // A dropout loses a few milliseconds of sound; the segment goes on.
        let stream = device.build_input_stream(config.into(), deliver, |_| {}, None);
        let stream = stream.map_err(fail)?;
        stream.play().map_err(fail)?;
        *lock(&self.capture) = Some((stream, buffer, channels, rate));
        Ok(())
    }

    fn stop(&self) -> Result<Captured, PortError> {
        let running = lock(&self.capture).take().ok_or(fail("not recording"))?;
        let (stream, buffer, channels, sample_rate) = running;
        drop(stream);
        let samples = std::mem::take(&mut *lock(&buffer));
        Ok(Captured::new(samples, channels, sample_rate))
    }
}

/// whisper.cpp on Metal. The model loads on first use and stays loaded.
pub struct Whisper(PathBuf, Mutex<Option<WhisperContext>>);

impl Whisper {
    /// A transcriber for the ggml model at `path`.
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        whisper_rs::install_logging_hooks();
        Self(path, Mutex::new(None))
    }
}

impl Transcriber for Whisper {
    fn missing_model(&self) -> Option<String> {
        (!self.0.is_file()).then(|| self.0.display().to_string())
    }

    fn transcribe(&self, samples: &[f32], prompt: Option<&str>) -> Result<String, PortError> {
        let mut loaded = lock(&self.1);
        let load = || WhisperContext::new_with_params(&self.0, WhisperContextParameters::default());
        let context = match &mut *loaded {
            Some(context) => context,
            empty => empty.insert(load().map_err(fail)?),
        };
        let beam = SamplingStrategy::BeamSearch {
            beam_size: 5,
            patience: -1.0,
        };
        let mut params = FullParams::new(beam);
        params.set_language(Some("en"));
        params.set_suppress_nst(true);
        if let Some(prompt) = prompt {
            params.set_initial_prompt(prompt);
        }
        let mut state = context.create_state().map_err(fail)?;
        state.full(params, samples).map_err(fail)?;
        let text = state.as_iter().map(|s| s.to_str_lossy().map(String::from));
        text.collect::<Result<String, _>>().map_err(fail)
    }
}

/// The session and every real port, shared by the commands and the jobs.
pub struct App {
    dictation: Mutex<Dictation>,
    capture: Mutex<Option<Capture>>,
    whisper: Whisper,
    data: PathBuf,
    handle: AppHandle,
}

impl Files for App {
    fn read(&self, path: &str) -> Result<Vec<u8>, PortError> {
        std::fs::read(self.data.join(path)).map_err(fail)
    }

    fn write(&self, path: &str, bytes: &[u8]) -> Result<(), PortError> {
        std::fs::write(self.data.join(path), bytes).map_err(fail)
    }

    fn list(&self, folder: &str) -> Result<Vec<String>, PortError> {
        // Made if missing: the port promises a missing folder lists empty.
        std::fs::create_dir_all(self.data.join(folder)).map_err(fail)?;
        let entries = std::fs::read_dir(self.data.join(folder)).map_err(fail)?;
        let names = entries.flatten().map(|e| e.file_name().into_string());
        Ok(names.flatten().collect())
    }

    fn remove(&self, path: &str) -> Result<(), PortError> {
        std::fs::remove_file(self.data.join(path)).map_err(fail)
    }
}

impl Clipboard for App {
    fn write(&self, text: &str) -> Result<(), PortError> {
        self.handle.clipboard().write_text(text).map_err(fail)
    }
}

impl JobQueue for App {
    /// Transcribes on a blocking-pool thread, then reports the new snapshot.
    /// Jobs may finish in any order; the Whisper lock runs one at a time.
    fn send(&self, job: Job) -> Result<(), PortError> {
        let state = self.handle.try_state::<Arc<App>>();
        let app = Arc::clone(&*state.ok_or(fail("the app is starting"))?);
        let _task = tauri::async_runtime::spawn_blocking(move || {
            let outcome = job.run(&app.whisper);
            let snapshot = lock(&app.dictation).complete(outcome, &app.ports());
            // Fails only when the webview is gone, and then nobody is listening.
            let _sent = app.handle.emit(DICTATION_EVENT, snapshot);
        });
        Ok(())
    }
}

impl App {
    fn ports(&self) -> Ports<'_> {
        Ports::new(self, &self.whisper, self, self, self)
    }
}

/// The Tauri commands, one per entry in the design doc's bridge contract (in
/// a module: at the crate root, `#[tauri::command]` collides with itself).
#[expect(clippy::unreachable, reason = "macro expansion")]
pub mod commands {
    use std::sync::Arc;

    use tauri::State;

    use scriber_core::model::{Action, Snapshot};

    use super::{App, lock};

    /// `dictation_state`: the session as it stands.
    ///
    /// # Errors
    ///
    /// Never; async commands answer with a `Result`.
    #[tauri::command]
    pub async fn dictation_state(app: State<'_, Arc<App>>) -> Result<Snapshot, String> {
        Ok(lock(&app.dictation).snapshot(&app.ports()))
    }

    /// `dictation_act`: one action; `text` is the edited transcript for `copy`.
    ///
    /// # Errors
    ///
    /// The core refused the action, as one sentence.
    #[tauri::command]
    pub async fn dictation_act(
        app: State<'_, Arc<App>>,
        action: Action,
        text: String,
    ) -> Result<Snapshot, String> {
        let ports = app.ports();
        let snapshot = lock(&app.dictation).apply(action, &text, &ports);
        snapshot.map_err(|e| e.to_string())
    }
}

/// The shared state, with the session found on disk at launch.
fn start(handle: &AppHandle) -> Result<Arc<App>, tauri::Error> {
    let data = handle.path().app_data_dir()?;
    let whisper = Whisper::new(data.join(MODEL));
    let (dictation, capture, handle) = (Mutex::default(), Mutex::default(), handle.clone());
    let app = Arc::new(App {
        dictation,
        capture,
        whisper,
        data,
        handle,
    });
    *lock(&app.dictation) = Dictation::recover(&app.ports());
    Ok(app)
}

/// Builds and runs the app.
///
/// # Errors
///
/// Tauri could not build the app (a broken config or a missing webview).
#[expect(
    clippy::disallowed_methods,
    reason = "tauri::generate_context! expands to code that calls std::process::exit"
)]
pub fn run() -> Result<(), tauri::Error> {
    let app = tauri::Builder::default()
        // First, so a second launch focuses the open window and exits.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            if let Some(w) = app.get_webview_window("main") {
                // Best effort: a window that cannot be raised is still there.
                let shown = w.unminimize().and_then(|()| w.show());
                shown.and_then(|()| w.set_focus()).unwrap_or_default();
            }
        }))
        .plugin(tauri_plugin_clipboard_manager::init())
        .setup(|app| {
            let _first = app.manage(start(app.handle())?);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::dictation_state,
            commands::dictation_act,
        ])
        .build(tauri::generate_context!())?;
    // Quitting loses a transcription in flight, not its audio (design.md).
    app.run(|_handle, _event| {});
    Ok(())
}
