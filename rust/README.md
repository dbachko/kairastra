# Symphony Rust

This directory contains a Rust implementation of Symphony oriented around GitHub Issues and Projects v2, following [`SPEC.md`](../SPEC.md) and the GitHub design notes in `syphony-gh.md`.

## What is implemented

- `WORKFLOW.md` loader with YAML front matter, prompt body parsing, and last-known-good reload behavior.
- Typed runtime config for `tracker.kind: github`.
- GitHub tracker adapter with `projects_v2` as the primary mode and `issues_only` as a fallback.
- Per-issue workspace creation, deterministic workspace keys, and lifecycle hooks.
- Liquid prompt rendering.
- Codex app-server client with the required `initialize -> initialized -> thread/start -> turn/start` handshake.
- Polling orchestrator with in-memory claims, reconciliation, continuation retries, and exponential backoff retries.

## Run

```bash
cd rust
cargo test
cargo run -- /path/to/WORKFLOW.md
```

Use `cargo run -- --once /path/to/WORKFLOW.md` to execute a single orchestration tick.

You can also set `WORKFLOW_PATH=/path/to/WORKFLOW.md` instead of passing a positional CLI argument.

## Docker

Use the Make targets for the container flow:

```bash
cd rust
cp .env.example .env
make docker-build
make docker-up
```

Available targets:

- `make docker-build`: build the `symphony-rust` image from `rust/compose.yml`.
- `make docker-up`: start the stack in detached mode with rebuild.
- `make docker-down`: stop and remove the stack.
- `make docker-logs`: follow service logs.
- `make docker-login`: run interactive `codex login` in the running container.

`docker-login` is for ChatGPT subscription/device-auth flows (`CODEX_AUTH_MODE=chatgpt` or `auto` without `OPENAI_API_KEY`). Run it after `make docker-up` so auth state is stored in the persisted `/root/.codex` volume.

Files added for the container flow:

- `rust/Dockerfile`
- `rust/compose.yml`
- `rust/.env.example`

Important notes:

- Fill in `GITHUB_TOKEN` in `rust/.env`.
- Set `WORKFLOW_FILE` in `rust/.env` to the host path of the workflow file you want mounted into the container.
- Set `SEED_REPO_PATH` in `rust/.env` to the host path of the repository copy that should be cloned into per-issue workspaces. The default `..` works when you run Compose from `rust/` inside this repo.
- Your workflow should usually use `workspace.root: $SYMPHONY_WORKSPACE_ROOT` so the same file works inside Docker.
- The Compose setup mounts the seed repo at `/seed-repo`, and the checked-in `WORKFLOW.md` prefers cloning from that local mount before falling back to GitHub.
- `CODEX_AUTH_MODE` controls Codex auth bootstrap in the container:
  - `auto` (default): if `OPENAI_API_KEY` is set, bootstrap API-key login; otherwise rely on persisted login state.
  - `api_key`: bootstrap from `OPENAI_API_KEY` only.
  - `chatgpt`: skip API-key bootstrap and use persisted Codex login state only.
- Compose persists `/root/.codex` with the `symphony_rust_codex` volume so login survives restarts.
- The runtime image installs `codex`, `gh`, `make`, `docker`, and `docker-compose`, and it reuses the Rust toolchain from the builder stage so workspace `cargo` commands match the app build toolchain.
- `docker compose ...` inside the container is shimmed to Debian's `docker-compose` binary. That is enough for config-oriented validation; mount the host Docker socket separately if you want worker turns to build or run sibling containers.

### Headless device-auth (Docker ChatGPT subscription mode)

1. Set `CODEX_AUTH_MODE=chatgpt` (or `auto` with no `OPENAI_API_KEY`) in `rust/.env`.
2. Start the stack once: `make -C rust docker-up`.
3. Run device login inside the running container: `make -C rust docker-login`.
4. Complete the device code flow from a browser.
5. Restart normally; Compose keeps `/root/.codex` persisted.

### Headless device-auth (VPS without Docker)

1. SSH to the VPS and run `codex login`.
2. Complete the device code flow in your browser.
3. Start Symphony Rust on the VPS; Codex uses the saved login from `~/.codex`.

### API-key mode

- Set `OPENAI_API_KEY` and use `CODEX_AUTH_MODE=api_key` (or `auto`).
- On first start, the container runs `codex login --with-api-key` and stores auth under `/root/.codex`.

## Minimal GitHub workflow

```md
---
tracker:
  kind: github
  api_key: $GITHUB_TOKEN
  owner: your-org
  mode: projects_v2
  project_v2_number: 7
  active_states: ["Todo", "In Progress"]
  terminal_states: ["Done", "Closed", "Cancelled", "Duplicate"]
  status_source:
    type: project_field
    name: Status
  priority_source:
    type: project_field
    name: Priority
workspace:
  root: $SYMPHONY_WORKSPACE_ROOT
hooks:
  after_create: |
    git clone https://github.com/your-org/your-repo.git .
agent:
  max_concurrent_agents: 4
  max_turns: 20
codex:
  command: codex app-server
---

You are working on {{ issue.identifier }}.

Title: {{ issue.title }}
{% if issue.description %}
Body:
{{ issue.description }}
{% endif %}
```

## Notes

- This is a first Rust port, not a full parity rewrite of the Elixir implementation.
- The current code targets local workers only.
- GitHub dynamic tools are limited to `github_graphql` and a small `github_rest` allow-list.
