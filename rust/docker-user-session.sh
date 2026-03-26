#!/usr/bin/env bash
set -euo pipefail

runtime_dir="${XDG_RUNTIME_DIR:-$HOME/.runtime}"
mkdir -p "$runtime_dir" "$HOME/.cache" "$HOME/.local/share/keyrings"
chmod 700 "$runtime_dir"
export XDG_RUNTIME_DIR="$runtime_dir"

exec bash -lc '
set -euo pipefail

dbus-run-session -- bash -lc '"'"'
set -euo pipefail

runtime_dir="${XDG_RUNTIME_DIR:-$HOME/.runtime}"
mkdir -p "$runtime_dir" "$HOME/.cache" "$HOME/.local/share/keyrings"
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
