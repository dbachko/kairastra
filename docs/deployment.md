# Deployment Guide

This page covers the supported way to deploy and operate Kairastra's Rust runtime.

## Deployment Boundary

One Kairastra deployment manages one repository.

- The runtime bootstraps each issue workspace from one configured local repository checkout.
- PR discovery, check summaries, and workpad writes all happen against that repository.
- `projects_v2` can be used as the queue for that repository, but Kairastra still ignores project
  items from other repositories.
- If you need automation across multiple repositories, run multiple Kairastra services.

## Native Install

Install Kairastra natively with Cargo:

```bash
curl -fsSL https://raw.githubusercontent.com/dbachko/kairastra/main/install.sh | bash
```

Requirements:

- Rust and Cargo
- `git`
- `gh`
- the provider CLI you plan to use: `codex`, `claude`, or `gemini`

## Native Service Setup

Generate the workflow, env file, and service unit:

```bash
kairastra setup
kairastra auth menu
kairastra doctor
```

On Linux, setup writes `.kairastra/kairastra.service`. Typical flow:

```bash
sudo cp .kairastra/kairastra.service /etc/systemd/system/kairastra.service
sudo systemctl daemon-reload
sudo systemctl enable --now kairastra.service
```

## Workflow And Env Files

The recommended workflow keeps secrets and machine-specific values outside the file by referencing
environment variables such as:

- `GITHUB_TOKEN`
- `KAIRASTRA_GITHUB_OWNER`
- `KAIRASTRA_GITHUB_REPO`
- `KAIRASTRA_GITHUB_PROJECT_NUMBER`
- `KAIRASTRA_GITHUB_PROJECT_URL`
- `KAIRASTRA_WORKSPACE_ROOT`
- `KAIRASTRA_SEED_REPO`
- `KAIRASTRA_AGENT_ASSIGNEE`

In native mode:

- setup writes `WORKFLOW.md` and `.kairastra/kairastra.env` by default
- setup writes `.kairastra/kairastra.service` by default on Linux hosts
- the runtime reads the workflow file directly from the host filesystem

## References

- [auth.md](auth.md): token and provider auth details
- [workflow-reference.md](workflow-reference.md): `WORKFLOW.md` schema and behavior
- [operations.md](operations.md): day-2 commands and limitations
