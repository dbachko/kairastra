#!/usr/bin/env bash
set -euo pipefail

codex_auth_mode="${CODEX_AUTH_MODE:-auto}"
claude_auth_mode="${CLAUDE_AUTH_MODE:-auto}"
gemini_auth_mode="${GEMINI_AUTH_MODE:-auto}"
kairastra_user="${KAIRASTRA_USER:-kairastra}"
kairastra_home="${KAIRASTRA_HOME:-/home/kairastra}"
workspace_root="${KAIRASTRA_WORKSPACE_ROOT:-/workspaces}"
codex_auth_dir="/var/lib/kairastra-auth/codex"
claude_auth_dir="/var/lib/kairastra-auth/claude"
gemini_auth_dir="/var/lib/kairastra-auth/gemini"
claude_oauth_token_file="$claude_auth_dir/oauth-token"

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

case "$gemini_auth_mode" in
  auto|api_key|subscription) ;;
  *)
    echo "Unsupported GEMINI_AUTH_MODE='$gemini_auth_mode' (expected auto|api_key|subscription)" >&2
    exit 1
    ;;
esac

run_as_kairastra() {
  HOME="$kairastra_home" \
  USER="$kairastra_user" \
  LOGNAME="$kairastra_user" \
  CLAUDE_CONFIG_DIR="${CLAUDE_CONFIG_DIR:-$kairastra_home/.claude}" \
    gosu "$kairastra_user" "$@"
}

ensure_runtime_home() {
  mkdir -p "$kairastra_home" "$workspace_root" "$codex_auth_dir" "$claude_auth_dir" "$gemini_auth_dir"
  mkdir -p "$kairastra_home/.local/bin"

  if [[ ! -e "$kairastra_home/.codex" ]]; then
    ln -s "$codex_auth_dir" "$kairastra_home/.codex"
  fi

  if [[ ! -e "$kairastra_home/.claude" ]]; then
    ln -s "$claude_auth_dir" "$kairastra_home/.claude"
  fi

  if [[ ! -e "$kairastra_home/.gemini" ]]; then
    ln -s "$gemini_auth_dir" "$kairastra_home/.gemini"
  fi

  if [[ ! -e "$kairastra_home/.claude.json" ]]; then
    ln -s "$claude_auth_dir/.claude.json" "$kairastra_home/.claude.json"
  fi

  chown -R "$kairastra_user:$kairastra_user" \
    "$workspace_root" \
    "$kairastra_home" \
    "$codex_auth_dir" \
    "$claude_auth_dir" \
    "$gemini_auth_dir"

  if [[ -n "${CLAUDE_CODE_OAUTH_TOKEN:-}" ]]; then
    printf '%s' "$CLAUDE_CODE_OAUTH_TOKEN" > "$claude_oauth_token_file"
    chmod 600 "$claude_oauth_token_file"
    chown "$kairastra_user:$kairastra_user" "$claude_oauth_token_file"
  elif [[ -s "$claude_oauth_token_file" ]]; then
    export CLAUDE_CODE_OAUTH_TOKEN="$(cat "$claude_oauth_token_file")"
  fi
}

bootstrap_api_key_login() {
  if [[ -n "${OPENAI_API_KEY:-}" ]] && [[ ! -s "$kairastra_home/.codex/auth.json" ]]; then
    printf '%s' "$OPENAI_API_KEY" | run_as_kairastra codex login --with-api-key >/dev/null
  fi
}

ensure_runtime_home

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
    # Use persisted Codex login state from $KAIRASTRA_HOME/.codex.
    ;;
  chatgpt)
    # Use persisted Codex login state from $KAIRASTRA_HOME/.codex.
    ;;
esac

if [[ $# -eq 0 ]]; then
  set -- run
fi

export HOME="$kairastra_home"
export USER="$kairastra_user"
export LOGNAME="$kairastra_user"
export CLAUDE_CONFIG_DIR="${CLAUDE_CONFIG_DIR:-$kairastra_home/.claude}"
export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-$kairastra_home/.runtime}"
export PATH="/home/${kairastra_user}/.local/bin:${PATH}"

mkdir -p "$XDG_RUNTIME_DIR"
chmod 700 "$XDG_RUNTIME_DIR"
chown "$kairastra_user:$kairastra_user" "$XDG_RUNTIME_DIR"

exec gosu "$kairastra_user" /usr/local/bin/docker-user-session.sh kairastra "$@"
