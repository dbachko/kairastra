#!/usr/bin/env bash
set -euo pipefail

umask 077

DEFAULT_OUTPUT_DIR="reports/public-readiness/$(date -u +%Y%m%dT%H%M%SZ)"

output_dir="$DEFAULT_OUTPUT_DIR"

usage() {
  cat <<'EOF'
Usage:
  public-readiness-audit.sh [--output-dir <path>]

Runs a pre-publication secret-risk audit across the working tree and full git history.

Options:
  --output-dir <path>  Output directory for reports. Default: reports/public-readiness/<timestamp>
  --help               Show this help text.
EOF
}

log() {
  printf '==> %s\n' "$*"
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

require_command() {
  local name="$1"
  command -v "$name" >/dev/null 2>&1 || die "required command not found: $name"
}

line_count() {
  local path="$1"
  if [[ -s "$path" ]]; then
    wc -l < "$path" | tr -d ' '
  else
    printf '0'
  fi
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --output-dir)
      [[ $# -ge 2 ]] || die "--output-dir requires a value"
      output_dir="$2"
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

require_command git
require_command rg

scan_runner=""
if command -v gitleaks >/dev/null 2>&1; then
  scan_runner="native"
elif command -v docker >/dev/null 2>&1; then
  scan_runner="docker"
else
  die "either gitleaks or docker is required"
fi

repo_root="$(git rev-parse --show-toplevel)"
cd "$repo_root"

mkdir -p "$output_dir"

summary_file="${output_dir}/SUMMARY.md"
history_paths_file="${output_dir}/history-paths.txt"
history_path_findings_file="${output_dir}/history-path-findings.txt"
working_tree_findings_file="${output_dir}/working-tree-content-findings.txt"
history_content_findings_file="${output_dir}/history-content-findings.txt"
gitleaks_report_file="${output_dir}/gitleaks.sarif"

secret_pattern='BEGIN (RSA|EC|OPENSSH|DSA)? ?PRIVATE KEY|AKIA[0-9A-Z]{16}|ASIA[0-9A-Z]{16}|ghp_[A-Za-z0-9]{36}|github_pat_[A-Za-z0-9_]{20,}|xox[baprs]-[A-Za-z0-9-]{10,}|sk-[A-Za-z0-9]{20,}'
path_pattern='id_rsa|id_ed25519|\.pem$|\.key$|\.p12$|\.pfx$|cookies\.txt|lastpass|proton|llm-audit|backup|dump|export|\.sqlite$|\.db$'

if [[ "$scan_runner" == "docker" ]]; then
  output_dir_abs="$(cd "$output_dir" && pwd -P)"
  if [[ "$output_dir_abs" != "$repo_root"* ]]; then
    die "when using docker-based gitleaks, --output-dir must be inside repo root: $repo_root"
  fi
fi

log "Running gitleaks full-history scan"
gitleaks_status=0
set +e
if [[ "$scan_runner" == "native" ]]; then
  gitleaks detect \
    --source="$repo_root" \
    --log-opts='--all' \
    --redact \
    --no-banner \
    --report-format sarif \
    --report-path "$gitleaks_report_file"
  gitleaks_status=$?
else
  gitleaks_report_abs="$(cd "$(dirname "$gitleaks_report_file")" && pwd -P)/$(basename "$gitleaks_report_file")"
  docker_report_path="${gitleaks_report_abs#${repo_root}/}"
  docker run --rm -v "$repo_root":/repo zricethezav/gitleaks:latest detect \
    --source=/repo \
    --log-opts='--all' \
    --redact \
    --no-banner \
    --report-format sarif \
    --report-path "/repo/${docker_report_path}"
  gitleaks_status=$?
fi
set -e

log "Collecting historical path inventory"
git log --all --name-only --pretty=format: | sed '/^$/d' | sort -u > "$history_paths_file"
rg -n -i "$path_pattern" "$history_paths_file" > "$history_path_findings_file" || true

log "Scanning working tree for high-confidence secret signatures"
rg -n -S "$secret_pattern" --glob '!.git/**' > "$working_tree_findings_file" || true

log "Scanning full history for high-confidence secret signatures"
git rev-list --all | while read -r commit; do
  git grep -n -I -E "$secret_pattern" "$commit" -- .
done > "$history_content_findings_file" || true

history_path_findings_count="$(line_count "$history_path_findings_file")"
working_tree_findings_count="$(line_count "$working_tree_findings_file")"
history_content_findings_count="$(line_count "$history_content_findings_file")"
commit_count="$(git rev-list --count --all)"

status="PASS"
if [[ "$gitleaks_status" -ne 0 || "$history_path_findings_count" -ne 0 || "$working_tree_findings_count" -ne 0 || "$history_content_findings_count" -ne 0 ]]; then
  status="BLOCK"
fi

cat > "$summary_file" <<EOF
# Public Readiness Audit Summary

- Timestamp (UTC): $(date -u +%Y-%m-%dT%H:%M:%SZ)
- Repo: $(basename "$repo_root")
- HEAD: $(git rev-parse HEAD)
- Commits scanned: ${commit_count}
- Result: ${status}

## Checks

- gitleaks exit code: ${gitleaks_status} (report: \`${gitleaks_report_file}\`)
- history path findings: ${history_path_findings_count} (\`${history_path_findings_file}\`)
- working tree content findings: ${working_tree_findings_count} (\`${working_tree_findings_file}\`)
- history content findings: ${history_content_findings_count} (\`${history_content_findings_file}\`)

## Blocker Policy

This audit should block publication until findings are triaged and explicitly resolved.
EOF

log "Wrote summary: $summary_file"

if [[ "$status" == "BLOCK" ]]; then
  die "public-readiness audit failed; review $summary_file"
fi

log "Public-readiness audit passed"
