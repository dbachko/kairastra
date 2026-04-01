#!/usr/bin/env bash
set -euo pipefail

DEFAULT_REPO_URL="${KAIRASTRA_REPO_URL:-https://github.com/dbachko/kairastra.git}"
DEFAULT_REF="${KAIRASTRA_REF:-main}"
DEFAULT_INSTALL_ROOT="${KAIRASTRA_INSTALL_ROOT:-${HOME}/.local}"
DEFAULT_SOURCE_ROOT="${XDG_DATA_HOME:-${HOME}/.local/share}/kairastra-src"

usage() {
  cat <<'EOF'
Usage:
  install.sh [--ref <git-ref>] [--repo <git-url>] [--install-root <path>] [--source-root <path>]

Build and install Kairastra natively with Cargo.

Options:
  --ref <git-ref>         Git ref to install from when cloning remotely. Default: main
  --repo <git-url>        Repository to clone when not running from a local checkout.
  --install-root <path>   Cargo install root. Default: ~/.local
  --source-root <path>    Managed source checkout path. Default: ~/.local/share/kairastra-src
  --help                  Show this help text.
EOF
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

require_command() {
  command -v "$1" >/dev/null 2>&1 || die "required command not found: $1"
}

resolve_script_dir() {
  if [[ -n "${BASH_SOURCE[0]:-}" && -f "${BASH_SOURCE[0]}" ]]; then
    (
      cd "$(dirname "${BASH_SOURCE[0]}")"
      pwd -P
    )
    return 0
  fi

  return 1
}

sync_repo_checkout() {
  local repo_url="$1"
  local git_ref="$2"
  local source_root="$3"

  mkdir -p "$(dirname "$source_root")"

  if [[ -d "$source_root/.git" ]]; then
    printf '==> Updating managed source checkout in %s\n' "$source_root"
    git -C "$source_root" remote set-url origin "$repo_url"
    git -C "$source_root" fetch --tags --prune origin
  else
    printf '==> Cloning %s into %s\n' "$repo_url" "$source_root"
    rm -rf "$source_root"
    git clone -- "$repo_url" "$source_root"
  fi

  if git -C "$source_root" show-ref --verify --quiet "refs/remotes/origin/$git_ref"; then
    git -C "$source_root" checkout --force -B "$git_ref" "origin/$git_ref"
  else
    git -C "$source_root" checkout --force "$git_ref"
  fi
}

install_krstr_wrapper() {
  local install_root="$1"
  local bin_dir="${install_root}/bin"
  local wrapper_path="${bin_dir}/krstr"

  mkdir -p "$bin_dir"
  cat > "$wrapper_path" <<EOF
#!/usr/bin/env bash
set -euo pipefail
exec "${bin_dir}/kairastra" "\$@"
EOF
  chmod 755 "$wrapper_path"
}

main() {
  local repo_url="$DEFAULT_REPO_URL"
  local git_ref="$DEFAULT_REF"
  local install_root="$DEFAULT_INSTALL_ROOT"
  local source_root="$DEFAULT_SOURCE_ROOT"
  local script_dir=""
  local repo_source=""

  while [[ $# -gt 0 ]]; do
    case "$1" in
      --ref)
        [[ $# -ge 2 ]] || die "--ref requires a value"
        git_ref="$2"
        shift 2
        ;;
      --repo)
        [[ $# -ge 2 ]] || die "--repo requires a value"
        repo_url="$2"
        shift 2
        ;;
      --install-root)
        [[ $# -ge 2 ]] || die "--install-root requires a value"
        install_root="$2"
        shift 2
        ;;
      --source-root)
        [[ $# -ge 2 ]] || die "--source-root requires a value"
        source_root="$2"
        shift 2
        ;;
      --help|-h)
        usage
        exit 0
        ;;
      *)
        die "unknown argument: $1"
        ;;
    esac
  done

  require_command cargo
  require_command git

  if script_dir="$(resolve_script_dir)" && [[ -f "$script_dir/rust/Cargo.toml" ]]; then
    repo_source="$script_dir"
    printf '==> Installing from local checkout at %s\n' "$repo_source"
  else
    sync_repo_checkout "$repo_url" "$git_ref" "$source_root"
    repo_source="$source_root"
  fi

  printf '==> Building and installing Kairastra into %s\n' "$install_root"
  cargo install \
    --locked \
    --force \
    --path "$repo_source/rust" \
    --root "$install_root"

  install_krstr_wrapper "$install_root"

  printf '\nInstalled binaries:\n'
  printf '  %s/bin/kairastra\n' "$install_root"
  printf '  %s/bin/krstr\n' "$install_root"
  printf '\nNext steps:\n'
  printf '  1. Add %s/bin to PATH if needed.\n' "$install_root"
  printf '  2. Run `krstr setup` inside the repo you want to automate.\n'
  printf '  3. Run `krstr auth menu` to initialize provider auth.\n'
  printf '  4. Run `krstr doctor` from that repo.\n'
}

main "$@"
