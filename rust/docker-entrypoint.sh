#!/usr/bin/env bash
set -euo pipefail

codex_auth_mode="${CODEX_AUTH_MODE:-auto}"
claude_auth_mode="${CLAUDE_AUTH_MODE:-auto}"

case "$codex_auth_mode" in
  auto|api_key|subscription|chatgpt) ;;
  *)
    echo "Unsupported CODEX_AUTH_MODE='$codex_auth_mode' (expected auto|api_key|subscription)" >&2
    exit 1
    ;;
esac

case "$claude_auth_mode" in
  auto|api_key|subscription) ;;
  *)
    echo "Unsupported CLAUDE_AUTH_MODE='$claude_auth_mode' (expected auto|api_key|subscription)" >&2
    exit 1
    ;;
esac

mkdir -p /root/.codex
mkdir -p /root/.claude

bootstrap_api_key_login() {
  if [[ -n "${OPENAI_API_KEY:-}" ]] && [[ ! -s /root/.codex/auth.json ]]; then
    printf '%s' "$OPENAI_API_KEY" | codex login --with-api-key >/dev/null
  fi
}

case "$codex_auth_mode" in
  auto)
    if [[ -n "${OPENAI_API_KEY:-}" ]]; then
      bootstrap_api_key_login
    fi
    ;;
  api_key)
    bootstrap_api_key_login
    ;;
  subscription)
    # Use persisted Codex login state from /root/.codex.
    ;;
  chatgpt)
    # Use persisted Codex login state from /root/.codex.
    ;;
esac

if [[ $# -eq 0 ]]; then
  set -- run
fi

exec symphony-rust "$@"
