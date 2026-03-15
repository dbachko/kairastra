use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::process::Command;
use tokio::sync::mpsc::UnboundedSender;

use crate::app_server::{AppServerEvent, AppServerSession};
use crate::github::{GitHubTracker, Tracker};
use crate::model::Issue;
use crate::prompt::{build_prompt, continuation_prompt};
use crate::workflow::WorkflowSnapshot;
use crate::workspace;

#[derive(Debug, Clone)]
pub enum WorkerOutcome {
    Completed,
    NeedsContinuation,
}

#[derive(Debug, Clone)]
pub enum WorkerMessage {
    RuntimeInfo {
        issue_id: String,
        identifier: String,
        workspace_path: PathBuf,
    },
    AppEvent {
        issue_id: String,
        event: AppServerEvent,
    },
    Finished {
        issue_id: String,
        identifier: String,
        workspace_path: PathBuf,
        attempt: Option<u32>,
        result: Result<WorkerOutcome, String>,
    },
}

pub async fn run_issue(
    snapshot: WorkflowSnapshot,
    tracker: Arc<GitHubTracker>,
    issue: Issue,
    attempt: Option<u32>,
    event_tx: UnboundedSender<WorkerMessage>,
) -> Result<WorkerOutcome> {
    let workspace = workspace::ensure_workspace(&snapshot.settings, &issue)
        .await
        .with_context(|| format!("failed to prepare workspace for {}", issue.identifier))?;

    let _ = event_tx.send(WorkerMessage::RuntimeInfo {
        issue_id: issue.id.clone(),
        identifier: issue.identifier.clone(),
        workspace_path: workspace.path.clone(),
    });

    workspace::run_before_run_hook(&snapshot.settings, &workspace.path, &issue).await?;

    let result = async {
        let mut session =
            AppServerSession::start(&snapshot.settings, tracker.clone(), &workspace.path).await?;

        let mut current_issue = issue.clone();
        let workpad_body = render_workpad_bootstrap(&workspace.path, &current_issue).await?;
        current_issue = tracker
            .ensure_workpad_comment(&current_issue, &workpad_body)
            .await?;

        for turn_number in 1..=snapshot.settings.agent.max_turns {
            let prompt = if turn_number == 1 {
                build_prompt(&snapshot, &current_issue, attempt)?
            } else {
                continuation_prompt(
                    &current_issue,
                    turn_number,
                    snapshot.settings.agent.max_turns,
                )
            };

            let event_forwarder = tokio::sync::mpsc::unbounded_channel::<AppServerEvent>();
            let forward_tx = event_forwarder.0;
            let mut forward_rx = event_forwarder.1;
            let turn_tx = event_tx.clone();
            let issue_id = current_issue.id.clone();

            let forwarder = tokio::spawn(async move {
                while let Some(event) = forward_rx.recv().await {
                    let _ = turn_tx.send(WorkerMessage::AppEvent {
                        issue_id: issue_id.clone(),
                        event,
                    });
                }
            });

            session
                .run_turn(&snapshot.settings, &current_issue, &prompt, &forward_tx)
                .await?;
            drop(forward_tx);
            let _ = forwarder.await;

            let refreshed = tracker
                .fetch_issue_states_by_ids(&[current_issue.id.clone()])
                .await?;
            match refreshed.into_iter().next() {
                Some(mut issue) if snapshot.settings.active_state(&issue.state) => {
                    if current_issue.workpad_comment_id.is_some() {
                        issue = tracker.refresh_workpad_comment(&issue).await?;
                    }

                    if issue.state.trim().eq_ignore_ascii_case("in progress") {
                        if let Some((owner, repo)) = issue_repo(&issue) {
                            if let Some(branch) = current_branch(&workspace.path).await? {
                                let has_open_pr = tracker
                                    .has_open_pull_request_for_branch(&owner, &repo, &branch)
                                    .await?;
                                if has_open_pr && workpad_has_progress(&issue) {
                                    issue = tracker
                                        .transition_issue_project_status(&issue, "Human Review")
                                        .await?;
                                }
                            }
                        }
                    }

                    if !snapshot.settings.active_state(&issue.state) {
                        session.stop().await.ok();
                        return Ok(WorkerOutcome::Completed);
                    }

                    current_issue = issue;
                    if turn_number == snapshot.settings.agent.max_turns {
                        session.stop().await.ok();
                        return Ok(WorkerOutcome::NeedsContinuation);
                    }
                }
                _ => {
                    session.stop().await.ok();
                    return Ok(WorkerOutcome::Completed);
                }
            }
        }

        Ok(WorkerOutcome::NeedsContinuation)
    }
    .await;

    workspace::run_after_run_hook(&snapshot.settings, &workspace.path, &issue).await;
    result
}

