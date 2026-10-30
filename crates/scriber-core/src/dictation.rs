//! The dictation session: a state machine over [`Phase`].
//!
//! ```text
//! Idle --new--> Ready --record--> Recording --pause--> Paused --resume--> Recording
//! Recording | Paused --done--> Transcribing --(all text in)--> Finished | Failed
//! Transcribing --(all text in, cleanup on)--> Transcribing --(cleanup answered)--> Finished
//! Failed --retry--> Transcribing
//! Idle | Finished | Failed --new--> Ready (an empty session)
//! anything but Idle and Transcribing --discard--> Ready (an empty session)
//! ```
//!
//! Each pause closes a segment. The caller transcribes it off its own
//! thread with [`Job::run`] and hands the [`Outcome`] back to
//! [`Dictation::complete`], so segments transcribe while the user thinks and
//! Done only waits for the last one. Outcomes may arrive in any order; the
//! transcript joins segments in recording order. With the cleanup setting on
//! and a cleanup model in the ports, a non-empty transcript then goes to the
//! model as one more job ([`crate::cleanup`]) and the session stays
//! transcribing until it answers; the transcript copied is the model's reply
//! when [`cleanup::accept`] keeps it, and Whisper's text otherwise. An illegal
//! action returns [`DictationError::Illegal`] and changes nothing.

use std::collections::BTreeMap;
use std::sync::Mutex;

use crate::audio::{decode_wav, duration_ms, encode_wav, to_mono_16k};
use crate::cleanup;
use crate::download::{self, Source};
use crate::library::{self, CleanupRecord, clean_summary, entry_path, valid_id};
use crate::model::{Action, Download, Phase, Snapshot};
use crate::ports::{Library, PortError, Ports};
use crate::settings;
use crate::store;
use crate::sync::lock;
use crate::transcript::{is_silent, join, keep, trim_trailing_silence};
use crate::vocabulary::{self, prompt};
use serde::Serialize;

/// Why an action was refused. The session is unchanged.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DictationError {
    /// The action is not allowed in the current phase.
    #[error("Cannot {action} while {phase}.")]
    Illegal {
        /// What was asked for.
        action: Action,
        /// Where the session was.
        phase: Phase,
    },
    /// The speech model is not installed (or still downloading); the text
    /// says which.
    #[error("{0}")]
    ModelMissing(String),
    /// A model download was asked for while the model is installed or
    /// already downloading.
    #[error("The speech model is already installed or downloading.")]
    NoDownload,
    /// A model download was asked for while the model is a local file, so
    /// there is no source to download it from.
    #[error("The speech model is a local file, so there is nothing to download.")]
    LocalModel,
    /// The microphone could not start.
    #[error("Could not start recording: {0}")]
    Recorder(PortError),
    /// The last session's audio could not be deleted.
    #[error("Could not clear the last session's audio: {0}")]
    Store(PortError),
    /// The clipboard could not be written.
    #[error("Could not copy to the clipboard: {0}")]
    Clipboard(PortError),
}

/// The sentence shown when the speech model is missing and not downloading.
#[must_use]
pub fn model_missing(path: &str) -> String {
    format!("The speech model is missing from {path}.")
}

/// The sentence shown when the speech model's download failed.
#[must_use]
pub fn download_failed(reason: &str) -> String {
    format!("Could not download the speech model: {reason}.")
}

/// Why Record is refused while the model downloads.
pub const DOWNLOADING: &str = "The speech model is still downloading.";

/// The notice shown when the chosen input device `wanted` would not open, so
/// the segment records from the Mac's default device, `opened`, instead.
#[must_use]
pub fn input_fallback(wanted: &str, opened: &str) -> String {
    format!("{wanted} is not available, so this part is recording from the Mac default, {opened}.")
}

/// Shown when the transcript was copied.
pub const COPIED: &str = "Copied to the clipboard.";

/// Shown when every segment was silent.
pub const NOTHING_HEARD: &str = "No speech was heard. If you spoke, check that Scriber may use the microphone in System Settings, Privacy & Security, Microphone.";

/// Work for the worker: one segment to transcribe, made by Pause, Done or
/// Retry; the speech model's warm-up, queued at launch and after a
/// download; the model's download, queued at launch when it is missing
/// and by [`Action::DownloadModel`]; a finished transcript to clean up, when
/// the cleanup setting is on; or, with the library on, a new library entry's
/// audio to encode and its summary to write.
#[derive(Debug, Clone, PartialEq)]
pub struct Job(Work);

