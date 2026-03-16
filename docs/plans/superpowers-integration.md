# Integrating `obra/superpowers` Into Symphony

## Summary

This document evaluates how Symphony should expose the [`obra/superpowers`](https://github.com/obra/superpowers)
skill pack to Codex runs and recommends a staged integration plan.

Recommendation: do not enable every superpower by default and do not force users to hand-pick each
skill one by one during setup. Instead, add an optional setup flow that installs a small
recommended core set by default and can also enable the full catalog or extra packs for users who
want them.

## Current Repo Context

Symphony already has two distinct skill layers:

- Repository-local workflow skills in `.codex/skills/` that support Symphony's issue lifecycle.
- Operator environment setup in `rust/src/setup.rs`, `rust/README.md`, `rust/.env.example`, and
  `WORKFLOW.md`.

That split matters because `superpowers` is not a repository feature in the same sense as the local
Symphony skills. Upstream documents it as an operator-installed skill bundle for Codex:

- clone `obra/superpowers`
- symlink or copy the relevant skill directories into `~/.agents/skills/`
- optionally install `collab` if the user wants the subagent-related skills

For Symphony, that means the integration point is the runtime environment used by Codex
(native host or Docker container), not the issue workspace repository alone.

## Goals

- Make useful superpowers available to Symphony-managed Codex sessions.
- Keep setup predictable for first-time operators.
- Avoid surprising behavior changes from enabling a very large skill catalog by default.
- Preserve a clear boundary between checked-in repo skills and user-environment skill bundles.
- Make the feature work for both native and Docker deployments.

## Non-Goals

- Re-vendor the upstream `superpowers` repository into this repo.
- Fork or rewrite upstream skills.
- Build a full marketplace or per-run skill resolver in the first version.

## Options Considered

### Option A: Enable all superpowers by default

Implementation shape:

- Setup always clones `obra/superpowers`.
- Setup always links the entire upstream skills tree into the Codex skill search path.
- Docker images or bootstrap hooks ensure the bundle is present for every run.

Pros:

- Lowest decision burden for operators.
- Fastest path to "everything is available".
- No follow-up setup required for advanced users.

Cons:

- Large behavior change for every Codex session, including simple issue runs that do not need the
  extra skills.
- Higher prompt-context and tool-discovery noise.
- Harder to reason about reproducibility when upstream adds or changes many skills.
- Includes skills that depend on extra tooling or workflows some operators will not want.

Verdict: not recommended as the default.

### Option B: Make users choose individual skills during setup

Implementation shape:

- Setup presents a long checklist of upstream skills.
- Selected skills are linked individually into the runtime skill path.

Pros:

- Maximum operator control.
- Keeps installations minimal.

Cons:

- Too much friction for first-run setup.
- Requires Symphony to maintain an internal catalog of upstream skills and dependencies.
- Fragile when upstream renames, removes, or adds skills.
- Hard to explain in a non-interactive setup flow.

Verdict: too complex for the first implementation.

### Option C: Install a recommended core set and allow optional add-ons

Implementation shape:

- Setup asks whether to enable `superpowers`.
- If enabled, setup offers a small number of modes:
  - `core`
  - `full`
  - `custom` (optional later)
- `core` links only the recommended starter skills.
- `full` links the entire upstream catalog.
- A generated env value and bootstrap hook ensure the chosen set is installed in the actual Codex
  runtime environment.

Pros:

- Good default experience without forcing the whole catalog on everyone.
- Small surface area for the first release.
- Keeps room for power users to opt into the full bundle.
- Easier to explain and automate in both native and Docker paths.

Cons:

- Requires curation of a stable recommended core list.
- Still needs a lightweight installer/bootstrap path.

Verdict: recommended.

## Recommended Product Behavior

### Operator experience

Add a new optional `superpowers` step to `cargo run -- setup`:

1. Ask whether the operator wants to enable upstream superpowers for Codex.
2. If yes, ask for an install mode:
   - `Recommended core`
   - `Full catalog`
   - `Disabled`
3. Optionally ask whether `collab`-dependent skills should be enabled when that dependency is
   available.

For non-interactive setup:

- default to disabled unless an explicit environment variable opts in
- support values such as:
  - `SYMPHONY_SUPERPOWERS_MODE=off|core|full`
  - `SYMPHONY_SUPERPOWERS_ENABLE_COLLAB=true|false`

### Runtime behavior

Symphony should not depend on the internet at issue-run time for this feature. Instead:

- setup records the desired mode in generated config
- runtime bootstrap ensures the pinned upstream repo is present in a stable tool location
- runtime bootstrap creates the necessary symlinks into the Codex-visible skills directory before
  Codex starts

This should happen in the environment where Codex actually runs:

- native mode: on the VPS user account that launches Symphony
- Docker mode: inside the container filesystem or a mounted persistent tools volume

### Repository behavior

Keep the checked-in `.codex/skills/` directory focused on Symphony's own repo-specific workflows.
Do not mix upstream `superpowers` skill contents into this repo tree. The repo should only store:

- configuration knobs
- bootstrap logic
- documentation
- possibly a small curated manifest of the recommended core set

## Proposed Technical Design

### 1. Add setup/config inputs

Extend `rust/src/setup.rs` and generated config outputs with:

- `SYMPHONY_SUPERPOWERS_MODE`
- `SYMPHONY_SUPERPOWERS_REF`
- `SYMPHONY_SUPERPOWERS_ENABLE_COLLAB`
- `SYMPHONY_SUPERPOWERS_HOME`

Suggested defaults:

- `SYMPHONY_SUPERPOWERS_MODE=off`
- `SYMPHONY_SUPERPOWERS_REF=<pinned commit or tag>`
- `SYMPHONY_SUPERPOWERS_ENABLE_COLLAB=false`
- `SYMPHONY_SUPERPOWERS_HOME=/opt/symphony/superpowers` in native mode
- `SYMPHONY_SUPERPOWERS_HOME=/opt/superpowers` in Docker mode

### 2. Introduce a curated manifest for the core set

Add a checked-in manifest file, for example:

- `docs/plans/` for the initial spec only
- later implementation: `.codex/superpowers-core.txt` or `rust/config/superpowers-core.txt`

The manifest should contain the upstream skill directory names Symphony considers safe and broadly
useful for most runs. This avoids baking the core list directly into Rust source and keeps updates
reviewable.

Selection guidance for the initial core set:

- skills that improve analysis, debugging, or planning
- skills with low external dependency burden
- skills that do not assume a specific language or deployment stack
- exclude subagent-oriented or heavy workflow skills unless `collab` is enabled

### 3. Bootstrap install/link logic

Add a dedicated bootstrap script that:

- clones or updates `obra/superpowers` at the pinned ref
- optionally verifies the repo state after fetch
- clears and recreates a managed Symphony-owned skill link directory
- links either the curated core manifest or all upstream skills into the Codex-visible location
- optionally links `collab`-dependent skills only when requested and dependency checks pass

Prefer a single Symphony-managed destination such as:

- native: `~/.agents/skills/symphony-superpowers/`
- Docker: `/root/.agents/skills/symphony-superpowers/`

Then point the runtime bootstrap at that managed directory instead of scattering symlinks directly
across the base skills folder.

### 4. Call bootstrap from the runtime hooks

Add the bootstrap invocation in a place that runs before Codex sessions start, not inside every
issue task unless necessary.

Best initial placement:

- native setup: as part of setup output instructions plus doctor validation
- Docker setup: in image startup or container bootstrap before the orchestrator loop starts

Fallback placement:

- `WORKFLOW.md` `before_run` hook can verify the managed skill directory exists and self-heal if it
  does not

Avoid cloning/updating the upstream repo inside each issue workspace. That would make runs slower,
less deterministic, and harder to cache.

### 5. Add doctor coverage

Extend `doctor` so that when `SYMPHONY_SUPERPOWERS_MODE != off` it checks:

- the upstream checkout exists
- the pinned ref is resolvable
- the managed skill directory exists
- configured links resolve correctly
- optional `collab` dependency is present when enabled

## Suggested Rollout Plan

### Phase 1: Documentation and config scaffolding

- document the feature and operating model
- add setup prompts and generated env values
- add placeholder doctor warnings for misconfiguration

### Phase 2: Managed installer for `core` and `full`

- add the bootstrap installer/update script
- add the curated core manifest
- wire native and Docker startup flows to install/link the selected mode

### Phase 3: Validation and refinement

- test native and Docker flows end to end
- tune the curated core manifest based on real issue runs
- optionally add a `custom` mode later if operators need finer control

## Validation Plan For The Future Implementation

Native mode:

1. Run `cargo run -- setup --mode native`.
2. Enable `superpowers` in `core` mode.
3. Run `cargo run -- doctor`.
4. Confirm the managed skill directory exists and links resolve.
5. Start a Codex-backed Symphony run and verify Codex sees the installed skills.

Docker mode:

1. Run `cargo run -- setup --mode docker`.
2. Enable `superpowers` in `core` mode.
3. Start the stack.
4. Run `cargo run -- doctor --mode docker` or the container equivalent.
5. Verify the container has the managed skill directory and Codex can use it.

Upgrade path:

1. Change `SYMPHONY_SUPERPOWERS_REF` to a newer pinned ref.
2. Re-run the installer/bootstrap path.
3. Verify links still resolve and the curated core manifest still matches upstream paths.

## Final Recommendation

Adopt Option C:

- default `superpowers` to off
- offer `core` and `full` modes in setup
- ship a curated core manifest
- install the upstream repo into the runtime environment, not into each issue workspace
- keep repo-local Symphony skills separate from upstream `superpowers`

This gives Symphony a controlled, supportable integration path without turning first-run setup into
a long skill-selection wizard or forcing every operator onto the full upstream bundle.
