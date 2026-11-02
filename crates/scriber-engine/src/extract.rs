//! The pure logic of the study-104 extractor (`examples/extract.rs`): its
//! config and the checks on it, how a recording folder's input is chosen,
//! the 32-bit float WAV files it reads and writes, the stage-2 plan and
//! transcript, the features report and its production M0 outcome, the
//! Silero causality comparison, provenance and the run manifest.
//!
//! Nothing here calls Whisper, afconvert or the filesystem: the example
//! does, and hands the results to these functions, so the gate tests every
//! rule without a speech model.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::ops::Range;
use std::path::Path;

use scriber_core::ports::Phrase;
use scriber_core::store::segment_index;
use scriber_core::transcript::{
    SILENCE_RMS, SILENCE_WINDOW, TRAILING_PADDING, TRAILING_SILENCE_RMS, is_silent, join,
    kept_text, phantom, phrase_audio, trim_trailing_silence,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::encode::AFCONVERT;
use crate::whisper::{BEAM_PATIENCE, BEAM_SIZE, DetailedPhrase, LANGUAGE};

/// The whisper-rs version the extractor decodes with, as locked in
/// `Cargo.lock` (a test keeps the two equal).
pub const WHISPER_RS_VERSION: &str = "0.16.0";

/// Samples per second of every PCM the extractor handles.
pub const RATE: u32 = 16_000;

/// The step between the growing prefixes of the causality check, in seconds.
pub const PREFIX_STEP_S: f64 = 2.0;

/// The length of the independent chunks of the causality check, in seconds.
pub const CHUNK_S: f64 = 1.0;

/// How far two span boundaries may differ and still match, in seconds.
pub const TOLERANCE_S: f64 = 0.05;

/// The bytes read at a time when hashing (64 KiB).
pub const HASH_CHUNK: usize = 65_536;

/// Why the extractor cannot go on: with the config (the whole run) or with
/// one recording (that recording only).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExtractError {
    /// The config is unusable; nothing is written.
    #[error("config: {0}")]
    Config(String),
    /// A WAV file is not what the extractor reads or writes.
    #[error("wav: {0}")]
    Wav(String),
    /// One recording cannot be extracted.
    #[error("{0}")]
    Recording(String),
    /// Reading or writing failed.
    #[error("io: {0}")]
    Io(String),
}

fn config_error(message: impl Into<String>) -> ExtractError {
    ExtractError::Config(message.into())
}

fn recording_error(message: impl Into<String>) -> ExtractError {
    ExtractError::Recording(message.into())
}

fn wav_error(message: &str) -> ExtractError {
    ExtractError::Wav(message.to_string())
}

// ---- config ----------------------------------------------------------------

/// What the extractor produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Decoder features, PCM and the production outcome per recording.
    Features,
    /// The transcript under the lab's gating decisions.
    Stage2,
}

/// Which audio of a recording folder the extractor reads.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum InputMode {
    /// Segment WAVs when there are any, else audio.m4a, else audio.wav.
    #[default]
    Auto,
    /// audio.m4a, else audio.wav; segment WAVs are ignored.
    M4a,
    /// Segment WAVs only.
    Segments,
}

/// One Silero VAD setting to run.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VadSettings {
    /// The setting's name, unique in the config.
    pub name: String,
    /// The speech probability threshold.
    pub threshold: f32,
    /// The shortest silence that ends a span, in ms.
    pub min_silence_duration_ms: i32,
    /// The padding added on both sides of a span, in ms.
    pub speech_pad_ms: i32,
    /// The shortest span kept, in ms.
    pub min_speech_duration_ms: i32,
}

/// The Silero model and the settings to run it with.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SileroConfig {
    /// The path of the Silero ggml model.
    pub model: String,
    /// The settings, run in order.
    pub settings: Vec<VadSettings>,
}

/// The lab's gating decisions for one recording (stage 2).
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Decisions {
    /// Times (seconds on the recording's timeline) to cut pieces at (G3).
    #[serde(default)]
    pub split_s: Option<Vec<f64>>,
    /// One decision per piece, in timeline order.
    #[serde(default)]
    pub pieces: Option<Vec<PieceDecision>>,
}

/// The lab's decision for one piece.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PieceDecision {
    /// Skip the piece entirely (G1).
    #[serde(default)]
    pub skip: Option<bool>,
    /// Keep only this many seconds from the piece's start (G2).
    #[serde(default)]
    pub trim_end_s: Option<f64>,
    /// Whisper phrase indices of the piece to drop (G4).
    #[serde(default)]
    pub drop: Option<Vec<usize>>,
}

/// The extractor's config file.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// What to produce.
    pub mode: Mode,
    /// Which audio to read.
    #[serde(default)]
    pub input: InputMode,
    /// The ggml speech model.
    pub model: String,
    /// Where every output goes.
    pub output_dir: String,
    /// The recording folders, in order.
    pub recordings: Vec<String>,
    /// A vocabulary file for the prompt.
    #[serde(default)]
    pub vocabulary: Option<String>,
    /// Silero runs (features mode only).
    #[serde(default)]
    pub silero: Option<SileroConfig>,
    /// Gating decisions by recording name (stage2 mode only).
    #[serde(default)]
    pub decisions: Option<BTreeMap<String, Decisions>>,
}

/// A recording's name: its folder's final path component.
///
/// # Errors
///
/// The path has no final component (such as `/` or `..`).
pub fn recording_name(folder: &str) -> Result<String, ExtractError> {
    Path::new(folder)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| config_error(format!("recording {folder:?} has no folder name")))
}

/// The config in `text`, checked.
///
/// # Errors
///
/// [`ExtractError::Config`]: the JSON does not parse into a [`Config`] or
/// breaks one of the rules checked here.
pub fn parse_config(text: &str) -> Result<Config, ExtractError> {
    let config: Config = serde_json::from_str(text).map_err(|e| config_error(e.to_string()))?;
    validate(&config)?;
    Ok(config)
}

fn validate(config: &Config) -> Result<(), ExtractError> {
    match config.mode {
        Mode::Features if config.decisions.is_some() => {
            return Err(config_error("decisions are for mode stage2 only"));
        }
        Mode::Stage2 if config.silero.is_some() => {
            return Err(config_error("silero is for mode features only"));
        }
        Mode::Features | Mode::Stage2 => {}
    }
    if config.recordings.is_empty() {
        return Err(config_error("recordings is empty"));
    }
    let mut names = BTreeSet::new();
    for folder in &config.recordings {
        let name = recording_name(folder)?;
        if !names.insert(name.clone()) {
            return Err(config_error(format!("duplicate recording name {name}")));
        }
    }
    if let Some(silero) = &config.silero {
        if silero.settings.is_empty() {
            return Err(config_error("silero.settings is empty"));
        }
        let mut seen = BTreeSet::new();
        for s in &silero.settings {
            if !seen.insert(s.name.as_str()) {
                return Err(config_error(format!("duplicate silero setting {}", s.name)));
            }
        }
    }
    for (key, d) in config.decisions.iter().flatten() {
        if !names.contains(key) {
            return Err(config_error(format!(
                "decisions names unknown recording {key}"
            )));
        }
        validate_decisions(key, d)?;
    }
    Ok(())
}

