# Scriber design

Scriber is a macOS dictation app built on Rust, Tauri 2 and Svelte 5. This file is its spec and the bridge contract between the webview and the Rust core. Change it in the same commit as the behaviour it describes, and when reality disagrees with it, fix whichever one is wrong.

## What it does

The user dictates long instructions to paste into another app (typically a Claude Code session). They click **New** to start a session, **Record**, talk, **Pause**, think, **Resume**, talk more, as many times as they like, then **Done**, or do all of it from the keyboard (⌘N, Space, Return). While recording, a waveform above the buttons shows that the microphone is picking them up. After a brief transcription step the transcript is shown in an editable box and is already on the clipboard, paste-ready. Every finished dictation is also kept in a recording library, a drawer on the left of the window, titled by when it started and summarized in one sentence by a local model (Ollama) when one is running. Transcription is fully local (whisper.cpp on Metal); the only network use is downloading the speech model once, when it is missing, and asking the local Ollama server (127.0.0.1) for summaries and, when the user turns it on, a cleanup pass over each transcript. There is no global hotkey, no file watcher, no speaker labels and no cloud. Small transcription errors are acceptable; the reader has context to absorb them.

## Architecture

```
crates/scriber-core          all logic; no Tauri, no async runtime, no direct I/O
  src/dictation.rs           the session state machine, jobs (segments, warm-up, download), joining, recovery
  src/audio.rs               resampling to 16 kHz mono, WAV encode/decode
  src/level.rs               the live input level: 50 ms RMS windows in dBFS, the recorder's tap
  src/download.rs            the speech model's download: stream, verify size and SHA-256, rename
  src/sync.rs                a lock that survives a panic; the model that loads once for every caller
  src/transcript.rs          the silence, hallucination and no-letter filters, paragraph joining
  src/vocabulary.rs          the transcriber prompt: style sentence plus glossary; the glossary's terms
  src/cleanup.rs             the optional cleanup pass: its prompt and request, and the check on the model's reply
  src/store.rs               the data folder's layout (segments, glossary, model path)
  src/library.rs             the recording library: titles and ids, excerpts, summary cleaning, the archive, listing
  src/settings.rs            the user's settings in settings.json
  src/model.rs               the values that cross the bridge (serde, camelCase)
  src/ports.rs               Recorder, Transcriber, Files, Clipboard, JobQueue, Downloader, Clock, Encoder, Summarizer, Cleaner
  tests/common               fakes for every port
crates/scriber-engine        the core with the real adapters, as a library for any Rust app (see Engine API); no Tauri, no async runtime
  src/lib.rs                 Engine, EngineConfig, ModelSource, Event, EngineError; the std-thread job queue
  src/device.rs              CpalRecorder: cpal, the default input device
  src/whisper.rs             Whisper: whisper-rs, the model loaded once
  src/files.rs               DataDir: std::fs under the consumer's data folder
  src/net.rs                 HttpDownloader: ureq over native-tls
  src/clock.rs               LocalClock: chrono, the local wall-clock time
  src/encode.rs              Afconvert: macOS's /usr/bin/afconvert, WAV to AAC in M4A
  src/ollama.rs              Ollama: ureq POST to a local Ollama server's /api/chat
  src/editor.rs              TextEditor: macOS's /usr/bin/open -t, the glossary in the default text editor
  tests/common               fakes for the recorder, transcriber, downloader, clock, encoder, summarizer and cleaner
  examples/smoke.rs          real-model smoke check (scripts/smoke.sh), never run by the gate
  examples/exit_check.rs     Mac exit check (scripts/exit-check.sh), never run by the gate: exit() after shutdown must not abort
apps/desktop/src-tauri       thin client over the engine: the clipboard and dialog plugins, the two events, eight commands
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
| `Clock` | `scriber-engine` `LocalClock` (`clock.rs`): `chrono::Local::now()` | the local wall-clock time (year, month, day, hour, minute, second), to title a library entry |
| `Encoder` | `scriber-engine` `Afconvert` (`encode.rs`): runs `/usr/bin/afconvert -f m4af -d aac -b 32000 <in.wav> <out.m4a>` with both paths under the data folder | encode a WAV file as AAC in M4A; a missing binary (every non-macOS host) or a non-zero exit is an error |
| `Summarizer` | `scriber-engine` `Ollama` (`ollama.rs`): ureq 3 POST to `<ollama_url>/api/chat`, 60 s overall timeout; a reply whose `done_reason` is `length` is an error (the entry keeps its excerpt) | a transcript's one-sentence summary, the model's raw `message.content` (the core cleans it) |
| `Cleaner` | `scriber-engine` `Ollama` (`ollama.rs`): the same ureq 3 POST to `<ollama_url>/api/chat` with `summary_model`, a 120 s overall timeout (`CLEANUP_TIMEOUT`); a reply whose `done_reason` is `length` is an error | a system prompt and a request to the model's raw `message.content` (the core judges it, `cleanup::accept`) |

`Clock`, `Encoder` and `Summarizer` are only used with the library on: they travel together in `ports::Library`, which `Ports::with_library` adds; `Ports::new` leaves it `None`. `Cleaner` is not in `ports::Library`: `Ports::with_cleaner` adds it whether or not the library is on (the engine always does), and `Ports::new` leaves it `None`, which never cleans up. The gate never needs Ollama or afconvert: the core's tests fake every port, the Ollama adapter is tested against an in-process HTTP stub, and the afconvert adapter against stub scripts.

The Whisper context loads once and stays loaded (`sync::LoadOnce`). At launch, when the model is installed, the core queues a warm-up job that loads it (about 7 s on an M-series Mac for `large-v3-turbo`, cold), so it is ready by the time the first segment closes; a transcription that arrives while the warm-up is still loading waits for it under the same lock rather than loading a second copy, and a load that fails is tried again by the next transcription, which reports the failure. A failed warm-up changes nothing in the session. Later segments take well under a second each. Decoding is beam search (beam 5), English, with non-speech tokens suppressed. Each job runs on a std thread of its own, and the Whisper lock lets one transcription run at a time; jobs may finish in any order.

## The data folder

The app data folder is `~/Library/Application Support/com.jasonweddington.scriber/` (Tauri's `app_data_dir`); another app using the engine names its own (`EngineConfig::data_dir`). `crates/scriber-core/src/store.rs` owns its layout:

```
vocabulary.txt                   optional glossary, one term per line (the default; settings.json can name another file)
settings.json                    the user's settings: {"summaries": bool, "library_open": bool, "vocabulary": string, "cleanup": bool}
segments/segment-0000.wav        the current session's audio, one file per segment
segments/transcript.txt          the session's transcript, once finished (and after each Copy)
segments/started.txt             the session's start as a title (2026-10-26 14:03:09), with the library on
library/<id>/entry.json          a library entry: {"id","title","summary","summary_source","cleanup",["cleanup_error",]"duration_ms"}
library/<id>/transcript.txt      its transcript
library/<id>/transcript.raw.txt  Whisper's transcript, when an accepted cleanup changed it
library/<id>/cleanup.reply.txt   the cleanup model's rejected reply, verbatim
library/<id>/audio.m4a           its audio as AAC (audio.wav until it is encoded, or when encoding fails)
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
| `transcribing` | none | Done was clicked; waiting for the last segments (and the cleanup, when on) |
| `finished` | new, discard, copy | the transcript is ready |
| `failed` | new, discard, retry | a segment could not be transcribed |

