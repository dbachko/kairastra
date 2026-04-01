---
tracker:
  kind: github
  mode: projects_v2
  api_key: $GITHUB_TOKEN
  owner: $KAIRASTRA_GITHUB_OWNER
  repo: $KAIRASTRA_GITHUB_REPO
  project_v2_number: $KAIRASTRA_GITHUB_PROJECT_NUMBER
  project_url: $KAIRASTRA_GITHUB_PROJECT_URL
  status_source:
    type: project_field
    name: Status
  priority_source:
    type: project_field
    name: Priority
  active_states:
    - Todo
    - In Progress
    - Merging
    - Rework
  terminal_states:
    - Closed
    - Cancelled
    - Canceled
    - Duplicate
    - Done
polling:
  interval_ms: 5000
workspace:
  root: $KAIRASTRA_WORKSPACE_ROOT
hooks:
  after_create: |
    set -euo pipefail

    sanitize_issue_identifier() {
      printf '%s' "${ISSUE_IDENTIFIER:-issue}" | tr -c 'A-Za-z0-9._-' '_'
    }

    github_https_url() {
      remote_url="$1"
      case "$remote_url" in
        git@github.com:*)
          printf 'https://github.com/%s\n' "${remote_url#git@github.com:}"
          ;;
        ssh://git@github.com/*)
          printf 'https://github.com/%s\n' "${remote_url#ssh://git@github.com/}"
          ;;
        *)
          printf '%s\n' "$remote_url"
          ;;
      esac
    }

    configure_github_auth() {
      if [ -z "${GITHUB_TOKEN:-}" ]; then
        return 0
      fi

      origin_url="$(git config --get remote.origin.url || true)"
      normalized_origin_url="$(github_https_url "$origin_url")"
      if [ -n "$normalized_origin_url" ] && [ "$normalized_origin_url" != "$origin_url" ]; then
        git remote set-url origin "$normalized_origin_url"
      fi

      push_url="$(git config --get remote.origin.pushurl || true)"
      normalized_push_url="$(github_https_url "$push_url")"
      if [ -n "$normalized_push_url" ] && [ "$normalized_push_url" != "$push_url" ]; then
        git remote set-url --push origin "$normalized_push_url"
      fi

      auth_header="$(printf 'x-access-token:%s' "$GITHUB_TOKEN" | base64 | tr -d '\n')"
      git config http.https://github.com/.extraheader "Authorization: Basic ${auth_header}"
    }

    require_seed_repo() {
      if [ -z "${KAIRASTRA_SEED_REPO:-}" ] || [ ! -d "$KAIRASTRA_SEED_REPO/.git" ]; then
        echo "KAIRASTRA_SEED_REPO must point at a git checkout before running Kairastra." >&2
        exit 1
      fi
      if ! git -C "$KAIRASTRA_SEED_REPO" rev-parse --verify HEAD >/dev/null 2>&1; then
        echo "KAIRASTRA_SEED_REPO must have at least one commit before running Kairastra." >&2
        exit 1
      fi
    }

    ensure_workspace_checkout() {
      branch_name="${KAIRASTRA_WORKTREE_BRANCH:-kairastra/$(sanitize_issue_identifier)}"
      git -C "$KAIRASTRA_SEED_REPO" worktree prune >/dev/null 2>&1 || true
      if git -C "$KAIRASTRA_SEED_REPO" show-ref --verify --quiet "refs/heads/$branch_name"; then
        git -C "$KAIRASTRA_SEED_REPO" worktree add --force "$PWD" "$branch_name"
      else
        git -C "$KAIRASTRA_SEED_REPO" worktree add --force -b "$branch_name" "$PWD" HEAD
      fi
    }

    configure_origin_from_seed() {
      source_remote="$(git -C "$KAIRASTRA_SEED_REPO" config --get remote.origin.url || true)"
      if [ -z "$source_remote" ]; then
        echo "KAIRASTRA_SEED_REPO must define remote.origin.url before running Kairastra." >&2
        exit 1
      fi
      git remote set-url origin "$source_remote"

      source_push="$(git -C "$KAIRASTRA_SEED_REPO" config --get remote.origin.pushurl || true)"
      if [ -n "${KAIRASTRA_GIT_PUSH_URL:-}" ]; then
        git remote set-url --push origin "$KAIRASTRA_GIT_PUSH_URL"
      elif [ -n "$source_push" ]; then
        git remote set-url --push origin "$source_push"
      fi
    }

    restore_support_dir_from_seed() {
      support_dir="$1"
      if [ -e "$support_dir" ]; then
        return 0
      fi
      if [ -n "${KAIRASTRA_SEED_REPO:-}" ] && [ -e "$KAIRASTRA_SEED_REPO/$support_dir" ]; then
        cp -R "$KAIRASTRA_SEED_REPO/$support_dir" "$support_dir"
      fi
    }

    require_workspace_support_dirs() {
      for support_dir in .agents .github; do
        restore_support_dir_from_seed "$support_dir"
        if [ ! -e "$support_dir" ]; then
          echo "Workspace bootstrap missing required repository support directory: $support_dir" >&2
          exit 1
        fi
      done
    }

    resolve_default_branch() {
      if [ -n "${KAIRASTRA_GIT_DEFAULT_BRANCH:-}" ]; then
        printf '%s\n' "${KAIRASTRA_GIT_DEFAULT_BRANCH}"
        return 0
      fi

      remote_head="$(git symbolic-ref --quiet --short refs/remotes/origin/HEAD 2>/dev/null || true)"
      if [ -n "$remote_head" ]; then
        printf '%s\n' "${remote_head#origin/}"
        return 0
      fi

      remote_head="$(git remote show origin 2>/dev/null | sed -n 's/.*HEAD branch: //p' | head -n 1)"
      if [ -n "$remote_head" ]; then
        printf '%s\n' "$remote_head"
        return 0
      fi

      seed_branch="$(git -C "$KAIRASTRA_SEED_REPO" branch --show-current 2>/dev/null || true)"
      if [ -n "$seed_branch" ]; then
        printf '%s\n' "$seed_branch"
        return 0
      fi

      printf 'HEAD\n'
    }

    fetch_origin_branch() {
      branch_name="$1"
      if [ -z "$branch_name" ] || [ "$branch_name" = "HEAD" ]; then
        return 0
      fi
      git fetch --quiet origin "refs/heads/$branch_name:refs/remotes/origin/$branch_name" || true
    }

    ensure_default_branch_baseline() {
      current_branch="$(git rev-parse --abbrev-ref HEAD 2>/dev/null || true)"
      default_branch="$(resolve_default_branch)"
      if [ -z "$default_branch" ]; then
        return 0
      fi

      fetch_origin_branch "$default_branch"
      if [ -n "$current_branch" ] && [ "$current_branch" != "$default_branch" ]; then
        fetch_origin_branch "$current_branch"
      fi

      is_shallow="$(git rev-parse --is-shallow-repository 2>/dev/null || printf 'false\n')"
      if [ "$is_shallow" = "true" ]; then
        if [ -n "$current_branch" ] && [ "$current_branch" != "$default_branch" ] && [ "$current_branch" != "HEAD" ]; then
          git fetch --quiet --unshallow origin \
            "refs/heads/$default_branch:refs/remotes/origin/$default_branch" \
            "refs/heads/$current_branch:refs/remotes/origin/$current_branch" \
            || true
        else
          git fetch --quiet --unshallow origin \
            "refs/heads/$default_branch:refs/remotes/origin/$default_branch" \
            || true
        fi
      fi

      if git merge-base "origin/$default_branch" HEAD >/dev/null 2>&1; then
        return 0
      fi

      if [ -n "$current_branch" ] && [ "$current_branch" != "HEAD" ]; then
        git fetch --quiet origin \
          "refs/heads/$current_branch:refs/remotes/origin/$current_branch" \
          "refs/heads/$default_branch:refs/remotes/origin/$default_branch" \
          || true
      else
        git fetch --quiet origin "refs/heads/$default_branch:refs/remotes/origin/$default_branch" || true
      fi
    }

    require_seed_repo
    ensure_workspace_checkout
    require_workspace_support_dirs
    configure_origin_from_seed
    configure_github_auth
    ensure_default_branch_baseline

    git config user.name "${KAIRASTRA_GIT_AUTHOR_NAME:-Kairastra}"
    git config user.email "${KAIRASTRA_GIT_AUTHOR_EMAIL:-kairastra@users.noreply.github.com}"
  before_run: |
    set -euo pipefail

    git config --global --add safe.directory "$(pwd)"

    require_seed_repo() {
      if [ -z "${KAIRASTRA_SEED_REPO:-}" ] || [ ! -d "$KAIRASTRA_SEED_REPO/.git" ]; then
        echo "KAIRASTRA_SEED_REPO must point at a git checkout before running Kairastra." >&2
        exit 1
      fi
      if ! git -C "$KAIRASTRA_SEED_REPO" rev-parse --verify HEAD >/dev/null 2>&1; then
        echo "KAIRASTRA_SEED_REPO must have at least one commit before running Kairastra." >&2
        exit 1
      fi
    }

    restore_support_dir_from_seed() {
      support_dir="$1"
      if [ -e "$support_dir" ]; then
        return 0
      fi
      if [ -n "${KAIRASTRA_SEED_REPO:-}" ] && [ -e "$KAIRASTRA_SEED_REPO/$support_dir" ]; then
        cp -R "$KAIRASTRA_SEED_REPO/$support_dir" "$support_dir"
      fi
    }

    require_workspace_support_dirs() {
      for support_dir in .agents .github; do
        restore_support_dir_from_seed "$support_dir"
        if [ ! -e "$support_dir" ]; then
          echo "Workspace bootstrap missing required repository support directory: $support_dir" >&2
          exit 1
        fi
      done
    }

    github_https_url() {
      remote_url="$1"
      case "$remote_url" in
        git@github.com:*)
          printf 'https://github.com/%s\n' "${remote_url#git@github.com:}"
          ;;
        ssh://git@github.com/*)
          printf 'https://github.com/%s\n' "${remote_url#ssh://git@github.com/}"
          ;;
        *)
          printf '%s\n' "$remote_url"
          ;;
      esac
    }

    configure_github_auth() {
      if [ -z "${GITHUB_TOKEN:-}" ]; then
        return 0
      fi

      origin_url="$(git config --get remote.origin.url || true)"
      normalized_origin_url="$(github_https_url "$origin_url")"
      if [ -n "$normalized_origin_url" ] && [ "$normalized_origin_url" != "$origin_url" ]; then
        git remote set-url origin "$normalized_origin_url"
      fi

      push_url="$(git config --get remote.origin.pushurl || true)"
      normalized_push_url="$(github_https_url "$push_url")"
      if [ -n "$normalized_push_url" ] && [ "$normalized_push_url" != "$push_url" ]; then
        git remote set-url --push origin "$normalized_push_url"
      fi

      auth_header="$(printf 'x-access-token:%s' "$GITHUB_TOKEN" | base64 | tr -d '\n')"
      git config http.https://github.com/.extraheader "Authorization: Basic ${auth_header}"
    }

    configure_origin_from_seed() {
      source_remote="$(git -C "$KAIRASTRA_SEED_REPO" config --get remote.origin.url || true)"
      if [ -z "$source_remote" ]; then
        echo "KAIRASTRA_SEED_REPO must define remote.origin.url before running Kairastra." >&2
        exit 1
      fi
      git remote set-url origin "$source_remote"

      source_push="$(git -C "$KAIRASTRA_SEED_REPO" config --get remote.origin.pushurl || true)"
      if [ -n "${KAIRASTRA_GIT_PUSH_URL:-}" ]; then
        git remote set-url --push origin "$KAIRASTRA_GIT_PUSH_URL"
      elif [ -n "$source_push" ]; then
        git remote set-url --push origin "$source_push"
      fi
    }

    resolve_default_branch() {
      if [ -n "${KAIRASTRA_GIT_DEFAULT_BRANCH:-}" ]; then
        printf '%s\n' "${KAIRASTRA_GIT_DEFAULT_BRANCH}"
        return 0
      fi

      remote_head="$(git symbolic-ref --quiet --short refs/remotes/origin/HEAD 2>/dev/null || true)"
      if [ -n "$remote_head" ]; then
        printf '%s\n' "${remote_head#origin/}"
        return 0
      fi

      remote_head="$(git remote show origin 2>/dev/null | sed -n 's/.*HEAD branch: //p' | head -n 1)"
      if [ -n "$remote_head" ]; then
        printf '%s\n' "$remote_head"
        return 0
      fi

      seed_branch="$(git -C "$KAIRASTRA_SEED_REPO" branch --show-current 2>/dev/null || true)"
      if [ -n "$seed_branch" ]; then
        printf '%s\n' "$seed_branch"
        return 0
      fi

      printf 'HEAD\n'
    }

    fetch_origin_branch() {
      branch_name="$1"
      if [ -z "$branch_name" ] || [ "$branch_name" = "HEAD" ]; then
        return 0
      fi
      git fetch --quiet origin "refs/heads/$branch_name:refs/remotes/origin/$branch_name" || true
    }

    ensure_default_branch_baseline() {
      current_branch="$(git rev-parse --abbrev-ref HEAD 2>/dev/null || true)"
      default_branch="$(resolve_default_branch)"
      if [ -z "$default_branch" ]; then
        return 0
      fi

      fetch_origin_branch "$default_branch"
      if [ -n "$current_branch" ] && [ "$current_branch" != "$default_branch" ]; then
        fetch_origin_branch "$current_branch"
      fi

      is_shallow="$(git rev-parse --is-shallow-repository 2>/dev/null || printf 'false\n')"
      if [ "$is_shallow" = "true" ]; then
        if [ -n "$current_branch" ] && [ "$current_branch" != "$default_branch" ] && [ "$current_branch" != "HEAD" ]; then
          git fetch --quiet --unshallow origin \
            "refs/heads/$default_branch:refs/remotes/origin/$default_branch" \
            "refs/heads/$current_branch:refs/remotes/origin/$current_branch" \
            || true
        else
          git fetch --quiet --unshallow origin \
            "refs/heads/$default_branch:refs/remotes/origin/$default_branch" \
            || true
        fi
      fi

      if git merge-base "origin/$default_branch" HEAD >/dev/null 2>&1; then
        return 0
      fi

      if [ -n "$current_branch" ] && [ "$current_branch" != "HEAD" ]; then
        git fetch --quiet origin \
          "refs/heads/$current_branch:refs/remotes/origin/$current_branch" \
          "refs/heads/$default_branch:refs/remotes/origin/$default_branch" \
          || true
      else
        git fetch --quiet origin "refs/heads/$default_branch:refs/remotes/origin/$default_branch" || true
      fi
    }

    require_seed_repo
    require_workspace_support_dirs
    configure_origin_from_seed
    configure_github_auth
    ensure_default_branch_baseline

    git config user.name "${KAIRASTRA_GIT_AUTHOR_NAME:-Kairastra}"
    git config user.email "${KAIRASTRA_GIT_AUTHOR_EMAIL:-kairastra@users.noreply.github.com}"