fn validate_decisions(key: &str, d: &Decisions) -> Result<(), ExtractError> {
    let mut previous: Option<f64> = None;
    for &s in d.split_s.iter().flatten() {
        if !s.is_finite() || s <= 0.0 {
            return Err(config_error(format!("{key}: split_s {s} is not > 0")));
        }
        if previous.is_some_and(|p| s <= p) {
            return Err(config_error(format!(
                "{key}: split_s is not strictly increasing at {s}"
            )));
        }
        previous = Some(s);
    }
    for (i, piece) in d.pieces.iter().flatten().enumerate() {
        if piece.skip == Some(true) && (piece.trim_end_s.is_some() || piece.drop.is_some()) {
            return Err(config_error(format!(
                "{key}: piece {i} is skipped and also trimmed or dropped"
            )));
        }
        if let Some(t) = piece.trim_end_s
            && (!t.is_finite() || t <= 0.0)
        {
            return Err(config_error(format!(
                "{key}: piece {i} trim_end_s {t} is not > 0"
            )));
        }
        let mut seen = BTreeSet::new();
        for &index in piece.drop.iter().flatten() {
            if !seen.insert(index) {
                return Err(config_error(format!(
                    "{key}: piece {i} drops phrase {index} twice"
                )));
            }
        }
    }
    Ok(())
}

// ---- numbers -----------------------------------------------------------------

/// The sample offset of `s` seconds at 16 kHz, rounded to the nearest
/// sample.
///
/// # Errors
///
/// `s` is negative, not finite, or past the `u32` range in samples.
pub fn seconds_to_samples(s: f64) -> Result<usize, ExtractError> {
    if !s.is_finite() || s < 0.0 {
        return Err(recording_error(format!("{s} is not a time in seconds")));
    }
    let samples = (s * f64::from(RATE)).round();
    if samples > f64::from(u32::MAX) {
        return Err(recording_error(format!("{s} s is out of range")));
    }
    Ok(f64_to_usize(samples))
}

#[expect(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the caller has checked that the value is a whole number in the u32 range, which usize holds"
)]
fn f64_to_usize(x: f64) -> usize {
    x as usize
}

/// `n` samples at 16 kHz, in seconds.
///
/// # Errors
///
/// `n` is past the `u32` range.
pub fn samples_to_seconds(n: usize) -> Result<f64, ExtractError> {
    let n = u32::try_from(n).map_err(|e| recording_error(format!("{n} samples: {e}")))?;
    Ok(f64::from(n) / f64::from(RATE))
}

/// The highest RMS of any [`SILENCE_WINDOW`] of `samples` (a trailing
/// partial window counts), windowed as [`is_silent`] is; 0 for no samples.
#[must_use]
pub fn window_rms_max(samples: &[f32]) -> f32 {
    samples
        .chunks(SILENCE_WINDOW)
        .map(|window| {
            let energy: f32 = window.iter().map(|s| s * s).sum();
            let count = u16::try_from(window.len()).map_or(f32::MAX, f32::from);
            (energy / count).sqrt()
        })
        .fold(0.0, f32::max)
}

/// False for empty audio, which is never sent to Whisper.
#[must_use]
pub fn needs_decode(samples: &[f32]) -> bool {
    !samples.is_empty()
}

/// The prompt for a vocabulary file's contents: none without one.
#[must_use]
pub fn build_prompt(vocabulary: Option<&str>) -> Option<String> {
    vocabulary.map(scriber_core::vocabulary::prompt)
}

// ---- hashing -------------------------------------------------------------------

/// The lowercase hex SHA-256 of everything `reader` yields, read
/// [`HASH_CHUNK`] bytes at a time.
///
/// # Errors
///
/// [`ExtractError::Io`]: a read failed.
pub fn sha256_hex(reader: &mut dyn Read) -> Result<String, ExtractError> {
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; HASH_CHUNK];
    loop {
        let n = reader
            .read(&mut buffer)
            .map_err(|e| ExtractError::Io(e.to_string()))?;
        let Some(read) = buffer.get(..n).filter(|r| !r.is_empty()) else {
            break;
        };
        hasher.update(read);
    }
    Ok(hasher
        .finalize()
        .iter()
        .flat_map(|b| [b >> 4, b & 0x0f])
        .filter_map(|nibble| char::from_digit(u32::from(nibble), 16))
        .collect())
}

// ---- input -------------------------------------------------------------------------

/// Where a recording's input comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputKind {
    /// `segments/segment-NNNN.wav`, one input segment each.
    SegmentWavs,
    /// `audio.m4a`, decoded by afconvert, one input segment.
    M4a,
    /// `audio.wav`, one input segment.
    Wav,
}

impl InputKind {
    /// The report's `input` value.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::SegmentWavs => "segment-wavs",
            Self::M4a => "m4a-decoded-single-segment",
            Self::Wav => "wav-single-segment",
        }
    }

    /// The report's `audio_file` value.
    #[must_use]
    pub fn audio_file(self) -> &'static str {
        match self {
            Self::SegmentWavs => "segments",
            Self::M4a => "audio.m4a",
            Self::Wav => "audio.wav",
        }
    }
}

/// The input to read under `mode`, given which of the segment WAVs,
/// audio.m4a and audio.wav the folder holds.
///
/// # Errors
///
/// [`ExtractError::Recording`]: nothing usable under `mode`.
pub fn resolve_input(
    mode: InputMode,
    has_segments: bool,
    has_m4a: bool,
    has_wav: bool,
) -> Result<InputKind, ExtractError> {
    match mode {
        InputMode::Auto | InputMode::Segments if has_segments => Ok(InputKind::SegmentWavs),
        InputMode::Segments => Err(recording_error(
            "input \"segments\" requires segments/segment-NNNN.wav files, found none",
        )),
        InputMode::Auto | InputMode::M4a if has_m4a => Ok(InputKind::M4a),
        InputMode::Auto | InputMode::M4a if has_wav => Ok(InputKind::Wav),
        InputMode::Auto | InputMode::M4a => Err(recording_error(
            "no input: no segment WAVs, audio.m4a or audio.wav",
        )),
    }
}

/// The names among `names` that are segment WAVs, in ascending index order.
#[must_use]
pub fn segment_wav_names(names: &[&str]) -> Vec<String> {
    let mut found: Vec<(u32, &str)> = names
        .iter()
        .filter_map(|n| segment_index(n).map(|i| (i, *n)))
        .collect();
    found.sort_unstable();
    found.into_iter().map(|(_, n)| n.to_string()).collect()
}

/// afconvert's arguments that decode `m4a` to a 16 kHz mono 32-bit float
/// WAV at `wav`.
#[must_use]
pub fn afconvert_args(m4a: &Path, wav: &Path) -> Vec<String> {
    let mut args: Vec<String> = ["-f", "WAVE", "-d", "LEF32@16000", "-c", "1"]
        .into_iter()
        .map(String::from)
        .collect();
    args.push(m4a.to_string_lossy().into_owned());
    args.push(wav.to_string_lossy().into_owned());
    args
}

// ---- 32-bit float WAV --------------------------------------------------------------

