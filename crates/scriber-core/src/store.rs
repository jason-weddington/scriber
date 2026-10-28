//! What Scriber keeps in its data folder, through the [`Files`] port:
//!
//! ```text
//! vocabulary.txt                 the user's glossary (optional; the default, overridable in settings.json)
//! settings.json                  the user's settings (crate::settings)
//! segments/segment-0000.wav      the current session's audio, one file per segment
//! segments/transcript.txt        the session's transcript, once it has one
//! segments/started.txt           the session's start, `2026-10-26 14:03:09`, with the library on
//! library/<id>/entry.json        a recording in the library (crate::library): its title and summary
//! library/<id>/transcript.txt    its transcript
//! library/<id>/audio.m4a         its audio (audio.wav until encoded, or when encoding fails)
//! models/ggml-large-v3-turbo.bin the speech model (read by the transcriber, not here)
//! ```

use crate::ports::{Files, PortError};

/// The folder that holds the current session's segments.
pub const SEGMENTS: &str = "segments";

/// The finished session's transcript, kept beside its audio so a relaunch
/// shows it rather than offering to transcribe it again.
pub const TRANSCRIPT: &str = "segments/transcript.txt";

/// When the session started, as a library title (`2026-10-26 14:03:09`),
/// written at its first Record when the library is on.
pub const STARTED: &str = "segments/started.txt";

/// Where the transcript is written before it is renamed into place.
const TRANSCRIPT_PARTIAL: &str = "segments/transcript.txt.partial";

/// The default glossary file.
pub const VOCABULARY: &str = "vocabulary.txt";

/// The speech model: ggml `large-v3-turbo` from the `ggerganov/whisper.cpp`
/// Hugging Face repository (`scripts/fetch-model.sh` downloads it).
pub const MODEL: &str = "models/ggml-large-v3-turbo.bin";

/// The path of segment `index`: `segments/segment-0007.wav`.
#[must_use]
pub fn segment_path(index: u32) -> String {
    format!("{SEGMENTS}/segment-{index:04}.wav")
}

/// The segment index a file name in [`SEGMENTS`] stands for, or `None` for
/// any other file: `segment-` then decimal digits only (no sign) then `.wav`.
#[must_use]
pub fn segment_index(name: &str) -> Option<u32> {
    let digits = name.strip_prefix("segment-")?.strip_suffix(".wav")?;
    if digits.bytes().all(|b| b.is_ascii_digit()) {
        digits.parse().ok()
    } else {
        None
    }
}

/// Writes segment `index`.
///
/// # Errors
///
/// The file cannot be written.
pub fn save(files: &dyn Files, index: u32, wav: &[u8]) -> Result<(), PortError> {
    files.write(&segment_path(index), wav)
}

/// Reads segment `index` back.
///
/// # Errors
///
/// The file is missing or unreadable.
pub fn load(files: &dyn Files, index: u32) -> Result<Vec<u8>, PortError> {
    files.read(&segment_path(index))
}

/// Every stored segment's index, ascending. Other files are ignored, and a
/// missing folder holds none.
///
/// # Errors
///
/// The folder exists but cannot be read.
pub fn stored(files: &dyn Files) -> Result<Vec<u32>, PortError> {
    let mut indices: Vec<u32> = files
        .list(SEGMENTS)?
        .iter()
        .filter_map(|name| segment_index(name))
        .collect();
    indices.sort_unstable();
    Ok(indices)
}

/// The file name of a path in [`SEGMENTS`].
fn name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Deletes every stored segment, the transcript and the session's start,
/// and nothing else.
///
/// # Errors
///
/// The folder cannot be read or a file cannot be deleted.
pub fn clear(files: &dyn Files) -> Result<(), PortError> {
    let session = [name(TRANSCRIPT), name(STARTED)];
    files
        .list(SEGMENTS)?
        .iter()
        .filter(|n| segment_index(n).is_some() || session.contains(&n.as_str()))
        .try_for_each(|n| files.remove(&format!("{SEGMENTS}/{n}")))
}

/// Deletes every stored segment's audio, keeping the transcript and the
/// session's start: what is left once a session is in the library. A file
/// that cannot be deleted, or a folder that cannot be read, is left.
pub fn remove_segments(files: &dyn Files) {
    for index in stored(files).unwrap_or_default() {
        let _removed = files.remove(&segment_path(index));
    }
}

/// Keeps the session's transcript, atomically: it is written beside
/// [`TRANSCRIPT`] and then renamed over it, so a process that exits mid-save
/// leaves either the whole transcript or none, never a truncated one.
///
/// # Errors
///
/// The file cannot be written or renamed into place.
pub fn save_transcript(files: &dyn Files, text: &str) -> Result<(), PortError> {
    files.write(TRANSCRIPT_PARTIAL, text.as_bytes())?;
    files.rename(TRANSCRIPT_PARTIAL, TRANSCRIPT)
}

/// The kept transcript, or `None` when there is none (or it is not UTF-8).
#[must_use]
pub fn transcript(files: &dyn Files) -> Option<String> {
    String::from_utf8(files.read(TRANSCRIPT).ok()?).ok()
}

/// The glossary file's path: the one in the settings when there is one (an
/// absolute path), else [`VOCABULARY`] in the data folder.
#[must_use]
pub fn vocabulary_path(files: &dyn Files) -> String {
    let configured = crate::settings::load(files).vocabulary;
    if configured.is_empty() {
        VOCABULARY.to_string()
    } else {
        configured
    }
}

/// The glossary's text, read from [`vocabulary_path`], or `None` when it is
/// missing, unreadable or not UTF-8.
#[must_use]
pub fn vocabulary(files: &dyn Files) -> Option<String> {
    String::from_utf8(files.read(&vocabulary_path(files)).ok()?).ok()
}
