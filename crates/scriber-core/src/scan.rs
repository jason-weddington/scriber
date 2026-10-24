//! The example feature: walk a folder and summarise its files by extension.
//!
//! The walk goes through [`Host`](crate::ports::Host), so it is tested
//! against a fake filesystem; progress goes out through
//! [`ProgressSink`](crate::ports::ProgressSink), paced by the
//! [`Clock`](crate::ports::Clock) port so tests never sleep.

use std::collections::BTreeMap;
use std::path::Path;

use crate::model::{ExtensionSummary, ScanProgress, ScanSummary};
use crate::ports::{EntryKind, Ports};

/// The shortest gap between two progress reports. The final totals are
/// always reported, however soon after the last report they arrive.
pub const PROGRESS_INTERVAL_MS: u64 = 100;

/// Why a scan could not run at all. A folder below the root that cannot be
/// read is not an error: it is counted in
/// [`ScanSummary::unreadable_folders`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ScanError {
    /// The folder the user picked could not be listed.
    #[error("{path} could not be read: {reason}")]
    Unreadable {
        /// The folder, as given.
        path: String,
        /// What the filesystem said.
        reason: String,
    },
}

/// Scans `root` and every folder below it.
///
/// Regular files are counted by extension; symbolic links and other special
/// entries are neither counted nor followed. Progress is reported at most
/// once per [`PROGRESS_INTERVAL_MS`], and once more with the final totals.
///
/// # Errors
///
/// [`ScanError::Unreadable`] when the root itself cannot be listed.
pub fn scan(ports: Ports<'_>, root: &Path) -> Result<ScanSummary, ScanError> {
    let started = ports.clock.now_ms();
    let mut last_report = started;
    let mut progress = ScanProgress {
        files: 0,
        bytes: 0,
        folders: 0,
    };
    let mut by_extension: BTreeMap<String, Totals> = BTreeMap::new();
    let mut unreadable_folders: u64 = 0;
    let mut pending = vec![root.to_path_buf()];

    while let Some(folder) = pending.pop() {
        let entries = match ports.host.read_dir(&folder) {
            Ok(entries) => entries,
            Err(e) if folder == root => {
                return Err(ScanError::Unreadable {
                    path: root.display().to_string(),
                    reason: e.to_string(),
                });
            }
            Err(_) => {
                unreadable_folders = unreadable_folders.saturating_add(1);
                continue;
            }
        };
        progress.folders = progress.folders.saturating_add(1);
        for entry in entries {
            match entry.kind {
                EntryKind::File { size } => {
                    progress.files = progress.files.saturating_add(1);
                    progress.bytes = progress.bytes.saturating_add(size);
                    by_extension
                        .entry(extension_of(&entry.name))
                        .or_default()
                        .add(size);
                }
                EntryKind::Folder => pending.push(folder.join(&entry.name)),
                EntryKind::Other => {}
            }
        }
        let now = ports.clock.now_ms();
        if now.saturating_sub(last_report) >= PROGRESS_INTERVAL_MS {
            ports.sink.progress(&progress);
            last_report = now;
        }
    }
    ports.sink.progress(&progress);

    Ok(ScanSummary {
        root: root.display().to_string(),
        files: progress.files,
        total_bytes: progress.bytes,
        unreadable_folders,
        extensions: summarise(by_extension),
        elapsed_ms: ports.clock.now_ms().saturating_sub(started),
    })
}

/// The extension a file is counted under: the text after the last dot,
/// lowercased. A name with no dot, or whose only dot is its first character
/// (`.profile`), has none, and is counted under the empty string.
///
/// ```
/// use scriber_core::scan::extension_of;
///
/// assert_eq!(extension_of("IMG_0001.JPG"), "jpg", "lowercased");
/// assert_eq!(extension_of(".profile"), "", "a leading dot is not an extension");
/// ```
#[must_use]
pub fn extension_of(name: &str) -> String {
    match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => extension.to_lowercase(),
        Some(_) | None => String::new(),
    }
}

/// Running totals for one extension.
#[derive(Debug, Default, Clone, Copy)]
pub struct Totals {
    files: u64,
    bytes: u64,
}

impl Totals {
    /// Counts one more file of `size` bytes.
    pub fn add(&mut self, size: u64) {
        self.files = self.files.saturating_add(1);
        self.bytes = self.bytes.saturating_add(size);
    }
}

/// The per-extension list, in display order: largest total size first, then
/// most files, then extension A to Z.
#[must_use]
pub fn summarise(by_extension: BTreeMap<String, Totals>) -> Vec<ExtensionSummary> {
    let mut list: Vec<ExtensionSummary> = by_extension
        .into_iter()
        .map(|(extension, t)| ExtensionSummary {
            extension,
            files: t.files,
            bytes: t.bytes,
        })
        .collect();
    list.sort_by(|a, b| {
        b.bytes
            .cmp(&a.bytes)
            .then(b.files.cmp(&a.files))
            .then(a.extension.cmp(&b.extension))
    });
    list
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_is_the_lowercased_text_after_the_last_dot() {
        let cases = [
            ("photo.JPG", "jpg"),
            ("archive.tar.gz", "gz"),
            ("Makefile", ""),
            (".profile", ""),
            ("..", ""),
            ("trailing.", ""),
            ("a.b", "b"),
            ("Ünïcode.ÄBC", "äbc"),
        ];
        for (name, want) in cases {
            assert_eq!(extension_of(name), want, "extension of {name:?}");
        }
    }

    #[test]
    fn totals_count_files_and_bytes() {
        let mut t = Totals::default();
        t.add(5);
        t.add(7);
        assert_eq!((t.files, t.bytes), (2, 12), "two files, twelve bytes");
    }

    #[test]
    fn totals_saturate_instead_of_wrapping() {
        let mut t = Totals {
            files: u64::MAX,
            bytes: u64::MAX,
        };
        t.add(1);
        assert_eq!((t.files, t.bytes), (u64::MAX, u64::MAX), "saturated");
    }

    #[test]
    fn summary_order_is_bytes_then_files_then_name() {
        let mut map = BTreeMap::new();
        for (ext, files, bytes) in [
            ("b", 1, 10),
            ("a", 1, 10),
            ("c", 2, 10),
            ("d", 9, 5),
            ("e", 1, 50),
        ] {
            let _: Option<Totals> = map.insert(ext.to_string(), Totals { files, bytes });
        }
        let order: Vec<_> = summarise(map).into_iter().map(|e| e.extension).collect();
        assert_eq!(order, ["e", "c", "a", "b", "d"], "display order");
    }
}