/// The 44-byte header of a 16 kHz mono 32-bit float WAV of `n` samples.
///
/// # Errors
///
/// [`ExtractError::Wav`]: `n` samples do not fit a WAV file.
pub fn f32_wav_header(n: usize) -> Result<[u8; 44], ExtractError> {
    let data = u32::try_from(n)
        .ok()
        .and_then(|n| n.checked_mul(4))
        .filter(|d| d.checked_add(36).is_some())
        .ok_or_else(|| wav_error("too many samples for a WAV file"))?;
    let mut header = [0u8; 44];
    let fields: [&[u8]; 13] = [
        b"RIFF",
        &data.saturating_add(36).to_le_bytes(),
        b"WAVE",
        b"fmt ",
        &16u32.to_le_bytes(),
        &3u16.to_le_bytes(),
        &1u16.to_le_bytes(),
        &RATE.to_le_bytes(),
        &RATE.saturating_mul(4).to_le_bytes(),
        &4u16.to_le_bytes(),
        &32u16.to_le_bytes(),
        b"data",
        &data.to_le_bytes(),
    ];
    for (to, from) in header.iter_mut().zip(fields.concat()) {
        *to = from;
    }
    Ok(header)
}

/// `samples` as a 16 kHz mono 32-bit float WAV file.
///
/// # Errors
///
/// [`ExtractError::Wav`]: too many samples for a WAV file.
pub fn write_f32_wav(samples: &[f32]) -> Result<Vec<u8>, ExtractError> {
    let mut bytes = f32_wav_header(samples.len())?.to_vec();
    for s in samples {
        bytes.extend_from_slice(&s.to_le_bytes());
    }
    Ok(bytes)
}

fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(*bytes.get(at..)?.first_chunk()?))
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(*bytes.get(at..)?.first_chunk()?))
}

/// The samples of a 16 kHz mono 32-bit float WAV file (afconvert's
/// `LEF32@16000` output). Chunks other than `fmt ` and `data` are skipped,
/// with their even-byte padding.
///
/// # Errors
///
/// [`ExtractError::Wav`]: the bytes are not such a file.
pub fn read_f32_wav(bytes: &[u8]) -> Result<Vec<f32>, ExtractError> {
    if bytes.get(0..4) != Some(b"RIFF".as_slice()) || bytes.get(8..12) != Some(b"WAVE".as_slice()) {
        return Err(wav_error("not a RIFF WAVE file"));
    }
    let truncated = || wav_error("truncated");
    let mut format = None;
    let mut data = None;
    let mut at = 12usize;
    while at < bytes.len() {
        let id = bytes.get(at..at.saturating_add(4)).ok_or_else(truncated)?;
        let size = u32_at(bytes, at.saturating_add(4)).ok_or_else(truncated)?;
        let start = at.saturating_add(8);
        let end = start.saturating_add(usize::try_from(size).unwrap_or(usize::MAX));
        let body = bytes.get(start..end).ok_or_else(truncated)?;
        match id {
            b"fmt " => format = Some(body),
            b"data" => data = Some(body),
            _ => {}
        }
        at = end.saturating_add(end % 2);
    }
    let format = format.ok_or_else(|| wav_error("no fmt chunk"))?;
    let data = data.ok_or_else(|| wav_error("no data chunk"))?;
    if u16_at(format, 0) != Some(3) {
        return Err(wav_error("not IEEE float (format tag 3)"));
    }
    if u16_at(format, 2) != Some(1) {
        return Err(wav_error("not mono"));
    }
    if u32_at(format, 4) != Some(RATE) {
        return Err(wav_error("not 16 kHz"));
    }
    if u16_at(format, 14) != Some(32) {
        return Err(wav_error("not 32 bits per sample"));
    }
    Ok(data
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect())
}

// ---- checks ---------------------------------------------------------------------------

fn check_intervals(kind: &str, items: &[(f64, f64)], duration_s: f64) -> Result<(), ExtractError> {
    let mut previous_start = f64::NEG_INFINITY;
    for (i, &(start, end)) in items.iter().enumerate() {
        let problem = if start > end {
            "is inverted"
        } else if start < 0.0 {
            "starts before 0"
        } else if end > duration_s + 1.0 {
            "ends past the audio"
        } else if start < previous_start {
            "is out of order"
        } else {
            previous_start = start;
            continue;
        };
        return Err(recording_error(format!(
            "{kind} {i} ({start}-{end} s of {duration_s} s) {problem}"
        )));
    }
    Ok(())
}

/// Checks VAD spans (start, end in seconds) of audio `duration_s` long:
/// none inverted, negative, ending more than 1 s past the audio, or
/// starting before the one before it.
///
/// # Errors
///
/// [`ExtractError::Recording`] naming the first span that fails.
pub fn check_spans(spans: &[(f64, f64)], duration_s: f64) -> Result<(), ExtractError> {
    check_intervals("span", spans, duration_s)
}

/// Checks phrase times (t0, t1 in seconds) against the audio they were
/// decoded from, `duration_s` long, by the same rules as [`check_spans`].
///
/// # Errors
///
/// [`ExtractError::Recording`] naming the first phrase that fails.
pub fn check_phrases(phrases: &[(f64, f64)], duration_s: f64) -> Result<(), ExtractError> {
    check_intervals("phrase", phrases, duration_s)
}

fn phrase_times(phrases: &[Phrase], offset: usize) -> Result<Vec<(f64, f64)>, ExtractError> {
    phrases
        .iter()
        .map(|p| {
            Ok((
                samples_to_seconds(offset.saturating_add(p.start))?,
                samples_to_seconds(offset.saturating_add(p.end))?,
            ))
        })
        .collect()
}

// ---- stage 2 --------------------------------------------------------------------------

/// One piece of a recording's timeline, with the lab's decision applied.
#[derive(Debug, Clone, PartialEq)]
pub struct PlannedPiece {
    /// The piece's extent on the timeline, in samples, before any trim.
    pub range: Range<usize>,
    /// Skipped: not decoded, no text.
    pub skip: bool,
    /// The samples decoded from the piece's start (0 when skipped).
    pub decode_len: usize,
    /// The trim as configured, in seconds from the piece's start.
    pub trim_end_s: Option<f64>,
    /// Phrase indices to drop.
    pub drop: Vec<usize>,
}

/// True when `d` gates anything: a split or a piece list.
fn gates(d: &Decisions) -> bool {
    d.split_s.as_ref().is_some_and(|s| !s.is_empty())
        || d.pieces.as_ref().is_some_and(|p| !p.is_empty())
}

/// The pieces of a recording whose input segments are `input_lengths`
/// samples long, under the decisions `d` (none: one ungated piece per input
/// segment).
///
/// # Errors
///
/// [`ExtractError::Recording`]: a split is outside the recording, at an
/// input segment boundary or not strictly increasing in samples; the piece
/// list does not have one entry per piece; or a trim is 0 or longer than its
/// piece.
pub fn plan_pieces(
    input_lengths: &[usize],
    d: Option<&Decisions>,
) -> Result<Vec<PlannedPiece>, ExtractError> {
    let d = d.filter(|d| gates(d));
    let mut boundaries = Vec::new();
    let mut total = 0usize;
    for &len in input_lengths {
        boundaries.push(total);
        total = total.saturating_add(len);
    }
    let mut cuts = BTreeSet::new();
    let mut previous = 0usize;
    for &s in d.and_then(|d| d.split_s.as_ref()).into_iter().flatten() {
        let cut = seconds_to_samples(s)?;
        if cut <= previous || cut >= total || boundaries.contains(&cut) {
            return Err(recording_error(format!(
                "split_s {s} is not strictly inside the recording, past the previous split and off the input segment boundaries"
            )));
        }
        previous = cut;
        let _new = cuts.insert(cut);
    }
    let mut ranges = Vec::new();
    let mut start = 0usize;
    for &len in input_lengths {
        let end = start.saturating_add(len);
        let mut from = start;
        for &cut in cuts.range(start..end) {
            ranges.push(from..cut);
            from = cut;
        }
        ranges.push(from..end);
        start = end;
    }
    let decisions: &[PieceDecision] = match d.and_then(|d| d.pieces.as_ref()) {
        Some(p) if p.len() != ranges.len() => {
            return Err(recording_error(format!(
                "decisions pieces has {} entries, recording has {} pieces",
                p.len(),
                ranges.len()
            )));
        }
        Some(p) => p,
        None => &[],
    };
    ranges
        .into_iter()
        .enumerate()
        .map(|(i, range)| plan_piece(i, range, decisions.get(i)))
        .collect()
}

