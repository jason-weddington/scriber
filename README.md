# Scriber

A macOS dictation app built with Rust, Tauri 2 and Svelte 5. Click New, Record, talk, Pause, think, Resume, talk more, then Done: the transcript appears in the window and is already on the clipboard. Transcription is local, with whisper.cpp (`large-v3-turbo`) on the GPU through Metal; nothing leaves the Mac. `docs/design.md` has the details.

## Prerequisites

macOS with the Xcode command line tools (`xcode-select --install`), then:

```sh
# Rust (the exact toolchain is pinned in rust-toolchain.toml and installs itself)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Node and pnpm (the pnpm version is pinned in apps/desktop/package.json)
brew install node pnpm

# Git hooks, commit messages, spelling, secrets
brew install lefthook cocogitto typos-cli gitleaks

# whisper.cpp builds with cmake; the smoke check uses ffmpeg
brew install cmake ffmpeg

# Cargo tools the gate runs
brew install cargo-nextest cargo-llvm-cov cargo-deny cargo-sort cargo-mutants
cargo install cargo-machete
```

## Setup

```sh
lefthook install
cd apps/desktop && pnpm install --frozen-lockfile && pnpm exec playwright install chromium
```

## The speech model

```sh
bash scripts/fetch-model.sh
```

Downloads `ggml-large-v3-turbo.bin` (1.6 GB) into `~/Library/Application Support/com.jasonweddington.scriber/models/`, where the app looks for it. Without it the app says so and refuses to record. `bash scripts/smoke.sh` then checks the real model end to end: it speaks a sentence with `say`, converts it with ffmpeg and prints what the app's Whisper adapter hears, with timings.

An optional glossary, `vocabulary.txt` in the same folder (one term per line, `#` comments), steers the spelling of names and jargon.

## Develop

```sh
cd apps/desktop && pnpm tauri dev    # the app, with hot reload
cd apps/desktop && pnpm dev          # the frontend alone in a browser, against the mock backend
bash scripts/gate.sh pre-commit      # the checks every commit runs
bash scripts/gate.sh full            # every check, including coverage, mutation testing and e2e
```

## Install

```sh
bash scripts/install.sh
```

Builds the release app, verifies its signature (ad hoc, so it runs on the machine that built it) and installs it in `/Applications`. The first Record asks for the microphone; allow it in the prompt (or later in System Settings, Privacy & Security, Microphone).

## Release

```sh
./release.sh
```

Derives the next version from the conventional-commit history, sets it in every manifest, writes `CHANGELOG.md`, tags and pushes.

## Documentation

- `docs/design.md`: the app's spec and the bridge contract between the webview and Rust.
- `docs/quality-gates.md`: what each gate catches and why it exists.
- `docs/tauri-design-notes.md`: patterns for Tauri 2 apps on macOS.
- `CLAUDE.md`: how an AI coding agent should work in this repository.
