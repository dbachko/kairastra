# Workflow Reference

Kairastra reads runtime behavior from `WORKFLOW.md`. In native mode, one deployment workflow owns
both settings and prompt behavior. In Docker mode, the deployment config still comes from a
workflow file, but workspace prompt/hooks can come from repo-root `WORKFLOW.md` files inside the
seeded repository.

## Structure

`WORKFLOW.md` is split into:

1. YAML front matter between leading `---` delimiters
2. A Markdown prompt template body rendered per issue

If the file has no front matter, Kairastra treats the entire file as the prompt body.

## Docker workflow surfaces

Docker deployments have two workflow surfaces:

- Deployment workflow: stored in `/config/WORKFLOW.md`, usually written by `make docker-setup` or
  the remote bootstrap script. This file owns tracker settings, polling, workspace root,
  provider defaults, and other deployment-scoped configuration.
- Repo workflow: optional `WORKFLOW.md` at the root of the seeded repository inside each
  workspace. This file owns the workspace prompt body plus repo-local lifecycle hooks.

Repo workflow rules in Docker mode:

- Only the prompt body and `hooks.after_create`, `hooks.before_run`, `hooks.after_run`, and
  `hooks.before_remove` are accepted.
- Any other front matter fields are rejected as invalid repo workflow config.
- If the file is absent, Kairastra falls back to its built-in default repo workflow.
- Repo prompt/hooks replace the deployment prompt/hooks for Docker workers. Deployment config still
  owns tracker, auth, queue, and other global settings.

Operator implications:

- After changing repo-root `WORKFLOW.md`, run `make docker-sync-seed` so the seed volume matches
  the checkout Kairastra will clone from.
- After changing deployment config, run `make docker-setup` or `make docker-config-import` so
  `/config/WORKFLOW.md` is refreshed.

## Core sections

### `tracker`

Required for the current Rust runtime:

- `kind: github`
- `mode: projects_v2` or `issues_only`
- `api_key`: usually `$GITHUB_TOKEN`
- `owner`: repository owner
- `repo`: repository name
- `project_v2_number` for `projects_v2`

Useful optional fields:

- `project_owner`: GitHub user or org that owns the Project v2 when it differs from the repository owner
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
- `issues_only` is the repo-first mode. Kairastra polls `owner/repo` issues directly.
- `projects_v2` is still repo-scoped. Kairastra reads items from the configured project, then only dispatches issues that belong to the configured repository.
- If one GitHub Project contains issues from multiple repositories, run one Kairastra deployment per repository instead of sharing one runtime.

### Project status behavior

For `projects_v2`, Kairastra is now workflow-driven on both the read and write sides.

- `status_source` selects the Project field used as the issue state source.
- `active_states` controls which states remain dispatchable.
- `terminal_states` controls which states are treated as finished.
- `claimable_states` controls which active states are treated as ready-to-claim queues and which states participate in dependency gating for `blocked_by`.
- `in_progress_state` is the Project status Kairastra writes when it claims an issue.
- `human_review_state` is the Project status Kairastra writes when it hands an issue back for operator review or blocked follow-up.
- `done_state` is the Project status Kairastra writes when it reconciles a closed GitHub issue.
- Set any of `in_progress_state`, `human_review_state`, or `done_state` to `null` to disable that automatic Project mutation.

Example:

```yaml
tracker:
  status_source:
    type: project_field
    name: Status
  active_states: ["Ready", "Doing", "Needs Review"]
  terminal_states: ["Complete", "Closed"]
  claimable_states: ["Ready"]
  in_progress_state: "Doing"
  human_review_state: "Needs Review"
  done_state: "Complete"
```

Recommended operator paths:

- Default and safest: keep the existing Project statuses and generate a matching workflow.
- Optional: normalize the Project's `Status` field to Kairastra's canonical options when you want the standard Kairastra workflow.

Setup behavior:

- Interactive `setup` inspects the configured Project `Status` field when a GitHub token is available.
- The default choice is `Keep existing Project statuses (recommended)`, which does not mutate GitHub.
- `Normalize Project to Kairastra statuses` is a destructive action. Setup requires typed confirmation and refuses to rewrite a live Project when items already exist in statuses that would be changed or removed.
- Non-interactive setup never normalizes Project statuses.

Doctor behavior:

- `doctor` validates that configured `active_states`, `terminal_states`, `claimable_states`, and any non-null transition targets exist in the configured Project status field.

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

Mode-specific behavior:

- Native mode runs hooks from the deployment workflow directly.
- Docker mode first performs Kairastra's internal workspace bootstrap from `/seed-repo`, then runs
  repo-local hooks from the workspace root `WORKFLOW.md` when present.

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
- `gemini`

## Auth naming

Current accepted auth mode names:

- Env files: `auto`, `subscription`, `api_key`
- CLI login: `subscription`, `api-key`
