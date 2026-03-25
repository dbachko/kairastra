---
tracker:
  kind: github
  mode: projects_v2
  api_key: $GITHUB_TOKEN
  owner: $KAIRASTRA_GITHUB_OWNER
  repo: $KAIRASTRA_GITHUB_REPO
  project_v2_number: $KAIRASTRA_GITHUB_PROJECT_NUMBER
  project_url: $KAIRASTRA_GITHUB_PROJECT_URL
  status_source:
    type: project_field
    name: Status
  priority_source:
    type: project_field
    name: Priority
  active_states:
    - Todo
    - In Progress
    - Merging
    - Rework
  terminal_states:
    - Closed
    - Cancelled
    - Canceled
    - Duplicate
    - Done
workspace:
  root: $KAIRASTRA_WORKSPACE_ROOT
hooks:
  after_create: |
    set -euo pipefail

    clone_with_auth() {
      clone_url="$1"
      if [ -n "${GITHUB_TOKEN:-}" ] && printf '%s' "$clone_url" | grep -q '^https://github.com/'; then
        auth_header="$(printf 'x-access-token:%s' "$GITHUB_TOKEN" | base64 | tr -d '\n')"
        git -c http.extraheader="Authorization: Basic ${auth_header}" clone --depth 1 "$clone_url" .
        git config http.https://github.com/.extraheader "Authorization: Basic ${auth_header}"
      else
        git clone --depth 1 "$clone_url" .
      fi
    }

    overlay_seed_repo() {
      seed_repo="$1"
      if command -v rsync >/dev/null 2>&1; then
        rsync -a --delete --exclude '.git' "${seed_repo}/" ./
      else
        echo "rsync is required when overlaying KAIRASTRA_SEED_REPO on top of a remote clone." >&2
        exit 1
      fi
    }

    if [ -n "${KAIRASTRA_GIT_CLONE_URL:-}" ]; then
      clone_with_auth "$KAIRASTRA_GIT_CLONE_URL"
      if [ -n "${KAIRASTRA_SEED_REPO:-}" ] && [ -d "$KAIRASTRA_SEED_REPO" ]; then
        overlay_seed_repo "$KAIRASTRA_SEED_REPO"
      fi
    elif [ -n "${KAIRASTRA_SEED_REPO:-}" ] && [ -d "$KAIRASTRA_SEED_REPO/.git" ]; then
      git clone "$KAIRASTRA_SEED_REPO" .
    else
      echo "Set KAIRASTRA_GIT_CLONE_URL, or point KAIRASTRA_SEED_REPO at a git checkout, before running Kairastra." >&2
      exit 1
    fi

    if [ -n "${KAIRASTRA_GIT_PUSH_URL:-}" ]; then
      git remote set-url origin "$KAIRASTRA_GIT_PUSH_URL"
    fi

    git config user.name "${KAIRASTRA_GIT_AUTHOR_NAME:-Kairastra}"
    git config user.email "${KAIRASTRA_GIT_AUTHOR_EMAIL:-kairastra@users.noreply.github.com}"
agent:
  provider: codex
  max_concurrent_agents: 4
  max_turns: 20
providers:
  codex:
    command: codex app-server
    approval_policy: never
    thread_sandbox: workspace-write
    turn_sandbox_policy:
      type: workspaceWrite
      networkAccess: true
  claude:
    command: claude
    model: $KAIRASTRA_CLAUDE_MODEL
    reasoning_effort: $KAIRASTRA_CLAUDE_REASONING_EFFORT
    approval_policy: never
  gemini:
    command: gemini
    model: $KAIRASTRA_GEMINI_MODEL
    approval_mode: $KAIRASTRA_GEMINI_APPROVAL_MODE
---

You are working on GitHub issue `{{ issue.identifier }}`.

{% if tracker.dashboard_url %}
Dashboard: {{ tracker.dashboard_url }}
{% endif %}
Title: {{ issue.title }}
URL: {{ issue.url }}

Description:
{% if issue.description %}
{{ issue.description }}
{% else %}
No description provided.
{% endif %}
