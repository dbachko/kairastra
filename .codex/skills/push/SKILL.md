---
name: push
description:
  Push current branch changes to origin and create or update the corresponding
  pull request; use when asked to push, publish updates, or create pull request.
---

# Push

## Prerequisites

- `gh` CLI is preferred when it can talk to GitHub successfully in this repo.
- If `gh` API calls fail because of local TLS/certificate problems or similar
  host-specific transport issues, fall back to direct GitHub API calls with the
  existing repo token instead of stopping.

## Goals

- Push current branch changes to `origin` safely.
- Create a PR if none exists for the branch, otherwise update the existing PR.
- Keep branch history clean when remote has moved.

## Related Skills

- `pull`: use this when push is rejected or sync is not clean (non-fast-forward,
  merge conflict risk, or stale branch).

## Steps

1. Identify current branch and confirm remote state.
2. Run local validation appropriate for the current change before pushing.
   - For runtime/code changes in this repo, default to `cargo fmt --check` and
     `cargo test` from `rust/`.
   - For docs-only changes, run the smallest relevant verification and state
     what you skipped.
3. Push branch to `origin` with upstream tracking if needed, using whatever
   remote URL is already configured.
4. If push is not clean/rejected:
   - If the failure is a non-fast-forward or sync problem, run the `pull`
     skill to merge `origin/main`, resolve conflicts, and rerun validation.
   - Push again; use `--force-with-lease` only when history was rewritten.
   - If the failure is due to auth, permissions, or workflow restrictions on
     the configured remote, stop and surface the exact error instead of
     rewriting remotes or switching protocols as a workaround.

5. Ensure a PR exists for the branch:
   - If no PR exists, create one.
   - If a PR exists and is open, update it.
   - If branch is tied to a closed/merged PR, create a new branch + PR.
   - Write a proper PR title that clearly describes the change outcome
   - For branch updates, explicitly reconsider whether current PR title still
     matches the latest scope; update it if it no longer does.
6. Write/update PR body explicitly using `.github/pull_request_template.md`:
   - Fill every section with concrete content for this change.
   - Replace all placeholder comments (`<!-- ... -->`).
   - Keep bullets/checkboxes where template expects them.
   - If PR already exists, refresh body content so it reflects the total PR
     scope (all intended work on the branch), not just the newest commits,
     including newly added work, removed work, or changed approach.
   - Do not reuse stale description text from earlier iterations.
7. Validate the PR body locally before finishing:
   - Ensure all template sections are filled.
   - Ensure placeholder comments (`<!-- ... -->`) are removed.
   - Ensure the `Test Plan` reflects the actual validation run for this change.
8. If `gh pr create` / `gh pr edit` cannot reach GitHub because of local TLS,
   certificate, or host transport issues, use a direct API fallback:
   - Reuse the exact same title and fully rendered PR body file.
   - Prefer `github_rest` / `github_graphql` if injected.
   - Otherwise use `curl` with `GITHUB_TOKEN`, `GH_TOKEN`, or the token already
     embedded in the configured `origin` URL.
   - The fallback must still satisfy the repo PR template and placeholder
     checks; this is not a license to post an ad hoc PR body.
9. Reply with the PR URL from `gh pr view` or the direct API response.
10. Never rely on interactive `gh` prompts. All `gh pr create` / `gh pr edit`
    calls must pass both the title and a concrete body file.

## Commands

```sh
# Identify branch
branch=$(git branch --show-current)

# Minimal validation gate
(cd rust && cargo fmt --check && cargo test)

# Initial push: respect the current origin remote.
git push -u origin HEAD

# If that failed because the remote moved, use the pull skill. After
# pull-skill resolution and re-validation, retry the normal push:
git push -u origin HEAD

# If the configured remote rejects the push for auth, permissions, or workflow
# restrictions, stop and surface the exact error.

# Only if history was rewritten locally:
git push --force-with-lease origin HEAD

# Ensure a PR exists (create only if missing)
pr_state=$(gh pr view --json state -q .state 2>/dev/null || true)
if [ "$pr_state" = "MERGED" ] || [ "$pr_state" = "CLOSED" ]; then
  echo "Current branch is tied to a closed PR; create a new branch + PR." >&2
  exit 1
fi

# Write a clear, human-friendly title that summarizes the shipped change.
pr_title="<clear PR title written for this change>"
tmp_pr_body=$(mktemp)
# Draft a fully concrete body from .github/pull_request_template.md before any
# gh PR mutation. Do not leave template comments or placeholders behind.
# Example workflow:
# 1) copy the template into $tmp_pr_body
# 2) replace every placeholder section with real content for this change
# 3) use the same body file for both create and edit flows
cp .github/pull_request_template.md "$tmp_pr_body"

if rg -n '<!--|TODO|TBD' "$tmp_pr_body"; then
  echo "PR body still contains placeholders" >&2
  exit 1
fi

if [ -z "$pr_state" ]; then
  gh pr create --title "$pr_title" --body-file "$tmp_pr_body"
else
  # Reconsider title on every branch update; edit if scope shifted.
  gh pr edit --title "$pr_title" --body-file "$tmp_pr_body"
fi

# Show PR URL for the reply
gh pr view --json url -q .url

rm -f "$tmp_pr_body"
```

## Direct API Fallback

Use this only when the `gh` path fails because the local environment cannot
talk to GitHub cleanly.

```sh
branch=$(git branch --show-current)
repo_slug=$(git remote get-url origin | sed -E 's#(https://x-access-token:[^@]+@|https://|git@)github.com[:/]##; s#\\.git$##')
repo_owner=${repo_slug%%/*}
repo_name=${repo_slug#*/}
token=${GITHUB_TOKEN:-${GH_TOKEN:-}}
if [ -z "$token" ]; then
  token=$(git remote get-url origin | sed -nE 's#https://x-access-token:([^@]+)@github.com/.*#\\1#p')
fi

if [ -z "$token" ]; then
  echo "No GitHub token available for direct API fallback." >&2
  exit 1
fi

existing_pr_number=$(curl -fsSL \
  -H "Authorization: Bearer $token" \
  -H "Accept: application/vnd.github+json" \
  "https://api.github.com/repos/${repo_owner}/${repo_name}/pulls?head=${repo_owner}:${branch}&state=open" \
  | jq -r '.[0].number // empty')

if [ -z "$existing_pr_number" ]; then
  curl -fsSL -X POST \
    -H "Authorization: Bearer $token" \
    -H "Accept: application/vnd.github+json" \
    "https://api.github.com/repos/${repo_owner}/${repo_name}/pulls" \
    -d @<(jq -n \
      --arg title "$pr_title" \
      --arg head "$branch" \
      --arg base "main" \
      --rawfile body "$tmp_pr_body" \
      '{title:$title, head:$head, base:$base, body:$body}') \
    | jq -r '.html_url'
else
  curl -fsSL -X PATCH \
    -H "Authorization: Bearer $token" \
    -H "Accept: application/vnd.github+json" \
    "https://api.github.com/repos/${repo_owner}/${repo_name}/pulls/${existing_pr_number}" \
    -d @<(jq -n \
      --arg title "$pr_title" \
      --rawfile body "$tmp_pr_body" \
      '{title:$title, body:$body}') \
    | jq -r '.html_url'
fi
```

## Notes

- Do not use `--force`; only use `--force-with-lease` as the last resort.
- Distinguish sync problems from remote auth/permission problems:
  - Use the `pull` skill for non-fast-forward or stale-branch issues.
  - Surface auth, permissions, or workflow restrictions directly instead of
    changing remotes or protocols.
