# Scriber on the web

Status: **approved by Jason 2026-11-22, to build.** Requested by Jason: the full Scriber experience on his iPad, from a web app installed like any PWA, hosted on the Mac Studio, with the audio captured in the browser, transcribed on the Studio and the text copied to the iPad's clipboard. It is Scriber's own feature and is not coupled to Vortac in any way.

## Shape

```text
browser (Svelte UI, same code as the Mac app)
  ├─ mic ──▶ AudioWorklet ──▶ 16 kHz PCM16 binary frames ─┐
  └─ commands and events as JSON over one WebSocket ─────┤
                                                          ▼
              scriber-server (axum) ──▶ scriber-engine: StreamRecorder → whisper.cpp → cleanup → library
                                                          ▲
                              serves the built web app as static files from the same origin
```

- **One new crate, `crates/scriber-server`** in the root cargo workspace: a binary around `scriber-engine`. It owns no rules. It opens one `Engine` with `EngineConfig::recorder = Some(StreamRecorder)`, `clipboard: None` and `library: true`, serves the web build as static files, and exposes one WebSocket at `/ws`. Only this crate may use tokio and axum; `scripts/check-architecture.sh` keeps them out of the core and the engine.
- **The web app is the existing Svelte app**, built a second way (`VITE_SCRIBER_BACKEND=web`, script `pnpm build:web`, output `apps/desktop/dist-web`). Only `bridge.ts` knows which backend it talks to (Tauri, the mock, or the server); it may import a sibling `bridge-web.ts`. No `@tauri-apps` import appears anywhere else and the production web bundle carries no mock.
- **No global shortcut, no menu bar.** Everything else is the same screens, states and in-app keys: Space is Record, Pause and Resume, Return is Done, ⌘N is New (keydown handlers that already exist).

## Server

