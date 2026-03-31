use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use tokio::process::Command;
use tokio::time::{timeout, Duration};

use crate::config::Settings;
use crate::model::Issue;
use crate::providers;
use crate::workflow::{
    default_repo_workflow, load_repo_workflow, RepoWorkflow, REPO_WORKFLOW_FILENAME,
};

const DEFAULT_XDG_CACHE_HOME: &str = "/tmp/kairastra/xdg-cache";
const DEFAULT_COREPACK_HOME: &str = "/tmp/kairastra/corepack";
const DEFAULT_PNPM_HOME: &str = "/tmp/kairastra/pnpm";
const DEFAULT_NPM_CONFIG_CACHE: &str = "/tmp/kairastra/npm-cache";

#[derive(Debug, Clone)]
pub struct Workspace {
    pub path: PathBuf,
    pub workspace_key: String,
    pub created_now: bool,
}

pub fn sanitize_workspace_key(identifier: &str) -> String {
    let sanitized: String = identifier
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-') {
                character
            } else {
                '_'
            }
        })
        .collect();

    if sanitized.is_empty() {
        "issue".to_string()
    } else {
        sanitized
    }
}

pub fn workspace_path(settings: &Settings, identifier: &str) -> Result<PathBuf> {
    let root = ensure_workspace_root(&settings.workspace.root)?;
    Ok(root.join(sanitize_workspace_key(identifier)))
}

pub(crate) fn apply_runtime_tool_env(command: &mut Command) {
    for (name, value) in runtime_tool_env_overrides() {
        command.env(name, value);
    }
}

pub async fn ensure_workspace(settings: &Settings, issue: &Issue) -> Result<Workspace> {
    let root = ensure_workspace_root(&settings.workspace.root)?;
    let workspace_key = sanitize_workspace_key(&issue.identifier);
    let path = root.join(&workspace_key);

    let created_now = if path.exists() {
        if path.is_dir() {
            false
        } else {
            fs::remove_file(&path).or_else(|_| fs::remove_dir_all(&path))?;
            fs::create_dir_all(&path)?;
            true
        }
    } else {
        fs::create_dir_all(&path)?;
        true
    };

    validate_workspace_path(&root, &path)?;

    let workspace = Workspace {
        path,
        workspace_key,
        created_now,
    };

    if workspace.created_now {
        if docker_mode_enabled() {
            run_internal_docker_after_create(settings, &workspace.path, issue).await?;
            let repo_workflow = load_workspace_repo_workflow(&workspace.path)?;
            if let Some(script) = repo_workflow.hooks.after_create.as_deref() {
                run_hook(settings, "after_create", script, &workspace.path, issue).await?;
            }
        } else if let Some(script) = settings.hooks.after_create.as_deref() {
            run_hook(settings, "after_create", script, &workspace.path, issue).await?;
        }
    }

    Ok(workspace)
}

pub async fn run_before_run_hook(
    settings: &Settings,
    workspace: &Path,
    issue: &Issue,
) -> Result<()> {
    if docker_mode_enabled() {
        run_internal_docker_before_run(settings, workspace, issue).await?;
        let repo_workflow = load_workspace_repo_workflow(workspace)?;
        if let Some(script) = repo_workflow.hooks.before_run.as_deref() {
            run_hook(settings, "before_run", script, workspace, issue).await?;
        }
    } else if let Some(script) = settings.hooks.before_run.as_deref() {
        run_hook(settings, "before_run", script, workspace, issue).await?;
    }
    Ok(())
}

pub async fn run_after_run_hook(
    settings: &Settings,
    workspace: &Path,
    issue: &Issue,
) -> Result<()> {
    if docker_mode_enabled() {
        let repo_workflow = load_workspace_repo_workflow(workspace)?;
        if let Some(script) = repo_workflow.hooks.after_run.as_deref() {
            run_hook(settings, "after_run", script, workspace, issue).await?;
        }
    } else if let Some(script) = settings.hooks.after_run.as_deref() {
        run_hook(settings, "after_run", script, workspace, issue).await?;
    }
    Ok(())
}

