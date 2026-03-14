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

Build and run with Docker Compose:

```bash
cd rust
cp .env.example .env
docker compose up --build
```

Files added for the container flow:

- `rust/Dockerfile`
- `rust/compose.yml`
- `rust/.env.example`

Important notes:

- Fill in `GITHUB_TOKEN` and `OPENAI_API_KEY` in `rust/.env`.
- Set `WORKFLOW_FILE` in `rust/.env` to the host path of the workflow file you want mounted into the container.
- Your workflow should usually use `workspace.root: $SYMPHONY_WORKSPACE_ROOT` so the same file works inside Docker.
- The runtime image installs `codex` and starts `symphony-rust` directly.

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
