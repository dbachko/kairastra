# Auth Guide

This page collects the auth and token policy details for Kairastra's GitHub-focused Symphony
runtime.

## GitHub Token Requirements

If `GITHUB_TOKEN` or `GH_TOKEN` is already set, setup uses it and skips the interactive token
prompt.

For `tracker.mode: issues_only`, Kairastra needs repository access only.

For `tracker.mode: projects_v2`, Kairastra needs a token that can read and usually mutate the
target Project v2.

### Project ownership rules

For a user-owned Project v2 such as `https://github.com/users/<user>/projects/<number>`:

- Use a classic personal access token.
- Do not use a fine-grained personal access token.

For an org-owned Project v2 such as `https://github.com/orgs/<org>/projects/<number>`:

- A classic PAT works.
- A fine-grained PAT may work if the org exposes an org-level `Projects` permission.
- If that permission is not available during token creation, use a classic PAT instead.

### Recommended classic PAT scopes

For `projects_v2`:

- `project`
- `repo` if the target repository is private
- `workflow` if agent branches may edit `.github/workflows/`

For `issues_only`:

- `repo` if the target repository is private
- `workflow` only if agent branches may edit `.github/workflows/`

## Provider Auth Modes

Supported runtime auth modes:

- `auto`: prefer the provider API key env var when present, otherwise use persisted login state
- `api_key`: require the provider API key env var
- `subscription`: use persisted browser, device, or account login state only

The matching API key env vars are:

- `OPENAI_API_KEY` for Codex
- `ANTHROPIC_API_KEY` for Claude
- `GEMINI_API_KEY` for Gemini

## Status And Login Commands

Default recommendation:

```bash
kairastra auth menu
```

Use direct `auth login --mode ...` commands only when you want to force a specific auth path.

Status:

```bash
kairastra auth status
kairastra auth --provider claude status
kairastra auth --provider gemini status
```

Login:

```bash
kairastra auth menu
kairastra auth login --mode subscription
kairastra auth login --mode api-key
kairastra auth --provider claude login --mode subscription
kairastra auth --provider claude login --mode api-key
kairastra auth --provider gemini login --mode subscription
kairastra auth --provider gemini login --mode api-key
```

## References

- [deployment.md](deployment.md): native deployment and service setup
- [operations.md](operations.md): doctor checks and day-2 commands
- [troubleshooting.md](troubleshooting.md): failure-oriented fixes