#[derive(Debug, Clone, PartialEq)]
enum Work {
    /// Load the model, so the first segment does not wait for it.
    Warm,
    /// Download the model.
    Download(Source),
    /// Transcribe segment `index` of session `session`.
    Segment {
        session: u64,
        index: u32,
        samples: Vec<f32>,
        prompt: Option<String>,
    },
    /// Encode library entry `id`'s audio.wav as audio.m4a.
    Encode { id: String },
    /// Summarize library entry `id`, whose transcript is `text`.
    Summary { id: String, text: String },
    /// Clean up session `session`'s transcript `text` with glossary `terms`.
    Cleanup {
        session: u64,
        text: String,
        terms: Vec<String>,
    },
}

/// Which job an outcome is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mark {
    Warm,
    Download,
    Segment { session: u64, index: u32 },
    Encode,
    Summary,
    Cleanup { session: u64 },
}

impl Job {
    fn segment(session: u64, index: u32, samples: Vec<f32>, prompt: Option<String>) -> Self {
        Self(Work::Segment {
            session,
            index,
            samples,
            prompt,
        })
    }

    fn mark(&self) -> Mark {
        match &self.0 {
            Work::Warm => Mark::Warm,
            Work::Download(_) => Mark::Download,
            Work::Segment { session, index, .. } => Mark::Segment {
                session: *session,
                index: *index,
            },
            Work::Encode { .. } => Mark::Encode,
            Work::Summary { .. } => Mark::Summary,
            Work::Cleanup { session, .. } => Mark::Cleanup { session: *session },
        }
    }

    /// Does the work. A silent segment is not sent to the transcriber, any
    /// other segment is trimmed of trailing silence before transcription, and
    /// the hallucination length test measures the trimmed audio, because that
    /// is what Whisper heard. A
    /// hallucination or text with no letter or digit is dropped (see [`crate::transcript`]). A download
    /// calls `progress` with the bytes received as each whole percent
    /// arrives (see [`download::fetch`]). The warm-up's outcome changes
    /// nothing: a model that failed to load is loaded again by the first
    /// segment, which reports the failure. An encode replaces the entry's
    /// audio.wav with audio.m4a, keeping the WAV when encoding fails; a
    /// summary replaces the entry's excerpt with the model's sentence, and
    /// leaves it when the model fails or says nothing. A cleanup asks the
    /// cleanup model for its raw reply to [`cleanup::request`] under
    /// [`cleanup::PROMPT`]; judging it is left to [`Dictation::complete`].
    #[must_use]
    pub fn run(&self, ports: &Ports<'_>, progress: &dyn Fn(u64)) -> Outcome {
        let result = match &self.0 {
            Work::Warm => ports
                .transcriber
                .warm()
                .map(|()| None)
                .map_err(|e| e.to_string()),
            Work::Download(source) => {
                download::fetch(source, ports.files, ports.downloader, progress).map(|()| None)
            }
            Work::Segment { samples, .. } if is_silent(samples) => Ok(None),
            Work::Segment {
                samples, prompt, ..
            } => {
                let heard_audio = trim_trailing_silence(samples);
                ports
                    .transcriber
                    .transcribe(heard_audio, prompt.as_deref())
                    .map(|heard| keep(heard_audio.len(), &heard))
                    .map_err(|e| e.to_string())
            }
            Work::Encode { id } => encode(id, ports).map(|()| None),
            Work::Summary { id, text } => summarize(id, text, ports).map(Some),
            Work::Cleanup { text, terms, .. } => match ports.cleaner {
                None => Err(CLEANUP_OFF.to_string()),
                Some(cleaner) => cleaner
                    .clean(
                        &settings::load(ports.files).cleanup_model,
                        cleanup::PROMPT,
                        &cleanup::request(text, terms),
                    )
                    .map(Some)
                    .map_err(|e| e.to_string()),
            },
        };
        Outcome {
            mark: self.mark(),
            result,
        }
    }
}

/// Why a library job did nothing, when the library is off.
const LIBRARY_OFF: &str = "the library is off";

/// Why a cleanup job did nothing, when there is no cleanup model.
const CLEANUP_OFF: &str = "the cleanup model is off";

/// Why a library job did nothing, for an id that is not an entry's.
const NOT_IN_LIBRARY: &str = "not in the library";

/// The library's ports, for a job on entry `id`.
fn library_for<'a>(id: &str, ports: &Ports<'a>) -> Result<Library<'a>, String> {
    let library = ports.library.ok_or(LIBRARY_OFF)?;
    if valid_id(id) {
        Ok(library)
    } else {
        Err(NOT_IN_LIBRARY.to_string())
    }
}

