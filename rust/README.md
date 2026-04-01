# Kairastra Rust Runtime

This directory contains Kairastra's operator-facing Rust runtime: an opinionated, GitHub-focused
implementation of [OpenAI Symphony](https://github.com/openai/symphony) for GitHub Issues and
Projects v2.

Use the repo-root [README.md](../README.md) for product overview. Use this README when you want to
run or operate the Rust implementation directly.

## What This Runtime Does

- Loads `WORKFLOW.md` front matter plus prompt template.
- Uses the repo-root `WORKFLOW.md` as the canonical template when generating repo-local workflows.
- Polls GitHub Issues or Projects v2 for work in one repository.
- Creates deterministic per-issue workspaces.
- Runs the configured provider runtime for each issue.
- Exposes `setup`, `doctor`, and `auth` operator commands.

## Native Setup

Build and install locally:

```bash
cargo install --path rust --locked --force --root ~/.local
```

Generate config:

```bash
kairastra setup
kairastra auth menu
kairastra doctor
```

By default, `kairastra setup` writes local operator state under `.kairastra/`, adds that directory
to local Git ignore rules, copies the required Kairastra workflow skills into `.agents/skills/`
after confirmation, scaffolds repo support dirs when needed for workspace bootstrap, and inspects
the GitHub labels and Project fields the workflow expects. Use `--bootstrap-github` to let a
non-interactive run apply those GitHub changes automatically.

Run it:

```bash
kairastra run
```

## Main Commands

| Command | What it does |
| --- | --- |
| `kairastra run` | Start the orchestrator loop using repo-root `WORKFLOW.md` by default. |
| `kairastra run --once` | Run one dispatch pass and wait for started workers before exit. |
| `kairastra setup` | Run the guided setup flow for the current Git repo. |
| `kairastra setup --reconfigure` | Re-run setup and overwrite the generated local Kairastra files for the current repo. |
| `kairastra doctor` | Validate workflow, GitHub connectivity, local commands, and auth state. |
| `kairastra auth status` | Show provider auth status. |
| `kairastra auth login --mode subscription` | Run subscription or browser login for the default provider. |
| `kairastra auth login --mode api-key` | Configure API-key auth for the default provider. |
| `kairastra auth menu` | Open the provider auth menu. |

## What Setup Produces

- `WORKFLOW.md`
- `.kairastra/kairastra.env`
- `.kairastra/kairastra.service` on Linux hosts
- `.kairastra/` added to `.gitignore`
- `.github/.gitkeep` when the repo needs a minimal support directory for workspace bootstrap

## Where To Look Next

- [../docs/auth.md](../docs/auth.md): GitHub token scopes, provider auth modes, and login flows
- [../docs/deployment.md](../docs/deployment.md): native deployment and service setup
- [../docs/operations.md](../docs/operations.md): doctor checks, day-2 operations, and limitations
- [../docs/workflow-reference.md](../docs/workflow-reference.md): `WORKFLOW.md` structure and runtime rules
- [../docs/troubleshooting.md](../docs/troubleshooting.md): common setup and runtime failures
- [../docs/architecture.md](../docs/architecture.md): runtime internals
- [../SPEC.md](../SPEC.md): normative behavior