pub async fn remove_issue_workspace(settings: &Settings, identifier: &str) -> Result<()> {
    let root = ensure_workspace_root(&settings.workspace.root)?;
    let workspace = root.join(sanitize_workspace_key(identifier));
    if !workspace.exists() {
        return Ok(());
    }

    validate_workspace_path(&root, &workspace)?;

    if docker_mode_enabled() {
        if let Ok(repo_workflow) = load_workspace_repo_workflow(&workspace) {
            if let Some(script) = repo_workflow.hooks.before_remove.as_deref() {
                let synthetic_issue = Issue {
                    id: String::new(),
                    project_item_id: None,
                    identifier: identifier.to_string(),
                    title: String::new(),
                    description: None,
                    priority: None,
                    state: String::new(),
                    branch_name: None,
                    url: None,
                    assignees: Vec::new(),
                    labels: Vec::new(),
                    blocked_by: Vec::new(),
                    created_at: None,
                    updated_at: None,
                    workpad_comment_id: None,
                    workpad_comment_url: None,
                    workpad_comment_body: None,
                };
                let _ = run_hook(
                    settings,
                    "before_remove",
                    script,
                    &workspace,
                    &synthetic_issue,
                )
                .await;
            }
        }
    } else if let Some(script) = settings.hooks.before_remove.as_deref() {
        let synthetic_issue = Issue {
            id: String::new(),
            project_item_id: None,
            identifier: identifier.to_string(),
            title: String::new(),
            description: None,
            priority: None,
            state: String::new(),
            branch_name: None,
            url: None,
            assignees: Vec::new(),
            labels: Vec::new(),
            blocked_by: Vec::new(),
            created_at: None,
            updated_at: None,
            workpad_comment_id: None,
            workpad_comment_url: None,
            workpad_comment_body: None,
        };
        let _ = run_hook(
            settings,
            "before_remove",
            script,
            &workspace,
            &synthetic_issue,
        )
        .await;
    }

    fs::remove_dir_all(&workspace)
        .with_context(|| format!("failed to remove {}", workspace.display()))?;
    Ok(())
}

fn ensure_workspace_root(root: &Path) -> Result<PathBuf> {
    fs::create_dir_all(root).with_context(|| format!("failed to create {}", root.display()))?;
    root.canonicalize()
        .with_context(|| format!("failed to canonicalize {}", root.display()))
}

fn validate_workspace_path(root: &Path, workspace: &Path) -> Result<()> {
    let canonical_root = root
        .canonicalize()
        .with_context(|| format!("failed to canonicalize {}", root.display()))?;

    let candidate = if workspace.exists() {
        workspace
            .canonicalize()
            .with_context(|| format!("failed to canonicalize {}", workspace.display()))?
    } else {
        workspace.to_path_buf()
    };

    if candidate == canonical_root {
        return Err(anyhow!(
            "workspace_equals_root: {}",
            canonical_root.display()
        ));
    }

    let root_prefix = format!("{}/", canonical_root.display());
    let candidate_display = candidate.display().to_string();
    if !candidate_display.starts_with(&root_prefix) {
        return Err(anyhow!(
            "workspace_outside_root: {} not under {}",
            candidate.display(),
            canonical_root.display()
        ));
    }

    Ok(())
}

