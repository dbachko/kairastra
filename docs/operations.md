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

Older generated workflows may have added `.github/` or `.agents/` to the seed's shared Git
`info/exclude`. Regenerated hooks remove those exact legacy rules and preserve other entries.
Cleanup preserves a workspace with those rules still present and reports
`managed_worktree_legacy_exclusions`; run the regenerated hooks or remove the legacy rules
before retrying cleanup. Newly visible uncommitted files continue to block deletion.

Claude and Gemini attempts keep a lease under
`<workspace-root>/.kairastra-provider-leases/<workspace-path-hash>`, outside the checkout,
until the provider exits and its input/output handlers finish. Git cleanup cannot remove
the lease from inside the checkout. A cleanup timeout returns promptly while the runtime
continues waiting in the background. The lease blocks workspace reuse, removal, and
`after_run` hooks until inherited stderr closes. If the daemon stops or stderr cleanup fails
before this can be verified, the lease remains. Stop the daemon, verify that no provider or
descendant still uses the workspace, and remove only the stale lease before retrying.

- One runtime does not manage multiple repositories.
- The current implementation targets local workers only.
- The operator UX is terminal-first; there is no web onboarding flow here.

## References

- [troubleshooting.md](troubleshooting.md): failure-oriented fixes
- [deployment.md](deployment.md): installation and service setup
- [auth.md](auth.md): GitHub token and provider auth guidance
