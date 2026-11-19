//! Priority-ordered, process-local terminology supplied by an embedder.
use serde::{Deserialize, Serialize};

/// The most app terms retained.
pub const MAX_TERMS: usize = 50;
/// The longest written or spoken form, in characters.
pub const MAX_CHARS: usize = 64;
/// The app terms' share of the Whisper prompt, in characters.
pub const PROMPT_SHARE: usize = 200;

/// A written term and, optionally, how the speaker says it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppTerm {
    /// The spelling to write.
    pub term: String,
    /// The spoken form, when different from the spelling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spoken: Option<String>,
}

fn clean(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .to_string()
}

/// Cleans forms, keeps the first case-insensitive spelling and limits the list.
#[must_use]
pub fn normalize(terms: Vec<AppTerm>) -> Vec<AppTerm> {
    let mut kept: Vec<AppTerm> = Vec::new();
    for app in terms {
        let term = clean(&app.term);
        if term.is_empty()
            || term.chars().count() > MAX_CHARS
            || kept
                .iter()
                .any(|a| a.term.to_lowercase() == term.to_lowercase())
        {
            continue;
        }
        let spoken = app.spoken.map(|s| clean(&s)).filter(|s| {
            !s.is_empty()
                && s.chars().count() <= MAX_CHARS
                && !s.contains(['(', ')'])
                && s.to_lowercase() != term.to_lowercase()
        });
        kept.push(AppTerm { term, spoken });
        if kept.len() == MAX_TERMS {
            break;
        }
    }
    kept
}