async fn run_hook(
    settings: &Settings,
    hook_name: &str,
    script: &str,
    workspace: &Path,
    issue: &Issue,
) -> Result<()> {
    let cargo_home = workspace.join(".cargo-home");
    let mut command = Command::new("bash");
    command.arg("-lc").arg(script);
    command.current_dir(workspace);
    apply_runtime_tool_env(&mut command);
    command.env("CARGO_HOME", &cargo_home);
    command.env("ISSUE_ID", &issue.id);
    command.env("ISSUE_IDENTIFIER", &issue.identifier);
    command.env("ISSUE_TITLE", &issue.title);
    command.env("ISSUE_STATE", &issue.state);
    command.kill_on_drop(true);

    let output = timeout(
        Duration::from_millis(settings.hooks.timeout_ms),
        command.output(),
    )
    .await
    .map_err(|_| anyhow!("workspace_hook_timeout: {hook_name}"))?
    .with_context(|| format!("failed to launch hook {hook_name}"))?;

    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        Err(anyhow!(
            "workspace_hook_failed: {hook_name} status={} stdout={} stderr={}",
            output.status,
            stdout.trim(),
            stderr.trim()
        ))
    }
}

pub fn load_workspace_repo_workflow(workspace: &Path) -> Result<RepoWorkflow> {
    if !docker_mode_enabled() {
        return Ok(default_repo_workflow());
    }

    load_repo_workflow(&workspace.join(REPO_WORKFLOW_FILENAME))
}

fn docker_mode_enabled() -> bool {
    matches!(
        std::env::var("KAIRASTRA_DEPLOY_MODE").as_deref(),
        Ok("docker")
    )
}

fn runtime_tool_env_overrides() -> [(&'static str, String); 4] {
    [
        (
            "XDG_CACHE_HOME",
            runtime_tool_env_value("XDG_CACHE_HOME", DEFAULT_XDG_CACHE_HOME),
        ),
        (
            "COREPACK_HOME",
            runtime_tool_env_value("COREPACK_HOME", DEFAULT_COREPACK_HOME),
        ),
        (
            "PNPM_HOME",
            runtime_tool_env_value("PNPM_HOME", DEFAULT_PNPM_HOME),
        ),
        (
            "NPM_CONFIG_CACHE",
            runtime_tool_env_value("NPM_CONFIG_CACHE", DEFAULT_NPM_CONFIG_CACHE),
        ),
    ]
}

fn runtime_tool_env_value(name: &str, default: &str) -> String {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| default.to_string())
}

fn docker_support_dirs(settings: &Settings) -> Vec<&'static str> {
    let mut support_dirs = Vec::new();
    for provider in settings.providers.ids() {
        for dir in providers::repo_support_dirs(provider).unwrap_or(&[".github"]) {
            if !support_dirs.iter().any(|existing| existing == dir) {
                support_dirs.push(*dir);
            }
        }
    }

    if support_dirs.is_empty() {
        for dir in
            providers::repo_support_dirs(settings.agent.provider.as_str()).unwrap_or(&[".github"])
        {
            support_dirs.push(*dir);
        }
    }

    support_dirs
}

async fn run_internal_docker_after_create(
    settings: &Settings,
    workspace: &Path,
    issue: &Issue,
) -> Result<()> {
    let script = render_internal_docker_after_create_script(settings);
    run_hook(settings, "__docker_after_create", &script, workspace, issue).await
}

async fn run_internal_docker_before_run(
    settings: &Settings,
    workspace: &Path,
    issue: &Issue,
) -> Result<()> {
    let script = render_internal_docker_before_run_script(settings);
    run_hook(settings, "__docker_before_run", &script, workspace, issue).await
}

