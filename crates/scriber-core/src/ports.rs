//! The core's view of the outside world. Each trait is one kind of effect;
//! the engine crate (`crates/scriber-engine`) supplies the real
//! implementations (the desktop app, its clipboard) and the tests supply
//! fakes (`tests/common`). Nothing in the core touches the microphone, the
//! transcription model, the filesystem or the clipboard except through
//! these.

use crate::dictation::Job;

/// A failed effect, as one readable sentence.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct PortError(pub String);

/// Any error as a [`PortError`], for an adapter's `map_err(fail)`.
pub fn fail(e: impl std::fmt::Display) -> PortError {
    PortError(e.to_string())
}

impl From<crate::sync::Closed> for PortError {
    fn from(_: crate::sync::Closed) -> Self {
        Self("the engine has shut down".to_string())
    }
}

/// Sound as the input device delivered it.
#[derive(Debug, Clone, PartialEq)]
pub struct Captured {
    /// Interleaved samples, nominally between -1 and 1.
    pub samples: Vec<f32>,
    /// Channels per frame.
    pub channels: u16,
    /// Frames per second.
    pub sample_rate: u32,
}

impl Captured {
    /// `samples` interleaved over `channels` at `sample_rate` frames per
    /// second.
    #[must_use]
    pub fn new(samples: Vec<f32>, channels: u16, sample_rate: u32) -> Self {
        Self {
            samples,
            channels,
            sample_rate,
        }
    }
}

/// The microphone.
pub trait Recorder {
    /// Starts capturing a segment from the default input device.
    ///
    /// # Errors
    ///
    /// There is no input device, or it cannot be opened.
    fn start(&self) -> Result<(), PortError>;

    /// Stops capturing and hands over everything captured since `start`.
    ///
    /// # Errors
    ///
    /// Nothing was being captured, or the device failed while it was.
    fn stop(&self) -> Result<Captured, PortError>;
}

/// Speech to text.
pub trait Transcriber {
    /// Where the model should be, when it is not installed there.
    fn missing_model(&self) -> Option<String>;

    /// Loads the model, if it is not loaded yet, so the first transcription
    /// does not wait for it. A transcription asked for while this runs waits
    /// for it rather than loading the model again.
    ///
    /// # Errors
    ///
    /// The model cannot be loaded.
    fn warm(&self) -> Result<(), PortError>;

    /// The text spoken in `samples` (16 kHz mono), steered by `prompt`.
    ///
    /// # Errors
    ///
    /// The model cannot be loaded or the transcription fails.
    fn transcribe(&self, samples: &[f32], prompt: Option<&str>) -> Result<String, PortError>;

    /// Frees the loaded model, first waiting for a load or transcription
    /// already in progress to finish (calls still waiting for the model are
    /// refused, not run). Afterwards `warm` and `transcribe` fail with a
    /// [`PortError`] and never load the model again. Returns whether a loaded
    /// model was freed (a second call returns false).
    fn unload(&self) -> bool;
}

/// The app's data folder. Paths are relative to it and use `/`; a path that
/// starts with `/` is absolute and names that file wherever it is.
pub trait Files {
    /// The contents of the file at `path`.
    ///
    /// # Errors
    ///
    /// The file is missing or unreadable.
    fn read(&self, path: &str) -> Result<Vec<u8>, PortError>;

    /// Writes the file at `path`, creating its folder if needed.
    ///
    /// # Errors
    ///
    /// The file cannot be written.
    fn write(&self, path: &str, bytes: &[u8]) -> Result<(), PortError>;

    /// The names of the files in `folder`, in any order; a missing folder
    /// has none.
    ///
    /// # Errors
    ///
    /// The folder cannot be read.
    fn list(&self, folder: &str) -> Result<Vec<String>, PortError>;

    /// Deletes the file at `path`.
    ///
    /// # Errors
    ///
    /// The file cannot be deleted.
    fn remove(&self, path: &str) -> Result<(), PortError>;

    /// Adds `bytes` to the end of the file at `path`, which must exist.
    ///
    /// # Errors
    ///
    /// The file is missing or cannot be written.
    fn append(&self, path: &str, bytes: &[u8]) -> Result<(), PortError>;

    /// Renames the file at `from` to `to` in one step, replacing any file at
    /// `to`: a reader sees the old file or the new one, never part of it.
    ///
    /// # Errors
    ///
    /// The file cannot be renamed.
    fn rename(&self, from: &str, to: &str) -> Result<(), PortError>;
}

/// The network, for the speech model's download.
pub trait Downloader {
    /// Requests `url`, following redirects, and answers with its body to
    /// read. A read fails when the connection drops before the body ends.
    ///
    /// # Errors
    ///
    /// The request fails or the server answers with an error status.
    fn open(&self, url: &str) -> Result<Box<dyn std::io::Read>, PortError>;
}