Transitions: New (idle, finished, failed) and Discard (any phase but idle and transcribing) delete the session's audio, re-read the glossary and give an empty `ready` session; Discard while recording stops the microphone and throws the running segment away. Record (ready) and Resume (paused) start the microphone. Pause closes a segment. Done closes the running segment, if any, and moves to `transcribing`, which becomes `finished` or `failed` as soon as no segment is pending (at once when nothing is) or, with cleanup on and a non-empty transcript, `finished` once the cleanup job answers (see Cleanup). Retry (failed) re-reads each failed segment's WAV and transcribes it again; segments already transcribed are kept.

Download model (`downloadModel`) is not part of the phase table: it is allowed in any phase while the model is missing and not downloading, and refused otherwise ("The speech model is already installed or downloading."). A session with no download source (the model is a local file, `Dictation::launch_local`, or a bare `recover`) refuses it first with "The speech model is a local file, so there is nothing to download."

Decided beyond the brief: Discard is refused while idle (there is no session to discard); Discard deletes the session's audio like New does; Record and Resume are refused while the model is missing or downloading, rather than New.

### Segments

Each Pause (and Done while recording) closes a segment: the captured sound is averaged to mono and resampled to 16 kHz by linear interpolation (`audio::to_mono_16k`), saved as `segments/segment-NNNN.wav` (16 kHz mono 16-bit PCM), and queued as a job, so it transcribes while the user thinks and Done only waits for the last one. A job carries its session number; a result from a session that has since been discarded, or for a segment that is no longer pending, is ignored. The transcript joins the segments' text in recording order, whatever order they finish in, with a blank line between segments (a pause is usually a thought boundary).

The recorded time is the length of the audio of the closed segments (`recordedMs`); while recording, the window adds the running segment's time from its own clock.

### Input level

`crates/scriber-core/src/level.rs`, constants pinned by tests. The recorder adapter hands every sample block to the core's tap and computes nothing itself. The core takes the RMS of each 50 ms window of sound (`WINDOW_MS`, all channels' samples counted), converts it to dBFS and maps it linearly from -52 dBFS (`FLOOR_DBFS`, 0.0) to -20 dBFS (`CEILING_DBFS`, 1.0), clamped; silence and a non-number read 0. Decibels, not linear RMS, because speech would draw as a nearly flat line on a linear scale. The ceiling is deliberately well below full scale: on a MacBook mic room silence is about -52 dBFS, dictation about -41 to -33 dBFS and the loudest about -27, so a 0 dBFS ceiling would draw speech at half height or less, and -20 leaves headroom above quiet-room dictation for louder voices in large rooms. -36 dBFS reads 0.5, -52 dBFS (room silence) and below 0.0, -40 dBFS 0.375, -33 dBFS about 0.59, -27 dBFS (the loudest measured) about 0.78, and anything at or above -20 dBFS (a full-scale sine included) 1.0. One reading per window, and at most one per delivered block (the loudest window it completed), so the level event runs at no more than 20 a second. Each reading is sent outside the tap's lock.

### Filters

In `crates/scriber-core/src/transcript.rs`, all constants pinned by tests:

