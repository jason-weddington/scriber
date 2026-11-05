//! The optional cleanup pass over a finished transcript: a local language
//! model fixes words Whisper misheard that should be glossary terms,
//! repairs the speaker's spoken self-corrections into what they meant, and
//! writes a.m./p.m. clock times with a colon (9:43 a.m.), and nothing else.
//!
//! [`PROMPT`] is the system prompt and [`request`] the user message; the
//! model's answer is a [`Reply`] with a corrected transcript, the changes it
//! made and the words the speaker spelled out. [`filter`] reverts bogus
//! replacements, [`accept_reply`] keeps the reply only when [`accept`] finds
//! its transcript close enough, and [`learned_terms`] picks the new words
//! worth learning; otherwise the raw transcript is used. This module is pure:
//! the call itself goes through [`crate::ports::Cleaner`].

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

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

/// One change the cleanup model made: `from` as the transcript had it,
/// `to` as it should read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Replacement {
    /// The word or phrase as the transcript has it.
    pub from: String,
    /// The word or phrase to use instead.
    pub to: String,
}

/// The cleanup model's structured answer: the changes it made, the words the
/// speaker spelled out, and the corrected transcript.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Reply {
    /// Every change the model made.
    #[serde(default)]
    pub replacements: Vec<Replacement>,
    /// Every new word or proper name the speaker spelled out.
    #[serde(default)]
    pub learned: Vec<String>,
    /// The corrected transcript.
    pub transcript: String,
}

impl Reply {
    /// A reply carrying `transcript` with no replacements and nothing
    /// learned.
    #[must_use]
    pub fn new(transcript: impl Into<String>) -> Self {
        Self {
            replacements: Vec::new(),
            learned: Vec::new(),
            transcript: transcript.into(),
        }
    }
}

/// The cleanup model's system prompt.
pub const PROMPT: &str = concat!(
    "You correct a dictated transcript. Make only four kinds of change.",
    " ",
    "First, where a word or phrase was misheard and sounds closely like one of the glossary terms (and fits the sentence), replace it with that term spelled exactly as in the glossary; never replace an unrelated name or foreign word just because it starts with a similar sound.",
    " ",
    "Second, where the speaker explicitly spells out a word or name inside the transcript (for example, \"X is spelled A-B-C\"), use that spelling in normal title case (not all-caps unless it is an acronym) everywhere that word or name appears in the transcript (fixing any phonetic mishearing, including when joined to another word), while keeping the speaker's spelling sentence intact unless they were correcting a false start.",
    " ",
    "Third, where the speaker corrected themselves while speaking (a false start, a repeated phrase, or words such as \"I mean\" or \"what I actually meant was\"), work out what they were trying to say. If their final restatement says it cleanly, keep that restatement exactly as spoken. If it does not, write what they were trying to say in words that match the rest of the transcript, its vocabulary and its tone, adding as few words they did not say as you can. Either way, delete the abandoned words.",
    " ",
    "Fourth, where a clock time followed by a.m. or p.m. has a dot between the hour and the minutes, change the dot to a colon: 9.43 a.m. becomes 9:43 a.m. Leave every other number unchanged, including version numbers and decimals.",
    " ",
    "Change nothing else: keep every other word, the word order, the spelling, the punctuation and the paragraph breaks exactly as they are. Do not rephrase, summarize, add, explain or answer anything else in the transcript.",
    " ",
    "Record every change you make (or any spelled-out word, with `from` and `to` equal when already spelled right) in `replacements`, every new word or proper name the speaker spelled out in `learned` in normal title case, and the corrected transcript in `transcript` (unchanged when nothing needs changing).",
);

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

