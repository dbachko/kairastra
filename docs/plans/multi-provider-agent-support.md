# Multi-Provider Agent Support Plan

## Summary

This document proposes a staged implementation plan for letting Symphony orchestrate multiple
coding-agent backends instead of only Codex. The initial target providers are:

- Codex (existing backend)
- Claude Code
- Gemini CLI

Recommendation: do not bolt Claude or Gemini directly onto the existing Codex-specific
`CodexSession`. First extract a provider-neutral runner interface, move the current Codex
implementation behind it, and then add Claude and Gemini backends behind the same contract.

That path keeps the orchestrator stable while isolating provider-specific session, streaming,
approval, auth, and tool behavior inside backend adapters.

## Current Repo Context

Today the orchestration core is mostly provider-agnostic, but the runner layer is not.

Provider-agnostic pieces:

- `rust/src/orchestrator.rs` manages issue selection, concurrency, retries, and reconciliation.
- `rust/src/runner.rs` manages per-issue workspaces, prompt construction, continuation turns, and
  workpad updates.
- `SPEC.md` already defines the runner contract in terms of a coding-agent app-server lifecycle.

Codex-specific pieces:

- `rust/src/agent/codex.rs` launches `codex app-server` and speaks the Codex app-server protocol.
- `rust/src/config.rs` stores provider settings under `agent.provider` and `providers.codex`.
- `rust/src/doctor.rs` and `rust/src/auth.rs` still only validate or bootstrap Codex tooling/auth.
- `rust/src/setup.rs` emits provider-aware generated workflow and env values, defaulting to Codex.

This means the system is not far from supporting multiple providers, but the current boundaries are
wrong for it.

## Goals

- Let Symphony run the same orchestration loop against different coding-agent providers.
- Preserve the existing orchestrator behavior for claims, retries, continuation, and concurrency.
- Keep Codex as the first-class existing backend with no functional regression.
- Add a clean path for Claude Code and Gemini without entangling provider-specific logic in the
  orchestrator.
- Support selecting a provider globally first, with room to add per-state or per-issue routing
  later.

## Non-Goals

- Build a generic plugin marketplace in the first version.
- Normalize every provider feature to the least common denominator if that degrades Codex behavior.
- Support mixed-provider selection policies in the first implementation unless they fall out
  naturally from the design.
- Implement a provider-specific prompt dialect for every model family in v1.

## Product Decisions

### Decision 1: Introduce a first-class provider abstraction

Symphony should define an internal backend trait that represents a live agent session rather than
hard-coding Codex app-server semantics into the runner.

Illustrative shape:

```rust
#[async_trait]
pub trait AgentBackend: Send + Sync {
    async fn start_session(
        &self,
        settings: &Settings,
        tracker: Arc<GitHubTracker>,
        workspace: &Path,
    ) -> Result<Box<dyn AgentSession>>;
}

#[async_trait]
pub trait AgentSession: Send {
    async fn run_turn(
        &mut self,
        settings: &Settings,
        issue: &Issue,
        prompt: &str,
        on_event: &UnboundedSender<AgentEvent>,
    ) -> Result<TurnResult>;

    async fn stop(&mut self) -> Result<()>;
    fn process_id(&self) -> Option<u32>;
}
```

The exact shape may differ, but the core requirement is stable: `runner.rs` should depend on a
provider-neutral session interface, not on `CodexSession`.

### Decision 2: Keep the orchestrator unchanged

`rust/src/orchestrator.rs` should stay ignorant of whether a worker is backed by Codex, Claude, or
Gemini. It only needs a normalized stream of worker outcomes and runtime events.

This preserves the existing retry, continuation, and concurrency model and avoids widening the most
critical state machine in the system.

### Decision 3: Start with provider selection at workflow scope

The first implementation should support one provider per workflow, for example:

```yaml
agent:
  provider: codex
```

or:

```yaml
agent:
  provider: claude
```

or:

```yaml
agent:
  provider: gemini
```

Per-issue or per-state routing can be added later, but it should not complicate the first rollout.

### Decision 4: Prefer adapters over protocol emulation inside the orchestrator

Each provider backend should own its own translation layer from the provider's native CLI or SDK
behavior into Symphony's normalized `AgentEvent` stream and `TurnResult`.

Do not teach the orchestrator or generic runner about provider-specific wire formats.

## Options Considered

### Option A: Replace `codex.command` with a different CLI and hope for compatibility

Implementation shape:

- Point the existing `codex.command` at `claude` or `gemini`.
- Reuse `rust/src/agent/codex.rs` unchanged.

Pros:

