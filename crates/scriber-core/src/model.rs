//! The values the core hands to the app. Each one crosses the bridge as JSON,
//! so its serde shape is part of the contract in `docs/design.md`: field
//! names are camelCase, and unknown fields are refused rather than dropped.
//! `tests/serde_shapes.rs` pins every shape.

use serde::{Deserialize, Serialize};

/// What a finished scan found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScanSummary {
    /// The folder that was scanned, as the user picked it.
    pub root: String,
    /// Every regular file under the root, at any depth.
    pub files: u64,
    /// The sum of those files' sizes in bytes.
    pub total_bytes: u64,
    /// Folders below the root that could not be read, so their contents are
    /// missing from the totals.
    pub unreadable_folders: u64,
    /// One entry per extension, largest total size first (see
    /// [`crate::scan::summarise`] for the full order).
    pub extensions: Vec<ExtensionSummary>,
    /// Wall-clock time the scan took, from the clock port.
    pub elapsed_ms: u64,
}

/// The files of one extension.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExtensionSummary {
    /// Lowercase, without the dot. Empty for a file with no extension.
    pub extension: String,
    /// How many files have it.
    pub files: u64,
    /// Their total size in bytes.
    pub bytes: u64,
}

/// A running count, sent while a scan is in progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScanProgress {
    /// Files counted so far.
    pub files: u64,
    /// Their total size in bytes so far.
    pub bytes: u64,
    /// Folders read so far, the root included.
    pub folders: u64,
}

/// What the app reports about itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppInfo {
    /// The app's version, from its manifest.
    pub version: String,
}
