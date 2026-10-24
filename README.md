# Scriber

A macOS desktop app built with Rust, Tauri 2 and Svelte 5. It ships with one example feature: choose a folder and see its files summarised by type.

## Prerequisites

macOS with the Xcode command line tools (`xcode-select --install`), then:

```sh
# Rust (the exact toolchain is pinned in rust-toolchain.toml and installs itself)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Node and pnpm (the pnpm version is pinned in apps/desktop/package.json)
brew install node pnpm

# Git hooks, commit messages, spelling, secrets
brew install lefthook cocogitto typos-cli gitleaks

# Cargo tools the gate runs
brew install cargo-nextest cargo-llvm-cov cargo-deny cargo-sort cargo-mutants
cargo install cargo-machete
```

## Setup

```sh
lefthook install
cd apps/desktop && pnpm install --frozen-lockfile && pnpm exec playwright install chromium
```

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

Builds the release app, verifies its signature (ad hoc, so it runs on the machine that built it) and installs it in `/Applications`.

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
