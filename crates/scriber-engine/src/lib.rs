//! Scriber's dictation engine: [`scriber_core`]'s session with the real
//! adapters around it, for any Rust app.
//!
//! [`Engine::open`] takes a data folder and a [`ModelSource`], downloads the
//! speech model when it is missing (reporting progress as [`Event::State`]),
//! warms it up, and then records, pauses, resumes and stops sessions
//! transcribed locally by whisper.cpp. Jobs (transcription, warm-up,
//! download) run on plain named std threads: there is no async runtime and
//! no Tauri. The Scriber desktop app (`apps/desktop/src-tauri`) is one thin
//! client over this crate.
//!
//! With [`EngineConfig::library`] on, every finished session with a
//! transcript is kept in the data folder's recording library
//! ([`Engine::library`]): titled by its start in local time, its audio
//! encoded as M4A by macOS's `afconvert` (kept as WAV where that is
//! missing) and summarized in one sentence by a local Ollama server (an
//! excerpt of the transcript where that is unreachable). It is off by
//! default, so an app that embeds the engine keeps no audio it did not ask
//! for.
//!
//! With the `cleanup` setting on ([`Settings::cleanup`]), each finished
//! transcript goes once more to the same Ollama server and model, which fixes
//! misheard glossary terms and spoken self-corrections and answers with a
//! three-field value (the corrected transcript, its replacements and the words
//! the speaker spelled out); the raw Whisper text is used whenever that fails
//! or the reply does not pass the core's check
//! (`scriber_core::cleanup::accept_reply`).
//! While it is on, the engine also keeps the cleanup model loaded in Ollama on a thread of its own ([`Warmer`]), renewing it every [`EngineConfig::keep_warm_every`] and releasing it when cleanup turns off, the model changes or the engine shuts down.
//!
//! ```rust,no_run
//! use std::sync::{Arc, Mutex, mpsc};
//! use std::time::Duration;
//!
//! use scriber_engine::{Engine, EngineConfig, Event};
//!
//! let (tx, rx) = mpsc::channel();
//! let tx = Mutex::new(tx);
//! // Forward to a channel: never call the engine from the callback.
//! let events = Arc::new(move |e: Event| {
//!     let _sent = tx.lock().map(|tx| tx.send(e));
//! });
//! let engine = Engine::open(EngineConfig::new("/path/to/data"), events);
//! engine.record()?;
//! // ... the user speaks ...
//! let text = engine.stop_and_wait(Duration::from_secs(300))?;
//! // On macOS, always before the process exits (see `Engine::shutdown`).
//! let _report = engine.shutdown();
//! # drop((rx, text));
//! # Ok::<(), scriber_engine::EngineError>(())
//! ```

mod clock;
mod device;
mod editor;
mod encode;
pub mod extract;
mod files;
mod net;
mod ollama;
mod stream;
mod warm;
mod whisper;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError, Weak};
use std::time::{Duration, Instant};

use scriber_core::dictation::{Dictation, Job, work};
use scriber_core::library;
use scriber_core::ports::{JobQueue, Library, Ports};
use scriber_core::settings;
use scriber_core::store::{LEARNED, MODEL_FOLDER, VOCABULARY};
use scriber_core::sync::lock;
use serde::Serialize;

pub use clock::LocalClock;
pub use device::CpalRecorder;
pub use editor::{OPEN, TextEditor};
pub use encode::{AFCONVERT, Afconvert};
pub use files::DataDir;
pub use net::HttpDownloader;
pub use ollama::{
    CLEANUP_TIMEOUT, KEEP_ALIVE, KEEP_WARM_INTERVAL, Ollama, RELEASE_TIMEOUT, SUMMARY_TIMEOUT,
    SYSTEM_PROMPT, TAGS_TIMEOUT, WARM_TIMEOUT, installed_models,
};
pub use scriber_core::dictation::DictationError;
pub use scriber_core::download::Source;
pub use scriber_core::menubar;
pub use scriber_core::model::{
    Action, Download, InputDevices, LibraryEntry, Phase, Settings, Snapshot,
};
pub use scriber_core::ports::{
    Captured, Cleaner, Clipboard, Clock, Downloader, Encoder, Files, LocalTime, Phrase, PortError,
    Recorder, Summarizer, Transcriber, fail,
};
pub use stream::{STREAM_CHANNELS, STREAM_DEFAULT_LABEL, STREAM_SAMPLE_RATE, StreamRecorder};
pub use warm::Warmer;
pub use whisper::{DetailedPhrase, DetailedToken, Whisper, vad_spans};

use crate::warm::KeepWarm;

/// Where the speech model comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelSource {
    /// A ggml model file already on disk. Nothing is downloaded, and
    /// [`Engine::download_model`] is refused with
    /// [`DictationError::LocalModel`].
    Local(PathBuf),
    /// Downloaded into `settings.json`'s `model_folder` (the data folder's
    /// `models` when unset) when it is missing, and verified against the
    /// source's size and SHA-256; a mirror only needs its own URL.
    Download(Source),
}

impl Default for ModelSource {
    /// Whisper large-v3-turbo, downloaded from Hugging Face.
    fn default() -> Self {
        Self::Download(Source::default())
    }
}

/// The local Ollama server that summarizes library entries.
pub const DEFAULT_OLLAMA_URL: &str = "http://127.0.0.1:11434";

/// The Ollama model that summarizes library entries.
pub const DEFAULT_SUMMARY_MODEL: &str = "qwen3.8:27b-mlx";

