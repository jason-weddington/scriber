//! The Scriber desktop app: a Tauri 2 shell around `scriber-core`.
//!
//! Each command in [`commands`] forwards to the core and maps its error to a
//! string; the real implementations of the core's ports (the filesystem, the
//! clock, the progress sink) live here. The bridge contract (command names,
//! argument names, JSON shapes, events) is `docs/design.md`; nothing here
//! decides anything.
#![forbid(unsafe_code)]

use std::path::Path;
use std::time::Instant;

use tauri::{AppHandle, Emitter, Manager};

// The core's imports are a group of their own, so rustfmt keeps the order
// whatever the app is named.
use scriber_core::model::ScanProgress;
use scriber_core::ports::{Clock, DirEntry, EntryKind, Host, HostError, ProgressSink};

/// Event name for [`ScanProgress`] updates.
pub const SCAN_PROGRESS_EVENT: &str = "scan-progress";

/// The real filesystem.
struct FsHost;

impl Host for FsHost {
    fn read_dir(&self, path: &Path) -> Result<Vec<DirEntry>, HostError> {
        let fail = |e: std::io::Error| HostError(e.to_string());
        let mut entries = Vec::new();
        for entry in std::fs::read_dir(path).map_err(fail)? {
            let entry = entry.map_err(fail)?;
            // `DirEntry::file_type` does not follow symbolic links.
            let file_type = entry.file_type().map_err(fail)?;
            let kind = if file_type.is_dir() {
                EntryKind::Folder
            } else if file_type.is_file() {
                EntryKind::File {
                    size: entry.metadata().map_err(fail)?.len(),
                }
            } else {
                EntryKind::Other
            };
            entries.push(DirEntry {
                name: entry.file_name().to_string_lossy().into_owned(),
                kind,
            });
        }
        Ok(entries)
    }
}

/// Monotonic time since the clock was made.
struct SystemClock(Instant);

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        u64::try_from(self.0.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

/// Forwards progress to the webview.
struct WebviewSink(AppHandle);

impl ProgressSink for WebviewSink {
    fn progress(&self, progress: &ScanProgress) {
        // An emit fails only when the webview is gone, and then nobody is
        // listening anyway.
        self.0
            .emit(SCAN_PROGRESS_EVENT, progress)
            .unwrap_or_default();
    }
}

/// The Tauri commands, one per entry in the design doc's bridge contract.
///
/// They live in a nested module because `#[tauri::command]` on a `pub fn`
/// at the crate root collides with its own generated macro (E0255).
pub mod commands {
    use std::path::Path;
    use std::time::Instant;

    use tauri::AppHandle;

    use scriber_core::model::{AppInfo, ScanSummary};
    use scriber_core::ports::Ports;
    use scriber_core::scan::scan;

    use super::{FsHost, SystemClock, WebviewSink};

    /// `app_info`.
    #[tauri::command]
    #[must_use]
    pub fn app_info() -> AppInfo {
        AppInfo {
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }

    /// `scan_folder`: scans `path`, emitting progress events while it runs.
    ///
    /// # Errors
    ///
    /// The folder cannot be read, or the blocking task itself fails.
    #[tauri::command]
    pub async fn scan_folder(app: AppHandle, path: String) -> Result<ScanSummary, String> {
        // The walk blocks on the filesystem, so it runs off the async
        // runtime's worker threads.
        tauri::async_runtime::spawn_blocking(move || {
            let clock = SystemClock(Instant::now());
            let sink = WebviewSink(app);
            let ports = Ports {
                host: &FsHost,
                clock: &clock,
                sink: &sink,
            };
            scan(ports, Path::new(&path)).map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| e.to_string())?
    }
}

/// Brings the existing window forward when the app is launched again.
fn focus_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        // Each is best effort: a window that cannot be raised is still there.
        w.unminimize().unwrap_or_default();
        w.show().unwrap_or_default();
        w.set_focus().unwrap_or_default();
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
        // First, so a second launch hands over and exits before it builds
        // anything else.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            focus_main(app);
        }))
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            commands::app_info,
            commands::scan_folder,
        ])
        .build(tauri::generate_context!())?;
    // Quitting must stop everything the app started (child processes,
    // background work): a feature that starts any handles
    // `RunEvent::ExitRequested` and `RunEvent::Exit` here. The example starts
    // nothing that outlives a command.
    app.run(|_handle, _event| {});
    Ok(())
}
