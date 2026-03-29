# Public Repo Readiness

Use this checklist before changing repo visibility from private to public.

## What becomes public

Changing visibility on the existing repository exposes:

- full git commit history and file history
- issues, pull requests, reviews, comments, and discussions
- GitHub Actions logs and artifacts that are still retained

Treat visibility flip as irreversible disclosure of historical metadata.

## Pre-public checklist

1. Run the audit script locally and resolve findings.
2. Ensure default branch checks are green.
3. Confirm no pending commits with local keys, dumps, or env files.
4. Confirm publication owner accepts metadata exposure.

Run the audit:

```bash
bash scripts/public-readiness-audit.sh
```

Artifacts are written under `reports/public-readiness/<timestamp>/` and include:

- `SUMMARY.md` (final gate result)
- `gitleaks.sarif`
- `history-path-findings.txt`
- `working-tree-content-findings.txt`
- `history-content-findings.txt`

## Blocker policy

Block publication on any unresolved secret-risk finding, including:

- real credentials or tokens
- private key material
- sensitive data dumps or exports with confidential data

Placeholders, fake tokens in tests, and empty templates are non-blockers only after explicit triage.

## GitHub Actions audit workflow

Manual and CI audit workflow:

- `.github/workflows/public-readiness-audit.yml`

It runs the same script and uploads artifacts for review.

## Post-flip verification

After visibility is changed to public:

1. Verify secret scanning is enabled/available.
2. Re-run the audit workflow on `main`.
3. Review any GitHub secret-scanning alerts before announcement.

Secret scanning API check example:

```bash
gh api repos/dbachko/kairastra/secret-scanning/alerts --paginate
```

## Remote bootstrap URL guidance

For a public repository, `raw.githubusercontent.com` URLs should use:

- branch name (for example `main`),
- a release tag (for example `v0.1.0-alpha.1`), or
- a commit SHA.

Use `sed`/`cat` for non-interactive review instead of `less` in scripted host commands.

Latest `main` example (run on the host):

```bash
curl -fsSL -o /tmp/install-remote-docker.sh https://raw.githubusercontent.com/dbachko/kairastra/main/scripts/install-remote-docker.sh && bash /tmp/install-remote-docker.sh --ref main
```

Pinned ref example (run on the host):

```bash
curl -fsSL -o /tmp/install-remote-docker.sh https://raw.githubusercontent.com/dbachko/kairastra/v0.1.0-alpha.1/scripts/install-remote-docker.sh && bash /tmp/install-remote-docker.sh --ref v0.1.0-alpha.1
```