/// How to open an [`Engine`].
pub struct EngineConfig {
    /// The data folder: the session's audio, its transcript, the default
    /// glossary (`vocabulary.txt`) and, for [`ModelSource::Download`], the
    /// model unless `settings.json`'s `model_folder` names another absolute
    /// folder.
    pub data_dir: PathBuf,
    /// Where the speech model comes from.
    pub model: ModelSource,
    /// Where a finished transcript is copied; `None` copies it nowhere
    /// (the snapshot's notice still says "Copied to the clipboard.").
    pub clipboard: Option<Arc<dyn Clipboard + Send + Sync>>,
    /// Whether finished sessions are kept in the recording library
    /// (`library/` in the data folder, see [`Engine::library`]).
    pub library: bool,
    /// The Ollama server that summarizes library entries and cleans up
    /// transcripts.
    pub ollama_url: String,
    /// The Ollama model that summarizes library entries and cleans up
    /// transcripts: the default model for both summaries and cleanup when
    /// settings.json names none.
    pub summary_model: String,
    /// The program that opens the glossary in a text editor
    /// ([`Engine::open_vocabulary`]); macOS's `open` ([`OPEN`]).
    pub editor: PathBuf,
    /// How often the cleanup model's keep-alive is renewed while the cleanup
    /// setting is on ([`KEEP_WARM_INTERVAL`]).
    pub keep_warm_every: Duration,
    /// Tightbeam's way to feed remote audio: a [`StreamRecorder`] the app
    /// pushes samples into. `None` is the cpal device recorder.
    pub recorder: Option<StreamRecorder>,
}

impl EngineConfig {
    /// The default model, downloaded into `data_dir`, no clipboard, no
    /// library ([`DEFAULT_OLLAMA_URL`] and [`DEFAULT_SUMMARY_MODEL`] for
    /// when it is turned on), [`OPEN`] as the text editor, and
    /// [`KEEP_WARM_INTERVAL`] between keep-alive renewals.
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
            model: ModelSource::default(),
            clipboard: None,
            library: false,
            ollama_url: DEFAULT_OLLAMA_URL.to_string(),
            summary_model: DEFAULT_SUMMARY_MODEL.to_string(),
            editor: PathBuf::from(OPEN),
            keep_warm_every: KEEP_WARM_INTERVAL,
            recorder: None,
        }
    }
}

/// What the engine reports as it works. More variants may come (a
/// streaming mode, say).
#[derive(Debug, Clone, PartialEq)]
#[expect(
    clippy::large_enum_variant,
    reason = "Snapshot carries the transcript so far; boxing it would change the public Event API"
)]
pub enum Event {
    /// The input level while recording, between 0 and 1, on the audio
    /// thread.
    Level(f64),
    /// The session changed: on the caller's thread with the session lock held
    /// when a method answers a snapshot, or on a job (or scriber-split) thread
    /// with the lock held. The callback must never call an engine method.
    State(Snapshot),
}

/// Why an engine call did not do what was asked.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EngineError {
    /// The session refused the action; it is unchanged.
    #[error("{0}")]
    Refused(#[from] DictationError),
    /// The session failed, for this reason; retry or discard it.
    #[error("{0}")]
    Failed(String),
    /// The transcript was not ready before the timeout.
    #[error("The transcript was not ready in time.")]
    Timeout,
    /// There is no stopped session to wait for.
    #[error("There is no stopped session to wait for.")]
    NotStopped,
    /// The engine was shut down ([`Engine::shutdown`]); nothing more can be done with it.
    #[error("The engine has shut down.")]
    ShutDown,
    /// The id names no entry in the recording library (or the library is
    /// off).
    #[error("That recording is no longer in the library.")]
    NotInLibrary,
    /// The settings could not be saved, for this reason.
    #[error("Could not save the settings: {0}.")]
    Settings(String),
    /// The glossary could not be opened in the text editor, for this reason.
    #[error("Could not open the vocabulary file: {0}.")]
    Editor(String),
    /// The installed Ollama models could not be listed, for this reason.
    #[error("Local AI needs Ollama: {0}.")]
    Ollama(String),
    /// The input devices could not be listed, for this reason.
    #[error("Could not list the microphones: {0}.")]
    Microphones(String),
}

/// What one [`Engine::shutdown`] call did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShutdownReport {
    /// True only on the call that shut the engine down; later calls (on any
    /// clone) answer false.
    pub first: bool,
    /// True only when this call paused a recording, saving its audio.
    pub paused_recording: bool,
    /// True only when this call freed a loaded speech model.
    pub freed_model: bool,
}

/// Where the recorder reports the input level.
pub type LevelSink = Arc<dyn Fn(f64) + Send + Sync>;

/// The engine's adapters for the microphone, the transcriber, the network
/// and the recording library ([`Engine::with_adapters`] takes others, e.g.
/// fakes).
pub struct Adapters {
    /// The microphone.
    pub recorder: Box<dyn Recorder + Send + Sync>,
    /// Speech to text.
    pub transcriber: Box<dyn Transcriber + Send + Sync>,
    /// The network, for the model's download.
    pub downloader: Box<dyn Downloader + Send + Sync>,
    /// The local time, naming each library entry.
    pub clock: Box<dyn Clock + Send + Sync>,
    /// Compresses each library entry's audio.
    pub encoder: Box<dyn Encoder + Send + Sync>,
    /// Summarizes each library entry.
    pub summarizer: Box<dyn Summarizer + Send + Sync>,
    /// Cleans up each transcript when the setting is on.
    pub cleaner: Box<dyn Cleaner + Send + Sync>,
    /// Keeps the cleanup model loaded while the cleanup setting is on.
    pub warmer: Arc<dyn Warmer + Send + Sync>,
}