fn render_internal_docker_after_create_script(settings: &Settings) -> String {
    let support_dirs = docker_support_dirs(settings).join(" ");
    format!(
        r#"set -euo pipefail

clone_with_auth() {{
  clone_url="$1"
  if [ -n "${{GITHUB_TOKEN:-}}" ] && printf '%s' "$clone_url" | grep -q '^https://github.com/'; then
    auth_header="$(printf 'x-access-token:%s' "$GITHUB_TOKEN" | base64 | tr -d '\n')"
    git -c http.extraheader="Authorization: Basic ${{auth_header}}" clone --depth 1 "$clone_url" .
    git config http.https://github.com/.extraheader "Authorization: Basic ${{auth_header}}"
  else
    git clone --depth 1 "$clone_url" .
  fi
}}

overlay_seed_repo() {{
  seed_repo="$1"
  if command -v rsync >/dev/null 2>&1; then
    rsync -a --delete --exclude '.git' "${{seed_repo}}/" ./
  else
    echo "rsync is required when overlaying KAIRASTRA_SEED_REPO on top of a remote clone." >&2
    exit 1
  fi
}}

github_https_url() {{
  remote_url="$1"
  case "$remote_url" in
    git@github.com:*)
      printf 'https://github.com/%s\n' "${{remote_url#git@github.com:}}"
      ;;
    ssh://git@github.com/*)
      printf 'https://github.com/%s\n' "${{remote_url#ssh://git@github.com/}}"
      ;;
    *)
      printf '%s\n' "$remote_url"
      ;;
  esac
}}

configure_github_auth() {{
  if [ -z "${{GITHUB_TOKEN:-}}" ]; then
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
  git config http.https://github.com/.extraheader "Authorization: Basic ${{auth_header}}"
}}

restore_support_dir_from_seed() {{
  support_dir="$1"
  if [ -e "$support_dir" ]; then
    return 0
  fi
  if [ -n "${{KAIRASTRA_SEED_REPO:-}}" ] && [ -e "${{KAIRASTRA_SEED_REPO}}/$support_dir" ]; then
    cp -R "${{KAIRASTRA_SEED_REPO}}/$support_dir" "$support_dir"
  fi
}}

exclude_workspace_support_dir() {{
  support_dir="$1"
  exclude_path="$(git rev-parse --git-path info/exclude 2>/dev/null || true)"
  if [ -z "$exclude_path" ]; then
    return 0
  fi
  mkdir -p "$(dirname "$exclude_path")"
  touch "$exclude_path"
  entry="$support_dir/"
  if ! grep -Fqx "$entry" "$exclude_path" 2>/dev/null; then
    printf '%s\n' "$entry" >> "$exclude_path"
  fi
}}

prune_legacy_codex_workspace_support() {{
  if [ ! -e ".codex" ]; then
    return 0
  fi
  if git ls-files -- .codex 2>/dev/null | grep -q .; then
    return 0
  fi
  rm -rf .codex
}}

require_workspace_support_dirs() {{
  for support_dir in {support_dirs}; do
    restore_support_dir_from_seed "$support_dir"
    if [ ! -e "$support_dir" ]; then
      echo "Workspace bootstrap missing required repository support directory: $support_dir" >&2
      exit 1
    fi
    exclude_workspace_support_dir "$support_dir"
  done
}}

adopt_seed_repo_origin() {{
  if [ -z "${{KAIRASTRA_SEED_REPO:-}}" ] || [ ! -d "$KAIRASTRA_SEED_REPO/.git" ]; then
    return 0
  fi
  source_remote="$(git -C "$KAIRASTRA_SEED_REPO" config --get remote.origin.url || true)"
  current_remote="$(git config --get remote.origin.url || true)"
  if [ -n "$source_remote" ] && {{ [ "$current_remote" = "$KAIRASTRA_SEED_REPO" ] || [ -z "$current_remote" ]; }}; then
    git remote set-url origin "$source_remote"
  fi
}}

resolve_default_branch() {{
  if [ -n "${{KAIRASTRA_GIT_DEFAULT_BRANCH:-}}" ]; then
    printf '%s\n' "${{KAIRASTRA_GIT_DEFAULT_BRANCH}}"
    return 0
  fi

  remote_head="$(git symbolic-ref --quiet --short refs/remotes/origin/HEAD 2>/dev/null || true)"
  if [ -n "$remote_head" ]; then
    printf '%s\n' "${{remote_head#origin/}}"
    return 0
  fi

  remote_head="$(git remote show origin 2>/dev/null | sed -n 's/.*HEAD branch: //p' | head -n 1)"
  if [ -n "$remote_head" ]; then
    printf '%s\n' "$remote_head"
    return 0
  fi

  printf 'main\n'
}}

