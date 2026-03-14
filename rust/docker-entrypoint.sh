#!/usr/bin/env bash
set -euo pipefail

auth_mode="${CODEX_AUTH_MODE:-auto}"
case "$auth_mode" in
  auto|api_key|chatgpt) ;;
  *)
    echo "Unsupported CODEX_AUTH_MODE='$auth_mode' (expected auto|api_key|chatgpt)" >&2
    exit 1
    ;;
esac

mkdir -p /root/.codex

bootstrap_api_key_login() {
  if [[ -n "${OPENAI_API_KEY:-}" ]] && [[ ! -s /root/.codex/auth.json ]]; then
    printf '%s' "$OPENAI_API_KEY" | codex login --with-api-key >/dev/null
  fi
}

case "$auth_mode" in
  auto)
    if [[ -n "${OPENAI_API_KEY:-}" ]]; then
      bootstrap_api_key_login
    fi
    ;;
  api_key)
    bootstrap_api_key_login
    ;;
  chatgpt)
    # Use persisted Codex login state from /root/.codex.
    ;;
esac

exec symphony-rust "$@"