/// Encodes entry `id`'s audio.wav as audio.m4a, removing whichever of the
/// two is not kept (ignoring a failure to remove it).
fn encode(id: &str, ports: &Ports<'_>) -> Result<(), String> {
    let library = library_for(id, ports)?;
    let (wav, m4a) = (entry_path(id, "audio.wav"), entry_path(id, "audio.m4a"));
    match library.encoder.to_m4a(&wav, &m4a) {
        Ok(()) => {
            let _removed = ports.files.remove(&wav);
            Ok(())
        }
        Err(e) => {
            let _removed = ports.files.remove(&m4a);
            Err(e.to_string())
        }
    }
}

/// Asks the model for entry `id`'s summary and writes it into the entry.
fn summarize(id: &str, text: &str, ports: &Ports<'_>) -> Result<String, String> {
    let library = library_for(id, ports)?;
    let model = settings::load(ports.files).summary_model;
    let raw = library
        .summarizer
        .summarize(&model, text)
        .map_err(|e| e.to_string())?;
    let summary = clean_summary(&raw).ok_or("the summary was empty")?;
    library::set_summary(ports.files, id, &summary, &model).map_err(|e| e.to_string())?;
    Ok(summary)
}

/// A job's result, for [`Dictation::complete`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    mark: Mark,
    result: Result<Option<String>, String>,
}

/// The speech model's download.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
enum Fetch {
    /// Not running: the model is installed, or nothing has started it.
    #[default]
    Idle,
    /// Running, with this many bytes received.
    Running(u64),
    /// Failed, for this reason.
    Failed(String),
}

/// Runs `job` on a worker thread for the session in `shared`, reporting
/// each snapshot it changes through `report`: a download's progress as it
/// goes, then the outcome. The lock is held only to record them, never
/// while the job works.
pub fn work(shared: &Mutex<Dictation>, job: &Job, ports: &Ports<'_>, report: &dyn Fn(Snapshot)) {
    let progress = |bytes| report(lock(shared).progress(bytes, ports));
    let outcome = job.run(ports, &progress);
    report(lock(shared).complete(outcome, ports));
}

/// The session's start kept in [`store::STARTED`], if it is a title.
fn started(ports: &Ports<'_>) -> Option<String> {
    let bytes = ports.files.read(store::STARTED).ok()?;
    let text = String::from_utf8(bytes).ok()?;
    let text = text.trim();
    let shape = "DDDD-DD-DD DD:DD:DD";
    let fits = text.chars().count() == shape.len()
        && text.chars().zip(shape.chars()).all(|(c, s)| match s {
            'D' => c.is_ascii_digit(),
            other => c == other,
        });
    fits.then(|| text.to_string())
}

/// A [`store::MICROPHONE_LOG`] line for one Record or Resume.
#[derive(Serialize)]
struct StartLine<'a> {
    event: &'static str,
    started: Option<&'a str>,
    action: &'static str,
    wanted: &'a str,
    opened: Option<&'a str>,
    outcome: &'static str,
    first_error: Option<String>,
    second_error: Option<String>,
    mismatch: bool,
}

/// A [`store::MICROPHONE_LOG`] line for one closed segment.
#[derive(Serialize)]
struct SegmentLine<'a> {
    event: &'static str,
    started: Option<&'a str>,
    device: Option<&'a str>,
    channels: u16,
    sample_rate: u32,
    samples: usize,
    peak: f32,
}

/// Writes `line` to [`store::MICROPHONE_LOG`], best effort.
fn log(ports: &Ports<'_>, line: &impl Serialize) {
    if let Ok(text) = serde_json::to_string(line) {
        store::log_microphone(ports.files, &text);
    }
}

/// A segment's state.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Slot {
    /// Being transcribed.
    Pending,
    /// Transcribed: the kept text, or `None` for silence.
    Heard(Option<String>),
    /// Not transcribed, for this reason.
    Failed(String),
}

/// One dictation session and the app's place in it. The default is idle,
/// with no session and no download source: until [`Self::launch`] or
/// [`Self::launch_from`] sets one, [`Action::DownloadModel`] is refused with
/// [`DictationError::LocalModel`].
#[derive(Debug, Default)]
pub struct Dictation {
    phase: Phase,
    /// Bumped by every New and Discard, so a late outcome from a session
    /// that is gone is ignored.
    session: u64,
    slots: BTreeMap<u32, Slot>,
    next_index: u32,
    /// The length of the audio of every closed segment.
    recorded_ms: u64,
    prompt: Option<String>,
    text: String,
    error: Option<String>,
    notice: Option<String>,
    /// Bumped by every applied action and every accepted result.
    revision: u64,
    /// The speech model's download.
    fetch: Fetch,
    /// The session's start, as a library title, once it has recorded with
    /// the library on.
    started: Option<String>,
    /// Bumped by every entry added to the library and every summary written.
    library_revision: u64,
    /// What the download fetches: the model, except in tests; `None` when
    /// the model is a local file ([`Self::launch_local`]).
    source: Option<Source>,
    /// While the cleanup model works on the transcript: Whisper's
    /// transcript and the glossary terms sent with it.
    cleaning: Option<(String, Vec<String>)>,
    /// The input device opened for the session's latest segment.
    microphone: Option<String>,
}

