//! What to keep of the transcriber's output, and how segments join.
//!
//! Whisper invents text for silence ("Thank you.") rather than returning
//! nothing, so a silent segment is never sent to it, and a short segment
//! whose whole text is a known invention is dropped. Text with no letter or
//! digit is dropped at any length: Whisper primed with the punctuated style
//! prompt returns punctuation such as "." for noise. A segment is trimmed of
//! trailing silence before transcription (Whisper appends "Thank you." to room
//! tone), and the short-segment test runs on the trimmed length.

/// The silence test looks at windows of this many samples (100 ms).
pub const SILENCE_WINDOW: usize = 1_600;

/// A segment is silent when no [`SILENCE_WINDOW`] reaches this RMS level
/// (-40 dBFS). Speech at a normal distance from a laptop microphone sits
/// well above it; a quiet room sits well below.
pub const SILENCE_RMS: f32 = 0.01;

/// Samples kept after the last loud window by [`trim_trailing_silence`]
/// (500 ms at 16 kHz). Generous because a quiet speaker's final syllables sit
/// just under [`SILENCE_RMS`].
pub const TRAILING_PADDING: usize = 8_000;

/// Segments shorter than this many samples (10 s) are checked against
/// [`HALLUCINATIONS`].
pub const SHORT_SEGMENT: usize = 10 * 16_000;

/// Texts Whisper is known to produce from silence or noise. A short
/// segment whose whole text (trimmed, ignoring case) is one of these is
/// treated as silent.
pub const HALLUCINATIONS: [&str; 12] = [
    "Thank you.",
    "Thank you",
    "Thank you very much.",
    "Thanks for watching!",
    "Thanks for watching.",
    "Thank you for watching.",
    "Thank you for watching!",
    "Bye.",
    "you",
    "[BLANK_AUDIO]",
    "(silence)",
    "Subtitles by the Amara.org community",
];

/// What separates two segments' text in the transcript: a pause is usually
/// a thought boundary, so each segment is a paragraph.
pub const SEGMENT_SEPARATOR: &str = "\n\n";

/// True when `window` reaches [`SILENCE_RMS`].
fn loud(window: &[f32]) -> bool {
    let energy: f32 = window.iter().map(|s| s * s).sum();
    let count = u16::try_from(window.len()).map_or(f32::MAX, f32::from);
    energy / count >= SILENCE_RMS * SILENCE_RMS
}

/// True when no 100 ms window of `samples` reaches [`SILENCE_RMS`]. Empty
/// audio is silent; a trailing partial window counts as a window.
#[must_use]
pub fn is_silent(samples: &[f32]) -> bool {
    samples.chunks(SILENCE_WINDOW).all(|window| !loud(window))
}

/// The prefix of `samples` up to the end of its last loud 100 ms window (a
/// trailing partial window counts, judged on its own length) plus
/// [`TRAILING_PADDING`] samples, clamped to the length. Audio with no loud
/// window (empty or all quiet) gives an empty prefix, with no padding. The
/// result always starts at index 0, so leading silence is kept.
#[must_use]
pub fn trim_trailing_silence(samples: &[f32]) -> &[f32] {
    let last = samples
        .chunks(SILENCE_WINDOW)
        .enumerate()
        .rfind(|(_, window)| loud(window));
    let Some((i, window)) = last else {
        return samples.get(..0).unwrap_or(samples);
    };
    let end = i
        .saturating_mul(SILENCE_WINDOW)
        .saturating_add(window.len());
    let n = end.saturating_add(TRAILING_PADDING).min(samples.len());
    samples.get(..n).unwrap_or(samples)
}

/// The text worth keeping from `heard`, the transcriber's reply to `samples`
/// samples of audio (the length of what it was actually given, i.e. after
/// [`trim_trailing_silence`]): trimmed, and `None` when the text has no
/// letter or digit (at any length; the style prompt makes Whisper return "."
/// for noise) or the segment is short and the text is a known hallucination.
#[must_use]
pub fn keep(samples: usize, heard: &str) -> Option<String> {
    let text = heard.trim();
    let invented =
        samples < SHORT_SEGMENT && HALLUCINATIONS.iter().any(|h| h.eq_ignore_ascii_case(text));
    (text.chars().any(char::is_alphanumeric) && !invented).then(|| text.to_string())
}

/// Segment texts, in recording order, as one transcript.
#[must_use]
pub fn join<'a>(texts: impl IntoIterator<Item = &'a str>) -> String {
    texts
        .into_iter()
        .collect::<Vec<_>>()
        .join(SEGMENT_SEPARATOR)
}
