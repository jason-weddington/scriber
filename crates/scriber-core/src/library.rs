//! The recording library: every finished session with a transcript, kept in
//! `library/<id>/` in the data folder.
//!
//! ```text
//! library/20261026-140309/entry.json          {"id","title","summary","summary_source","cleanup",["cleanup_error",]"duration_ms"[,"summary_model"]}
//! library/20261026-140309/transcript.txt      the transcript
//! library/20261026-140309/transcript.raw.txt  Whisper's transcript as spoken, when an accepted cleanup or the shortcut glyphs changed the text
//! library/20261026-140309/cleanup.reply.txt   the rejected reply's `transcript` field as the adapter returned it (after its `</think>` cut)
//! library/20261026-140309/context.json        the embedder's conversation turns and the context terms appended to the cleanup glossary, when it supplied any
//! library/20261026-140309/segments/segment-NNNN.wav  with keep_segments on: each part's WAV exactly as stored; a segment that cannot be kept is reported in `keep_error` and never fails the archive
//! library/20261026-140309/audio.m4a          the session's audio (audio.wav until encoded, or when encoding fails)
//! ```
//!
//! An entry is named after its session's start in local time ([`title`],
//! [`base_id`]). Its summary starts as an [`excerpt`] of the transcript and
//! is replaced by the local model's one sentence ([`clean_summary`]) when
//! that arrives. Every function that takes an id treats one that is not
//! [`valid_id`] as not in the library, so an id can never reach outside it.
//! `cleanup` is the cleanup pass's outcome label ([`crate::cleanup`]) and
//! `cleanup_error` why the model failed, when it did. `summary_model` is the
//! model name sent for the summary (`""` = the engine's default), written
//! once the summary lands.

use std::collections::BTreeSet;

use serde::Serialize;
use serde_json::{Map, Value};

use crate::audio::{decode_wav, duration_ms, encode_wav};
use crate::model::LibraryEntry;
use crate::ports::{Files, LocalTime, PortError, fail};
use crate::store;

/// The folder that holds the library.
pub const LIBRARY: &str = "library";

/// Shown in place of a summary that has not arrived: at most this many
/// characters of the transcript, then `...`.
const EXCERPT_CHARS: usize = 30;

/// An excerpt is cut at a word break only when one is at least this far in.
const EXCERPT_MIN_CUT: usize = 20;

/// A session's start as a title: `2026-10-26 14:03:09`.
#[must_use]
pub fn title(t: &LocalTime) -> String {
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        t.year, t.month, t.day, t.hour, t.minute, t.second
    )
}

/// The entry id for a title: `2026-01-05 09:03:07` becomes
/// `20260105-090307`.
#[must_use]
pub fn base_id(title: &str) -> String {
    title
        .chars()
        .filter_map(|c| match c {
            '-' | ':' => None,
            ' ' => Some('-'),
            other => Some(other),
        })
        .collect()
}

/// Whether `s` is non-empty ASCII digits, exactly `len` of them if given.
fn digits(s: &str, len: Option<usize>) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) && len.is_none_or(|n| s.len() == n)
}

/// Whether `id` names a library entry: 8 digits, `-`, 6 digits, and
/// optionally `-` and more digits (`20261026-090000-2`).
#[must_use]
pub fn valid_id(id: &str) -> bool {
    let parts: Vec<&str> = id.split('-').collect();
    match parts.as_slice() {
        [date, time] => digits(date, Some(8)) && digits(time, Some(6)),
        [date, time, n] => digits(date, Some(8)) && digits(time, Some(6)) && digits(n, None),
        _ => false,
    }
}

/// Every whitespace run as one space, trimmed.
fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The summary shown until the model's arrives: the transcript on one line,
/// whole when it is 30 characters or fewer; otherwise its first 30, cut at
/// the last space when that is at character 20 or later, without trailing
/// punctuation, and followed by `...`.
#[must_use]
pub fn excerpt(text: &str) -> String {
    let flat = collapse(text);
    if flat.chars().count() <= EXCERPT_CHARS {
        return flat;
    }
    let head: Vec<char> = flat.chars().take(EXCERPT_CHARS).collect();
    let cut = head
        .iter()
        .rposition(|c| *c == ' ')
        .filter(|i| *i >= EXCERPT_MIN_CUT)
        .unwrap_or(EXCERPT_CHARS);
    let kept: String = head.iter().take(cut).collect();
    let kept = kept.trim_end_matches(|c: char| c.is_whitespace() || ",;:-.".contains(c));
    format!("{kept}...")
}