impl Dictation {
    /// Runs one action from the window and answers with the new snapshot.
    /// `text` is the edited transcript for [`Action::Copy`] and ignored
    /// otherwise. Segments the action closes go to the queue; one the queue
    /// refuses fails at once, so it can be retried.
    ///
    /// # Errors
    ///
    /// The action's own errors; see each method.
    pub fn apply(
        &mut self,
        action: Action,
        text: &str,
        ports: &Ports<'_>,
    ) -> Result<Snapshot, DictationError> {
        let jobs = match action {
            Action::New => self.new_session(ports).map(|()| Vec::new()),
            Action::Record => self.record(ports).map(|()| Vec::new()),
            Action::Pause => self.pause(ports).map(Vec::from_iter),
            Action::Resume => self.resume(ports).map(|()| Vec::new()),
            Action::Done => self.done(ports).map(Vec::from_iter),
            Action::Discard => self.discard(ports).map(|()| Vec::new()),
            Action::Retry => self.retry(ports),
            Action::Copy => self.copy(text, ports).map(|()| Vec::new()),
            Action::DownloadModel => self.download_model(ports).map(|job| vec![job]),
        }?;
        self.revision = self.revision.wrapping_add(1);
        self.queue(jobs, ports);
        Ok(self.snapshot(ports))
    }

    /// Sends each job to the worker; one the queue refuses fails at once.
    fn queue(&mut self, jobs: Vec<Job>, ports: &Ports<'_>) {
        for job in jobs {
            let mark = job.mark();
            if let Err(e) = ports.queue.send(job) {
                let result = Err(format!("it could not be queued: {e}"));
                self.record_outcome(Outcome { mark, result }, ports);
            }
        }
    }

    /// The state at launch ([`Self::recover`]). When the speech model is
    /// installed its warm-up is queued, so the first segment does not wait
    /// seconds for it to load (a queue that refuses it only means the first
    /// segment loads the model itself); when it is missing its download is.
    #[must_use]
    pub fn launch(ports: &Ports<'_>) -> Self {
        Self::launch_from(Source::default(), ports)
    }

    /// [`Self::launch`], downloading the model from `source`.
    #[must_use]
    pub fn launch_from(source: Source, ports: &Ports<'_>) -> Self {
        let mut this = Self {
            source: Some(source.clone()),
            ..Self::recover(ports)
        };
        let job = if ports.transcriber.missing_model().is_some() {
            this.fetch = Fetch::Running(0);
            Job(Work::Download(source))
        } else {
            Job(Work::Warm)
        };
        this.queue(vec![job], ports);
        this
    }

    /// [`Self::launch`] for a model that is a local file, with no download
    /// source: the warm-up is queued when the model is installed, and
    /// nothing when it is missing (the snapshot says where it belongs, and
    /// [`Action::DownloadModel`] is refused with [`DictationError::LocalModel`]).
    #[must_use]
    pub fn launch_local(ports: &Ports<'_>) -> Self {
        let mut this = Self::recover(ports);
        if ports.transcriber.missing_model().is_none() {
            this.queue(vec![Job(Work::Warm)], ports);
        }
        this
    }

    /// The state found on disk: finished with the last session's kept
    /// transcript; failed with the segments of a session that never finished
    /// (a crash, a quit mid-transcription) waiting for Retry; or idle. It
    /// has no download source, so [`Action::DownloadModel`] is refused with
    /// [`DictationError::LocalModel`].
    #[must_use]
    pub fn recover(ports: &Ports<'_>) -> Self {
        let mut this = Self {
            started: started(ports),
            ..Self::default()
        };
        let stored = store::stored(ports.files).unwrap_or_default();
        if let Some(text) = store::transcript(ports.files) {
            this.slots = stored.iter().map(|i| (*i, Slot::Heard(None))).collect();
            this.text = text;
            this.phase = Phase::Finished;
        } else if !stored.is_empty() {
            this.slots = stored
                .iter()
                .map(|i| (*i, Slot::Failed("interrupted".to_string())))
                .collect();
            this.prompt = Some(prompt(&store::vocabulary(ports.files).unwrap_or_default()));
            this.phase = Phase::Failed;
            this.error = Some(format!(
                "The last session was not transcribed. Retry to transcribe its {} recorded {}, or start a new session.",
                stored.len(),
                if stored.len() == 1 { "part" } else { "parts" }
            ));
        }
        this
    }

