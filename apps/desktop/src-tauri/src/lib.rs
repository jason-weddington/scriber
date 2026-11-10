//! Scriber's Tauri 2 shell: a thin client over `scriber-engine`, with the clipboard adapter, the bridge's commands, the tray and the global shortcut (see `docs/design.md`).
#![forbid(unsafe_code)]

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use scriber_engine::{
    Action, Clipboard, Engine, EngineConfig, EngineError, Event, InputDevices, LibraryEntry, Phase,
    PortError, Settings, Snapshot, fail, menubar,
};
use tauri::image::Image;
use tauri::menu::{IsMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tauri::tray::{TrayIcon, TrayIconBuilder};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_clipboard_manager::ClipboardExt;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutEvent, ShortcutState};
use tauri_plugin_notification::NotificationExt;

/// Event name for a snapshot that changed in the background.
pub const DICTATION_EVENT: &str = "dictation-changed";

/// Event name for the input level while recording (`scriber_core::level`).
pub const LEVEL_EVENT: &str = "dictation-level";

/// The menu bar's template, recording and busy icons (`image-png` decodes them).
const TEMPLATE_PNG: &[u8] = include_bytes!("../icons/tray-template.png");
const RECORDING_PNG: &[u8] = include_bytes!("../icons/tray-recording.png");
const BUSY_PNG: &[u8] = include_bytes!("../icons/tray-busy.png");
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
    use super::{
        Action, AppHandle, Engine, InputDevices, LibraryEntry, Manager, Ordering, Settings, Shown,
        Snapshot, State, menubar,
    };

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

    /// `settings_set`: saves the settings, moves the global shortcut to the saved one.
    #[tauri::command]
    pub(crate) async fn settings_set(
        app: AppHandle,
        engine: State<'_, Engine>,
        settings: Settings,
    ) -> Result<Settings, String> {
        let saved = engine.set_settings(settings).map_err(|e| e.to_string())?;
        // The cached setting is why a change takes effect without a relaunch, and this line is its only proof.
        #[rustfmt::skip]
        let (outcome, error) = match app.try_state::<Shown>() {
            Some(shown) => { shown.copy_sound.store(saved.copy_sound, Ordering::SeqCst); ("cached", None) }
            None => ("failed", Some("no cached setting")),
        };
        #[rustfmt::skip]
        engine.log_menubar(&menubar::Log::Sound { trigger: "settings", phase: &engine.state().phase.to_string(), previous: None, copy_sound: saved.copy_sound, reason: "cached", outcome, error });
        super::register_shortcuts(&app, engine.inner(), "settings");
        Ok(saved)
    }

    /// `vocabulary_open`: opens the glossary in the default text editor.
    #[tauri::command]
    pub(crate) async fn vocabulary_open(engine: State<'_, Engine>) -> Result<(), String> {
        engine.open_vocabulary().map_err(|e| e.to_string())
    }

    /// `learned_vocabulary_open`: opens the learned list in the default text editor.
    #[tauri::command]
    pub(crate) async fn learned_vocabulary_open(engine: State<'_, Engine>) -> Result<(), String> {
        engine.open_learned_vocabulary().map_err(|e| e.to_string())
    }

    /// `ollama_models`: the installed text models, sorted.
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

/// The window's visibility (for the notification rule), the phase the tray last showed, the frames the pulse drew in this episode and the cached copy-sound setting.
struct Shown {
    visible: AtomicBool,
    phase: Mutex<Phase>,
    pulse_frames: AtomicU64,
    copy_sound: AtomicBool,
}
/// The tray's image for `icon`, decoded from the three checked-in images.
fn tray_image(icon: menubar::Icon) -> Option<Image<'static>> {
    let bytes = match icon {
        menubar::Icon::Template => TEMPLATE_PNG,
        menubar::Icon::Recording => RECORDING_PNG,
        menubar::Icon::Busy => BUSY_PNG,
    };
    Image::from_bytes(bytes).ok()
}

