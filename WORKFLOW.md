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
    git -c http.extraheader="Authorization: Basic ${auth_header}" clone --depth 1 https://github.com/dbachko/symphony-gh.git .
agent:
  max_concurrent_agents: 1
  max_turns: 3
codex:
  command: codex app-server
  approval_policy: never
  thread_sandbox: workspace-write
  turn_sandbox_policy:
    type: workspaceWrite
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
