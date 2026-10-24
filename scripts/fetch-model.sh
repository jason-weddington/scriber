#!/usr/bin/env bash
# Download Scriber's speech model (ggml large-v3-turbo, about 1.6 GB) into the
# app's data folder, where the app looks for it. Idempotent: a model already
# in place is left alone. Never run by the gate.
#
#   scripts/fetch-model.sh            into the installed app's data folder
#   SCRIBER_DATA_DIR=/x scripts/fetch-model.sh   somewhere else (tests, smoke checks)
set -euo pipefail
cd "$(dirname "$0")/.."

url=https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo.bin
# The real file is 1,624,555,275 bytes; anything far smaller is an error page
# or a truncated download.
min_bytes=1500000000

identifier=$(node -p "require('./apps/desktop/src-tauri/tauri.conf.json').identifier")
data=${SCRIBER_DATA_DIR:-"$HOME/Library/Application Support/$identifier"}
model="$data/models/ggml-large-v3-turbo.bin"

size_of() { stat -f %z "$1" 2>/dev/null || echo 0; }

if [ -f "$model" ] && [ "$(size_of "$model")" -ge "$min_bytes" ]; then
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
if [ "$size" -lt "$min_bytes" ]; then
  echo "error: the download is only $size bytes; expected at least $min_bytes" >&2
  exit 1
fi
mv "$partial" "$model"
echo "model ready: $model ($size bytes)"
