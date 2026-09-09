#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 1 || $# -gt 2 ]]; then
  echo "用法：$0 <video-url> [mode]" >&2
  exit 1
fi

url="$1"
if [[ $# -eq 2 ]]; then
  exec bilisum "$url" --mode "$2"
fi
exec bilisum "$url"
