use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use tokio::process::Command;
use tokio::time::{timeout, Duration};

use crate::config::Settings;
use crate::model::Issue;

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

pub async fn run_after_run_hook(settings: &Settings, workspace: &Path, issue: &Issue) {
    if let Some(script) = settings.hooks.after_run.as_deref() {
        let _ = run_hook(settings, "after_run", script, workspace, issue).await;
    }
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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use tempfile::tempdir;

    use crate::config::Settings;
    use crate::model::{Issue, WorkflowDefinition};

    use super::{ensure_workspace, remove_issue_workspace, run_hook, sanitize_workspace_key};

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
}
