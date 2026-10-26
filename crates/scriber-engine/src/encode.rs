//! AAC encoding through macOS's `afconvert`, for a library entry's audio.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use scriber_core::ports::{Encoder, PortError, fail};

/// Where macOS keeps `afconvert`. Other systems have none, so encoding
/// fails there and the library keeps the WAV.
pub const AFCONVERT: &str = "/usr/bin/afconvert";

/// `afconvert` at `binary`, with paths relative to the data folder at
/// `root`.
pub struct Afconvert {
    binary: PathBuf,
    root: PathBuf,
}

impl Afconvert {
    /// The encoder at `binary` for the data folder at `root`.
    #[must_use]
    pub fn new(binary: PathBuf, root: PathBuf) -> Self {
        Self { binary, root }
    }

    /// The arguments that encode `input` as 32 kbit/s AAC in an M4A file at
    /// `output`.
    #[must_use]
    pub fn args(input: &Path, output: &Path) -> Vec<OsString> {
        let mut args: Vec<OsString> = ["-f", "m4af", "-d", "aac", "-b", "32000"]
            .into_iter()
            .map(OsString::from)
            .collect();
        args.push(input.into());
        args.push(output.into());
        args
    }
}

impl Encoder for Afconvert {
    fn to_m4a(&self, wav: &str, m4a: &str) -> Result<(), PortError> {
        let args = Self::args(&self.root.join(wav), &self.root.join(m4a));
        let output = Command::new(&self.binary)
            .args(args)
            .output()
            .map_err(fail)?;
        if output.status.success() {
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(PortError(format!(
                "afconvert failed ({}): {}",
                output.status,
                stderr.trim()
            )))
        }
    }
}
