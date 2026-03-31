#!/usr/bin/env bash
set -euo pipefail

codex_auth_mode="${CODEX_AUTH_MODE:-auto}"
claude_auth_mode="${CLAUDE_AUTH_MODE:-auto}"
gemini_auth_mode="${GEMINI_AUTH_MODE:-auto}"
kairastra_user="${KAIRASTRA_USER:-kairastra}"
kairastra_home="${KAIRASTRA_HOME:-/home/kairastra}"
workspace_root="${KAIRASTRA_WORKSPACE_ROOT:-/workspaces}"
tool_cache_root="${KAIRASTRA_TOOL_CACHE_ROOT:-/tmp/kairastra}"
xdg_cache_home="${XDG_CACHE_HOME:-$tool_cache_root/xdg-cache}"
corepack_home="${COREPACK_HOME:-$tool_cache_root/corepack}"
pnpm_home="${PNPM_HOME:-$tool_cache_root/pnpm}"
npm_config_cache="${NPM_CONFIG_CACHE:-$tool_cache_root/npm-cache}"
config_root="/config"
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

seed_gemini_cli_defaults() {
  local gemini_settings_file="$gemini_auth_dir/settings.json"
  local gemini_trusted_folders_file="$gemini_auth_dir/trustedFolders.json"
  local tmp_file

  tmp_file="$(mktemp)"
  if [[ -f "$gemini_settings_file" ]]; then
    if jq '.general = ((.general // {}) + {"enableAutoUpdate": false, "enableAutoUpdateNotification": false})' "$gemini_settings_file" > "$tmp_file" 2>/dev/null; then
      mv "$tmp_file" "$gemini_settings_file"
    else
      rm -f "$tmp_file"
    fi
  else
    cat > "$tmp_file" <<'EOF'
{
  "general": {
    "enableAutoUpdate": false,
    "enableAutoUpdateNotification": false
  }
}
EOF
    mv "$tmp_file" "$gemini_settings_file"
  fi

  tmp_file="$(mktemp)"
  if [[ -f "$gemini_trusted_folders_file" ]]; then
    if jq 'if has("/app") then . else . + {"/app":"TRUST_FOLDER"} end' "$gemini_trusted_folders_file" > "$tmp_file" 2>/dev/null; then
      mv "$tmp_file" "$gemini_trusted_folders_file"
    else
      rm -f "$tmp_file"
    fi
  else
    cat > "$tmp_file" <<'EOF'
{
  "/app": "TRUST_FOLDER"
}
EOF
    mv "$tmp_file" "$gemini_trusted_folders_file"
  fi

  chmod 600 "$gemini_settings_file" "$gemini_trusted_folders_file" 2>/dev/null || true
}

sync_codex_seed_skills() {
  local seed_repo="${KAIRASTRA_SEED_REPO:-}"
  local seed_skills_dir="$seed_repo/.codex/skills"
  local target_skills_dir="$codex_auth_dir/skills"

  if [[ -z "$seed_repo" ]]; then
    return 0
  fi

  if [[ ! -d "$seed_skills_dir" ]]; then
    rm -rf "$target_skills_dir"
    return 0
  fi

  mkdir -p "$target_skills_dir"
  if command -v rsync >/dev/null 2>&1; then
    rsync -a --delete "$seed_skills_dir/" "$target_skills_dir/"
  else
    rm -rf "$target_skills_dir"
    mkdir -p "$target_skills_dir"
    cp -R "$seed_skills_dir/." "$target_skills_dir/"
  fi
}

ensure_safe_runtime_dir() {
  local dir_path="$1"
  local env_name="$2"

  if [[ "$dir_path" == /tmp/* || "$dir_path" == /var/tmp/* || "$dir_path" == "$kairastra_home"/* ]]; then
    return 0
  fi

  echo "$env_name must point inside /tmp, /var/tmp, or $kairastra_home (got '$dir_path')" >&2
  exit 1
}

ensure_runtime_home() {
  ensure_safe_runtime_dir "$tool_cache_root" "KAIRASTRA_TOOL_CACHE_ROOT"
  ensure_safe_runtime_dir "$xdg_cache_home" "XDG_CACHE_HOME"
  ensure_safe_runtime_dir "$corepack_home" "COREPACK_HOME"
  ensure_safe_runtime_dir "$pnpm_home" "PNPM_HOME"
  ensure_safe_runtime_dir "$npm_config_cache" "NPM_CONFIG_CACHE"

  mkdir -p \
    "$kairastra_home" \
    "$workspace_root" \
    "$config_root" \
    "$codex_auth_dir" \
    "$claude_auth_dir" \
    "$gemini_auth_dir" \
    "$tool_cache_root" \
    "$xdg_cache_home" \
    "$corepack_home" \
    "$pnpm_home" \
    "$npm_config_cache"
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

  # ~/.claude.json can legitimately be a symlink to a file that Claude has not
  # created yet. In that case `-e` is false, but we still must not recreate the
  # link on every startup.
  if [[ ! -e "$kairastra_home/.claude.json" && ! -L "$kairastra_home/.claude.json" ]]; then
    ln -s "$claude_auth_dir/.claude.json" "$kairastra_home/.claude.json"
  fi

  seed_gemini_cli_defaults
  sync_codex_seed_skills

  chown -R "$kairastra_user:$kairastra_user" \
    "$workspace_root" \
    "$config_root" \
    "$kairastra_home" \
    "$tool_cache_root" \
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
export XDG_CACHE_HOME="$xdg_cache_home"
export COREPACK_HOME="$corepack_home"
export PNPM_HOME="$pnpm_home"
export NPM_CONFIG_CACHE="$npm_config_cache"
export PATH="/home/${kairastra_user}/.local/bin:${PATH}"

mkdir -p "$XDG_RUNTIME_DIR"
chmod 700 "$XDG_RUNTIME_DIR"
chown "$kairastra_user:$kairastra_user" "$XDG_RUNTIME_DIR"

exec gosu "$kairastra_user" /usr/local/bin/docker-user-session.sh kairastra "$@"
