# Scriber

macOS dictation app: Rust core, Tauri 2 shell, Svelte 5 frontend. New, Record, Pause, Resume, Done; local Whisper transcription; the transcript lands on the clipboard. `docs/design.md` is the spec and the bridge contract (command names, argument names, JSON shapes, events). Read it before changing behaviour, and change it in the same commit when reality disagrees.

## Layout

- `crates/scriber-core` (root cargo workspace): all logic. No Tauri, no async runtime, no direct I/O: every effect is a trait in `src/ports.rs`. Fakes for every port are in `tests/common`. The recording library (`library.rs`, `settings.rs`) adds the `Clock`, `Encoder` and `Summarizer` ports, which travel together in `ports::Library` and are only used with the library on.
- `crates/scriber-engine` (root cargo workspace): the core plus the real adapters (cpal, whisper-rs, `std::fs`, ureq, chrono, afconvert, Ollama over ureq) and a std-thread job queue, behind a small `Engine` API that any Rust app can depend on. No Tauri, no async runtime. Its fakes are in its own `tests/common`. The gate never needs Ollama or afconvert: the Ollama adapter is tested against an in-process HTTP stub and the afconvert adapter against stub scripts; the lead checks the real ones on the Mac.
- `apps/desktop/src-tauri`: its OWN cargo workspace (`apps/desktop/Cargo.toml`, own `Cargo.lock`). A thin client over the engine: the clipboard adapter, the two events and one `#[tauri::command]` per contract entry, each forwarding to the `Engine`.
- `apps/desktop/src`: Svelte 5 + TypeScript. `src/lib/bridge.ts` is the only file that imports `@tauri-apps/*`; outside Tauri it answers from `src/lib/mock.ts`.
- `apps/desktop/tests/unit` (pure TS) and `tests/e2e` (the app in Chromium against the mock), both Playwright.

## Commands

```sh
bash scripts/gate.sh pre-commit      # what every commit must pass (~10 s warm)
bash scripts/gate.sh full            # everything but mutation testing: coverage, audits, e2e, build
bash scripts/gate.sh release         # full plus mutation testing over the whole core; release.sh runs it
cargo nextest run --workspace        # core and engine tests while iterating
cd apps/desktop && pnpm test         # frontend unit + e2e while iterating
cd apps/desktop && pnpm dev          # the frontend in a browser, on the mock
bash scripts/install.sh              # release build, verify the signature, install to /Applications
bash scripts/fetch-model.sh          # download the speech model into the app data folder (never in the gate)
bash scripts/smoke.sh                # real-model check: `say` + ffmpeg + the app's Whisper adapter
bash scripts/exit-check.sh [--control]   # Mac exit check: quitting with the model loaded must not abort
./release.sh                         # gate.sh release (full + mutants), then cog bump --auto, tag, push
```

## Rules

- **`scripts/gate.sh` is the definition of done.** The hooks call it, and so should you before you say anything is finished. Run `bash scripts/gate.sh full` for a change of any size.
- **Hooks check, never fix.** When `fmt` or `cargo-sort` fails, run `cargo fmt --all` or `cargo sort --workspace --grouped` yourself (in both workspaces) and commit again. Never commit with `--no-verify`.
- **Coverage floors only go up.** They are literals at the top of `scripts/gate.sh` (Rust lines, at least 98; frontend lines). When coverage climbs, raise them; never lower one to get a change through.
- **Zero surviving mutants** in the core and the engine, checked by `gate.sh release`, which release.sh runs; a survivor found there blocks the release and gets a test.
- **No `#[allow(...)]`.** Use `#[expect(lint, reason = "...")]`, which fails once it is no longer needed.
- **No logic in `src-tauri`**, and no `@tauri-apps` import outside `bridge.ts`. `scripts/check-architecture.sh` enforces both.
- **The mock answers like the core.** When a core rule or JSON shape changes, change `mock.ts`, `tests/unit/mock.spec.ts` and `crates/scriber-core/tests/fixtures/bridge-shapes.json` with it.
- **Never launch the GUI app from an agent session**: no `pnpm tauri dev`, no `open` on the bundle, no osascript, no screen capture. macOS attributes the app's permission prompts to whatever launched it. Build and install with `scripts/install.sh`, and let the human open it. Browser tests through Playwright against the mock are fine.
- Every list that can grow scrolls, and gets an `expectEveryItemReachable` test (`tests/e2e/helpers.ts`). Every screen state gets an axe check (`tests/e2e/a11y.spec.ts`).

## Adding to a feature

Write the rule in `crates/scriber-core/src` with any new effect as a port in `ports.rs` and its fake in `tests/common`, and its real adapter in `crates/scriber-engine/src` (unless it needs Tauri); pin new JSON shapes in `model.rs`, `tests/serde_shapes.rs` and the fixture; keep `src-tauri/src/lib.rs` to the clipboard adapter and one command per contract entry over the `Engine` (it has a 250-line limit, so prefer extending `dictation_act`'s `Action` over a new command), and give the engine a method or `EngineConfig` field when another app would need it too; add the bridge function in `bridge.ts` and its mock in `mock.ts`; update `tests/e2e/contract.spec.ts`'s `CALLS`; and update `docs/design.md` in the same commit. The whisper.cpp build needs `cmake` (`brew install cmake`).

More: `docs/roadmap.md` (what is planned but not built), `docs/quality-gates.md` (what each gate catches and why), `docs/tauri-design-notes.md` (patterns for Tauri 2 apps on macOS).