    /// What the window shows now.
    #[must_use]
    pub fn snapshot(&self, ports: &Ports<'_>) -> Snapshot {
        let count = |pending: bool| {
            let n = self
                .slots
                .values()
                .filter(|s| pending == (**s == Slot::Pending))
                .count();
            u32::try_from(n).unwrap_or(u32::MAX)
        };
        Snapshot {
            revision: self.revision,
            phase: self.phase,
            recorded_ms: self.recorded_ms,
            segments: count(true).saturating_add(count(false)),
            pending: count(true),
            text: self.text.clone(),
            error: self.error.clone(),
            notice: self.notice.clone(),
            model_error: self.model_error(ports),
            download: match self.fetch {
                Fetch::Running(received) => Some(Download {
                    received_bytes: received,
                    total_bytes: self.source.as_ref().map_or(0, |s| s.bytes),
                }),
                Fetch::Idle | Fetch::Failed(_) => None,
            },
            library_revision: self.library_revision,
            microphone: self.microphone.clone(),
        }
    }

    /// Why the model is missing, unless it is downloading.
    fn model_error(&self, ports: &Ports<'_>) -> Option<String> {
        let path = ports.transcriber.missing_model()?;
        match &self.fetch {
            Fetch::Idle => Some(model_missing(&path)),
            Fetch::Running(_) => None,
            Fetch::Failed(reason) => Some(download_failed(reason)),
        }
    }

    /// Records how far the model's download has got and answers with the
    /// new snapshot; ignored unless a download is running.
    pub fn progress(&mut self, received: u64, ports: &Ports<'_>) -> Snapshot {
        if let Fetch::Running(bytes) = &mut self.fetch {
            *bytes = received;
            self.revision = self.revision.wrapping_add(1);
        }
        self.snapshot(ports)
    }

    /// Download model: starts the model's download again, in any phase.
    ///
    /// # Errors
    ///
    /// The model is a local file with no download source; the model is
    /// installed or already downloading.
    pub fn download_model(&mut self, ports: &Ports<'_>) -> Result<Job, DictationError> {
        let Some(source) = self.source.clone() else {
            return Err(DictationError::LocalModel);
        };
        if ports.transcriber.missing_model().is_none() || matches!(self.fetch, Fetch::Running(_)) {
            return Err(DictationError::NoDownload);
        }
        self.fetch = Fetch::Running(0);
        Ok(Job(Work::Download(source)))
    }

    fn allow(&self, action: Action, allowed: &[Phase]) -> Result<(), DictationError> {
        if allowed.contains(&self.phase) {
            Ok(())
        } else {
            Err(DictationError::Illegal {
                action,
                phase: self.phase,
            })
        }
    }

    /// Empties the session (after its audio is cleared) and makes it Ready.
    fn reset(&mut self, ports: &Ports<'_>) {
        self.session = self.session.wrapping_add(1);
        self.slots.clear();
        self.next_index = 0;
        self.recorded_ms = 0;
        self.prompt = Some(prompt(&store::vocabulary(ports.files).unwrap_or_default()));
        self.text.clear();
        self.error = None;
        self.notice = None;
        self.started = None;
        self.microphone = None;
        self.phase = Phase::Ready;
    }

    /// New: an empty session, its glossary read afresh, the last session's
    /// audio deleted.
    ///
    /// # Errors
    ///
    /// Illegal while ready, recording, paused or transcribing; the old audio
    /// cannot be deleted.
    pub fn new_session(&mut self, ports: &Ports<'_>) -> Result<(), DictationError> {
        self.allow(Action::New, &[Phase::Idle, Phase::Finished, Phase::Failed])?;
        store::clear(ports.files).map_err(DictationError::Store)?;
        self.reset(ports);
        Ok(())
    }

    /// Discard: like New, from any phase that has a session and is not
    /// transcribing; a running recording is stopped and thrown away.
    ///
    /// # Errors
    ///
    /// Illegal while idle or transcribing; the audio cannot be deleted.
    pub fn discard(&mut self, ports: &Ports<'_>) -> Result<(), DictationError> {
        self.allow(
            Action::Discard,
            &[
                Phase::Ready,
                Phase::Recording,
                Phase::Paused,
                Phase::Finished,
                Phase::Failed,
            ],
        )?;
        store::clear(ports.files).map_err(DictationError::Store)?;
        if self.phase == Phase::Recording {
            // The recording is being thrown away, so a failure to stop
            // cleanly loses nothing.
            let _stopped = ports.recorder.stop();
        }
        self.reset(ports);
        Ok(())
    }

