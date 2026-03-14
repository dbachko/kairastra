---
tracker:
  kind: github
  mode: issues_only
  api_key: $GITHUB_TOKEN
  owner: dbachko
  repo: symphony-gh
  active_states:
    - Open
  terminal_states:
    - Closed
polling:
  interval_ms: 5000
workspace:
  root: $SYMPHONY_WORKSPACE_ROOT
hooks:
  after_create: |
    auth_header="$(printf 'x-access-token:%s' "$GITHUB_TOKEN" | base64 | tr -d '\n')"

    if [ -n "$SYMPHONY_SEED_REPO" ] && [ -d "$SYMPHONY_SEED_REPO/.git" ]; then
      git clone "$SYMPHONY_SEED_REPO" .
      git remote set-url origin https://github.com/dbachko/symphony-gh.git
    else
      git -c http.extraheader="Authorization: Basic ${auth_header}" clone --depth 1 https://github.com/dbachko/symphony-gh.git .
    fi

    git config http.https://github.com/.extraheader "Authorization: Basic ${auth_header}"
    git config user.name "Symphony Rust"
    git config user.email "symphony-rust@users.noreply.github.com"
agent:
  max_concurrent_agents: 1
  max_turns: 10
codex:
  command: codex app-server
  approval_policy: never
  thread_sandbox: workspace-write
  turn_sandbox_policy:
    type: workspaceWrite
    networkAccess: true
  read_timeout_ms: 5000
  turn_timeout_ms: 1800000
  stall_timeout_ms: 300000
---

You are working on GitHub issue `{{ issue.identifier }}` in the `dbachko/symphony-gh` repository.

{% if attempt %}
Continuation context:

- This is retry attempt #{{ attempt }} because the issue is still in an active state.
- Resume from the current workspace instead of restarting from scratch.
- Do not repeat already-completed investigation or validation unless it is required by new changes.
{% endif %}

Issue context:
Identifier: {{ issue.identifier }}
Title: {{ issue.title }}
Current status: {{ issue.state }}
Labels: {{ issue.labels }}
URL: {{ issue.url }}

Description:
{% if issue.description %}
{{ issue.description }}
{% else %}
No description provided.
{% endif %}

Instructions:

1. This is an unattended orchestration session. Never ask a human to perform follow-up actions.
2. Work only inside the provided workspace clone of this repository.
3. Prefer small, targeted changes with direct validation.
4. If you are blocked by missing required auth, permissions, or secrets, post a concise issue comment explaining the blocker and then stop.
5. Final turn output must summarize completed actions and blockers only.

## Rust workflow constraints

- The current Rust implementation supports GitHub issues plus the injected `github_graphql` and `github_rest` tools.
- Do not assume Linear, workpad comments, PR attachment metadata, or merge automation are available.
- If you need to communicate progress back to GitHub, use issue comments via `github_rest`.
- Treat the GitHub issue state as the source of truth for whether work is still active.

## Default execution flow

1. Read the issue carefully and identify the smallest complete implementation or validation step.
2. Reproduce the problem or confirm the requested behavior before making changes when practical.
3. Make the necessary code or documentation changes in the workspace.
4. Run focused validation for the touched scope.
5. Post a concise issue comment with what changed and what was validated.
6. If the task is fully complete, close the issue with `github_rest`. Otherwise leave it open.

## Complex E2E protocol

If the issue title starts with `Symphony Rust Complex E2E`, run this protocol instead of the default flow:

1. Treat the issue body as the single source of truth for progress.
2. Replace the issue body with:
   - the original feature request content
   - a `## Codex Workpad` section
   - a short `Plan` section with a 4-6 item markdown checklist
   - an `Acceptance Criteria` checklist
   - a `Validation` checklist
   - a `Notes` section
3. Create a new branch from `main` named `symphony-rust-e2e/<issue-number>-<short-slug>`.
4. Work through the checklist in order. After each meaningful milestone, update the same issue body and check off completed items.
5. Use `github_rest` to post concise progress comments when you finish planning, when implementation is done, and when validation passes.
6. Run focused validation for the changed scope and record the exact commands and outcomes in the issue body's `Validation` section.
7. Commit the completed changes, push the branch to `origin`, and create a pull request with `github_rest` against `main`.
8. Post a final issue comment containing the branch name, commit SHA, PR URL, and validation summary.
9. Close the issue with `github_rest`.
10. Do not ask for human input unless GitHub auth or required secrets are missing.

## E2E smoke-test protocol

If the issue title starts with `Symphony Rust E2E`, run this exact protocol instead of the default flow:

1. In the workspace root, create a file named `symphony_rust_e2e.txt` with exactly these lines:
   issue={{ issue.identifier }}
   title={{ issue.title }}
   status=ok
2. Use `github_rest` to post this exact comment on the issue:
   `Symphony Rust E2E smoke test complete.`
3. Use `github_rest` to close the current issue by setting its GitHub issue state to `closed`.
4. Do not commit, push, or open a pull request.
5. Do not modify any files other than `symphony_rust_e2e.txt`.
