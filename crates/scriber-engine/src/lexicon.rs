//! An ordinary-word dictionary, lowercased, consulted by the core both to tell
//! an ordinary word from the jargon of an embedder's conversation
//! ([`scriber_core::context`]) and to prevent ordinary words from entering the
//! learned vocabulary ([`scriber_core::learn`], [`scriber_core::cleanup`]).
//! The engine resolves the dictionary once, on the caller's thread, when the
//! engine opens.

use std::collections::BTreeSet;
use std::path::Path;

use scriber_core::ports::Lexicon;

/// The bundled SCOWL 2020.12.07 size-60 English dictionary
/// (`crates/scriber-engine/data/scowl-en-60.txt`).
const BUNDLED_WORDS: &str = include_str!("../data/scowl-en-60.txt");

/// Every whitespace-separated word of a dictionary, lowercased.
#[derive(Debug, Clone)]
pub struct WordList {
    words: BTreeSet<String>,
}

impl WordList {
    /// Parses every whitespace-separated word in `text`, lowercased.
    fn from_text(text: &str) -> Self {
        Self {
            words: text
                .split_whitespace()
                .map(str::to_lowercase)
                .collect::<BTreeSet<String>>(),
        }
    }

    /// The bundled SCOWL 2020.12.07 size-60 English dictionary, compiled into
    /// the binary with no host dictionary I/O.
    ///
    /// Provenance and reproducible generation recipe:
    /// - Source archive URL:
    ///   `https://downloads.sourceforge.net/project/wordlist/SCOWL/2020.12.07/scowl-2020.12.07.tar.gz`
    /// - Verified archive SHA-256:
    ///   `5587667caa20c4891390c2d42dbb4d5c4c3f41bee77af1457ece3ba23fb859cc`
    /// - Selected archive members (all 28/28 present, 0 absent):
    ///   `final/{english,american,british,canadian}-words.{10,20,35,40,50,55,60}`
    ///   (`english-words.{10,20,35,40,50,55,60}`,
    ///   `american-words.{10,20,35,40,50,55,60}`,
    ///   `british-words.{10,20,35,40,50,55,60}`,
    ///   `canadian-words.{10,20,35,40,50,55,60}`)
    /// - Generation commands (run from the extracted `scowl-2020.12.07/` directory):
    ///   ```sh
    ///   cat final/{english,american,british,canadian}-words.{10,20,35,40,50,55,60} \
    ///     | iconv -f iso-8859-1 -t utf-8 \
    ///     | LC_ALL=C tr 'A-Z' 'a-z' \
    ///     | LC_ALL=C grep -E '^[a-z]+$' \
    ///     | LC_ALL=C sort -u \
    ///     > crates/scriber-engine/data/scowl-en-60.txt
    ///   cp Copyright crates/scriber-engine/data/SCOWL-COPYRIGHT
    ///   ```
    /// - Output `crates/scriber-engine/data/scowl-en-60.txt`:
    ///   79,685 lines (LF including final newline), 762,573 bytes,
    ///   SHA-256 `4ff42db4b8561e16d80b68ed8a80fadd13997d6a3adc0baed28fadf8f1743fc4`
    /// - Output `crates/scriber-engine/data/SCOWL-COPYRIGHT`:
    ///   SHA-256 `283326a422e29c510e2ba6805518c418ce3c35ed1fadc3eec83d2da4f6c5a055`
    #[must_use]
    pub fn bundled() -> Self {
        Self::from_text(BUNDLED_WORDS)
    }

    /// The word list at `path`, or `None` when the file cannot be read or is
    /// not UTF-8, which leaves the rules that use it off. An empty file is a
    /// list that knows no word.
    #[must_use]
    pub fn load(path: &Path) -> Option<Self> {
        let bytes = std::fs::read(path).ok()?;
        let text = String::from_utf8(bytes).ok()?;
        Some(Self::from_text(&text))
    }
}

impl Lexicon for WordList {
    fn is_word(&self, word: &str) -> bool {
        self.words.contains(word)
    }
}
