# Kairastra Runtime

This directory contains the current Rust implementation of Kairastra for GitHub Issues and Projects
v2. It is the operator-facing runtime in this repo: it loads `WORKFLOW.md`, polls GitHub, creates
per-issue workspaces, launches the configured agent provider, and keeps the issue lifecycle in sync
with the runtime.

Use this README as the practical setup and operations guide. The normative behavior still lives in
[`SPEC.md`](../SPEC.md).

## What it does

- Loads `WORKFLOW.md` front matter plus prompt template and keeps the last known good config on reload errors.
- Talks to GitHub through GraphQL and REST using a typed `tracker.kind: github` config.
- Supports repo-first `issues_only` queues and optional repo-scoped `projects_v2` queues.
- Creates deterministic per-issue workspaces and runs lifecycle hooks around them.
- Starts the configured provider runtime for each issue.
- Tracks retries, continuation turns, backoff, and reconciliation in a single orchestrator loop.
- Exposes operator commands for setup, doctor checks, and provider auth management.

## Deployment model

One Kairastra deployment manages one repository.

- The runtime bootstraps every issue workspace from one configured repository checkout or clone URL.
- PR discovery, check summaries, and workpad writes all happen against that repository.
- `projects_v2` can be used as the queue for that repository, but Kairastra still ignores project items from other repositories.
- If you want automation across multiple repositories, run multiple Kairastra services or containers.

## Requirements

At minimum:

- Rust toolchain
- the provider CLI or CLIs you intend to route to available in `PATH`:
  - `codex` for Codex
  - `claude` for Claude Code
  - `gemini` for Gemini CLI
- GitHub token with access to the target repo, and the target project when using `projects_v2`
- A `WORKFLOW.md` file or a generated equivalent

For native VPS mode:

- Linux host with `systemd`
- A stable path to the built `kairastra` binary

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

For `tracker.mode: issues_only`, Kairastra needs repo access only.

For `tracker.mode: projects_v2`, Kairastra needs a GitHub token that can read and usually mutate the
target Project v2.

For a user-owned Project v2 like `https://github.com/users/<user>/projects/<number>`:

- Use a personal access token (classic)
- Do not use a fine-grained personal access token

The reason is GitHub does not support fine-grained PATs for Projects owned by a user account, and
the Projects API docs require `read:project` for queries or `project` for queries plus mutations.
GitHub also documents `repo` for command-line repository access.

Recommended classic PAT scopes for Kairastra:

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
   - `project` for full Kairastra project-state automation
   - `repo` if the repository is private
   - `workflow` if you want agent runs to be able to push workflow-file changes

Notes:

- If you only want to test read-only project access, `read:project` can replace `project`.
- Kairastra moves issues between project states, so `project` is the practical choice for end-to-end use.
- Without `workflow`, pushes that modify `.github/workflows/*` will be rejected by GitHub even if normal code pushes succeed.
- If you are accessing org resources protected by SSO, GitHub may require SSO authorization for the token after creation.

References:

- GitHub Projects API auth requirements: https://docs.github.com/en/enterprise-server%403.20/issues/planning-and-tracking-with-projects/automating-your-project/using-the-api-to-manage-projects
- GitHub token creation and `repo` scope guidance: https://docs.github.com/en/enterprise-server%403.19/authentication/keeping-your-account-and-data-secure/managing-your-personal-access-tokens
- GitHub note that fine-grained PATs do not support user-owned Projects: https://docs.github.com/ko/enterprise-server%403.14/authentication/keeping-your-account-and-data-secure/managing-your-personal-access-tokens

