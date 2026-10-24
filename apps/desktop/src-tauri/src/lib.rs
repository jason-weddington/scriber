//! Scriber's Tauri 2 shell: real ports and commands over `scriber-core` (see `docs/design.md`).
#![forbid(unsafe_code)]

use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_clipboard_manager::ClipboardExt;
use ureq::tls::{RootCerts, TlsConfig, TlsProvider};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

use scriber_core::dictation::{Dictation, Job, work};
use scriber_core::level::{Tap, feeder};
use scriber_core::model::{Action, Snapshot};
use scriber_core::ports::{Captured, Clipboard, Downloader, Files, JobQueue, PortError, Ports};
use scriber_core::ports::{Recorder, Transcriber, fail};
use scriber_core::store::MODEL;
use scriber_core::sync::{LoadOnce, lock};

/// Event name for a snapshot that changed in the background.
pub const DICTATION_EVENT: &str = "dictation-changed";

/// Event name for the input level while recording (`scriber_core::level`).
pub const LEVEL_EVENT: &str = "dictation-level";

impl Recorder for App {
    fn start(&self) -> Result<(), PortError> {
        let (host, none) = (cpal::default_host(), fail("there is no input device"));
        let device = host.default_input_device().ok_or(none)?;
        let config = device.default_input_config().map_err(fail)?;
        let (channels, rate) = (config.channels(), config.sample_rate());
        let tap = Arc::new(Mutex::new(Tap::new(channels, rate)));
        let handle = self.handle.clone();
        // An emit (here, in `send`) fails only once the webview is gone: no one hears.
        let emit = move |level| handle.emit(LEVEL_EVENT, level).unwrap_or_default();
        let deliver = feeder(Arc::clone(&tap), emit);
        // A dropout loses a few milliseconds of sound; the segment goes on.
        let stream = device.build_input_stream(config.into(), deliver, |_| {}, None);
        let stream = stream.map_err(fail)?;
        stream.play().map_err(fail)?;
        *lock(&self.capture) = Some((stream, tap));
        Ok(())
    }

    fn stop(&self) -> Result<Captured, PortError> {
        let (stream, tap) = lock(&self.capture).take().ok_or(fail("not recording"))?;
        drop(stream);
        Ok(lock(&tap).take())
    }
}

/// whisper.cpp on Metal; the model loads once (warm-up or first use) and stays.
pub struct Whisper(PathBuf, LoadOnce<WhisperContext>);

impl Whisper {
    /// A transcriber for the ggml model at `path`.
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        whisper_rs::install_logging_hooks();
        Self(path, LoadOnce::default())
    }

    fn load(&self) -> Result<WhisperContext, PortError> {
        let params = WhisperContextParameters::default();
        WhisperContext::new_with_params(&self.0, params).map_err(fail)
    }
}

impl Transcriber for Whisper {
    fn missing_model(&self) -> Option<String> {
        (!self.0.is_file()).then(|| self.0.display().to_string())
    }

    fn warm(&self) -> Result<(), PortError> {
        self.1.with(&mut || self.load(), &mut |_| Ok(()))
    }

    fn transcribe(&self, samples: &[f32], prompt: Option<&str>) -> Result<String, PortError> {
        self.1
            .with(&mut || self.load(), &mut |m| decode(m, samples, prompt))
    }
}

/// Beam search (beam 5), English, non-speech tokens suppressed.
fn decode(model: &WhisperContext, pcm: &[f32], prompt: Option<&str>) -> Result<String, PortError> {
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
    let mut state = model.create_state().map_err(fail)?;
    state.full(params, pcm).map_err(fail)?;
    let text = state.as_iter().map(|s| s.to_str_lossy().map(String::from));
    text.collect::<Result<String, _>>().map_err(fail)
}

