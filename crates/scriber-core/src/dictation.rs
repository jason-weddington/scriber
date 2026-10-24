//! The dictation session: a state machine over [`Phase`].
//!
//! ```text
//! Idle --new--> Ready --record--> Recording --pause--> Paused --resume--> Recording
//! Recording | Paused --done--> Transcribing --(all text in)--> Finished | Failed
//! Failed --retry--> Transcribing
//! Idle | Finished | Failed --new--> Ready (an empty session)
//! anything but Idle and Transcribing --discard--> Ready (an empty session)
//! ```
//!
//! Each pause closes a segment. The caller transcribes it off its own
//! thread with [`Job::run`] and hands the [`Outcome`] back to
//! [`Dictation::complete`], so segments transcribe while the user thinks and
//! Done only waits for the last one. Outcomes may arrive in any order; the
//! transcript joins segments in recording order. An illegal action returns
//! [`DictationError::Illegal`] and changes nothing.

use std::collections::BTreeMap;
use std::sync::Mutex;

use crate::audio::{decode_wav, duration_ms, encode_wav, to_mono_16k};
use crate::download::{self, Source};
use crate::model::{Action, Download, Phase, Snapshot};
use crate::ports::{PortError, Ports};
use crate::store;
use crate::sync::lock;
use crate::transcript::{is_silent, join, keep};
use crate::vocabulary::prompt;

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

/// Shown when the transcript was copied.
pub const COPIED: &str = "Copied to the clipboard.";

/// Shown when every segment was silent.
pub const NOTHING_HEARD: &str = "No speech was heard. If you spoke, check that Scriber may use the microphone in System Settings, Privacy & Security, Microphone.";

/// Work for the worker: one segment to transcribe, made by Pause, Done or
/// Retry; the speech model's warm-up, queued at launch and after a
/// download; or the model's download, queued at launch when it is missing
/// and by [`Action::DownloadModel`].
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
}

/// Which job an outcome is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mark {
    Warm,
    Download,
    Segment { session: u64, index: u32 },
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
        }
    }

    /// Does the work. A silent segment is not sent to the transcriber, and a
    /// hallucination is dropped (see [`crate::transcript`]). A download
    /// calls `progress` with the bytes received as each whole percent
    /// arrives (see [`download::fetch`]). The warm-up's outcome changes
    /// nothing: a model that failed to load is loaded again by the first
    /// segment, which reports the failure.
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
            } => ports
                .transcriber
                .transcribe(samples, prompt.as_deref())
                .map(|heard| keep(samples.len(), &heard))
                .map_err(|e| e.to_string()),
        };
        Outcome {
            mark: self.mark(),
            result,
        }
    }
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
    /// What the download fetches: the model, except in tests; `None` when
    /// the model is a local file ([`Self::launch_local`]).
    source: Option<Source>,
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
        let mut this = Self::default();
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
            this.prompt = store::vocabulary(ports.files).as_deref().and_then(prompt);
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
        self.prompt = store::vocabulary(ports.files).as_deref().and_then(prompt);
        self.text.clear();
        self.error = None;
        self.notice = None;
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

    /// Starts the microphone; shared by Record and Resume.
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
        ports.recorder.start().map_err(DictationError::Recorder)?;
        self.notice = None;
        self.phase = Phase::Recording;
        Ok(())
    }

    /// Record: starts the first segment.
    ///
    /// # Errors
    ///
    /// Illegal unless ready; the model is missing; the microphone fails.
    pub fn record(&mut self, ports: &Ports<'_>) -> Result<(), DictationError> {
        self.start(Action::Record, Phase::Ready, ports)
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
    /// that is not pending, or from the warm-up, is ignored.
    pub fn complete(&mut self, outcome: Outcome, ports: &Ports<'_>) -> Snapshot {
        self.record_outcome(outcome, ports);
        self.snapshot(ports)
    }

    fn record_outcome(&mut self, outcome: Outcome, ports: &Ports<'_>) {
        match outcome.mark {
            Mark::Warm => {}
            Mark::Download => self.downloaded(outcome.result, ports),
            Mark::Segment { session, index } => self.heard(session, index, outcome.result, ports),
        }
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

    /// While transcribing: once no segment is pending, finish (and copy the
    /// transcript) or fail with the first failed segment's reason.
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
        self.text = join(texts);
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