Kairastra currently assumes a classic PAT for user-owned Project v2 workflows. If you want to stay
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
cargo run -- doctor --workflow ../WORKFLOW.md --env-file ../kairastra.env
```

If you use subscription auth:

```bash
cargo run -- auth login --mode subscription
cargo run -- auth status
# For Claude instead:
# cargo run -- auth --provider claude login --mode subscription
# cargo run -- auth --provider claude status
# For Gemini instead:
# cargo run -- auth --provider gemini login --mode subscription
# cargo run -- auth --provider gemini status
```

If you use API-key auth:

```bash
export OPENAI_API_KEY=...
cargo run -- auth login --mode api-key
# For Claude instead:
# export ANTHROPIC_API_KEY=...
# cargo run -- auth --provider claude login --mode api-key
# For Gemini instead:
# export GEMINI_API_KEY=...
# cargo run -- auth --provider gemini login --mode api-key
```

### Docker

1. Copy `rust/.env.example` to `rust/.env`.
2. Fill in `GITHUB_TOKEN` and the deployment-related `KAIRASTRA_*` values.
3. Run `make docker-setup` to write the deployment config into Docker-managed storage.
4. Run `make docker-sync-seed` to publish the current checkout into the Docker seed volume.
5. Start the stack.
6. If you use subscription/device auth, run the Docker login helper once.

Example:

```bash
cd rust
cp .env.example .env
make docker-build
make docker-setup
make docker-sync-seed
make docker-up
make docker-login
```

### Remote Docker bootstrap (run on host after SSH)

If Docker is already installed on the remote machine, use the bootstrap script instead of manually
copying the repo around. This is the supported path for a remote Mac mini or other Docker host.

Latest supported build from `main` (run on the host):

```bash
curl -fsSL -o /tmp/install-remote-docker.sh https://raw.githubusercontent.com/dbachko/kairastra/main/scripts/install-remote-docker.sh && bash /tmp/install-remote-docker.sh --ref main
```

Pinned release example (run on the host):

```bash
curl -fsSL -o /tmp/install-remote-docker.sh https://raw.githubusercontent.com/dbachko/kairastra/v0.1.0-alpha.1/scripts/install-remote-docker.sh && bash /tmp/install-remote-docker.sh --ref v0.1.0-alpha.1
```

Upgrade an existing remote install to the latest `main` and re-run setup:

```bash
~/kairastra/repo/scripts/install-remote-docker.sh --ref main --reconfigure
```

Notes:

- For `raw.githubusercontent.com`, use `main`, a tag, or a commit SHA as the ref segment.
- Do not use `refs/heads/main` in raw URLs.
- The remote bootstrap seeds `GITHUB_TOKEN` from host `GITHUB_TOKEN`, `GH_TOKEN`, or `gh auth token` when available. If none are present, the containerized setup flow now prompts and persists the token into the generated Docker env file.
- To get the newest setup flow, use the `main` install command above or re-run the managed script with
  `--ref main --reconfigure`. The pinned release example intentionally keeps you on that older release.
- If setup still shows the old prompt text `GitHub Project URL (optional, can auto-fill owner and number)`,
  you are running an older ref or image. Re-run the latest `main` command above, or on an existing host run
  `~/kairastra/repo/scripts/install-remote-docker.sh --ref main --reconfigure`.
- If the repo is still private, raw URLs return `404`; bootstrap by cloning over Git SSH instead:

```bash
set -euo pipefail
boot="$HOME/kairastra-bootstrap"
if [ -d "$boot/.git" ]; then
  git -C "$boot" fetch --tags --prune origin
else
  git clone git@github.com:dbachko/kairastra.git "$boot"
