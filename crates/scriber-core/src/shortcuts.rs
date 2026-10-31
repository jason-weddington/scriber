//! Spoken keyboard shortcuts, written as Mac glyphs.
//!
//! A hand-written matcher (no regex: the rule needs lookaround and the
//! workspace lints forbid unwrapping a compiled pattern) with leftmost-first
//! backtracking, like the prototype it ports.

/// Modifier words and their bit in the modifier set.
const MODIFIERS: [(&str, u8); 7] = [
    ("command", 8),
    ("cmd", 8),
    ("control", 1),
    ("ctrl", 1),
    ("option", 2),
    ("alt", 2),
    ("shift", 4),
];

/// Modifier bits in the order they are written: ⌃⌥⇧⌘.
const GLYPHS: [(u8, char); 4] = [(1, '⌃'), (2, '⌥'), (4, '⇧'), (8, '⌘')];

/// Key words, longest first, with what they are written as and whether
/// they are evidence enough on their own.
const KEYS: [(&str, &str, bool); 30] = [
    ("forward slash", "/", false),
    ("right bracket", "]", false),
    ("close bracket", "]", false),
    ("left bracket", "[", false),
    ("open bracket", "[", false),
    ("backslash", "\\", false),
    ("semicolon", ";", false),
    ("backspace", "⌫", false),
    ("backtick", "`", false),
    ("period", ".", false),
    ("equals", "=", false),
    ("hyphen", "-", false),
    ("return", "↩", true),
    ("escape", "⎋", false),
    ("delete", "⌫", false),
    ("comma", ",", false),
    ("slash", "/", false),
    ("equal", "=", false),
    ("minus", "-", false),
    ("quote", "'", false),
    ("enter", "↩", true),
    ("space", "Space", false),
    ("right", "→", false),
    ("dash", "-", false),
    ("down", "↓", false),
    ("left", "←", false),
    ("dot", ".", false),
    ("tab", "⇥", true),
    ("esc", "⎋", false),
    ("up", "↑", false),
];

/// One way the key part can match.
struct Key {
    end: usize,
    out: String,
    letter: bool,
    strong: bool,
}

/// Writes spoken keyboard shortcuts in `text` as Mac glyphs.
///
/// A shortcut is one or more modifier words (command or cmd ⌘, control or
/// ctrl ⌃, option or alt ⌥, shift ⇧) and a key, apart by a space, a hyphen,
/// `+` or the word plus, in any case. Modifiers are written in the order
/// ⌃⌥⇧⌘ with duplicates collapsed. Keys are the punctuation words (comma,
/// period, slash, backslash, equals, minus, semicolon, backtick, quote and
/// the brackets), return and enter ↩, tab ⇥, escape ⎋, delete ⌫, the arrows,
/// Space (kept as the word), a letter (capitalised), a digit or F0 to F19.
/// The match must not touch a letter, digit, `_` or `-` on either side.
/// It converts only with strong evidence: ⌘ or ⌃, two or more modifiers, a
/// hyphen in the match, or enter, return or tab as the key; a single letter
/// also needs ⌘, ⌃ or a hyphen. Anything else stays as spoken.
#[must_use]
pub fn to_glyphs(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while let Some(c) = chars.get(i) {
        if let Some((end, replacement)) = start_at(&chars, i) {
            out.push_str(&replacement);
            i = end;
        } else {
            out.push(*c);
            i = i.saturating_add(1);
        }
    }
    out
}

fn word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '-'
}

fn boundary_before(chars: &[char], i: usize) -> bool {
    i.checked_sub(1)
        .and_then(|p| chars.get(p))
        .is_none_or(|c| !word_char(*c))
}

fn boundary_after(chars: &[char], i: usize) -> bool {
    chars.get(i).is_none_or(|c| !word_char(*c))
}

/// Matches `word` at `pos`, ignoring ASCII case; with `flex` a space in the
/// word also matches a hyphen. Answers the position after it.
fn lit(chars: &[char], pos: usize, word: &str, flex: bool) -> Option<usize> {
    let mut p = pos;
    for w in word.chars() {
        let c = chars.get(p)?;
        let ok = if flex && w == ' ' {
            *c == ' ' || *c == '-'
        } else {
            c.eq_ignore_ascii_case(&w)
        };
        if !ok {
            return None;
        }
        p = p.saturating_add(1);
    }
    Some(p)
}