fn plan_piece(
    i: usize,
    range: Range<usize>,
    d: Option<&PieceDecision>,
) -> Result<PlannedPiece, ExtractError> {
    let d = d.cloned().unwrap_or_default();
    let skip = d.skip == Some(true);
    let decode_len = match d.trim_end_s {
        _ if skip => 0,
        None => range.len(),
        Some(t) => {
            let n = seconds_to_samples(t)?;
            if n == 0 || n > range.len() {
                return Err(recording_error(format!(
                    "piece {i}: trim_end_s {t} is not within the piece's {} samples",
                    range.len()
                )));
            }
            n
        }
    };
    Ok(PlannedPiece {
        range,
        skip,
        decode_len,
        trim_end_s: d.trim_end_s,
        drop: d.drop.unwrap_or_default(),
    })
}

/// Checks that `plan` covers `0..total` with no gap or overlap.
///
/// # Errors
///
/// [`ExtractError::Recording`]: the pieces are not contiguous.
pub fn check_contiguous(plan: &[PlannedPiece], total: usize) -> Result<(), ExtractError> {
    let mut at = 0usize;
    for (i, piece) in plan.iter().enumerate() {
        if piece.range.start != at {
            return Err(recording_error(format!(
                "piece {i} starts at sample {}, not {at}",
                piece.range.start
            )));
        }
        at = piece.range.end;
    }
    if at == total {
        Ok(())
    } else {
        Err(recording_error(format!(
            "the pieces end at sample {at}, not {total}"
        )))
    }
}

/// One phrase of a stage-2 piece.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Stage2Phrase {
    /// Its index among the piece's Whisper phrases.
    pub index: usize,
    /// Its start, in seconds on the recording's timeline.
    pub t0_s: f64,
    /// Its end, in seconds on the recording's timeline.
    pub t1_s: f64,
    /// Its text, as Whisper produced it.
    pub text: String,
    /// Dropped by the lab's decision.
    pub dropped: bool,
}

/// One stage-2 piece.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Stage2Piece {
    /// Its position in timeline order.
    pub index: usize,
    /// Its start, in seconds on the timeline.
    pub start_s: f64,
    /// Its end before any trim, in seconds on the timeline.
    pub end_s: f64,
    /// Skipped by the lab's decision.
    pub skipped: bool,
    /// The trim as configured.
    pub trim_end_s: Option<f64>,
    /// How long the decode took.
    pub elapsed_ms: u64,
    /// The piece's Whisper phrases.
    pub phrases: Vec<Stage2Phrase>,
    /// The kept phrases' text, trimmed; `None` when skipped.
    pub text: Option<String>,
}

/// The stage-2 outcome of one recording.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Stage2Output {
    /// True when there is a piece and every piece is skipped.
    pub skipped: bool,
    /// The pieces in timeline order.
    pub pieces: Vec<Stage2Piece>,
    /// The transcript before cleanup; `None` when skipped.
    pub text: Option<String>,
}

/// The stage-2 transcript of `plan`, given each piece's decoded phrases
/// (offsets into the piece) and decode time: the kept phrases of each piece
/// joined as Whisper produced them and trimmed, and the non-empty piece texts
/// joined as production joins segments.
///
/// # Errors
///
/// [`ExtractError::Recording`]: `decoded` or `elapsed_ms` does not hold one
/// entry per piece, or a drop index is past its piece's phrases.
pub fn assemble_pieces(
    plan: &[PlannedPiece],
    decoded: &[Vec<Phrase>],
    elapsed_ms: &[u64],
) -> Result<Stage2Output, ExtractError> {
    if decoded.len() != plan.len() || elapsed_ms.len() != plan.len() {
        return Err(recording_error(format!(
            "{} pieces, but {} decodes and {} timings",
            plan.len(),
            decoded.len(),
            elapsed_ms.len()
        )));
    }
    let mut pieces = Vec::new();
    for (index, ((piece, phrases), &elapsed)) in
        plan.iter().zip(decoded).zip(elapsed_ms).enumerate()
    {
        if let Some(&bad) = piece.drop.iter().find(|&&i| i >= phrases.len()) {
            return Err(recording_error(format!(
                "piece {index}: drop index {bad} is past its {} phrases",
                phrases.len()
            )));
        }
        let times = phrase_times(phrases, piece.range.start)?;
        let out: Vec<Stage2Phrase> = phrases
            .iter()
            .zip(times)
            .enumerate()
            .map(|(i, (p, (t0_s, t1_s)))| Stage2Phrase {
                index: i,
                t0_s,
                t1_s,
                text: p.text.clone(),
                dropped: piece.drop.contains(&i),
            })
            .collect();
        let text = (!piece.skip).then(|| {
            out.iter()
                .filter(|p| !p.dropped)
                .map(|p| p.text.as_str())
                .collect::<String>()
                .trim()
                .to_string()
        });
        pieces.push(Stage2Piece {
            index,
            start_s: samples_to_seconds(piece.range.start)?,
            end_s: samples_to_seconds(piece.range.end)?,
            skipped: piece.skip,
            trim_end_s: piece.trim_end_s,
            elapsed_ms: elapsed,
            phrases: out,
            text,
        });
    }
    let skipped = !pieces.is_empty() && pieces.iter().all(|p| p.skipped);
    let text = (!skipped).then(|| {
        join(
            pieces
                .iter()
                .filter_map(|p| p.text.as_deref())
                .filter(|t| !t.is_empty()),
        )
    });
    Ok(Stage2Output {
        skipped,
        pieces,
        text,
    })
}

// ---- features -------------------------------------------------------------------------

/// One token of a reported phrase.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TokenOut {
    /// The token's text.
    pub text: String,
    /// The token id.
    pub id: i32,
    /// A special token.
    pub special: bool,
    /// Its probability.
    pub probability: f32,
    /// Its log probability.
    pub log_probability: f32,
}

/// One reported Whisper phrase.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PhraseOut {
    /// Its index within its input segment's decode.
    pub index: usize,
    /// Its start in seconds.
    pub t0_s: f64,
    /// Its end in seconds.
    pub t1_s: f64,
    /// Its text.
    pub text: String,
    /// Whisper's no-speech probability.
    pub no_speech_probability: f32,
    /// Its own audio is silent at [`SILENCE_RMS`].
    pub silent: bool,
    /// [`phantom`] on the audio it was decoded from.
    pub phantom: bool,
    /// [`window_rms_max`] of its own audio.
    pub rms_max: f32,
    /// Every token.
    pub tokens: Vec<TokenOut>,
}