fi
git -C "$boot" checkout --force main
bash "$boot/scripts/install-remote-docker.sh" --repo git@github.com:dbachko/kairastra.git --ref main
```

What it does:

- creates a managed install root under `~/kairastra` by default
- clones the repo into `~/kairastra/repo`
- keeps the host Docker env file in `~/kairastra/config`
- builds the Docker image from that managed checkout
- opens the interactive `kairastra setup --mode docker` wizard in your SSH terminal and writes deployment config into the Docker `/config` volume
- runs `doctor`
- syncs the managed checkout into the Docker seed volume
- starts the stack with `docker compose up -d`
- opens `auth menu` on first install or explicit reconfigure unless you pass `--skip-auth`

Useful flags:

```bash
~/kairastra/repo/scripts/install-remote-docker.sh --install-dir ~/kairastra
~/kairastra/repo/scripts/install-remote-docker.sh --reconfigure
~/kairastra/repo/scripts/install-remote-docker.sh --skip-auth
~/kairastra/repo/scripts/install-remote-docker.sh --repo https://github.com/dbachko/kairastra.git --ref main
```

After the first install, the same script is available on the remote host at:

```bash
~/kairastra/repo/scripts/install-remote-docker.sh
```

Re-running that script updates the managed checkout to the requested ref, rebuilds the image, runs
Docker doctor checks, and refreshes the stack without replacing persisted Docker volumes.

Project status handling:

- For `projects_v2`, Kairastra now reads and writes Project statuses from workflow config.
- Read-side queue behavior uses `status_source`, `active_states`, `terminal_states`, and
  `claimable_states`.
- Write-side transitions use `in_progress_state`, `human_review_state`, and `done_state`.
- Set any of those write-side target states to `null` to disable that automatic Project mutation.
- `doctor` validates that the configured states and transition targets exist in the Project's
  configured status field.
- Interactive `setup` inspects the Project status field and defaults to
  `Keep existing Project statuses (recommended)`, which generates a matching workflow without
  mutating GitHub.
- `Normalize Project to Kairastra statuses` remains available, but it is interactive-only,
  requires typed confirmation, and is blocked for live Projects that already contain items in
  statuses that would be changed or removed.
- Non-interactive setup never normalizes Project statuses. Use these env overrides when you want
  custom mappings in generated workflows:
  - `KAIRASTRA_ACTIVE_STATES`
  - `KAIRASTRA_TERMINAL_STATES`
  - `KAIRASTRA_CLAIMABLE_STATES`
  - `KAIRASTRA_IN_PROGRESS_STATE`
  - `KAIRASTRA_HUMAN_REVIEW_STATE`
  - `KAIRASTRA_DONE_STATE`
- The explicit normalization helper is still `scripts/bootstrap_github_project.py`, which now has
  separate preserve vs normalize modes and applies the same safety checks as setup.

`make docker-up` now runs a Docker-scoped `doctor` preflight before starting the long-lived
service, so missing workflow env vars or invalid tracker settings fail once at startup instead of
triggering a Compose restart loop.

Docker now keeps the deployment config in a named `/config` volume and the seeded checkout in a
named `/seed-repo` volume. Workspace prompt/hooks come from repo-root `WORKFLOW.md` files inside
those seeded workspaces when present, or Kairastra's built-in default workspace workflow when
absent.

`make docker-login` opens a provider picker that shows which providers are already ready and which
still need action. Codex, Claude, and Gemini can use subscription login in Docker, and API-key auth
is still available when you want it. You can skip the picker with
`make docker-login PROVIDER=codex`, `make docker-login PROVIDER=claude`, or
`make docker-login PROVIDER=gemini`.
For Claude subscription login in Docker, the command prints the OAuth URL and then renders a
masked terminal prompt named `Paste Authentication Code` so you can paste the browser code back
into the same terminal session. After submit, Kairastra prints progress lines while it waits for
Claude to finish the login handshake.
For Gemini subscription login in Docker, Kairastra seeds Gemini's `/app` trust entry and disables
Gemini CLI auto-update prompts in the container. Gemini still renders its own auth UI, but
Kairastra now closes that session automatically once a new login is saved. If you intentionally
re-auth over an existing saved Gemini login, finish with `/quit`.
For Gemini issue execution, Kairastra also auto-registers a trusted `kairastra_github` MCP server
in `~/.gemini/settings.json`, so Gemini gets working `github_graphql` and `github_rest` tools
without any manual Gemini CLI setup.
Docker also sets `KAIRASTRA_DEPLOY_MODE=docker`, so `doctor` inside the container validates Docker
prerequisites instead of looking for `systemctl`.

## Guided setup

The setup flow is intentionally narrow: it does not try to turn a VPS into a full workstation. It
collects only the information needed to run Kairastra safely.

Interactive mode:

```bash
cargo run -- setup
```

Non-interactive mode:

```bash
cargo run -- setup --mode native --non-interactive
```

For Docker deployments, prefer `make docker-setup` so the deployment config is written into the
Docker-managed `/config` volume while the host `.env` file stays authoritative for compose.

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

- GitHub repo, either as a repo name or a full GitHub repo URL
- GitHub token when `GITHUB_TOKEN` or `GH_TOKEN` is not already set
- queue source: `issues_only` or `projects_v2`
- when using `projects_v2`: GitHub Project URL, optional project owner override, and Project v2 number
- when using `projects_v2` with a working GitHub token: inspect the Project `Status` field and choose between
  `Keep existing Project statuses (recommended)` or `Normalize Project to Kairastra statuses`
- when keeping existing statuses: active states, terminal states, claimable states, and optional transition targets
- when normalizing statuses: typed destructive confirmation before Kairastra rewrites the Project field
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
- optional Gemini model override
- Gemini approval mode override: `default`, `auto_edit`, `yolo`, or `plan`

What setup writes:

- workflow file
- env file
- native `systemd` unit when `--mode native`

Default output behavior:

- Setup writes `WORKFLOW.md` by default unless `--workflow` is provided.
- Native mode writes `kairastra.env` and `kairastra.service` by default.
- Docker mode writes `rust/.env.generated` when `rust/.env` already exists; otherwise it writes `rust/.env`.
- Native mode auto-detects the systemd binary path. If the current executable is clearly a cargo
  build artifact under `target/debug` or `target/release`, setup falls back to
  `/usr/local/bin/kairastra`. Override with `--binary-path` or `KAIRASTRA_BINARY_PATH` when needed.
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
- for `projects_v2`, configured Project status field mappings and transition targets
- workspace root existence or whether its parent exists

Expected behavior:

- Native mode on macOS or other non-`systemd` hosts will warn or fail on the `systemctl` check.
- A workflow that still references missing env vars will fail validation until the env file or shell exports are present.

## Provider auth model

Supported runtime modes:

- `auto`: prefer the matching provider API key env var when present (`OPENAI_API_KEY` for Codex, `ANTHROPIC_API_KEY` for Claude, `GEMINI_API_KEY` for Gemini); otherwise rely on persisted login state or a saved Claude subscription token
- `api_key`: require the matching provider API key env var
- `subscription`: use persisted device-auth, account login state, saved Gemini login state, or a saved Claude subscription token only

Status command:

```bash
cargo run -- auth status
cargo run -- auth --provider claude status
cargo run -- auth --provider gemini status
```

This reports:

- selected auth provider
- configured auth mode
- inferred auth mode
- whether the provider CLI is available locally
- whether the provider's local auth state exists (`~/.codex` for Codex; `~/.gemini/oauth_creds.json` for Gemini; for Claude, Kairastra treats `claude auth status --json` plus a saved `~/.claude/oauth-token` as authoritative, and the documented Linux/Windows credential path is `~/.claude/.credentials.json`)
- whether the matching API key env var is set (`OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, or `GEMINI_API_KEY`)
- a reminder about the matching Docker auth volume inside the container