fetch_origin_branch() {{
  branch_name="$1"
  if [ -z "$branch_name" ] || [ "$branch_name" = "HEAD" ]; then
    return 0
  fi
  git fetch --quiet origin "refs/heads/$branch_name:refs/remotes/origin/$branch_name" || true
}}

ensure_default_branch_baseline() {{
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
}}

if [ -n "${{KAIRASTRA_GIT_CLONE_URL:-}}" ]; then
  clone_with_auth "$KAIRASTRA_GIT_CLONE_URL"
  if [ -n "${{KAIRASTRA_SEED_REPO:-}}" ] && [ -d "$KAIRASTRA_SEED_REPO" ]; then
    overlay_seed_repo "$KAIRASTRA_SEED_REPO"
  fi
elif [ -n "${{KAIRASTRA_SEED_REPO:-}}" ] && [ -d "$KAIRASTRA_SEED_REPO/.git" ]; then
  git clone "$KAIRASTRA_SEED_REPO" .
  adopt_seed_repo_origin
else
  echo "Set KAIRASTRA_GIT_CLONE_URL, or point KAIRASTRA_SEED_REPO at a git checkout, before running Kairastra." >&2
  exit 1
fi

if [ -n "${{KAIRASTRA_GIT_PUSH_URL:-}}" ]; then
  git remote set-url --push origin "$KAIRASTRA_GIT_PUSH_URL"
fi

prune_legacy_codex_workspace_support
require_workspace_support_dirs
configure_github_auth
ensure_default_branch_baseline

git config user.name "${{KAIRASTRA_GIT_AUTHOR_NAME:-Kairastra}}"
git config user.email "${{KAIRASTRA_GIT_AUTHOR_EMAIL:-kairastra@users.noreply.github.com}}"
"#,
        support_dirs = support_dirs
    )
}

fn render_internal_docker_before_run_script(settings: &Settings) -> String {
    let support_dirs = docker_support_dirs(settings).join(" ");
    format!(
        r#"set -euo pipefail

git config --global --add safe.directory "$(pwd)"

restore_support_dir_from_seed() {{
  support_dir="$1"
  if [ -e "$support_dir" ]; then
    return 0
  fi
  if [ -n "${{KAIRASTRA_SEED_REPO:-}}" ] && [ -e "${{KAIRASTRA_SEED_REPO}}/$support_dir" ]; then
    cp -R "${{KAIRASTRA_SEED_REPO}}/$support_dir" "$support_dir"
  fi
}}

exclude_workspace_support_dir() {{
  support_dir="$1"
  exclude_path="$(git rev-parse --git-path info/exclude 2>/dev/null || true)"
  if [ -z "$exclude_path" ]; then
    return 0
  fi
  mkdir -p "$(dirname "$exclude_path")"
  touch "$exclude_path"
  entry="$support_dir/"
  if ! grep -Fqx "$entry" "$exclude_path" 2>/dev/null; then
    printf '%s\n' "$entry" >> "$exclude_path"
  fi
}}

prune_legacy_codex_workspace_support() {{
  if [ ! -e ".codex" ]; then
    return 0
  fi
  if git ls-files -- .codex 2>/dev/null | grep -q .; then
    return 0
  fi
  rm -rf .codex
}}

require_workspace_support_dirs() {{
  for support_dir in {support_dirs}; do
    restore_support_dir_from_seed "$support_dir"
    if [ ! -e "$support_dir" ]; then
      echo "Workspace bootstrap missing required repository support directory: $support_dir" >&2
      exit 1
    fi
    exclude_workspace_support_dir "$support_dir"
  done
}}

github_https_url() {{
  remote_url="$1"
  case "$remote_url" in
    git@github.com:*)
      printf 'https://github.com/%s\n' "${{remote_url#git@github.com:}}"
      ;;
    ssh://git@github.com/*)
      printf 'https://github.com/%s\n' "${{remote_url#ssh://git@github.com/}}"
      ;;
    *)
      printf '%s\n' "$remote_url"
      ;;
  esac
}}