- **Silence.** A segment is silent when no 100 ms window (1,600 samples) reaches an RMS of 0.01 (-40 dBFS). A silent segment is never sent to the transcriber. Windowing keeps a few words in a long quiet segment from being averaged away.
- **Hallucinations.** Whisper invents text for silence and noise. A segment shorter than 10 s whose whole text, trimmed and ignoring ASCII case, is one of "Thank you.", "Thank you", "Thank you very much.", "Thanks for watching!", "Thanks for watching.", "Thank you for watching.", "Thank you for watching!", "Bye.", "you", "[BLANK_AUDIO]", "(silence)" or "Subtitles by the Amara.org community" is treated as silent. The same words inside longer text, or in a segment of 10 s or more, are kept.
- **No letters or digits.** Text is trimmed; empty text, and text with no letter or digit (no char for which `char::is_alphanumeric` is true), is treated as silent at any segment length. Measured 2026-10-25: a 1.5 s noise thump above the silence level transcribed as "you" without a prompt and as "." with the style prompt.

### Glossary

The glossary file named by `vocabulary` in `settings.json` (an absolute path), else `vocabulary.txt` in the data folder, read from that path at each New and Discard (and at launch for a recovered session). One term per line; surrounding whitespace is trimmed; blank lines and lines whose first non-blank character is `#` are ignored; control characters are removed from terms. The prompt is the style sentence `Okay, here's what I need. First, check the board; then, once it's done, let me know.` followed, when there are terms, by one space and `Glossary: term1, term2, ...` with a period after the last term, built in file order while the whole prompt stays within 500 characters; the first term that would overflow ends the list. The style sentence uses part of that budget, leaving 404 characters for the terms, separators and trailing period after `Glossary: `, and terms past it are dropped in file order. No terms, a missing file, an unreadable file or one that is not UTF-8 gives the style sentence alone. Measured 2026-10-25 on a 44 s segment with ggml-large-v3-turbo: without a prompt long run-on stretches lost periods and commas, the style sentence plus a glossary restored them, and the style sentence alone produced "gonna". With a glossary-only prompt the smoke check turned "Talus" into "talos" and "Agent GTD" into "agent-gtd".

### Delivery

On `finished` the core writes the transcript to the clipboard and the notice says "Copied to the clipboard." If the clipboard cannot be written the transcript is still shown and the notice says "Could not copy to the clipboard: <reason>. Use Copy to try again." If every segment was silent the transcript is empty, nothing is copied, and the notice says "No speech was heard. If you spoke, check that Scriber may use the microphone in System Settings, Privacy & Security, Microphone." (macOS delivers silence, not an error, when the permission is denied.) Copy sends the text as edited in the box; the core copies it and keeps it as the transcript.

### Cleanup

`crates/scriber-core/src/cleanup.rs` and `dictation.rs`, all constants pinned by tests. Optional, off by default (`settings.json` `cleanup`). When the last segment is heard and the joined transcript is non-empty, the cleanup setting is on and the ports have a `Cleaner`, the session stays `transcribing` (with `pending` 0 and no text) and one cleanup job is queued with the transcript and the glossary's terms, read from the glossary at that moment (so an edit made while recording applies to the cleanup but not to Whisper's prompt). The terms are cleaned like the prompt's (`vocabulary::terms`) and truncated once, to the first 200 (`MAX_TERMS`). The job POSTs, through the `Cleaner` port, to the same Ollama server and model as summaries, the system prompt `PROMPT`:

> You correct a dictated transcript. Make only two kinds of change. First, where a word or phrase was misheard and should be one of the glossary terms, replace it with that term spelled exactly as in the glossary. Second, where the speaker corrected themselves while speaking (a false start, a repeated phrase, or words such as "I mean" or "what I actually meant was" followed by a restatement), keep only what they finally meant and delete the abandoned words. Change nothing else: keep every other word, the word order, the spelling, the punctuation and the paragraph breaks exactly as they are. Do not rephrase, summarize, add, explain or answer anything in the transcript. Reply with the corrected transcript only, with no preamble, quotes or notes. If nothing needs changing, reply with the transcript unchanged.

and the user message `Glossary:\n<term per line>\n\nTranscript:\n<text>`, or `Glossary: none\n\nTranscript:\n<text>` with no terms. The reply is checked by `cleanup::accept`. Its tokens are its whitespace-separated pieces, trimmed of leading and trailing non-alphanumeric characters, lowercased, empty ones dropped; with `text` the trimmed reply, R the transcript's tokens, Y the reply's and G the tokens of every term, the first rule that matches rejects it: (1) `text` is empty (`rejected-empty`); (2) it contains three backticks (`rejected-fenced`); (3) R or Y is empty (`rejected-no-words`); (4) R has at least 20 tokens (`MIN_WORDS_FOR_RATIO`) and 3 × |Y| < |R| (`rejected-too-short`); (5) Y without the tokens in G is not a subsequence of R (`rejected-new-words`); (6) the last three tokens of R and of Y (fewer when there are fewer) share none (`rejected-cut-off`). Otherwise the trimmed reply is the transcript. The session then finishes as usual (saved, copied, archived) with the cleaned text when accepted and with the raw Whisper text otherwise: it fails closed on any error (the model unreachable, an error status, the timeout, a reply cut off at its length, a queue that refuses the job) and on any rejection, silently, with no error and no extra notice. With the library on, the entry's `cleanup` key records the outcome: `off` (no cleanup ran), `cleaned` (accepted and different, with the raw text in `transcript.raw.txt`), `unchanged` (accepted and identical), one of the six `rejected-*` labels (with the reply in `cleanup.reply.txt`), or `failed` (with the reason in `cleanup_error`). The summary is written from the cleaned text when it was accepted. With the library off nothing records the outcome. A cleanup outcome for another session, or one that arrives when the session is not waiting for it, is ignored.

