//! The transcriber's initial prompt: a fixed style sentence that primes punctuation, then the user's glossary file
//! (`vocabulary.txt` in the app's data folder). Naming the terms up front
//! makes Whisper spell project names and jargon the way the user does.

/// A punctuated sentence the prompt always starts with, so Whisper writes periods and commas. Measured against real audio: do not edit.
pub const STYLE: &str =
    "Okay, here's what I need. First, check the board; then, once it's done, let me know.";

/// What the prompt starts with.
pub const PREFIX: &str = "Glossary: ";

/// What separates two terms in the prompt.
pub const SEPARATOR: &str = ", ";

/// The longest prompt, in characters. Whisper reads at most about 224
/// tokens of prompt; 500 characters stays well inside that.
pub const MAX_PROMPT_CHARS: usize = 500;

/// The glossary's terms, in file order: one per line, surrounding
/// whitespace trimmed, lines whose first non-blank character is `#` skipped,
/// control characters removed, and terms left empty skipped.
#[must_use]
pub fn terms(file: &str) -> Vec<String> {
    file.lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('#'))
        .map(|line| line.chars().filter(|c| !c.is_control()).collect::<String>())
        .filter(|term| !term.is_empty())
        .collect()
}

/// The prompt for a glossary file's contents: "<STYLE> Glossary: a, b, c.", or [`STYLE`] alone when there are no terms.
///
/// One term per line, surrounding whitespace trimmed. Blank lines and lines
/// whose first non-blank character is `#` are ignored, and control
/// characters are removed from terms. Terms are added in file order while
/// the whole prompt (style sentence included) stays within
/// [`MAX_PROMPT_CHARS`]; the first term that would overflow it ends the
/// list. No terms gives the style sentence alone.
#[must_use]
pub fn prompt(file: &str) -> String {
    let mut prompt = format!("{STYLE} {PREFIX}");
    let mut length = prompt.chars().count();
    let mut added = 0usize;
    for term in terms(file) {
        let separator = if added == 0 { "" } else { SEPARATOR };
        let grown = length
            .saturating_add(separator.chars().count())
            .saturating_add(term.chars().count());
        if grown.saturating_add(1) > MAX_PROMPT_CHARS {
            break;
        }
        prompt.push_str(separator);
        prompt.push_str(&term);
        length = grown;
        added = added.saturating_add(1);
    }
    if added == 0 {
        return STYLE.to_string();
    }
    prompt.push('.');
    prompt
}
