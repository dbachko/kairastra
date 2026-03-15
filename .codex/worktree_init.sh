#!/usr/bin/env bash
set -eo pipefail

script_dir="$(cd "$(dirname "$0")" && pwd)"
repo_root="$(cd "$script_dir/.." && pwd)"
project_root="$repo_root/rust"

if ! command -v cargo >/dev/null 2>&1; then
  echo "cargo is required. Install Rust from https://rustup.rs/" >&2
  exit 1
fi

cd "$project_root"
cargo fetch
