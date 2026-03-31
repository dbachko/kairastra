# Troubleshooting

Use these checks when Kairastra setup, auth, or runtime behavior is not matching expectations.

## First checks

If you are using Docker, run:

```bash
cd rust
make docker-doctor
```

If you are not using Docker, run:

```bash
cd rust
cargo run -- doctor --workflow ../WORKFLOW.md --env-file .env
```

For native deployments, point `--env-file` at the native env file or omit it.

## Common failures

### `missing_github_api_token`

Cause:

- `tracker.api_key` resolved to an empty value

Fix:

- set `GITHUB_TOKEN`
- or set `GH_TOKEN`
- verify the env file is loaded
- verify the token has the required `project`/`repo` scopes
- token creation: https://github.com/settings/tokens/new
- setup details: `rust/README.md` -> `GitHub token requirements`

### `github_project_not_found`

Cause:

- wrong `owner` or `project_v2_number`
- token cannot access the Project v2

Fix:

- verify the project URL
- use a classic PAT for user-owned projects
- for org-owned projects, a fine-grained PAT may work if the org exposes the `Projects` permission; if not, use a classic PAT
- authorize the token for SSO if needed
- verify the token includes `project` or `read:project` as appropriate

### Issue is visible in the Project but never dispatches

Cause:

- the Kairastra deployment is scoped to a different repository
- the project contains issues from multiple repositories
- `project_owner` / `project_url` does not match the actual Project v2 owner

Fix:

- verify `tracker.owner` and `tracker.repo` point at the repository this deployment should manage
- verify `tracker.project_owner` or `tracker.project_url` points at the Project v2 owner when it differs from the repository owner
- run a separate Kairastra deployment for each repository represented in the project

### `workspace_hook_failed: after_create|before_run|after_run|before_remove`

Cause:

- a lifecycle hook exited non-zero or timed out

Fix:

- reproduce the command manually in a workspace
- check that required tools like `git`, `rsync`, or provider CLIs exist
- inspect hook stdout/stderr in the error output or runtime logs

### `removed_docker_env_keys`

Cause:

- the Docker env file still contains removed host-bind keys such as `WORKFLOW_FILE` or
  `SEED_REPO_PATH`

Fix:

- rerun `make docker-setup`
- or rerun the remote bootstrap script with `--reconfigure`
- if setup offers to migrate the Docker env file, accept that prompt so it can rewrite the file in the current format
- or remove the stale keys and import/write the deployment config into Docker-managed volumes again

### Repo `WORKFLOW.md` changes do not show up in Docker workspaces

Cause:

- the seed volume still contains an older checkout

Fix:

- run `cd rust && make docker-sync-seed`
- restart the stack with `make docker-up` if needed
- on a remote install, rerun `~/kairastra/repo/scripts/install-remote-docker.sh --reconfigure`

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