- **Settings (env, all optional except the token):** `SCRIBER_BIND` (default `127.0.0.1:8742`), `SCRIBER_TOKEN` (the shared secret; the server refuses to start without it unless `SCRIBER_ALLOW_NO_TOKEN=1`, for local development), `SCRIBER_DATA` (the engine's data folder; default `~/Library/Application Support/com.jasonweddington.scriber-server`, deliberately not the Mac app's folder, so the two never write the same library), `SCRIBER_MODEL_FOLDER` (where the Whisper model is read from; set it to the Mac app's models folder to share the 1.6 GB file read-only; the engine downloads the model there when it is missing, as it does today) and `SCRIBER_WEB_DIR` (the static files; default `apps/desktop/dist-web` relative to the working directory).
- **One engine, one session.** Every connection sees the same session and receives the same events. Audio is accepted only from the connection that issued the last Record or Resume, so two devices cannot interleave audio.
- **`GET /healthz`** answers `ok` and needs no token. Static files need no token (they contain no secret).

## Protocol (`/ws`, JSON text frames plus binary audio)

- **Auth.** The first frame from the client must be `{"hello":{"token":"..."}}`. A wrong or missing token closes the socket with code 4401 and no other frame is processed. The server answers `{"hello":{"version":"<crate version>","platform":"web"}}`.
- **Request and reply.** `{"id":<u32>,"call":"<name>","args":{...}}` is answered by exactly one `{"id":<same>,"ok":<value>}` or `{"id":<same>,"err":"<message>"}`. The calls and their arguments and results are the bridge contract in `docs/design.md` (Bridge), with the same names, argument names and JSON shapes: `dictation_state`, `dictation_act`, `library_list`, `library_get`, `library_copy` (see Clipboard), `settings_get`, `settings_set`, `ollama_models`, `app_version`. These have no server meaning and answer a fixed value: `input_devices` (`{"default":"Browser microphone","others":[]}`), `window_fullscreen` (`true`). `vocabulary_open`, `learned_vocabulary_open`, `pick_vocabulary_file` and `pick_model_folder` answer `err` "Not available in the web app."
- **Events.** `{"event":"dictation-changed","payload":<Snapshot>}` and `{"event":"dictation-level","payload":<number>}`, the same two events the Tauri app emits, to every connection. A new connection gets the current Snapshot right after the hello.
- **Audio.** Binary frames from client to server, little-endian PCM16, 16 kHz, mono, any length (the browser sends 4,000 samples, 250 ms, per frame). The server pushes each frame with `StreamRecorder::push_pcm16` in order. Frames outside phase `recording`, or from a connection that does not own the recording, are dropped. A connection's frames and calls are processed in the order sent, so the audio sent before a Done call is in the capture when Done runs.
- **Errors and limits.** A malformed frame or an unknown call is answered `err` and the socket stays open. A binary frame over 1 MiB closes the socket with code 1009. Closing the owning connection while recording pauses nothing by itself: the session stays `recording` until another connection acts, and the status row says so (see the client).

## Client

- **Capture.** Capture uses `getUserMedia({audio: {channelCount: 1, echoCancellation: true, noiseSuppression: true}})`, started synchronously inside the click or keydown that asked for Record or Resume, because Safari only allows it from a gesture, and an AudioWorklet (a static `public/capture-worklet.js`; the CSP excludes `blob:` and inline worklets) that downmixes to mono and posts blocks to the main thread, which resamples to 16 kHz, converts to PCM16 and sends 250 ms frames. Tracks are stopped on Pause and Done, so the browser's microphone indicator goes off. The browser must have flushed its last frame before the Done call is sent.
- **Clipboard.** The engine has no clipboard in the server, so the browser writes it. On the Done action the web bridge immediately starts `navigator.clipboard.write([new ClipboardItem({"text/plain": <promise>})])` inside the gesture, where the promise resolves with the transcript when the finished Snapshot arrives (Safari accepts a promise there; a write after an `await` outside the gesture is refused). If the write rejects, the status row shows the existing failure sentence `Could not copy to the clipboard: <reason>. Use Copy to try again.` and Copy still works. `library_copy` in the web build reads `library_get` and writes the text in the click's gesture. The notice the core sets ("Copied to the clipboard.") is shown only after the write succeeds. **This is the one unproven behaviour. It cannot be tested in CI, so the build includes the fallback above, and Jason confirms the automatic copy on the iPad after the first deploy.**
- **Backend and connect screen.** The web build talks to the same origin it was served from. When that is not a Scriber server (the hello never arrives or the token is refused), a Connect screen asks for the server URL and token, keeps them in `localStorage` and retries; it is also reachable from Settings. This is what lets a GitHub user host the server anywhere. Browsers only expose the microphone on HTTPS or `localhost`, so a remote server needs TLS in front of it and the README says so.
- **Settings screen.** The toggle shortcut, pause shortcut, copy sound, vocabulary file, learned list, model folder and microphone controls are not shown in the web build. Every other control works as on the Mac. The window's left inset is the full-screen one, since a PWA has no traffic lights.
- **PWA.** `manifest.webmanifest` (name Scriber, `display: standalone`, theme and background colours from the app, the Scriber icon at 192 and 512 px, maskable variants), the Apple touch icon (180 px), the `apple-mobile-web-app-*` meta tags and a viewport that disables zoom. There is no service worker: installing needs none and the app is useless offline.
- **Owner reconnects.** When the socket drops and reconnects the page requests state and rebinds; a recording that lost its audio connection stays in `recording` and shows "Connection lost. Press Done to finish or Discard to start over." in the status row (a reserved row, no layout shift).

## Not here

- Authentication beyond one shared token, multiple users, or more than one concurrent session.
- Editing the vocabulary file from the browser (the learned words still work on the server).
- A service worker, offline use, or push notifications.
- Anything specific to this lab: the host name, TLS, the reverse proxy, the launchd job and the token value live in `~/git/homelab-ops`, not in this repository.

## Tests

- The server is tested in process: a real WebSocket client against the server over a fake-adapter engine (hello and token, request and reply, events, audio ownership, in-order Done, limits). The coverage and mutation floors that cover the core and the engine are unchanged; `scripts/gate.sh` and `docs/quality-gates.md` say how the server crate is gated.
- The web build is tested in Playwright against a small Node stand-in for the server that speaks this protocol (`apps/desktop/tests/e2e`), with a fake microphone; the real clipboard behaviour is the iPad check above.