configure_github_auth() {{
  if [ -z "${{GITHUB_TOKEN:-}}" ]; then
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
  git config http.https://github.com/.extraheader "Authorization: Basic ${{auth_header}}"
}}

adopt_seed_repo_origin() {{
  if [ -z "${{KAIRASTRA_SEED_REPO:-}}" ] || [ ! -d "$KAIRASTRA_SEED_REPO/.git" ]; then
    return 0
  fi
  source_remote="$(git -C "$KAIRASTRA_SEED_REPO" config --get remote.origin.url || true)"
  current_remote="$(git config --get remote.origin.url || true)"
  if [ -n "$source_remote" ] && {{ [ "$current_remote" = "$KAIRASTRA_SEED_REPO" ] || [ -z "$current_remote" ]; }}; then
    git remote set-url origin "$source_remote"
  fi
}}

resolve_default_branch() {{
  if [ -n "${{KAIRASTRA_GIT_DEFAULT_BRANCH:-}}" ]; then
    printf '%s\n' "${{KAIRASTRA_GIT_DEFAULT_BRANCH}}"
    return 0
  fi

  remote_head="$(git symbolic-ref --quiet --short refs/remotes/origin/HEAD 2>/dev/null || true)"
  if [ -n "$remote_head" ]; then
    printf '%s\n' "${{remote_head#origin/}}"
    return 0
  fi

  remote_head="$(git remote show origin 2>/dev/null | sed -n 's/.*HEAD branch: //p' | head -n 1)"
  if [ -n "$remote_head" ]; then
    printf '%s\n' "$remote_head"
    return 0
  fi

  printf 'main\n'
}}

fetch_origin_branch() {{
  branch_name="$1"
  if [ -z "$branch_name" ] || [ "$branch_name" = "HEAD" ]; then
    return 0
  fi
  git fetch --quiet origin "refs/heads/$branch_name:refs/remotes/origin/$branch_name" || true
}}

ensure_default_branch_baseline() {{
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
}}

prune_legacy_codex_workspace_support
require_workspace_support_dirs
adopt_seed_repo_origin

if [ -n "${{KAIRASTRA_GIT_PUSH_URL:-}}" ]; then
  git remote set-url --push origin "$KAIRASTRA_GIT_PUSH_URL"
fi

configure_github_auth
ensure_default_branch_baseline