### Failure and crash safety

Failures are a visible state, never a silent empty transcript. A segment whose transcription fails, or that the queue refuses, makes the session `failed` once nothing is pending, with `error` naming the first failed part: "Part 2 of 3 could not be transcribed: <reason>. Retry to try again." Retry reads that part's WAV back; if it cannot be read the part fails again with "its audio could not be read: <reason>".

The session's WAV files stay on disk until the next New or Discard, and a finished session's transcript is kept beside them (`segments/transcript.txt`, rewritten by each Copy; a failure to write it only means a relaunch offers Retry instead). With the library on, a finished session with a non-empty transcript moves its audio into `library/<id>/` (see The library) and keeps only `segments/transcript.txt` and `segments/started.txt`, so a relaunch shows it as finished with no parts. At launch the core reads that folder: with a transcript the session starts `finished`, showing it (nothing is copied again); with segments but no transcript (a crash, or a quit while transcribing) it starts `failed` with "The last session was not transcribed. Retry to transcribe its N recorded parts, or start a new session." Quitting shuts the engine down (`Engine::shutdown`), which saves the open segment's audio and lets a transcription already decoding finish; if its result is saved before the process exits the next launch shows it, otherwise it offers Retry with the audio intact. A quit or crash during cleanup leaves the session not transcribed (its transcript is saved only on finishing), so the next launch offers Retry, which transcribes and cleans it up again; shutdown does not wait for a cleanup job. The transcript is written atomically (to `segments/transcript.txt.partial`, then renamed), so a relaunch never sees a truncated one.

A microphone that will not start refuses Record or Resume ("Could not start recording: <reason>") and leaves the session where it was. A microphone that fails while recording loses only that segment: Pause still pauses, and the notice says "The microphone stopped: <reason>. The last part was not recorded." A segment whose WAV cannot be saved still transcribes from memory, with the notice "Could not keep this part's audio for Retry: <reason>."

## The library

`crates/scriber-core/src/library.rs`, with the archive run from `dictation.rs`. Off by default in the engine (`EngineConfig::library`), so an app embedding it keeps no audio it did not ask for; the desktop app turns it on. With it off the core writes nothing under `library/`, writes no `segments/started.txt`, queues no encode or summary and leaves `libraryRevision` at 0.

**Session start.** The session's first Record (from `ready`) reads the clock and keeps the title `YYYY-MM-DD HH:MM:SS` (24-hour, local time) in memory and in `segments/started.txt` (a write failure is ignored). New and Discard forget it. At launch, `started.txt` is used when its trimmed contents are exactly 19 characters shaped `DDDD-DD-DD DD:DD:DD`; otherwise the entry is titled by the clock when it is archived. So a crashed session finished later by Retry keeps the time it was recorded.

**Archive.** When a session becomes `finished` with a non-empty transcript (after the transcript is saved and copied), synchronously under the session lock, all through the `Files` port: (1) the id is the title without `-` and `:` and with the space as `-` (`20261026-140309`); when a folder in `library/` already has that name, `-2`, `-3` and so on are tried. (2) Every stored segment is read in order, unreadable ones skipped, and their samples are joined into `library/<id>/audio.wav` (not written when none was read). (3) `transcript.txt` is written, then `transcript.raw.txt` when an accepted cleanup changed the text, then `cleanup.reply.txt` when a cleanup reply was rejected; a failed write fails the archive. (4) `entry.json` is written to `entry.json.partial` and renamed into place, last, so an entry without one never lists: `{"id","title","summary","summary_source","cleanup",["cleanup_error",]"duration_ms"}` in that order, compact, with the excerpt as the summary, `summary_source` `"excerpt"`, `cleanup` the cleanup outcome (see Cleanup) and `cleanup_error` only when the cleanup failed. (5) Only when 1 to 4 succeeded are the segment WAVs removed (a failure to remove one is ignored). (6) `libraryRevision` and `revision` are bumped. (7) An encode job is queued when `audio.wav` was written, and a summary job when `settings.json` has summaries on. If any of 1 to 4 fails, every segment WAV stays, no `entry.json` exists, nothing is bumped and the session is exactly as finished left it. A session that heard nothing, a failed one and a discarded one are never archived; Retry that later finishes is archived then. `libraryRevision` starts at 0 at launch and is never reset by New or Discard. `duration_ms` and `summary_source` are written but nothing reads them yet.

**Encode.** Off the transcription path, on a job thread: `audio.wav` is encoded to `audio.m4a` (measured 2026-10-26: a 43.9 s segment went from 1,403,948 bytes of WAV to 186,402 of 32 kbit/s AAC, and decoded back to 43.872 s). On success `audio.wav` is removed; on failure (afconvert missing, as on every non-macOS host, or failing) any `audio.m4a` is removed and `audio.wav` kept: audio is never lost.

