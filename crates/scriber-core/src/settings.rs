//! The user's settings, kept in `settings.json` in the data folder.
//!
//! On disk the file is a JSON object with the `snake_case` keys `summaries`,
//! `library_open`, `vocabulary`, `cleanup`, `summary_model` and
//! `cleanup_model`. Reading is lenient: a missing, unreadable or invalid file
//! means the defaults (`summaries` and `library_open` true, `vocabulary`
//! empty, `cleanup` false, both models `""`, meaning the engine's default
//! model),
//! and each key is taken on its own, so a missing key or one of another type
//! takes its default and unknown keys are ignored. `vocabulary` is kept only
//! when it is an absolute path (it starts with `/`); anything else means `""`,
//! the default glossary. `summary_model` and `cleanup_model` are kept exactly
//! as stored when they are strings, else `""`. Saving writes the file beside itself and renames it
//! into place.

use serde::Serialize;
use serde_json::{Map, Value};

use crate::model::Settings;
use crate::ports::{Files, PortError, fail};

/// The settings file.
pub const SETTINGS: &str = "settings.json";

/// Where the settings are written before they are renamed into place.
const SETTINGS_PARTIAL: &str = "settings.json.partial";

/// The settings as they are written to disk.
#[derive(Serialize)]
struct Disk {
    summaries: bool,
    library_open: bool,
    vocabulary: String,
    cleanup: bool,
    summary_model: String,
    cleanup_model: String,
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
    };
    let bytes = serde_json::to_vec(&disk).map_err(fail)?;
    files.write(SETTINGS_PARTIAL, &bytes)?;
    files.rename(SETTINGS_PARTIAL, SETTINGS)
}