/// The session and every real port, shared by the commands and the jobs;
/// `capture` holds cpal's stream and the capture it feeds while recording.
pub struct App {
    dictation: Mutex<Dictation>,
    capture: Mutex<Option<(cpal::Stream, Arc<Mutex<Tap>>)>>,
    whisper: Whisper,
    data: PathBuf,
    handle: AppHandle,
}

impl Files for App {
    fn read(&self, path: &str) -> Result<Vec<u8>, PortError> {
        std::fs::read(self.data.join(path)).map_err(fail)
    }

    fn write(&self, path: &str, bytes: &[u8]) -> Result<(), PortError> {
        let file = self.data.join(path);
        std::fs::create_dir_all(file.parent().unwrap_or(&self.data)).map_err(fail)?;
        std::fs::write(file, bytes).map_err(fail)
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

    fn append(&self, path: &str, bytes: &[u8]) -> Result<(), PortError> {
        let file = OpenOptions::new().append(true).open(self.data.join(path));
        file.map_err(fail)?.write_all(bytes).map_err(fail)
    }

    fn rename(&self, from: &str, to: &str) -> Result<(), PortError> {
        std::fs::rename(self.data.join(from), self.data.join(to)).map_err(fail)
    }
}

impl Downloader for App {
    /// HTTPS through Security.framework, trusting the system's roots.
    fn open(&self, url: &str) -> Result<Box<dyn Read>, PortError> {
        let tls = TlsConfig::builder().provider(TlsProvider::NativeTls);
        let tls = tls.root_certs(RootCerts::PlatformVerifier).build();
        let agent = ureq::Agent::config_builder().tls_config(tls).build();
        let response = agent.new_agent().get(url).call().map_err(fail)?;
        Ok(Box::new(response.into_body().into_reader()))
    }
}

impl Clipboard for App {
    fn write(&self, text: &str) -> Result<(), PortError> {
        self.handle.clipboard().write_text(text).map_err(fail)
    }
}

impl JobQueue for App {
    /// A blocking-pool thread per job; the model's lock runs one at a time.
    fn send(&self, job: Job) -> Result<(), PortError> {
        let state = self.handle.try_state::<Arc<App>>();
        let app = Arc::clone(&*state.ok_or(fail("the app is starting"))?);
        let _task = tauri::async_runtime::spawn_blocking(move || {
            let report = |s: Snapshot| app.handle.emit(DICTATION_EVENT, s).unwrap_or_default();
            work(&app.dictation, &job, &app.ports(), &report);
        });
        Ok(())
    }
}

impl App {
    fn ports(&self) -> Ports<'_> {
        Ports::new(self, &self.whisper, self, self, self, self)
    }
}

/// The bridge's commands, in a module: `#[tauri::command]` code trips a lint.
#[expect(clippy::unreachable, reason = "macro expansion")]
mod commands {
    use super::{Action, App, Arc, Snapshot, State, lock};

    /// `dictation_state`: the session as it stands (never an error).
    #[tauri::command]
    pub(crate) async fn dictation_state(app: State<'_, Arc<App>>) -> Result<Snapshot, String> {
        Ok(lock(&app.dictation).snapshot(&app.ports()))
    }

    /// `dictation_act`: one action (`text` is the edited transcript for `copy`).
    #[tauri::command]
    pub(crate) async fn dictation_act(
        app: State<'_, Arc<App>>,
        action: Action,
        text: String,
    ) -> Result<Snapshot, String> {
        let snapshot = lock(&app.dictation).apply(action, &text, &app.ports());
        snapshot.map_err(|e| e.to_string())
    }
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
            let data = app.path().app_data_dir()?;
            let state = Arc::new(App {
                dictation: Mutex::default(),
                capture: Mutex::default(),
                whisper: Whisper::new(data.join(MODEL)),
                data,
                handle: app.handle().clone(),
            });
            let _first = app.manage(Arc::clone(&state));
            // Once managed: the jobs `launch` queues look the state up.
            *lock(&state.dictation) = Dictation::launch(&state.ports());
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
