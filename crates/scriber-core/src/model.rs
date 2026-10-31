//! The values the core hands to the app. Each one crosses the bridge as JSON,
//! so its serde shape is part of the contract in `docs/design.md`: field
//! names are camelCase, and unknown fields are refused rather than dropped.
//! `tests/serde_shapes.rs` pins every shape.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Where a dictation session is. `docs/design.md` has the transition table.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Phase {
    /// The app has just started and no session exists.
    #[default]
    Idle,
    /// A session exists and nothing has been recorded yet.
    Ready,
    /// The microphone is capturing a segment.
    Recording,
    /// Between segments; earlier segments may still be transcribing.
    Paused,
    /// Done was clicked; waiting for the last segments' text.
    Transcribing,
    /// The transcript is ready.
    Finished,
    /// A segment could not be transcribed; Retry re-reads its audio.
    Failed,
}

impl fmt::Display for Phase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Idle => "idle",
            Self::Ready => "ready",
            Self::Recording => "recording",
            Self::Paused => "paused",
            Self::Transcribing => "transcribing",
            Self::Finished => "finished",
            Self::Failed => "failed",
        })
    }
}

/// What the user asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Action {
    /// Start a new, empty session.
    New,
    /// Start recording the first segment.
    Record,
    /// Close the current segment.
    Pause,
    /// Start recording another segment.
    Resume,
    /// Close the session and wait for its transcript.
    Done,
    /// Throw the session away.
    Discard,
    /// Transcribe the failed segments again from their saved audio.
    Retry,
    /// Copy the (possibly edited) transcript again.
    Copy,
    /// Download the speech model again, after a download failed.
    DownloadModel,
    /// Forget the terms the last Copy learned, restoring the learned list
    /// as it was before it.
    UndoLearn,
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::New => "start a new session",
            Self::Record => "record",
            Self::Pause => "pause",
            Self::Resume => "resume",
            Self::Done => "finish",
            Self::Discard => "discard",
            Self::Retry => "retry",
            Self::Copy => "copy",
            Self::DownloadModel => "download the speech model",
            Self::UndoLearn => "undo learning",
        })
    }
}

/// Everything the window shows about the session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Snapshot {
    /// Grows with every change. Commands and events can arrive out of order,
    /// so the window keeps the snapshot with the highest revision.
    pub revision: u64,
    /// Where the session is.
    pub phase: Phase,
    /// Milliseconds of audio in the session's closed segments. The window
    /// adds the running segment's time itself while recording.
    pub recorded_ms: u64,
    /// Segments recorded in this session.
    pub segments: u32,
    /// Of those, how many are still being transcribed.
    pub pending: u32,
    /// The transcript once finished; empty before.
    pub text: String,
    /// Why the session failed, when the phase is `failed`.
    pub error: Option<String>,
    /// One sentence about the last action (copied, the microphone stopped).
    pub notice: Option<String>,
    /// Set when the speech model is missing and not downloading: why, as
    /// one sentence; the window offers [`Action::DownloadModel`].
    pub model_error: Option<String>,
    /// The speech model's download, while it runs.
    pub download: Option<Download>,
    /// Grows whenever the recording library gains an entry or an entry's
    /// summary is replaced, so the window knows to list it again.
    pub library_revision: u64,
    /// The input device actually opened for this session's latest segment,
    /// by name; `None` before the session's first Record.
    pub microphone: Option<String>,
    /// The terms the last Copy learned from the user's corrections
    /// ([`crate::learn`]), until Undo, New, Discard or relaunch; empty when
    /// nothing was learned.
    pub learned: Vec<String>,
}

/// How far the speech model's download has got.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Download {
    /// Bytes received so far.
    pub received_bytes: u64,
    /// The model's size in bytes.
    pub total_bytes: u64,
}

/// One recording in the library, as the window lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LibraryEntry {
    /// The entry's folder name: its session's start, `20261026-140309`,
    /// with `-2`, `-3` and so on after a collision.
    pub id: String,
    /// Its session's start in local time: `2026-10-26 14:03:09`.
    pub title: String,
    /// One sentence: the local model's summary, or an excerpt of the
    /// transcript.
    pub summary: String,
}

/// The user's settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Settings {
    /// Whether new library entries are summarized by the local model.
    pub summaries: bool,
    /// Whether the window shows the library drawer.
    pub library_open: bool,
    /// The glossary file's absolute path, or `""` for the default
    /// (`vocabulary.txt` in the data folder). Over the bridge the engine
    /// always answers an absolute path.
    pub vocabulary: String,
    /// Whether each finished transcript is cleaned up by the local model
    /// ([`crate::cleanup`]).
    pub cleanup: bool,
    /// The Ollama model for summaries, or `""` for the engine's default.
    /// Over the bridge the engine always answers a model name.
    pub summary_model: String,
    /// The Ollama model for the cleanup pass, or `""` for the engine's
    /// default. Over the bridge the engine always answers a model name.
    pub cleanup_model: String,
    /// The library drawer's width in pixels, 110 to 440 (default 220). Read
    /// leniently from disk; see [`crate::settings`].
    pub library_width: u64,
    /// The input device to record from, by name, or `""` for the Mac's
    /// default input device. Stored and answered verbatim.
    pub input_device: String,
}

/// The input devices the microphone menu offers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InputDevices {
    /// The system's default input device, by name; `None` when there is
    /// none.
    pub default_device: Option<String>,
    /// Every other input device's name, sorted case-insensitively.
    pub others: Vec<String>,
}

impl InputDevices {
    /// The default device and every device's `names`: an empty default is
    /// none; `others` leaves out empty names and the default's, is sorted
    /// case-insensitively (ties by byte order) and holds each name once.
    #[must_use]
    pub fn new(default_device: Option<String>, names: impl IntoIterator<Item = String>) -> Self {
        let default_device = default_device.filter(|name| !name.is_empty());
        let mut others: Vec<String> = names
            .into_iter()
            .filter(|name| !name.is_empty() && Some(name) != default_device.as_ref())
            .collect();
        others.sort_by(|a, b| {
            a.to_lowercase()
                .cmp(&b.to_lowercase())
                .then_with(|| a.cmp(b))
        });
        others.dedup();
        Self {
            default_device,
            others,
        }
    }
}