/// Sets the tray's image for `icon` and, on macOS, whether it draws as a template; the error says why it could not.
fn set_icon(tray: &TrayIcon<tauri::Wry>, icon: menubar::Icon) -> Result<(), String> {
    #[rustfmt::skip]
    let Some(image) = tray_image(icon) else { return Err("no image for the icon".to_string()); };
    tray.set_icon(Some(image)).map_err(|e| e.to_string())?;
    #[cfg(target_os = "macos")]
    tray.set_icon_as_template(icon.draws_as_template())
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Starts the menu bar's pulse: one thread for the app's life, which posts a frame twice a second.
fn spawn_pulse(app: &AppHandle) {
    let handle = app.clone();
    #[rustfmt::skip]
    let _spawned = std::thread::Builder::new().name("scriber-pulse".to_string()).spawn(move || pulse(&handle));
}

/// Posts one pulse frame per tick, on the main thread; the core says which frame is due and when none is.
fn pulse(app: &AppHandle) {
    let mut tick: u64 = 0;
    loop {
        std::thread::sleep(Duration::from_millis(menubar::PULSE_TICK_MS));
        tick = tick.wrapping_add(1);
        let posted = app.clone();
        #[rustfmt::skip]
        let _posted = app.clone().run_on_main_thread(move || pulse_frame(&posted, tick));
    }
}

/// Draws one frame of the pulse on the main thread, counting it only when the tray took it: nothing while not transcribing.
fn pulse_frame(app: &AppHandle, tick: u64) {
    #[rustfmt::skip]
    let Some(shown) = app.try_state::<Shown>() else { return; };
    #[rustfmt::skip]
    let Some(tray) = app.tray_by_id("main") else { return; };
    let phase = *shown.phase.lock().unwrap_or_else(PoisonError::into_inner);
    #[rustfmt::skip]
    let Some(icon) = menubar::icon_at(phase, tick) else { return; };
    if set_icon(&tray, icon).is_ok() {
        let _frames = shown.pulse_frames.fetch_add(1, Ordering::SeqCst);
    }
}

/// Plays the system's soft pop once and logs the outcome, on a `scriber-tray` thread so the main thread never waits for audio.
fn play_copy_sound(engine: Engine, phase: String, previous: String) {
    on_tray_thread(move || {
        #[rustfmt::skip]
        let played = std::process::Command::new("afplay").arg(menubar::COPY_SOUND).status();
        #[rustfmt::skip]
        let (outcome, error) = match played {
            Ok(status) if status.success() => ("played", None),
            Ok(status) => ("failed", Some(format!("afplay exited with {:?}", status.code()))),
            Err(e) => ("failed", Some(e.to_string())),
        };
        #[rustfmt::skip]
        engine.log_menubar(&menubar::Log::Sound { trigger: "finish", phase: &phase, previous: Some(&previous), copy_sound: true, reason: "play", outcome, error: error.as_deref() });
    });
}

/// The tray menu for `phase`: [`menubar::items`] in order, a separator after Show Scriber.
fn tray_menu(app: &AppHandle, phase: Phase) -> tauri::Result<Menu<tauri::Wry>> {
    let made = menubar::items(phase)
        .iter()
        .map(|item| MenuItem::with_id(app, item.id, item.label, item.enabled, None::<&str>))
        .collect::<tauri::Result<Vec<_>>>()?;
    let separator = PredefinedMenuItem::separator(app)?;
    let mut ordered: Vec<&dyn IsMenuItem<tauri::Wry>> = Vec::new();
    if let Some(first) = made.first() {
        ordered.push(first);
    }
    ordered.push(&separator);
    for item in made.iter().skip(1) {
        ordered.push(item);
    }
    Menu::with_items(app, &ordered)
}

/// Runs engine work on a `scriber-tray` thread: every engine method takes the session lock and Record opens the microphone, so never the main thread.
fn on_tray_thread(work: impl FnOnce() + Send + 'static) {
    let _spawned = std::thread::Builder::new()
        .name("scriber-tray".to_string())
        .spawn(work);
}

/// Registers the saved toggle and pause shortcuts, logging each attempt; after a settings save the old chords go first, so a change works without a relaunch.
#[rustfmt::skip]
fn register_shortcuts(app: &AppHandle, engine: &Engine, trigger: &'static str) {
    let shortcuts = app.global_shortcut();
    let toggle = engine.toggle_accelerator();
    let pause = engine.pause_accelerator();
    if trigger == "settings" {
        match shortcuts.unregister_all() {
            Ok(()) => log_shortcut(engine, "both", trigger, "", "unregistered", None),
            Err(e) => log_shortcut(engine, "both", trigger, "", "failed", Some(&e.to_string())),
        }
    }
    match shortcuts.on_shortcut(toggle.as_str(), |handle, _shortcut, event| on_shortcut(handle, event, Engine::toggle)) {
        Ok(()) => log_shortcut(engine, "toggle", trigger, &toggle, "registered", None),
        Err(e) => log_shortcut(engine, "toggle", trigger, &toggle, "failed", Some(&e.to_string())),
    }
    match pause {
        None => log_shortcut(engine, "pause", trigger, "", "skipped", None),
        Some(pause) => match shortcuts.on_shortcut(pause.as_str(), |handle, _shortcut, event| on_shortcut(handle, event, Engine::press_pause)) {
            Ok(()) => log_shortcut(engine, "pause", trigger, &pause, "registered", None),
            Err(e) => log_shortcut(engine, "pause", trigger, &pause, "failed", Some(&e.to_string())),
        },
    }
}

/// Writes one `shortcut` line, with the plugin's error text verbatim.
fn log_shortcut(e: &Engine, chord: &str, t: &str, accel: &str, outcome: &str, error: Option<&str>) {
    #[rustfmt::skip]
    e.log_menubar(&menubar::Log::Shortcut { chord, trigger: t, accelerator: accel, outcome, error });
}

/// One press's handler: act on Pressed only (the plugin reports Released too), on a `scriber-tray` thread.
#[rustfmt::skip]
fn on_shortcut(app: &AppHandle, event: ShortcutEvent, gesture: fn(&Engine) -> Result<Snapshot, EngineError>) {
    if event.state != ShortcutState::Pressed { return; }
    let Some(engine) = app.try_state::<Engine>() else { return; };
    let engine = engine.inner().clone();
    on_tray_thread(move || { let _result = gesture(&engine); });
}

/// Builds the menu bar icon and its menu, once, at launch.
fn install_tray(app: &AppHandle) -> tauri::Result<()> {
    let menu = tray_menu(app, app.state::<Engine>().state().phase)?;
    let mut builder = TrayIconBuilder::with_id("main")
        .tooltip("Scriber")
        .menu(&menu)
        .on_menu_event(|handle: &AppHandle, event| on_menu_event(handle, &event));
    if let Some(image) = tray_image(menubar::Icon::Template) {
        builder = builder.icon(image);
    }
    #[cfg(target_os = "macos")]
    let builder = builder.icon_as_template(true);
    let _tray = builder.build(app)?;
    Ok(())
}

/// The only writer of tray state: on the main thread for every snapshot, so it takes no session lock.
fn refresh(app: &AppHandle, snapshot: &Snapshot) {
    #[rustfmt::skip]
    let Some(shown) = app.try_state::<Shown>() else { return; };
    #[rustfmt::skip]
    let previous = std::mem::replace(&mut *shown.phase.lock().unwrap_or_else(PoisonError::into_inner), snapshot.phase);
    let mut icon_error = None;
    if let Some(tray) = app.tray_by_id("main") {
        if previous != snapshot.phase
            && let Ok(menu) = tray_menu(app, snapshot.phase)
        {
            let _set = tray.set_menu(Some(menu));
        }
        if menubar::icon(previous) != menubar::icon(snapshot.phase) {
            icon_error = set_icon(&tray, menubar::icon(snapshot.phase)).err();
        }
    }
    // At most two pulse lines per transcribing episode: a start, then a stop with its frames and its last image.
    if let Some(engine) = app.try_state::<Engine>() {
        if previous != Phase::Transcribing && snapshot.phase == Phase::Transcribing {
            let _frames = shown.pulse_frames.swap(0, Ordering::SeqCst);
            #[rustfmt::skip]
            engine.log_menubar(&menubar::Log::Pulse { phase: &snapshot.phase.to_string(), tick_ms: menubar::PULSE_TICK_MS, frames: 0, last: menubar::icon(Phase::Transcribing).name(), outcome: "started", error: None });
        } else if previous == Phase::Transcribing && snapshot.phase != Phase::Transcribing {
            let frames = shown.pulse_frames.swap(0, Ordering::SeqCst);
            #[rustfmt::skip]
            let (last, outcome) = if icon_error.is_none() { (menubar::icon(snapshot.phase).name(), "stopped") } else { (menubar::icon(previous).name(), "failed") };
            #[rustfmt::skip]
            engine.log_menubar(&menubar::Log::Pulse { phase: &snapshot.phase.to_string(), tick_ms: menubar::PULSE_TICK_MS, frames, last, outcome, error: icon_error.as_deref() });
        }
    }
    // The copy sound is decided before the notice's early return: every finish is judged here.
    if snapshot.phase == Phase::Finished
        && let Some(engine) = app.try_state::<Engine>()
    {
        let on = shown.copy_sound.load(Ordering::SeqCst);
        let decision = menubar::sound(previous, snapshot, on);
        if decision == menubar::Sound::Play {
            #[rustfmt::skip]
            play_copy_sound(engine.inner().clone(), snapshot.phase.to_string(), previous.to_string());
        } else {
            #[rustfmt::skip]
            engine.log_menubar(&menubar::Log::Sound { trigger: "finish", phase: &snapshot.phase.to_string(), previous: Some(&previous.to_string()), copy_sound: on, reason: decision.reason(), outcome: "skipped", error: None });
        }
    }
    #[rustfmt::skip]
    let Some(notice) = menubar::notice(previous, snapshot) else { return; };
    let visible = shown.visible.load(Ordering::SeqCst);
    let (outcome, error) = if visible {
        ("skipped", None)
    } else {
        match app
            .notification()
            .builder()
            .title("Scriber")
            .body(notice)
            .show()
        {
            Ok(()) => ("shown", None),
            Err(e) => ("failed", Some(e.to_string())),
        }
    };
    if let Some(engine) = app.try_state::<Engine>() {
        #[rustfmt::skip]
        engine.log_menubar(&menubar::Log::Notify { phase: &snapshot.phase.to_string(), visible, notice: Some(notice), outcome, error: error.as_deref() });
    }
}

/// Shows and focuses the window, brings the Dock icon and app menu back, and marks it visible: the one path the second launch, Reopen and Show Scriber take.
fn reveal(app: &AppHandle) {
    #[cfg(target_os = "macos")]
    let _policy = app.set_activation_policy(tauri::ActivationPolicy::Regular);
    if let Some(window) = app.get_webview_window("main") {
        window
            .unminimize()
            .and_then(|()| window.show())
            .and_then(|()| window.set_focus())
            .unwrap_or_default();
    }
    if let Some(shown) = app.try_state::<Shown>() {
        shown.visible.store(true, Ordering::SeqCst);
    }
}

/// Closing the window hides it: Scriber keeps running in the menu bar, with no Dock icon once the window is gone.
fn on_window_event(window: &tauri::Window, event: &tauri::WindowEvent) {
    let tauri::WindowEvent::CloseRequested { api, .. } = event else {
        return;
    };
    api.prevent_close();
    window.hide().unwrap_or_default();
    #[cfg(target_os = "macos")]
    let _policy = window
        .app_handle()
        .set_activation_policy(tauri::ActivationPolicy::Accessory);
    if let Some(shown) = window.app_handle().try_state::<Shown>() {
        shown.visible.store(false, Ordering::SeqCst);
    }
}

/// The tray's clicks: no decisions of its own, every engine call off the main thread, one `menubar-log.jsonl` line about it.
fn on_menu_event(app: &AppHandle, event: &MenuEvent) {
    let id = event.id().as_ref().to_string();
    if id == "quit" {
        app.exit(0);
        return;
    }
    if id == "show" {
        reveal(app);
        return;
    }
    let Some(engine) = app.try_state::<Engine>() else {
        return;
    };
    let engine = engine.inner().clone();
    on_tray_thread(move || {
        let phase = engine.state().phase;
        let result = match id.as_str() {
            "record" => Some(engine.record()),
            "pause" => Some(engine.act(Action::Pause, "")),
            "done" => Some(engine.act(Action::Done, "")),
            "discard" => Some(engine.act(Action::Discard, "")),
            _ => None,
        };
        let (outcome, error) = match result {
            None => ("unknown", None),
            Some(Ok(_)) => ("ok", None),
            Some(Err(e @ EngineError::Refused(_))) => ("refused", Some(e.to_string())),
            Some(Err(e)) => ("failed", Some(e.to_string())),
        };
        #[rustfmt::skip]
        engine.log_menubar(&menubar::Log::Menu { id: &id, phase: &phase.to_string(), outcome, error: error.as_deref() });
    });
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
    #[rustfmt::skip]
    let handler: fn(tauri::ipc::Invoke<tauri::Wry>) -> bool = tauri::generate_handler![commands::dictation_state, commands::dictation_act, commands::library_list, commands::library_get, commands::library_copy, commands::settings_get, commands::settings_set, commands::vocabulary_open, commands::learned_vocabulary_open, commands::ollama_models, commands::input_devices];
    let app = tauri::Builder::default()
        // First, so a second launch focuses the open window and exits.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            reveal(app);
        }))
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .setup(|app| {
            let handle = app.handle().clone();
            // An emit fails only once the webview is gone: no one hears.
            let events = Arc::new(move |e: Event| match e {
                Event::Level(l) => handle.emit(LEVEL_EVENT, l).unwrap_or_default(),
                Event::State(s) => {
                    handle.emit(DICTATION_EVENT, &s).unwrap_or_default();
                    // The tray and the notification are main-thread work.
                    let main = handle.clone();
                    let posted = main.clone();
                    let _posted = main.run_on_main_thread(move || refresh(&posted, &s));
                }
            });
            let config = EngineConfig {
                library: true,
                clipboard: Some(Arc::new(SystemClipboard(app.handle().clone()))),
                ..EngineConfig::new(app.path().app_data_dir()?)
            };
            let engine = Engine::open(config, events);
            let phase = engine.state().phase;
            let _first = app.manage(engine.clone());
            let _shown = app.manage(Shown {
                visible: AtomicBool::new(true),
                phase: Mutex::new(phase),
                pulse_frames: AtomicU64::new(0),
                copy_sound: AtomicBool::new(engine.settings().copy_sound),
            });
            spawn_pulse(app.handle());
            install_tray(app.handle())?;
            register_shortcuts(app.handle(), &engine, "launch");
            Ok(())
        })
        .on_window_event(on_window_event)
        .invoke_handler(handler)
        .build(tauri::generate_context!())?;
    // The engine is shut down on Exit (Cmd-Q, the menu Quit), which runs before
    // AppKit's exit(), so the Whisper Metal context is freed first (design.md).
    app.run(|handle, event| match event {
        tauri::RunEvent::Exit => {
            if let Some(engine) = handle.try_state::<Engine>() {
                let _report = engine.shutdown();
            }
        }
        #[cfg(target_os = "macos")]
        tauri::RunEvent::Reopen { .. } => reveal(handle),
        _ => {}
    });
    Ok(())
}