/// Inline-spelled words in `raw`: each whitespace-separated token trimmed of
/// leading and trailing non-alphanumerics and split on `-`, kept when it has
/// at least three parts that are each one ASCII letter, as the lowercase
/// concatenation of those letters, de-duplicated in first-found order.
#[must_use]
pub fn spelled(raw: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for piece in raw.split_whitespace() {
        let trimmed = piece.trim_matches(|c: char| !c.is_alphanumeric());
        let parts: Vec<&str> = trimmed.split('-').collect();
        if parts.len() >= 3
            && parts
                .iter()
                .all(|part| part.len() == 1 && part.chars().all(|c: char| c.is_ascii_alphabetic()))
        {
            let word: String = parts.iter().map(|p| p.to_ascii_lowercase()).collect();
            if !out.contains(&word) {
                out.push(word);
            }
        }
    }
    out
}

/// `word` with its first char uppercased and every other char lowercased.
fn titled(word: &str) -> String {
    let mut out = String::new();
    for (i, c) in word.chars().enumerate() {
        if i == 0 {
            out.push(c.to_ascii_uppercase());
        } else {
            out.push(c.to_ascii_lowercase());
        }
    }
    out
}

/// The reply with every bogus replacement reverted in its transcript and left
/// out, and its learned words passed through. Only a short substitution (1 to
/// 3 tokens a side, no ASCII digit) is judged: a replacement whose keys match
/// is bogus when it capitalises a word the speaker spelled out, and one whose
/// keys differ is bogus when its source is a spelled word or its target is
/// neither grounded in a spelled word nor within 40% of its source.
#[must_use]
pub fn filter(raw: &str, reply: &Reply) -> Reply {
    let spelled = spelled(raw);
    let mut transcript = reply.transcript.clone();
    let mut kept: Vec<Replacement> = Vec::new();
    for r in &reply.replacements {
        let fw = tokens(&r.from);
        let tw = tokens(&r.to);
        let short = (1..=3).contains(&fw.len())
            && (1..=3).contains(&tw.len())
            && !r.from.chars().any(|c: char| c.is_ascii_digit())
            && !r.to.chars().any(|c: char| c.is_ascii_digit());
        let bogus = short && {
            let fk = fw.join("");
            let tk = tw.join("");
            if fk == tk {
                spelled.contains(&fk)
                    && r.to
                        .chars()
                        .filter(|c: &char| c.is_ascii_alphabetic())
                        .count()
                        >= 4
                    && r.to.chars().all(|c: char| c.is_ascii_uppercase())
                    && r.from.chars().any(|c: char| c.is_ascii_lowercase())
            } else {
                let fk_chars: Vec<char> = fk.chars().collect();
                let tk_chars: Vec<char> = tk.chars().collect();
                let distance = crate::learn::levenshtein(&fk_chars, &tk_chars);
                let longest = fk_chars.len().max(tk_chars.len());
                let grounded = spelled.iter().any(|s| tk.contains(s));
                spelled.contains(&fk)
                    || (!grounded && distance.saturating_mul(5) > longest.saturating_mul(2))
            }
        };
        if bogus && !r.from.is_empty() && !r.to.is_empty() {
            transcript = transcript.replacen(&r.to, &r.from, 1);
        } else {
            kept.push(r.clone());
        }
    }
    Reply {
        replacements: kept,
        learned: reply.learned.clone(),
        transcript,
    }
}

/// Whether `candidate` is a new term worth learning from `reply`: not already
/// in `terms`, grounded in the transcript or `raw`, and either grounded in a
/// word the speaker spelled out or a learned word that the reply replaced
/// with a close enough term.
fn keeps(candidate: &str, raw: &str, reply: &Reply, terms: &[String], spelled: &[String]) -> bool {
    let lowered = candidate.to_lowercase();
    if terms.iter().any(|t| t.to_lowercase() == lowered) {
        return false;
    }
    let key = crate::learn::key(candidate);
    if !crate::learn::key(&reply.transcript).contains(&key)
        && !crate::learn::key(raw).contains(&key)
    {
        return false;
    }
    if spelled.iter().any(|s| key.contains(s)) {
        return true;
    }
    reply.learned.iter().any(|l| l.to_lowercase() == lowered)
        && candidate
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_uppercase())
        && reply.replacements.iter().any(|r| {
            let fk = tokens(&r.from).join("");
            let tk = tokens(&r.to).join("");
            fk != tk
                && r.to.to_lowercase() == lowered
                && !crate::learn::learn(&r.from, &r.to).is_empty()
        })
}

