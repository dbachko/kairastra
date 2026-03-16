# Symphony Implementation Status

This file maps the current Rust implementation to the updated GitHub organization + webhook design.

Status legend:

- `implemented`: present and aligned with the target design
- `partial`: present but not fully aligned with the target design
- `missing`: not implemented yet

## Core orchestration

| Area | Status | Notes |
| --- | --- | --- |
| Workflow loading and typed config | implemented | `WORKFLOW.md` front matter, strict prompt rendering, env-backed config, and reload behavior are in place. |
| Workspace lifecycle and hooks | implemented | Per-issue workspaces, lifecycle hooks, and cleanup behavior are implemented. |
| App-server session management | implemented | Rust supports current and legacy Codex app-server protocols. |
| Retry and continuation handling | implemented | Continuation retries, exponential backoff, and stall handling are implemented. |

## GitHub tracker profile

| Area | Status | Notes |
| --- | --- | --- |
| Project v2 candidate fetch | implemented | `projects_v2` is the primary tracker mode. |
| Organization-owned Project profile | partial | The spec now treats this as canonical, but the code still keeps user-owned Project fallback active. |
| `issues_only` fallback | implemented | Repository-only issue enumeration works. |
| Project field mapping (`Status`, `Priority`) | implemented | Configurable field-source mapping exists. |
| Assignee-based dispatch filter | implemented | `agent.assignee_login` gates dispatch. |
| PR lookup and required-check inspection | implemented | Open PR lookup and check summary gating are in place. |

## Webhook/event model

| Area | Status | Notes |
| --- | --- | --- |
| Webhook listener and signature verification | implemented | Built-in listener verifies `X-Hub-Signature-256`. |
| Repository webhook wakeups | implemented | Issue/PR/review/check events wake the orchestrator. |
| Organization Project webhook wakeups | partial | Listener accepts Project event names, but deployment guidance and runtime behavior are still generic. |
| Targeted reconciliation from webhook payloads | missing | Current webhook path wakes a broad reconciliation pass instead of reconciling only the affected issue/project item. |
| Recovery reconciliation polling | implemented | Slow polling remains available and should now be treated as recovery-only in the canonical deployment model. |

## Workflow ownership split

| Area | Status | Notes |
| --- | --- | --- |
| Runtime-owned claim transition | implemented | `Todo -> In Progress` is runtime-owned. |
| Runtime-owned review handoff gate | implemented | `In Progress -> Human Review` requires PR, workpad progress, and green checks. |
| Runtime-owned terminal cleanup | implemented | Closed issues are reconciled to project `Done`. |
| Agent-owned workpad content | partial | The runtime now preserves agent-authored content and appends a machine block, but bootstrap generation is still opinionated. |
| Workpad progress enforcement | partial | The gate exists, but the runtime still relies on simple heuristics such as checkbox detection. |

## Highest-priority follow-up gaps

1. Make webhook-triggered reconciliation payload-aware so the runtime can reconcile only the affected issue/project item instead of waking a full candidate scan.
2. Revisit workpad bootstrap so the runtime creates the minimum safe scaffold while leaving more structure to the workflow prompt.
3. Demote or isolate user-owned Project fallback in the code and docs so the canonical organization-owned deployment path stays clean.
4. Add deployment docs and helper scripts for registering both repository and organization webhooks against the same public endpoint.