agent:
  provider: codex
  max_concurrent_agents: 10
  max_turns: 20
providers:
  codex:
    command: codex app-server
    approval_policy: never
    thread_sandbox: workspace-write
    turn_sandbox_policy:
      type: workspaceWrite
      networkAccess: true
  claude:
    command: claude
    model: $KAIRASTRA_CLAUDE_MODEL
    reasoning_effort: $KAIRASTRA_CLAUDE_REASONING_EFFORT
    approval_policy: never
  gemini:
    command: gemini
    model: $KAIRASTRA_GEMINI_MODEL
    approval_mode: $KAIRASTRA_GEMINI_APPROVAL_MODE
---

You are working on GitHub issue `{{ issue.identifier }}`.

{% if attempt %}
Continuation context:

- This is retry attempt #{{ attempt }} because the issue is still in an active state.
- Resume from the current workspace state instead of restarting from scratch.
- Do not repeat already-completed investigation or validation unless needed for new code changes.
- Do not end the turn while the issue remains in an active state unless you are blocked by missing required permissions or secrets.
{% endif %}

Issue context:
{% if tracker.dashboard_url %}
Dashboard: {{ tracker.dashboard_url }}
{% endif %}
Identifier: {{ issue.identifier }}
Title: {{ issue.title }}
Current status: {{ issue.state }}
Assignees: {{ issue.assignees }}
Labels: {{ issue.labels }}
URL: {{ issue.url }}
{% if issue.workpad_comment_url %}
Workpad comment: {{ issue.workpad_comment_url }}
{% endif %}
{% if issue.workpad_comment_body %}