/// A phrase on the recording's concatenated timeline.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RecordingPhraseOut {
    /// The input segment it belongs to.
    pub segment: usize,
    /// The phrase, with times offset onto the concatenated timeline.
    #[serde(flatten)]
    pub phrase: PhraseOut,
}

/// One decode's report.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DecodeResult<P> {
    /// The phrases.
    pub phrases: Vec<P>,
    /// How long the decode took.
    pub elapsed_ms: u64,
}

/// One decode as the example hands it over.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Decoded {
    /// Whisper's phrases (empty when not decoded).
    pub phrases: Vec<DetailedPhrase>,
    /// How long it took (0 when not decoded).
    pub elapsed_ms: u64,
}

/// Production's outcome for one input segment.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SegmentM0 {
    /// [`is_silent`] on the untrimmed audio: production never decodes it.
    pub skipped_as_silent: bool,
    /// [`kept_text`] on the trimmed audio and its phrases.
    pub text_before_cleanup: Option<String>,
    /// The last phrase's index when it is a [`phantom`].
    pub phantom_dropped_index: Option<usize>,
    /// The phrases [`kept_text`] keeps.
    pub kept_phrase_indices: Vec<usize>,
    /// Phrases were kept but `keep` rejected their text.
    pub keep_rejected: bool,
}

/// Production's M0 outcome for one input segment: `untrimmed` as recorded,
/// `trimmed` as given to Whisper and `phrases` as Whisper heard `trimmed`.
#[must_use]
pub fn segment_m0(untrimmed: &[f32], trimmed: &[f32], phrases: &[Phrase]) -> SegmentM0 {
    if is_silent(untrimmed) {
        return SegmentM0 {
            skipped_as_silent: true,
            text_before_cleanup: None,
            phantom_dropped_index: None,
            kept_phrase_indices: Vec::new(),
            keep_rejected: false,
        };
    }
    let text = kept_text(trimmed, phrases);
    let phantom_index = phrases
        .split_last()
        .filter(|(last, _)| phantom(trimmed, last))
        .map(|(_, rest)| rest.len());
    let kept: Vec<usize> = (0..phrases.len())
        .filter(|&i| Some(i) != phantom_index)
        .collect();
    SegmentM0 {
        skipped_as_silent: false,
        keep_rejected: !kept.is_empty() && text.is_none(),
        text_before_cleanup: text,
        phantom_dropped_index: phantom_index,
        kept_phrase_indices: kept,
    }
}

/// A phantom dropped from one input segment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PhantomRef {
    /// The input segment.
    pub segment: usize,
    /// The phrase index in its trimmed decode.
    pub index: usize,
}

/// Production's outcome for the whole recording.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RecordingM0 {
    /// Every input segment is silent.
    pub skipped_as_silent: bool,
    /// The segments' texts joined as production joins them.
    pub text_before_cleanup: Option<String>,
    /// The phantoms dropped.
    pub phantom_dropped: Vec<PhantomRef>,
    /// The segments whose text `keep` rejected.
    pub keep_rejected_segments: Vec<usize>,
}

/// The recording's M0: the segments' outcomes joined in order.
#[must_use]
pub fn recording_m0(segments: &[SegmentM0]) -> RecordingM0 {
    let texts: Vec<&str> = segments
        .iter()
        .filter_map(|s| s.text_before_cleanup.as_deref())
        .collect();
    RecordingM0 {
        skipped_as_silent: segments.iter().all(|s| s.skipped_as_silent),
        text_before_cleanup: (!texts.is_empty()).then(|| join(texts)),
        phantom_dropped: segments
            .iter()
            .enumerate()
            .filter_map(|(segment, s)| {
                s.phantom_dropped_index
                    .map(|index| PhantomRef { segment, index })
            })
            .collect(),
        keep_rejected_segments: segments
            .iter()
            .enumerate()
            .filter(|(_, s)| s.keep_rejected)
            .map(|(i, _)| i)
            .collect(),
    }
}

/// One input segment's report.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SegmentOut {
    /// Its position, from 0.
    pub index: usize,
    /// Its segment WAV's name; `None` for a single-file input.
    pub file: Option<String>,
    /// Samples as recorded.
    pub samples_untrimmed: usize,
    /// Samples after the trailing trim.
    pub samples_trimmed: usize,
    /// Seconds as recorded.
    pub duration_untrimmed_s: f64,
    /// Seconds after the trailing trim.
    pub duration_trimmed_s: f64,
    /// [`is_silent`] on the untrimmed audio.
    pub is_silent: bool,
    /// [`window_rms_max`] of the untrimmed audio.
    pub rms_max: f32,
    /// The decode of the untrimmed audio.
    pub untrimmed: DecodeResult<PhraseOut>,
    /// The decode of the trimmed audio.
    pub trimmed: DecodeResult<PhraseOut>,
    /// Production's outcome.
    pub m0: SegmentM0,
}

/// One input segment as the example hands it over.
#[derive(Debug, Clone, PartialEq)]
pub struct SegmentInput {
    /// Its segment WAV's name; `None` for a single-file input.
    pub file: Option<String>,
    /// Its decoded audio, as recorded.
    pub untrimmed: Vec<f32>,
    /// Whisper on `untrimmed`.
    pub untrimmed_decode: Decoded,
    /// Whisper on `trim_trailing_silence(untrimmed)`.
    pub trimmed_decode: Decoded,
}

fn phrases_out(pcm: &[f32], decoded: &Decoded) -> Result<Vec<PhraseOut>, ExtractError> {
    decoded
        .phrases
        .iter()
        .enumerate()
        .map(|(index, d)| {
            let own = phrase_audio(pcm, &d.phrase);
            Ok(PhraseOut {
                index,
                t0_s: samples_to_seconds(d.phrase.start)?,
                t1_s: samples_to_seconds(d.phrase.end)?,
                text: d.phrase.text.clone(),
                no_speech_probability: d.no_speech_probability,
                silent: is_silent(own),
                phantom: phantom(pcm, &d.phrase),
                rms_max: window_rms_max(own),
                tokens: d
                    .tokens
                    .iter()
                    .map(|t| TokenOut {
                        text: t.text.clone(),
                        id: t.id,
                        special: t.special,
                        probability: t.probability,
                        log_probability: t.log_probability,
                    })
                    .collect(),
            })
        })
        .collect()
}

fn decode_result(pcm: &[f32], decoded: &Decoded) -> Result<DecodeResult<PhraseOut>, ExtractError> {
    let phrases: Vec<Phrase> = decoded.phrases.iter().map(|d| d.phrase.clone()).collect();
    check_phrases(&phrase_times(&phrases, 0)?, samples_to_seconds(pcm.len())?)?;
    Ok(DecodeResult {
        phrases: phrases_out(pcm, decoded)?,
        elapsed_ms: decoded.elapsed_ms,
    })
}

