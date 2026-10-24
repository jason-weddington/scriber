#!/usr/bin/env bash
# Build the release app, verify its signature and install it in
# /Applications for a person to open. An agent session builds and installs;
# it never launches the app (docs/tauri-design-notes.md says why).
#
# The bundle is signed ad hoc (signingIdentity "-" in tauri.conf.json), which
# is enough to run on the machine that built it.
set -euo pipefail
cd "$(dirname "$0")/.."

title=$(node -p "require('./apps/desktop/src-tauri/tauri.conf.json').productName")
target="/Applications/$title.app"

# Replacing a running app's bundle leaves the running copy reading files that
# have changed under it.
if pgrep -f "$target/Contents/MacOS/" >/dev/null; then
  echo "error: $title is running; quit it, then run this again" >&2
  exit 1
fi

(cd apps/desktop && pnpm install --frozen-lockfile --prefer-offline && pnpm tauri build --bundles app)

built="apps/desktop/target/release/bundle/macos/$title.app"
[ -d "$built" ] || { echo "error: the build did not produce $built" >&2; exit 1; }
codesign --verify --deep --strict "$built"

# Copy beside the old one, then swap, so a failed copy never leaves a
# half-installed app behind.
rm -rf "$target.new" "$target.old"
ditto "$built" "$target.new"
[ -d "$target" ] && mv "$target" "$target.old"
mv "$target.new" "$target"
rm -rf "$target.old"
echo "installed $target"