Login commands:

```bash
cargo run -- auth login --mode subscription
cargo run -- auth login --mode api-key
cargo run -- auth --provider claude login --mode subscription
cargo run -- auth --provider claude login --mode api-key
cargo run -- auth --provider gemini login --mode subscription
cargo run -- auth --provider gemini login --mode api-key
cargo run -- auth menu
```

Use `subscription` for device/browser or account login and `api-key` when the matching provider API
key is already set in the current shell. In Docker, Claude subscription login now uses Kairastra's own
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

- deployment config lives in the `kairastra_config` volume and is read from `/config/WORKFLOW.md`.
- the seeded checkout lives in the `kairastra_seed` volume and is mounted at `/seed-repo`.
- workspaces live in the `kairastra_workspaces` volume.
- runtime home state persists in the `kairastra_home` volume.
- Codex auth persists in the `kairastra_codex` volume and is linked into the runtime home.
- Claude auth persists in the `kairastra_claude` volume and is linked into the runtime home.
- Gemini auth persists in the `kairastra_gemini` volume and is linked into the runtime home.
- a saved Claude long-lived OAuth token is stored at `~/.claude/oauth-token` inside that shared auth volume.
- the container now runs Kairastra as a non-root `kairastra` user so Claude's bypass-permissions mode works.
- Claude Code is installed from Anthropic's native Linux installer inside the image rather than the npm package.
- Docker now starts a per-container D-Bus session plus a headless GNOME keyring for the `kairastra` user so Claude subscription auth has a Linux secret store available in headless environments.
- the headless keyring lives under the persisted runtime home volume; by default it is unlocked with an empty password inside the container session, and you can override that by setting `KAIRASTRA_CLAUDE_KEYRING_PASSWORD`.
- Compose now passes through the workflow-related `KAIRASTRA_*` variables so env-backed workflow
  fields resolve inside the container at runtime.
