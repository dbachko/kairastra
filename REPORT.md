# Repository Work Report

## Executive Summary

The repository has moved from an earlier Elixir-based implementation toward a GitHub-focused Rust
orchestration runtime. The current checked-in state provides a working orchestration skeleton with
GitHub Issues and Projects v2 integration, workspace lifecycle management, prompt rendering, and a
Codex app-server runner, plus a Docker-based local workflow for running the service.

## Work Completed

### 1. GitHub-oriented Rust orchestration runtime

The current Rust implementation covers the main orchestration path described in the repository
documentation:

- workflow loading from [`WORKFLOW.md`](WORKFLOW.md)
- typed runtime configuration for GitHub tracker settings
- GitHub Issues and Projects v2 tracker integration
- per-issue workspace creation and lifecycle hooks
- prompt rendering for agent sessions
- Codex app-server execution support
- polling orchestration with retries and reconciliation

Primary implementation files:

- `rust/src/config.rs`
- `rust/src/github.rs`
- `rust/src/orchestrator.rs`
- `rust/src/prompt.rs`
- `rust/src/runner.rs`
- `rust/src/workflow.rs`
- `rust/src/workspace.rs`

### 2. Containerized local development flow

The repository includes a Docker workflow for building and running the Rust service locally. The
checked-in container setup includes:

- `rust/Dockerfile`
- `rust/compose.yml`
- `rust/Makefile`
- `rust/docker-entrypoint.sh`
- `rust/docker-compose-shim`
- `rust/.env.example`

This matches the documented `make docker-build`, `make docker-up`, `make docker-down`,
`make docker-logs`, and `make docker-login` flow in [`rust/README.md`](rust/README.md).

### 3. Repository and workflow documentation

The repo already contains the core documentation needed to explain and operate the current system:

- [`README.md`](README.md) describes Symphony at a high level and points users to the Rust
  implementation.
- [`SPEC.md`](SPEC.md) defines the service goals, architecture, and domain model.
- [`WORKFLOW.md`](WORKFLOW.md) contains the checked-in tracker/runtime policy and worker prompt.
- [`rust/README.md`](rust/README.md) documents the implemented Rust runtime and operator workflow.

## Recent Milestones From Commit History

The recent history shows a clear implementation arc:

- `583be77` introduced the Rust GitHub orchestration runtime.
- `806d05b` and `a55dc28` hardened Docker-oriented end-to-end behavior.
- `d29a840`, `440da66`, `e0c8e9d`, and `a120387` improved the container and workspace workflow.
- `7d9df6b` refactored the repository to a GitHub-only Rust orchestration focus.

This sequence indicates that the major implementation work is already present and that current work
has focused on stability, operability, and narrowing the repository around the Rust runtime.

## Current Repository Shape

At the top level, the project currently contains:

- high-level docs and workflow configuration at the repository root
- the active Rust implementation under `rust/`
- a bootstrap script at `scripts/bootstrap_github_project.py`

Within `rust/src/`, the codebase is organized around the main runtime concerns:

- `app_server.rs`
- `config.rs`
- `github.rs`
- `main.rs`
- `model.rs`
- `orchestrator.rs`
- `prompt.rs`
- `runner.rs`
- `workflow.rs`
- `workspace.rs`

## Conclusion

The repository already contains substantial completed work. The most important completed outcome is
the GitHub-only Rust orchestration runtime together with the supporting documentation and Docker
workflow needed to run and validate it. This report captures that current state in a single
reviewable document.
