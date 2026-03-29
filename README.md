![Kairastra logo](.github/media/kairastra-logo.svg)

# Kairastra

_Align intent. Launch execution. Land the merge._

Kairastra is a GitHub-native autonomous work runner for continuous issue execution.

![Kairastra board](.github/media/kairastra-board.png)

_In this screen, Kairastra shows a GitHub work queue, isolated agent workspaces, and reviewable
proof of work across pull requests, CI status, and issue updates._

> [!WARNING]
> This repository is still intended for trusted environments.

## What This Repo Is

This is not a generic issue-bot starter.

This repository contains the Rust implementation of Kairastra for GitHub:

- one deployment per repository, with GitHub Issues or Projects v2 as the queue
- per-issue isolated workspaces
- multi-provider agent support (`codex`, `claude`, and `gemini`)
- issue workpad comments, PR discovery, and review handoff logic
- operator commands for setup, doctor checks, and auth bootstrap

The local service contract lives in [SPEC.md](SPEC.md) and documents the runtime behavior
implemented in this repository.

## Why "Kairastra"

Kairastra is derived from `kairos`, the opportune moment, and `astra`, the stars. That fits the
actual behavior better: the system waits for the right dispatch moment, launches isolated runs, and
keeps work moving toward review and merge without constant operator supervision.

## Start Here

If you want to run the implementation in this repository, start with:

- [rust/README.md](rust/README.md) for setup, deployment modes, auth, and operations
- [docs/README.md](docs/README.md) for architecture, workflow config, and troubleshooting
- [SPEC.md](SPEC.md) for the repo's normative service contract

## Implementation Scope

The current implementation is opinionated around GitHub:

- `tracker.kind: github`
- one runtime manages one repository checkout and one repository push target
- `issues_only` is the simplest queue model; `projects_v2` is optional when you want a repo-scoped project queue
- GitHub-backed workpad comments and PR/check integration
- `WORKFLOW.md` as the in-repo control surface for prompt and runtime behavior

The runtime also supports multiple coding-agent providers through `agent.provider` and
`providers.<name>` workflow config blocks.

## License

This project is licensed under the [Apache License 2.0](LICENSE).