- `CODEX_AUTH_MODE=subscription` plus `make docker-login PROVIDER=codex` is the intended Codex subscription/device-auth path.
- `CODEX_AUTH_MODE=api_key` plus `OPENAI_API_KEY` is the intended API-key path.
- `CLAUDE_AUTH_MODE=subscription` plus `make docker-login PROVIDER=claude` is the intended Claude subscription path; in Docker Kairastra drives the browser OAuth flow itself and persists the resulting long-lived subscription token into the shared Claude auth volume.
- `CLAUDE_AUTH_MODE=api_key` plus `ANTHROPIC_API_KEY` remains available when you want Anthropic Console billing instead of a Claude subscription login.
- `GEMINI_AUTH_MODE=subscription` plus `make docker-login PROVIDER=gemini` is the intended Gemini Google-login path.
- `GEMINI_AUTH_MODE=api_key` plus `GEMINI_API_KEY` remains available when you want non-interactive Gemini API auth.
- `CLAUDE_CODE_OAUTH_TOKEN` is also supported directly when you want to pre-seed Docker/VPS auth from a token generated elsewhere.

Available make targets:

- `make docker-build`
- `make docker-doctor`
- `make docker-setup`
- `make docker-sync-seed`
- `make docker-config-export DEST=...`
- `make docker-config-import SRC=...`
- `make docker-up`
- `make docker-down`
- `make docker-logs`
- `make docker-login` opens an interactive provider picker and then runs the matching login flow when needed
- `make docker-login PROVIDER=codex` goes straight to the Codex login flow
- `make docker-login PROVIDER=claude` goes straight to the Claude subscription login flow
- `make docker-login PROVIDER=gemini` goes straight to the Gemini login flow
- the Claude Docker login helper prints a browser authorize URL directly and never drops you into Claude's raw terminal TUI
- after you paste the browser auth code, Kairastra exchanges it directly and either saves the token or returns the exact HTTP error body from Anthropic instead of hanging

## Native VPS deployment details

Setup can generate a `systemd` unit, but it does not install it automatically. That is deliberate:
the wizard writes artifacts, and the operator chooses when to promote them into the live system.

Typical flow:

```bash
sudo cp kairastra.service /etc/systemd/system/kairastra.service
sudo systemctl daemon-reload
sudo systemctl enable --now kairastra.service
sudo systemctl status kairastra.service
journalctl -u kairastra.service -f
```

The generated unit references:

- the env file through `EnvironmentFile=...`
- the current working directory as `WorkingDirectory=...`
- the auto-detected or overridden binary path via `ExecStart=<binary> run <workflow>`

If your installed binary lives somewhere non-standard, pass `--binary-path /absolute/path/to/kairastra`
to setup or export `KAIRASTRA_BINARY_PATH` before running it.

## Workflow and env files

The recommended workflow keeps secrets and machine-specific values outside the file by referencing
environment variables such as:

- `GITHUB_TOKEN`
- `KAIRASTRA_GITHUB_OWNER`
- `KAIRASTRA_GITHUB_REPO`
- `KAIRASTRA_GITHUB_PROJECT_NUMBER`
- `KAIRASTRA_GITHUB_PROJECT_URL`
- `KAIRASTRA_WORKSPACE_ROOT`
- `KAIRASTRA_GIT_CLONE_URL`
- `KAIRASTRA_SEED_REPO`
- `KAIRASTRA_AGENT_ASSIGNEE`
- `KAIRASTRA_CLAUDE_MODEL`
- `KAIRASTRA_CLAUDE_REASONING_EFFORT`
- `KAIRASTRA_CODEX_MODEL`
- `KAIRASTRA_CODEX_REASONING_EFFORT`
- `KAIRASTRA_CODEX_FAST`
- `KAIRASTRA_GEMINI_MODEL`
- `KAIRASTRA_GEMINI_APPROVAL_MODE`

