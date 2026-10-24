//! What Scriber keeps in its data folder, through the [`Files`] port:
//!
//! ```text
//! vocabulary.txt                 the user's glossary (optional)
//! segments/segment-0000.wav      the current session's audio, one file per segment
//! segments/transcript.txt        the session's transcript, once it has one
//! models/ggml-large-v3-turbo.bin the speech model (read by the transcriber, not here)
//! ```

use crate::ports::{Files, PortError};

/// The folder that holds the current session's segments.
pub const SEGMENTS: &str = "segments";

/// The finished session's transcript, kept beside its audio so a relaunch
/// shows it rather than offering to transcribe it again.
pub const TRANSCRIPT: &str = "segments/transcript.txt";

/// The glossary file.
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

/// Deletes every stored segment and the transcript, and nothing else.
///
/// # Errors
///
/// The folder cannot be read or a file cannot be deleted.
pub fn clear(files: &dyn Files) -> Result<(), PortError> {
    let transcript = TRANSCRIPT.rsplit('/').next().unwrap_or(TRANSCRIPT);
    files
        .list(SEGMENTS)?
        .iter()
        .filter(|name| segment_index(name).is_some() || name.as_str() == transcript)
        .try_for_each(|name| files.remove(&format!("{SEGMENTS}/{name}")))
}

/// Keeps the session's transcript.
///
/// # Errors
///
/// The file cannot be written.
pub fn save_transcript(files: &dyn Files, text: &str) -> Result<(), PortError> {
    files.write(TRANSCRIPT, text.as_bytes())
}

/// The kept transcript, or `None` when there is none (or it is not UTF-8).
#[must_use]
pub fn transcript(files: &dyn Files) -> Option<String> {
    String::from_utf8(files.read(TRANSCRIPT).ok()?).ok()
}

/// The glossary's text, or `None` when it is missing, unreadable or not
/// UTF-8.
#[must_use]
pub fn vocabulary(files: &dyn Files) -> Option<String> {
    String::from_utf8(files.read(VOCABULARY).ok()?).ok()
}
