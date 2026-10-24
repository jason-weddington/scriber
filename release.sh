#!/usr/bin/env bash
# Release at a meaningful boundary, decoupled from every commit. Derives the
# SemVer bump from conventional-commit history (cog), sets the version in
# every manifest and refreshes both lockfiles (cog.toml pre_bump_hooks),
# writes CHANGELOG.md, tags, and pushes to origin with the tag.
#
# A maintainer may keep a private pre-publish check as ./release.guard.local.sh
# (gitignored, like every *.local.* file). When it exists it runs before
# anything is pushed, with PUBLIC_GUARD_REMOTE=origin, and any non-zero exit
# aborts the release.
set -euo pipefail
cd "$(dirname "$0")"

git diff --quiet && git diff --cached --quiet || { echo "error: the working tree is dirty" >&2; exit 1; }
[ "$(git rev-parse --abbrev-ref HEAD)" = "main" ] || { echo "error: not on main" >&2; exit 1; }
git fetch -q origin
git merge-base --is-ancestor origin/main HEAD || { echo "error: behind origin/main; pull first" >&2; exit 1; }

if [ -f ./release.guard.local.sh ]; then
  PUBLIC_GUARD_REMOTE=origin bash ./release.guard.local.sh || { echo "error: release.guard.local.sh refused the release" >&2; exit 1; }
else
  echo "release: no release.guard.local.sh, skipping the local guard" >&2
fi

cog bump --auto
git push origin main --follow-tags