fn modifier(chars: &[char], pos: usize) -> Option<(usize, u8)> {
    MODIFIERS
        .iter()
        .find_map(|(word, bit)| lit(chars, pos, word, false).map(|end| (end, *bit)))
}

fn skip_whitespace(chars: &[char], mut p: usize) -> usize {
    while chars.get(p).is_some_and(|c| c.is_whitespace()) {
        p = p.saturating_add(1);
    }
    p
}

/// Every end a separator at `pos` can have, in the order they are tried.
fn separators(chars: &[char], pos: usize) -> Vec<usize> {
    let mut ends = Vec::new();
    if matches!(chars.get(pos), Some(' ' | '-')) {
        ends.push(pos.saturating_add(1));
    }
    let plus = skip_whitespace(chars, pos);
    if chars.get(plus) == Some(&'+') {
        let from = plus.saturating_add(1);
        ends.extend((from..=skip_whitespace(chars, from)).rev());
    }
    if chars.get(pos) == Some(&'-') {
        ends.extend(lit(chars, pos.saturating_add(1), "plus-", false));
    }
    ends.extend(lit(chars, pos, "plus-", false));
    ends.extend(lit(chars, pos, " plus ", false));
    ends
}

/// Every way a key can match at `pos`, in the order they are tried.
fn keys(chars: &[char], pos: usize) -> Vec<Key> {
    let mut found: Vec<Key> = KEYS
        .iter()
        .filter_map(|(word, out, strong)| {
            lit(chars, pos, word, true).map(|end| Key {
                end,
                out: (*out).to_string(),
                letter: false,
                strong: *strong,
            })
        })
        .collect();
    if let Some(c) = chars.get(pos).filter(|c| c.is_ascii_alphanumeric()) {
        found.push(Key {
            end: pos.saturating_add(1),
            out: c.to_ascii_uppercase().to_string(),
            letter: c.is_ascii_alphabetic(),
            strong: false,
        });
    }
    if lit(chars, pos, "f", false).is_some() {
        let digit = |p: usize| chars.get(p).is_some_and(char::is_ascii_digit);
        let one = pos.saturating_add(1);
        let two = pos.saturating_add(2);
        let ends = [
            (chars.get(one) == Some(&'1') && digit(two)).then(|| two.saturating_add(1)),
            digit(one).then_some(two),
        ];
        for end in ends.into_iter().flatten() {
            found.push(Key {
                end,
                out: slice(chars, pos, end).to_ascii_uppercase(),
                letter: false,
                strong: false,
            });
        }
    }
    found
}

fn slice(chars: &[char], from: usize, to: usize) -> String {
    chars
        .get(from..to)
        .map(|s| s.iter().collect())
        .unwrap_or_default()
}

fn start_at(chars: &[char], start: usize) -> Option<(usize, String)> {
    if !boundary_before(chars, start) {
        return None;
    }
    let (end, bit) = modifier(chars, start)?;
    chain(chars, start, end, bit)
}

/// The modifiers so far end at `pos`: tries one more, then the key.
fn chain(chars: &[char], start: usize, pos: usize, mods: u8) -> Option<(usize, String)> {
    let seps = separators(chars, pos);
    for sep in &seps {
        if let Some((end, bit)) = modifier(chars, *sep)
            && let Some(found) = chain(chars, start, end, mods | bit)
        {
            return Some(found);
        }
    }
    for sep in &seps {
        for key in keys(chars, *sep) {
            if boundary_after(chars, key.end) {
                return Some((key.end, render(chars, start, mods, &key)));
            }
        }
    }
    None
}

/// The glyphs for a match, or the match as spoken without the evidence.
fn render(chars: &[char], start: usize, mods: u8, key: &Key) -> String {
    let spoken = slice(chars, start, key.end);
    let hyphen = spoken.contains('-');
    let strong = mods & 9 != 0 || mods.count_ones() > 1 || hyphen || key.strong;
    if !strong || (key.letter && mods & 9 == 0 && !hyphen) {
        return spoken;
    }
    let mut out: String = GLYPHS
        .iter()
        .filter(|(bit, _)| mods & bit != 0)
        .map(|(_, glyph)| *glyph)
        .collect();
    out.push_str(&key.out);
    out
}
