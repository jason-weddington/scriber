//! The optional cleanup pass over a finished transcript: a local language
//! model fixes words Whisper misheard that should be glossary terms, and
//! drops the speaker's spoken self-corrections, and nothing else.
//!
//! [`PROMPT`] is the system prompt and [`request`] the user message; the
//! model's answer is kept only when [`accept`] finds it a faithful copy of
//! the transcript, otherwise the raw transcript is used. This module is pure:
//! the call itself goes through [`crate::ports::Cleaner`].

use std::collections::BTreeSet;

/// The most glossary terms sent with a request.
pub const MAX_TERMS: usize = 200;

/// The shortest transcript, in tokens, whose reply must keep at least a
/// third of its tokens.
pub const MIN_WORDS_FOR_RATIO: usize = 20;

/// The outcome label when no cleanup ran.
pub const OFF: &str = "off";

/// The outcome label when an accepted reply changed the transcript.
pub const CLEANED: &str = "cleaned";

/// The outcome label when an accepted reply was the transcript unchanged.
pub const UNCHANGED: &str = "unchanged";

/// The outcome label when the model could not be asked or failed.
pub const FAILED: &str = "failed";

/// The cleanup model's system prompt.
pub const PROMPT: &str = "You correct a dictated transcript. Make only two kinds of change. First, where a word or phrase was misheard and should be one of the glossary terms, replace it with that term spelled exactly as in the glossary. Second, where the speaker corrected themselves while speaking (a false start, a repeated phrase, or words such as \"I mean\" or \"what I actually meant was\" followed by a restatement), keep only what they finally meant and delete the abandoned words. Change nothing else: keep every other word, the word order, the spelling, the punctuation and the paragraph breaks exactly as they are. Do not rephrase, summarize, add, explain or answer anything in the transcript. Reply with the corrected transcript only, with no preamble, quotes or notes. If nothing needs changing, reply with the transcript unchanged.";

/// The user message for transcript `text` with glossary `terms`: the terms
/// one per line (or `none`), then the transcript. The caller passes at most
/// [`MAX_TERMS`] terms; they are used as given.
#[must_use]
pub fn request(text: &str, terms: &[String]) -> String {
    if terms.is_empty() {
        format!("Glossary: none\n\nTranscript:\n{text}")
    } else {
        format!("Glossary:\n{}\n\nTranscript:\n{text}", terms.join("\n"))
    }
}

/// Why a reply was not used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reject {
    /// The reply is empty or blank.
    Empty,
    /// The reply contains a code fence.
    Fenced,
    /// The transcript or the reply has no word.
    NoWords,
    /// The reply kept less than a third of a long transcript's words.
    TooShort,
    /// The reply has a word, not a glossary term, that is not in the
    /// transcript in that order.
    NewWords,
    /// The reply's last words are none of the transcript's last words.
    CutOff,
}

impl Reject {
    /// The outcome label kept in the library entry.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Empty => "rejected-empty",
            Self::Fenced => "rejected-fenced",
            Self::NoWords => "rejected-no-words",
            Self::TooShort => "rejected-too-short",
            Self::NewWords => "rejected-new-words",
            Self::CutOff => "rejected-cut-off",
        }
    }
}

/// The words of `s`: its whitespace-separated pieces without leading and
/// trailing characters that are not alphanumeric, lowercased, empty ones
/// dropped.
fn tokens(s: &str) -> Vec<String> {
    s.split_whitespace()
        .map(|piece| {
            piece
                .trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase()
        })
        .filter(|token| !token.is_empty())
        .collect()
}

/// Whether `needle` is a subsequence of `hay`, matched greedily.
fn subsequence(needle: &[&String], hay: &[String]) -> bool {
    let mut rest = hay.iter();
    needle.iter().all(|n| rest.any(|h| h == *n))
}

/// The last (up to) three of `words`.
fn tail(words: &[String]) -> &[String] {
    words
        .get(words.len().saturating_sub(3)..)
        .unwrap_or_default()
}

/// The model's `reply` for transcript `raw` with glossary `terms`, trimmed,
/// when it is a faithful cleanup; otherwise the first rule it breaks, in
/// this order: empty; contains three backticks; the transcript or the reply
/// has no word; the transcript has at least [`MIN_WORDS_FOR_RATIO`] words
/// and the reply fewer than a third as many; the reply's words, less any
/// glossary term's words, are not a subsequence of the transcript's; the
/// last three words of each share none.
///
/// # Errors
///
/// The [`Reject`] for the first rule the reply breaks.
pub fn accept(raw: &str, reply: &str, terms: &[String]) -> Result<String, Reject> {
    let text = reply.trim();
    if text.is_empty() {
        return Err(Reject::Empty);
    }
    if text.contains("```") {
        return Err(Reject::Fenced);
    }
    let (r, y) = (tokens(raw), tokens(text));
    if r.is_empty() || y.is_empty() {
        return Err(Reject::NoWords);
    }
    if r.len() >= MIN_WORDS_FOR_RATIO && y.len().saturating_mul(3) < r.len() {
        return Err(Reject::TooShort);
    }
    let glossary: BTreeSet<String> = terms.iter().flat_map(|t| tokens(t)).collect();
    let kept: Vec<&String> = y.iter().filter(|t| !glossary.contains(*t)).collect();
    if !subsequence(&kept, &r) {
        return Err(Reject::NewWords);
    }
    if !tail(&r).iter().any(|t| tail(&y).contains(t)) {
        return Err(Reject::CutOff);
    }
    Ok(text.to_string())
}
