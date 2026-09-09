#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 1 || $# -gt 2 ]]; then
  echo "用法：$0 <model-file> [huggingface|hf-mirror]" >&2
  exit 1
fi

model_file="$1"
mirror="${2:-hf-mirror}"
exec bilisum models download "$model_file" --mirror "$mirror"