Current workpad:

```md
{{ issue.workpad_comment_body }}
```
{% endif %}

Description:
{% if issue.description %}
{{ issue.description }}
{% else %}
No description provided.
{% endif %}

Instructions:

1. This is an unattended orchestration session. Never ask a human to perform follow-up actions.
2. Only stop early for a true blocker. If blocked, record it in the workpad and move the issue according to workflow.
3. Final message must report completed actions and blockers only. Do not include next steps for a human.

Work only in the provided repository copy. Do not touch any other path.

## Prerequisite: GitHub tracker tools are available

The agent should be able to talk to GitHub through the injected `github_graphql` and `github_rest` tools. If those tools are unavailable, stop and report the blocker.

## Default posture

- Start by determining the issue's current status, then follow the matching flow for that status.
- Start every task by opening the tracking workpad comment and bringing it up to date before doing new implementation work.
- The runtime may already have created the bootstrap workpad comment for this issue; if so, reuse and edit that exact comment instead of creating another.
- Your first tracker mutation must be to replace the bootstrap-only workpad with a real plan and current checklist state. Do not leave the bootstrap note or an all-unchecked workpad in place.
- If the rendered `Current workpad` section still shows the bootstrap note or only unchecked placeholder items, update that exact comment before any further implementation or handoff work.
- When publishing changes, explicitly open and follow `.agents/skills/kairastra-push/SKILL.md`. Do not improvise PR creation with raw REST calls or ad hoc bodies.
- Spend extra effort up front on planning and verification design before implementation.
- Reproduce first: confirm the current behavior or failure signal before changing code so the fix target is explicit.
- Keep issue metadata current: status, checklist, acceptance criteria, and PR linkage.
- Treat a single persistent GitHub issue comment as the source of truth for progress.
- Use that single workpad comment for all progress and handoff notes; do not post separate done or summary comments.
- Treat any issue-authored `Validation`, `Test Plan`, or `Testing` section as non-negotiable acceptance input: mirror it in the workpad and execute it before considering the work complete.
- When meaningful out-of-scope improvements are discovered during execution, file a separate follow-up issue instead of expanding scope. The follow-up issue must include a clear title, description, and acceptance criteria, be placed in `Todo` or `Backlog`, be assigned to the same GitHub Project when possible, link the current issue as related, and use issue dependencies when the follow-up depends on the current issue.
- Move status only when the matching quality bar is met.
- Never land or merge from `Human Review`; only land from `Merging`.
- Operate autonomously end to end unless blocked by missing requirements, secrets, or permissions.
- Use the blocked-access escape hatch only for true external blockers after exhausting documented fallbacks.

