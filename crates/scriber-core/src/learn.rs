//! Learning vocabulary from the user's corrections. When the user fixes a
//! word in the finished transcript and presses Copy, [`learn`] compares the
//! transcript they were shown with the one they copied and returns the
//! corrected terms worth remembering; [`remember`] puts them at the head of
//! the learned list ([`crate::store::learned_path`], `learned.txt` in the data
//! folder unless `settings.json`'s `learned_vocabulary` names another); and
//! [`enforce`] rewrites later transcripts to the learned spellings. Pure
//! functions over text, with no I/O, so any app can use them.
//!
//! A replaced run of 1 to [`MAX_RUN_WORDS`] words on each side is learned
//! when it is a case-only change (but not a capital at a sentence start),
//! a join or split (`tight beam` to `tightbeam`), or a close spelling fix of
//! one word of at least [`MIN_FIX_CHARS`] characters. More than
//! [`MAX_RUNS`] such runs in one Copy is a rewrite, and nothing is learned.
//! `docs/design.md`, "Learning", has the full rule.

/// The most words on either side of a replaced run that can be learned.
pub const MAX_RUN_WORDS: usize = 3;

/// The most qualifying runs in one Copy; more is a rewrite, not corrections.
pub const MAX_RUNS: usize = 5;

/// The shortest corrected word a spelling fix can learn, in characters.
pub const MIN_FIX_CHARS: usize = 4;

/// The most terms the learned list keeps, the newest.
pub const MAX_LEARNED: usize = 100;

/// One word of a text: its stripped form and whether it starts a sentence.
struct Word {
    text: String,
    start: bool,
}

/// The text's words: split on whitespace, stripped of leading and trailing
/// non-alphanumeric chars and a trailing possessive `'s`, empty ones dropped.
fn words(text: &str) -> Vec<Word> {
    let mut out = Vec::new();
    let mut after_end = true;
    for raw in text.split(char::is_whitespace) {
        let stripped = raw.trim_matches(|c: char| !c.is_alphanumeric());
        let word: String = ["'s", "'S", "’s", "’S"]
            .iter()
            .find_map(|suffix| stripped.strip_suffix(suffix))
            .unwrap_or(stripped)
            .to_string();
        if word.is_empty() {
            continue;
        }
        out.push(Word {
            text: word,
            start: after_end,
        });
        after_end = raw
            .trim_end_matches(['"', '\'', '”', '’', ')', ']'])
            .ends_with(['.', '!', '?']);
    }
    out
}

/// A replaced run: the original words and the edited words.
type Run<'a> = (&'a [Word], &'a [Word]);

/// `words[from..to]`, empty when out of range.
fn part(words: &[Word], from: usize, to: usize) -> &[Word] {
    words.get(from..to).unwrap_or_default()
}

/// The runs of unmatched words between matched ones (and the text ends).
fn runs<'a>(original: &'a [Word], edited: &'a [Word]) -> Vec<Run<'a>> {
    let same = |a: &Word, b: &Word| a.text == b.text;
    let prefix = original
        .iter()
        .zip(edited)
        .take_while(|(a, b)| same(a, b))
        .count();
    let rest = original.len().min(edited.len()).saturating_sub(prefix);
    let suffix = original
        .iter()
        .rev()
        .zip(edited.iter().rev())
        .take(rest)
        .take_while(|(a, b)| same(a, b))
        .count();
    let old = part(original, prefix, original.len().saturating_sub(suffix));
    let new = part(edited, prefix, edited.len().saturating_sub(suffix));
    let table = lcs_table(old, new);
    let lcs = |row: usize, col: usize| {
        table
            .get(row)
            .and_then(|cells| cells.get(col))
            .copied()
            .unwrap_or(0)
    };
    let mut out = Vec::new();
    let (mut row, mut col, mut from_row, mut from_col) = (0, 0, 0, 0);
    while let (Some(a), Some(b)) = (old.get(row), new.get(col)) {
        let (down, right) = (row.saturating_add(1), col.saturating_add(1));
        if a.text == b.text {
            out.push((part(old, from_row, row), part(new, from_col, col)));
            (row, col) = (down, right);
            (from_row, from_col) = (row, col);
        } else if lcs(down, col) >= lcs(row, right) {
            row = down;
        } else {
            col = right;
        }
    }
    out.push((
        part(old, from_row, old.len()),
        part(new, from_col, new.len()),
    ));
    out
}

