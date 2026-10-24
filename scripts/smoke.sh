#!/usr/bin/env bash
# Real-model smoke check, never run by the gate: speak a sentence with `say`,
# convert it to 16 kHz mono 16-bit WAV with ffmpeg, and transcribe it through
# the engine's Whisper adapter (crates/scriber-engine/examples/smoke.rs).
# Needs the model (scripts/fetch-model.sh) and ffmpeg (brew install ffmpeg).
#
#   scripts/smoke.sh ["sentence to speak"]
set -euo pipefail
cd "$(dirname "$0")/.."

sentence=${1:-"Scriber turns a paused dictation into paragraphs. Please open the agent GTD board and dispatch the talos run before lunch."}
identifier=$(node -p "require('./apps/desktop/src-tauri/tauri.conf.json').identifier")
data=${SCRIBER_DATA_DIR:-"$HOME/Library/Application Support/$identifier"}
model="$data/models/ggml-large-v3-turbo.bin"
[ -f "$model" ] || { echo "error: no model at $model; run scripts/fetch-model.sh" >&2; exit 1; }

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
say -o "$work/speech.aiff" "$sentence"
ffmpeg -loglevel error -i "$work/speech.aiff" -ar 16000 -ac 1 -c:a pcm_s16le "$work/speech.wav"

echo "spoken: \"$sentence\""
cargo run --quiet --release --locked -p scriber-engine --example smoke -- "$model" "$work/speech.wav"
