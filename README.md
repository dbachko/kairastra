![Kairastra logo](.github/media/kairastra-logo.svg)

# Kairastra

_Align intent. Launch execution. Land the merge._

Kairastra is a GitHub-native autonomous work runner built from the Symphony service model.

[![Kairastra demo video preview](.github/media/symphony-demo-poster.jpg)](.github/media/symphony-demo.mp4)

_In the [demo video](.github/media/symphony-demo.mp4), Kairastra watches a GitHub work queue,
spins up isolated agent workspaces, drives implementation forward, and leaves behind reviewable
proof of work in pull requests, CI status, and issue updates._

> [!WARNING]
> This repository is still intended for trusted environments.

## What This Repo Is

This is not a generic "build your own Symphony" starter.

This repository contains our own Rust implementation of a Symphony-style orchestrator for GitHub:

- GitHub Issues plus Projects v2 as the work queue
- per-issue isolated workspaces
- multi-provider agent support (`codex` and `claude`)
- issue workpad comments, PR discovery, and review handoff logic
- operator commands for setup, doctor checks, and auth bootstrap

The local service contract lives in [SPEC.md](SPEC.md). It is reconciled against the upstream
[openai/symphony](https://github.com/openai/symphony) specification, but this repo documents and
implements its own GitHub-oriented behavior.

## Why "Kairastra"

`symphony-gh` is an accurate repo slug, but it is not a very good product name.

Kairastra is derived from `kairos`, the opportune moment, and `astra`, the stars. That fits the
actual behavior better: the system waits for the right dispatch moment, launches isolated runs, and
keeps work moving toward review and merge without constant operator supervision.

## Start Here

If you want to run the implementation in this repository, start with:

- [rust/README.md](rust/README.md) for setup, deployment modes, auth, and operations
- [docs/README.md](docs/README.md) for architecture, workflow config, and troubleshooting
- [SPEC.md](SPEC.md) for the repo's normative service contract

The repo slug and several operator-facing paths still use historical `symphony` names. The public
docs use `Kairastra` for the product and call out those legacy names where operators need them.

## Implementation Scope

The current implementation is opinionated around GitHub:

- `tracker.kind: github`
- GitHub Projects v2 as the primary queueing model, with `issues_only` as a fallback
- GitHub-backed workpad comments and PR/check integration
- `WORKFLOW.md` as the in-repo control surface for prompt and runtime behavior

The runtime also supports multiple coding-agent providers through `agent.provider` and
`providers.<name>` workflow config blocks.

## Relationship To Upstream Symphony

This project was implemented from the upstream Symphony specification and keeps that relationship
explicit:

- Upstream project: [openai/symphony](https://github.com/openai/symphony)
- Upstream spec baseline: tracked in [SPEC.md](SPEC.md)

The goal here is not to mirror upstream branding or every upstream implementation detail. The goal
is to ship a clean GitHub-native orchestration service that follows the Symphony model where it
helps and diverges where this implementation needs stronger GitHub-specific behavior.

## License

This project is licensed under the [Apache License 2.0](LICENSE).
