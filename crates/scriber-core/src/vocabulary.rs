//! The transcriber's initial prompt, built from the user's glossary file
//! (`vocabulary.txt` in the app's data folder). Naming the terms up front
//! makes Whisper spell project names and jargon the way the user does.

/// What the prompt starts with.
pub const PREFIX: &str = "Glossary: ";

/// What separates two terms in the prompt.
pub const SEPARATOR: &str = ", ";

/// The longest prompt, in characters. Whisper reads at most about 224
/// tokens of prompt; 500 characters stays well inside that.
pub const MAX_PROMPT_CHARS: usize = 500;

/// The prompt for a glossary file's contents: `Glossary: a, b, c`.
///
/// One term per line, surrounding whitespace trimmed. Blank lines and lines
/// whose first non-blank character is `#` are ignored, and control
/// characters are removed from terms. Terms are added in file order while
/// the prompt stays within [`MAX_PROMPT_CHARS`]; the first term that would
/// overflow it ends the list. No terms gives no prompt.
#[must_use]
pub fn prompt(file: &str) -> Option<String> {
    let mut prompt = String::from(PREFIX);
    let mut length = PREFIX.chars().count();
    let mut terms = 0usize;
    let cleaned = file
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('#'))
        .map(|line| line.chars().filter(|c| !c.is_control()).collect::<String>())
        .filter(|term| !term.is_empty());
    for term in cleaned {
        let separator = if terms == 0 { "" } else { SEPARATOR };
        let grown = length
            .saturating_add(separator.chars().count())
            .saturating_add(term.chars().count());
        if grown > MAX_PROMPT_CHARS {
            break;
        }
        prompt.push_str(separator);
        prompt.push_str(&term);
        length = grown;
        terms = terms.saturating_add(1);
    }
    (terms > 0).then_some(prompt)
}
