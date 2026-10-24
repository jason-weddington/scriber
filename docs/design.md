# Scriber design

Scriber is a macOS desktop app built on Rust, Tauri 2 and Svelte 5. This file is its spec and the bridge contract between the webview and the Rust core. Change it in the same commit as the behaviour it describes, and when reality disagrees with it, fix whichever one is wrong.

The app ships with one example feature, the folder summary, which exists to show every seam working. Replace it with your own (see `CLAUDE.md`, "Replacing the example feature") and rewrite this file to match.

## Architecture

```
crates/scriber-core          all logic; no Tauri, no async runtime, no direct I/O
  src/model.rs               the values that cross the bridge (serde, camelCase)
  src/ports.rs               one trait per kind of effect: Host, Clock, ProgressSink
  src/scan.rs                the example feature
  tests/common               fakes for every port
apps/desktop/src-tauri       thin adapter: real port implementations, one command per contract entry
apps/desktop/src             Svelte 5 frontend; src/lib/bridge.ts is its only door to Rust
```

The rules, each enforced by `scripts/check-architecture.sh` and proven by `scripts/selftest-checks.sh`:

- The core's dependency tree has no Tauri, no webview and no async runtime, and its code reads no file, clock, environment or network directly. Every effect goes through a trait in `ports.rs`; the real implementation lives in `src-tauri`, the fake in the core's tests.
- `src-tauri` holds `lib.rs` and `main.rs` only, `lib.rs` stays under 250 lines, and it has no tests. Logic that needs a test belongs in the core.
- `src/lib/bridge.ts` is the only frontend file that imports `@tauri-apps/*` (also an eslint rule). Outside Tauri it answers from `src/lib/mock.ts`, and marks `<html data-bridge="mock">`; inside Tauri it marks `data-bridge="tauri"`.
- No `#[allow(...)]` anywhere; a lint exception is `#[expect(lint, reason = "...")]`.

## The example feature: folder summary

The user chooses a folder. The core walks it through the `Host` port and returns, for each file extension, how many files have it and their total size, largest first. Progress arrives as events while the walk runs, and the result list scrolls however long it is.

Rules (crates/scriber-core/src/scan.rs, mirrored in the mock and checked by `tests/unit/mock.spec.ts`):

- Only regular files are counted. Symbolic links and other special entries are neither counted nor followed, so a link cycle cannot make a scan loop.
- A file's extension is the text after its last dot, lowercased. A name with no dot, or whose only dot is its first character (`.profile`), has none and is counted under the empty string, shown as "No extension".
- Order: total bytes descending, then file count descending, then extension A to Z.
- A folder below the root that cannot be read is counted in `unreadableFolders` and skipped. A root that cannot be read is an error.
- Progress is reported at most once per 100 ms of the clock port, and always once more with the final totals.
- Totals saturate at the largest `u64` rather than wrapping.

## Bridge contract

Tauri camel-cases argument names: a Rust parameter `path` is sent as `path`, `remove_after` as `removeAfter`. `tests/e2e/contract.spec.ts` reads `src-tauri/src/lib.rs` and fails when a command, an argument or the event name disagrees with `bridge.ts`. `crates/scriber-core/tests/fixtures/bridge-shapes.json` holds one example of every JSON shape; the core's serde tests and the mock's honesty test both check against it.

### Commands

| Command | Arguments | Returns | Errors |
|---|---|---|---|
| `app_info` | none | `AppInfo` | never |
| `scan_folder` | `path: string` | `ScanSummary` | the root cannot be read: `"<path> could not be read: <reason>"` |

Errors arrive in the webview as a rejected promise carrying one readable sentence.

The folder picker is the dialog plugin's `open({ directory: true, multiple: false })`, called from `bridge.ts`; it resolves to the chosen path or to `null` when the user cancels.

### Events

| Event | Payload | When |
|---|---|---|
| `scan-progress` | `ScanProgress` | during `scan_folder`, at most every 100 ms, and once with the final totals |

### JSON shapes

```ts
interface AppInfo {
  version: string; // the app's version from its manifest; "0.0.0-mock" from the mock
}

interface ScanProgress {
  files: number; // files counted so far
  bytes: number; // their total size so far
  folders: number; // folders read so far, the root included
}

interface ScanSummary {
  root: string; // the folder as chosen
  files: number;
  totalBytes: number;
  unreadableFolders: number;
  extensions: ExtensionSummary[]; // in display order
  elapsedMs: number;
}

interface ExtensionSummary {
  extension: string; // lowercase, no dot; "" for none
  files: number;
  bytes: number;
}
```

Every object is exactly these fields: the Rust types refuse unknown and missing fields.

## The mock

`src/lib/mock.ts` answers every bridge call in the browser, deterministically, from an in-memory tree chosen by URL parameter: `?tree=photos` (the default: two JPEGs, two DNGs, a note, a file with no extension, a link and an unreadable folder), `many` (40 extensions, to overflow the list), `empty`, or `locked` (the root cannot be read). `?pick=cancel` makes the picker return `null`; `?fail=<command>` makes that command reject. A test can hold a command mid-call with `window.__mockBridge.hold('scan_folder')` and let it finish with `release`.

## UI

One window with an overlay title bar: a top band (which drags the window) holds the heading and the "Choose folder…" button, inset past the traffic lights. Below it a status line (`aria-live="polite"`) and the list of extensions, which is the scrolling element. Light and dark follow the system. `tests/e2e/a11y.spec.ts` runs axe over every state in both.
