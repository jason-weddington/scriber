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
//! let text = engine.stop_and_wait(Duration::from_secs(120))?;
//! // On macOS, always before the process exits (see `Engine::shutdown`).
//! let _report = engine.shutdown();
//! # drop((rx, text));
//! # Ok::<(), scriber_engine::EngineError>(())
//! ```

mod clock;
mod device;
mod encode;
mod files;
mod net;
mod ollama;
mod whisper;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError, Weak};
use std::time::{Duration, Instant};

use scriber_core::dictation::{Dictation, Job, work};
use scriber_core::library;
use scriber_core::ports::{JobQueue, Library, Ports};
use scriber_core::settings;
use scriber_core::store::MODEL;
use scriber_core::sync::lock;

pub use clock::LocalClock;
pub use device::CpalRecorder;
pub use encode::{AFCONVERT, Afconvert};
pub use files::DataDir;
pub use net::HttpDownloader;
pub use ollama::{Ollama, SUMMARY_TIMEOUT, SYSTEM_PROMPT};
pub use scriber_core::dictation::DictationError;
pub use scriber_core::download::Source;
pub use scriber_core::model::{Action, Download, LibraryEntry, Phase, Settings, Snapshot};
pub use scriber_core::ports::{
    Captured, Clipboard, Clock, Downloader, Encoder, Files, LocalTime, PortError, Recorder,
    Summarizer, Transcriber, fail,
};
pub use whisper::Whisper;

/// Where the speech model comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelSource {
    /// A ggml model file already on disk. Nothing is downloaded, and
    /// [`Engine::download_model`] is refused with
    /// [`DictationError::LocalModel`].
    Local(PathBuf),
    /// Downloaded into the data folder (`models/ggml-large-v3-turbo.bin`)
    /// when it is missing, and verified against the source's size and
    /// SHA-256; a mirror only needs its own URL.
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
    /// The data folder: the session's audio, its transcript, the glossary
    /// (`vocabulary.txt`) and, for [`ModelSource::Download`], the model.
    pub data_dir: PathBuf,
    /// Where the speech model comes from.
    pub model: ModelSource,
    /// Where a finished transcript is copied; `None` copies it nowhere
    /// (the snapshot's notice still says "Copied to the clipboard.").
    pub clipboard: Option<Arc<dyn Clipboard + Send + Sync>>,
    /// Whether finished sessions are kept in the recording library
    /// (`library/` in the data folder, see [`Engine::library`]).
    pub library: bool,
    /// The Ollama server that summarizes library entries.
    pub ollama_url: String,
    /// The Ollama model that summarizes library entries.
    pub summary_model: String,
}

impl EngineConfig {
    /// The default model, downloaded into `data_dir`, no clipboard, and no
    /// library ([`DEFAULT_OLLAMA_URL`] and [`DEFAULT_SUMMARY_MODEL`] for
    /// when it is turned on).
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
            model: ModelSource::default(),
            clipboard: None,
            library: false,
            ollama_url: DEFAULT_OLLAMA_URL.to_string(),
            summary_model: DEFAULT_SUMMARY_MODEL.to_string(),
        }
    }
}

/// What the engine reports as it works. More variants may come (a
/// streaming mode, say).
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// The input level while recording, between 0 and 1, on the audio
    /// thread.
    Level(f64),
    /// The session changed off the caller's thread: a download's progress,
    /// a segment transcribed, the session finished or failed. Delivered on
    /// a job thread while the session lock is held.
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
        );
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

/// One dictation session over the real (or given) adapters. Clones share
/// the session; dropping the last one while no job runs drops the adapters.
#[derive(Clone)]
pub struct Engine(Arc<Inner>);

impl Engine {
    /// Opens the session in `config.data_dir` with the real adapters: the
    /// default input device, whisper.cpp and HTTPS. A previous session there
    /// is recovered: a finished one with its transcript, an unfinished one
    /// failed and waiting for [`Self::retry`]. The model warms up, or for
    /// [`ModelSource::Download`] downloads first when missing, on a job
    /// thread; neither waits for the other or for this call.
    ///
    /// Threading: [`Event::State`] is delivered on a job thread WHILE THE
    /// SESSION LOCK IS HELD, so `events` must not call any `Engine` method
    /// (it would deadlock); forward the event to a channel instead.
    /// [`Event::Level`] is delivered on the audio thread and must return
    /// quickly.
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
        let path = match &config.model {
            ModelSource::Local(path) => path.clone(),
            ModelSource::Download(_) => config.data_dir.join(MODEL),
        };
        let data_dir = config.data_dir.clone();
        let ollama_url = config.ollama_url.clone();
        let summary_model = config.summary_model.clone();
        Self::with_adapters(
            config,
            move |level| Adapters {
                recorder: Box::new(CpalRecorder::new(level)),
                transcriber: Box::new(Whisper::new(path)),
                downloader: Box::new(HttpDownloader::default()),
                clock: Box::new(LocalClock),
                encoder: Box::new(Afconvert::new(PathBuf::from(AFCONVERT), data_dir)),
                summarizer: Box::new(Ollama::new(&ollama_url, &summary_model)),
            },
            events,
        )
    }

    /// [`Self::open`] over the adapters `make` builds from the level sink it
    /// is given (which reports to `events`). The threading contract is
    /// [`Self::open`]'s: never call the engine from `events`.
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
            ollama_url: _,
            summary_model: _,
        } = config;
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
        });
        let ports = inner.ports();
        // Launched with the lock held, so a job it queues cannot record its
        // outcome in the placeholder session.
        let mut guard = lock(&inner.dictation);
        *guard = match model {
            ModelSource::Local(_) => Dictation::launch_local(&ports),
            ModelSource::Download(source) => Dictation::launch_from(source, &ports),
        };
        drop(guard);
        Self(inner)
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
        Ok(dictation.apply(action, text, &ports)?)
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
        match dictation.snapshot(&ports).phase {
            Phase::Idle | Phase::Finished => {
                let _ready = dictation.apply(Action::New, "", &ports)?;
            }
            Phase::Ready
            | Phase::Recording
            | Phase::Paused
            | Phase::Transcribing
            | Phase::Failed => {}
        }
        Ok(dictation.apply(Action::Record, "", &ports)?)
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

    /// Waits up to `timeout` for a stopped session's transcript.
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
    /// [`Self::record`] then this.
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
    /// outcome on its job thread.
    #[must_use]
    pub fn shutdown(&self) -> ShutdownReport {
        let ports = self.0.ports();
        let mut dictation = lock(&self.0.dictation);
        let first = !self.0.shut.swap(true, Ordering::SeqCst);
        let paused_recording = first && dictation.apply(Action::Pause, "", &ports).is_ok();
        drop(dictation);
        let freed_model = self.0.adapters.transcriber.unload();
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

    /// The user's settings (the defaults where none are saved).
    #[must_use]
    pub fn settings(&self) -> Settings {
        settings::load(&self.0.files)
    }

    /// Saves the user's settings and answers with what was saved.
    ///
    /// # Errors
    ///
    /// [`EngineError::Settings`]: the file cannot be written;
    /// [`EngineError::ShutDown`]: the engine has shut down.
    pub fn set_settings(&self, s: Settings) -> Result<Settings, EngineError> {
        if self.0.shut.load(Ordering::SeqCst) {
            return Err(EngineError::ShutDown);
        }
        settings::save(&self.0.files, &s).map_err(|e| EngineError::Settings(e.to_string()))?;
        Ok(s)
    }
}