/// The new terms worth learning from `reply`: its `learned` words then the
/// target of each replacement, normalized, title-cased and compound-expanded
/// like the speaker's spellings, kept only when [`keeps`] agrees,
/// de-duplicated case-insensitively, and none at all when more than
/// [`crate::learn::MAX_RUNS`] qualify.
#[must_use]
pub fn learned_terms(raw: &str, reply: &Reply, terms: &[String]) -> Vec<String> {
    let spelled = spelled(raw);
    let mut candidates: Vec<String> = reply.learned.clone();
    candidates.extend(reply.replacements.iter().map(|r| r.to.clone()));
    let mut kept: Vec<String> = Vec::new();
    for candidate in candidates {
        let words: Vec<String> = candidate
            .split_whitespace()
            .map(|word| {
                word.trim_matches(|c: char| !c.is_alphanumeric())
                    .to_string()
            })
            .filter(|word| !word.is_empty())
            .collect();
        let normalized = words.join(" ");
        let letters = normalized
            .chars()
            .filter(|c: &char| c.is_alphanumeric())
            .count();
        if !(1..=crate::learn::MAX_RUN_WORDS).contains(&words.len())
            || letters < crate::learn::MIN_FIX_CHARS
            || !words
                .iter()
                .all(|word| word.chars().all(|c: char| c.is_alphanumeric() || c == '-'))
        {
            continue;
        }
        let mut expanded = vec![normalized.clone()];
        if words.len() == 1 {
            if normalized.chars().count() >= crate::learn::MIN_FIX_CHARS
                && normalized.chars().all(|c: char| c.is_ascii_uppercase())
                && spelled.iter().any(|s| *s == crate::learn::key(&normalized))
            {
                expanded = vec![titled(&normalized)];
            } else if let Some(s) = spelled
                .iter()
                .find(|s| crate::learn::key(&normalized) == format!("{s}labs"))
            {
                expanded = vec![titled(s), format!("{} Labs", titled(s))];
            }
        }
        for term in expanded {
            if keeps(&term, raw, reply, terms, &spelled) {
                kept.push(term);
            }
        }
    }
    if kept.len() > crate::learn::MAX_RUNS {
        return Vec::new();
    }
    let mut out: Vec<String> = Vec::new();
    for term in kept {
        if !out
            .iter()
            .any(|seen| seen.to_lowercase() == term.to_lowercase())
        {
            out.push(term);
        }
    }
    out
}

/// The cleaned text and the new terms to learn from `reply`, after [`filter`]
/// reverts its bogus replacements and [`accept`] judges the result with the
/// words the speaker spelled out added to the glossary.
///
/// # Errors
///
/// The [`Reject`] for the first rule [`accept`] finds the filtered transcript
/// breaking.
pub fn accept_reply(
    raw: &str,
    reply: &Reply,
    terms: &[String],
) -> Result<(String, Vec<String>), Reject> {
    let filtered = filter(raw, reply);
    let spelled = spelled(raw);
    let mut augmented = terms.to_vec();
    augmented.extend(spelled.iter().cloned());
    for candidate in &filtered.learned {
        if spelled
            .iter()
            .any(|s| crate::learn::key(candidate).contains(s))
        {
            augmented.extend(
                candidate
                    .split_whitespace()
                    .map(|word| word.trim_matches(|c: char| !c.is_alphanumeric()))
                    .filter(|word| !word.is_empty())
                    .map(str::to_string),
            );
        }
    }
    let clean = accept(raw, &filtered.transcript, &augmented)?;
    Ok((clean, learned_terms(raw, &filtered, terms)))
}
