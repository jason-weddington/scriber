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
//! models/ggml-large-v3-turbo.bin the speech model (read by the transcriber, not here); the default, overridable in settings.json as model_folder
//! microphone-log.jsonl           which microphone each Record or Resume opened, and each segment's format (newest 200 lines)
//! learned.txt                    terms learned from the user's corrections, newest first (crate::learn); the default, overridable in settings.json as learned_vocabulary
//! <learned path>.partial         the learned list while it is being written, before the rename
//! learning-log.jsonl             each learning Copy and each Undo: what was found and added, and why (newest 200 lines)
//! model-log.jsonl            how the model's location was decided: each launch, each download outcome, each settings write (newest 200 lines)
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

/// The microphone decision log: one JSON line per Record or Resume that
/// reached the microphone (what was wanted, what opened, why) and one per
/// closed segment (its device, format, length and peak). Device names and
/// error texts only, never transcript text.
pub const MICROPHONE_LOG: &str = "microphone-log.jsonl";

/// How many lines [`MICROPHONE_LOG`] keeps, the newest.
const MICROPHONE_LOG_LINES: usize = 200;

/// The terms learned from the user's corrections ([`crate::learn`]): one
/// per line, newest first. Written at [`learned_path`]; written only by the
/// app.
pub const LEARNED: &str = "learned.txt";

/// The learning log: one JSON line per Copy that found a correction (what
/// was found and added, and the outcome) and one per Undo of learning.
pub const LEARNING_LOG: &str = "learning-log.jsonl";

/// How many lines [`LEARNING_LOG`] keeps, the newest.
pub const LEARNING_LOG_LINES: usize = 200;

/// The model-folder decision log: one JSON line per launch and each
/// download outcome (the core) and each settings write (the engine).
pub const MODEL_LOG: &str = "model-log.jsonl";

/// How many lines [`MODEL_LOG`] keeps, the newest.
pub const MODEL_LOG_LINES: usize = 200;

/// The default glossary file.
pub const VOCABULARY: &str = "vocabulary.txt";

/// The default model folder, in the data folder.
pub const MODEL_FOLDER: &str = "models";

/// The speech model's file name, inside [`MODEL_FOLDER`].
pub const MODEL_FILE: &str = "ggml-large-v3-turbo.bin";

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

/// The learned list's path: the one in the settings when there is one (an
/// absolute path), else [`LEARNED`] (`learned.txt`) in the data folder.
#[must_use]
pub fn learned_path(files: &dyn Files) -> String {
    let configured = crate::settings::load(files).learned_vocabulary;
    if configured.is_empty() {
        LEARNED.to_string()
    } else {
        configured
    }
}

/// The model's folder: the one in the settings when there is one (an
/// absolute path), else [`MODEL_FOLDER`] (`models`) in the data folder.
#[must_use]
pub fn model_folder(files: &dyn Files) -> String {
    let configured = crate::settings::load(files).model_folder;
    if configured.is_empty() {
        MODEL_FOLDER.to_string()
    } else {
        configured
    }
}

/// The speech model's path: [`MODEL`] (`models/ggml-large-v3-turbo.bin`) in
/// the data folder, or `model_folder`'s [`MODEL_FILE`] when the settings name
/// another (absolute) folder.
#[must_use]
pub fn model_path(files: &dyn Files) -> String {
    let configured = crate::settings::load(files).model_folder;
    if configured.is_empty() {
        MODEL.to_string()
    } else {
        format!("{}/{MODEL_FILE}", configured.trim_end_matches('/'))
    }
}

/// The glossary's text, read from [`vocabulary_path`], or `None` when it is
/// missing, unreadable or not UTF-8.
#[must_use]
pub fn vocabulary(files: &dyn Files) -> Option<String> {
    String::from_utf8(files.read(&vocabulary_path(files)).ok()?).ok()
}

/// Adds `line` (one JSON object, no newline) to [`MICROPHONE_LOG`], keeping
/// the newest [`MICROPHONE_LOG_LINES`] lines. Best effort: an unreadable log
/// counts as empty, and a failure to write it is ignored, because the log
/// must never stop a recording.
pub fn log_microphone(files: &dyn Files, line: &str) {
    log_line(files, MICROPHONE_LOG, MICROPHONE_LOG_LINES, line);
}

/// Adds `line` (one JSON object, no newline) to [`LEARNING_LOG`], keeping
/// the newest [`LEARNING_LOG_LINES`] lines, best effort like
/// [`log_microphone`].
pub fn log_learning(files: &dyn Files, line: &str) {
    log_line(files, LEARNING_LOG, LEARNING_LOG_LINES, line);
}

/// Adds `line` (one JSON object, no newline) to [`MODEL_LOG`], keeping the
/// newest [`MODEL_LOG_LINES`] lines, best effort like [`log_microphone`].
pub fn log_model(files: &dyn Files, line: &str) {
    log_line(files, MODEL_LOG, MODEL_LOG_LINES, line);
}

/// Appends `line` to the log at `path`, keeping its newest `keep` lines.
fn log_line(files: &dyn Files, path: &str, keep: usize, line: &str) {
    let old = files.read(path).unwrap_or_default();
    let old = String::from_utf8_lossy(&old);
    let lines: Vec<&str> = old.lines().collect();
    let drop = lines.len().saturating_sub(keep.saturating_sub(1));
    let mut text = String::new();
    for kept in lines.iter().skip(drop) {
        text.push_str(kept);
        text.push('\n');
    }
    text.push_str(line);
    text.push('\n');
    let _kept = files.write(path, text.as_bytes());
}

/// The learned terms ([`learned_path`]), in file order, read like the
/// glossary ([`crate::vocabulary::terms`]). A file that cannot be read counts
/// as empty, because the [`Files`] port cannot tell missing from unreadable;
/// `None` when it is not UTF-8, so it is never overwritten.
#[must_use]
pub fn learned(files: &dyn Files) -> Option<Vec<String>> {
    let Ok(bytes) = files.read(&learned_path(files)) else {
        return Some(Vec::new());
    };
    String::from_utf8(bytes)
        .ok()
        .map(|text| crate::vocabulary::terms(&text))
}

/// Writes the learned terms to [`learned_path`], one per line, atomically
/// like [`save_transcript`].
///
/// # Errors
///
/// The file cannot be written or renamed into place.
pub fn save_learned(files: &dyn Files, terms: &[String]) -> Result<(), PortError> {
    let path = learned_path(files);
    let partial = format!("{path}.partial");
    let mut text = String::new();
    for term in terms {
        text.push_str(term);
        text.push('\n');
    }
    files.write(&partial, text.as_bytes())?;
    files.rename(&partial, &path)
}
