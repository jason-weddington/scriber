# Scriber design

Scriber is a macOS dictation app built on Rust, Tauri 2 and Svelte 5. This file is its spec and the bridge contract between the webview and the Rust core. Change it in the same commit as the behaviour it describes, and when reality disagrees with it, fix whichever one is wrong.

## What it does

The user dictates long instructions to paste into another app (typically a Claude Code session). They click **New** to start a session, **Record**, talk, **Pause**, think, **Resume**, talk more, as many times as they like, then **Done**. After a brief transcription step the transcript is shown in an editable box and is already on the clipboard, paste-ready. Transcription is fully local (whisper.cpp on Metal). There is no global hotkey, no file watcher, no speaker labels and no cloud. Small transcription errors are acceptable; the reader has context to absorb them.

## Architecture

```
crates/scriber-core          all logic; no Tauri, no async runtime, no direct I/O
  src/dictation.rs           the session state machine, segment jobs, joining, recovery
  src/audio.rs               resampling to 16 kHz mono, WAV encode/decode
  src/transcript.rs          the silence and hallucination filters, paragraph joining
  src/vocabulary.rs          the glossary prompt
  src/store.rs               the data folder's layout (segments, glossary, model path)
  src/model.rs               the values that cross the bridge (serde, camelCase)
  src/ports.rs               Recorder, Transcriber, Files, Clipboard, JobQueue
  tests/common               fakes for every port
apps/desktop/src-tauri       thin adapter: cpal, whisper-rs, the filesystem, the clipboard plugin, two commands
  examples/smoke.rs          real-model smoke check (scripts/smoke.sh), never run by the gate
apps/desktop/src             Svelte 5 frontend; src/lib/bridge.ts is its only door to Rust
```

The rules, each enforced by `scripts/check-architecture.sh` and proven by `scripts/selftest-checks.sh`:

- The core's dependency tree has no Tauri, no webview and no async runtime, and its code reads no file, clock, environment or network directly. Every effect goes through a trait in `ports.rs`; the real implementation lives in `src-tauri`, the fake in the core's tests.
- `src-tauri` holds `lib.rs` and `main.rs` only, `lib.rs` stays under 250 lines, and it has no tests. Logic that needs a test belongs in the core.
- `src/lib/bridge.ts` is the only frontend file that imports `@tauri-apps/*` (also an eslint rule). Outside Tauri it answers from `src/lib/mock.ts`, and marks `<html data-bridge="mock">`; inside Tauri it marks `data-bridge="tauri"`.
- No `#[allow(...)]` anywhere; a lint exception is `#[expect(lint, reason = "...")]`.

### Ports

| Port | Real adapter (`src-tauri/src/lib.rs`) | What the core asks of it |
|---|---|---|
| `Recorder` | cpal, the default input device at its default config (f32) | start a segment; stop and hand over interleaved samples with their channel count and rate |
| `Transcriber` | whisper-rs 0.16 with the `metal` feature | whether the model file is missing; samples (16 kHz mono) plus an optional prompt to text |
| `Files` | `std::fs` under the app data folder | read, write, list a folder (a missing one is created and lists empty), remove; paths relative to the data folder |
| `Clipboard` | `tauri-plugin-clipboard-manager` | write text |
| `JobQueue` | `tauri::async_runtime::spawn_blocking` per job | take a closed segment's job to transcribe off the UI thread |

The Whisper context loads on the first transcription (about 7 s on an M-series Mac for `large-v3-turbo`, cold) and stays loaded; later segments take well under a second each. Decoding is beam search (beam 5), English, with non-speech tokens suppressed. Each job runs on Tauri's blocking pool, and the Whisper lock lets one transcription run at a time; jobs may finish in any order.

## The data folder

