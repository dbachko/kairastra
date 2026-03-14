#!/usr/bin/env bash
set -euo pipefail

if [[ -n "${OPENAI_API_KEY:-}" ]]; then
  mkdir -p /root/.codex
  if [[ ! -s /root/.codex/auth.json ]]; then
    printf '%s' "$OPENAI_API_KEY" | codex login --with-api-key >/dev/null
  fi
fi

exec symphony-rust "$@"
