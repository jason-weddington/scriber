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
//! # drop((rx, text));
//! # Ok::<(), scriber_engine::EngineError>(())
//! ```

mod device;
mod files;
mod net;
mod whisper;

use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, PoisonError, Weak};
use std::time::{Duration, Instant};

use scriber_core::dictation::{Dictation, Job, work};
use scriber_core::ports::{JobQueue, Ports};
use scriber_core::store::MODEL;
use scriber_core::sync::lock;

pub use device::CpalRecorder;
pub use files::DataDir;
pub use net::HttpDownloader;
pub use scriber_core::dictation::DictationError;
pub use scriber_core::download::Source;
pub use scriber_core::model::{Action, Download, Phase, Snapshot};
pub use scriber_core::ports::{
    Captured, Clipboard, Downloader, Files, PortError, Recorder, Transcriber, fail,
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
}

impl EngineConfig {
    /// The default model, downloaded into `data_dir`, and no clipboard.
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
            model: ModelSource::default(),
            clipboard: None,
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
}

/// Where the recorder reports the input level.
pub type LevelSink = Arc<dyn Fn(f64) + Send + Sync>;

/// The engine's adapters for the microphone, the transcriber and the
/// network ([`Engine::with_adapters`] takes others, e.g. fakes).
pub struct Adapters {
    /// The microphone.
    pub recorder: Box<dyn Recorder + Send + Sync>,
    /// Speech to text.
    pub transcriber: Box<dyn Transcriber + Send + Sync>,
    /// The network, for the model's download.
    pub downloader: Box<dyn Downloader + Send + Sync>,
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
}

impl Inner {
    fn ports(&self) -> Ports<'_> {
        let a = &self.adapters;
        let files: &dyn Files = &self.files;
        Ports::new(
            &*a.recorder,
            &*a.transcriber,
            files,
            &*self.clipboard,
            &self.queue,
            &*a.downloader,
        )
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
        Self::with_adapters(
            config,
            move |level| Adapters {
                recorder: Box::new(CpalRecorder::new(level)),
                transcriber: Box::new(Whisper::new(path)),
                downloader: Box::new(HttpDownloader::default()),
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
    ///     Adapters, CpalRecorder, Engine, EngineConfig, HttpDownloader, Whisper,
    /// };
    ///
    /// let engine = Engine::with_adapters(
    ///     EngineConfig::new("/path/to/data"),
    ///     |level| Adapters {
    ///         recorder: Box::new(CpalRecorder::new(level)),
    ///         transcriber: Box::new(Whisper::new("/path/to/model.bin".into())),
    ///         downloader: Box::new(HttpDownloader::default()),
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
    /// [`EngineError::Refused`]: the action is not allowed now.
    pub fn act(&self, action: Action, text: &str) -> Result<Snapshot, EngineError> {
        let ports = self.0.ports();
        Ok(lock(&self.0.dictation).apply(action, text, &ports)?)
    }

    /// Starts recording: from idle or finished, a new session first (which
    /// deletes the last one); from ready, at once. A failed session is never
    /// wiped: [`Self::retry`] or [`Self::discard`] it first.
    ///
    /// # Errors
    ///
    /// [`EngineError::Refused`]: recording, paused, transcribing or failed;
    /// the model is missing; the microphone fails (the session is then
    /// ready).
    pub fn record(&self) -> Result<Snapshot, EngineError> {
        let ports = self.0.ports();
        let mut dictation = lock(&self.0.dictation);
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
    /// ready, recording or paused.
    pub fn wait(&self, timeout: Duration) -> Result<String, EngineError> {
        let deadline = Instant::now().checked_add(timeout);
        let ports = self.0.ports();
        let mut dictation = lock(&self.0.dictation);
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
}
