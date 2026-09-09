#!/usr/bin/env bash
set -euo pipefail

if [[ $# -eq 0 ]]; then
  exec bilisum cache ls
fi

if [[ "$1" == "-h" || "$1" == "--help" ]]; then
  exec bilisum cache --help
fi

exec bilisum cache "$@"