/// `table[i][j]`: the length of the longest common subsequence of
/// `old[i..]` and `new[j..]`.
fn lcs_table(old: &[Word], new: &[Word]) -> Vec<Vec<usize>> {
    let mut table = vec![vec![0usize; new.len().saturating_add(1)]; old.len().saturating_add(1)];
    for (row, a) in old.iter().enumerate().rev() {
        for (col, b) in new.iter().enumerate().rev() {
            let cell = |r: usize, c: usize| {
                table
                    .get(r)
                    .and_then(|cells| cells.get(c))
                    .copied()
                    .unwrap_or(0)
            };
            let (down, right) = (row.saturating_add(1), col.saturating_add(1));
            let value = if a.text == b.text {
                cell(down, right).saturating_add(1)
            } else {
                cell(down, col).max(cell(row, right))
            };
            if let Some(slot) = table.get_mut(row).and_then(|cells| cells.get_mut(col)) {
                *slot = value;
            }
        }
    }
    table
}

/// The Levenshtein distance between two char sequences.
fn levenshtein(a: &[char], b: &[char]) -> usize {
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut next = vec![i.saturating_add(1)];
        for (j, cb) in b.iter().enumerate() {
            let diagonal = row.get(j).copied().unwrap_or(0);
            let above = row.get(j.saturating_add(1)).copied().unwrap_or(0);
            let left = next.last().copied().unwrap_or(0);
            next.push(if ca == cb {
                diagonal
            } else {
                diagonal.min(above).min(left).saturating_add(1)
            });
        }
        row = next;
    }
    row.last().copied().unwrap_or(0)
}

