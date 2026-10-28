//! Opening the glossary in the user's text editor through macOS's `open`.

use std::ffi::OsString;
use std::fs::OpenOptions;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Command;

use scriber_core::ports::{PortError, fail};

/// Where macOS keeps `open`. Other systems have none, so opening fails
/// there (after the file is created).
pub const OPEN: &str = "/usr/bin/open";

/// A file opener at `binary` that takes `-t <path>`, as macOS's `open` does:
/// the file opens in the app registered for plain text.
pub struct TextEditor {
    binary: PathBuf,
}

impl TextEditor {
    /// The opener at `binary`.
    #[must_use]
    pub fn new(binary: PathBuf) -> Self {
        Self { binary }
    }

    /// The arguments that open `path` in the default text editor.
    #[must_use]
    pub fn args(path: &Path) -> Vec<OsString> {
        vec![OsString::from("-t"), path.into()]
    }

    /// Opens `path` in the default text editor, first creating it (and its
    /// folder) empty when it is missing. An existing file is never
    /// truncated, and a file created here stays even when the editor fails.
    ///
    /// # Errors
    ///
    /// The folder or the file cannot be created (the editor is then not
    /// run), the editor cannot be started, or it exits with a failure.
    pub fn open(&self, path: &Path) -> Result<(), PortError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(fail)?;
        }
        let created = OpenOptions::new().write(true).create_new(true).open(path);
        if let Err(e) = created
            && e.kind() != ErrorKind::AlreadyExists
        {
            return Err(fail(e));
        }
        let output = Command::new(&self.binary)
            .args(Self::args(path))
            .output()
            .map_err(fail)?;
        if output.status.success() {
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(PortError(format!(
                "open failed ({}): {}",
                output.status,
                stderr.trim()
            )))
        }
    }
}
