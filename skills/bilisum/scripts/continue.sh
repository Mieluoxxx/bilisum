#!/usr/bin/env bash
set -euo pipefail

session_id="${1:-}"
mode="${2:-}"

args=(-c)
if [[ -n "$session_id" ]]; then
  args+=("$session_id")
fi
if [[ -n "$mode" ]]; then
  args+=(--mode "$mode")
fi

exec bilisum "${args[@]}"
