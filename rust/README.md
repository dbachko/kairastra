# Symphony Rust

This directory contains the current Rust implementation of Symphony for GitHub Issues and Projects
v2. It is the operator-facing runtime in this repo: it loads `WORKFLOW.md`, polls GitHub, creates
per-issue workspaces, launches the configured agent provider, and keeps the issue lifecycle in sync
with the runtime.

Use this README as the practical setup and operations guide. The normative behavior still lives in
[`SPEC.md`](../SPEC.md).

## What it does

- Loads `WORKFLOW.md` front matter plus prompt template and keeps the last known good config on reload errors.
- Talks to GitHub through GraphQL and REST using a typed `tracker.kind: github` config.
- Supports `projects_v2` as the primary tracker mode and `issues_only` as a fallback.
- Creates deterministic per-issue workspaces and runs lifecycle hooks around them.
- Starts the configured provider runtime for each issue.
- Tracks retries, continuation turns, backoff, and reconciliation in a single orchestrator loop.
- Exposes operator commands for setup, doctor checks, and provider auth management.

## Requirements

At minimum:

- Rust toolchain
- the provider CLI or CLIs you intend to route to available in `PATH`:
  - `codex` for Codex
  - `claude` for Claude Code
- GitHub token with access to the target repo and project
- A `WORKFLOW.md` file or a generated equivalent

For native VPS mode:

- Linux host with `systemd`
- A stable path to the built `symphony-rust` binary

For Docker mode:

- Docker and Compose
- `rust/.env` populated from `rust/.env.example`

## CLI overview

The binary uses explicit subcommands:

```bash
cargo run -- run /path/to/WORKFLOW.md
cargo run -- setup
cargo run -- doctor
cargo run -- auth status
cargo run -- auth login --mode subscription
cargo run -- auth login --mode api-key
```

What each command does:

- `run`: start the orchestrator loop. `--once` performs one dispatch pass and waits for any
  workers started in that pass to finish. Continuations and retries are deferred to the next run.
- `setup`: guided first-run flow for native VPS or Docker.
- `doctor`: validate local prerequisites, workflow loading, GitHub connectivity, and the selected provider auth state.
- `auth status`: print the current provider auth state as JSON. The default provider is `codex`
  unless you pass `--provider`.
- `auth login`: run either subscription/device login or API-key bootstrap through the selected
  provider CLI.

Additional operator docs:

- [docs/architecture.md](../docs/architecture.md)
- [docs/workflow-reference.md](../docs/workflow-reference.md)
- [docs/troubleshooting.md](../docs/troubleshooting.md)

## Quick start

## GitHub token requirements

For `tracker.mode: projects_v2`, Symphony needs a GitHub token that can read and usually mutate the
target Project v2.

For a user-owned Project v2 like `https://github.com/users/<user>/projects/<number>`:

- Use a personal access token (classic)
- Do not use a fine-grained personal access token

The reason is GitHub does not support fine-grained PATs for Projects owned by a user account, and
the Projects API docs require `read:project` for queries or `project` for queries plus mutations.
GitHub also documents `repo` for command-line repository access.

Recommended classic PAT scopes for Symphony:

- `project`
- `repo` if the target repository is private
- `workflow` if agent branches may add or edit files under `.github/workflows/`

Minimum classic PAT scopes for read-only diagnostics:

- `read:project`
- `repo` if the target repository is private

How to create it:

Direct links:

- Token settings: https://github.com/settings/tokens
- Classic token creation: https://github.com/settings/tokens/new

Creation flow:

1. Open `https://github.com/settings/tokens`
2. Open `Tokens (classic)`
3. Click `Generate new token (classic)`
4. Select:
   - `project` for full Symphony project-state automation
   - `repo` if the repository is private
   - `workflow` if you want agent runs to be able to push workflow-file changes

Notes:

- If you only want to test read-only project access, `read:project` can replace `project`.
- Symphony moves issues between project states, so `project` is the practical choice for end-to-end use.
- Without `workflow`, pushes that modify `.github/workflows/*` will be rejected by GitHub even if normal code pushes succeed.
- If you are accessing org resources protected by SSO, GitHub may require SSO authorization for the token after creation.

References:

- GitHub Projects API auth requirements: https://docs.github.com/en/enterprise-server%403.20/issues/planning-and-tracking-with-projects/automating-your-project/using-the-api-to-manage-projects
- GitHub token creation and `repo` scope guidance: https://docs.github.com/en/enterprise-server%403.19/authentication/keeping-your-account-and-data-secure/managing-your-personal-access-tokens
- GitHub note that fine-grained PATs do not support user-owned Projects: https://docs.github.com/ko/enterprise-server%403.14/authentication/keeping-your-account-and-data-secure/managing-your-personal-access-tokens