**Excerpt.** Every whitespace run (`char::is_whitespace`, newlines included) becomes one space and the ends are trimmed. Text of 30 characters or fewer is the excerpt whole. Otherwise the first 30 characters are taken, cut at the last space among them when that space is at character index 20 or later, trailing whitespace and `,;:-.` are trimmed, and `...` is appended. Examples (each pinned in the core and the mock): "This is part 1 of the dictation." gives "This is part 1 of the..."; "Okay, here's what I need. First, check the board; ..." gives "Okay, here's what I need..."; "Supercalifragilisticexpialidocious is long" gives "Supercalifragilisticexpialidoc..."; "Abcdefghijklmnopqrstuvwxyzabc, more" gives "Abcdefghijklmnopqrstuvwxyzabc..."; 19 `a`, a space and 20 `b` keeps 10 `b` (the space at index 19 is too early to cut at); 20 `a`, a space and 20 `b` gives 20 `a` and `...`. Characters are counted, not bytes.

**Summary.** Off the transcription path, on a job thread of its own, so a slow or hung Ollama never delays transcribing or archiving the next session. The engine POSTs to `<ollama_url>/api/chat` (default `http://127.0.0.1:11434`, model `qwen3.8:27b-mlx`) `{"model": <summary_model>, "stream": false, "think": false, "messages": [{"role": "system", "content": "You write a one-sentence summary of a dictated note, at most 15 words, plain text, no quotes, no preamble."}, {"role": "user", "content": <transcript>}]}` with a 60 s overall timeout; `think: false` keeps qwen from spending ~300 tokens thinking (measured 2026-10-26 with ollama 0.40.6: about 2 s cold, 0.5 s warm). The core collapses the reply's `message.content` to one line, strips one pair of surrounding straight or curly double quotes, and writes it into `entry.json` as `summary` with `summary_source` `"ollama"` (through `entry.json.partial` and a rename, every other key kept), then bumps `libraryRevision` and `revision`. Any failure keeps the excerpt silently: connection refused, a non-2xx status, the timeout, invalid JSON, no `message.content`, or content empty after cleaning. A summary that lands after New or Discard still updates its entry: it is keyed by id, not by session. Failed summaries are not retried, and turning summaries back on does not summarize older entries.

**Settings.** `settings.json` in the data folder, `{"summaries": bool, "library_open": bool, "vocabulary": string, "cleanup": bool}` in that key order, compact; `summaries` and `library_open` true by default, `cleanup` false (a missing key or one that is not a bool means false). A missing, unreadable or invalid file means the defaults; each key is read on its own (a missing key or one of another type takes its default; unknown keys are ignored). `vocabulary` is the glossary file's absolute path; empty, missing, not a string or relative (not starting with `/`) means the default, `vocabulary.txt` in the data folder, stored as `""`. The engine answers the default as its absolute path, so over the bridge `vocabulary` is never empty. On write every field is replaced (`cleanup` included, like `summaries`), except that an empty `vocabulary` keeps the saved path (a client that has not read the settings sends `''`), and the absolute default path is stored as `""`; a rejected stored value (a relative path, say) is discarded on the next save. Saving writes `settings.json.partial` and renames it into place. Turning summaries off affects new entries only. `library_open` is the drawer's state, which the window keeps there.

