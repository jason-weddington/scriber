//! Sound in the form the transcriber wants: 16 kHz mono `f32`, and the WAV
//! files a session keeps for Retry (16 kHz, mono, 16-bit PCM).

use crate::ports::Captured;

/// The transcriber's sample rate, in samples per second.
pub const SAMPLE_RATE: u32 = 16_000;

/// [`SAMPLE_RATE`] as a `u64`, for the resampler's index arithmetic.
const RATE: u64 = 16_000;

/// [`SAMPLE_RATE`] as an `f32`, for the interpolation fraction.
const RATE_F32: f32 = 16_000.0;

/// Interleaved device sound as 16 kHz mono: each frame's channels averaged,
/// then resampled by linear interpolation between neighbouring frames. A
/// trailing partial frame is dropped; no channels or a zero rate give nothing.
#[must_use]
pub fn to_mono_16k(captured: &Captured) -> Vec<f32> {
    let channels = usize::from(captured.channels);
    if channels == 0 {
        return Vec::new();
    }
    let per_frame = f32::from(captured.channels);
    let mono: Vec<f32> = captured
        .samples
        .chunks_exact(channels)
        .map(|frame| frame.iter().sum::<f32>() / per_frame)
        .collect();
    let rate = u64::from(captured.sample_rate);
    let frames = u64::try_from(mono.len()).unwrap_or(u64::MAX);
    let out_len = frames.saturating_mul(RATE).checked_div(rate).unwrap_or(0);
    (0..out_len)
        .map(|i| {
            let position = i.saturating_mul(rate);
            let at = usize::try_from(position / RATE).unwrap_or(usize::MAX);
            let before = mono.get(at).copied().unwrap_or_default();
            let after = mono.get(at.saturating_add(1)).copied().unwrap_or(before);
            let fraction = u16::try_from(position % RATE).map_or(0.0, f32::from) / RATE_F32;
            before + (after - before) * fraction
        })
        .collect()
}

/// A sample as 16-bit PCM: clamped to -1..1, scaled by 32767 and rounded.
#[expect(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    reason = "the value is clamped and rounded into i16's range first, and a float-to-int `as` saturates"
)]
fn to_pcm(sample: f32) -> i16 {
    (sample.clamp(-1.0, 1.0) * 32767.0).round() as i16
}

/// The fixed part of a canonical 44-byte PCM header.
const HEADER_LEN: u32 = 36;

/// `samples` as a WAV file: RIFF, 16 kHz, mono, 16-bit PCM.
#[must_use]
pub fn encode_wav(samples: &[f32]) -> Vec<u8> {
    let data_len = u32::try_from(samples.len())
        .unwrap_or(u32::MAX)
        .saturating_mul(2);
    let mut out = Vec::with_capacity(samples.len().saturating_mul(2).saturating_add(44));
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&HEADER_LEN.saturating_add(data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    out.extend_from_slice(&SAMPLE_RATE.saturating_mul(2).to_le_bytes()); // bytes per second
    out.extend_from_slice(&2u16.to_le_bytes()); // bytes per frame
    out.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for sample in samples {
        out.extend_from_slice(&to_pcm(*sample).to_le_bytes());
    }
    out
}

/// Why a WAV file could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WavError {
    /// It does not start with a RIFF WAVE header.
    #[error("not a WAV file")]
    NotWav,
    /// A chunk runs past the end of the file.
    #[error("the WAV file is truncated")]
    Truncated,
    /// There is no `fmt ` chunk, or no `data` chunk.
    #[error("the WAV file has no {0} chunk")]
    MissingChunk(&'static str),
    /// Anything but 16 kHz mono 16-bit PCM.
    #[error("the WAV file is not 16 kHz mono 16-bit PCM")]
    Unsupported,
}

fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(*bytes.get(at..)?.first_chunk()?))
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(*bytes.get(at..)?.first_chunk()?))
}

/// The samples of a 16 kHz mono 16-bit PCM WAV file, as `f32` (each
/// 16-bit value divided by 32768). Chunks other than `fmt ` and `data` are
/// skipped, so files from other tools (ffmpeg's LIST chunk) read too.
///
/// # Errors
///
/// The bytes are not such a file; see [`WavError`].
pub fn decode_wav(bytes: &[u8]) -> Result<Vec<f32>, WavError> {
    if bytes.get(0..4) != Some(b"RIFF".as_slice()) || bytes.get(8..12) != Some(b"WAVE".as_slice()) {
        return Err(WavError::NotWav);
    }
    let mut format = None;
    let mut data = None;
    let mut at = 12usize;
    while at < bytes.len() {
        let id = bytes
            .get(at..at.saturating_add(4))
            .ok_or(WavError::Truncated)?;
        let size = u32_at(bytes, at.saturating_add(4)).ok_or(WavError::Truncated)?;
        let start = at.saturating_add(8);
        let end = start.saturating_add(usize::try_from(size).map_err(|_| WavError::Truncated)?);
        let body = bytes.get(start..end).ok_or(WavError::Truncated)?;
        match id {
            b"fmt " => format = Some(body),
            b"data" => data = Some(body),
            _ => {}
        }
        // Chunks are padded to an even length.
        at = end.saturating_add(end % 2);
    }
    let format = format.ok_or(WavError::MissingChunk("fmt"))?;
    let data = data.ok_or(WavError::MissingChunk("data"))?;
    let pcm_mono_16k = u16_at(format, 0) == Some(1)
        && u16_at(format, 2) == Some(1)
        && u32_at(format, 4) == Some(SAMPLE_RATE)
        && u16_at(format, 14) == Some(16);
    if !pcm_mono_16k {
        return Err(WavError::Unsupported);
    }
    Ok(data
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| f32::from(i16::from_le_bytes(*pair)) / 32768.0)
        .collect())
}

/// Milliseconds of sound in `samples` samples at [`SAMPLE_RATE`].
#[must_use]
pub fn duration_ms(samples: usize) -> u64 {
    u64::try_from(samples)
        .unwrap_or(u64::MAX)
        .saturating_mul(1_000)
        / RATE
}