Symphony currently assumes a classic PAT for user-owned Project v2 workflows. If you want to stay
on a fine-grained PAT, use `issues_only` mode or move the project to an organization and verify the
token policy there.

### Native VPS

1. Build the binary.
2. Run the setup wizard.
3. Review the generated workflow, env file, and `systemd` unit.
4. Run doctor against those generated files.
5. Install and start the service.

Example:

```bash
cd rust
cargo build
cargo run -- setup --mode native
cargo run -- doctor --workflow ../WORKFLOW.md --env-file ../symphony.env
```

If you use subscription auth:

```bash
cargo run -- auth login --mode subscription
cargo run -- auth status
# For Claude instead:
# cargo run -- auth --provider claude login --mode subscription
# cargo run -- auth --provider claude status
```

If you use API-key auth:

```bash
export OPENAI_API_KEY=...
cargo run -- auth login --mode api-key
# For Claude instead:
# export ANTHROPIC_API_KEY=...
# cargo run -- auth --provider claude login --mode api-key
```

### Docker

1. Copy `rust/.env.example` to `rust/.env`.
2. Fill in `GITHUB_TOKEN` and the workflow-related `SYMPHONY_*` values.
3. Point `WORKFLOW_FILE` at the workflow you want mounted.
4. Start the stack.
5. If you use subscription/device auth, run the Docker login helper once.

Example:

```bash
cd rust
cp .env.example .env
make docker-build
make docker-up
make docker-login
```

`make docker-login` opens a provider picker that shows which providers are already ready and which
still need action. Both Codex and Claude can use subscription login in Docker, and API-key auth is
still available when you want it. You can skip the picker with
`make docker-login PROVIDER=codex` or `make docker-login PROVIDER=claude`.
For Claude subscription login in Docker, the command prints the OAuth URL and then renders a
masked terminal prompt named `Paste Authentication Code` so you can paste the browser code back
into the same terminal session. After submit, Symphony prints progress lines while it waits for
Claude to finish the login handshake.
Docker also sets `SYMPHONY_DEPLOY_MODE=docker`, so `doctor` inside the container validates Docker
prerequisites instead of looking for `systemctl`.

## Guided setup

The setup flow is intentionally narrow: it does not try to turn a VPS into a full workstation. It
collects only the information needed to run Symphony safely.

Interactive mode:

```bash
cargo run -- setup
```

Non-interactive mode:

```bash
cargo run -- setup --mode native --non-interactive
cargo run -- setup --mode docker --non-interactive
```

Optional flags:

```text
--mode native|docker
--workflow <PATH>
--env-file <PATH>
--service-unit <PATH>
--binary-path <PATH>
--non-interactive
```

What setup asks for:

- GitHub Project URL, with owner and Project v2 number auto-derived when possible
- GitHub repo, either as a repo name or a full GitHub repo URL
- workspace root
- seed repo path
- optional canonical clone URL
- optional assignee login filter
- concurrency and turn limits
- default provider selection
- provider auth path to optimize for
- optional Codex model override
- optional Codex thinking effort override: `none`, `minimal`, `low`, `medium`, `high`, or `xhigh`
- whether to force Codex fast mode on
- optional Claude model override
- optional Claude thinking effort override: `low`, `medium`, or `high`

What setup writes:

- workflow file
- env file
- native `systemd` unit when `--mode native`

Default output behavior:

- Setup writes `WORKFLOW.md` by default unless `--workflow` is provided.
- Native mode writes `symphony.env` and `symphony.service` by default.
- Docker mode writes `rust/.env.generated` when `rust/.env` already exists; otherwise it writes `rust/.env`.
- Native mode auto-detects the systemd binary path. If the current executable is clearly a cargo
  build artifact under `target/debug` or `target/release`, setup falls back to
  `/usr/local/bin/symphony-rust`. Override with `--binary-path` or `SYMPHONY_BINARY_PATH` when needed.
- Setup now detects whether you launched it from the repo root or from `rust/` and writes Docker
  env files to the Compose directory either way.

## Doctor checks

Run doctor before enabling the service, after changing auth, or when a deployment is behaving
strangely.

Examples:

```bash
cargo run -- doctor
cargo run -- doctor --workflow /path/to/WORKFLOW.md --env-file /path/to/envfile
cargo run -- doctor --mode docker --format json
```

Doctor currently checks:

- presence of required local commands such as the selected provider CLI, `gh`, and `docker` or `systemctl`
- selected provider auth state
- workflow load/validation
- GitHub tracker connectivity using the configured token
- workspace root existence or whether its parent exists

Expected behavior:

