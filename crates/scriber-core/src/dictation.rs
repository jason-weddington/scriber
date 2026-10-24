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

use crate::audio::{decode_wav, duration_ms, encode_wav, to_mono_16k};
use crate::model::{Action, Phase, Snapshot};
use crate::ports::{PortError, Ports, Transcriber};
use crate::store;
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
    /// The speech model is not installed; the text says how to install it.
    #[error("{0}")]
    ModelMissing(String),
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

/// The sentence shown when the speech model is missing.
#[must_use]
pub fn model_missing(path: &str) -> String {
    format!("Model missing: run scripts/fetch-model.sh to download it to {path}.")
}

/// Shown when the transcript was copied.
pub const COPIED: &str = "Copied to the clipboard.";

/// Shown when every segment was silent.
pub const NOTHING_HEARD: &str = "No speech was heard. If you spoke, check that Scriber may use the microphone in System Settings, Privacy & Security, Microphone.";

/// One segment to transcribe, made by Pause, Done or Retry.
#[derive(Debug, Clone, PartialEq)]
pub struct Job {
    session: u64,
    index: u32,
    samples: Vec<f32>,
    prompt: Option<String>,
}

impl Job {
    /// Transcribes the segment. A silent segment is not sent to the
    /// transcriber, and a hallucination is dropped (see [`crate::transcript`]).
    #[must_use]
    pub fn run(&self, transcriber: &dyn Transcriber) -> Outcome {
        let result = if is_silent(&self.samples) {
            Ok(None)
        } else {
            transcriber
                .transcribe(&self.samples, self.prompt.as_deref())
                .map(|heard| keep(self.samples.len(), &heard))
                .map_err(|e| e.to_string())
        };
        Outcome {
            session: self.session,
            index: self.index,
            result,
        }
    }
}

/// A segment's transcription result, for [`Dictation::complete`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    session: u64,
    index: u32,
    result: Result<Option<String>, String>,
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
/// with no session.
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
        }?;
        self.revision = self.revision.wrapping_add(1);
        for job in jobs {
            let (session, index) = (job.session, job.index);
            if let Err(e) = ports.queue.send(job) {
                let result = Err(format!("it could not be queued: {e}"));
                self.record_outcome(
                    Outcome {
                        session,
                        index,
                        result,
                    },
                    ports,
                );
            }
        }
        Ok(self.snapshot(ports))
    }

    /// The state at launch: finished with the last session's kept
    /// transcript; failed with the segments of a session that never finished
    /// (a crash, a quit mid-transcription) waiting for Retry; or idle.
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
            model_error: ports
                .transcriber
                .missing_model()
                .as_deref()
                .map(model_missing),
        }
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
        if let Some(path) = ports.transcriber.missing_model() {
            return Err(DictationError::ModelMissing(model_missing(&path)));
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
        Some(Job {
            session: self.session,
            index,
            samples,
            prompt: self.prompt.clone(),
        })
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
                    jobs.push(Job {
                        session: self.session,
                        index: *index,
                        samples,
                        prompt: self.prompt.clone(),
                    });
                }
                Err(e) => *slot = Slot::Failed(format!("its audio could not be read: {e}")),
            }
        }
        self.phase = Phase::Transcribing;
        self.settle(ports);
        Ok(jobs)
    }

    /// Records a segment's result and answers with the new snapshot. An
    /// outcome from a session that has since been discarded, or for a
    /// segment that is not pending, is ignored.
    pub fn complete(&mut self, outcome: Outcome, ports: &Ports<'_>) -> Snapshot {
        self.record_outcome(outcome, ports);
        self.snapshot(ports)
    }

    fn record_outcome(&mut self, outcome: Outcome, ports: &Ports<'_>) {
        if outcome.session != self.session {
            return;
        }
        let Some(slot) = self.slots.get_mut(&outcome.index) else {
            return;
        };
        if *slot != Slot::Pending {
            return;
        }
        *slot = match outcome.result {
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