- Minimal code changes if it worked.

Cons:

- It will not work reliably because the current code expects a specific startup handshake,
  request/response protocol, approval flow, and event model.
- It bakes false assumptions into runtime behavior and would be hard to debug.

Verdict: reject.

### Option B: Add provider-specific branches throughout `runner.rs` and `agent/codex.rs`

Implementation shape:

- Add `if provider == ...` branches where behavior differs.
- Reuse one large session implementation for all providers.

Pros:

- Lower immediate refactor cost than a proper abstraction.

Cons:

- High long-term complexity.
- Error-prone coupling between providers.
- Difficult to test and extend.

Verdict: reject.

### Option C: Extract a provider-neutral runner boundary and add separate backends

Implementation shape:

- Introduce `AgentBackend` and `AgentSession` abstractions.
- Move the current Codex implementation behind `CodexBackend`.
- Add `ClaudeBackend` and `GeminiBackend`.

Pros:

- Clean separation of concerns.
- Low risk to orchestrator correctness.
- Scales to additional providers later.

Cons:

- Higher up-front refactor cost.
- Requires config and setup migration work.

Verdict: recommended.

## Proposed Architecture

### 1. Replace `CodexSession` as the runner dependency

Refactor `rust/src/runner.rs` so it depends on a provider-neutral session interface. The runner
still owns:

- workspace setup
- prompt generation
- workpad bootstrap and updates
- continuation-turn loop
- PR/check inspection

The backend owns:

- subprocess or SDK session startup
- provider-specific streaming protocol
- approval handling
- user-input-required handling
- tool call mediation
- provider-specific error normalization

### 2. Normalize runtime events

Introduce a provider-neutral event enum, likely by renaming or generalizing the current
`AgentEvent` and `AgentEventKind`.

Candidate shape:

- `SessionStarted`
- `Notification`
- `TurnCompleted`
- `TurnFailed`
- `TurnCancelled`
- `TurnInputRequired`
- `ApprovalAutoApproved`
- `ApprovalRequired`
- `ToolCallCompleted`
- `ToolCallFailed`
- `UnsupportedToolCall`
- `Malformed`
- `OtherMessage`
- `TurnEndedWithError`

Codex should map directly from its current implementation. Claude and Gemini should map their
native outputs into the same enum.

### 3. Move provider selection into config

Add a new config section and phase out the hard-coded `codex` namespace.

Recommended target shape:

```yaml
agent:
  provider: codex
  max_concurrent_agents: 10
  max_turns: 20
  max_retry_backoff_ms: 300000

provider:
  command: codex app-server
  model: gpt-5.4
  reasoning_effort: high
  fast: true
  approval_policy: never
  thread_sandbox: workspace-write
  turn_sandbox_policy:
    type: workspace-write
```

A more explicit alternative is:

```yaml
agent:
  provider: codex

providers:
  codex:
    command: codex app-server
    model: gpt-5.4
  claude:
    command: claude
    model: sonnet
  gemini:
    command: gemini
    model: gemini-2.5-pro
```

Recommendation: use `agent.provider` plus provider-specific blocks in a `providers:` map. That
avoids trying to force one provider's knobs onto all providers.

### 4. Keep dynamic GitHub tools behind a portable interface

The current Codex backend exposes dynamic tool support around GitHub operations. That should be
retained as a normalized backend capability, not as a Codex-only special case in runner code.

The generic backend contract should allow:

- no tools
- GitHub GraphQL tool support
- GitHub REST tool support

Providers that cannot support the same tool semantics in v1 may initially run without them, but
the backend interface should leave room for the capability.

### 5. Split provider modules

Recommended module layout:

- `rust/src/agent/mod.rs`
- `rust/src/agent/backend.rs`
- `rust/src/agent/events.rs`
- `rust/src/agent/codex.rs`
- `rust/src/agent/claude.rs`
- `rust/src/agent/gemini.rs`

`rust/src/agent/codex.rs` is the Codex reference implementation of the backend contract.

## Provider-Specific Strategy

### Codex backend

Codex becomes the reference implementation of the new backend trait. Behavior should stay the same
as today:

- persistent session across continuation turns
- support for approval policy and sandbox policy
- dynamic GitHub tools
- existing timeout handling

This backend should land first as a no-behavior-change refactor.

### Claude backend

Claude support should be implemented behind a dedicated backend that uses Claude's native
non-interactive or SDK-backed execution model and maps it into Symphony's session and turn contract.

Important design points:

- Claude may not match Codex's session/thread model exactly.
- If persistent threads are not available or are awkward, the backend can emulate continuity at the
  Symphony layer by carrying forward prompt context, but this should be explicit and documented.
