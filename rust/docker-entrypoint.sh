#!/usr/bin/env bash
set -euo pipefail

codex_auth_mode="${CODEX_AUTH_MODE:-auto}"
claude_auth_mode="${CLAUDE_AUTH_MODE:-auto}"
symphony_user="${SYMPHONY_USER:-symphony}"
symphony_home="${SYMPHONY_HOME:-/home/symphony}"
workspace_root="${SYMPHONY_WORKSPACE_ROOT:-/workspaces}"
codex_auth_dir="/var/lib/symphony-auth/codex"
claude_auth_dir="/var/lib/symphony-auth/claude"
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

run_as_symphony() {
  HOME="$symphony_home" \
  USER="$symphony_user" \
  LOGNAME="$symphony_user" \
  CLAUDE_CONFIG_DIR="${CLAUDE_CONFIG_DIR:-$symphony_home/.claude}" \
    gosu "$symphony_user" "$@"
}

ensure_runtime_home() {
  mkdir -p "$symphony_home" "$workspace_root" "$codex_auth_dir" "$claude_auth_dir"
  mkdir -p "$symphony_home/.local/bin"

  if [[ ! -e "$symphony_home/.codex" ]]; then
    ln -s "$codex_auth_dir" "$symphony_home/.codex"
  fi

  if [[ ! -e "$symphony_home/.claude" ]]; then
    ln -s "$claude_auth_dir" "$symphony_home/.claude"
  fi

  if [[ ! -e "$symphony_home/.claude.json" ]]; then
    ln -s "$claude_auth_dir/.claude.json" "$symphony_home/.claude.json"
  fi

  chown -R "$symphony_user:$symphony_user" \
    "$workspace_root" \
    "$symphony_home" \
    "$codex_auth_dir" \
    "$claude_auth_dir"

  if [[ -n "${CLAUDE_CODE_OAUTH_TOKEN:-}" ]]; then
    printf '%s' "$CLAUDE_CODE_OAUTH_TOKEN" > "$claude_oauth_token_file"
    chmod 600 "$claude_oauth_token_file"
    chown "$symphony_user:$symphony_user" "$claude_oauth_token_file"
  elif [[ -s "$claude_oauth_token_file" ]]; then
    export CLAUDE_CODE_OAUTH_TOKEN="$(cat "$claude_oauth_token_file")"
  fi
}

bootstrap_api_key_login() {
  if [[ -n "${OPENAI_API_KEY:-}" ]] && [[ ! -s "$symphony_home/.codex/auth.json" ]]; then
    printf '%s' "$OPENAI_API_KEY" | run_as_symphony codex login --with-api-key >/dev/null
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
    # Use persisted Codex login state from $SYMPHONY_HOME/.codex.
    ;;
  chatgpt)
    # Use persisted Codex login state from $SYMPHONY_HOME/.codex.
    ;;
esac

if [[ $# -eq 0 ]]; then
  set -- run
fi

export HOME="$symphony_home"
export USER="$symphony_user"
export LOGNAME="$symphony_user"
export CLAUDE_CONFIG_DIR="${CLAUDE_CONFIG_DIR:-$symphony_home/.claude}"
export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-$symphony_home/.runtime}"
export PATH="/home/${symphony_user}/.local/bin:${PATH}"

mkdir -p "$XDG_RUNTIME_DIR"
chmod 700 "$XDG_RUNTIME_DIR"
chown "$symphony_user:$symphony_user" "$XDG_RUNTIME_DIR"

exec gosu "$symphony_user" /usr/local/bin/docker-user-session.sh symphony-rust "$@"
