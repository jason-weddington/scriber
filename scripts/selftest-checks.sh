#!/usr/bin/env bash
# Prove the custom checks catch what they claim to. A check that relies on the
# ABSENCE of something proves nothing unless something was put there: each
# case copies the tree, requires the check to pass on the clean copy, plants
# one violation, and requires the check to fail.
set -uo pipefail
repo=$(cd "$(dirname "$0")/.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
fail=0

# A copy of the tracked and untracked (not ignored) files, plus a link to the
# installed node_modules so the eslint case needs no install.
fresh() {
  rm -rf "$work/t" && mkdir -p "$work/t"
  (cd "$repo" && git ls-files -co --exclude-standard -z | tar --null -T - -cf -) | tar -C "$work/t" -xf -
  if [ -d "$repo/apps/desktop/node_modules" ]; then
    ln -s "$repo/apps/desktop/node_modules" "$work/t/apps/desktop/node_modules"
  fi
}

# expect_fail <name> <check command> <plant command> <what the failure must say>
# The last argument is a regex the check's output must match, so a check that
# fails for some other reason (a broken plant, a missing tool) is not
# mistaken for one that caught the violation.
expect_fail() {
  local name=$1 check=$2 plant=$3 want=$4 out
  fresh
  if ! out=$(cd "$work/t" && bash -c "$check" 2>&1); then
    echo "selftest: '$check' fails on the clean tree (case: $name):" >&2
    echo "$out" >&2; fail=1; return
  fi
  if ! (cd "$work/t" && bash -c "$plant") >/dev/null 2>&1; then
    echo "selftest: could not plant: $name" >&2; fail=1; return
  fi
  if out=$(cd "$work/t" && bash -c "$check" 2>&1); then
    echo "selftest: '$check' MISSED a planted violation: $name" >&2; fail=1
  elif ! grep -qE "$want" <<<"$out"; then
    echo "selftest: '$check' failed, but not for '$name':" >&2
    echo "$out" >&2; fail=1
  else
    echo "selftest: ok: $name" >&2
  fi
}

arch=scripts/check-architecture.sh
core=crates/scriber-core
engine=crates/scriber-engine
app=apps/desktop/src-tauri

# A local stand-in named tokio: planting the real crate would need it in the
# registry cache, which a fresh machine or CI runner does not have.
plant_fake_tokio() {
  mkdir -p fake-tokio/src &&
    printf '[package]\nname = "tokio"\nversion = "1.0.0"\nedition = "2024"\npublish = false\n' > fake-tokio/Cargo.toml &&
    : > fake-tokio/src/lib.rs &&
    perl -pi -e 's/^thiserror = .*$/$&\ntokio = { path = "..\/..\/fake-tokio" }/' "$1/Cargo.toml" &&
    cargo update --offline -q
}
export -f plant_fake_tokio
expect_fail "the core depends on tokio" "$arch" \
  "plant_fake_tokio $core" \
  'scriber-core depends on tokio'
expect_fail "the engine depends on tokio" "$arch" \
  "plant_fake_tokio $engine" \
  'scriber-engine depends on tokio'
expect_fail "the core reads a file directly" "$arch" \
  "printf 'fn f() { let _ = std::fs::read(\"x\"); }\n' >> $core/src/dictation.rs" \
  'direct I/O in the core'
expect_fail "the core reads the clock directly" "$arch" \
  "printf 'fn f() { let _ = std::time::Instant::now(); }\n' >> $core/src/dictation.rs" \
  'direct I/O in the core'
expect_fail "a logic module in src-tauri" "$arch" \
  "printf '//! logic\n' > $app/src/rules.rs" \
  'more than lib.rs and main.rs'
expect_fail "a test in src-tauri" "$arch" \
  "printf '#[cfg(test)]\nmod tests {}\n' >> $app/src/lib.rs" \
  'tests in src-tauri'
expect_fail "src-tauri grows past the limit" "$arch" \
  "for i in \$(seq 1 510); do echo '// filler' >> $app/src/lib.rs; done" \
  'limit 500'
expect_fail "a component imports Tauri" "$arch" \
  "printf \"import { invoke } from '@tauri-apps/api/core';\\n\" >> apps/desktop/src/lib/format.ts" \
  'only src/lib/bridge.ts may import'
expect_fail "the mock stops identifying itself" "$arch" \
  "perl -pi -e \"s/dataset.bridge = 'mock'/dataset.bridge = 'real'/\" apps/desktop/src/lib/bridge.ts" \
  'data-bridge'
expect_fail "an allow attribute" "$arch" \
  "printf '#[allow(dead_code)]\nfn g() {}\n' >> $core/src/dictation.rs" \
  'not #\[allow\]'
expect_fail "a crate-level allow attribute" "$arch" \
  "perl -0pi -e 's/^/#![allow(missing_docs)]\n/' $app/src/main.rs" \
  'not #\[allow\]'

# The eslint rule that mirrors check 4, so an editor shows it as you type.
expect_fail "eslint: a component imports Tauri" \
  "cd apps/desktop && node_modules/.bin/eslint --max-warnings 0 src/lib/format.ts" \
  "printf \"import { invoke } from '@tauri-apps/api/core';\\nexport const x = invoke;\\n\" >> apps/desktop/src/lib/format.ts" \
  'no-restricted-imports'

# The docs-only skip fails safe: a code path anywhere in the change runs the
# heavy steps.
docs_case() {
  fresh
  # A hook run exports the outer repo's GIT_INDEX_FILE (e.g. under
  # `git commit -a`); unset it, after fresh has read the outer index, so the
  # scratch repo uses its own. This case runs last.
  unset GIT_DIR GIT_INDEX_FILE GIT_WORK_TREE
  (cd "$work/t" && git init -q && git add docs/design.md README.md) || { fail=1; return; }
  if ! (cd "$work/t" && scripts/docs-only.sh staged 2>/dev/null); then
    echo "selftest: docs-only.sh does not skip a docs-only change" >&2; fail=1
  fi
  (cd "$work/t" && git add "$core/src/dictation.rs")
  if (cd "$work/t" && scripts/docs-only.sh staged 2>/dev/null); then
    echo "selftest: docs-only.sh MISSED a code file in the change" >&2; fail=1
  else
    echo "selftest: ok: docs-only.sh fails safe" >&2
  fi
}
docs_case

exit $fail