**Ids.** An id is 8 digits, `-`, 6 digits, optionally `-` and more digits. Every core and engine function that takes an id treats any other string as not in the library, which keeps an id from reaching outside `library/`. The list takes the folders in `library/` whose names are ids and whose `entry.json` parses with a string `title` and `summary` (extra keys are ignored; the id is always the folder's name), newest first: by the 15-character base id, then by suffix (none counts as 1).

## Engine API

`crates/scriber-engine` is the whole capability as a library: another Rust app depends on it (as a git dependency) and the desktop app is one client of it. It has no Tauri and no async runtime; jobs run on std threads.

- **`EngineConfig`**: `data_dir` (the consumer's data folder, laid out as in The data folder; it need not exist), `model: ModelSource`, `clipboard: Option<Arc<dyn Clipboard + Send + Sync>>`, `library: bool` (keep finished sessions in the recording library; default false), `ollama_url: String` (default `DEFAULT_OLLAMA_URL`, `http://127.0.0.1:11434`) and `summary_model: String` (default `DEFAULT_SUMMARY_MODEL`, `qwen3.8:27b-mlx`; the same server and model do the cleanup) and `editor: PathBuf` (the program that opens the glossary in a text editor with `-t <path>`; default `OPEN`, macOS's `/usr/bin/open`). `EngineConfig::new(data_dir)` takes the default model, no clipboard, no library and `/usr/bin/open`.
- **`ModelSource`**: `Local(path)` uses a ggml model file where it is and never downloads; a missing file says "The speech model is missing from <path>." and `download_model()` is refused with `DictationError::LocalModel`. `Download(Source { url, bytes, sha256 })` downloads into `<data_dir>/models/ggml-large-v3-turbo.bin` when missing and verifies size and SHA-256, so a mirror only needs its own URL. The default is `Download(Source::default())`: Hugging Face `large-v3-turbo`, pinned above.
- **Opening**: `Engine::open(config, events)` with the real adapters (`CpalRecorder`, `Whisper`, `HttpDownloader`, `LocalClock`, `Afconvert` at `/usr/bin/afconvert` over the data folder, and `Ollama` at `ollama_url` with `summary_model` twice: the summarizer with a 60 s timeout and the cleaner with `CLEANUP_TIMEOUT`, 120 s), or `Engine::with_adapters(config, make, events)` with others (`make` gets the level sink to hand to its recorder and returns `Adapters { recorder, transcriber, downloader, clock, encoder, summarizer, cleaner }`). Either recovers the data folder as the app does at launch and starts the warm-up, or the download, on a job thread. The session is assigned under its lock before any job it queued can record an outcome.
- **`Event`**: `Level(f64)` (0 to 1, at most 20 a second while recording) and `State(Snapshot)` (whatever `dictation-changed` carries). No method emits events: the snapshot a method returns is its result.
- **Threading**: `Event::State` is delivered on a job thread while the session lock is held, so the callback must never call the engine (it deadlocks); forward it to a channel. `Event::Level` is delivered on the audio thread and must return quickly. `Engine` is `Send + Sync` and `Clone`; dropping the last handle while no job runs drops the adapters.
- **Recording**: `record()`, `pause()`, `resume()`, `stop()` (Done), `discard()`, `retry()`, `download_model()`, and `act(action, text)` for any action. Record then stop, with no pause, is the primary path. `record()` from idle or finished starts a new session first (deleting the last one); it never applies New from failed, which would delete audio not yet transcribed: it refuses ("Cannot record while failed."), and the consumer retries or discards.
- **Waiting**: `wait(timeout)` answers a finished session's text at once, a failed session's error as `EngineError::Failed`, `NotStopped` while idle, ready, recording or paused, and while transcribing waits on a condition variable paired with the session lock, until the deadline (`Timeout`). `stop_and_wait(timeout)` is `stop()` then `wait`. With cleanup on, the wait includes the cleanup call, up to `CLEANUP_TIMEOUT`, so a consumer should allow for it (the crate example waits 300 s). A finished session recovered at open answers its old text until `record()` starts a new one.
- **Shutdown**: `shutdown()` takes the session lock, refuses new work from then on (no job starts, so a download that finishes later queues no warm-up), pauses a recording so its audio is saved, releases the lock and then frees the speech model (`Transcriber::unload`). It blocks for at most the one model load or transcription already running, which finishes first; calls still waiting for the model are refused. It is idempotent on every clone and thread, and each call returns only once the model is freed. It answers a `ShutdownReport { first, paused_recording, freed_model }`. Afterwards `act` (and every action method), `record`, `wait` and `stop_and_wait` return `EngineError::ShutDown` ("The engine has shut down.") and leave the session unchanged; `state()` still answers; a waiter already blocked keeps waiting for the transcription in flight or its timeout. On macOS a consumer MUST call it before the process exits: whisper.cpp's Metal device is a C++ static freed at exit that aborts (`GGML_ASSERT([rsets->data count] == 0)`) while a whisper context is alive, and dropping every `Engine` handle is not enough, because a job thread may still hold the engine. The desktop app calls it on `RunEvent::Exit`, which Cmd-Q, the menu Quit and closing the last window all reach before AppKit's `exit()`. `scripts/exit-check.sh` checks it on the Mac.
- **Library**: `library()` lists the entries newest first (`LibraryEntry { id, title, summary }`), and is empty without touching the data folder when the library is off; it does not wait for the session lock. `library_transcript(id)` answers an entry's transcript, and `library_copy(id)` puts it on the configured clipboard; both answer `EngineError::NotInLibrary` ("That recording is no longer in the library.") for an id that is not an entry's, a missing transcript or the library off, and `library_copy` answers `EngineError::Refused(DictationError::Clipboard(..))` when the clipboard fails. Library changes arrive as `Event::State` snapshots whose `library_revision` grew.
- **Settings**: `settings()` reads `settings.json` (the defaults where there is none), with an empty `vocabulary` answered as `<data_dir>/vocabulary.txt`, and `set_settings(s)` saves it by the write rule in Settings above and answers with `settings()`, whether or not the library is on; a write failure is `EngineError::Settings` ("Could not save the settings: <reason>."). `open_vocabulary()` creates the glossary at `settings().vocabulary` empty when it is missing (its folder too; an existing file is never truncated, and a created one stays even when the editor fails), then runs `<editor> -t <path>`; a failure is `EngineError::Editor` ("Could not open the vocabulary file: <reason>."). After `shutdown()`, `set_settings` answers `ShutDown`; `settings()`, `open_vocabulary()`, `library()`, `library_transcript()` and `library_copy()` keep working.
- **Clipboard**: with `clipboard: None` the transcript is copied nowhere, and the snapshot's notice still says "Copied to the clipboard." (pinned by a test).
- **Not yet**: no streaming or partial transcripts. `Event` is not `#[non_exhaustive]` today, but a streaming mode would add a variant (and a mode), not change these.

## Bridge contract

Tauri camel-cases argument names. `tests/e2e/contract.spec.ts` reads `src-tauri/src/lib.rs` and fails when a command, an argument or the event name disagrees with `bridge.ts`. `crates/scriber-core/tests/fixtures/bridge-shapes.json` holds an example of every JSON shape; the core's serde tests and the mock's honesty test both check against it.

### Commands

| Command | Arguments | Returns | Errors |
|---|---|---|---|
| `dictation_state` | none | `Snapshot` | never |
| `dictation_act` | `action: Action`, `text: string` | `Snapshot` after the action (`downloadModel` included) | the core's refusal, as one sentence (see above) |
| `library_list` | none | `LibraryEntry[]`, newest first | never |
| `library_get` | `id: string` | `string`, the entry's transcript | "That recording is no longer in the library." |
| `library_copy` | `id: string` | nothing; the transcript is on the clipboard | "That recording is no longer in the library."; "Could not copy to the clipboard: <reason>" |
| `settings_get` | none | `Settings` | never |
| `settings_set` | `settings: Settings` | `Settings`, as saved | "Could not save the settings: <reason>."; "The engine has shut down." |
| `vocabulary_open` | none | nothing | "Could not open the vocabulary file: <reason>." |

`text` is the edited transcript for `copy` and is ignored (the bridge sends `""`) for every other action. Errors arrive in the webview as a rejected promise carrying one readable sentence. The app's version comes from Tauri's own `getVersion()` (`@tauri-apps/api/app`), not a command of ours, and Browse… for the vocabulary file is the dialog plugin's native open panel (`open` from `@tauri-apps/plugin-dialog`, one `.txt` file, starting at the current path; the capability grants `dialog:allow-open`), which answers a path or null.

### Events

| Event | Payload | When |
|---|---|---|
| `dictation-changed` | `Snapshot` | a job changed the snapshot: a segment's transcription finished (or failed), whether or not the phase changed (a session archived into the library on finishing included); the model's download moved another whole percent, finished or failed; the warm-up finished (unchanged); an encode finished (unchanged); a summary was written (`libraryRevision` grew) or failed (unchanged) |
| `dictation-level` | `number` (0 to 1) | while recording, one per 50 ms window of sound (at most 20 a second); see Input level |

Commands and events can arrive out of order (a segment can finish before the command that queued it has answered), so the window keeps the snapshot with the highest `revision`. There is no library event: the window lists the library again whenever an accepted snapshot's `libraryRevision` is higher than the one it last listed for.

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
  libraryRevision: number; // grows when an entry is added or its summary replaced (each also grows revision)
}