/// A level sink that reports each level to `events` as [`Event::Level`].
pub fn level_sink(events: Arc<dyn Fn(Event) + Send + Sync>) -> LevelSink {
    Arc::new(move |l| events(Event::Level(l)))
}

/// A clipboard that copies nowhere, for an engine opened without one.
struct NoClipboard;

impl Clipboard for NoClipboard {
    fn write(&self, _text: &str) -> Result<(), PortError> {
        Ok(())
    }
}

/// Runs each job on a thread of its own.
struct Jobs(Weak<Inner>);

impl JobQueue for Jobs {
    fn send(&self, job: Job) -> Result<(), PortError> {
        let inner = self.0.upgrade().ok_or(fail("the engine has shut down"))?;
        // Only ever called, and `shut` only ever set, with the session lock
        // held: no job starts once shutdown has begun.
        if inner.shut.load(Ordering::SeqCst) {
            return Err(fail("the engine has shut down"));
        }
        let thread = std::thread::Builder::new().name("scriber-job".to_string());
        let _job = thread
            .spawn(move || {
                let report = |snapshot| {
                    (inner.events)(Event::State(snapshot));
                    inner.condvar.notify_all();
                };
                work(&inner.dictation, &job, &inner.ports(), &report);
            })
            .map_err(fail)?;
        Ok(())
    }
}

struct Inner {
    dictation: Mutex<Dictation>,
    /// Notified after every job's report, with `dictation` held.
    condvar: Condvar,
    adapters: Adapters,
    files: DataDir,
    clipboard: Arc<dyn Clipboard + Send + Sync>,
    events: Arc<dyn Fn(Event) + Send + Sync>,
    queue: Jobs,
    /// Set (with `dictation` held) by [`Engine::shutdown`].
    shut: AtomicBool,
    /// Whether finished sessions are kept in the library.
    library: bool,
    /// Opens the glossary in the text editor.
    editor: TextEditor,
    /// The Ollama server, for listing its models.
    ollama_url: String,
    /// The model asked when settings.json names none.
    default_model: String,
    /// Keeps the cleanup model loaded while the cleanup setting is on.
    keep_warm: KeepWarm,
}

/// The recorder's split notice for the engine behind `weak`. It runs on the
/// audio thread (for a `StreamRecorder`, the thread that pushes, before the
/// push returns) and only spawns a scriber-split thread, which takes the
/// closed segments into the session and reports the new snapshot. Locking
/// the session on the audio thread could deadlock: Pause holds the lock while
/// `stop` drops the stream, which waits for the audio callback. A failed
/// spawn loses nothing: the closed segment stays in the capture and the next
/// stop hands it over.
fn split_notifier(weak: Weak<Inner>) -> Arc<dyn Fn() + Send + Sync> {
    Arc::new(move || {
        let weak = Weak::clone(&weak);
        let thread = std::thread::Builder::new().name("scriber-split".to_string());
        let _spawned = thread.spawn(move || {
            let Some(inner) = weak.upgrade() else {
                return;
            };
            let ports = inner.ports();
            let mut dictation = lock(&inner.dictation);
            if let Some(snapshot) = dictation.split(&ports) {
                (inner.events)(Event::State(snapshot));
            }
        });
    })
}

impl Inner {
    fn ports(&self) -> Ports<'_> {
        let a = &self.adapters;
        let files: &dyn Files = &self.files;
        let ports = Ports::new(
            &*a.recorder,
            &*a.transcriber,
            files,
            &*self.clipboard,
            &self.queue,
            &*a.downloader,
        )
        .with_cleaner(&*a.cleaner);
        if self.library {
            ports.with_library(Library {
                clock: &*a.clock,
                encoder: &*a.encoder,
                summarizer: &*a.summarizer,
            })
        } else {
            ports
        }
    }
}

/// A `model-log.jsonl` line for one settings write.
#[derive(Serialize)]
struct SettingsLine<'a> {
    event: &'static str,
    requested: &'a str,
    stored: &'a str,
    answered: &'a str,
    dropped: bool,
}

/// A phase's [`Phase::Display`] name, as `menubar-log.jsonl` carries it.
fn phase_name(phase: Phase) -> &'static str {
    match phase {
        Phase::Idle => "idle",
        Phase::Ready => "ready",
        Phase::Recording => "recording",
        Phase::Paused => "paused",
        Phase::Transcribing => "transcribing",
        Phase::Finished => "finished",
        Phase::Failed => "failed",
    }
}

/// One dictation session over the real (or given) adapters. Clones share
/// the session; dropping the last one while no job runs drops the adapters.
#[derive(Clone)]
pub struct Engine(Arc<Inner>);