- Native mode on macOS or other non-`systemd` hosts will warn or fail on the `systemctl` check.
- A workflow that still references missing env vars will fail validation until the env file or shell exports are present.

## Provider auth model

Supported runtime modes:

- `auto`: prefer the matching provider API key env var when present (`OPENAI_API_KEY` for Codex, `ANTHROPIC_API_KEY` for Claude); otherwise rely on persisted login state or a saved Claude subscription token
- `api_key`: require the matching provider API key env var
- `subscription`: use persisted device-auth, account login state, or a saved Claude subscription token only

Status command:

```bash
cargo run -- auth status
cargo run -- auth --provider claude status
```

This reports:

- selected auth provider
- configured auth mode
- inferred auth mode
- whether the provider CLI is available locally
- whether the provider's local auth state exists (`~/.codex` for Codex; for Claude, Symphony treats `claude auth status --json` plus a saved `~/.claude/oauth-token` as authoritative, and the documented Linux/Windows credential path is `~/.claude/.credentials.json`)
- whether the matching API key env var is set (`OPENAI_API_KEY` or `ANTHROPIC_API_KEY`)
- a reminder about the matching Docker auth volume inside the container

Login commands:

```bash
cargo run -- auth login --mode subscription
cargo run -- auth login --mode api-key
cargo run -- auth --provider claude login --mode subscription
cargo run -- auth --provider claude login --mode api-key
cargo run -- auth menu
```

Use `subscription` for device/browser or account login and `api-key` when the matching provider API
key is already set in the current shell. In Docker, Claude subscription login now uses Symphony's own
OAuth flow: it prints the Claude authorize URL, prompts for the pasted authentication code, exchanges
that code using Claude Code's JSON PKCE token exchange, requests the same 1-year subscription token
expiry that `claude setup-token` uses, and persists the returned Claude subscription token in the
shared Claude auth volume so the running worker can pick it up without restarting the service
container.
Paste the Authentication Code shown on Claude's browser page after sign-in, not the authorize URL itself.

## Docker deployment details

Compose files:

- `rust/Dockerfile`
- `rust/compose.yml`
- `rust/.env.example`

Important details:

- `WORKFLOW_FILE` is mounted read-only at `/config/WORKFLOW.md`.
- `SEED_REPO_PATH` is mounted read-only at `/seed-repo`.
- workspaces live in the `symphony_rust_workspaces` volume.
- runtime home state persists in the `symphony_rust_home` volume.
- Codex auth persists in the `symphony_rust_codex` volume and is linked into the runtime home.
- Claude auth persists in the `symphony_rust_claude` volume and is linked into the runtime home.
- a saved Claude long-lived OAuth token is stored at `~/.claude/oauth-token` inside that shared auth volume.
- the container now runs Symphony as a non-root `symphony` user so Claude's bypass-permissions mode works.
- Claude Code is installed from Anthropic's native Linux installer inside the image rather than the npm package.
- Docker now starts a per-container D-Bus session plus a headless GNOME keyring for the `symphony` user so Claude subscription auth has a Linux secret store available in headless environments.
- the headless keyring lives under the persisted runtime home volume; by default it is unlocked with an empty password inside the container session, and you can override that by setting `SYMPHONY_CLAUDE_KEYRING_PASSWORD`.
- Compose now passes through the workflow-related `SYMPHONY_*` variables so env-backed workflow
  fields resolve inside the container at runtime.
- `CODEX_AUTH_MODE=subscription` plus `make docker-login PROVIDER=codex` is the intended Codex subscription/device-auth path.
- `CODEX_AUTH_MODE=api_key` plus `OPENAI_API_KEY` is the intended API-key path.
- `CLAUDE_AUTH_MODE=subscription` plus `make docker-login PROVIDER=claude` is the intended Claude subscription path; in Docker Symphony drives the browser OAuth flow itself and persists the resulting long-lived subscription token into the shared Claude auth volume.
- `CLAUDE_AUTH_MODE=api_key` plus `ANTHROPIC_API_KEY` remains available when you want Anthropic Console billing instead of a Claude subscription login.
- `CLAUDE_CODE_OAUTH_TOKEN` is also supported directly when you want to pre-seed Docker/VPS auth from a token generated elsewhere.

Available make targets:

- `make docker-build`
- `make docker-up`
- `make docker-down`
- `make docker-logs`
- `make docker-login` opens an interactive provider picker and then runs the matching login flow when needed
- `make docker-login PROVIDER=codex` goes straight to the Codex login flow
- `make docker-login PROVIDER=claude` goes straight to the Claude subscription login flow
- the Claude Docker login helper prints a browser authorize URL directly and never drops you into Claude's raw terminal TUI
- after you paste the browser auth code, Symphony exchanges it directly and either saves the token or returns the exact HTTP error body from Anthropic instead of hanging