/// The system clipboard.
pub trait Clipboard {
    /// Replaces the clipboard's contents with `text`.
    ///
    /// # Errors
    ///
    /// The clipboard cannot be written.
    fn write(&self, text: &str) -> Result<(), PortError>;
}

/// Where jobs go to run off the caller's thread: closed segments to
/// transcribe, the model's warm-up and its download (the engine,
/// `crates/scriber-engine`, runs each on a std thread of its own through
/// [`crate::dictation::work`], which hands the result to
/// [`crate::dictation::Dictation::complete`]).
pub trait JobQueue {
    /// Queues `job`.
    ///
    /// # Errors
    ///
    /// The worker is gone.
    fn send(&self, job: Job) -> Result<(), PortError>;
}

/// A moment in the Mac's local wall-clock time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalTime {
    /// The year, e.g. 2026.
    pub year: i32,
    /// The month, 1 to 12.
    pub month: u32,
    /// The day of the month, 1 to 31.
    pub day: u32,
    /// The hour, 0 to 23.
    pub hour: u32,
    /// The minute, 0 to 59.
    pub minute: u32,
    /// The second, 0 to 59.
    pub second: u32,
}

/// The clock, for naming a library entry after its session's start.
pub trait Clock {
    /// The local wall-clock time now.
    fn now(&self) -> LocalTime;
}

/// Audio compression, for a library entry's audio.
pub trait Encoder {
    /// Encodes the WAV file at `wav` as AAC in an M4A file at `m4a`; both
    /// paths are relative to the data folder, as for [`Files`].
    ///
    /// # Errors
    ///
    /// The encoder is missing or fails.
    fn to_m4a(&self, wav: &str, m4a: &str) -> Result<(), PortError>;
}

/// A local language model, for a library entry's one-sentence summary.
pub trait Summarizer {
    /// The model's raw answer for `transcript`; cleaning it is the core's
    /// job ([`crate::library::clean_summary`]).
    ///
    /// # Errors
    ///
    /// The model cannot be reached or answers with an error.
    fn summarize(&self, transcript: &str) -> Result<String, PortError>;
}

/// A local language model, for the optional cleanup pass over a finished
/// transcript ([`crate::cleanup`]).
pub trait Cleaner {
    /// The model's raw answer to the cleanup `request`, under the `system`
    /// prompt; judging the answer is the core's job
    /// ([`crate::cleanup::accept`]).
    ///
    /// # Errors
    ///
    /// The model cannot be reached, answers with an error, or the reply was
    /// cut off.
    fn clean(&self, system: &str, request: &str) -> Result<String, PortError>;
}

/// The ports only the recording library needs ([`crate::library`]).
#[derive(Clone, Copy)]
pub struct Library<'a> {
    /// Names each entry after its session's start.
    pub clock: &'a dyn Clock,
    /// Compresses each entry's audio.
    pub encoder: &'a dyn Encoder,
    /// Writes each entry's summary.
    pub summarizer: &'a dyn Summarizer,
}

/// Every port a session action needs, borrowed for the length of one call.
#[derive(Clone, Copy)]
pub struct Ports<'a> {
    /// The microphone.
    pub recorder: &'a dyn Recorder,
    /// Consulted for whether the model is installed; transcription itself
    /// runs off the caller's thread through [`crate::dictation::Job::run`].
    pub transcriber: &'a dyn Transcriber,
    /// The data folder: the session's audio and the glossary.
    pub files: &'a dyn Files,
    /// Where the finished transcript goes.
    pub clipboard: &'a dyn Clipboard,
    /// The transcription worker's queue.
    pub queue: &'a dyn JobQueue,
    /// The network, for the model's download.
    pub downloader: &'a dyn Downloader,
    /// The recording library's ports; `None` keeps no library.
    pub library: Option<Library<'a>>,
    /// The transcript's cleanup model; `None` never cleans up.
    pub cleaner: Option<&'a dyn Cleaner>,
}

impl<'a> Ports<'a> {
    /// Gathers the ports; each parameter is a different trait, so a mix-up
    /// does not compile.
    #[must_use]
    pub fn new(
        recorder: &'a dyn Recorder,
        transcriber: &'a dyn Transcriber,
        files: &'a dyn Files,
        clipboard: &'a dyn Clipboard,
        queue: &'a dyn JobQueue,
        downloader: &'a dyn Downloader,
    ) -> Self {
        Self {
            recorder,
            transcriber,
            files,
            clipboard,
            queue,
            downloader,
            library: None,
            cleaner: None,
        }
    }

    /// These ports, keeping finished sessions in the recording library.
    #[must_use]
    pub fn with_library(self, library: Library<'a>) -> Self {
        Self {
            library: Some(library),
            ..self
        }
    }

    /// These ports, cleaning up each transcript with `cleaner` when the
    /// setting is on.
    #[must_use]
    pub fn with_cleaner(self, cleaner: &'a dyn Cleaner) -> Self {
        Self {
            cleaner: Some(cleaner),
            ..self
        }
    }
}