/// The report of input segment `index`.
///
/// # Errors
///
/// [`ExtractError::Recording`]: a decode's phrase times fail
/// [`check_phrases`] against the audio it was given.
pub fn build_segment(index: usize, input: &SegmentInput) -> Result<SegmentOut, ExtractError> {
    let untrimmed = input.untrimmed.as_slice();
    let trimmed = trim_trailing_silence(untrimmed);
    let heard: Vec<Phrase> = input
        .trimmed_decode
        .phrases
        .iter()
        .map(|d| d.phrase.clone())
        .collect();
    Ok(SegmentOut {
        index,
        file: input.file.clone(),
        samples_untrimmed: untrimmed.len(),
        samples_trimmed: trimmed.len(),
        duration_untrimmed_s: samples_to_seconds(untrimmed.len())?,
        duration_trimmed_s: samples_to_seconds(trimmed.len())?,
        is_silent: is_silent(untrimmed),
        rms_max: window_rms_max(untrimmed),
        untrimmed: decode_result(untrimmed, &input.untrimmed_decode)?,
        trimmed: decode_result(trimmed, &input.trimmed_decode)?,
        m0: segment_m0(untrimmed, trimmed, &heard),
    })
}

/// Where each file of a segment-WAV input came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SegmentFile {
    /// The file name.
    pub file: String,
    /// Its SHA-256.
    pub sha256: String,
    /// Its samples.
    pub samples: usize,
}

/// How the audio was decoded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AudioDecode {
    /// afconvert for an m4a input, else `None`.
    pub tool: Option<String>,
    /// afconvert's exact arguments, else empty.
    pub args: Vec<String>,
    /// The samples of every input segment.
    pub samples: usize,
    /// Always 16000.
    pub sample_rate: u32,
}

impl AudioDecode {
    /// An afconvert decode with `args` (empty: no afconvert) of `samples`
    /// samples.
    #[must_use]
    pub fn new(args: Vec<String>, samples: usize) -> Self {
        Self {
            tool: (!args.is_empty()).then(|| AFCONVERT.to_string()),
            args,
            samples,
            sample_rate: RATE,
        }
    }
}

/// The decode parameters, as whisper.rs sets them.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DecodeParams {
    /// Always `beam_search`.
    pub strategy: &'static str,
    /// The beam width.
    pub beam_size: i32,
    /// The beam patience.
    pub patience: f32,
    /// The forced language.
    pub language: &'static str,
    /// Non-speech tokens are suppressed.
    pub suppress_nst: bool,
    /// The initial prompt.
    pub prompt: Option<String>,
}

impl DecodeParams {
    /// The production decode parameters with `prompt`.
    #[must_use]
    pub fn new(prompt: Option<String>) -> Self {
        Self {
            strategy: "beam_search",
            beam_size: BEAM_SIZE,
            patience: BEAM_PATIENCE,
            language: LANGUAGE,
            suppress_nst: true,
            prompt,
        }
    }
}

/// The core's silence thresholds.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Thresholds {
    /// [`SILENCE_RMS`].
    pub silence_rms: f32,
    /// [`TRAILING_SILENCE_RMS`].
    pub trailing_silence_rms: f32,
    /// [`TRAILING_PADDING`].
    pub trailing_padding: usize,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            silence_rms: SILENCE_RMS,
            trailing_silence_rms: TRAILING_SILENCE_RMS,
            trailing_padding: TRAILING_PADDING,
        }
    }
}

/// Which code the numbers came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Provenance {
    /// The scriber commit.
    pub scriber_commit: String,
    /// Whether the working tree had changes; `None` when the commit came
    /// from `SCRIBER_COMMIT`.
    pub scriber_dirty: Option<bool>,
}

/// The provenance: `SCRIBER_COMMIT` (`env_commit`) when set and non-empty,
/// else `git rev-parse HEAD` (`git_head`) with `git status --porcelain`'s
/// output (`git_status`) deciding dirtiness.
///
/// # Errors
///
/// [`ExtractError::Config`]: neither gives a commit.
pub fn provenance(
    env_commit: Option<&str>,
    git_head: Option<&str>,
    git_status: Option<&str>,
) -> Result<Provenance, ExtractError> {
    if let Some(commit) = env_commit.map(str::trim).filter(|c| !c.is_empty()) {
        return Ok(Provenance {
            scriber_commit: commit.to_string(),
            scriber_dirty: None,
        });
    }
    let head = git_head
        .map(str::trim)
        .filter(|h| !h.is_empty())
        .ok_or_else(|| {
            config_error("no scriber commit: set SCRIBER_COMMIT or build in a git checkout")
        })?;
    Ok(Provenance {
        scriber_commit: head.to_string(),
        scriber_dirty: git_status.map(|s| !s.trim().is_empty()),
    })
}

/// The fields both reports open with.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Common {
    /// [`InputKind::label`].
    pub input: &'static str,
    /// The provenance.
    #[serde(flatten)]
    pub provenance: Provenance,
    /// [`WHISPER_RS_VERSION`].
    pub whisper_rs_version: &'static str,
    /// The speech model's file name.
    pub model_file: String,
    /// Its SHA-256.
    pub model_sha256: String,
    /// [`InputKind::audio_file`].
    pub audio_file: &'static str,
    /// The audio's SHA-256 (segment WAVs: of their bytes concatenated).
    pub audio_sha256: String,
    /// The segment WAVs read.
    pub audio_segment_files: Vec<SegmentFile>,
    /// How the audio was decoded.
    pub audio_decode: AudioDecode,
    /// The decode parameters.
    pub decode: DecodeParams,
    /// Always false: production prompts with the style sentence and the
    /// learned terms, the extractor with the vocabulary file only.
    pub prompt_matches_production: bool,
}

/// One VAD span.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Span {
    /// Its start in seconds.
    pub start_s: f64,
    /// Its end in seconds.
    pub end_s: f64,
}

impl From<(f64, f64)> for Span {
    fn from((start_s, end_s): (f64, f64)) -> Self {
        Self { start_s, end_s }
    }
}

/// One Silero run.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VadRun {
    /// The setting's name.
    pub name: String,
    /// The threshold.
    pub threshold: f32,
    /// The minimum silence, in ms.
    pub min_silence_duration_ms: i32,
    /// The padding, in ms.
    pub speech_pad_ms: i32,
    /// The minimum speech, in ms.
    pub min_speech_duration_ms: i32,
    /// How long the run took.
    pub vad_elapsed_ms: u64,
    /// The speech spans.
    pub spans: Vec<Span>,
}

impl VadRun {
    /// The run of `settings` that found `spans` in `elapsed_ms`.
    #[must_use]
    pub fn new(settings: &VadSettings, elapsed_ms: u64, spans: Vec<Span>) -> Self {
        Self {
            name: settings.name.clone(),
            threshold: settings.threshold,
            min_silence_duration_ms: settings.min_silence_duration_ms,
            speech_pad_ms: settings.speech_pad_ms,
            min_speech_duration_ms: settings.min_speech_duration_ms,
            vad_elapsed_ms: elapsed_ms,
            spans,
        }
    }
}

/// The Silero report.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SileroOut {
    /// The model's file name.
    pub model_file: String,
    /// Its SHA-256.
    pub model_sha256: String,
    /// One per setting.
    pub runs: Vec<VadRun>,
    /// The causality check (first recording only).
    pub causality: Option<Causality>,
}

