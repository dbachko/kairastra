# Kairastra Architecture

Kairastra is this repository's GitHub-native orchestration runtime. The current shipped runtime is
Rust-based and targets GitHub Issues plus Projects v2.

## Runtime shape

- `rust/src/workflow.rs`: loads `WORKFLOW.md`, parses front matter, and caches the last known good config.
- `rust/src/config.rs`: converts workflow front matter into validated runtime settings.
- `rust/src/github.rs`: reads and mutates GitHub Issues, Projects v2, workpad comments, and PR status.
- `rust/src/orchestrator.rs`: owns dispatch, reconciliation, retries, and worker lifecycle.
- `rust/src/runner.rs`: prepares workspaces, renders prompts, runs turns, and updates runtime workpad status.
- `rust/src/workspace.rs`: creates per-issue workspaces and runs lifecycle hooks.
- `rust/src/providers/`: provider-specific auth, config, and runtime adapters.
- `rust/src/setup.rs`: generates workflow/env/systemd scaffolding for operators.
- `rust/src/doctor.rs`: validates local commands, auth state, GitHub connectivity, and workspace paths.

## Deployment boundary

One Kairastra deployment is scoped to one repository checkout and one repository push target.

- `tracker.repo` names the repository Kairastra is expected to work in.
- Workspaces are cloned or overlaid from one configured seed repository.
- PR lookup, status checks, and workpad writes are all performed against the repository encoded in the issue identifier.
- If you need automation across multiple repositories, run multiple Kairastra deployments instead of one shared runtime.

## Execution model

1. `run` loads `WORKFLOW.md` and validates it into `Settings`.
2. The orchestrator polls GitHub for dispatchable issues and filters them to the configured repository scope.
3. Eligible issues get isolated workspaces under `workspace.root/<sanitized-issue-id>`.
4. The runner starts the selected provider session in that workspace.
5. Runtime status is written back to the issue workpad comment after each turn.
6. Terminal issues trigger workspace cleanup.

## Important operational behaviors

- `run --once` performs one dispatch pass and waits for workers started in that pass to finish.
- Workflow reload failures do not crash a long-running process if a last known good workflow exists.
- `after_run` hook failures are surfaced in logs and fail the worker outcome.
- `setup` now fails if the generated files do not pass `doctor`.
