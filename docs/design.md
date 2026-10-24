# Scriber design

Scriber is a macOS dictation app built on Rust, Tauri 2 and Svelte 5. This file is its spec and the bridge contract between the webview and the Rust core. Change it in the same commit as the behaviour it describes, and when reality disagrees with it, fix whichever one is wrong.

## What it does

The user dictates long instructions to paste into another app (typically a Claude Code session). They click **New** to start a session, **Record**, talk, **Pause**, think, **Resume**, talk more, as many times as they like, then **Done**, or do all of it from the keyboard (⌘N, Space, Return). While recording, a waveform above the buttons shows that the microphone is picking them up. After a brief transcription step the transcript is shown in an editable box and is already on the clipboard, paste-ready. Transcription is fully local (whisper.cpp on Metal); the only network use is downloading the speech model once, when it is missing. There is no global hotkey, no file watcher, no speaker labels and no cloud. Small transcription errors are acceptable; the reader has context to absorb them.

## Architecture

```
crates/scriber-core          all logic; no Tauri, no async runtime, no direct I/O
  src/dictation.rs           the session state machine, jobs (segments, warm-up, download), joining, recovery
  src/audio.rs               resampling to 16 kHz mono, WAV encode/decode
  src/level.rs               the live input level: 50 ms RMS windows in dBFS, the recorder's tap
  src/download.rs            the speech model's download: stream, verify size and SHA-256, rename
  src/sync.rs                a lock that survives a panic; the model that loads once for every caller
  src/transcript.rs          the silence and hallucination filters, paragraph joining
  src/vocabulary.rs          the glossary prompt
  src/store.rs               the data folder's layout (segments, glossary, model path)
  src/model.rs               the values that cross the bridge (serde, camelCase)
  src/ports.rs               Recorder, Transcriber, Files, Clipboard, JobQueue, Downloader
  tests/common               fakes for every port
crates/scriber-engine        the core with the real adapters, as a library for any Rust app (see Engine API); no Tauri, no async runtime
  src/lib.rs                 Engine, EngineConfig, ModelSource, Event, EngineError; the std-thread job queue
  src/device.rs              CpalRecorder: cpal, the default input device
  src/whisper.rs             Whisper: whisper-rs, the model loaded once
  src/files.rs               DataDir: std::fs under the consumer's data folder
  src/net.rs                 HttpDownloader: ureq over native-tls
  tests/common               fakes for the recorder, transcriber and downloader
  examples/smoke.rs          real-model smoke check (scripts/smoke.sh), never run by the gate
apps/desktop/src-tauri       thin client over the engine: the clipboard plugin, the two events, two commands
apps/desktop/src             Svelte 5 frontend; src/lib/bridge.ts is its only door to Rust
```

The rules, each enforced by `scripts/check-architecture.sh` and proven by `scripts/selftest-checks.sh`:

- The core's dependency tree has no Tauri, no webview and no async runtime, and its code reads no file, clock, environment or network directly. Every effect goes through a trait in `ports.rs`; the real implementation lives in `crates/scriber-engine` (the clipboard's in `src-tauri`), the fake in the core's tests.
- The engine's dependency tree has no Tauri, no webview and no async runtime either, so none reaches an app that depends on it.
- `src-tauri` holds `lib.rs` and `main.rs` only, `lib.rs` stays under 250 lines, and it has no tests. Logic that needs a test belongs in the core, and an adapter any app could use in the engine.
- `src/lib/bridge.ts` is the only frontend file that imports `@tauri-apps/*` (also an eslint rule). Outside Tauri it answers from `src/lib/mock.ts`, and marks `<html data-bridge="mock">`; inside Tauri it marks `data-bridge="tauri"`.
- No `#[allow(...)]` anywhere; a lint exception is `#[expect(lint, reason = "...")]`.

### Ports