/// The features report of one recording.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FeaturesReport {
    /// The opening fields.
    #[serde(flatten)]
    pub common: Common,
    /// The core's thresholds.
    pub thresholds: Thresholds,
    /// The number of input segments.
    pub segment_count: usize,
    /// Samples as recorded.
    pub samples_untrimmed: usize,
    /// Samples after each segment's trailing trim.
    pub samples_trimmed: usize,
    /// Seconds as recorded.
    pub duration_untrimmed_s: f64,
    /// Seconds after the trims.
    pub duration_trimmed_s: f64,
    /// Every input segment is silent.
    pub is_silent: bool,
    /// The highest segment `rms_max`.
    pub rms_max: f32,
    /// The untrimmed decodes on the concatenated untrimmed timeline.
    pub untrimmed: DecodeResult<RecordingPhraseOut>,
    /// The trimmed decodes on the concatenated trimmed timeline.
    pub trimmed: DecodeResult<RecordingPhraseOut>,
    /// Production's outcome.
    pub m0: RecordingM0,
    /// The Silero runs; `None` when not configured.
    pub silero: Option<SileroOut>,
    /// The input segments.
    pub segments: Vec<SegmentOut>,
}

fn concatenated(
    segments: &[SegmentOut],
    lengths: &[usize],
    pick: fn(&SegmentOut) -> &DecodeResult<PhraseOut>,
) -> Result<DecodeResult<RecordingPhraseOut>, ExtractError> {
    let mut offset = 0usize;
    let mut phrases = Vec::new();
    let mut elapsed_ms = 0u64;
    for (s, &len) in segments.iter().zip(lengths) {
        let offset_s = samples_to_seconds(offset)?;
        let result = pick(s);
        elapsed_ms = elapsed_ms.saturating_add(result.elapsed_ms);
        for p in &result.phrases {
            let mut phrase = p.clone();
            phrase.t0_s += offset_s;
            phrase.t1_s += offset_s;
            phrases.push(RecordingPhraseOut {
                segment: s.index,
                phrase,
            });
        }
        offset = offset.saturating_add(len);
    }
    Ok(DecodeResult {
        phrases,
        elapsed_ms,
    })
}

/// The features report of a recording whose input segments are `segments`.
///
/// # Errors
///
/// [`ExtractError::Recording`]: the recording is too long to time.
pub fn features_report(
    common: Common,
    segments: Vec<SegmentOut>,
    silero: Option<SileroOut>,
) -> Result<FeaturesReport, ExtractError> {
    let untrimmed: Vec<usize> = segments.iter().map(|s| s.samples_untrimmed).collect();
    let trimmed: Vec<usize> = segments.iter().map(|s| s.samples_trimmed).collect();
    let samples_untrimmed = untrimmed.iter().fold(0usize, |a, b| a.saturating_add(*b));
    let samples_trimmed = trimmed.iter().fold(0usize, |a, b| a.saturating_add(*b));
    let m0s: Vec<SegmentM0> = segments.iter().map(|s| s.m0.clone()).collect();
    Ok(FeaturesReport {
        common,
        thresholds: Thresholds::default(),
        segment_count: segments.len(),
        samples_untrimmed,
        samples_trimmed,
        duration_untrimmed_s: samples_to_seconds(samples_untrimmed)?,
        duration_trimmed_s: samples_to_seconds(samples_trimmed)?,
        is_silent: segments.iter().all(|s| s.is_silent),
        rms_max: segments.iter().map(|s| s.rms_max).fold(0.0, f32::max),
        untrimmed: concatenated(&segments, &untrimmed, |s| &s.untrimmed)?,
        trimmed: concatenated(&segments, &trimmed, |s| &s.trimmed)?,
        m0: recording_m0(&m0s),
        silero,
        segments,
    })
}

/// The stage-2 report of one recording.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Stage2Report {
    /// The opening fields.
    #[serde(flatten)]
    pub common: Common,
    /// The decisions applied, as configured; `None` when none.
    pub decisions: Option<Decisions>,
    /// Each input segment's length in seconds.
    pub input_segment_lengths_s: Vec<f64>,
    /// The outcome.
    #[serde(flatten)]
    pub output: Stage2Output,
}

/// The stage-2 report of a recording with input segments `lengths` samples
/// long.
///
/// # Errors
///
/// [`ExtractError::Recording`]: a length is too long to time.
pub fn stage2_report(
    common: Common,
    decisions: Option<Decisions>,
    lengths: &[usize],
    output: Stage2Output,
) -> Result<Stage2Report, ExtractError> {
    Ok(Stage2Report {
        common,
        decisions,
        input_segment_lengths_s: lengths
            .iter()
            .map(|&n| samples_to_seconds(n))
            .collect::<Result<_, _>>()?,
        output,
    })
}

// ---- causality ---------------------------------------------------------------------------

/// The prefix lengths (samples) of the causality check for a recording of
/// `total` samples: every [`PREFIX_STEP_S`] step short of the end, then the
/// whole recording.
#[must_use]
pub fn prefix_lengths(total: usize) -> Vec<usize> {
    // A range bounded by `total`, so the loop always ends.
    let mut out: Vec<usize> = (32_000..total).step_by(32_000).collect();
    out.push(total);
    out
}

/// The consecutive [`CHUNK_S`] chunks (sample ranges) of a recording of
/// `total` samples; the last may be shorter.
#[must_use]
pub fn chunk_ranges(total: usize) -> Vec<Range<usize>> {
    (0..total)
        .step_by(16_000)
        .map(|start| start..start.saturating_add(16_000).min(total))
        .collect()
}

/// `spans` moved `offset_s` seconds later.
#[must_use]
pub fn offset_spans(spans: &[Span], offset_s: f64) -> Vec<Span> {
    spans
        .iter()
        .map(|s| Span {
            start_s: s.start_s + offset_s,
            end_s: s.end_s + offset_s,
        })
        .collect()
}

/// One prefix run of the causality check.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PrefixRun {
    /// The prefix length in seconds.
    pub prefix_s: f64,
    /// Every span the run found.
    pub spans: Vec<Span>,
    /// Indices (after sorting by start) of the settled spans that differ
    /// from the offline run or have no partner.
    pub mismatched_span_indices: Vec<usize>,
}

/// The first disagreement found.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Mismatch {
    /// `prefix` or `chunk`.
    pub kind: &'static str,
    /// The prefix length, or the start of the first differing chunk span.
    pub at_s: f64,
    /// The offline spans compared.
    pub offline: Vec<Span>,
    /// The spans observed.
    pub observed: Vec<Span>,
}

/// Whether Silero's spans can be had causally.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Causality {
    /// The recording checked.
    pub recording: String,
    /// The setting checked.
    pub setting: String,
    /// [`PREFIX_STEP_S`].
    pub prefix_step_s: f64,
    /// [`CHUNK_S`].
    pub chunk_s: f64,
    /// [`TOLERANCE_S`].
    pub tolerance_s: f64,
    /// Every prefix run's settled spans match the offline run's.
    pub prefix_stable: bool,
    /// The prefix runs that do not.
    pub prefix_mismatch_count: usize,
    /// The merged chunk spans match the offline spans.
    pub chunks_match_offline: bool,
    /// The offline spans.
    pub offline_span_count: usize,
    /// The chunk spans before merging.
    pub chunk_spans_raw_count: usize,
    /// The chunk spans after merging.
    pub chunk_spans_merged_count: usize,
    /// The largest boundary difference over every paired span.
    pub max_abs_delta_s: f64,
    /// The prefix runs.
    pub prefix_runs: Vec<PrefixRun>,
    /// The chunk spans, on the recording's timeline, before merging.
    pub chunk_spans: Vec<Span>,
    /// The first disagreement.
    pub first_mismatch: Option<Mismatch>,
    /// Same as `prefix_stable`.
    pub causal: bool,
    /// One sentence.
    pub verdict: String,
}