async fn current_branch(workspace: &std::path::Path) -> Result<Option<String>> {
    let output = Command::new("git")
        .arg("rev-parse")
        .arg("--abbrev-ref")
        .arg("HEAD")
        .current_dir(workspace)
        .output()
        .await
        .context("failed to read current git branch")?;

    if !output.status.success() {
        return Ok(None);
    }

    let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if branch.is_empty() || branch == "HEAD" {
        Ok(None)
    } else {
        Ok(Some(branch))
    }
}

fn issue_repo(issue: &Issue) -> Option<(String, String)> {
    let (repo_path, _) = issue.identifier.split_once('#')?;
    let (owner, repo) = repo_path.split_once('/')?;
    Some((owner.to_string(), repo.to_string()))
}

fn workpad_has_progress(issue: &Issue) -> bool {
    let Some(body) = issue.workpad_comment_body.as_deref() else {
        return false;
    };

    body.contains("## Codex Workpad")
        && body.contains("[x]")
        && !body.contains("Bootstrap created by Symphony runtime before the first Codex turn.")
}

#[cfg(test)]
mod tests {
    use super::workpad_has_progress;
    use crate::model::Issue;

    fn issue_with_workpad(body: Option<&str>) -> Issue {
        Issue {
            id: "1".to_string(),
            project_item_id: None,
            identifier: "dbachko/symphony-gh#1".to_string(),
            title: "Issue".to_string(),
            description: None,
            priority: None,
            state: "In Progress".to_string(),
            branch_name: None,
            url: Some("https://github.com/dbachko/symphony-gh/issues/1".to_string()),
            assignees: Vec::new(),
            labels: Vec::new(),
            blocked_by: Vec::new(),
            created_at: None,
            updated_at: None,
            workpad_comment_id: Some(1),
            workpad_comment_url: Some(
                "https://github.com/dbachko/symphony-gh/issues/1#issuecomment-1".to_string(),
            ),
            workpad_comment_body: body.map(ToString::to_string),
        }
    }

    #[test]
    fn bootstrap_workpad_does_not_count_as_progress() {
        let issue = issue_with_workpad(Some(
            "## Codex Workpad\n\n### Validation\n\n- [ ] issue-provided validation steps executed\n\n### Notes\n\n- Bootstrap created by Symphony runtime before the first Codex turn.\n",
        ));
        assert!(!workpad_has_progress(&issue));
    }

    #[test]
    fn checked_workpad_without_bootstrap_note_counts_as_progress() {
        let issue = issue_with_workpad(Some(
            "## Codex Workpad\n\n### Plan\n\n- [x] 1. Done\n\n### Notes\n\n- Updated by Codex.\n",
        ));
        assert!(workpad_has_progress(&issue));
    }
}

async fn render_workpad_bootstrap(workspace: &std::path::Path, issue: &Issue) -> Result<String> {
    let hostname = runtime_hostname().await?;
    let sha = current_head_short_sha(workspace)
        .await?
        .unwrap_or_else(|| "unknown".to_string());
    let issue_url = issue.url.clone().unwrap_or_default();

    Ok(format!(
        "## Codex Workpad\n\n```text\n{hostname}:{}@{sha}\n```\n\n### Plan\n\n- [ ] 1\\. Reconcile tracker and repository state\n- [ ] 2\\. Implement the requested issue scope\n- [ ] 3\\. Run required validation\n- [ ] 4\\. Open or update the pull request and link it to the issue\n\n### Acceptance Criteria\n\n- [ ] The requested issue scope is implemented for {}.\n- [ ] Required validation from the issue is complete.\n- [ ] A pull request is opened and linked before review handoff.\n\n### Validation\n\n- [ ] issue-provided validation steps executed\n\n### Notes\n\n- Bootstrap created by Symphony runtime before the first Codex turn.\n- Issue: {}\n",
        workspace.display(),
        issue.identifier,
        issue_url
    ))
}

async fn current_head_short_sha(workspace: &std::path::Path) -> Result<Option<String>> {
    let output = Command::new("git")
        .arg("rev-parse")
        .arg("--short")
        .arg("HEAD")
        .current_dir(workspace)
        .output()
        .await
        .context("failed to read current git sha")?;

    if !output.status.success() {
        return Ok(None);
    }

    let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if sha.is_empty() {
        Ok(None)
    } else {
        Ok(Some(sha))
    }
}

async fn runtime_hostname() -> Result<String> {
    if let Ok(value) = std::env::var("HOSTNAME") {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_string());
        }
    }

    let output = Command::new("hostname")
        .output()
        .await
        .context("failed to read hostname")?;
    if !output.status.success() {
        return Ok("unknown-host".to_string());
    }

    let hostname = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if hostname.is_empty() {
        Ok("unknown-host".to_string())
    } else {
        Ok(hostname)
    }
}
