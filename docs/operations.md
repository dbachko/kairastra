# Operations Guide

This page collects the routine commands and operational notes for a running Kairastra deployment.

## Doctor Checks

Run doctor before enabling the service, after changing auth, or when a deployment behaves
unexpectedly.

Examples:

```bash
kairastra doctor
kairastra doctor --format json
```

Doctor currently checks:

- required local commands such as the selected provider CLI, `gh`, and `systemctl`
- selected provider auth state
- workflow load and validation
- for `workspace.bootstrap_mode: seed_worktree`, seed-repo git readiness, shared skills, and required support directories such as `.github/`
- repo label readiness for the fixed Kairastra labels plus labels derived from the configured statuses
- required GitHub Project fields when `projects_v2` is configured
- GitHub tracker connectivity using the configured token
- for `projects_v2`, configured Project status mappings and transition targets
- workspace root existence or whether its parent exists

## Day-2 Commands

Useful commands once Kairastra is running:

```bash
kairastra auth menu
kairastra auth status
kairastra doctor
kairastra run
journalctl -u kairastra.service -f
```

## GitHub Bootstrap

`kairastra setup` now inspects the GitHub labels and Project fields that the generated workflow
expects. In interactive mode it asks before applying missing labels or fields. In
`--non-interactive` mode it stays read-only unless you pass `--bootstrap-github`.

## Current Limitations

- One runtime does not manage multiple repositories.
- The current implementation targets local workers only.
- The operator UX is terminal-first; there is no web onboarding flow here.

## References

- [troubleshooting.md](troubleshooting.md): failure-oriented fixes
- [deployment.md](deployment.md): installation and service setup
- [auth.md](auth.md): GitHub token and provider auth guidance