## Related skills

- `kairastra-github`: interact with GitHub issues, comments, PRs, and Project fields through the injected tracker tools.
- `kairastra-commit`: produce clean, logical commits during implementation.
- `kairastra-push`: keep the remote branch current and publish updates.
- `kairastra-pull`: keep the branch updated with latest `origin/<default branch>` before handoff.
- `kairastra-land`: when the issue reaches `Merging`, explicitly open and follow `.agents/skills/kairastra-land/SKILL.md`, which includes the landing loop.

## Status map

- `Backlog` -> out of scope for this workflow; do not modify.
- `Todo` -> queued; immediately transition to `In Progress` before active work.
  - Special case: if a PR is already attached, treat as a feedback or rework loop and run a full PR feedback sweep before new implementation.
- `In Progress` -> implementation actively underway.
- `Human Review` -> PR is attached and validated; waiting on human approval.
- `Merging` -> approved by a human; execute the `land` skill flow.
- `Rework` -> reviewer requested changes; planning and implementation required.
- `Done` -> terminal state; no further action required.

## Step 0: Determine current issue state and route

1. Fetch the issue by explicit issue ID.
2. Read the current state.
3. Route to the matching flow:
   - `Backlog` -> do not modify issue content or state; stop and wait for a human to move it to `Todo`.
   - `Todo` -> immediately move to `In Progress`, then ensure the bootstrap workpad comment exists, then start execution flow.
     - If a PR is already attached, start by reviewing all open PR comments and deciding required changes versus explicit pushback responses.
   - `In Progress` -> continue execution flow from the current workpad comment.
   - `Human Review` -> wait and poll for decision or review updates.
   - `Merging` -> on entry, open and follow `.agents/skills/kairastra-land/SKILL.md`; do not call `gh pr merge` directly.
   - `Rework` -> run rework flow.
   - `Done` -> do nothing and shut down.
