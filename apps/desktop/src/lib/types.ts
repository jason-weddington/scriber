// The bridge contract's JSON shapes (docs/design.md). The Rust side is
// crates/scriber-core/src/model.rs; crates/scriber-core/tests/fixtures/bridge-shapes.json
// holds one example of each, checked against both sides.

export interface AppInfo {
  version: string;
}

export interface ScanProgress {
  files: number;
  bytes: number;
  folders: number;
}

export interface ExtensionSummary {
  /** Lowercase, without the dot; empty for a file with no extension. */
  extension: string;
  files: number;
  bytes: number;
}

export interface ScanSummary {
  root: string;
  files: number;
  totalBytes: number;
  unreadableFolders: number;
  /** Largest total size first, then most files, then extension A to Z. */
  extensions: ExtensionSummary[];
  elapsedMs: number;
}