impl Engine {
    /// Opens the session in `config.data_dir` with the real adapters: the
    /// input device the settings choose (the default one when they choose
    /// none; or the config's [`StreamRecorder`] when it has one), whisper.cpp and HTTPS. A previous session there
    /// is recovered: a finished one with its transcript, an unfinished one
    /// failed and waiting for [`Self::retry`]. The model warms up, or for
    /// [`ModelSource::Download`] downloads first when missing, on a job
    /// thread; neither waits for the other or for this call.
    ///
    /// Threading: [`Event::State`] is delivered on the caller's thread with
    /// the session lock held when a method answers a snapshot, and on a job
    /// (or scriber-split) thread with the lock held otherwise, so `events`
    /// must not call any `Engine` method (it would deadlock); forward the
    /// event to a channel instead. [`Event::Level`] is delivered on the audio
    /// thread (for a [`StreamRecorder`], the thread that calls `push`, before
    /// `push` returns) and must return quickly; it must not block on the pusher.
    ///
    /// ```rust,no_run
    /// use std::sync::Arc;
    ///
    /// use scriber_engine::{Engine, EngineConfig, ModelSource};
    ///
    /// let mut config = EngineConfig::new("/path/to/data");
    /// config.model = ModelSource::Local("/path/to/ggml-large-v3-turbo.bin".into());
    /// let engine = Engine::open(config, Arc::new(|_event| {}));
    /// # drop(engine);
    /// ```
    pub fn open(config: EngineConfig, events: Arc<dyn Fn(Event) + Send + Sync>) -> Self {
        let transcriber = match &config.model {
            ModelSource::Local(path) => Whisper::new(path.clone()),
            ModelSource::Download(_) => Whisper::in_data_dir(config.data_dir.clone()),
        };
        let data_dir = config.data_dir.clone();
        let ollama_url = config.ollama_url.clone();
        let summary_model = config.summary_model.clone();
        let stream = config.recorder.clone();
        Self::with_adapters(
            config,
            move |level| Adapters {
                recorder: match stream {
                    Some(s) => {
                        s.with_level(level);
                        Box::new(s)
                    }
                    None => Box::new(CpalRecorder::new(level)),
                },
                transcriber: Box::new(transcriber),
                downloader: Box::new(HttpDownloader::default()),
                clock: Box::new(LocalClock),
                encoder: Box::new(Afconvert::new(PathBuf::from(AFCONVERT), data_dir)),
                summarizer: Box::new(Ollama::new(&ollama_url, &summary_model)),
                cleaner: Box::new(Ollama::with_timeout(
                    &ollama_url,
                    &summary_model,
                    CLEANUP_TIMEOUT,
                )),
                warmer: Arc::new(Ollama::with_timeout(
                    &ollama_url,
                    &summary_model,
                    WARM_TIMEOUT,
                )),
            },
            events,
        )
    }

    /// [`Self::open`] over the adapters `make` builds from the level sink it
    /// is given (which reports to `events`). The threading contract is
    /// [`Self::open`]'s: never call the engine from `events`.
    /// `config.recorder` is ignored here; supply the recorder in `make`.
    ///
    /// ```rust,no_run
    /// use std::sync::Arc;
    ///
    /// use scriber_engine::{
    ///     AFCONVERT, Adapters, Afconvert, CpalRecorder, Engine, EngineConfig, HttpDownloader,
    ///     LocalClock, Ollama, Whisper,
    /// };
    ///
    /// let engine = Engine::with_adapters(
    ///     EngineConfig::new("/path/to/data"),
    ///     |level| Adapters {
    ///         recorder: Box::new(CpalRecorder::new(level)),
    ///         transcriber: Box::new(Whisper::new("/path/to/model.bin".into())),
    ///         downloader: Box::new(HttpDownloader::default()),
    ///         clock: Box::new(LocalClock),
    ///         encoder: Box::new(Afconvert::new(AFCONVERT.into(), "/path/to/data".into())),
    ///         summarizer: Box::new(Ollama::new("http://127.0.0.1:11434", "qwen3.8:27b-mlx")),
    ///         cleaner: Box::new(Ollama::new("http://127.0.0.1:11434", "qwen3.8:27b-mlx")),
    ///         warmer: Arc::new(Ollama::new("http://127.0.0.1:11434", "qwen3.8:27b-mlx")),
    ///     },
    ///     Arc::new(|_event| {}),
    /// );
    /// # drop(engine);
    /// ```
    pub fn with_adapters(
        config: EngineConfig,
        make: impl FnOnce(LevelSink) -> Adapters,
        events: Arc<dyn Fn(Event) + Send + Sync>,
    ) -> Self {
        let adapters = make(level_sink(Arc::clone(&events)));
        let EngineConfig {
            data_dir,
            model,
            clipboard,
            library,
            ollama_url,
            summary_model,
            editor,
            keep_warm_every,
            recorder: _,
        } = config;
        let keep_warm = KeepWarm::start(Arc::clone(&adapters.warmer), keep_warm_every);
        let clipboard = clipboard.unwrap_or_else(|| Arc::new(NoClipboard));
        let inner = Arc::new_cyclic(|weak| Inner {
            dictation: Mutex::default(),
            condvar: Condvar::new(),
            adapters,
            files: DataDir::new(data_dir),
            clipboard,
            events,
            queue: Jobs(Weak::clone(weak)),
            shut: AtomicBool::new(false),
            library,
            editor: TextEditor::new(editor),
            ollama_url,
            default_model: summary_model,
            keep_warm,
        });
        inner
            .adapters
            .recorder
            .on_split(split_notifier(Arc::downgrade(&inner)));
        let ports = inner.ports();
        // Launched with the lock held, so a job it queues cannot record its
        // outcome in the placeholder session.
        let mut guard = lock(&inner.dictation);
        *guard = match model {
            ModelSource::Local(_) => Dictation::launch_local(&ports),
            ModelSource::Download(source) => Dictation::launch_from(source, &ports),
        };
        drop(guard);
        let engine = Self(inner);
        engine.0.keep_warm.refresh(|| engine.settings());
        engine
    }

    /// The session as it stands.
    #[must_use]
    pub fn state(&self) -> Snapshot {
        lock(&self.0.dictation).snapshot(&self.0.ports())
    }

