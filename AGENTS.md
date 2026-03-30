# Agent Instructions

## PR Creation Rules

When opening a pull request for this repository, always start from the repo template:

`gh pr create --template pull_request_template.md --editor`

Use the template name, not the `.github/...` path. `gh` (GitHub CLI) resolves PR templates by
name and rejects the repo-relative path form.

Rules:

- Keep these exact section headers:
  - `#### Context`
  - `#### TL;DR`
  - `#### Summary`
  - `#### Alternatives`
  - `#### Test Plan`
- Remove all template HTML comments before submitting.
- Do not include `TODO` or `TBD` in the final PR body.
- Do not use ad-hoc `--body` or `--fill` for PR creation in this repo.

## PR Update Rules

When editing an existing PR body, preserve the same required sections and ordering from
`.github/pull_request_template.md`, and do not add HTML comments.