/// Whether a run is learned (AC4 in the design's "Learning" section).
fn qualifies(o: &[Word], e: &[Word]) -> bool {
    if o.len() == e.len()
        && o.iter()
            .zip(e)
            .all(|(a, b)| a.text.to_lowercase() == b.text.to_lowercase())
    {
        return o.iter().zip(e).any(|(a, b)| {
            let (ac, bc): (Vec<char>, Vec<char>) =
                (a.text.chars().collect(), b.text.chars().collect());
            if ac.len() != bc.len() {
                return true;
            }
            ac.iter()
                .zip(&bc)
                .enumerate()
                .any(|(k, (x, y))| x != y && (k != 0 || !a.start))
        });
    }
    let key = |ws: &[Word]| -> String {
        ws.iter()
            .map(|w| w.text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase()
            .chars()
            .filter(|c| *c != ' ' && *c != '-')
            .collect()
    };
    if key(o) == key(e) {
        return true;
    }
    if let ([a], [b]) = (o, e) {
        let a: Vec<char> = a.text.to_lowercase().chars().collect();
        let b_chars: Vec<char> = b.text.to_lowercase().chars().collect();
        if b.text.chars().count() < MIN_FIX_CHARS {
            return false;
        }
        let longest = a.len().max(b_chars.len());
        return levenshtein(&a, &b_chars) <= (longest / 3).max(1);
    }
    false
}

/// Every term the edit qualifies for, in document order, neither capped
/// nor de-duplicated.
#[must_use]
pub fn candidates(original: &str, edited: &str) -> Vec<String> {
    let (o, e) = (words(original), words(edited));
    runs(&o, &e)
        .into_iter()
        .filter(|(a, b)| {
            (1..=MAX_RUN_WORDS).contains(&a.len()) && (1..=MAX_RUN_WORDS).contains(&b.len())
        })
        .filter(|(a, b)| qualifies(a, b))
        .map(|(_, b)| {
            b.iter()
                .map(|w| w.text.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect()
}

/// The terms to learn from the edit: none when more than [`MAX_RUNS`]
/// qualify, else the candidates de-duplicated case-insensitively, the first
/// kept.
#[must_use]
pub fn learn(original: &str, edited: &str) -> Vec<String> {
    let found = candidates(original, edited);
    if found.len() > MAX_RUNS {
        return Vec::new();
    }
    let mut out: Vec<String> = Vec::new();
    for term in found {
        if !out.iter().any(|t| t.to_lowercase() == term.to_lowercase()) {
            out.push(term);
        }
    }
    out
}

/// The learned list after learning `learned`: terms spelled exactly as in
/// `vocabulary` are dropped, the rest go to the head in order, older entries
/// equal to one of them case-insensitively are removed, and the list keeps
/// the newest [`MAX_LEARNED`]. Returns the new list and the terms placed at
/// its head.
#[must_use]
pub fn remember(
    list: &[String],
    learned: &[String],
    vocabulary: &[String],
) -> (Vec<String>, Vec<String>) {
    let added: Vec<String> = learned
        .iter()
        .filter(|t| !vocabulary.contains(t))
        .cloned()
        .collect();
    let lowered: Vec<String> = added.iter().map(|t| t.to_lowercase()).collect();
    let mut new = added.clone();
    new.extend(
        list.iter()
            .filter(|t| !lowered.contains(&t.to_lowercase()))
            .cloned(),
    );
    new.truncate(MAX_LEARNED);
    (new, added)
}

/// A term's key: lowercase, ASCII spaces and hyphens removed. A key that is
/// empty or holds anything but letters and digits never matches, because a
/// match only ever joins runs of letters and digits.
fn key(term: &str) -> String {
    term.to_lowercase()
        .chars()
        .filter(|c| *c != ' ' && *c != '-')
        .collect()
}

/// Whether `c` joins a word, so it cannot border a match.
fn joins(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '-'
}

/// `text` with every whole-word occurrence of a learned term rewritten to
/// the term's spelling: case-insensitive, and a join written with one space
/// or hyphen between up to three words counts (`tight beam`, `Tight-Beam`).
#[must_use]
pub fn enforce(text: &str, terms: &[String]) -> String {
    let keys: Vec<(&String, String)> = terms.iter().map(|t| (t, key(t))).collect();
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    // A `for` over the characters always ends, whatever a match reports.
    let mut skip_to = 0;
    for (i, &c) in chars.iter().enumerate() {
        if i < skip_to {
            continue;
        }
        let before = i.checked_sub(1).and_then(|p| chars.get(p));
        let found = if before.is_some_and(|b| joins(*b)) {
            None
        } else {
            best_match(&chars, i, &keys)
        };
        if let Some((end, term)) = found {
            out.push_str(term);
            skip_to = end;
        } else {
            out.push(c);
        }
    }
    out
}

/// The longest match starting at `start`, as (its end, the term), the
/// earlier term winning a tie.
fn best_match<'a>(
    chars: &[char],
    start: usize,
    keys: &[(&'a String, String)],
) -> Option<(usize, &'a String)> {
    let alnum = |k: usize| chars.get(k).is_some_and(|c| c.is_alphanumeric());
    let mut best = None;
    let mut joined = String::new();
    let mut at = start;
    for _ in 0..3 {
        if !alnum(at) {
            break;
        }
        while let Some(&c) = chars.get(at).filter(|c| c.is_alphanumeric()) {
            joined.push(c);
            at = at.saturating_add(1);
        }
        let lowered = joined.to_lowercase();
        if !chars.get(at).copied().is_some_and(joins)
            && let Some((term, _)) = keys.iter().find(|(_, k)| *k == lowered)
        {
            best = Some((at, *term));
        }
        if !matches!(chars.get(at), Some(' ' | '-')) {
            break;
        }
        at = at.saturating_add(1);
    }
    best
}
