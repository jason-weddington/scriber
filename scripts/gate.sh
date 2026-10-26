#!/usr/bin/env bash
# The ONE definition of the quality gate (docs/quality-gates.md). lefthook
# calls it for the git hooks, an agent harness or CI calls `full`, and
# release.sh calls `release`, so hooks, agents and releases are held to the
# same bar and cannot drift.
#
#   scripts/gate.sh pre-commit   per-commit checks; a docs-only commit skips the heavy ones
#   scripts/gate.sh pre-push     pre-commit plus coverage, doctests, audits and e2e, with no mutation testing
#   scripts/gate.sh full         everything except mutation testing, never skipped; the agent and GTD gate command
#   scripts/gate.sh release      everything full runs plus mutation testing over the whole core and engine; release.sh runs it
#
# Every step CHECKS; none rewrites a tracked file. Steps run in order and the
# script stops at the first failure, naming the step that failed.
set -euo pipefail
cd "$(dirname "$0")/.."

mode=${1:?usage: gate.sh pre-commit|pre-push|full|release}
case "$mode" in pre-commit|pre-push|full|release) ;; *) echo "unknown mode: $mode" >&2; exit 2 ;; esac

# Coverage floors. RATCHET UP as coverage climbs, never down.
# Rust: line coverage of the core and the engine, at least 98.
RUST_COVERAGE_FLOOR=100
# Frontend: line coverage of src/ (unit + e2e; the mock is excluded, see
# apps/desktop/tests/coverage/options.ts), whole-number percent.
FRONTEND_COVERAGE_FLOOR=99

# Scratch space on disk: /tmp is a small tmpfs on some hosts and concurrent
# gates can fill it. Unless TMPDIR already points elsewhere, use target/gate-tmp
# (gitignored) for every step.
case "${TMPDIR:-/tmp}" in
  /tmp|/tmp/*) mkdir -p target/gate-tmp; export TMPDIR="$PWD/target/gate-tmp" ;;
esac

step() {
  local name=$1; shift
  echo "gate: $name" >&2
  if ! "$@"; then
    echo "gate: FAILED at step '$name'" >&2
    exit 1
  fi
}

in_app() { (cd apps/desktop && "$@"); }

docs_only=false
case "$mode" in
  pre-commit) scripts/docs-only.sh staged 2>/dev/null && docs_only=true ;;
  pre-push) scripts/docs-only.sh push 2>/dev/null && docs_only=true ;;
  full|release) ;;
esac

# ---- cheap checks: always, even for docs ------------------------------------
step fmt cargo fmt --all -- --check
step fmt-app in_app cargo fmt --all -- --check
step cargo-sort cargo sort --workspace --grouped --check
step cargo-sort-app in_app cargo sort --workspace --grouped --check
step typos typos
case "$mode" in
  pre-commit) step gitleaks gitleaks git --staged --redact --no-banner ;;
  pre-push|full|release) step gitleaks gitleaks dir . --redact --no-banner ;;
esac

if $docs_only; then
  echo "gate: docs-only change, heavy steps skipped" >&2
  exit 0
fi

# ---- every commit -----------------------------------------------------------
# The frontend install comes first: the selftest's eslint case needs it.
step pnpm-install in_app pnpm install --frozen-lockfile --prefer-offline --silent
step architecture scripts/check-architecture.sh
step checks-selftest scripts/selftest-checks.sh
step clippy cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
step clippy-app in_app cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
# One instrumented build serves every Rust test step below, so the core is
# compiled once, not once per step.
step tests cargo llvm-cov nextest --workspace --all-features --locked --no-report
# The same suite under an odd time zone and a decimal-comma locale: nothing
# may depend on the machine's zone or language.
step tests-tz-locale env TZ=Pacific/Chatham LANG=de_DE.UTF-8 LC_ALL=de_DE.UTF-8 \
  cargo llvm-cov nextest --workspace --all-features --locked --no-report
step svelte-check in_app pnpm check
step eslint in_app pnpm lint
step unit-tests in_app pnpm exec playwright test --project unit

[ "$mode" = pre-commit ] && { echo "gate: $mode passed" >&2; exit 0; }

# ---- pre-push, full and release ------------------------------------------------------
# Measures afresh (cargo llvm-cov cleans its old profiles first), so a gate can
# never pass on coverage data left by an earlier commit.
# The engine's cpal and whisper.cpp adapters (and its smoke example) need a
# microphone and a speech model the gate never has, so they are not measured;
# every other file, the engine's lib.rs included, is held to the floor.
step coverage cargo llvm-cov nextest --workspace --all-features --locked \
  --ignore-filename-regex 'crates/scriber-engine/(src/(device|whisper)\.rs|examples/)' \
  --fail-under-lines "$RUST_COVERAGE_FLOOR"
# nextest does not run doctests; without this they silently never run.
step doctests cargo test --doc --workspace --locked

# Mutation testing runs only in release mode, over the whole core and engine,
# with zero survivors required. The copies cargo-mutants builds live under target/.
if [ "$mode" = release ]; then
  mkdir -p target/mutants-tmp
  step mutants env TMPDIR="$PWD/target/mutants-tmp" cargo mutants --no-shuffle
  step mutants-nonempty grep -Eq '"total_mutants": *[1-9]' mutants.out/outcomes.json
fi

step machete cargo machete
step deny cargo deny check
step deny-app in_app cargo deny check
step pnpm-audit in_app pnpm audit --prod
step browsers in_app pnpm exec playwright install chromium
# The whole Playwright suite (unit + e2e with the axe checks), instrumented,
# with the frontend coverage floor enforced in its global teardown.
step e2e-coverage env COVERAGE=1 COVERAGE_MIN="$FRONTEND_COVERAGE_FLOOR" \
  bash -c 'cd apps/desktop && pnpm exec playwright test'
step vite-build in_app pnpm build
echo "gate: $mode passed" >&2
