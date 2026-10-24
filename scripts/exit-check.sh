#!/usr/bin/env bash
# Mac exit check, never run by the gate: open the engine on the real model,
# let it warm up (a Metal context on macOS), shut it down and exit(0) with the
# engine alive (crates/scriber-engine/examples/exit_check.rs). Without the
# shutdown, whisper.cpp's Metal device aborts at exit (status 134), which is
# the "Scriber quit unexpectedly" crash. Needs the model (scripts/fetch-model.sh).
#
#   scripts/exit-check.sh [--control]
#
# --control also runs without the shutdown and requires that run to abort
# (134), proving the check can tell the two apart.
set -euo pipefail
cd "$(dirname "$0")/.."

control=false
case "${1:-}" in
  "") ;;
  --control) control=true ;;
  *) echo "usage: scripts/exit-check.sh [--control]" >&2; exit 2 ;;
esac

identifier=$(node -p "require('./apps/desktop/src-tauri/tauri.conf.json').identifier")
data=${SCRIBER_DATA_DIR:-"$HOME/Library/Application Support/$identifier"}
model="$data/models/ggml-large-v3-turbo.bin"
[ -f "$model" ] || { echo "error: no model at $model; run scripts/fetch-model.sh" >&2; exit 1; }

set +e
cargo run --quiet --release --locked -p scriber-engine --example exit_check -- "$model"
status=$?
set -e
echo "exit status: $status"

if [ "$control" = true ]; then
  set +e
  cargo run --quiet --release --locked -p scriber-engine --example exit_check -- "$model" --no-shutdown
  control_status=$?
  set -e
  echo "control exit status (no shutdown, expected 134): $control_status"
  [ "$status" -eq 0 ] && [ "$control_status" -eq 134 ] || exit 1
  exit 0
fi

[ "$status" -eq 0 ] || exit 1