    /// Runs one action (`text` is the edited transcript for
    /// [`Action::Copy`]) and answers with the new snapshot.
    ///
    /// # Errors
    ///
    /// [`EngineError::Refused`]: the action is not allowed now;
    /// [`EngineError::ShutDown`]: the engine has shut down.
    pub fn act(&self, action: Action, text: &str) -> Result<Snapshot, EngineError> {
        let ports = self.0.ports();
        let mut dictation = lock(&self.0.dictation);
        self.refuse_if_shut()?;
        let snapshot = dictation.apply(action, text, &ports)?;
        (self.0.events)(Event::State(snapshot.clone()));
        Ok(snapshot)
    }

    /// [`EngineError::ShutDown`] once [`Self::shutdown`] has run; call with
    /// the session lock held.
    fn refuse_if_shut(&self) -> Result<(), EngineError> {
        if self.0.shut.load(Ordering::SeqCst) {
            Err(EngineError::ShutDown)
        } else {
            Ok(())
        }
    }

    /// Starts recording: from idle or finished, a new session first (which
    /// deletes the last one); from ready, at once. A failed session is never
    /// wiped: [`Self::retry`] or [`Self::discard`] it first.
    ///
    /// # Errors
    ///
    /// [`EngineError::Refused`]: recording, paused, transcribing or failed;
    /// the model is missing; the microphone fails (the session is then
    /// ready). [`EngineError::ShutDown`]: the engine has shut down.
    pub fn record(&self) -> Result<Snapshot, EngineError> {
        let ports = self.0.ports();
        let mut dictation = lock(&self.0.dictation);
        self.refuse_if_shut()?;
        self.record_locked(&mut dictation, &ports)
    }

    /// [`Self::record`]'s body with the session lock already held: from
    /// `idle` or `finished`, a new session first (which deletes the last
    /// one), then Record. Shared with [`Self::toggle`].
    fn record_locked(
        &self,
        dictation: &mut Dictation,
        ports: &Ports<'_>,
    ) -> Result<Snapshot, EngineError> {
        match dictation.snapshot(ports).phase {
            Phase::Idle | Phase::Finished => {
                let _ready = dictation.apply(Action::New, "", ports)?;
            }
            Phase::Ready
            | Phase::Recording
            | Phase::Paused
            | Phase::Transcribing
            | Phase::Failed => {}
        }
        let snapshot = dictation.apply(Action::Record, "", ports)?;
        (self.0.events)(Event::State(snapshot.clone()));
        Ok(snapshot)
    }

    /// The menu bar's toggle: one press of the global shortcut, decided by
    /// [`menubar::toggle`] from the phase the session lock shows.
    ///
    /// A `record` press calls [`Self::record_locked`]; an `act` press sends
    /// its action (`done` or `resume`); `nothing` answers the snapshot
    /// unchanged. Every press writes one `menubar-log.jsonl` line, and a
    /// successful one is delivered as [`Event::State`].
    ///
    /// # Errors
    ///
    /// Whatever the session answers ([`EngineError::Refused`] included);
    /// [`EngineError::ShutDown`] once [`Self::shutdown`] has run, in every
    /// phase, because the check comes before the phase is read.
    pub fn toggle(&self) -> Result<Snapshot, EngineError> {
        let ports = self.0.ports();
        let mut dictation = lock(&self.0.dictation);
        let shut = self.refuse_if_shut();
        let phase = dictation.snapshot(&ports).phase;
        let toggle = menubar::toggle(phase);
        // A press after shutdown still logs its failure: the check comes
        // before the phase is read, in every phase, `nothing` included.
        let result = match shut {
            Err(e) => Err(e),
            Ok(()) => match toggle {
                menubar::Toggle::Record => self.record_locked(&mut dictation, &ports),
                menubar::Toggle::Act(action) => dictation
                    .apply(action, "", &ports)
                    .map_err(EngineError::from),
                menubar::Toggle::Nothing => Ok(dictation.snapshot(&ports)),
            },
        };
        let error: Option<String> = match &result {
            Ok(_) => None,
            Err(e) => Some(e.to_string()),
        };
        let (outcome, success) = match &result {
            Ok(snapshot) => (
                if toggle == menubar::Toggle::Nothing {
                    "nothing"
                } else {
                    "ok"
                },
                Some(snapshot.phase),
            ),
            Err(EngineError::Refused(_)) => ("refused", None),
            Err(_) => ("failed", None),
        };
        let action = match toggle {
            menubar::Toggle::Act(Action::Done) => Some("done"),
            menubar::Toggle::Act(Action::Resume) => Some("resume"),
            menubar::Toggle::Act(_) | menubar::Toggle::Record | menubar::Toggle::Nothing => None,
        };
        self.log_menubar(&menubar::Log::Toggle {
            phase: phase_name(phase),
            toggle: match toggle {
                menubar::Toggle::Record => "record",
                menubar::Toggle::Act(_) => "act",
                menubar::Toggle::Nothing => "nothing",
            },
            action,
            outcome,
            error: error.as_deref(),
            phase_after: phase_name(success.unwrap_or(phase)),
        });
        if let Ok(snapshot) = &result {
            (self.0.events)(Event::State(snapshot.clone()));
        }
        result
    }