/// The model's answer as a summary: on one line, without one pair of
/// surrounding straight or curly double quotes; `None` when nothing is
/// left.
#[must_use]
pub fn clean_summary(raw: &str) -> Option<String> {
    let flat = collapse(raw);
    let chars: Vec<char> = flat.chars().collect();
    let quoted = chars.len() >= 2
        && matches!(chars.first(), Some('"' | '\u{201c}'))
        && matches!(chars.last(), Some('"' | '\u{201d}'));
    let clean = if quoted {
        let inner: String = chars
            .iter()
            .skip(1)
            .take(chars.len().saturating_sub(2))
            .collect();
        inner.trim().to_string()
    } else {
        flat
    };
    (!clean.is_empty()).then_some(clean)
}

/// Where the summary in an entry came from.
const EXCERPT: &str = "excerpt";

/// The summary came from the local model.
pub(crate) const OLLAMA: &str = "ollama";

/// entry.json as the archive writes it, keys in this order.
#[derive(Serialize)]
struct EntryFile<'a> {
    id: &'a str,
    title: &'a str,
    summary: &'a str,
    summary_source: &'a str,
    cleanup: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    cleanup_error: Option<&'a str>,
    duration_ms: u64,
}

/// How the cleanup pass went, for a new entry: its outcome label
/// ([`crate::cleanup`]), Whisper's transcript when an accepted cleanup
/// or the shortcut glyphs changed the text, the rejected reply's `transcript`
/// field, and why the model failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CleanupRecord {
    pub(crate) label: &'static str,
    pub(crate) raw: Option<String>,
    pub(crate) reply: Option<String>,
    pub(crate) error: Option<String>,
}

/// The path of `name` in entry `id`'s folder.
#[must_use]
pub fn entry_path(id: &str, name: &str) -> String {
    format!("{LIBRARY}/{id}/{name}")
}

/// A finished session, as it was kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Archived {
    /// The entry's id.
    pub(crate) id: String,
    /// Whether `audio.wav` was written.
    pub(crate) audio: bool,
    /// The first segment that could not be kept, when `keep_segments` was on; `None` otherwise.
    pub(crate) keep_error: Option<PortError>,
}

/// The ids already in the library, with any id-like name counted.
fn taken(files: &dyn Files) -> Result<BTreeSet<String>, PortError> {
    Ok(files
        .list(LIBRARY)?
        .iter()
        .filter_map(|name| name.split('/').next())
        .map(str::to_string)
        .collect())
}

/// The embedder's conversation turns as they stood when a recording's cleanup
/// job was built, and the context terms appended to that job's glossary.
#[derive(Debug)]
pub(crate) struct RecordedContext {
    /// The turns, oldest first, verbatim.
    pub(crate) turns: Vec<String>,
    /// The terms the turns contributed and the glossary did not already hold.
    pub(crate) terms: Vec<String>,
}

impl RecordedContext {
    /// What to record for `turns`, or `None` when the embedder supplied no
    /// turn at all (so a session without a context writes no `context.json`,
    /// whatever `terms` holds).
    pub(crate) fn of(turns: &[String], terms: Vec<String>) -> Option<Self> {
        if turns.is_empty() {
            return None;
        }
        Some(Self {
            turns: turns.to_vec(),
            terms,
        })
    }
}

/// `context.json`: the embedder's turns and the context terms of one
/// recording, compact and in that key order.
#[derive(Serialize)]
struct ContextFile<'a> {
    turns: &'a [String],
    terms: &'a [String],
}