    /// Starts the microphone; shared by Record and Resume. It opens the input
    /// device the settings name, or the Mac's default when they name none;
    /// when the named device will not open it falls back once to the Mac's
    /// default and says so in the notice, leaving the setting as it is.
    /// Every attempt that reaches the microphone is written to
    /// [`store::MICROPHONE_LOG`].
    fn start(
        &mut self,
        action: Action,
        from: Phase,
        ports: &Ports<'_>,
    ) -> Result<(), DictationError> {
        self.allow(action, &[from])?;
        if ports.transcriber.missing_model().is_some() {
            let why = self.model_error(ports).unwrap_or(DOWNLOADING.to_string());
            return Err(DictationError::ModelMissing(why));
        }
        let wanted = settings::load(ports.files).input_device;
        let first = ports.recorder.start(&wanted);
        let second = match &first {
            Err(_) if !wanted.is_empty() => Some(ports.recorder.start("")),
            Ok(_) | Err(_) => None,
        };
        let error_text = |r: Option<&Result<String, PortError>>| match r {
            Some(Err(e)) => Some(e.to_string()),
            Some(Ok(_)) | None => None,
        };
        let mismatch = matches!(&first, Ok(name) if !wanted.is_empty() && *name != wanted);
        let result = match (&first, &second) {
            (Ok(name), _) => Ok((name.clone(), None)),
            (Err(_), Some(Ok(name))) => Ok((name.clone(), Some(input_fallback(&wanted, name)))),
            (Err(_), Some(Err(e))) | (Err(e), None) => Err(e.clone()),
        };
        let (opened, outcome) = match &result {
            Ok((name, None)) => (Some(name.as_str()), "opened"),
            Ok((name, Some(_))) => (Some(name.as_str()), "fallback"),
            Err(_) => (None, "failed"),
        };
        log(
            ports,
            &StartLine {
                event: "start",
                started: self.started.as_deref(),
                action: if action == Action::Record {
                    "record"
                } else {
                    "resume"
                },
                wanted: &wanted,
                opened,
                outcome,
                first_error: error_text(Some(&first)),
                second_error: error_text(second.as_ref()),
                mismatch,
            },
        );
        let (name, notice) = result.map_err(DictationError::Recorder)?;
        self.microphone = Some(name);
        self.notice = notice;
        self.phase = Phase::Recording;
        Ok(())
    }

    /// Record: starts the first segment.
    ///
    /// # Errors
    ///
    /// Illegal unless ready; the model is missing; the microphone fails.
    pub fn record(&mut self, ports: &Ports<'_>) -> Result<(), DictationError> {
        self.start(Action::Record, Phase::Ready, ports)?;
        if let Some(lib) = ports.library
            && self.started.is_none()
        {
            let title = library::title(&lib.clock.now());
            // Losing it only means the entry is titled when it is archived.
            let _kept = ports.files.write(store::STARTED, title.as_bytes());
            self.started = Some(title);
        }
        Ok(())
    }

    /// Resume: starts another segment.
    ///
    /// # Errors
    ///
    /// Illegal unless paused; the model is missing; the microphone fails.
    pub fn resume(&mut self, ports: &Ports<'_>) -> Result<(), DictationError> {
        self.start(Action::Resume, Phase::Paused, ports)
    }

    /// Stops the microphone and turns what it captured into a pending
    /// segment, its audio saved for Retry. A microphone failure loses that
    /// segment and says so in the notice.
    fn close_segment(&mut self, ports: &Ports<'_>) -> Option<Job> {
        let captured = match ports.recorder.stop() {
            Ok(captured) => captured,
            Err(e) => {
                self.notice = Some(format!(
                    "The microphone stopped: {e}. The last part was not recorded."
                ));
                return None;
            }
        };
        let peak = captured.samples.iter().fold(0.0_f32, |m, s| m.max(s.abs()));
        log(
            ports,
            &SegmentLine {
                event: "segment",
                started: self.started.as_deref(),
                device: self.microphone.as_deref(),
                channels: captured.channels,
                sample_rate: captured.sample_rate,
                samples: captured.samples.len(),
                peak,
            },
        );
        let samples = to_mono_16k(&captured);
        self.recorded_ms = self.recorded_ms.saturating_add(duration_ms(samples.len()));
        let index = self.next_index;
        self.next_index = index.saturating_add(1);
        if let Err(e) = store::save(ports.files, index, &encode_wav(&samples)) {
            self.notice = Some(format!("Could not keep this part's audio for Retry: {e}."));
        }
        let _: Option<Slot> = self.slots.insert(index, Slot::Pending);
        Some(Job::segment(
            self.session,
            index,
            samples,
            self.prompt.clone(),
        ))
    }

    /// Pause: closes the running segment. Returns it for transcription.
    ///
    /// # Errors
    ///
    /// Illegal unless recording.
    pub fn pause(&mut self, ports: &Ports<'_>) -> Result<Option<Job>, DictationError> {
        self.allow(Action::Pause, &[Phase::Recording])?;
        self.notice = None;
        let job = self.close_segment(ports);
        self.phase = Phase::Paused;
        Ok(job)
    }

