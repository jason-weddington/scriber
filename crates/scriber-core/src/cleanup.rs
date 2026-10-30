//! The optional cleanup pass over a finished transcript: a local language
//! model fixes words Whisper misheard that should be glossary terms,
//! repairs the speaker's spoken self-corrections into what they meant, and
//! writes a.m./p.m. clock times with a colon (9:43 a.m.), and nothing else.
//!
//! [`PROMPT`] is the system prompt and [`request`] the user message; the
//! model's answer is kept only when [`accept`] finds it close enough to
//! the transcript, otherwise the raw transcript is used. This module is pure:
//! the call itself goes through [`crate::ports::Cleaner`].

use std::collections::BTreeSet;

/// The most glossary terms sent with a request.
pub const MAX_TERMS: usize = 200;

/// The shortest transcript, in tokens, whose reply must keep at least a
/// third of its tokens.
pub const MIN_WORDS_FOR_RATIO: usize = 20;

/// The most new words any reply may add, however short the transcript.
pub const NEW_WORDS_MIN: usize = 3;

/// One more new word is allowed for every this many transcript words.
pub const NEW_WORDS_PER: usize = 20;

/// The outcome label when no cleanup ran.
pub const OFF: &str = "off";

/// The outcome label when an accepted reply changed the transcript.
pub const CLEANED: &str = "cleaned";

/// The outcome label when an accepted reply was the transcript unchanged.
pub const UNCHANGED: &str = "unchanged";

/// The outcome label when the model could not be asked or failed.
pub const FAILED: &str = "failed";

/// The cleanup model's system prompt.
pub const PROMPT: &str = "You correct a dictated transcript. Make only three kinds of change. First, where a word or phrase was misheard and should be one of the glossary terms, replace it with that term spelled exactly as in the glossary. Second, where the speaker corrected themselves while speaking (a false start, a repeated phrase, or words such as \"I mean\" or \"what I actually meant was\"), work out what they were trying to say. If their final restatement says it cleanly, keep that restatement exactly as spoken. If it does not, write what they were trying to say in words that match the rest of the transcript, its vocabulary and its tone, adding as few words they did not say as you can. Either way, delete the abandoned words. Third, where a clock time followed by a.m. or p.m. has a dot between the hour and the minutes, change the dot to a colon: 9.43 a.m. becomes 9:43 a.m. Leave every other number unchanged, including version numbers and decimals. Change nothing else: keep every other word, the word order, the spelling, the punctuation and the paragraph breaks exactly as they are. Do not rephrase, summarize, add, explain or answer anything else in the transcript. Reply with the corrected transcript only, with no preamble, quotes or notes. If nothing needs changing, reply with the transcript unchanged.";

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
    /// The reply adds more new words than the budget allows: words that are not glossary terms and are outside the longest run of the transcript's words it keeps in order. The budget is [`NEW_WORDS_MIN`], or one per [`NEW_WORDS_PER`] transcript words when that is more.
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

/// How many of `kept` are new: its length less the length of the longest common subsequence of `kept` and `r`, so the fewest words a reply could have added.
fn new_words(kept: &[&String], r: &[String]) -> usize {
    let mut prev = vec![0_usize; r.len().saturating_add(1)];
    for t in kept {
        let mut cur = vec![0_usize];
        for ((h, diag), up) in r.iter().zip(prev.iter()).zip(prev.iter().skip(1)) {
            let left = cur.last().copied().unwrap_or(0);
            cur.push(if h == *t {
                diag.saturating_add(1)
            } else {
                (*up).max(left)
            });
        }
        prev = cur;
    }
    kept.len().saturating_sub(prev.last().copied().unwrap_or(0))
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
/// glossary term's words, include more new words (see [`Reject::NewWords`]) than the larger of [`NEW_WORDS_MIN`] and the transcript's word count divided by [`NEW_WORDS_PER`]; the
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
    let budget = NEW_WORDS_MIN.max(r.len().checked_div(NEW_WORDS_PER).unwrap_or(0));
    if new_words(&kept, &r) > budget {
        return Err(Reject::NewWords);
    }
    if !tail(&r).iter().any(|t| tail(&y).contains(t)) {
        return Err(Reject::CutOff);
    }
    Ok(text.to_string())
}
