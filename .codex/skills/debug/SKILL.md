---
name: debug
description:
  Investigate stuck runs and execution failures by tracing Symphony and Codex
  logs with issue/session identifiers; use when runs stall, retry repeatedly, or
  fail unexpectedly.
---

# Debug

## Goals

- Find why a run is stuck, retrying, or failing.
- Correlate a GitHub issue to a worker attempt quickly.
- Read the right runtime output in the right order to isolate root cause.

## Runtime output sources

Symphony Rust logs to stdout/stderr through `tracing`; there is no repo-local
file logger by default.

Use one of these sources:

- Local foreground run:
  - `RUST_LOG=info cargo run -- /path/to/WORKFLOW.md 2>&1 | tee /tmp/symphony.log`
- Docker Compose:
  - `make -C rust docker-logs`
  - or `docker compose -f rust/compose.yml --env-file rust/.env logs -f symphony-rust`
- Service manager / hosted process:
  - whatever stdout capture the host uses, for example `journalctl`, container
    logs, or redirected files.

## Correlation Keys

- `issue_identifier`: GitHub issue identifier such as `openai/symphony#42`
- `issue_id`: GitHub node id when available in runtime state
- `session_id`: Codex thread-turn pair (`<thread_id>-<turn_id>`) when emitted in
  app-server payloads

In practice, `issue_identifier` is the most stable entry point in the current
runtime logs.

## Quick Triage (Stuck Run)

1. Confirm scheduler/worker symptoms for the ticket.
2. Find recent lines for the ticket (`issue_identifier` first).
3. Look for whether the worker completed, retried, or was cleaned up as
   terminal.
4. If needed, inspect app-server output and PR/issue state to separate runtime
   failures from workflow-state transitions.
5. Decide class of failure: startup/auth failure, turn failure, stall/timeout,
   retry loop, or bad tracker state.

## Commands

```bash
# 1) Capture local logs to a file if you are running in the foreground
RUST_LOG=info cargo run -- /path/to/WORKFLOW.md 2>&1 | tee /tmp/symphony.log

# 2) Narrow by GitHub issue identifier
rg -n 'issue_identifier=.*owner/repo#123' /tmp/symphony.log

# 3) Focus on retry / terminal signals
rg -n 'worker failed|worker completed|turn_stalled|turn_failed|cleanup failed|failed to normalize closed issue' /tmp/symphony.log

# 4) Docker logs
make -C rust docker-logs

# 5) Inspect live GitHub state if runtime behavior looks inconsistent
gh issue view owner/repo#123
gh pr view --json state,mergeable,headRefName,url
```

## Investigation Flow

1. Locate the ticket slice:
    - Search by `issue_identifier=<owner>/<repo>#<number>`.
    - If the run was containerized, start with Compose logs before grepping.
2. Establish timeline:
    - Find the first worker dispatch or event line for that issue.
    - Follow through `worker completed`, `worker failed`, retry scheduling, or
      cleanup warnings.
3. Classify the problem:
    - Claim / routing issue: wrong assignee, wrong Project status, or blocker
      state prevented dispatch.
    - App-server startup: Codex command/auth/runtime launch failed.
    - Turn execution failure: tool call failure, approval failure, or
      `turn_stalled`.
    - Retry loop: repeated worker failure or max-turn continuation behavior.
    - Cleanup mismatch: issue closed but Project item still not in `Done`.
4. Validate scope:
    - Check whether failures are isolated to one issue or repeating across
      multiple issues.
5. Capture evidence:
    - Save key log lines with timestamps and `issue_identifier`.
    - Pair them with the current GitHub issue/PR/Project state.
    - Record probable root cause and the exact failing stage.

## Reading runtime output

Read the Rust runtime as a lifecycle:

1. startup log / workflow load
2. dispatch or reconciliation activity for an issue
3. app-server warnings or worker failure/completion
4. retry or cleanup behavior

For one specific session investigation, keep the trace narrow:

1. Capture the issue identifier.
2. Build a timestamped slice for only that issue from stdout or container logs.
3. Mark the exact failing stage.
4. Pair findings with live GitHub state before concluding the bug is in the
   runtime.

## Notes

- Prefer `rg` over `grep` for speed on large captured logs.
- If you need richer local traces, rerun with `RUST_LOG=debug`.
- The current runtime does not persist a default log file; create one explicitly
  with shell redirection or use container/service logs.