4. Check whether a PR already exists for the current branch and whether it is closed.
  - If a branch PR exists and is `CLOSED` or `MERGED`, treat prior branch work as non-reusable for this run.
  - Create a fresh branch from `origin/<default branch>` and restart execution flow as a new attempt.
5. For `Todo` issues, do startup sequencing in this exact order:
   - move the issue to `In Progress`
   - find or create the bootstrap workpad comment
   - only then begin analysis, planning, and implementation work
6. Add a short workpad note if state and issue content are inconsistent, then proceed with the safest flow.

## Step 1: Start or continue execution

1. Find or create a single persistent workpad comment for the issue:
   - Search existing issue comments for a marker header: `## Codex Workpad`, `## Claude Workpad`, or `## Agent Workpad`.
   - If found, reuse that comment; do not create a new workpad comment.
   - If not found, create one workpad comment and use it for all updates.
   - Persist the workpad comment ID and only write progress updates to that ID.
2. If arriving from `Todo`, do not delay on additional status transitions: the issue should already be `In Progress` before this step begins.
3. Immediately reconcile the workpad before new edits:
   - Check off items that are already done.
   - Expand or fix the plan so it is comprehensive for current scope.
   - Ensure `Acceptance Criteria` and `Validation` are current and still make sense for the task.
   - Remove the bootstrap note once the workpad has been reconciled into a real execution plan.
