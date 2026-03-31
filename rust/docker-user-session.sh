#!/usr/bin/env bash
set -euo pipefail

runtime_dir="${XDG_RUNTIME_DIR:-$HOME/.runtime}"
tool_cache_root="${KAIRASTRA_TOOL_CACHE_ROOT:-/tmp/kairastra}"
export XDG_CACHE_HOME="${XDG_CACHE_HOME:-$tool_cache_root/xdg-cache}"
export COREPACK_HOME="${COREPACK_HOME:-$tool_cache_root/corepack}"
export PNPM_HOME="${PNPM_HOME:-$tool_cache_root/pnpm}"
export NPM_CONFIG_CACHE="${NPM_CONFIG_CACHE:-$tool_cache_root/npm-cache}"
mkdir -p \
  "$runtime_dir" \
  "$HOME/.local/share/keyrings" \
  "$XDG_CACHE_HOME" \
  "$COREPACK_HOME" \
  "$PNPM_HOME" \
  "$NPM_CONFIG_CACHE"
chmod 700 "$runtime_dir"
export XDG_RUNTIME_DIR="$runtime_dir"

should_preserve_terminal=false
if [[ "${1:-}" == "kairastra" ]]; then
  case "${2:-}" in
    auth|setup)
      should_preserve_terminal=true
      ;;
  esac
fi

if [[ "$should_preserve_terminal" == "true" ]]; then
  exec dbus-run-session -- bash -lc '
set -euo pipefail

runtime_dir="${XDG_RUNTIME_DIR:-$HOME/.runtime}"
tool_cache_root="${KAIRASTRA_TOOL_CACHE_ROOT:-/tmp/kairastra}"
export XDG_CACHE_HOME="${XDG_CACHE_HOME:-$tool_cache_root/xdg-cache}"
export COREPACK_HOME="${COREPACK_HOME:-$tool_cache_root/corepack}"
export PNPM_HOME="${PNPM_HOME:-$tool_cache_root/pnpm}"
export NPM_CONFIG_CACHE="${NPM_CONFIG_CACHE:-$tool_cache_root/npm-cache}"
mkdir -p \
  "$runtime_dir" \
  "$HOME/.local/share/keyrings" \
  "$XDG_CACHE_HOME" \
  "$COREPACK_HOME" \
  "$PNPM_HOME" \
  "$NPM_CONFIG_CACHE"
chmod 700 "$runtime_dir"
export XDG_RUNTIME_DIR="$runtime_dir"

if command -v gnome-keyring-daemon >/dev/null 2>&1; then
  keyring_password="${KAIRASTRA_CLAUDE_KEYRING_PASSWORD:-}"
  eval "$(printf "%s\n" "$keyring_password" | gnome-keyring-daemon --unlock 2>/dev/null)"
  eval "$(printf "%s\n" "$keyring_password" | gnome-keyring-daemon --start --components=secrets 2>/dev/null)"
fi

exec "$@"
' bash "$@"
fi

exec bash -lc '
set -euo pipefail

dbus-run-session -- bash -lc '"'"'
set -euo pipefail

runtime_dir="${XDG_RUNTIME_DIR:-$HOME/.runtime}"
tool_cache_root="${KAIRASTRA_TOOL_CACHE_ROOT:-/tmp/kairastra}"
export XDG_CACHE_HOME="${XDG_CACHE_HOME:-$tool_cache_root/xdg-cache}"
export COREPACK_HOME="${COREPACK_HOME:-$tool_cache_root/corepack}"
export PNPM_HOME="${PNPM_HOME:-$tool_cache_root/pnpm}"
export NPM_CONFIG_CACHE="${NPM_CONFIG_CACHE:-$tool_cache_root/npm-cache}"
mkdir -p \
  "$runtime_dir" \
  "$HOME/.local/share/keyrings" \
  "$XDG_CACHE_HOME" \
  "$COREPACK_HOME" \
  "$PNPM_HOME" \
  "$NPM_CONFIG_CACHE"
chmod 700 "$runtime_dir"
export XDG_RUNTIME_DIR="$runtime_dir"

if command -v gnome-keyring-daemon >/dev/null 2>&1; then
  keyring_password="${KAIRASTRA_CLAUDE_KEYRING_PASSWORD:-}"
  eval "$(printf "%s\n" "$keyring_password" | gnome-keyring-daemon --unlock 2>/dev/null)"
  eval "$(printf "%s\n" "$keyring_password" | gnome-keyring-daemon --start --components=secrets 2>/dev/null)"
fi

exec "$@"
'"'"' bash "$@" 2> >(
  while IFS= read -r line; do
    if [[ "$line" == dbus-run-session:\ ignoring\ unknown\ child\ process\ * ]]; then
      continue
    fi
    printf "%s\n" "$line" >&2
  done
)
' bash "$@"
