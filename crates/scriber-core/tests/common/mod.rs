//! Fakes for every port: an in-memory filesystem, a clock that advances
//! instantly, and a sink that records what it was sent.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use scriber_core::model::ScanProgress;
use scriber_core::ports::{Clock, DirEntry, EntryKind, Host, HostError, Ports, ProgressSink};

/// A filesystem held in a map from folder path to its listing (or to the
/// error reading it returns). A path that is not in the map does not exist.
#[derive(Default)]
pub(crate) struct FakeHost {
    folders: BTreeMap<PathBuf, Result<Vec<DirEntry>, String>>,
}

impl FakeHost {
    /// Adds a folder with these entries.
    pub(crate) fn folder(mut self, path: &str, entries: &[(&str, EntryKind)]) -> Self {
        let listing = entries
            .iter()
            .map(|(name, kind)| DirEntry {
                name: (*name).to_string(),
                kind: *kind,
            })
            .collect();
        let _: Option<_> = self.folders.insert(PathBuf::from(path), Ok(listing));
        self
    }

    /// Adds a folder that fails to read with `reason`.
    pub(crate) fn unreadable(mut self, path: &str, reason: &str) -> Self {
        let _: Option<_> = self
            .folders
            .insert(PathBuf::from(path), Err(reason.to_string()));
        self
    }
}

impl Host for FakeHost {
    fn read_dir(&self, path: &Path) -> Result<Vec<DirEntry>, HostError> {
        match self.folders.get(path) {
            Some(Ok(entries)) => Ok(entries.clone()),
            Some(Err(reason)) => Err(HostError(reason.clone())),
            None => Err(HostError("No such file or directory".to_string())),
        }
    }
}

/// A file entry of `size` bytes.
pub(crate) fn file(size: u64) -> EntryKind {
    EntryKind::File { size }
}

/// A clock that moves forward by `step` milliseconds every time it is read,
/// so a scan's pacing is exercised without any real waiting.
pub(crate) struct FakeClock {
    now: Cell<u64>,
    step: u64,
}

impl FakeClock {
    /// Starts at `start` and advances `step` per read.
    pub(crate) fn new(start: u64, step: u64) -> Self {
        Self {
            now: Cell::new(start),
            step,
        }
    }
}

impl Clock for FakeClock {
    fn now_ms(&self) -> u64 {
        let now = self.now.get();
        self.now.set(now.saturating_add(self.step));
        now
    }
}

/// Records every progress report.
#[derive(Default)]
pub(crate) struct Recorder(RefCell<Vec<ScanProgress>>);

impl Recorder {
    /// Everything reported so far.
    pub(crate) fn reports(&self) -> Vec<ScanProgress> {
        self.0.borrow().clone()
    }
}

impl ProgressSink for Recorder {
    fn progress(&self, progress: &ScanProgress) {
        self.0.borrow_mut().push(*progress);
    }
}

/// Borrows the three fakes as the core's ports.
pub(crate) fn ports<'a>(host: &'a FakeHost, clock: &'a FakeClock, sink: &'a Recorder) -> Ports<'a> {
    Ports { host, clock, sink }
}
