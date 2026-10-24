//! Downloading the speech model when it is missing.
//!
//! The model streams into [`PARTIAL`] beside where it belongs, one block at
//! a time through the [`Downloader`] port, and is renamed to
//! [`store::MODEL`] only once its size and SHA-256 match [`Source`]. A
//! download that fails, or that does not match, deletes the partial file:
//! the transcriber never sees a half-written model.

use std::io::ErrorKind;

use sha2::{Digest, Sha256};

use crate::ports::{Downloader, Files, PortError, fail};
use crate::store;

/// Where the model is downloaded from.
pub const MODEL_URL: &str =
    "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo.bin";

/// The model's size in bytes.
pub const MODEL_BYTES: u64 = 1_624_555_275;

/// The model's SHA-256, in lowercase hex.
pub const MODEL_SHA256: &str = "1fc70f774d38eb169993ac391eea357ef47c88757ef72ee5943879b7e8e2bc69";

/// Where a download in progress is written, until it is verified.
pub const PARTIAL: &str = "models/ggml-large-v3-turbo.bin.part";

/// A file to download and what it must turn out to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    /// Where it is.
    pub url: String,
    /// Its exact size in bytes.
    pub bytes: u64,
    /// Its SHA-256, in lowercase hex.
    pub sha256: String,
}

impl Default for Source {
    /// The speech model.
    fn default() -> Self {
        Self {
            url: MODEL_URL.to_string(),
            bytes: MODEL_BYTES,
            sha256: MODEL_SHA256.to_string(),
        }
    }
}

/// Downloads `source` to [`store::MODEL`], calling `progress` with the bytes
/// received each time another whole percent has arrived.
///
/// # Errors
///
/// Why the model is not in place, as a phrase ("it stopped after 10 of 20
/// bytes"); the partial file has been deleted.
pub fn fetch(
    source: &Source,
    files: &dyn Files,
    downloader: &dyn Downloader,
    progress: &dyn Fn(u64),
) -> Result<(), String> {
    // Writing it empty starts over any partial file a crash left.
    let verified = files
        .write(PARTIAL, &[])
        .and_then(|()| stream(source, files, downloader, progress))
        .map_err(|e| e.to_string())
        .and_then(|(received, sha256)| {
            if received != source.bytes {
                Err(format!(
                    "it stopped after {received} of {} bytes",
                    source.bytes
                ))
            } else if sha256 != source.sha256 {
                Err("its checksum does not match".to_string())
            } else {
                files
                    .rename(PARTIAL, store::MODEL)
                    .map_err(|e| e.to_string())
            }
        });
    if verified.is_err() {
        let _removed = files.remove(PARTIAL);
    }
    verified
}

/// How much of the body is read at a time.
const BLOCK: usize = 1 << 16;

/// Streams `source` onto the end of [`PARTIAL`]. Returns the bytes received
/// and their SHA-256 in lowercase hex.
fn stream(
    source: &Source,
    files: &dyn Files,
    downloader: &dyn Downloader,
    progress: &dyn Fn(u64),
) -> Result<(u64, String), PortError> {
    let mut body = downloader.open(&source.url)?;
    let mut buffer = vec![0u8; BLOCK];
    let mut hasher = Sha256::new();
    let mut received = 0u64;
    let mut percent = 0u64;
    loop {
        let bytes = match body.read(&mut buffer) {
            Ok(n) => buffer.get(..n).unwrap_or_default(),
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => return Err(fail(e)),
        };
        if bytes.is_empty() {
            return Ok((received, format!("{:x}", hasher.finalize())));
        }
        let length = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        received = received.saturating_add(length);
        if received > source.bytes {
            return Err(PortError("it is larger than expected".to_string()));
        }
        hasher.update(bytes);
        files.append(PARTIAL, bytes)?;
        let now = received.saturating_mul(100).checked_div(source.bytes);
        if now != Some(percent) {
            percent = now.unwrap_or(percent);
            progress(received);
        }
    }
}