interface LibraryEntry {
  id: string; // "20261026-140309", or "20261026-140309-2" after a collision
  title: string; // "2026-10-26 14:03:09", the session's start in local time
  summary: string; // the local model's sentence, or the excerpt
}

interface Settings {
  summaries: boolean; // summarize new entries with the local model
  libraryOpen: boolean; // the library drawer is open
  vocabulary: string; // the glossary file's absolute path; send '' when unknown (the saved one is kept)
  cleanup: boolean; // clean up each transcript with the local model
}
```

Every object is exactly these fields: the Rust types refuse unknown and missing fields.

## The mock

`src/lib/mock.ts` runs the core's state machine in the browser over a fake microphone and transcriber, deterministically. Segment N is heard as "This is part N of the dictation."; durations come from `Date.now()`, so Playwright's clock controls the recorded time. While recording it emits a synthetic, speech-like level every 50 ms, and stops on Pause, Done and Discard. URL parameters pick the scenario: `?model=download` (the model is missing and downloads at launch in four steps), `?model=fail` (the same, but the first download fails half way with "the connection was reset"; Retry download succeeds), `?mic=silent` (every segment silent), `?transcribe=fail` (each segment's first transcription fails, so Retry succeeds), `?clipboard=fail`, `?leftover=<n>` (launch finds n segments of an unfinished session), `?long=1` (long paragraphs, to overflow the box), `?fail=<call>` (`dictation_state`, `app_version`, an action, `library_list`, `library_get`, `library_copy`, `settings_get`, `settings_set` or `pick_vocabulary_file` rejects with "Mock failure: <call>"; `?fail=vocabulary_open` rejects with "Could not open the vocabulary file: no text editor."), `?pick=<path>` (the open panel picks that file; `/mock/Documents/terms.txt` without it, nothing with `?pick=cancel`), `?library=<n>` (the library starts with n entries; entry k, newest first, is 2026-10-26 09:00:00 local minus k minutes, summarized "Preloaded recording number k+1." with the transcript "This is preloaded recording number k+1."), `?summaries=off` and `?libraryOpen=0` (the settings start so), `?cleanup=on` (cleanup starts on) and `?ollama=down` (the summarizer fails, so entries keep their excerpts); `?clipboard=fail` fails `library_copy` too. A finished session with a transcript is archived like the core does it: titled by `Date.now()` at its first Record (or at archive, for a leftover session finished by Retry), with the excerpt, then summarized as "A dictated note." unless summaries are off or Ollama is down. A test can hold `dictation_act`, `dictation_state`, `transcribe`, `download` (each step of the download), `summarize` (each new entry's summary) or `cleanup` (with cleanup on, the cleanup of a finished transcript, which keeps the session `transcribing`; the mock cleaner answers the transcript unchanged) with `window.__mockBridge.hold(name)`, or from launch with `?hold=<name>`, and let it go with `release`; `window.__mockBridge.copied` lists every clipboard write, `window.__mockBridge.acts` every action the window asked for `window.__mockBridge.saved` every `settings_set` as sent, `window.__mockBridge.opened` every glossary path opened in the text editor and `window.__mockBridge.browsed` the path each open panel started at. The mock keeps `vocabulary` like the engine: the default answered as `/mock/data/vocabulary.txt`, `''` keeping the saved path and the default saved as `''`.

## UI

One window with an overlay title bar. The top band (which drags the window) holds the Library toggle, the "Dictation" heading, the recorded time (red while the microphone is live), New and the Settings gear; the library drawer runs down the left. Beside it: while the model downloads, a determinate progress bar with "Downloading the speech model, needed once: 42% (680 MB of 1.6 GB). Record works when it is done." (status is shown only when the user must change what they do); the model error as an alert with Retry download when it applies; a status line (`aria-live="polite"`) saying what to do next or how the last action went; the failure or refusal as an alert; the waveform while recording or paused; and the buttons Record, Pause, Resume, Done and Discard (plus Retry when failed), each enabled only when the core would accept it and while no action is in flight. When finished, an editable "Transcript" box fills the rest of the window and scrolls inside itself, with Copy beside its label. Light and dark follow the system. `tests/e2e/a11y.spec.ts` runs axe over every state in both.

**Library.** A toggle button left of the heading (a sidebar icon, named "Library", `aria-expanded`, `aria-controls="library"`) opens and closes a 220 px drawer on the window's left, `<nav aria-label="Recordings">`; its state is saved as `settings.libraryOpen` (open by default; a failure to save it is an alert in the band, and the drawer flips anyway). The drawer lists the entries newest first, each a button with the title on the first line and the summary on one line with an ellipsis below it; the list scrolls inside the drawer and the window never scrolls. An empty library says "No recordings yet."; one that cannot be read says "Could not read the library: <reason>" as an alert in the drawer. A just-finished session appears at the top once archived, and its second line changes in place from the excerpt to the summary. Nothing anywhere says whether Ollama is running or a summary is on its way. Selecting an entry (`aria-current="true"` on its row) shows it in place of the recorder: its title as a heading, the transcript read-only in a box labelled "Transcript", and Copy, after which a polite live line says "Copied to the clipboard."; a failure to read or copy it is an alert in the pane. While an entry is shown, New is enabled in every phase: New or ⌘N goes back to the recorder, and starts a new session too when the phase allows one. At the minimum window size (520 × 380) with the drawer open the recorder takes the rest of the width, its buttons wrap and the transcript box shrinks.

**Settings.** A gear button (named "Settings", `aria-expanded`, `aria-controls="settings"`) at the band's right opens a small panel, `<section aria-label="Settings">`, with the "Vocabulary file" group above two switches: "Enable post-processing with local LLM", checked from `settings.cleanup` (off by default), and "Summarize recordings with local AI (Ollama)", checked from `settings.summaries`. Nothing says whether Ollama is running or how a cleanup went; while it runs the status line reads "Transcribing…". The group (`role="group"`) shows the glossary's path in a read-only field (its full path as the tooltip, cut with an ellipsis), then Browse… and Open in Text Editor. Browse… opens the dialog plugin's native open panel at the current file; a picked file shows at once and is saved, and a cancel changes nothing. Open in Text Editor opens the file in the app macOS has for plain text (`open -t`), creating it empty first. Changing either switch saves the settings; a failure to pick, save or open is an alert in the panel. Escape or the gear closes it. Settings that cannot be read leave the defaults, silently.

**Waveform.** A scrolling bar waveform above the buttons, of thin bars: 2 px wide on a 4 px pitch, mirrored about the centre line and at least 2 px tall (so silence reads as a dotted baseline), as many as fit the strip's width (`barCount` in `src/lib/format.ts`, history capped at `MAX_BARS` = 512), the newest on the right, older bars moving left, each bar's height its level. Bars are red while recording and grey while paused. It shows while recording; while paused it stays, frozen (levels that arrive after Pause are dropped); leaving recording and paused (Done, New, Discard) clears it. Under `prefers-reduced-motion: reduce` one level bar showing the latest level replaces the scrolling bars. Both are `aria-hidden`; a visually hidden line says whether the microphone is picking up sound (or is off while paused).

**Keyboard.** In-window shortcuts, not global hotkeys: ⌘N is New; Space is Record from ready, Pause from recording and Resume from paused; Return is Done from recording or paused. A key does nothing (and sends nothing) in a phase whose action it is not, while an action is in flight, and on its auto-repeats. Space and Return type in the transcript and any other text field, where ⌘N still works. Each handled key cancels its default, and the Space keyup after it too, so a button that merely has focus from a click is not clicked as well; a button reached with Tab keeps Space and Return for itself, and fires once. A focused button that is left disabled once an action settles hands the focus back to the window, or it would swallow every key after it. While a library entry is shown, Space and Return send no action and only ⌘N acts. The summaries and post-processing switches keep Space for themselves (they toggle). The document puts the recorder (or the entry) before the drawer and the band, which the layout grid places above and beside it, so Tab reaches the recorder's buttons first. Each button with a shortcut shows it ("⌘N", "Space", "⏎", `aria-hidden`) and names it in `aria-keyshortcuts`. Tauri's default macOS menu (which the app keeps) binds ⌘Q, ⌘W, ⌘H, ⌥⌘H, ⌘M, ⌃⌘F and the Edit items, not ⌘N, and WKWebView gives ⌘N no meaning, so the webview receives it.

## Bundle

`src-tauri/Info.plist` (merged into the bundle's) carries `NSMicrophoneUsageDescription`, so macOS asks for the microphone the first time Record is clicked. `src-tauri/Entitlements.plist` grants `com.apple.security.device.audio-input`, which the hardened runtime requires for microphone input.
