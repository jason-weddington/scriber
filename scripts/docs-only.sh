#!/usr/bin/env bash
# Exits 0 only when the change is docs-only (docs/** and top-level *.md), so
# scripts/gate.sh can skip its heavy steps for it. Fail-safe: any error, an
# empty changeset or a single non-docs path (deletions included) exits 1 and
# the heavy steps run.
#   scripts/docs-only.sh staged   the index (pre-commit)
#   scripts/docs-only.sh push     HEAD against its merge-base with origin/main (pre-push)
set -uo pipefail

case "${1:-}" in
  staged) files=$(git diff --cached --name-only) || exit 1 ;;
  push)
    base=$(git merge-base origin/main HEAD 2>/dev/null) || exit 1
    files=$(git diff --name-only "$base" HEAD) || exit 1
    ;;
  *) exit 1 ;;
esac

[ -n "$files" ] || exit 1
if printf '%s\n' "$files" | grep -qvE '^(docs/|[^/]*\.md$)'; then
  exit 1
fi
echo "docs-only change: skipping the heavy steps" >&2
exit 0