If you provide `KAIRASTRA_GITHUB_PROJECT_URL` in the setup flow, Kairastra can derive the GitHub
owner and Project v2 number automatically for URLs like
`https://github.com/users/<owner>/projects/<number>` and
`https://github.com/orgs/<owner>/projects/<number>`.

For native deployments and repo-owned workflows like the checked-in [WORKFLOW.md](../WORKFLOW.md),
the workflow hook layer still prepares issue workspaces:

- clones the canonical repo when `KAIRASTRA_GIT_CLONE_URL` is set
- overlays `KAIRASTRA_SEED_REPO` on top when present
- sets the git author identity

For Docker deployments:

- setup writes deployment config into `/config/WORKFLOW.md`
- Kairastra performs the clone/overlay bootstrap internally from `/seed-repo` before repo hooks run
- workspace prompt/hooks come from repo-root `WORKFLOW.md` inside the seeded repository when
  present, or from Kairastra's built-in default repo workflow when absent

The checked-in [WORKFLOW.md](../WORKFLOW.md) remains a good reference for the richer review/handoff
prompt used in this repo, and in Docker mode it is also the repo-owned prompt/hook surface when
that repo is used as the seed source.

Provider runtime controls:

- `agent.provider` selects the default agent backend for the workflow. Supported values are
  `codex`, `claude`, and `gemini`.
- label overrides such as `agent:claude` or `agent:gemini` can route individual issues to a different configured provider.
- `providers.codex.model` sets the model Kairastra requests for the thread and subsequent turns.
- `providers.codex.reasoning_effort` controls thinking depth. Valid values are `none`, `minimal`, `low`,
  `medium`, `high`, and `xhigh`.
- `providers.codex.fast` is a boolean. `true` maps to Codex `serviceTier=fast`; `false` maps to
  `serviceTier=flex`.
- `providers.claude.model` and `providers.claude.reasoning_effort` control Claude Code selection and depth.
- `providers.gemini.model` sets the Gemini model override.
- `providers.gemini.approval_mode` controls Gemini CLI approval handling. Valid values are `default`, `auto_edit`, `yolo`, and `plan`.

## GitHub bootstrap helper

From the repo root, `scripts/bootstrap_github_project.py` can converge a GitHub Project and repo
toward the Kairastra workflow shape:

```bash
python3 scripts/bootstrap_github_project.py --dry-run
python3 scripts/bootstrap_github_project.py
```

It expects:

- `KAIRASTRA_GITHUB_OWNER`
- `KAIRASTRA_GITHUB_REPO`
- `KAIRASTRA_GITHUB_PROJECT_OWNER` when the project owner differs from the repo owner
- `KAIRASTRA_GITHUB_PROJECT_NUMBER`

It ensures:

- the expected `Status` options
- a numeric `Priority` field
- a default label pack for Kairastra-oriented filtering

## Day-2 operations

Useful commands once Kairastra is running:

```bash
cargo run -- doctor --workflow /path/to/WORKFLOW.md --env-file /path/to/envfile
cargo run -- auth status
make -C rust docker-logs
journalctl -u kairastra.service -f
```

If you are already inside the `rust/` directory, drop the `-C rust` prefix and run `make docker-logs`,
`make docker-up`, or `make docker-login` directly.

Common failure modes:

- missing `GITHUB_TOKEN`: workflow validation fails and GitHub connectivity checks fail
- missing workflow env vars: the workflow loads only after the env file is applied
- missing provider auth: `auth status` shows no local auth file and no API key
- wrong binary path in native mode: `systemd` starts but fails immediately

Running multiple repositories:

- Create one workflow/env pair per repository.
- Run one Docker Compose project or one native service per repository.
- If several repositories share one GitHub Project, point each deployment at the same project but keep each deployment scoped to its own `owner/repo`.

## Current limitations

- One runtime does not manage multiple repositories.
- The current implementation targets local workers only.
- GitHub dynamic tools are limited to `github_graphql` and a small `github_rest` allow-list.
- The operator UX is terminal-first; there is no web onboarding flow here.
- The setup wizard writes artifacts and validates them, but does not install system packages or mutate the host beyond those generated files.
