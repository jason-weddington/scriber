//! What the embedder's conversation contributes to the cleanup glossary: the
//! jargon in the turns it supplies, extracted without a model and without
//! I/O. This module is pure: it sees only strings and an optional word list.
//!
//! Every turn is split on whitespace and again on backticks; a piece that
//! starts with `~` is dropped before trimming (a path), and every other piece
//! is trimmed of anything that is not alphanumeric at its edges. A token is
//! dropped when it holds a `/`, starts with `www.`, holds a backslash, is
//! nothing but digits and `.` `,` `:` `-`, is shorter than 40 characters and
//! carries a run of eight or more hex-looking characters, or is longer than
//! 40 characters (so a token of exactly 40 is kept).
//!
//! A surviving token is kept when a lowercase letter is followed by an
//! uppercase one (camel-case names like `StreamRecorder`), when it mixes an
//! ASCII digit with a letter (`qwen3.8`), when an inner `-`, `_` or `.` has an
//! alphanumeric on each side (`scriber-core`), when it is a two to six letter
//! ASCII acronym (`GTD`), or when a word list is attached, it is at least
//! three letters, and the list does not know it as an ordinary word
//! (`yukara`). The last rule is off when no word list is attached, because
//! nothing can then tell jargon from an ordinary word.
//!
//! Kept tokens are deduped by their lowercase spelling (the first spelling is
//! kept and every occurrence counts), ordered by count, then by the last turn
//! that held them, then by where they first appeared, and cut to `max`.

use std::collections::HashMap;

/// The most context terms appended to one cleanup request's glossary.
pub const MAX_TERMS: usize = 30;

/// The longest token kept at all: a longer one is an id, a hash or a path.
const MAX_TOKEN_CHARS: usize = 40;

/// The shortest run of hex-looking characters that marks a token as an id or
/// a hash rather than a term.
const HEX_RUN_CHARS: usize = 8;

/// One kept token as it is accumulated: its first spelling, how often it was
/// seen, and where it was first and last seen.
struct Seen {
    /// The spelling of the first occurrence.
    spelling: String,
    /// Every occurrence, in any turn.
    count: usize,
    /// The index of the last turn holding it.
    last_turn: usize,
    /// The index of the turn of the first occurrence.
    first_turn: usize,
    /// The position in that turn of the first occurrence.
    first_at: usize,
}

/// The terms the turns contribute to the cleanup glossary, at most `max` of
/// them: the jargon of an embedder's conversation, in the order worth
/// glossing. `is_word` is the attached word list, or `None` when there is
/// none, which disables the rule that keeps a word the list does not know.
#[must_use]
pub fn terms(turns: &[String], is_word: Option<&dyn Fn(&str) -> bool>, max: usize) -> Vec<String> {
    let mut seen: Vec<Seen> = Vec::new();
    let mut known: HashMap<String, usize> = HashMap::new();
    for (turn, text) in turns.iter().enumerate() {
        let mut at: usize = 0;
        for word in text.split(char::is_whitespace) {
            for piece in word.split('`') {
                if piece.starts_with('~') {
                    continue;
                }
                let token = piece.trim_matches(|c: char| !c.is_alphanumeric());
                if token.is_empty() || discarded(token) || !kept(token, is_word) {
                    continue;
                }
                let key = token.to_lowercase();
                if let Some(&index) = known.get(&key) {
                    if let Some(entry) = seen.get_mut(index) {
                        entry.count = entry.count.saturating_add(1);
                        entry.last_turn = turn;
                    }
                } else {
                    let _known = known.insert(key, seen.len());
                    seen.push(Seen {
                        spelling: token.to_string(),
                        count: 1,
                        last_turn: turn,
                        first_turn: turn,
                        first_at: at,
                    });
                }
                at = at.saturating_add(1);
            }
        }
    }
    seen.sort_by(|a, b| {
        b.count
            .cmp(&a.count)
            .then_with(|| b.last_turn.cmp(&a.last_turn))
            .then_with(|| a.first_turn.cmp(&b.first_turn))
            .then_with(|| a.first_at.cmp(&b.first_at))
    });
    let mut terms: Vec<String> = seen.into_iter().map(|s| s.spelling).collect();
    terms.truncate(max);
    terms
}

/// Whether `token` is dropped whatever its shape: a path, a host, a Windows
/// path, a number, an id or hash, or something too long to be a term.
fn discarded(token: &str) -> bool {
    if token.contains('/') || token.starts_with("www.") || token.contains('\\') {
        return true;
    }
    if token
        .chars()
        .all(|c: char| c.is_ascii_digit() || matches!(c, '.' | ',' | ':' | '-'))
    {
        return true;
    }
    let chars: Vec<char> = token.chars().collect();
    if chars.len() < MAX_TOKEN_CHARS && longest_hex_run(&chars) >= HEX_RUN_CHARS {
        return true;
    }
    chars.len() > MAX_TOKEN_CHARS
}

/// The length of the longest maximal run of `[0-9a-fA-F]` characters.
fn longest_hex_run(chars: &[char]) -> usize {
    let mut longest: usize = 0;
    let mut run: usize = 0;
    for c in chars {
        if c.is_ascii_hexdigit() {
            run = run.saturating_add(1);
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    longest
}

/// Whether `token`, which survived every discard rule, has the shape of a
/// term: an inner camel case, a digit with a letter, an inner separator
/// between alphanumerics, a short acronym, or a word the attached list does
/// not know.
fn kept(token: &str, is_word: Option<&dyn Fn(&str) -> bool>) -> bool {
    let chars: Vec<char> = token.chars().collect();
    let camel = chars
        .iter()
        .zip(chars.iter().skip(1))
        .any(|(lower, upper)| lower.is_lowercase() && upper.is_uppercase());
    if camel {
        return true;
    }
    if token.chars().any(|c: char| c.is_ascii_digit())
        && token.chars().any(|c: char| c.is_alphabetic())
    {
        return true;
    }
    let separated = chars.windows(3).any(|window| {
        window
            .get(1)
            .is_some_and(|c: &char| matches!(c, '-' | '_' | '.'))
            && window.first().is_some_and(|c: &char| c.is_alphanumeric())
            && window.get(2).is_some_and(|c: &char| c.is_alphanumeric())
    });
    if separated {
        return true;
    }
    if (2..=6).contains(&chars.len()) && chars.iter().all(|c: &char| c.is_ascii_uppercase()) {
        return true;
    }
    let alphabetic = chars.len() >= 3 && chars.iter().all(|c: &char| c.is_alphabetic());
    if alphabetic
        && let Some(is_word) = is_word
        && !is_word(&token.to_lowercase())
    {
        return true;
    }
    false
}
