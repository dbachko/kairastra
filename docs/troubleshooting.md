# Troubleshooting

Use these checks when Kairastra setup, auth, or runtime behavior is not matching expectations.

## First Checks

```bash
kairastra doctor
kairastra auth status
```

Confirm:

- `gh` is installed and authenticated
- the selected provider CLI is installed
- provider auth is present
- `WORKFLOW.md` loads cleanly
- `.kairastra/kairastra.env` contains the expected repo and token values

## Common Failures

### Missing provider command

Doctor fails on `agent_provider_command`.

Fix:

- install the selected provider CLI
- re-run `kairastra doctor`

### Missing provider auth

Doctor warns or fails on `agent_provider_auth`.

Fix:

- run `kairastra auth menu`
- or run `kairastra auth --provider <provider> login --mode subscription`
- or set the matching API key env var and re-run doctor

### GitHub tracker failure

Doctor fails on `github_tracker`.

Check:

- wrong `owner` or `repo`
- wrong `project_v2_number`
- missing token scopes
- SSO authorization not granted for the token

### Workspace root failure

Doctor fails on `workspace_root`.

Fix:

- create the configured workspace root or its parent
- make sure the running user can read and write it

## References

- [auth.md](auth.md): auth and token setup
- [deployment.md](deployment.md): native setup and service installation