## Native VPS deployment details

Setup can generate a `systemd` unit, but it does not install it automatically. That is deliberate:
the wizard writes artifacts, and the operator chooses when to promote them into the live system.

Typical flow:

```bash
sudo cp symphony.service /etc/systemd/system/symphony.service
sudo systemctl daemon-reload
sudo systemctl enable --now symphony.service
sudo systemctl status symphony.service
journalctl -u symphony.service -f
```

The generated unit references:

- the env file through `EnvironmentFile=...`
- the current working directory as `WorkingDirectory=...`
- the auto-detected or overridden binary path via `ExecStart=<binary> run <workflow>`

If your installed binary lives somewhere non-standard, pass `--binary-path /absolute/path/to/symphony-rust`
to setup or export `SYMPHONY_BINARY_PATH` before running it.

## Workflow and env files

The recommended workflow keeps secrets and machine-specific values outside the file by referencing
environment variables such as:

- `GITHUB_TOKEN`
- `SYMPHONY_GITHUB_OWNER`
- `SYMPHONY_GITHUB_REPO`
- `SYMPHONY_GITHUB_PROJECT_NUMBER`
- `SYMPHONY_GITHUB_PROJECT_URL`
- `SYMPHONY_WORKSPACE_ROOT`
- `SYMPHONY_GIT_CLONE_URL`
- `SYMPHONY_SEED_REPO`
- `SYMPHONY_AGENT_ASSIGNEE`
- `SYMPHONY_CLAUDE_MODEL`
- `SYMPHONY_CLAUDE_REASONING_EFFORT`
- `SYMPHONY_CODEX_MODEL`
- `SYMPHONY_CODEX_REASONING_EFFORT`
- `SYMPHONY_CODEX_FAST`

If you provide `SYMPHONY_GITHUB_PROJECT_URL` in the setup flow, Symphony can derive the GitHub
owner and Project v2 number automatically for URLs like
`https://github.com/users/<owner>/projects/<number>` and
`https://github.com/orgs/<owner>/projects/<number>`.

The generated workflow also includes an `after_create` hook that:

- clones the canonical repo when `SYMPHONY_GIT_CLONE_URL` is set
- overlays `SYMPHONY_SEED_REPO` on top when present
- sets the git author identity

The checked-in [WORKFLOW.md](../WORKFLOW.md) remains a good reference for the richer review/handoff
prompt used in this repo.

Provider runtime controls:

- `agent.provider` selects the default agent backend for the workflow. Supported values are
  `codex` and `claude`.
- label overrides such as `agent:claude` can route individual issues to a different configured provider.
- `providers.codex.model` sets the model Symphony requests for the thread and subsequent turns.
- `providers.codex.reasoning_effort` controls thinking depth. Valid values are `none`, `minimal`, `low`,
  `medium`, `high`, and `xhigh`.
- `providers.codex.fast` is a boolean. `true` maps to Codex `serviceTier=fast`; `false` maps to
  `serviceTier=flex`.
- `providers.claude.model` and `providers.claude.reasoning_effort` control Claude Code selection and depth.

## GitHub bootstrap helper

From the repo root, `scripts/bootstrap_github_project.py` can converge a GitHub Project and repo
toward the Symphony workflow shape:

```bash
python3 scripts/bootstrap_github_project.py --dry-run
python3 scripts/bootstrap_github_project.py
```

It expects:

- `SYMPHONY_GITHUB_OWNER`
- `SYMPHONY_GITHUB_REPO`
- `SYMPHONY_GITHUB_PROJECT_NUMBER`

It ensures:

- the expected `Status` options
- a numeric `Priority` field
- a default label pack for Symphony-oriented filtering

## Day-2 operations

Useful commands once Symphony is running:

```bash
cargo run -- doctor --workflow /path/to/WORKFLOW.md --env-file /path/to/envfile
cargo run -- auth status
make -C rust docker-logs
journalctl -u symphony.service -f
```

If you are already inside the `rust/` directory, drop the `-C rust` prefix and run `make docker-logs`,
`make docker-up`, or `make docker-login` directly.

Common failure modes:

- missing `GITHUB_TOKEN`: workflow validation fails and GitHub connectivity checks fail
- missing workflow env vars: the workflow loads only after the env file is applied
- missing provider auth: `auth status` shows no local auth file and no API key
- wrong binary path in native mode: `systemd` starts but fails immediately

## Current limitations

- The current implementation targets local workers only.
- GitHub dynamic tools are limited to `github_graphql` and a small `github_rest` allow-list.
- The operator UX is terminal-first; there is no web onboarding flow here.
- The setup wizard writes artifacts and validates them, but does not install system packages or mutate the host beyond those generated files.