| Port | Real adapter | What the core asks of it |
|---|---|---|
| `Recorder` | `scriber-engine` `CpalRecorder` (`device.rs`): cpal, the default input device at its default config (f32) | start a segment; stop and hand over interleaved samples with their channel count and rate. Each block the device delivers goes to a core `level::Tap` (through `level::feeder`), which keeps it and meters it |
| `Transcriber` | `scriber-engine` `Whisper` (`whisper.rs`): whisper-rs 0.16, with the `metal` feature on macOS only (the CPU elsewhere, so the workspace builds and gates on Linux) | whether the model file is missing; load the model now (`warm`); samples (16 kHz mono) plus an optional prompt to text |
| `Files` | `scriber-engine` `DataDir` (`files.rs`): `std::fs` under the data folder the consumer gives | read, write (the folder is created), list a folder (a missing one is created and lists empty), remove, append, rename in one step; paths relative to the data folder |
| `Clipboard` | `src-tauri/src/lib.rs`: `tauri-plugin-clipboard-manager` (another consumer passes its own, or none) | write text |
| `JobQueue` | `scriber-engine` (`lib.rs`): one named std thread (`scriber-job`) per job | take a job (a closed segment, the warm-up, the download) off the UI thread; `dictation::work` runs it and reports each snapshot it changes |
| `Downloader` | `scriber-engine` `HttpDownloader` (`net.rs`): ureq 3 over native-tls (Security.framework, the system's roots) | open a URL, following redirects, as a body to read; an error status is an error |

The Whisper context loads once and stays loaded (`sync::LoadOnce`). At launch, when the model is installed, the core queues a warm-up job that loads it (about 7 s on an M-series Mac for `large-v3-turbo`, cold), so it is ready by the time the first segment closes; a transcription that arrives while the warm-up is still loading waits for it under the same lock rather than loading a second copy, and a load that fails is tried again by the next transcription, which reports the failure. A failed warm-up changes nothing in the session. Later segments take well under a second each. Decoding is beam search (beam 5), English, with non-speech tokens suppressed. Each job runs on a std thread of its own, and the Whisper lock lets one transcription run at a time; jobs may finish in any order.

## The data folder

The app data folder is `~/Library/Application Support/com.jasonweddington.scriber/` (Tauri's `app_data_dir`); another app using the engine names its own (`EngineConfig::data_dir`). `crates/scriber-core/src/store.rs` owns its layout:

```
vocabulary.txt                   optional glossary, one term per line
segments/segment-0000.wav        the current session's audio, one file per segment
segments/transcript.txt          the session's transcript, once finished (and after each Copy)
models/ggml-large-v3-turbo.bin   the speech model
```

The model is ggml `large-v3-turbo` from `https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo.bin`, 1,624,555,275 bytes, SHA-256 `1fc70f774d38eb169993ac391eea357ef47c88757ef72ee5943879b7e8e2bc69` (all three pinned in `crates/scriber-core/src/download.rs`).

When the model is missing at launch the app downloads it (`download.rs`): the body streams, 64 KiB at a time, onto `models/ggml-large-v3-turbo.bin.part` (written empty first, so a partial file a crash left starts over; there is no resume), and is renamed to the model's path in one step only once its size and SHA-256 match. A download that fails, is too short or too long, or does not match deletes the partial file, so the transcriber never sees a half-written model. Progress is reported once per whole percent. While it runs, every snapshot carries `download` and Record and Resume are refused ("The speech model is still downloading."). When it fails, `modelError` says "Could not download the speech model: <reason>." and the window offers Retry download (`downloadModel`). Once it succeeds the model warms up and Record turns on, with no notice. If the model goes missing with no download running (deleted while the app is open), `modelError` says "The speech model is missing from <path>." with the same Retry download. `scripts/fetch-model.sh` still downloads it from a terminal (idempotent, `curl -fL`, resumable, checks the same size and SHA-256); the gate never downloads it.

## The session

### States

`crates/scriber-core/src/dictation.rs`, a pure state machine. An action the current phase does not allow returns `DictationError::Illegal` ("Cannot pause while ready.") and changes nothing; the window disables those buttons.

| Phase | Legal actions | Meaning |
|---|---|---|
| `idle` | new | launched, no session |
| `ready` | record, discard | an empty session |
| `recording` | pause, done, discard | the microphone is capturing a segment |
| `paused` | resume, done, discard | between segments; earlier segments may still be transcribing |
| `transcribing` | none | Done was clicked; waiting for the last segments |
| `finished` | new, discard, copy | the transcript is ready |
| `failed` | new, discard, retry | a segment could not be transcribed |

Transitions: New (idle, finished, failed) and Discard (any phase but idle and transcribing) delete the session's audio, re-read the glossary and give an empty `ready` session; Discard while recording stops the microphone and throws the running segment away. Record (ready) and Resume (paused) start the microphone. Pause closes a segment. Done closes the running segment, if any, and moves to `transcribing`, which becomes `finished` or `failed` as soon as no segment is pending (at once when nothing is). Retry (failed) re-reads each failed segment's WAV and transcribes it again; segments already transcribed are kept.

Download model (`downloadModel`) is not part of the phase table: it is allowed in any phase while the model is missing and not downloading, and refused otherwise ("The speech model is already installed or downloading."). A session with no download source (the model is a local file, `Dictation::launch_local`, or a bare `recover`) refuses it first with "The speech model is a local file, so there is nothing to download."

Decided beyond the brief: Discard is refused while idle (there is no session to discard); Discard deletes the session's audio like New does; Record and Resume are refused while the model is missing or downloading, rather than New.

### Segments

Each Pause (and Done while recording) closes a segment: the captured sound is averaged to mono and resampled to 16 kHz by linear interpolation (`audio::to_mono_16k`), saved as `segments/segment-NNNN.wav` (16 kHz mono 16-bit PCM), and queued as a job, so it transcribes while the user thinks and Done only waits for the last one. A job carries its session number; a result from a session that has since been discarded, or for a segment that is no longer pending, is ignored. The transcript joins the segments' text in recording order, whatever order they finish in, with a blank line between segments (a pause is usually a thought boundary).

The recorded time is the length of the audio of the closed segments (`recordedMs`); while recording, the window adds the running segment's time from its own clock.

### Input level

`crates/scriber-core/src/level.rs`, constants pinned by tests. The recorder adapter hands every sample block to the core's tap and computes nothing itself. The core takes the RMS of each 50 ms window of sound (`WINDOW_MS`, all channels' samples counted), converts it to dBFS and maps it linearly from -60 dBFS (`FLOOR_DBFS`, 0.0) to 0 dBFS (1.0), clamped; silence and a non-number read 0. Decibels, not linear RMS, because speech around -40 to -20 dBFS would draw as a nearly flat line on a linear scale. A -30 dBFS signal reads 0.5 and a full-scale sine (RMS -3 dBFS) about 0.95. One reading per window, and at most one per delivered block (the loudest window it completed), so the level event runs at no more than 20 a second. Each reading is sent outside the tap's lock.

### Filters

In `crates/scriber-core/src/transcript.rs`, all constants pinned by tests:

- **Silence.** A segment is silent when no 100 ms window (1,600 samples) reaches an RMS of 0.01 (-40 dBFS). A silent segment is never sent to the transcriber. Windowing keeps a few words in a long quiet segment from being averaged away.
- **Hallucinations.** Whisper invents text for silence and noise. A segment shorter than 10 s whose whole text, trimmed and ignoring ASCII case, is one of "Thank you.", "Thank you", "Thank you very much.", "Thanks for watching!", "Thanks for watching.", "Thank you for watching.", "Thank you for watching!", "Bye.", "you", "[BLANK_AUDIO]", "(silence)" or "Subtitles by the Amara.org community" is treated as silent. The same words inside longer text, or in a segment of 10 s or more, are kept.
- Text is trimmed; empty text is treated as silent.

### Glossary

`vocabulary.txt` in the data folder, read at each New and Discard (and at launch for a recovered session). One term per line; surrounding whitespace is trimmed; blank lines and lines whose first non-blank character is `#` are ignored; control characters are removed from terms. The prompt is `Glossary: term1, term2, ...`, built in file order while it stays within 500 characters; the first term that would overflow ends the list. No terms, a missing file, an unreadable file or one that is not UTF-8 gives no prompt. In the smoke check the prompt turned "Talus" into "talos" and "Agent GTD" into "agent-gtd".

### Delivery

On `finished` the core writes the transcript to the clipboard and the notice says "Copied to the clipboard." If the clipboard cannot be written the transcript is still shown and the notice says "Could not copy to the clipboard: <reason>. Use Copy to try again." If every segment was silent the transcript is empty, nothing is copied, and the notice says "No speech was heard. If you spoke, check that Scriber may use the microphone in System Settings, Privacy & Security, Microphone." (macOS delivers silence, not an error, when the permission is denied.) Copy sends the text as edited in the box; the core copies it and keeps it as the transcript.

### Failure and crash safety

Failures are a visible state, never a silent empty transcript. A segment whose transcription fails, or that the queue refuses, makes the session `failed` once nothing is pending, with `error` naming the first failed part: "Part 2 of 3 could not be transcribed: <reason>. Retry to try again." Retry reads that part's WAV back; if it cannot be read the part fails again with "its audio could not be read: <reason>".

The session's WAV files stay on disk until the next New or Discard, and a finished session's transcript is kept beside them (`segments/transcript.txt`, rewritten by each Copy; a failure to write it only means a relaunch offers Retry instead). At launch the core reads that folder: with a transcript the session starts `finished`, showing it (nothing is copied again); with segments but no transcript (a crash, or a quit while transcribing) it starts `failed` with "The last session was not transcribed. Retry to transcribe its N recorded parts, or start a new session." Quitting drops the microphone stream and any transcription in flight; the audio survives for that Retry.

A microphone that will not start refuses Record or Resume ("Could not start recording: <reason>") and leaves the session where it was. A microphone that fails while recording loses only that segment: Pause still pauses, and the notice says "The microphone stopped: <reason>. The last part was not recorded." A segment whose WAV cannot be saved still transcribes from memory, with the notice "Could not keep this part's audio for Retry: <reason>."

## Engine API

`crates/scriber-engine` is the whole capability as a library: another Rust app depends on it (as a git dependency) and the desktop app is one client of it. It has no Tauri and no async runtime; jobs run on std threads.

- **`EngineConfig`**: `data_dir` (the consumer's data folder, laid out as in The data folder; it need not exist), `model: ModelSource` and `clipboard: Option<Arc<dyn Clipboard + Send + Sync>>`. `EngineConfig::new(data_dir)` takes the default model and no clipboard.
- **`ModelSource`**: `Local(path)` uses a ggml model file where it is and never downloads; a missing file says "The speech model is missing from <path>." and `download_model()` is refused with `DictationError::LocalModel`. `Download(Source { url, bytes, sha256 })` downloads into `<data_dir>/models/ggml-large-v3-turbo.bin` when missing and verifies size and SHA-256, so a mirror only needs its own URL. The default is `Download(Source::default())`: Hugging Face `large-v3-turbo`, pinned above.
- **Opening**: `Engine::open(config, events)` with the real adapters, or `Engine::with_adapters(config, make, events)` with others (`make` gets the level sink to hand to its recorder). Either recovers the data folder as the app does at launch and starts the warm-up, or the download, on a job thread. The session is assigned under its lock before any job it queued can record an outcome.
- **`Event`**: `Level(f64)` (0 to 1, at most 20 a second while recording) and `State(Snapshot)` (whatever `dictation-changed` carries). No method emits events: the snapshot a method returns is its result.
- **Threading**: `Event::State` is delivered on a job thread while the session lock is held, so the callback must never call the engine (it deadlocks); forward it to a channel. `Event::Level` is delivered on the audio thread and must return quickly. `Engine` is `Send + Sync` and `Clone`; dropping the last handle while no job runs drops the adapters.
- **Recording**: `record()`, `pause()`, `resume()`, `stop()` (Done), `discard()`, `retry()`, `download_model()`, and `act(action, text)` for any action. Record then stop, with no pause, is the primary path. `record()` from idle or finished starts a new session first (deleting the last one); it never applies New from failed, which would delete audio not yet transcribed: it refuses ("Cannot record while failed."), and the consumer retries or discards.
- **Waiting**: `wait(timeout)` answers a finished session's text at once, a failed session's error as `EngineError::Failed`, `NotStopped` while idle, ready, recording or paused, and while transcribing waits on a condition variable paired with the session lock, until the deadline (`Timeout`). `stop_and_wait(timeout)` is `stop()` then `wait`. A finished session recovered at open answers its old text until `record()` starts a new one.
- **Clipboard**: with `clipboard: None` the transcript is copied nowhere, and the snapshot's notice still says "Copied to the clipboard." (pinned by a test).
- **Not yet**: no streaming or partial transcripts. `Event` is not `#[non_exhaustive]` today, but a streaming mode would add a variant (and a mode), not change these.

## Bridge contract

Tauri camel-cases argument names. `tests/e2e/contract.spec.ts` reads `src-tauri/src/lib.rs` and fails when a command, an argument or the event name disagrees with `bridge.ts`. `crates/scriber-core/tests/fixtures/bridge-shapes.json` holds an example of every JSON shape; the core's serde tests and the mock's honesty test both check against it.

### Commands

| Command | Arguments | Returns | Errors |
|---|---|---|---|
| `dictation_state` | none | `Snapshot` | never |
| `dictation_act` | `action: Action`, `text: string` | `Snapshot` after the action (`downloadModel` included) | the core's refusal, as one sentence (see above) |

`text` is the edited transcript for `copy` and is ignored (the bridge sends `""`) for every other action. Errors arrive in the webview as a rejected promise carrying one readable sentence. The app's version comes from Tauri's own `getVersion()` (`@tauri-apps/api/app`), not a command of ours.

### Events

| Event | Payload | When |
|---|---|---|
| `dictation-changed` | `Snapshot` | a job changed the snapshot: a segment's transcription finished (or failed), whether or not the phase changed; the model's download moved another whole percent, finished or failed; the warm-up finished (unchanged) |
| `dictation-level` | `number` (0 to 1) | while recording, one per 50 ms window of sound (at most 20 a second); see Input level |

Commands and events can arrive out of order (a segment can finish before the command that queued it has answered), so the window keeps the snapshot with the highest `revision`.

### JSON shapes

```ts
type Phase = 'idle' | 'ready' | 'recording' | 'paused' | 'transcribing' | 'finished' | 'failed';
type Action = 'new' | 'record' | 'pause' | 'resume' | 'done' | 'discard' | 'retry' | 'copy' | 'downloadModel';

interface Download {
  receivedBytes: number;
  totalBytes: number; // the model's size
}

interface Snapshot {
  revision: number; // grows with every applied action and every accepted result
  phase: Phase;
  recordedMs: number; // audio in the closed segments
  segments: number; // segments recorded in this session
  pending: number; // of those, still transcribing
  text: string; // the transcript once finished, else ""
  error: string | null; // why the session failed, when phase is "failed"
  notice: string | null; // one sentence about the last action
  modelError: string | null; // why the model is missing, unless it is downloading
  download: Download | null; // the model's download, while it runs
}
```

Every object is exactly these fields: the Rust types refuse unknown and missing fields.

## The mock

`src/lib/mock.ts` runs the core's state machine in the browser over a fake microphone and transcriber, deterministically. Segment N is heard as "This is part N of the dictation."; durations come from `Date.now()`, so Playwright's clock controls the recorded time. While recording it emits a synthetic, speech-like level every 50 ms, and stops on Pause, Done and Discard. URL parameters pick the scenario: `?model=download` (the model is missing and downloads at launch in four steps), `?model=fail` (the same, but the first download fails half way with "the connection was reset"; Retry download succeeds), `?mic=silent` (every segment silent), `?transcribe=fail` (each segment's first transcription fails, so Retry succeeds), `?clipboard=fail`, `?leftover=<n>` (launch finds n segments of an unfinished session), `?long=1` (long paragraphs, to overflow the box) and `?fail=<call>` (`dictation_state`, `app_version` or an action rejects). A test can hold `dictation_act`, `dictation_state`, `transcribe` or `download` (each step of the download) with `window.__mockBridge.hold(name)`, or from launch with `?hold=<name>`, and let it go with `release`; `window.__mockBridge.copied` lists every clipboard write and `window.__mockBridge.acts` every action the window asked for.

## UI

One window with an overlay title bar. The top band (which drags the window) holds the "Dictation" heading, the recorded time (red while the microphone is live) and New. Below it: while the model downloads, a determinate progress bar with "Downloading the speech model, needed once: 42% (680 MB of 1.6 GB). Record works when it is done." (status is shown only when the user must change what they do); the model error as an alert with Retry download when it applies; a status line (`aria-live="polite"`) saying what to do next or how the last action went; the failure or refusal as an alert; the waveform while recording or paused; and the buttons Record, Pause, Resume, Done and Discard (plus Retry when failed), each enabled only when the core would accept it and while no action is in flight. When finished, an editable "Transcript" box fills the rest of the window and scrolls inside itself, with Copy beside its label. Light and dark follow the system. `tests/e2e/a11y.spec.ts` runs axe over every state in both.

**Waveform.** A scrolling bar waveform above the buttons, like Voice Memos: 48 bars (`BARS` in `src/lib/format.ts`, 2.4 s at 20 levels a second), the newest on the right, older bars moving left, each bar's height its level. It shows while recording; while paused it stays, frozen and dimmed (levels that arrive after Pause are dropped); leaving recording and paused (Done, New, Discard) clears it. Under `prefers-reduced-motion: reduce` one level bar showing the latest level replaces the scrolling bars. Both are `aria-hidden`; a visually hidden line says whether the microphone is picking up sound (or is off while paused).

**Keyboard.** In-window shortcuts, not global hotkeys: ⌘N is New; Space is Record from ready, Pause from recording and Resume from paused; Return is Done from recording or paused. A key does nothing (and sends nothing) in a phase whose action it is not, while an action is in flight, and on its auto-repeats. Space and Return type in the transcript and any other text field, where ⌘N still works. Each handled key cancels its default, and the Space keyup after it too, so a button that merely has focus from a click is not clicked as well; a button reached with Tab keeps Space and Return for itself, and fires once. A focused button that is left disabled once an action settles hands the focus back to the window, or it would swallow every key after it. Each button with a shortcut shows it ("⌘N", "Space", "⏎", `aria-hidden`) and names it in `aria-keyshortcuts`. Tauri's default macOS menu (which the app keeps) binds ⌘Q, ⌘W, ⌘H, ⌥⌘H, ⌘M, ⌃⌘F and the Edit items, not ⌘N, and WKWebView gives ⌘N no meaning, so the webview receives it.

## Bundle

`src-tauri/Info.plist` (merged into the bundle's) carries `NSMicrophoneUsageDescription`, so macOS asks for the microphone the first time Record is clicked. `src-tauri/Entitlements.plist` grants `com.apple.security.device.audio-input`, which the hardened runtime requires for microphone input.