    /// The menu bar's pause key: one press of the pause shortcut, decided by
    /// [`menubar::pause`] from the phase the session lock shows.
    ///
    /// While recording it sends [`Action::Pause`], while paused it resumes,
    /// and in every other phase it does nothing. Unlike [`Self::pause`],
    /// which sends [`Action::Pause`] unconditionally and is refused outside
    /// recording, `press_pause` is the phase-decided menu-bar gesture. Every
    /// press writes one `menubar-log.jsonl` line, and a successful one is
    /// delivered as [`Event::State`].
    ///
    /// # Errors
    ///
    /// Whatever the session answers ([`EngineError::Refused`] included);
    /// [`EngineError::ShutDown`] once [`Self::shutdown`] has run, in every
    /// phase, because the check comes before the phase is read.
    pub fn press_pause(&self) -> Result<Snapshot, EngineError> {
        let ports = self.0.ports();
        let mut dictation = lock(&self.0.dictation);
        let shut = self.refuse_if_shut();
        let phase = dictation.snapshot(&ports).phase;
        let pause = menubar::pause(phase);
        // A press after shutdown still logs its failure: the check comes
        // before the phase is read, in every phase, `nothing` included.
        let result = match shut {
            Err(e) => Err(e),
            Ok(()) => match pause {
                menubar::Toggle::Act(action) => dictation
                    .apply(action, "", &ports)
                    .map_err(EngineError::from),
                menubar::Toggle::Record | menubar::Toggle::Nothing => {
                    Ok(dictation.snapshot(&ports))
                }
            },
        };
        let error: Option<String> = match &result {
            Ok(_) => None,
            Err(e) => Some(e.to_string()),
        };
        let (outcome, success) = match &result {
            Ok(snapshot) => (
                if pause == menubar::Toggle::Nothing {
                    "nothing"
                } else {
                    "ok"
                },
                Some(snapshot.phase),
            ),
            Err(EngineError::Refused(_)) => ("refused", None),
            Err(_) => ("failed", None),
        };
        let action = match pause {
            menubar::Toggle::Act(Action::Pause) => Some("pause"),
            menubar::Toggle::Act(Action::Resume) => Some("resume"),
            menubar::Toggle::Act(_) | menubar::Toggle::Record | menubar::Toggle::Nothing => None,
        };
        self.log_menubar(&menubar::Log::Pause {
            phase: phase_name(phase),
            action,
            outcome,
            error: error.as_deref(),
            phase_after: phase_name(success.unwrap_or(phase)),
        });
        if let Ok(snapshot) = &result {
            (self.0.events)(Event::State(snapshot.clone()));
        }
        result
    }

    /// The accelerator the global-shortcut plugin takes, from the saved
    /// `toggle_shortcut`: `Alt+Super+KeyD` for the default. Never `None`,
    /// because [`settings::load`] keeps only a value `menubar::accelerator`
    /// accepts. Takes the session lock (with `settings`), so it is never
    /// called from the events callback.
    #[must_use]
    pub fn toggle_accelerator(&self) -> String {
        menubar::accelerator_or_default(&self.settings().toggle_shortcut)
    }

    /// The accelerator the global-shortcut plugin takes for the pause key,
    /// from the saved `pause_shortcut`: `Some("Alt+Super+KeyP")` for the
    /// default, and `None` for `""` (no second shortcut registered). Takes no
    /// session lock, so it is safe in the settings command.
    #[must_use]
    pub fn pause_accelerator(&self) -> Option<String> {
        menubar::accelerator(&self.settings().pause_shortcut)
    }

    /// Writes one `menubar-log.jsonl` line, best effort, in the shape of the
    /// `model-log.jsonl` write. Takes no lock.
    pub fn log_menubar(&self, event: &menubar::Log<'_>) {
        if let Ok(text) = serde_json::to_string(event) {
            scriber_core::store::log_menubar(&self.0.files, &text);
        }
    }

    /// Pauses: the segment so far starts transcribing.
    ///
    /// # Errors
    ///
    /// [`EngineError::Refused`] unless recording.
    pub fn pause(&self) -> Result<Snapshot, EngineError> {
        self.act(Action::Pause, "")
    }

    /// Resumes recording another segment.
    ///
    /// # Errors
    ///
    /// [`EngineError::Refused`] unless paused, or the microphone fails.
    pub fn resume(&self) -> Result<Snapshot, EngineError> {
        self.act(Action::Resume, "")
    }

    /// Stops: the session transcribes; [`Self::wait`] for its text.
    ///
    /// # Errors
    ///
    /// [`EngineError::Refused`] unless recording or paused.
    pub fn stop(&self) -> Result<Snapshot, EngineError> {
        self.act(Action::Done, "")
    }

    /// Throws the session away and starts an empty one.
    ///
    /// # Errors
    ///
    /// [`EngineError::Refused`] while idle or transcribing.
    pub fn discard(&self) -> Result<Snapshot, EngineError> {
        self.act(Action::Discard, "")
    }

    /// Transcribes a failed session's failed segments again.
    ///
    /// # Errors
    ///
    /// [`EngineError::Refused`] unless failed.
    pub fn retry(&self) -> Result<Snapshot, EngineError> {
        self.act(Action::Retry, "")
    }

    /// Downloads the model again after a failed download.
    ///
    /// # Errors
    ///
    /// [`EngineError::Refused`]: the model is a local file, installed, or
    /// already downloading.
    pub fn download_model(&self) -> Result<Snapshot, EngineError> {
        self.act(Action::DownloadModel, "")
    }

