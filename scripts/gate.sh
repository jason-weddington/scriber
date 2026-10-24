#!/usr/bin/env bash
# The ONE definition of the quality gate (docs/quality-gates.md). lefthook
# calls it for the git hooks, and an agent harness or CI calls `full`, so
# hooks and agents are held to the same bar and cannot drift.
#
#   scripts/gate.sh pre-commit   per-commit checks (a docs-only commit skips the heavy ones)
#   scripts/gate.sh pre-push     pre-commit plus coverage, doctests, mutants on the change, audits, e2e
#   scripts/gate.sh full         everything, never skipped, with mutants over the whole core
#
# Every step CHECKS; none rewrites a tracked file. Steps run in order and the
# script stops at the first failure, naming the step that failed.
set -euo pipefail
cd "$(dirname "$0")/.."

mode=${1:?usage: gate.sh pre-commit|pre-push|full}
case "$mode" in pre-commit|pre-push|full) ;; *) echo "unknown mode: $mode" >&2; exit 2 ;; esac

# Coverage floors. RATCHET UP as coverage climbs, never down.
# Rust: line coverage of the core, at least 98.
RUST_COVERAGE_FLOOR=100
# Frontend: line coverage of src/ (unit + e2e; the mock is excluded, see
# apps/desktop/tests/coverage/options.ts), whole-number percent.
FRONTEND_COVERAGE_FLOOR=96

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
  full) ;;
esac

# ---- cheap checks: always, even for docs ------------------------------------
step fmt cargo fmt --all -- --check
step fmt-app in_app cargo fmt --all -- --check
step cargo-sort cargo sort --workspace --grouped --check
step cargo-sort-app in_app cargo sort --workspace --grouped --check
step typos typos
case "$mode" in
  pre-commit) step gitleaks gitleaks git --staged --redact --no-banner ;;
  pre-push|full) step gitleaks gitleaks dir . --redact --no-banner ;;
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

# ---- pre-push and full ------------------------------------------------------
# Measures afresh (cargo llvm-cov cleans its old profiles first), so a gate can
# never pass on coverage data left by an earlier commit.
step coverage cargo llvm-cov nextest --workspace --all-features --locked \
  --fail-under-lines "$RUST_COVERAGE_FLOOR"
# nextest does not run doctests; without this they silently never run.
step doctests cargo test --doc --workspace --locked

# Zero surviving mutants. pre-push mutates only the code this change touches
# (the diff from the merge-base with origin/main, uncommitted and untracked
# files included); full mutates the whole core. An unknown base fails closed
# to the whole core. The copies cargo-mutants builds live under target/.
mkdir -p target/mutants-tmp
mutants_diff=target/mutants-tmp/change.diff
if [ "$mode" = pre-push ] && base=$(git merge-base origin/main HEAD 2>/dev/null); then
  git diff "$base" > "$mutants_diff"
  git ls-files --others --exclude-standard -z | while IFS= read -r -d '' f; do
    git diff --no-index /dev/null "$f" >> "$mutants_diff" || true
  done
  step mutants env TMPDIR="$PWD/target/mutants-tmp" cargo mutants --no-shuffle --in-diff "$mutants_diff"
else
  step mutants env TMPDIR="$PWD/target/mutants-tmp" cargo mutants --no-shuffle
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