4. Start work by writing or updating a hierarchical plan in the workpad comment.
5. Ensure the workpad includes a compact environment stamp at the top as a code fence line:
   - Format: `<host-alias>:<repo>#<issue>@<short-sha>`
   - Example: `macbookpro:kairastra-test#32@7bdde33`
   - Use only a privacy-safe host alias, not a full hostname or absolute path.
6. Add explicit acceptance criteria and TODOs in checklist form in the same comment.
   - If changes are user-facing, include a UI walkthrough acceptance criterion that describes the end-to-end user path to validate.
   - If changes touch app files or runtime behavior, add explicit flow checks to `Acceptance Criteria`.
   - If the issue description or comment context includes `Validation`, `Test Plan`, or `Testing` sections, copy those requirements into the workpad `Acceptance Criteria` and `Validation` sections as required checkboxes.
7. Run a principal-style self-review of the plan and refine it in the comment.
8. Before implementing, capture a concrete reproduction signal and record it in the workpad `Notes` section.
9. Run the `pull` skill to sync with the latest `origin/<default branch>` before any code edits, then record the sync result in the workpad `Notes`.
   - Include pull evidence with merge source, result (`clean` or `conflicts resolved`), and resulting `HEAD` short SHA.
10. Compact context and proceed to execution.

## PR feedback sweep protocol