/// Keeps the finished session titled `title`, with transcript `text`, as a
/// new entry: its stored segments joined into `audio.wav`, its transcript,
/// then `transcript.raw.txt` and `cleanup.reply.txt` when `cleanup` has them,
/// then `context.json` when the embedder supplied conversation turns, and
/// last its entry.json, so an entry without one never lists. The
/// segments themselves are left for the caller to remove. With
/// `keep_segments`, each stored segment is also copied unchanged to
/// `library/<id>/segments/segment-NNNN.wav`, before entry.json; a segment that
/// cannot be kept is reported in `keep_error` and never fails the archive.
///
/// # Errors
///
/// The library cannot be listed, or a file cannot be written.
pub(crate) fn archive(
    files: &dyn Files,
    title: &str,
    text: &str,
    cleanup: &CleanupRecord,
    context: Option<&RecordedContext>,
    keep_segments: bool,
) -> Result<Archived, PortError> {
    let base = base_id(title);
    let taken = taken(files)?;
    let mut id = base.clone();
    let mut n: u32 = 1;
    while taken.contains(&id) {
        n = n.saturating_add(1);
        id = format!("{base}-{n}");
    }
    let mut all = Vec::new();
    let mut parts = Vec::new();
    for index in store::stored(files)? {
        let bytes = store::load(files, index);
        if let Some(samples) = bytes.as_ref().ok().and_then(|b| decode_wav(b).ok()) {
            all.extend(samples);
        }
        parts.push((index, bytes));
    }
    let audio = !all.is_empty();
    if audio {
        files.write(&entry_path(&id, "audio.wav"), &encode_wav(&all))?;
    }
    files.write(&entry_path(&id, "transcript.txt"), text.as_bytes())?;
    if let Some(raw) = &cleanup.raw {
        files.write(&entry_path(&id, "transcript.raw.txt"), raw.as_bytes())?;
    }
    if let Some(reply) = &cleanup.reply {
        files.write(&entry_path(&id, "cleanup.reply.txt"), reply.as_bytes())?;
    }
    if let Some(context) = context {
        let bytes = serde_json::to_vec(&ContextFile {
            turns: &context.turns,
            terms: &context.terms,
        })
        .map_err(fail)?;
        files.write(&entry_path(&id, "context.json"), &bytes)?;
    }
    let mut keep_error = None;
    if keep_segments {
        for (index, bytes) in parts {
            let kept =
                bytes.and_then(|b| files.write(&entry_path(&id, &store::segment_path(index)), &b));
            if let Err(e) = kept {
                let _first = keep_error.get_or_insert(e);
            }
        }
    }
    let entry = EntryFile {
        id: &id,
        title,
        summary: &excerpt(text),
        summary_source: EXCERPT,
        cleanup: cleanup.label,
        cleanup_error: cleanup.error.as_deref(),
        duration_ms: duration_ms(all.len()),
    };
    let json = serde_json::to_vec(&entry).map_err(fail)?;
    let partial = entry_path(&id, "entry.json.partial");
    files.write(&partial, &json)?;
    files.rename(&partial, &entry_path(&id, "entry.json"))?;
    Ok(Archived {
        id,
        audio,
        keep_error,
    })
}

/// Replaces entry `id`'s summary with `summary` from the local model,
/// keeping every other key of its entry.json.
///
/// # Errors
///
/// The entry cannot be read, is not a JSON object, or cannot be rewritten.
pub(crate) fn set_summary(
    files: &dyn Files,
    id: &str,
    summary: &str,
    model: &str,
) -> Result<(), PortError> {
    let path = entry_path(id, "entry.json");
    let mut entry: Map<String, Value> =
        serde_json::from_slice(&files.read(&path)?).map_err(fail)?;
    let _summary = entry.insert("summary".to_string(), Value::from(summary));
    let _source = entry.insert("summary_source".to_string(), Value::from(OLLAMA));
    let _model = entry.insert("summary_model".to_string(), Value::from(model));
    let json = serde_json::to_vec(&entry).map_err(fail)?;
    let partial = entry_path(id, "entry.json.partial");
    files.write(&partial, &json)?;
    files.rename(&partial, &path)
}

/// Entry `id` as listed, if its entry.json has a string title and summary.
fn read_entry(files: &dyn Files, id: &str) -> Option<LibraryEntry> {
    let bytes = files.read(&entry_path(id, "entry.json")).ok()?;
    let entry: Map<String, Value> = serde_json::from_slice(&bytes).ok()?;
    Some(LibraryEntry {
        id: id.to_string(),
        title: entry.get("title")?.as_str()?.to_string(),
        summary: entry.get("summary")?.as_str()?.to_string(),
    })
}

/// How an id sorts: its base id, then its suffix (none counts as 1).
fn order(id: &str) -> (String, u128) {
    let mut parts = id.splitn(3, '-');
    let date = parts.next().unwrap_or_default();
    let time = parts.next().unwrap_or_default();
    let n = parts.next().and_then(|n| n.parse().ok()).unwrap_or(1);
    (format!("{date}-{time}"), n)
}

/// Every entry, newest first. Folders that are not entries, and entries
/// whose entry.json is missing or invalid, are skipped; a library that
/// cannot be read lists empty.
#[must_use]
pub fn list(files: &dyn Files) -> Vec<LibraryEntry> {
    let Ok(ids) = taken(files) else {
        return Vec::new();
    };
    let mut entries: Vec<LibraryEntry> = ids
        .iter()
        .filter(|id| valid_id(id))
        .filter_map(|id| read_entry(files, id))
        .collect();
    entries.sort_by_key(|e| std::cmp::Reverse(order(&e.id)));
    entries
}

/// Entry `id`'s transcript, or `None` when it is not in the library.
#[must_use]
pub fn transcript(files: &dyn Files, id: &str) -> Option<String> {
    if !valid_id(id) {
        return None;
    }
    String::from_utf8(files.read(&entry_path(id, "transcript.txt")).ok()?).ok()
}