- Approval and tool-call semantics may differ and must be normalized inside the backend.

### Gemini backend

Gemini support should follow the same pattern as Claude:

- use Gemini's native CLI or SDK execution model
- normalize structured streaming or JSON output into Symphony events
- keep provider-specific auth and invocation logic inside the backend

Gemini may prove easier than Claude for CLI automation if structured machine-readable output is
stable, but that should be validated during implementation rather than assumed.

## Implementation Phases

### Phase 1: Codex extraction refactor

Deliverables:

- add provider-neutral backend/session traits
- move current Codex logic behind `CodexBackend`
- rename normalized event types away from Codex-specific naming
- keep workflow behavior unchanged
- preserve existing tests or port them to the new interface

Exit criteria:

- existing Codex flows still pass
- no user-visible behavior change beyond internal code movement

### Phase 2: Config and setup migration

Deliverables:

- add `agent.provider`
- add provider-specific config blocks
- maintain backward compatibility for existing `codex:` config for at least one migration cycle
- update generated setup artifacts, `doctor`, and auth/status output

Exit criteria:

- old workflows still load
- new workflows can explicitly select `codex`
- setup and doctor speak in provider-aware terms rather than Codex-only terms

### Phase 3: Claude backend

Deliverables:

- initial Claude backend implementation
- backend-specific auth and doctor checks
- normalized event mapping
- explicit documentation of limitations relative to Codex

Exit criteria:

- Symphony can run a workflow with `agent.provider: claude`
- worker completion, retry, timeout, and continuation semantics work end to end

### Phase 4: Gemini backend

Deliverables:

- initial Gemini backend implementation
- backend-specific auth and doctor checks
- normalized event mapping
- explicit documentation of limitations relative to Codex

Exit criteria:

- Symphony can run a workflow with `agent.provider: gemini`
- worker completion, retry, timeout, and continuation semantics work end to end

### Phase 5: Optional routing and capability expansion

Possible follow-ons:

- per-state provider routing
- per-issue provider overrides
- provider-specific prompt tuning
- provider capability matrix for tools, approvals, sandboxing, and continuation quality

This phase should only start after one-provider-per-workflow operation is stable.

## Testing Plan

### Unit tests

- config parsing for `agent.provider` and provider blocks
- backend selection from workflow settings
- event normalization for each provider backend
- timeout and failure mapping behavior

### Integration tests

- mock backend scripts for Codex, Claude, and Gemini backends
- success, failure, cancellation, timeout, and malformed-output cases
- continuation-turn behavior across multiple turns
- unsupported tool call behavior

### End-to-end validation

- run a small issue workflow with each provider
- verify workpad updates, PR detection, retry scheduling, and cleanup still work
- verify `doctor` catches missing provider binaries and missing auth

## Risks

### Risk 1: Session model mismatch

Claude or Gemini may not map neatly onto Codex's persistent session and continuation-turn model.

Mitigation:

- treat session continuity as a backend concern
- allow a backend to emulate continuity if needed
- document any behavior differences explicitly

### Risk 2: Tool and approval feature mismatch

Dynamic GitHub tools, sandbox policy, and approval flows may not be equally available across
providers.

Mitigation:

- define a capability matrix
- keep unsupported capabilities backend-local rather than leaking complexity into the orchestrator
- fail clearly when a workflow requires a capability a provider does not support

### Risk 3: Config migration complexity

Renaming the `codex` config namespace risks breaking existing workflows and setup output.

Mitigation:

- keep backward compatibility for one or more releases
- emit deprecation warnings
- update setup-generated files first

### Risk 4: Test surface expansion

Adding multiple providers increases the number of runtime combinations significantly.

Mitigation:

- standardize as much test behavior as possible around a shared backend contract
- use fixture-backed fake provider processes for deterministic coverage

## Rough Effort Estimate

Assuming one engineer already familiar with the repo:

- Phase 1: 2 to 4 days
- Phase 2: 1 to 3 days
- Phase 3: 3 to 5 days
- Phase 4: 3 to 5 days
- Hardening and docs: 2 to 4 days

Total: roughly 2 to 4 weeks for a solid implementation, depending on provider integration friction
and how much feature parity is required with Codex.

## Recommended First Milestone

The first milestone should be a no-behavior-change Codex refactor that introduces the backend
interface and proves the runner can operate without depending directly on `CodexSession`.

That milestone reduces the risk of every later provider addition and gives a clean seam for Claude
and Gemini without destabilizing the orchestrator.