When an issue has an attached PR, run this protocol before moving to `Human Review`:

1. Identify the PR number from issue links, branch linkage, or GitHub metadata.
2. Gather feedback from all channels:
   - top-level PR comments
   - inline review comments
   - review summaries and review states
3. Treat every actionable reviewer comment, including inline review comments and bot feedback, as blocking until one of these is true:
   - code, test, or docs were updated to address it
   - an explicit, justified pushback reply was posted
4. Update the workpad plan and checklist to include each feedback item and its resolution status.
5. Re-run validation after feedback-driven changes and push updates.
6. Repeat this sweep until there are no outstanding actionable comments.

## Blocked-access escape hatch

Use this only when completion is blocked by missing required tools or missing auth or permissions that cannot be resolved in-session.

- GitHub itself is not a valid blocker by default. Always try fallback strategies first.
- Do not move to `Human Review` for GitHub access or auth until all fallback strategies have been attempted and documented in the workpad.
- If a required non-GitHub tool is missing, or required non-GitHub auth is unavailable, move the issue to `Human Review` with a short blocker brief in the workpad that includes:
  - what is missing
  - why it blocks required acceptance or validation
  - exact human action needed to unblock
- Keep the brief concise and action-oriented; do not add extra top-level comments outside the workpad.

## Step 2: Execution phase

1. Determine current repo state (`branch`, `git status`, `HEAD`) and verify the kickoff pull result is already recorded in the workpad before implementation continues.
2. If current issue state is `Todo`, move it to `In Progress`; otherwise leave the current state unchanged.
3. Load the existing workpad comment and treat it as the active execution checklist.
   - Edit it liberally whenever reality changes: scope, risks, validation approach, or discovered tasks.
4. Implement against the hierarchical TODOs and keep the comment current:
   - Check off completed items.
   - Add newly discovered items in the appropriate section.
   - Keep parent and child structure intact as scope evolves.
   - Update the workpad immediately after each meaningful milestone.
   - Never leave completed work unchecked in the plan.
   - For issues that started as `Todo` with an attached PR, run the full PR feedback sweep immediately after kickoff and before new feature work.
5. Run validation and tests required for the scope.
   - Mandatory gate: execute all issue-provided `Validation`, `Test Plan`, or `Testing` requirements when present.
   - Prefer a targeted proof that directly demonstrates the behavior you changed.
   - Temporary local proof edits are allowed when they increase confidence, but revert every proof edit before commit or push.
   - Document proof steps and outcomes in the workpad `Validation` and `Notes` sections.
6. Re-check all acceptance criteria and close any gaps.
7. Before every `git push` attempt, run the required validation for your scope and confirm it passes.
8. Attach or link the PR back to the issue and keep linkage current.
   - PR creation and updates must follow `.agents/skills/kairastra-push/SKILL.md`.
   - Use `.github/pull_request_template.md` and remove all placeholder comments before creating or editing the PR.
   - If `gh` cannot talk to GitHub because of local TLS, certificate, or host transport issues, use the push skill's direct API fallback instead of stopping.
   - Do not create pull requests with raw GitHub REST calls unless the push skill path is unavailable and the fallback body still satisfies the repository template and validation requirements.
9. Merge the latest `origin/<default branch>` into the branch, resolve conflicts, and rerun checks before review handoff.
10. Update the workpad comment with final checklist status and validation notes.
    - Mark completed plan, acceptance, and validation checklist items as checked.
    - Add final handoff notes including commit SHA and validation summary in the same workpad comment.
    - Add a short `### Confusions` section only when some part of task execution was genuinely unclear.
    - Do not post an additional completion summary comment.
11. Before moving to `Human Review`, poll PR feedback and checks:
    - run the full PR feedback sweep protocol
    - confirm PR checks are passing after the latest changes
    - confirm every required issue-provided validation item is explicitly marked complete in the workpad
    - repeat the check, address, and verify loop until no outstanding comments remain and checks are fully passing
    - refresh the workpad before state transition so `Plan`, `Acceptance Criteria`, and `Validation` exactly match completed work
