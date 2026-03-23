# Troubleshooting

## First checks

Run:

```bash
cd rust
cargo run -- doctor --workflow ../WORKFLOW.md --env-file .env
```

If you are not using Docker, point `--env-file` at the native env file or omit it.

## Common failures

### `missing_github_api_token`

Cause:

- `tracker.api_key` resolved to an empty value

Fix:

- set `GITHUB_TOKEN`
- verify the env file is loaded
- verify the token has the required `project`/`repo` scopes

### `github_project_not_found`

Cause:

- wrong `owner` or `project_v2_number`
- token cannot access the Project v2

Fix:

- verify the project URL
- use a classic PAT for user-owned projects
- authorize the token for SSO if needed

### `workspace_hook_failed: after_create|before_run|after_run|before_remove`

Cause:

- a lifecycle hook exited non-zero or timed out

Fix:

- reproduce the command manually in a workspace
- check that required tools like `git`, `rsync`, or provider CLIs exist
- inspect hook stdout/stderr in the error output or runtime logs

### `command=codex not found in PATH` or `command=claude not found in PATH`

Cause:

- provider CLI missing from the host/container

Fix:

- install the provider CLI
- rerun `doctor`

### Auth mode confusion

Fix:

- use `subscription` for device/browser login mode
- use `api_key` in env files and `api-key` in CLI flags

### `run --once` exits but issue still needs more work

Cause:

- `--once` performs a single dispatch pass only

Fix:

- run the long-lived `run` command for daemon behavior
- or invoke `run --once` again to pick up deferred continuations/retries
