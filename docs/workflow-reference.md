# Workflow Reference

Kairastra reads runtime behavior from `WORKFLOW.md`.

## Structure

`WORKFLOW.md` is split into:

1. YAML front matter between leading `---` delimiters
2. A Markdown prompt template body rendered per issue

If the file has no front matter, Kairastra treats the entire file as the prompt body.

## Core sections

### `tracker`

Required for the current Rust runtime:

- `kind: github`
- `mode: projects_v2` or `issues_only`
- `api_key`: usually `$GITHUB_TOKEN`
- `owner`
- `repo` for `issues_only`
- `project_v2_number` for `projects_v2`

Useful optional fields:

- `project_url`
- `status_source`
- `priority_source`
- `active_states`
- `terminal_states`

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

Available environment variables include:

- `ISSUE_ID`
- `ISSUE_IDENTIFIER`
- `ISSUE_TITLE`
- `ISSUE_STATE`

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

## Auth naming

Current accepted auth mode names:

- Env files: `auto`, `subscription`, `api_key`
- CLI login: `subscription`, `api-key`
