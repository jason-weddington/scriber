//! A word list on disk: the words of a dictionary file, lowercased, so the
//! core can tell an ordinary word from the jargon of an embedder's
//! conversation ([`scriber_core::context`]). The engine reads the file once,
//! on the caller's thread, when the engine opens.

use std::collections::BTreeSet;
use std::path::Path;

use scriber_core::ports::Lexicon;

/// Every whitespace-separated word of a dictionary file, lowercased.
#[derive(Debug, Clone)]
pub struct WordList {
    words: BTreeSet<String>,
}

impl WordList {
    /// The word list at `path`, or `None` when the file cannot be read or is
    /// not UTF-8, which leaves the rule that uses it off. An empty file is a
    /// list that knows no word.
    #[must_use]
    pub fn load(path: &Path) -> Option<Self> {
        let bytes = std::fs::read(path).ok()?;
        let text = String::from_utf8(bytes).ok()?;
        Some(Self {
            words: text
                .split_whitespace()
                .map(str::to_lowercase)
                .collect::<BTreeSet<String>>(),
        })
    }
}

impl Lexicon for WordList {
    fn is_word(&self, word: &str) -> bool {
        self.words.contains(word)
    }
}
