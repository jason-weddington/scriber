//! Scriber's Tauri 2 shell: a thin client over `scriber-engine`, with the
//! clipboard adapter and the bridge's commands (see `docs/design.md`).
#![forbid(unsafe_code)]

use std::sync::Arc;

use scriber_engine::{
    Action, Clipboard, Engine, EngineConfig, Event, InputDevices, LibraryEntry, PortError,
    Settings, Snapshot, fail,
};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_clipboard_manager::ClipboardExt;

/// Event name for a snapshot that changed in the background.
pub const DICTATION_EVENT: &str = "dictation-changed";

/// Event name for the input level while recording (`scriber_core::level`).
pub const LEVEL_EVENT: &str = "dictation-level";

/// The system clipboard, through Tauri's clipboard plugin.
struct SystemClipboard(AppHandle);

impl Clipboard for SystemClipboard {
    fn write(&self, text: &str) -> Result<(), PortError> {
        self.0.clipboard().write_text(text).map_err(fail)
    }
}

/// The bridge's commands, in a module: `#[tauri::command]` code trips a lint.
#[expect(clippy::unreachable, reason = "macro expansion")]
mod commands {
    use super::{Action, Engine, InputDevices, LibraryEntry, Settings, Snapshot, State};

    /// `dictation_state`: the session as it stands (never an error).
    #[tauri::command]
    pub(crate) async fn dictation_state(engine: State<'_, Engine>) -> Result<Snapshot, String> {
        Ok(engine.state())
    }

    /// `dictation_act`: one action (`text` is the edited transcript for `copy`).
    #[tauri::command]
    pub(crate) async fn dictation_act(
        engine: State<'_, Engine>,
        action: Action,
        text: String,
    ) -> Result<Snapshot, String> {
        engine.act(action, &text).map_err(|e| e.to_string())
    }

    /// `library_list`: the recording library, newest first.
    #[tauri::command]
    pub(crate) async fn library_list(
        engine: State<'_, Engine>,
    ) -> Result<Vec<LibraryEntry>, String> {
        Ok(engine.library())
    }

    /// `library_get`: one library entry's transcript.
    #[tauri::command]
    pub(crate) async fn library_get(
        engine: State<'_, Engine>,
        id: String,
    ) -> Result<String, String> {
        engine.library_transcript(&id).map_err(|e| e.to_string())
    }

    /// `library_copy`: one library entry's transcript, onto the clipboard.
    #[tauri::command]
    pub(crate) async fn library_copy(engine: State<'_, Engine>, id: String) -> Result<(), String> {
        engine.library_copy(&id).map_err(|e| e.to_string())
    }

    /// `settings_get`: the user's settings.
    #[tauri::command]
    pub(crate) async fn settings_get(engine: State<'_, Engine>) -> Result<Settings, String> {
        Ok(engine.settings())
    }

    /// `settings_set`: saves the user's settings and answers with them.
    #[tauri::command]
    pub(crate) async fn settings_set(
        engine: State<'_, Engine>,
        settings: Settings,
    ) -> Result<Settings, String> {
        engine.set_settings(settings).map_err(|e| e.to_string())
    }

    /// `vocabulary_open`: opens the glossary in the default text editor, creating it empty first.
    #[tauri::command]
    pub(crate) async fn vocabulary_open(engine: State<'_, Engine>) -> Result<(), String> {
        engine.open_vocabulary().map_err(|e| e.to_string())
    }

    /// `learned_vocabulary_open`: opens the learned list in the default text editor, creating it empty first.
    #[tauri::command]
    pub(crate) async fn learned_vocabulary_open(engine: State<'_, Engine>) -> Result<(), String> {
        engine.open_learned_vocabulary().map_err(|e| e.to_string())
    }

    /// `ollama_models`: the installed Ollama models' names that can generate text (embedding models hidden), sorted.
    #[tauri::command]
    pub(crate) async fn ollama_models(engine: State<'_, Engine>) -> Result<Vec<String>, String> {
        engine.ollama_models().map_err(|e| e.to_string())
    }

    /// `input_devices`: the Mac's default input device and every other one, by name.
    #[tauri::command]
    pub(crate) async fn input_devices(engine: State<'_, Engine>) -> Result<InputDevices, String> {
        engine.input_devices().map_err(|e| e.to_string())
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
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let handle = app.handle().clone();
            // An emit fails only once the webview is gone: no one hears.
            let events = Arc::new(move |e: Event| match e {
                Event::Level(l) => handle.emit(LEVEL_EVENT, l).unwrap_or_default(),
                Event::State(s) => handle.emit(DICTATION_EVENT, s).unwrap_or_default(),
            });
            let config = EngineConfig {
                library: true,
                clipboard: Some(Arc::new(SystemClipboard(app.handle().clone()))),
                ..EngineConfig::new(app.path().app_data_dir()?)
            };
            let _first = app.manage(Engine::open(config, events));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::dictation_state,
            commands::dictation_act,
            commands::library_list,
            commands::library_get,
            commands::library_copy,
            commands::settings_get,
            commands::settings_set,
            commands::vocabulary_open,
            commands::learned_vocabulary_open,
            commands::ollama_models,
            commands::input_devices,
        ])
        .build(tauri::generate_context!())?;
    // The engine is shut down on Exit (Cmd-Q, the menu Quit, the last window
    // closed), which runs before AppKit's exit(), so the Whisper Metal
    // context is freed first; freed at exit, it aborts (design.md).
    app.run(|handle, event| {
        if let tauri::RunEvent::Exit = event
            && let Some(engine) = handle.try_state::<Engine>()
        {
            let _report = engine.shutdown();
        }
    });
    Ok(())
}
