use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use tokio::process::Command;
use tokio::task::spawn_blocking;
use tokio::time::{timeout, Duration};

use crate::config::Settings;
use crate::git_checkout::checkout_git_common_dir;
use crate::model::{Issue, PendingHumanInput};
use crate::workflow::{default_repo_workflow, RepoWorkflow};

const DEFAULT_XDG_CACHE_HOME: &str = "/tmp/kairastra/xdg-cache";
const DEFAULT_COREPACK_HOME: &str = "/tmp/kairastra/corepack";
const DEFAULT_PNPM_HOME: &str = "/tmp/kairastra/pnpm";
const DEFAULT_NPM_CONFIG_CACHE: &str = "/tmp/kairastra/npm-cache";
const KAIRASTRA_STATE_DIR: &str = ".kairastra";
const PENDING_HUMAN_INPUT_FILE: &str = "pending-input.json";

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

    if sanitized.is_empty() || sanitized == "." || sanitized == ".." {
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

pub fn pending_human_input_path(workspace: &Path) -> PathBuf {
    workspace
        .join(KAIRASTRA_STATE_DIR)
        .join(PENDING_HUMAN_INPUT_FILE)
}

pub fn load_pending_human_input(workspace: &Path) -> Result<Option<PendingHumanInput>> {
    let path = pending_human_input_path(workspace);
    if !path.exists() {
        return Ok(None);
    }

    let content =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let pending = serde_json::from_str(&content)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    Ok(Some(pending))
}

pub fn write_pending_human_input(workspace: &Path, pending: &PendingHumanInput) -> Result<()> {
    let path = pending_human_input_path(workspace);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let content = serde_json::to_string_pretty(pending)
        .context("failed to encode pending human input state")?;
    fs::write(&path, content).with_context(|| format!("failed to write {}", path.display()))
}

pub fn clear_pending_human_input(workspace: &Path) -> Result<()> {
    let path = pending_human_input_path(workspace);
    if !path.exists() {
        return Ok(());
    }
    fs::remove_file(&path).with_context(|| format!("failed to remove {}", path.display()))
}

pub async fn ensure_workspace(settings: &Settings, issue: &Issue) -> Result<Workspace> {
    let root = ensure_workspace_root(&settings.workspace.root)?;
    let workspace_key = sanitize_workspace_key(&issue.identifier);
    let path = root.join(&workspace_key);
    validate_workspace_path(&root, &path)?;

    let created_now = if path.exists() {
        if path.is_dir() {
            if workspace_requires_recreate(settings, &path).await? {
                fs::remove_dir_all(&path)
                    .with_context(|| format!("failed to reset {}", path.display()))?;
                fs::create_dir_all(&path)?;
                true
            } else {
                false
            }
        } else {
            fs::remove_file(&path).or_else(|_| fs::remove_dir_all(&path))?;
            fs::create_dir_all(&path)?;
            true
        }
    } else {
        fs::create_dir_all(&path)?;
        true
    };

    let workspace = Workspace {
        path,
        workspace_key,
        created_now,
    };

    if workspace.created_now {
        if let Some(script) = settings.hooks.after_create.as_deref() {
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
    if let Some(script) = settings.hooks.before_run.as_deref() {
        run_hook(settings, "before_run", script, workspace, issue).await?;
    }
    Ok(())
}

pub async fn run_after_run_hook(
    settings: &Settings,
    workspace: &Path,
    issue: &Issue,
) -> Result<()> {
    if let Some(script) = settings.hooks.after_run.as_deref() {
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

    if let Some(script) = settings.hooks.before_remove.as_deref() {
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

    if settings.uses_seed_worktree_bootstrap() {
        if let Some(seed_repo) = configured_seed_repo() {
            match remove_managed_git_worktree(&seed_repo, &workspace, identifier).await {
                Ok(()) => return Ok(()),
                Err(error) if managed_git_worktree_safety_error(&error) => return Err(error),
                Err(error) if managed_git_worktree_probe_error(&error) => {}
                Err(error) => return Err(error),
            }
        }
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
        let parent = workspace
            .parent()
            .ok_or_else(|| anyhow!("workspace_invalid_path: {}", workspace.display()))?;
        let canonical_parent = parent
            .canonicalize()
            .with_context(|| format!("failed to canonicalize {}", parent.display()))?;
        let file_name = workspace
            .file_name()
            .ok_or_else(|| anyhow!("workspace_invalid_path: {}", workspace.display()))?;
        canonical_parent.join(file_name)
    };

    if candidate == canonical_root {
        return Err(anyhow!(
            "workspace_equals_root: {}",
            canonical_root.display()
        ));
    }

    if !candidate.starts_with(&canonical_root) {
        return Err(anyhow!(
            "workspace_outside_root: {} not under {}",
            candidate.display(),
            canonical_root.display()
        ));
    }

    Ok(())
}

fn configured_seed_repo() -> Option<PathBuf> {
    std::env::var("KAIRASTRA_SEED_REPO")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

async fn workspace_requires_recreate(settings: &Settings, workspace: &Path) -> Result<bool> {
    if !settings.uses_seed_worktree_bootstrap() {
        return Ok(false);
    }

    let Some(seed_repo) = configured_seed_repo() else {
        return Ok(false);
    };

    if !workspace.join(".git").exists() {
        return Ok(true);
    }

    let output = Command::new("git")
        .args(["rev-parse", "--verify", "HEAD"])
        .current_dir(workspace)
        .output()
        .await
        .with_context(|| format!("failed to validate {}", workspace.display()))?;
    if !output.status.success() {
        return Ok(true);
    }

    let expected_common_dir = seed_repo_git_common_dir(seed_repo).await?;
    let Some(expected_common_dir) = expected_common_dir else {
        return Ok(false);
    };
    let workspace_common_dir = workspace_git_common_dir(workspace).await?;

    Ok(workspace_common_dir
        .map(|common_dir| common_dir != expected_common_dir)
        .unwrap_or(true))
}

async fn workspace_git_common_dir(workspace: &Path) -> Result<Option<PathBuf>> {
    let workspace = workspace.to_path_buf();
    spawn_blocking(move || checkout_git_common_dir(&workspace))
        .await
        .context("workspace git common-dir task failed")?
}

async fn seed_repo_git_common_dir(seed_repo: PathBuf) -> Result<Option<PathBuf>> {
    spawn_blocking(move || checkout_git_common_dir(&seed_repo))
        .await
        .context("seed repo git common-dir task failed")?
}

fn managed_issue_branch_name(identifier: &str) -> String {
    format!("kairastra/{}", sanitize_workspace_key(identifier))
}

async fn remove_managed_git_worktree(
    seed_repo: &Path,
    workspace: &Path,
    identifier: &str,
) -> Result<()> {
    let seed_repo = seed_repo.to_path_buf();
    let Some(seed_common_dir) = seed_repo_git_common_dir(seed_repo.clone()).await? else {
        return Err(anyhow!("seed repo is not a git checkout"));
    };
    if workspace_git_common_dir(workspace).await? != Some(seed_common_dir) {
        return Err(anyhow!("workspace is not a managed seed worktree"));
    }

    ensure_managed_worktree_clean(workspace).await?;
    ensure_managed_branch_merged(&seed_repo, identifier).await?;

    let remove_output = Command::new("git")
        .arg("-C")
        .arg(&seed_repo)
        .args(["worktree", "remove", "--force"])
        .arg(workspace)
        .output()
        .await
        .context("failed to launch git worktree remove")?;

    if !remove_output.status.success() {
        return Err(anyhow!(
            "git worktree remove failed: {}",
            String::from_utf8_lossy(&remove_output.stderr).trim()
        ));
    }

    let branch_name = managed_issue_branch_name(identifier);
    let _ = Command::new("git")
        .arg("-C")
        .arg(&seed_repo)
        .args(["branch", "--delete", "--force", branch_name.as_str()])
        .output()
        .await;

    Ok(())
}

fn managed_git_worktree_safety_error(error: &anyhow::Error) -> bool {
    let message = error.to_string();
    message.contains("managed_worktree_dirty")
        || message.contains("managed_worktree_unmerged_branch")
}

fn managed_git_worktree_probe_error(error: &anyhow::Error) -> bool {
    let message = error.to_string();
    message.contains("workspace is not a managed seed worktree")
        || message.contains("seed repo is not a git checkout")
}

async fn ensure_managed_worktree_clean(workspace: &Path) -> Result<()> {
    let status_output = Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(["status", "--porcelain", "--untracked-files=all"])
        .output()
        .await
        .context("failed to inspect managed worktree status")?;
    if !status_output.status.success() {
        return Err(anyhow!(
            "managed_worktree_status_failed: {}",
            String::from_utf8_lossy(&status_output.stderr).trim()
        ));
    }
    let has_user_changes = String::from_utf8_lossy(&status_output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .any(|line| !managed_worktree_status_line_is_kairastra_state(line));
    if has_user_changes {
        return Err(anyhow!("managed_worktree_dirty: {}", workspace.display()));
    }
    Ok(())
}

fn managed_worktree_status_line_is_kairastra_state(line: &str) -> bool {
    let path = line.get(3..).unwrap_or(line).trim();
    let path = path.split(" -> ").last().unwrap_or(path).trim();
    path == ".kairastra/pending-input.json"
}

async fn ensure_managed_branch_merged(seed_repo: &Path, identifier: &str) -> Result<()> {
    let branch_name = managed_issue_branch_name(identifier);
    let branch_exists = Command::new("git")
        .arg("-C")
        .arg(seed_repo)
        .args(["show-ref", "--verify", "--quiet"])
        .arg(format!("refs/heads/{branch_name}"))
        .status()
        .await
        .context("failed to inspect managed branch")?;
    if !branch_exists.success() {
        return Ok(());
    }

    let merge_base = Command::new("git")
        .arg("-C")
        .arg(seed_repo)
        .args(["merge-base", "--is-ancestor", branch_name.as_str(), "HEAD"])
        .status()
        .await
        .context("failed to inspect managed branch ancestry")?;
    if !merge_base.success() {
        return Err(anyhow!("managed_worktree_unmerged_branch: {}", branch_name));
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
    let _ = workspace;
    Ok(default_repo_workflow())
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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command as StdCommand;

    use tempfile::tempdir;
    use tokio::sync::Mutex;

    use crate::config::{Settings, WorkspaceBootstrapMode};
    use crate::model::{
        HumanInputQuestion, HumanInputRequest, Issue, PendingHumanInput, PendingHumanInputStatus,
        WorkflowDefinition,
    };

    use super::{
        clear_pending_human_input, ensure_workspace, load_pending_human_input,
        load_workspace_repo_workflow, managed_git_worktree_probe_error,
        managed_git_worktree_safety_error, managed_worktree_status_line_is_kairastra_state,
        remove_issue_workspace, run_after_run_hook, run_hook, sanitize_workspace_key,
        write_pending_human_input,
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

    fn init_git_repo(path: &Path) {
        let status = StdCommand::new("git")
            .args(["init", "-q"])
            .current_dir(path)
            .status()
            .unwrap();
        assert!(status.success());
        fs::write(path.join("README.md"), "seed\n").unwrap();
        let status = StdCommand::new("git")
            .args(["add", "README.md"])
            .current_dir(path)
            .status()
            .unwrap();
        assert!(status.success());
        let status = StdCommand::new("git")
            .args([
                "-c",
                "user.name=Kairastra",
                "-c",
                "user.email=kairastra@example.com",
                "commit",
                "-q",
                "-m",
                "init",
            ])
            .current_dir(path)
            .status()
            .unwrap();
        assert!(status.success());
        let status = StdCommand::new("git")
            .args([
                "remote",
                "add",
                "origin",
                "https://github.com/example/repo.git",
            ])
            .current_dir(path)
            .status()
            .unwrap();
        assert!(status.success());
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
    async fn persists_pending_human_input_state() {
        let _guard = ENV_LOCK.lock().await;
        std::env::remove_var("KAIRASTRA_DEPLOY_MODE");
        let dir = tempdir().unwrap();
        let settings = test_settings(dir.path());
        let workspace = ensure_workspace(&settings, &issue("INPUT-1"))
            .await
            .unwrap();

        let pending = PendingHumanInput {
            request: HumanInputRequest {
                request_id: "input-123".to_string(),
                provider: "codex".to_string(),
                issue_id: "INPUT-1".to_string(),
                issue_identifier: "INPUT-1".to_string(),
                session_id: Some("thread-turn".to_string()),
                requested_at: chrono::Utc::now(),
                questions: vec![HumanInputQuestion {
                    id: "scope".to_string(),
                    header: Some("Scope".to_string()),
                    question: "Pick a scope".to_string(),
                    options: Vec::new(),
                }],
            },
            status: PendingHumanInputStatus::AwaitingAnswer,
            prompt_comment_id: Some(42),
            prompt_comment_url: Some("https://example.com/comment".to_string()),
            answered_comment_id: None,
            answered_by_login: None,
            answered_at: None,
            answers: Vec::new(),
        };

        write_pending_human_input(&workspace.path, &pending).unwrap();
        let loaded = load_pending_human_input(&workspace.path)
            .unwrap()
            .expect("pending state should exist");
        assert_eq!(loaded.request.request_id, "input-123");
        assert_eq!(loaded.prompt_comment_id, Some(42));

        clear_pending_human_input(&workspace.path).unwrap();
        assert!(load_pending_human_input(&workspace.path).unwrap().is_none());
    }

    #[test]
    fn managed_worktree_error_classification_controls_cleanup_fallback() {
        assert!(managed_git_worktree_probe_error(&anyhow::anyhow!(
            "workspace is not a managed seed worktree"
        )));
        assert!(managed_git_worktree_probe_error(&anyhow::anyhow!(
            "seed repo is not a git checkout"
        )));
        assert!(managed_git_worktree_safety_error(&anyhow::anyhow!(
            "managed_worktree_dirty: /tmp/workspace"
        )));
        assert!(!managed_git_worktree_probe_error(&anyhow::anyhow!(
            "git worktree remove failed: locked"
        )));
        assert!(!managed_git_worktree_safety_error(&anyhow::anyhow!(
            "git worktree remove failed: locked"
        )));
    }

    #[test]
    fn managed_worktree_cleanliness_ignores_pending_input_state_file() {
        assert!(managed_worktree_status_line_is_kairastra_state(
            "?? .kairastra/pending-input.json"
        ));
        assert!(!managed_worktree_status_line_is_kairastra_state(
            "?? .kairastra/other.json"
        ));
        assert!(!managed_worktree_status_line_is_kairastra_state(
            "?? src/lib.rs"
        ));
    }

    #[tokio::test]
    async fn removes_target_issue_workspace_only() {
        let _guard = ENV_LOCK.lock().await;
        std::env::remove_var("KAIRASTRA_DEPLOY_MODE");
        std::env::remove_var("KAIRASTRA_SEED_REPO");
        let dir = tempdir().unwrap();
        let settings = test_settings(dir.path());
        let target = ensure_workspace(&settings, &issue("MT-1")).await.unwrap();
        let keep = ensure_workspace(&settings, &issue("MT-2")).await.unwrap();

        remove_issue_workspace(&settings, "MT-1").await.unwrap();

        assert!(!target.path.exists());
        assert!(keep.path.exists());
    }

    #[tokio::test]
    async fn removes_managed_git_worktree_and_branch() {
        let _guard = ENV_LOCK.lock().await;
        let workspace_root = tempdir().unwrap();
        let seed_repo = tempdir().unwrap();
        init_git_repo(seed_repo.path());
        std::env::set_var("KAIRASTRA_SEED_REPO", seed_repo.path());

        let mut settings = test_settings(workspace_root.path());
        settings.workspace.bootstrap_mode = WorkspaceBootstrapMode::SeedWorktree;
        settings.hooks.after_create = Some(
            r#"
set -euo pipefail
git -C "$KAIRASTRA_SEED_REPO" worktree add --force -b "kairastra/MT-1" "$PWD" HEAD
"#
            .trim()
            .to_string(),
        );

        let workspace = ensure_workspace(&settings, &issue("MT-1")).await.unwrap();
        assert!(workspace.path.exists());

        remove_issue_workspace(&settings, "MT-1").await.unwrap();

        assert!(!workspace.path.exists());
        let branch_output = StdCommand::new("git")
            .args(["branch", "--list", "kairastra/MT-1"])
            .current_dir(seed_repo.path())
            .output()
            .unwrap();
        assert!(String::from_utf8_lossy(&branch_output.stdout)
            .trim()
            .is_empty());
        std::env::remove_var("KAIRASTRA_SEED_REPO");
    }

    #[tokio::test]
    async fn refuses_to_remove_dirty_managed_git_worktree() {
        let _guard = ENV_LOCK.lock().await;
        let workspace_root = tempdir().unwrap();
        let seed_repo = tempdir().unwrap();
        init_git_repo(seed_repo.path());
        std::env::set_var("KAIRASTRA_SEED_REPO", seed_repo.path());

        let mut settings = test_settings(workspace_root.path());
        settings.workspace.bootstrap_mode = WorkspaceBootstrapMode::SeedWorktree;
        settings.hooks.after_create = Some(
            r#"
set -euo pipefail
git -C "$KAIRASTRA_SEED_REPO" worktree add --force -b "kairastra/MT-6" "$PWD" HEAD
"#
            .trim()
            .to_string(),
        );

        let workspace = ensure_workspace(&settings, &issue("MT-6")).await.unwrap();
        fs::write(workspace.path.join("dirty.txt"), "local change\n").unwrap();

        let error = remove_issue_workspace(&settings, "MT-6")
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("managed_worktree_dirty"));
        assert!(workspace.path.exists());

        let branch_output = StdCommand::new("git")
            .args(["branch", "--list", "kairastra/MT-6"])
            .current_dir(seed_repo.path())
            .output()
            .unwrap();
        assert!(String::from_utf8_lossy(&branch_output.stdout).contains("kairastra/MT-6"));
        std::env::remove_var("KAIRASTRA_SEED_REPO");
    }

    #[tokio::test]
    async fn removes_managed_git_worktree_with_only_pending_input_state() {
        let _guard = ENV_LOCK.lock().await;
        let workspace_root = tempdir().unwrap();
        let seed_repo = tempdir().unwrap();
        init_git_repo(seed_repo.path());
        std::env::set_var("KAIRASTRA_SEED_REPO", seed_repo.path());

        let mut settings = test_settings(workspace_root.path());
        settings.workspace.bootstrap_mode = WorkspaceBootstrapMode::SeedWorktree;
        settings.hooks.after_create = Some(
            r#"
set -euo pipefail
git -C "$KAIRASTRA_SEED_REPO" worktree add --force -b "kairastra/MT-8" "$PWD" HEAD
"#
            .trim()
            .to_string(),
        );

        let workspace = ensure_workspace(&settings, &issue("MT-8")).await.unwrap();
        let pending = PendingHumanInput {
            request: HumanInputRequest {
                request_id: "input-123".to_string(),
                provider: "codex".to_string(),
                issue_id: "MT-8".to_string(),
                issue_identifier: "MT-8".to_string(),
                session_id: Some("thread-turn".to_string()),
                requested_at: chrono::Utc::now(),
                questions: vec![HumanInputQuestion {
                    id: "scope".to_string(),
                    header: Some("Scope".to_string()),
                    question: "Pick a scope".to_string(),
                    options: Vec::new(),
                }],
            },
            status: PendingHumanInputStatus::AwaitingAnswer,
            prompt_comment_id: Some(42),
            prompt_comment_url: Some("https://example.com/comment".to_string()),
            answered_comment_id: None,
            answered_by_login: None,
            answered_at: None,
            answers: Vec::new(),
        };
        write_pending_human_input(&workspace.path, &pending).unwrap();

        remove_issue_workspace(&settings, "MT-8").await.unwrap();

        assert!(!workspace.path.exists());
        let branch_output = StdCommand::new("git")
            .args(["branch", "--list", "kairastra/MT-8"])
            .current_dir(seed_repo.path())
            .output()
            .unwrap();
        assert!(String::from_utf8_lossy(&branch_output.stdout)
            .trim()
            .is_empty());
        std::env::remove_var("KAIRASTRA_SEED_REPO");
    }

    #[tokio::test]
    async fn refuses_to_remove_unmerged_managed_git_worktree_branch() {
        let _guard = ENV_LOCK.lock().await;
        let workspace_root = tempdir().unwrap();
        let seed_repo = tempdir().unwrap();
        init_git_repo(seed_repo.path());
        std::env::set_var("KAIRASTRA_SEED_REPO", seed_repo.path());

        let mut settings = test_settings(workspace_root.path());
        settings.workspace.bootstrap_mode = WorkspaceBootstrapMode::SeedWorktree;
        settings.hooks.after_create = Some(
            r#"
set -euo pipefail
git -C "$KAIRASTRA_SEED_REPO" worktree add --force -b "kairastra/MT-7" "$PWD" HEAD
"#
            .trim()
            .to_string(),
        );

        let workspace = ensure_workspace(&settings, &issue("MT-7")).await.unwrap();
        fs::write(workspace.path.join("change.txt"), "committed change\n").unwrap();
        let status = StdCommand::new("git")
            .args(["add", "change.txt"])
            .current_dir(&workspace.path)
            .status()
            .unwrap();
        assert!(status.success());
        let status = StdCommand::new("git")
            .args([
                "-c",
                "user.name=Kairastra",
                "-c",
                "user.email=kairastra@example.com",
                "commit",
                "-q",
                "-m",
                "local change",
            ])
            .current_dir(&workspace.path)
            .status()
            .unwrap();
        assert!(status.success());

        let error = remove_issue_workspace(&settings, "MT-7")
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("managed_worktree_unmerged_branch"));
        assert!(workspace.path.exists());

        let branch_output = StdCommand::new("git")
            .args(["branch", "--list", "kairastra/MT-7"])
            .current_dir(seed_repo.path())
            .output()
            .unwrap();
        assert!(String::from_utf8_lossy(&branch_output.stdout).contains("kairastra/MT-7"));
        std::env::remove_var("KAIRASTRA_SEED_REPO");
    }

    #[tokio::test]
    async fn recreates_invalid_seed_repo_workspace_before_reusing_it() {
        let _guard = ENV_LOCK.lock().await;
        let workspace_root = tempdir().unwrap();
        let seed_repo = tempdir().unwrap();
        init_git_repo(seed_repo.path());
        std::env::set_var("KAIRASTRA_SEED_REPO", seed_repo.path());

        let mut settings = test_settings(workspace_root.path());
        settings.workspace.bootstrap_mode = WorkspaceBootstrapMode::SeedWorktree;
        settings.hooks.after_create = Some(
            r#"
set -euo pipefail
git -C "$KAIRASTRA_SEED_REPO" worktree add --force -b "kairastra/MT-3" "$PWD" HEAD
"#
            .trim()
            .to_string(),
        );

        let existing = workspace_root.path().join("MT-3");
        fs::create_dir_all(&existing).unwrap();
        let status = StdCommand::new("git")
            .args(["init", "-q"])
            .current_dir(&existing)
            .status()
            .unwrap();
        assert!(status.success());

        let workspace = ensure_workspace(&settings, &issue("MT-3")).await.unwrap();
        assert!(workspace.created_now);

        let output = StdCommand::new("git")
            .args(["rev-parse", "--verify", "HEAD"])
            .current_dir(&workspace.path)
            .output()
            .unwrap();
        assert!(output.status.success());

        std::env::remove_var("KAIRASTRA_SEED_REPO");
    }

    #[tokio::test]
    async fn recreates_workspace_when_git_repo_is_not_seed_worktree() {
        let _guard = ENV_LOCK.lock().await;
        let workspace_root = tempdir().unwrap();
        let seed_repo = tempdir().unwrap();
        init_git_repo(seed_repo.path());
        std::env::set_var("KAIRASTRA_SEED_REPO", seed_repo.path());

        let mut settings = test_settings(workspace_root.path());
        settings.workspace.bootstrap_mode = WorkspaceBootstrapMode::SeedWorktree;
        settings.hooks.after_create = Some(
            r#"
set -euo pipefail
git -C "$KAIRASTRA_SEED_REPO" worktree add --force -b "kairastra/MT-4" "$PWD" HEAD
"#
            .trim()
            .to_string(),
        );

        let existing = workspace_root.path().join("MT-4");
        fs::create_dir_all(&existing).unwrap();
        init_git_repo(&existing);

        let workspace = ensure_workspace(&settings, &issue("MT-4")).await.unwrap();
        assert!(workspace.created_now);

        let common_dir_output = StdCommand::new("git")
            .args(["rev-parse", "--git-common-dir"])
            .current_dir(&workspace.path)
            .output()
            .unwrap();
        assert!(common_dir_output.status.success());
        let common_dir = String::from_utf8_lossy(&common_dir_output.stdout)
            .trim()
            .to_string();
        let resolved_common_dir = if Path::new(&common_dir).is_absolute() {
            PathBuf::from(common_dir)
        } else {
            workspace.path.join(common_dir)
        }
        .canonicalize()
        .unwrap();
        assert_eq!(
            resolved_common_dir,
            seed_repo.path().join(".git").canonicalize().unwrap()
        );

        std::env::remove_var("KAIRASTRA_SEED_REPO");
    }

    #[tokio::test]
    async fn plain_bootstrap_mode_does_not_recreate_existing_git_workspace() {
        let _guard = ENV_LOCK.lock().await;
        let workspace_root = tempdir().unwrap();
        let seed_repo = tempdir().unwrap();
        init_git_repo(seed_repo.path());
        std::env::set_var("KAIRASTRA_SEED_REPO", seed_repo.path());

        let mut settings = test_settings(workspace_root.path());
        settings.hooks.after_create = Some(
            r#"
set -euo pipefail
git -C "$KAIRASTRA_SEED_REPO" worktree add --force -b "kairastra/MT-5" "$PWD" HEAD
"#
            .trim()
            .to_string(),
        );

        let existing = workspace_root.path().join("MT-5");
        fs::create_dir_all(&existing).unwrap();
        init_git_repo(&existing);

        let workspace = ensure_workspace(&settings, &issue("MT-5")).await.unwrap();
        assert!(!workspace.created_now);

        let output = StdCommand::new("git")
            .args(["rev-parse", "--git-common-dir"])
            .current_dir(&workspace.path)
            .output()
            .unwrap();
        assert!(output.status.success());
        let common_dir = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let resolved_common_dir = if Path::new(&common_dir).is_absolute() {
            PathBuf::from(common_dir)
        } else {
            workspace.path.join(common_dir)
        }
        .canonicalize()
        .unwrap();
        assert_eq!(
            resolved_common_dir,
            existing.join(".git").canonicalize().unwrap()
        );

        std::env::remove_var("KAIRASTRA_SEED_REPO");
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
    fn repo_workflow_defaults_when_missing() {
        let dir = tempdir().unwrap();

        let workflow = load_workspace_repo_workflow(dir.path()).unwrap();
        assert_eq!(workflow.definition.prompt_template, "");
        assert!(workflow.hooks.after_create.is_none());
    }

    #[test]
    fn repo_workflow_ignores_repo_root_overrides_in_native_mode() {
        let dir = tempdir().unwrap();
        let workflow = load_workspace_repo_workflow(dir.path()).unwrap();
        assert_eq!(workflow.definition.prompt_template, "");
        assert!(workflow.hooks.before_run.is_none());
    }
}