git config user.name "${{KAIRASTRA_GIT_AUTHOR_NAME:-Kairastra}}"
git config user.email "${{KAIRASTRA_GIT_AUTHOR_EMAIL:-kairastra@users.noreply.github.com}}"
"#,
        support_dirs = support_dirs
    )
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use tempfile::tempdir;
    use tokio::sync::Mutex;

    use crate::config::Settings;
    use crate::model::{Issue, WorkflowDefinition};

    use super::{
        ensure_workspace, load_workspace_repo_workflow, remove_issue_workspace,
        render_internal_docker_after_create_script, render_internal_docker_before_run_script,
        run_after_run_hook, run_hook, sanitize_workspace_key,
    };

    static ENV_LOCK: Mutex<()> = Mutex::const_new(());

    fn test_settings(root: &Path) -> Settings {
        let definition = WorkflowDefinition {
            config: serde_yaml::from_str(&format!(
                r#"
tracker:
  kind: github
  owner: openai
  project_v2_number: 7
  api_key: fake
agent:
  provider: codex
providers:
  codex: {{}}
workspace:
  root: {}
"#,
                root.display()
            ))
            .unwrap(),
            prompt_template: String::new(),
        };
        Settings::from_workflow(&definition).unwrap()
    }

    fn issue(identifier: &str) -> Issue {
        Issue {
            id: identifier.to_string(),
            project_item_id: None,
            identifier: identifier.to_string(),
            title: "Title".to_string(),
            description: None,
            priority: None,
            state: "Todo".to_string(),
            branch_name: None,
            url: None,
            assignees: Vec::new(),
            labels: Vec::new(),
            blocked_by: Vec::new(),
            created_at: None,
            updated_at: None,
            workpad_comment_id: None,
            workpad_comment_url: None,
            workpad_comment_body: None,
        }
    }

    #[tokio::test]
    async fn workspace_paths_are_sanitized_and_reused() {
        let _guard = ENV_LOCK.lock().await;
        std::env::remove_var("KAIRASTRA_DEPLOY_MODE");
        let dir = tempdir().unwrap();
        let settings = test_settings(dir.path());

        let first = ensure_workspace(&settings, &issue("MT/Det")).await.unwrap();
        fs::write(first.path.join("local.txt"), "progress").unwrap();
        let second = ensure_workspace(&settings, &issue("MT/Det")).await.unwrap();

        assert_eq!(first.path, second.path);
        assert_eq!(sanitize_workspace_key("MT/Det"), "MT_Det");
        assert_eq!(
            fs::read_to_string(second.path.join("local.txt")).unwrap(),
            "progress"
        );
    }

    #[tokio::test]
    async fn removes_target_issue_workspace_only() {
        let _guard = ENV_LOCK.lock().await;
        std::env::remove_var("KAIRASTRA_DEPLOY_MODE");
        let dir = tempdir().unwrap();
        let settings = test_settings(dir.path());
        let target = ensure_workspace(&settings, &issue("MT-1")).await.unwrap();
        let keep = ensure_workspace(&settings, &issue("MT-2")).await.unwrap();

        remove_issue_workspace(&settings, "MT-1").await.unwrap();

        assert!(!target.path.exists());
        assert!(keep.path.exists());
    }

    #[tokio::test]
    async fn hook_commands_get_workspace_local_cargo_home() {
        let _guard = ENV_LOCK.lock().await;
        std::env::remove_var("KAIRASTRA_DEPLOY_MODE");
        let dir = tempdir().unwrap();
        let settings = test_settings(dir.path());
        let workspace = dir.path().join("workspace");
        fs::create_dir_all(&workspace).unwrap();

        run_hook(
            &settings,
            "after_create",
            "printf '%s' \"$CARGO_HOME\" > cargo-home-path.txt",
            &workspace,
            &issue("MT-1"),
        )
        .await
        .unwrap();

        assert_eq!(
            fs::read_to_string(workspace.join("cargo-home-path.txt")).unwrap(),
            workspace.join(".cargo-home").display().to_string()
        );
    }

    #[tokio::test]
    async fn hook_commands_get_runtime_tool_cache_env_defaults() {
        let _guard = ENV_LOCK.lock().await;
        std::env::remove_var("KAIRASTRA_DEPLOY_MODE");
        std::env::remove_var("XDG_CACHE_HOME");
        std::env::remove_var("COREPACK_HOME");
        std::env::remove_var("PNPM_HOME");
        std::env::remove_var("NPM_CONFIG_CACHE");
        let dir = tempdir().unwrap();
        let settings = test_settings(dir.path());
        let workspace = dir.path().join("workspace");
        fs::create_dir_all(&workspace).unwrap();

        run_hook(
            &settings,
            "after_create",
            "printf '%s\\n%s\\n%s\\n%s' \"$XDG_CACHE_HOME\" \"$COREPACK_HOME\" \"$PNPM_HOME\" \"$NPM_CONFIG_CACHE\" > runtime-env.txt",
            &workspace,
            &issue("MT-ENV"),
        )
        .await
        .unwrap();

        assert_eq!(
            fs::read_to_string(workspace.join("runtime-env.txt")).unwrap(),
            "/tmp/kairastra/xdg-cache\n/tmp/kairastra/corepack\n/tmp/kairastra/pnpm\n/tmp/kairastra/npm-cache"
        );
    }

    #[tokio::test]
    async fn after_run_hook_errors_are_returned() {
        let _guard = ENV_LOCK.lock().await;
        std::env::remove_var("KAIRASTRA_DEPLOY_MODE");
        let dir = tempdir().unwrap();
        let mut settings = test_settings(dir.path());
        settings.hooks.after_run = Some("exit 7".to_string());
        let workspace = dir.path().join("workspace");
        fs::create_dir_all(&workspace).unwrap();

        let error = run_after_run_hook(&settings, &workspace, &issue("MT-1"))
            .await
            .unwrap_err()
            .to_string();

        assert!(error.contains("workspace_hook_failed: after_run"));
    }

    #[test]
    fn docker_repo_workflow_defaults_when_missing() {
        let _guard = ENV_LOCK.blocking_lock();
        std::env::set_var("KAIRASTRA_DEPLOY_MODE", "docker");
        let dir = tempdir().unwrap();

        let workflow = load_workspace_repo_workflow(dir.path()).unwrap();
        assert_eq!(workflow.definition.prompt_template, "");
        assert!(workflow.hooks.after_create.is_none());

        std::env::remove_var("KAIRASTRA_DEPLOY_MODE");
    }

    #[test]
    fn docker_repo_workflow_loads_repo_root_workflow() {
        let _guard = ENV_LOCK.blocking_lock();
        std::env::set_var("KAIRASTRA_DEPLOY_MODE", "docker");
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("WORKFLOW.md"),
            r#"---
hooks:
  before_run: echo repo
---
Repo prompt
"#,
        )
        .unwrap();

        let workflow = load_workspace_repo_workflow(dir.path()).unwrap();
        assert_eq!(workflow.definition.prompt_template, "Repo prompt");
        assert_eq!(workflow.hooks.before_run.as_deref(), Some("echo repo"));

        std::env::remove_var("KAIRASTRA_DEPLOY_MODE");
    }

    #[test]
    fn docker_workspace_scripts_exclude_seeded_support_dirs_from_git_status() {
        let dir = tempdir().unwrap();
        let settings = test_settings(dir.path());

        let after_create = render_internal_docker_after_create_script(&settings);
        let before_run = render_internal_docker_before_run_script(&settings);

        assert!(after_create.contains("git rev-parse --git-path info/exclude"));
        assert!(after_create.contains("entry=\"$support_dir/\""));
        assert!(after_create.contains("prune_legacy_codex_workspace_support"));
        assert!(after_create.contains("git ls-files -- .codex"));
        assert!(!after_create.contains("for support_dir in .codex .github; do"));
        assert!(after_create.contains("resolve_default_branch()"));
        assert!(after_create.contains("fetch_origin_branch()"));
        assert!(after_create.contains("fetch_origin_branch \"$default_branch\""));
        assert!(after_create.contains("fetch_origin_branch \"$current_branch\""));
        assert!(after_create.contains("git fetch --quiet --unshallow origin \\"));
        assert!(before_run.contains("git rev-parse --git-path info/exclude"));
        assert!(before_run.contains("entry=\"$support_dir/\""));
        assert!(before_run.contains("prune_legacy_codex_workspace_support"));
        assert!(before_run.contains("git ls-files -- .codex"));
        assert!(!before_run.contains("for support_dir in .codex .github; do"));
        assert!(before_run.contains("resolve_default_branch()"));
        assert!(before_run.contains("fetch_origin_branch()"));
        assert!(before_run.contains("git fetch --quiet --unshallow origin \\"));
        assert!(before_run.contains("ensure_default_branch_baseline"));
    }
}