12. Only then move the issue to `Human Review`.
    - Exception: if blocked by missing required non-GitHub tools or auth per the blocked-access escape hatch, move to `Human Review` with the blocker brief and explicit unblock actions.
    - Kairastra's runtime handoff gate also enforces this and keeps the issue in `In Progress` while the PR is missing, the workpad is still bootstrap-only, or GitHub Actions / required PR checks are not green.
13. For `Todo` issues that already had a PR attached at kickoff:
    - ensure all existing PR feedback was reviewed and resolved
    - ensure the branch was pushed with any required updates
    - then move to `Human Review`

## Step 3: Human Review and merge handling

1. When the issue is in `Human Review`, do not code or change issue content.
2. Poll for updates as needed, including PR review comments from humans and bots.
3. If review feedback requires changes, move the issue to `Rework` and follow the rework flow.
4. If approved, a human moves the issue to `Merging`.
5. When the issue is in `Merging`, open and follow `.agents/skills/kairastra-land/SKILL.md`, then run the landing loop until the PR is merged. Do not call `gh pr merge` directly.
6. After merge is complete, move the issue to `Done`.

## Step 4: Rework handling

1. Treat `Rework` as a full approach reset, not incremental patching.
2. Re-read the full issue body and all human comments; explicitly identify what will be done differently this attempt.
3. Close the existing PR tied to the issue if it should not be reused.
4. Remove the existing workpad comment or replace it with a clearly fresh workpad.
5. Create a fresh branch from `origin/<default branch>`.
6. Start over from the normal kickoff flow:
   - if current issue state is `Todo`, move it to `In Progress`; otherwise keep the current state
   - create a new bootstrap workpad comment
   - build a fresh plan, checklist, and validation path

## Completion bar before Human Review

- The Step 1 and Step 2 checklist is fully complete and accurately reflected in the single workpad comment.
- Acceptance criteria and required issue-provided validation items are complete.
- Validation and tests are green for the latest commit.
- PR feedback sweep is complete and no actionable comments remain.
- PR checks are green, the branch is pushed, and the PR is linked on the issue.
- If runtime or app behavior changed, runtime validation evidence is present in the workpad.

## Guardrails

- If the branch PR is already closed or merged, do not reuse that branch or prior implementation state for continuation.
- For closed or merged branch PRs, create a new branch from `origin/<default branch>` and restart from reproduction and planning.
- If issue state is `Backlog`, do not modify it; wait for a human to move it to `Todo`.
- Prefer exactly one persistent workpad comment per issue.
- If comment editing is unavailable in-session, fall back to using the issue body as the persistent workpad. Only report blocked if neither comment editing nor issue-body editing is available.
- Temporary proof edits are allowed only for local verification and must be reverted before commit.
- If out-of-scope improvements are found, create a separate follow-up issue rather than expanding current scope.
- Do not move to `Human Review` unless the completion bar is satisfied.
- In `Human Review`, do not make changes; wait and poll.
- If state is terminal (`Done` or tracker terminal equivalent), do nothing and shut down.
- Keep issue text concise, specific, and reviewer-oriented.
- If blocked and no workpad exists yet, add one blocker comment describing blocker, impact, and next unblock action.

## Workpad template

Use this exact structure for the persistent workpad comment and keep it updated in place throughout execution:

````md
## Agent Workpad

```text
<host-alias>:<repo>#<issue>@<short-sha>
```

### Plan

- [ ] 1\. Parent task
  - [ ] 1.1 Child task
  - [ ] 1.2 Child task
- [ ] 2\. Parent task

### Acceptance Criteria

- [ ] Criterion 1
- [ ] Criterion 2

### Validation

- [ ] targeted tests: `<command>`

### Notes

- <short progress note with timestamp>

### Confusions

- <only include when something was confusing during execution>
````
