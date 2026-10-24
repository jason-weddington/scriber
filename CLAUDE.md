# Scriber

macOS desktop app: Rust core, Tauri 2 shell, Svelte 5 frontend. `docs/design.md` is the spec and the bridge contract (command names, argument names, JSON shapes, events). Read it before changing behaviour, and change it in the same commit when reality disagrees.

## Layout

- `crates/scriber-core` (root cargo workspace): all logic. No Tauri, no async runtime, no direct I/O: every effect is a trait in `src/ports.rs`. Fakes for every port are in `tests/common`.
- `apps/desktop/src-tauri`: its OWN cargo workspace (`apps/desktop/Cargo.toml`, own `Cargo.lock`). A thin adapter: the real port implementations and one `#[tauri::command]` per contract entry, each forwarding to the core.
- `apps/desktop/src`: Svelte 5 + TypeScript. `src/lib/bridge.ts` is the only file that imports `@tauri-apps/*`; outside Tauri it answers from `src/lib/mock.ts`.
- `apps/desktop/tests/unit` (pure TS) and `tests/e2e` (the app in Chromium against the mock), both Playwright.

## Commands

```sh
bash scripts/gate.sh pre-commit      # what every commit must pass (~10 s warm)
bash scripts/gate.sh full            # everything: coverage, mutants, audits, e2e, build
cargo nextest run --workspace        # core tests while iterating
cd apps/desktop && pnpm test         # frontend unit + e2e while iterating
cd apps/desktop && pnpm dev          # the frontend in a browser, on the mock
bash scripts/install.sh              # release build, verify the signature, install to /Applications
./release.sh                         # cut a release (cog bump --auto, tag, push)
```

## Rules

- **`scripts/gate.sh` is the definition of done.** The hooks call it, and so should you before you say anything is finished. Run `bash scripts/gate.sh full` for a change of any size.
- **Hooks check, never fix.** When `fmt` or `cargo-sort` fails, run `cargo fmt --all` or `cargo sort --workspace --grouped` yourself (in both workspaces) and commit again. Never commit with `--no-verify`.
- **Coverage floors only go up.** They are literals at the top of `scripts/gate.sh` (Rust lines, at least 98; frontend lines). When coverage climbs, raise them; never lower one to get a change through.
- **Zero surviving mutants** in the core. A survivor is a behaviour no test pins: add the test.
- **No `#[allow(...)]`.** Use `#[expect(lint, reason = "...")]`, which fails once it is no longer needed.
- **No logic in `src-tauri`**, and no `@tauri-apps` import outside `bridge.ts`. `scripts/check-architecture.sh` enforces both.
- **The mock answers like the core.** When a core rule or JSON shape changes, change `mock.ts`, `tests/unit/mock.spec.ts` and `crates/scriber-core/tests/fixtures/bridge-shapes.json` with it.
- **Never launch the GUI app from an agent session**: no `pnpm tauri dev`, no `open` on the bundle, no osascript, no screen capture. macOS attributes the app's permission prompts to whatever launched it. Build and install with `scripts/install.sh`, and let the human open it. Browser tests through Playwright against the mock are fine.
- Every list that can grow scrolls, and gets an `expectEveryItemReachable` test (`tests/e2e/helpers.ts`). Every screen state gets an axe check (`tests/e2e/a11y.spec.ts`).

## Replacing the example feature

The folder summary exists to show each seam working. To replace it: write the new rule in `crates/scriber-core/src` with its ports in `ports.rs` and its fakes in `tests/common`; pin its JSON shapes in `model.rs`, `tests/serde_shapes.rs` and the fixture; add one command per contract entry in `src-tauri/src/lib.rs`; add the bridge function in `bridge.ts` and its mock in `mock.ts`; update `tests/e2e/contract.spec.ts`'s `CALLS`; build the screen; then delete `scan.rs`, `SummaryList.svelte` and their tests, and rewrite `docs/design.md`.

More: `docs/quality-gates.md` (what each gate catches and why), `docs/tauri-design-notes.md` (patterns for Tauri 2 apps on macOS).