    /// Waits up to `timeout` for a stopped session's transcript. With the
    /// cleanup setting on, the wait includes the cleanup call, which may take
    /// up to [`CLEANUP_TIMEOUT`].
    ///
    /// # Errors
    ///
    /// [`EngineError::Failed`] with the session's error;
    /// [`EngineError::Timeout`]; [`EngineError::NotStopped`] while idle,
    /// ready, recording or paused; [`EngineError::ShutDown`] when called
    /// after [`Self::shutdown`] (a waiter already blocked when it runs keeps
    /// waiting for the transcription in flight or its timeout).
    pub fn wait(&self, timeout: Duration) -> Result<String, EngineError> {
        let deadline = Instant::now().checked_add(timeout);
        let ports = self.0.ports();
        let mut dictation = lock(&self.0.dictation);
        self.refuse_if_shut()?;
        loop {
            let snapshot = dictation.snapshot(&ports);
            match snapshot.phase {
                Phase::Finished => return Ok(snapshot.text),
                Phase::Failed => {
                    return Err(EngineError::Failed(snapshot.error.unwrap_or_default()));
                }
                Phase::Idle | Phase::Ready | Phase::Recording | Phase::Paused => {
                    return Err(EngineError::NotStopped);
                }
                Phase::Transcribing => {}
            }
            let now = Instant::now();
            let left = deadline.map_or(Duration::MAX, |d| d.saturating_duration_since(now));
            if left.is_zero() {
                return Err(EngineError::Timeout);
            }
            let waited = self.0.condvar.wait_timeout(dictation, left);
            dictation = waited.unwrap_or_else(PoisonError::into_inner).0;
        }
    }

    /// [`Self::stop`], then [`Self::wait`]: the primary path is
    /// [`Self::record`] then this. With the cleanup setting on, the wait
    /// includes the cleanup call, up to [`CLEANUP_TIMEOUT`].
    ///
    /// # Errors
    ///
    /// Either call's.
    pub fn stop_and_wait(&self, timeout: Duration) -> Result<String, EngineError> {
        let _stopped = self.stop()?;
        self.wait(timeout)
    }

    /// Shuts the engine down, on this handle and every clone: refuses new
    /// work, pauses a recording (stopping the microphone and saving the open
    /// segment's audio, which the next launch offers to retry), then frees
    /// the speech model. Idempotent: any later call, on any clone or thread,
    /// returns only once the model is freed.
    ///
    /// On macOS a consumer MUST call this before the process exits.
    /// whisper.cpp's Metal device is a C++ static freed at exit, and it
    /// aborts the process (`GGML_ASSERT([rsets->data count] == 0)`) while a
    /// whisper context is still alive. Dropping every `Engine` handle is NOT
    /// enough, because a job thread may still hold the engine.
    ///
    /// It blocks for at most the one model load or transcription already
    /// running, which finishes first; calls still waiting for the model are
    /// refused. Never call it from the events callback: it deadlocks, like
    /// every other method. Afterwards actions are refused with
    /// [`EngineError::ShutDown`] while [`Self::state`] still answers, and a
    /// transcription that finished during shutdown may still record its
    /// outcome on its job thread. It releases the cleanup model kept warm in Ollama (best effort, on the calling thread; the Ollama adapter waits at most [`RELEASE_TIMEOUT`], 2 s).
    #[must_use]
    pub fn shutdown(&self) -> ShutdownReport {
        let ports = self.0.ports();
        let mut dictation = lock(&self.0.dictation);
        let first = !self.0.shut.swap(true, Ordering::SeqCst);
        let paused_recording = first && dictation.apply(Action::Pause, "", &ports).is_ok();
        drop(dictation);
        let freed_model = self.0.adapters.transcriber.unload();
        if first {
            self.0.keep_warm.shutdown();
        }
        ShutdownReport {
            first,
            paused_recording,
            freed_model,
        }
    }

    /// The recording library, newest first; empty when the library is off.
    /// It does not wait for the session.
    #[must_use]
    pub fn library(&self) -> Vec<LibraryEntry> {
        if self.0.library {
            library::list(&self.0.files)
        } else {
            Vec::new()
        }
    }

    /// The transcript of library entry `id`.
    ///
    /// # Errors
    ///
    /// [`EngineError::NotInLibrary`]: the id names no entry, its transcript
    /// is unreadable, or the library is off.
    pub fn library_transcript(&self, id: &str) -> Result<String, EngineError> {
        if !self.0.library {
            return Err(EngineError::NotInLibrary);
        }
        library::transcript(&self.0.files, id).ok_or(EngineError::NotInLibrary)
    }

    /// Puts library entry `id`'s transcript on the clipboard.
    ///
    /// # Errors
    ///
    /// [`Self::library_transcript`]'s; [`EngineError::Refused`] with
    /// [`DictationError::Clipboard`] when the clipboard cannot be written.
    pub fn library_copy(&self, id: &str) -> Result<(), EngineError> {
        let text = self.library_transcript(id)?;
        self.0
            .clipboard
            .write(&text)
            .map_err(|e| EngineError::Refused(DictationError::Clipboard(e)))
    }

    /// The default glossary's absolute path: `vocabulary.txt` in the data
    /// folder.
    fn default_vocabulary(&self) -> String {
        self.0
            .files
            .root()
            .join(VOCABULARY)
            .to_string_lossy()
            .into_owned()
    }

    /// The default learned list's absolute path: `learned.txt` in the data
    /// folder.
    fn default_learned_vocabulary(&self) -> String {
        self.0
            .files
            .root()
            .join(LEARNED)
            .to_string_lossy()
            .into_owned()
    }

    /// The default model folder's absolute path: `models` in the data folder.
    fn default_model_folder(&self) -> String {
        self.0
            .files
            .root()
            .join(MODEL_FOLDER)
            .to_string_lossy()
            .into_owned()
    }

