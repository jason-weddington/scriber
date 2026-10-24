#!/usr/bin/env bash
# Download Scriber's speech model (ggml large-v3-turbo, about 1.6 GB) into the
# app's data folder, where the app looks for it. The app downloads it itself
# at launch when it is missing; this is the terminal route. Idempotent: a model
# already in place is left alone. Never run by the gate.
#
#   scripts/fetch-model.sh            into the installed app's data folder
#   SCRIBER_DATA_DIR=/x scripts/fetch-model.sh   somewhere else (tests, smoke checks)
set -euo pipefail
cd "$(dirname "$0")/.."

url=https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo.bin
# Pinned like the app's (crates/scriber-core/src/download.rs): the exact size
# and SHA-256. A model already in place is checked by size only (hashing 1.6 GB
# takes a while); a fresh download by both.
bytes=1624555275
sha256=1fc70f774d38eb169993ac391eea357ef47c88757ef72ee5943879b7e8e2bc69

identifier=$(node -p "require('./apps/desktop/src-tauri/tauri.conf.json').identifier")
data=${SCRIBER_DATA_DIR:-"$HOME/Library/Application Support/$identifier"}
model="$data/models/ggml-large-v3-turbo.bin"

size_of() { stat -f %z "$1" 2>/dev/null || echo 0; }

if [ -f "$model" ] && [ "$(size_of "$model")" -eq "$bytes" ]; then
  echo "model already present: $model"
  exit 0
fi

mkdir -p "$(dirname "$model")"
partial="$model.part"
echo "downloading $url"
echo "        to $model"
# -C - resumes an interrupted download of the .part file.
curl -fL --retry 3 -C - -o "$partial" "$url"
size=$(size_of "$partial")
if [ "$size" -ne "$bytes" ]; then
  echo "error: the download is $size bytes; expected $bytes" >&2
  rm -f "$partial"
  exit 1
fi
got=$(shasum -a 256 "$partial" | cut -d' ' -f1)
if [ "$got" != "$sha256" ]; then
  echo "error: the download's SHA-256 is $got; expected $sha256" >&2
  rm -f "$partial"
  exit 1
fi
mv "$partial" "$model"
echo "model ready: $model ($size bytes)"
