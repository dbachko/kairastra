# Workflow Reference

Kairastra reads runtime behavior from `WORKFLOW.md`. One deployment workflow owns both runtime
settings and prompt behavior.

The repo-root `WORKFLOW.md` is the canonical template shipped with Kairastra. `krstr setup`
generates repo-root `WORKFLOW.md` by combining setup-specific front matter with the canonical
prompt body from that root template.

## Structure

`WORKFLOW.md` is split into:

1. YAML front matter between leading `---` delimiters
2. A Markdown prompt template body rendered per issue

If the file has no front matter, Kairastra treats the entire file as the prompt body.

## Core Sections

### `tracker`

Required for the current Rust runtime:

- `kind: github`
- `mode: projects_v2` or `issues_only`
- `api_key`: usually `$GITHUB_TOKEN`
- `owner`: repository owner
- `repo`: repository name
- `project_v2_number` for `projects_v2`

Useful optional fields:

- `project_owner`
- `project_url`
- `status_source`
- `priority_source`
- `active_states`
- `terminal_states`
- `claimable_states`
- `in_progress_state`
- `human_review_state`
- `done_state`

Practical model:

- One workflow should target one repository.
- `issues_only` is the repo-first mode. Kairastra polls `owner/repo` issues directly and uses
  label-backed workflow states for routing and handoff.
- `projects_v2` is still repo-scoped. Kairastra reads items from the configured project, then only dispatches issues that belong to the configured repository.

### `polling`

- `interval_ms`

Default: `30000`

### `workspace`

- `root`

Default: OS temp dir plus `kairastra_workspaces` if omitted.

### `hooks`

Supported hooks:

- `after_create`
- `before_run`
- `after_run`
- `before_remove`
- `timeout_ms`

Hooks run in the issue workspace with `CARGO_HOME` redirected to `<workspace>/.cargo-home`.

### `agent`

- `provider`
- `max_concurrent_agents`
- `max_turns`
- `max_retry_backoff_ms`
- `assignee_login`
- `max_concurrent_agents_by_state`

### `providers`

The selected `agent.provider` must exist as a mapping under `providers`.

Current built-in providers:

- `codex`
- `claude`
- `gemini`

## Auth Naming

Current accepted auth mode names:

- Env files: `auto`, `subscription`, `api_key`
- CLI login: `subscription`, `api-key`