fn sorted(spans: impl IntoIterator<Item = Span>) -> Vec<Span> {
    let mut out: Vec<Span> = spans.into_iter().collect();
    out.sort_by(|a, b| a.start_s.total_cmp(&b.start_s));
    out
}

/// Pairs `expected` with `observed` by index: the mismatched indices and
/// the largest paired boundary difference.
fn compare(expected: &[Span], observed: &[Span]) -> (Vec<usize>, f64) {
    let mut max_delta = 0.0f64;
    let mut bad = Vec::new();
    for i in 0..expected.len().max(observed.len()) {
        match (expected.get(i), observed.get(i)) {
            (Some(e), Some(o)) => {
                let delta = (e.start_s - o.start_s).abs().max((e.end_s - o.end_s).abs());
                max_delta = max_delta.max(delta);
                if delta > TOLERANCE_S {
                    bad.push(i);
                }
            }
            _ => bad.push(i),
        }
    }
    (bad, max_delta)
}

/// `spans` with each gap of at most [`TOLERANCE_S`] closed.
fn merged(spans: &[Span]) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::new();
    for &s in spans {
        match out.last_mut() {
            Some(last) if s.start_s - last.end_s <= TOLERANCE_S => last.end_s = s.end_s,
            _ => out.push(s),
        }
    }
    out
}

/// The causality verdict for `recording` under `setting`: `offline` is the
/// run over the whole recording, `prefix_runs` each growing prefix's length
/// in seconds with its spans, and `chunk_spans` the independent chunks'
/// spans already offset onto the timeline. A prefix of length L is stable
/// when its spans ending before L - 1 s pair one to one (sorted by start)
/// with the offline spans ending before L - 1 s, within [`TOLERANCE_S`];
/// the chunk spans, merged across gaps of at most [`TOLERANCE_S`], are
/// compared the same way with every offline span.
#[must_use]
pub fn causality(
    recording: &str,
    setting: &str,
    offline: &[Span],
    prefix_runs: &[(f64, Vec<Span>)],
    chunk_spans: &[Span],
) -> Causality {
    let offline = sorted(offline.iter().copied());
    let mut max_abs_delta_s = 0.0f64;
    let mut first_mismatch = None;
    let mut runs = Vec::new();
    for (prefix_s, spans) in prefix_runs {
        let settled = |s: &&Span| s.end_s < prefix_s - 1.0;
        let expected = sorted(offline.iter().filter(settled).copied());
        let observed = sorted(spans.iter().filter(settled).copied());
        let (bad, delta) = compare(&expected, &observed);
        max_abs_delta_s = max_abs_delta_s.max(delta);
        if !bad.is_empty() && first_mismatch.is_none() {
            first_mismatch = Some(Mismatch {
                kind: "prefix",
                at_s: *prefix_s,
                offline: expected,
                observed,
            });
        }
        runs.push(PrefixRun {
            prefix_s: *prefix_s,
            spans: spans.clone(),
            mismatched_span_indices: bad,
        });
    }
    let prefix_mismatch_count = runs
        .iter()
        .filter(|r| !r.mismatched_span_indices.is_empty())
        .count();
    let chunk_merged = merged(&sorted(chunk_spans.iter().copied()));
    let (bad, delta) = compare(&offline, &chunk_merged);
    max_abs_delta_s = max_abs_delta_s.max(delta);
    let chunks_match_offline = bad.is_empty();
    if let Some(&i) = bad.first()
        && first_mismatch.is_none()
    {
        let at = offline.get(i).or_else(|| chunk_merged.get(i));
        first_mismatch = Some(Mismatch {
            kind: "chunk",
            at_s: at.map_or(0.0, |s| s.start_s),
            offline: offline.clone(),
            observed: chunk_merged.clone(),
        });
    }
    let prefix_stable = prefix_mismatch_count == 0;
    let verdict = format!(
        "WhisperVadContext {} be run causally on streaming chunks (prefix_stable={prefix_stable}, chunks_match_offline={chunks_match_offline}, max_abs_delta_s={max_abs_delta_s:.3})",
        if prefix_stable { "can" } else { "cannot" }
    );
    Causality {
        recording: recording.to_string(),
        setting: setting.to_string(),
        prefix_step_s: PREFIX_STEP_S,
        chunk_s: CHUNK_S,
        tolerance_s: TOLERANCE_S,
        prefix_stable,
        prefix_mismatch_count,
        chunks_match_offline,
        offline_span_count: offline.len(),
        chunk_spans_raw_count: chunk_spans.len(),
        chunk_spans_merged_count: chunk_merged.len(),
        max_abs_delta_s,
        prefix_runs: runs,
        chunk_spans: chunk_spans.to_vec(),
        first_mismatch,
        causal: prefix_stable,
        verdict,
    }
}

// ---- manifest ------------------------------------------------------------------------------

/// How one recording went.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// Extracted.
    Ok,
    /// Failed; see the error.
    Error,
    /// Stage 2 with every piece skipped.
    Skipped,
}

/// One recording's line in the manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RecordingStatus {
    /// The recording's name.
    pub name: String,
    /// How it went.
    pub status: Status,
    /// The error, when it failed.
    pub error: Option<String>,
    /// The files written for it.
    pub outputs: Vec<String>,
}

/// The run's manifest.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Manifest {
    /// The mode.
    pub mode: Mode,
    /// The provenance.
    #[serde(flatten)]
    pub provenance: Provenance,
    /// [`WHISPER_RS_VERSION`].
    pub whisper_rs_version: &'static str,
    /// The speech model's file name.
    pub model_file: String,
    /// Its SHA-256.
    pub model_sha256: String,
    /// The config as parsed.
    pub config: Config,
    /// When the run started (RFC 3339, UTC).
    pub started_at: String,
    /// When it finished (RFC 3339, UTC).
    pub finished_at: String,
    /// One per recording, in config order.
    pub recordings: Vec<RecordingStatus>,
    /// Recordings with status ok.
    pub ok_count: usize,
    /// Recordings with status error.
    pub error_count: usize,
}

impl Manifest {
    /// The manifest of a run of `config` that produced `recordings`; the
    /// model is `model_file` with SHA-256 `model_sha256`, and the run went
    /// from `times.0` to `times.1`.
    #[must_use]
    pub fn new(
        config: Config,
        provenance: Provenance,
        model: (String, String),
        times: (String, String),
        recordings: Vec<RecordingStatus>,
    ) -> Self {
        let count = |s: Status| recordings.iter().filter(|r| r.status == s).count();
        Self {
            mode: config.mode,
            provenance,
            whisper_rs_version: WHISPER_RS_VERSION,
            model_file: model.0,
            model_sha256: model.1,
            ok_count: count(Status::Ok),
            error_count: count(Status::Error),
            config,
            started_at: times.0,
            finished_at: times.1,
            recordings,
        }
    }

    /// The process's exit code: 2 when a recording failed, else 0.
    #[must_use]
    pub fn exit_code(&self) -> u8 {
        if self.error_count > 0 { 2 } else { 0 }
    }
}

/// Now, as RFC 3339 in UTC to the second.
#[must_use]
pub fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// A file name's final component, for the reports.
#[must_use]
pub fn file_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map_or_else(|| path.to_string(), |n| n.to_string_lossy().into_owned())
}
