//! The core's view of the outside world. Each trait is one kind of effect;
//! the app crate supplies the real implementations and the tests supply
//! fakes (`tests/common`). Nothing in the core touches the filesystem, the
//! clock or the webview except through these.

use std::path::Path;

use crate::model::ScanProgress;

/// One entry of a folder listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    /// The entry's file name (the last path component).
    pub name: String,
    /// What kind of entry it is.
    pub kind: EntryKind,
}

/// The kinds of entry a scan distinguishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    /// A regular file, with its size in bytes.
    File {
        /// Size in bytes.
        size: u64,
    },
    /// A folder, which the scan descends into.
    Folder,
    /// Anything else (a symbolic link, a socket, a device). Not counted and
    /// never followed, so a link cycle cannot make a scan loop.
    Other,
}

/// A failed filesystem read, as one readable sentence.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct HostError(pub String);

/// Filesystem reads.
pub trait Host {
    /// Lists one folder without following symbolic links.
    ///
    /// # Errors
    ///
    /// The folder does not exist, is not a folder, or cannot be read.
    fn read_dir(&self, path: &Path) -> Result<Vec<DirEntry>, HostError>;
}

/// Monotonic time.
pub trait Clock {
    /// Milliseconds since an arbitrary fixed point. Never goes backwards.
    fn now_ms(&self) -> u64;
}

/// Where progress goes (the app forwards it to the webview as an event).
pub trait ProgressSink {
    /// Called with the running totals while a scan is in progress.
    fn progress(&self, progress: &ScanProgress);
}

/// Every port a scan needs, borrowed for the length of one call.
#[derive(Clone, Copy)]
pub struct Ports<'a> {
    /// Filesystem reads.
    pub host: &'a dyn Host,
    /// Time, for progress pacing and the elapsed figure.
    pub clock: &'a dyn Clock,
    /// Progress reports.
    pub sink: &'a dyn ProgressSink,
}