    /// The user's settings (the defaults where none are saved), with the
    /// glossary, the learned list and the model folder always absolute paths
    /// and both models always named ([`EngineConfig::summary_model`] where
    /// none is saved).
    #[must_use]
    pub fn settings(&self) -> Settings {
        let mut s = settings::load(&self.0.files);
        if s.vocabulary.is_empty() {
            s.vocabulary = self.default_vocabulary();
        }
        if s.learned_vocabulary.is_empty() {
            s.learned_vocabulary = self.default_learned_vocabulary();
        }
        if s.model_folder.is_empty() {
            s.model_folder = self.default_model_folder();
        }
        for model in [&mut s.summary_model, &mut s.cleanup_model] {
            if model.is_empty() {
                model.clone_from(&self.0.default_model);
            }
        }
        s
    }

    /// Saves the user's settings and answers with them as [`Self::settings`]
    /// reads them back. Every field is replaced, except that an empty
    /// `vocabulary` keeps the saved one, and the default glossary's absolute
    /// path is saved as `""` (the default, wherever the data folder is).
    /// Likewise for `learned_vocabulary`: empty keeps the saved one, and its
    /// default absolute path is saved as `""`. Likewise for `model_folder`:
    /// empty keeps the saved one, and its default absolute path is saved as
    /// `""`; a relative path is stored verbatim and dropped by
    /// [`settings::load`] on read. Each write is logged to
    /// `model-log.jsonl`. Likewise for each of
    /// `summary_model` and `cleanup_model`: empty keeps
    /// the saved one, and [`EngineConfig::summary_model`] is saved as `""`.
    /// `input_device` is saved verbatim: `""` means the Mac's default input
    /// device.
    ///
    /// # Errors
    ///
    /// [`EngineError::Settings`]: the file cannot be written;
    /// [`EngineError::ShutDown`]: the engine has shut down.
    pub fn set_settings(&self, mut s: Settings) -> Result<Settings, EngineError> {
        if self.0.shut.load(Ordering::SeqCst) {
            return Err(EngineError::ShutDown);
        }
        let requested = s.model_folder.clone();
        if s.vocabulary.is_empty() {
            s.vocabulary = settings::load(&self.0.files).vocabulary;
        } else if s.vocabulary == self.default_vocabulary() {
            s.vocabulary = String::new();
        }
        if s.learned_vocabulary.is_empty() {
            s.learned_vocabulary = settings::load(&self.0.files).learned_vocabulary;
        } else if s.learned_vocabulary == self.default_learned_vocabulary() {
            s.learned_vocabulary = String::new();
        }
        if s.model_folder.is_empty() {
            s.model_folder = settings::load(&self.0.files).model_folder;
        } else if s.model_folder == self.default_model_folder() {
            s.model_folder = String::new();
        }
        let saved = settings::load(&self.0.files);
        s.keep_segments = saved.keep_segments;
        for (model, kept) in [
            (&mut s.summary_model, saved.summary_model),
            (&mut s.cleanup_model, saved.cleanup_model),
        ] {
            if model.is_empty() {
                *model = kept;
            } else if *model == self.0.default_model {
                model.clear();
            }
        }
        settings::save(&self.0.files, &s).map_err(|e| EngineError::Settings(e.to_string()))?;
        let answered = self.settings();
        let line = SettingsLine {
            event: "settings",
            requested: &requested,
            stored: &s.model_folder,
            answered: &answered.model_folder,
            dropped: !s.model_folder.is_empty() && answered.model_folder != s.model_folder,
        };
        if let Ok(text) = serde_json::to_string(&line) {
            scriber_core::store::log_model(&self.0.files, &text);
        }
        self.0.keep_warm.refresh(|| self.settings());
        Ok(answered)
    }

    /// The names of the text-generating models installed on the Ollama server
    /// ([`EngineConfig::ollama_url`]), embedding models hidden, sorted ([`installed_models`]). It
    /// takes no session lock and still works after [`Self::shutdown`].
    ///
    /// # Errors
    ///
    /// [`EngineError::Ollama`]: the server cannot be reached or answers
    /// with no model list.
    pub fn ollama_models(&self) -> Result<Vec<String>, EngineError> {
        installed_models(&self.0.ollama_url).map_err(|e| EngineError::Ollama(e.0))
    }

    /// The input devices there are now: the system default's name and every
    /// other one's, for choosing [`Settings::input_device`]. It takes no
    /// session lock and still works after [`Self::shutdown`].
    ///
    /// # Errors
    ///
    /// [`EngineError::Microphones`]: the devices cannot be listed.
    pub fn input_devices(&self) -> Result<InputDevices, EngineError> {
        self.0
            .adapters
            .recorder
            .inputs()
            .map_err(|e| EngineError::Microphones(e.0))
    }

    /// Opens the glossary ([`Self::settings`]' `vocabulary`) in the text
    /// editor, creating it empty first when it is missing. It still works
    /// after [`Self::shutdown`].
    ///
    /// # Errors
    ///
    /// [`EngineError::Editor`]: the file cannot be created, or the editor
    /// cannot be started or fails.
    pub fn open_vocabulary(&self) -> Result<(), EngineError> {
        let path = PathBuf::from(self.settings().vocabulary);
        self.0
            .editor
            .open(&path)
            .map_err(|e| EngineError::Editor(e.to_string()))
    }

    /// Opens the learned list ([`Self::settings`]' `learned_vocabulary`) in
    /// the text editor, creating it empty first when it is missing. It still
    /// works after [`Self::shutdown`].
    ///
    /// # Errors
    ///
    /// [`EngineError::Editor`]: the file cannot be created, or the editor
    /// cannot be started or fails.
    pub fn open_learned_vocabulary(&self) -> Result<(), EngineError> {
        let path = PathBuf::from(self.settings().learned_vocabulary);
        self.0
            .editor
            .open(&path)
            .map_err(|e| EngineError::Editor(e.to_string()))
    }
}