The app data folder is `~/Library/Application Support/com.jasonweddington.scriber/` (Tauri's `app_data_dir`). `crates/scriber-core/src/store.rs` owns its layout:

```
vocabulary.txt                   optional glossary, one term per line
segments/segment-0000.wav        the current session's audio, one file per segment
segments/transcript.txt          the session's transcript, once finished (and after each Copy)
models/ggml-large-v3-turbo.bin   the speech model
```

The model is ggml `large-v3-turbo` from the `ggerganov/whisper.cpp` Hugging Face repository (1,624,555,275 bytes). `scripts/fetch-model.sh` downloads it there if it is missing (idempotent, `curl -fL`, resumable, refuses a file under 1.5 GB); the gate never downloads it. When it is missing, every snapshot carries `modelError`: "Model missing: run scripts/fetch-model.sh to download it to <path>." The window shows it at once and Record is refused.

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

Decided beyond the brief: Discard is refused while idle (there is no session to discard); Discard deletes the session's audio like New does; Record and Resume are refused while the model is missing, rather than New.

### Segments

Each Pause (and Done while recording) closes a segment: the captured sound is averaged to mono and resampled to 16 kHz by linear interpolation (`audio::to_mono_16k`), saved as `segments/segment-NNNN.wav` (16 kHz mono 16-bit PCM), and queued as a job, so it transcribes while the user thinks and Done only waits for the last one. A job carries its session number; a result from a session that has since been discarded, or for a segment that is no longer pending, is ignored. The transcript joins the segments' text in recording order, whatever order they finish in, with a blank line between segments (a pause is usually a thought boundary).

The recorded time is the length of the audio of the closed segments (`recordedMs`); while recording, the window adds the running segment's time from its own clock.

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

## Bridge contract

Tauri camel-cases argument names. `tests/e2e/contract.spec.ts` reads `src-tauri/src/lib.rs` and fails when a command, an argument or the event name disagrees with `bridge.ts`. `crates/scriber-core/tests/fixtures/bridge-shapes.json` holds an example of every JSON shape; the core's serde tests and the mock's honesty test both check against it.

### Commands

| Command | Arguments | Returns | Errors |
|---|---|---|---|
| `dictation_state` | none | `Snapshot` | never |
| `dictation_act` | `action: Action`, `text: string` | `Snapshot` after the action | the core's refusal, as one sentence (see above) |

`text` is the edited transcript for `copy` and is ignored (the bridge sends `""`) for every other action. Errors arrive in the webview as a rejected promise carrying one readable sentence. The app's version comes from Tauri's own `getVersion()` (`@tauri-apps/api/app`), not a command of ours.

### Events

| Event | Payload | When |
|---|---|---|
| `dictation-changed` | `Snapshot` | a segment's transcription finished (or failed), whether or not the phase changed |

Commands and events can arrive out of order (a segment can finish before the command that queued it has answered), so the window keeps the snapshot with the highest `revision`.

### JSON shapes

```ts
type Phase = 'idle' | 'ready' | 'recording' | 'paused' | 'transcribing' | 'finished' | 'failed';
type Action = 'new' | 'record' | 'pause' | 'resume' | 'done' | 'discard' | 'retry' | 'copy';

interface Snapshot {
  revision: number; // grows with every applied action and every accepted result
  phase: Phase;
  recordedMs: number; // audio in the closed segments
  segments: number; // segments recorded in this session
  pending: number; // of those, still transcribing
  text: string; // the transcript once finished, else ""
  error: string | null; // why the session failed, when phase is "failed"
  notice: string | null; // one sentence about the last action
  modelError: string | null; // set while the model is missing
}
```

Every object is exactly these fields: the Rust types refuse unknown and missing fields.

## The mock

`src/lib/mock.ts` runs the core's state machine in the browser over a fake microphone and transcriber, deterministically. Segment N is heard as "This is part N of the dictation."; durations come from `Date.now()`, so Playwright's clock controls the recorded time. URL parameters pick the scenario: `?model=missing`, `?mic=silent` (every segment silent), `?transcribe=fail` (each segment's first transcription fails, so Retry succeeds), `?clipboard=fail`, `?leftover=<n>` (launch finds n segments of an unfinished session), `?long=1` (long paragraphs, to overflow the box) and `?fail=<call>` (`dictation_state`, `app_version` or an action rejects). A test can hold `dictation_act`, `dictation_state` or `transcribe` with `window.__mockBridge.hold(name)` and let it go with `release`; `window.__mockBridge.copied` lists every clipboard write.

## UI

One window with an overlay title bar. The top band (which drags the window) holds the "Dictation" heading, the recorded time (red while the microphone is live) and New. Below it: the model-missing alert when it applies, a status line (`aria-live="polite"`) saying what to do next or how the last action went, the failure or refusal as an alert, and the buttons Record, Pause, Resume, Done and Discard (plus Retry when failed), each enabled only when the core would accept it and while no action is in flight. When finished, an editable "Transcript" box fills the rest of the window and scrolls inside itself, with Copy beside its label. Light and dark follow the system. `tests/e2e/a11y.spec.ts` runs axe over every state in both.

## Bundle

`src-tauri/Info.plist` (merged into the bundle's) carries `NSMicrophoneUsageDescription`, so macOS asks for the microphone the first time Record is clicked. `src-tauri/Entitlements.plist` grants `com.apple.security.device.audio-input`, which the hardened runtime requires for microphone input.