    /// Done: closes the running segment, if any, and waits for every
    /// segment's text. Returns the closed segment for transcription.
    ///
    /// # Errors
    ///
    /// Illegal unless recording or paused.
    pub fn done(&mut self, ports: &Ports<'_>) -> Result<Option<Job>, DictationError> {
        self.allow(Action::Done, &[Phase::Recording, Phase::Paused])?;
        self.notice = None;
        let job = if self.phase == Phase::Recording {
            self.close_segment(ports)
        } else {
            None
        };
        self.phase = Phase::Transcribing;
        self.settle(ports);
        Ok(job)
    }

    /// Retry: reads each failed segment's audio back and returns them for
    /// transcription. A segment whose audio cannot be read fails again.
    ///
    /// # Errors
    ///
    /// Illegal unless failed.
    pub fn retry(&mut self, ports: &Ports<'_>) -> Result<Vec<Job>, DictationError> {
        self.allow(Action::Retry, &[Phase::Failed])?;
        self.notice = None;
        self.error = None;
        let mut jobs = Vec::new();
        for (index, slot) in &mut self.slots {
            if !matches!(slot, Slot::Failed(_)) {
                continue;
            }
            let audio = store::load(ports.files, *index).map_err(|e| e.to_string());
            match audio.and_then(|bytes| decode_wav(&bytes).map_err(|e| e.to_string())) {
                Ok(samples) => {
                    *slot = Slot::Pending;
                    jobs.push(Job::segment(
                        self.session,
                        *index,
                        samples,
                        self.prompt.clone(),
                    ));
                }
                Err(e) => *slot = Slot::Failed(format!("its audio could not be read: {e}")),
            }
        }
        self.phase = Phase::Transcribing;
        self.settle(ports);
        Ok(jobs)
    }

    /// Records a job's result and answers with the new snapshot. An
    /// outcome from a session that has since been discarded, for a segment
    /// that is not pending, from the warm-up or from an encode, is ignored;
    /// a summary written bumps the library's revision.
    pub fn complete(&mut self, outcome: Outcome, ports: &Ports<'_>) -> Snapshot {
        self.record_outcome(outcome, ports);
        self.snapshot(ports)
    }

    fn record_outcome(&mut self, outcome: Outcome, ports: &Ports<'_>) {
        match outcome.mark {
            Mark::Warm | Mark::Encode => {}
            Mark::Download => self.downloaded(outcome.result, ports),
            Mark::Segment { session, index } => self.heard(session, index, outcome.result, ports),
            Mark::Summary => {
                if let Ok(Some(_)) = outcome.result {
                    self.bump_library();
                }
            }
            Mark::Cleanup { session } => self.cleaned(session, outcome.result, ports),
        }
    }

    /// The cleanup model answered for `session`: finish with its reply when
    /// [`cleanup::accept`] keeps it, and with Whisper's transcript otherwise.
    /// Ignored unless that session is still waiting for it.
    fn cleaned(&mut self, session: u64, result: Result<Option<String>, String>, ports: &Ports<'_>) {
        if session != self.session || self.phase != Phase::Transcribing {
            return;
        }
        let Some((raw, terms)) = self.cleaning.take() else {
            return;
        };
        self.revision = self.revision.wrapping_add(1);
        let record = |label, kept, reply, error| CleanupRecord {
            label,
            raw: kept,
            reply,
            error,
        };
        let (text, record) = match result {
            Ok(reply) => {
                let reply = reply.unwrap_or_default();
                match cleanup::accept(&raw, &reply, &terms) {
                    Ok(clean) if clean == raw => {
                        (clean, record(cleanup::UNCHANGED, None, None, None))
                    }
                    Ok(clean) => (clean, record(cleanup::CLEANED, Some(raw), None, None)),
                    Err(r) => (raw, record(r.label(), None, Some(reply), None)),
                }
            }
            Err(e) => (raw, record(cleanup::FAILED, None, None, Some(e))),
        };
        self.finish(text, &record, ports);
    }

    /// The download finished: the model is in place and warms up, or the
    /// download failed and the window offers it again.
    fn downloaded(&mut self, result: Result<Option<String>, String>, ports: &Ports<'_>) {
        if !matches!(self.fetch, Fetch::Running(_)) {
            return;
        }
        self.revision = self.revision.wrapping_add(1);
        match result {
            Ok(_) => {
                self.fetch = Fetch::Idle;
                self.queue(vec![Job(Work::Warm)], ports);
            }
            Err(reason) => self.fetch = Fetch::Failed(reason),
        }
    }

