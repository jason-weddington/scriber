//! The user's settings, kept in `settings.json` in the data folder.
//!
//! On disk the file is a JSON object with the `snake_case` keys `summaries`,
//! `library_open`, `vocabulary`, `cleanup`, `summary_model`,
//! `cleanup_model`, `library_width`, `input_device`, `silence_split_ms`,
//! `keep_segments` and `learned_vocabulary`. Reading is lenient: a missing, unreadable or invalid file
//! means the defaults (`summaries` and `library_open` true, `vocabulary`
//! empty, `cleanup` false, both models `""`, meaning the engine's default
//! model, `library_width` 220, `input_device` `""`, meaning the Mac's default
//! input device, `silence_split_ms` 0, meaning no split, `keep_segments` false,
//! `learned_vocabulary` empty, meaning `learned.txt` in the data folder),
//! and each key is taken on its own, so a missing key or one of another type
//! takes its default and unknown keys are ignored. `vocabulary` is kept only
//! when it is an absolute path (it starts with `/`); anything else means `""`,
//! the default glossary. `summary_model` and `cleanup_model` are kept exactly
//! as stored when they are strings, else `""`. `library_width` is a
//! non-negative integer clamped to 110..=440 on read; anything else (a
//! negative number, a fraction, another type) means 220. `input_device` is
//! kept exactly as stored when it is a string, else `""`.
//! `silence_split_ms` is kept when it is a non-negative integer that is 0 or
//! in [`SILENCE_SPLIT_MIN_MS`]..=[`SILENCE_SPLIT_MAX_MS`]; anything else (out
//! of range, negative, a fraction, another type) means 0. `keep_segments` is true only for the JSON boolean true; anything else means false. `learned_vocabulary` is kept only when it is an absolute path (it starts with `/`); anything else means `""`, the default learned list in the data folder. Saving writes the file beside itself and renames it
//! into place.

use serde::Serialize;
use serde_json::{Map, Value};

use crate::model::Settings;
use crate::ports::{Files, PortError, fail};

/// The library drawer's default width in pixels.
pub const LIBRARY_WIDTH_DEFAULT: u64 = 220;

/// The narrowest the drawer may be, in pixels.
pub const LIBRARY_WIDTH_MIN: u64 = 110;

/// The widest the drawer may be, in pixels.
pub const LIBRARY_WIDTH_MAX: u64 = 440;

/// The shortest silence split, in milliseconds, kept on read.
pub const SILENCE_SPLIT_MIN_MS: u64 = 1_000;

/// The longest silence split, in milliseconds, kept on read.
pub const SILENCE_SPLIT_MAX_MS: u64 = 10_000;

/// The settings file.
pub const SETTINGS: &str = "settings.json";

/// Where the settings are written before they are renamed into place.
const SETTINGS_PARTIAL: &str = "settings.json.partial";

/// The settings as they are written to disk.
#[derive(Serialize)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "a flat bag of independent settings, mirrored by the JSON shape"
)]
struct Disk {
    summaries: bool,
    library_open: bool,
    vocabulary: String,
    cleanup: bool,
    summary_model: String,
    cleanup_model: String,
    library_width: u64,
    input_device: String,
    silence_split_ms: u64,
    keep_segments: bool,
    learned_vocabulary: String,
}

/// The saved settings, or the defaults where there are none.
#[must_use]
pub fn load(files: &dyn Files) -> Settings {
    let object = files
        .read(SETTINGS)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Map<String, Value>>(&bytes).ok())
        .unwrap_or_default();
    let flag =
        |key: &str, default: bool| object.get(key).and_then(Value::as_bool).unwrap_or(default);
    let text = |key: &str| {
        object
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    Settings {
        summaries: flag("summaries", true),
        library_open: flag("library_open", true),
        vocabulary: object
            .get("vocabulary")
            .and_then(Value::as_str)
            .filter(|path| path.starts_with('/'))
            .unwrap_or_default()
            .to_string(),
        cleanup: flag("cleanup", false),
        summary_model: text("summary_model"),
        cleanup_model: text("cleanup_model"),
        library_width: object
            .get("library_width")
            .and_then(Value::as_u64)
            .map_or(LIBRARY_WIDTH_DEFAULT, |w| {
                w.clamp(LIBRARY_WIDTH_MIN, LIBRARY_WIDTH_MAX)
            }),
        input_device: text("input_device"),
        silence_split_ms: object
            .get("silence_split_ms")
            .and_then(Value::as_u64)
            .filter(|ms| *ms == 0 || (SILENCE_SPLIT_MIN_MS..=SILENCE_SPLIT_MAX_MS).contains(ms))
            .unwrap_or(0),
        keep_segments: flag("keep_segments", false),
        learned_vocabulary: object
            .get("learned_vocabulary")
            .and_then(Value::as_str)
            .filter(|path| path.starts_with('/'))
            .unwrap_or_default()
            .to_string(),
    }
}

/// Keeps `s`, atomically.
///
/// # Errors
///
/// The file cannot be written or renamed into place.
pub fn save(files: &dyn Files, s: &Settings) -> Result<(), PortError> {
    let disk = Disk {
        summaries: s.summaries,
        library_open: s.library_open,
        vocabulary: s.vocabulary.clone(),
        cleanup: s.cleanup,
        summary_model: s.summary_model.clone(),
        cleanup_model: s.cleanup_model.clone(),
        library_width: s.library_width,
        input_device: s.input_device.clone(),
        silence_split_ms: s.silence_split_ms,
        keep_segments: s.keep_segments,
        learned_vocabulary: s.learned_vocabulary.clone(),
    };
    let bytes = serde_json::to_vec(&disk).map_err(fail)?;
    files.write(SETTINGS_PARTIAL, &bytes)?;
    files.rename(SETTINGS_PARTIAL, SETTINGS)
}
