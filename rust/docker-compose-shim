#!/usr/bin/env bash
set -euo pipefail

if [[ "${1:-}" == "compose" ]]; then
  shift
  exec /usr/bin/docker-compose "$@"
fi

exec /usr/bin/docker "$@"