    fn heard(
        &mut self,
        session: u64,
        index: u32,
        result: Result<Option<String>, String>,
        ports: &Ports<'_>,
    ) {
        if session != self.session {
            return;
        }
        let Some(slot) = self.slots.get_mut(&index) else {
            return;
        };
        if *slot != Slot::Pending {
            return;
        }
        *slot = match result {
            Ok(text) => Slot::Heard(text),
            Err(e) => Slot::Failed(e),
        };
        self.revision = self.revision.wrapping_add(1);
        if self.phase == Phase::Transcribing {
            self.settle(ports);
        }
    }

    /// While transcribing: once no segment is pending, fail with the first
    /// failed segment's reason, send the transcript to the cleanup model
    /// (when the setting is on, there is a model and something was heard), or
    /// finish at once.
    fn settle(&mut self, ports: &Ports<'_>) {
        if self.slots.values().any(|s| *s == Slot::Pending) {
            return;
        }
        let total = self.slots.len();
        let mut texts = Vec::new();
        for (n, slot) in self.slots.values().enumerate() {
            match slot {
                Slot::Heard(Some(text)) => texts.push(text.as_str()),
                Slot::Failed(reason) => {
                    self.error = Some(format!(
                        "Part {} of {total} could not be transcribed: {reason}. Retry to try again.",
                        n.saturating_add(1)
                    ));
                    self.phase = Phase::Failed;
                    return;
                }
                Slot::Heard(None) | Slot::Pending => {}
            }
        }
        let text = join(texts);
        if !text.is_empty() && ports.cleaner.is_some() && settings::load(ports.files).cleanup {
            let mut terms = vocabulary::terms(&store::vocabulary(ports.files).unwrap_or_default());
            terms.truncate(cleanup::MAX_TERMS);
            self.cleaning = Some((text.clone(), terms.clone()));
            let job = Job(Work::Cleanup {
                session: self.session,
                text,
                terms,
            });
            self.queue(vec![job], ports);
            return;
        }
        let record = CleanupRecord {
            label: cleanup::OFF,
            raw: None,
            reply: None,
            error: None,
        };
        self.finish(text, &record, ports);
    }

    /// Finishes the session with transcript `text`: keeps it, copies it, and
    /// archives it with the cleanup pass's `record`.
    fn finish(&mut self, text: String, record: &CleanupRecord, ports: &Ports<'_>) {
        self.text = text;
        self.phase = Phase::Finished;
        // Losing it only means a relaunch offers Retry instead of the text.
        let _kept = store::save_transcript(ports.files, &self.text);
        self.notice = Some(if self.text.is_empty() {
            NOTHING_HEARD.to_string()
        } else {
            match ports.clipboard.write(&self.text) {
                Ok(()) => COPIED.to_string(),
                Err(e) => format!("Could not copy to the clipboard: {e}. Use Copy to try again."),
            }
        });
        self.archive(ports, record);
    }

    fn bump_library(&mut self) {
        self.library_revision = self.library_revision.wrapping_add(1);
        self.revision = self.revision.wrapping_add(1);
    }

    /// With the library on, keeps the finished session (unless it heard
    /// nothing) as a new entry, removes its segments' audio, and queues the
    /// entry's encoding and, when summaries are on, its summary. When the
    /// entry cannot be written the segments stay and nothing else changes.
    fn archive(&mut self, ports: &Ports<'_>, record: &CleanupRecord) {
        let Some(lib) = ports.library else {
            return;
        };
        if self.text.is_empty() {
            return;
        }
        let title = self
            .started
            .clone()
            .unwrap_or_else(|| library::title(&lib.clock.now()));
        let Ok(archived) = library::archive(ports.files, &title, &self.text, record) else {
            return;
        };
        store::remove_segments(ports.files);
        self.bump_library();
        let mut jobs = Vec::new();
        if archived.audio {
            jobs.push(Job(Work::Encode {
                id: archived.id.clone(),
            }));
        }
        if settings::load(ports.files).summaries {
            jobs.push(Job(Work::Summary {
                id: archived.id,
                text: self.text.clone(),
            }));
        }
        self.queue(jobs, ports);
    }

    /// Copy: puts `text` (the transcript as the user edited it) on the
    /// clipboard and keeps it as the transcript.
    ///
    /// # Errors
    ///
    /// Illegal unless finished; the clipboard cannot be written.
    pub fn copy(&mut self, text: &str, ports: &Ports<'_>) -> Result<(), DictationError> {
        self.allow(Action::Copy, &[Phase::Finished])?;
        ports
            .clipboard
            .write(text)
            .map_err(DictationError::Clipboard)?;
        text.clone_into(&mut self.text);
        let _kept = store::save_transcript(ports.files, text);
        self.notice = Some(COPIED.to_string());
        Ok(())
    }
}
