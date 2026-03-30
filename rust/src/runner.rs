use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::Utc;
use tokio::process::Command;
use tokio::sync::mpsc::UnboundedSender;
use tracing::{info, warn};

use crate::agent::AgentEvent;
use crate::github::{GitHubTracker, OpenPullRequest, PullRequestChecksSummary, Tracker};
use crate::model::Issue;
use crate::prompt::{build_prompt, build_prompt_for_workflow, continuation_prompt};
use crate::providers::{
    self, is_bootstrap_workpad, is_workpad_comment, workpad_header, AGENT_BOOTSTRAP_NOTE,
};
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
    TurnStarted {
        issue_id: String,
        turn_number: usize,
    },
    AppEvent {
        issue_id: String,
        event: AgentEvent,
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
    info!(issue_identifier = %issue.identifier, "ensuring workspace");
    let workspace = workspace::ensure_workspace(&snapshot.settings, &issue)
        .await
        .with_context(|| format!("failed to prepare workspace for {}", issue.identifier))?;
    info!(issue_identifier = %issue.identifier, workspace = %workspace.path.display(), "workspace ready");

    let _ = event_tx.send(WorkerMessage::RuntimeInfo {
        issue_id: issue.id.clone(),
        identifier: issue.identifier.clone(),
        workspace_path: workspace.path.clone(),
    });

    info!(issue_identifier = %issue.identifier, "running before_run hook");
    workspace::run_before_run_hook(&snapshot.settings, &workspace.path, &issue).await?;
    info!(issue_identifier = %issue.identifier, "before_run hook complete");

    let result = async {
        info!(issue_identifier = %issue.identifier, "starting provider session");
        let mut session =
            providers::start_session(&snapshot.settings, tracker.clone(), &workspace.path).await?;
        info!(issue_identifier = %issue.identifier, "provider session started");

        let mut current_issue = issue.clone();
        let workpad_body = render_workpad_bootstrap(
            &workspace.path,
            &current_issue,
            snapshot.settings.agent.provider.as_str(),
        )
        .await?;
        current_issue = tracker
            .ensure_workpad_comment(&current_issue, &workpad_body)
            .await?;

        for turn_number in 1..=snapshot.settings.agent.max_turns {
            info!(issue_identifier = %issue.identifier, turn = turn_number, "running agent turn");
            let _ = event_tx.send(WorkerMessage::TurnStarted {
                issue_id: current_issue.id.clone(),
                turn_number,
            });
            let prompt = if turn_number == 1 {
                if matches!(
                    std::env::var("KAIRASTRA_DEPLOY_MODE").as_deref(),
                    Ok("docker")
                ) {
                    let repo_workflow = workspace::load_workspace_repo_workflow(&workspace.path)?;
                    build_prompt_for_workflow(
                        &snapshot.settings,
                        &repo_workflow.definition,
                        &current_issue,
                        attempt,
                    )?
                } else {
                    build_prompt(&snapshot, &current_issue, attempt)?
                }
            } else {
                continuation_prompt(
                    &current_issue,
                    turn_number,
                    snapshot.settings.agent.max_turns,
                )
            };

            let event_forwarder = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();
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
                .run_turn(&current_issue, &prompt, &forward_tx)
                .await?;
            drop(forward_tx);
            let _ = forwarder.await;
            info!(issue_identifier = %issue.identifier, turn = turn_number, "agent turn complete");

            // The selected agent may have updated the persistent workpad comment during the turn.
            // Refresh it before adding Kairastra's runtime section so we never clobber
            // the latest plan/checklist content with a stale bootstrap copy.
            if current_issue.workpad_comment_id.is_some() {
                current_issue = tracker.refresh_workpad_comment(&current_issue).await?;
            }

            let branch = current_branch(&workspace.path).await?;
            let open_pr = if let (Some((owner, repo)), Some(branch)) =
                (issue_repo(&current_issue), branch.as_deref())
            {
                tracker
                    .find_open_pull_request_for_branch(&owner, &repo, branch)
                    .await?
            } else {
                None
            };
            let pr_checks = if let (Some((owner, repo)), Some(pr)) =
                (issue_repo(&current_issue), open_pr.as_ref())
            {
                Some(
                    tracker
                        .pull_request_checks_summary(&owner, &repo, &pr.head_sha)
                        .await?,
                )
            } else {
                None
            };
            let workpad_body = synthesize_runtime_workpad(
                &workspace.path,
                &current_issue,
                snapshot.settings.agent.provider.as_str(),
                turn_number,
                branch.as_deref(),
                open_pr.as_ref(),
                pr_checks.as_ref(),
            )
            .await?;
            current_issue = tracker
                .update_workpad_comment(&current_issue, &workpad_body)
                .await?;

            let refreshed = tracker
                .fetch_issue_states_by_ids(&[current_issue.id.clone()])
                .await?;
            match refreshed.into_iter().next() {
                Some(mut issue) if snapshot.settings.active_state(&issue.state) => {
                    if current_issue.workpad_comment_id.is_some() {
                        issue = tracker.refresh_workpad_comment(&issue).await?;
                    }

                    if snapshot
                        .settings
                        .tracker
                        .in_progress_state
                        .as_deref()
                        .map(|state| issue.state.trim().eq_ignore_ascii_case(state))
                        .unwrap_or(false)
                        && snapshot.settings.tracker.human_review_state.is_some()
                    {
                        if let Some((owner, repo)) = issue_repo(&issue) {
                            if let Some(branch) = current_branch(&workspace.path).await? {
                                if let Some(pr) = tracker
                                    .find_open_pull_request_for_branch(&owner, &repo, &branch)
                                    .await?
                                {
                                    let checks = tracker
                                        .pull_request_checks_summary(&owner, &repo, &pr.head_sha)
                                        .await?;
                                    if checks.allows_review_handoff()
                                        && workpad_has_progress(&issue)
                                    {
                                        issue = tracker
                                            .transition_issue_to_human_review(&issue)
                                            .await?;
                                    }
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

    if let Err(error) =
        workspace::run_after_run_hook(&snapshot.settings, &workspace.path, &issue).await
    {
        warn!(
            issue_identifier = %issue.identifier,
            error = ?error,
            "after_run hook failed"
        );
        if result.is_ok() {
            return Err(error);
        }
    }
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

async fn git_status_lines(workspace: &std::path::Path) -> Result<Vec<String>> {
    let output = Command::new("git")
        .arg("status")
        .arg("--short")
        .current_dir(workspace)
        .output()
        .await
        .context("failed to read git status")?;

    if !output.status.success() {
        return Ok(Vec::new());
    }

    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.ends_with(".cargo-home/"))
        .map(ToString::to_string)
        .collect())
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

    is_workpad_comment(body) && body.contains("[x]") && !is_bootstrap_workpad(body)
}

const RUNTIME_STATUS_START: &str = "<!-- kairastra-runtime-status:start -->";
const RUNTIME_STATUS_END: &str = "<!-- kairastra-runtime-status:end -->";

async fn synthesize_runtime_workpad(
    workspace: &std::path::Path,
    issue: &Issue,
    provider: &str,
    turn_number: usize,
    branch: Option<&str>,
    open_pr: Option<&OpenPullRequest>,
    pr_checks: Option<&PullRequestChecksSummary>,
) -> Result<String> {
    let sha = current_head_short_sha(workspace)
        .await?
        .unwrap_or_else(|| "unknown".to_string());
    let status_lines = git_status_lines(workspace).await?;
    let base_body = issue
        .workpad_comment_body
        .clone()
        .unwrap_or_else(|| render_workpad_bootstrap_sync(workspace, issue, provider, &sha));
    let runtime_section =
        render_runtime_status_section(turn_number, branch, &sha, &status_lines, open_pr, pr_checks);
    Ok(merge_runtime_status_section(&base_body, &runtime_section))
}

fn render_runtime_status_section(
    turn_number: usize,
    branch: Option<&str>,
    sha: &str,
    status_lines: &[String],
    open_pr: Option<&OpenPullRequest>,
    pr_checks: Option<&PullRequestChecksSummary>,
) -> String {
    let mut lines = vec![
        RUNTIME_STATUS_START.to_string(),
        "### Runtime Status".to_string(),
        String::new(),
        format!(
            "- Last runtime refresh: {} UTC after turn {}",
            Utc::now().format("%Y-%m-%d %H:%M"),
            turn_number
        ),
        format!("- Branch: {}", branch.unwrap_or("main")),
        format!("- HEAD: {sha}"),
    ];

    if status_lines.is_empty() {
        lines.push("- Working tree: clean".to_string());
    } else {
        lines.push("- Working tree changes:".to_string());
        for line in status_lines.iter().take(10) {
            lines.push(format!("  - {line}"));
        }
    }

    if let Some(pr) = open_pr {
        lines.push(format!("- Open PR: {} ({})", pr.url, pr.title));
    } else {
        lines.push("- Open PR: none".to_string());
    }

    if let Some(checks) = pr_checks {
        lines.push(format!("- PR checks: {}", checks.summary_line()));
    } else {
        lines.push("- PR checks: unavailable (no open PR)".to_string());
    }

    lines.push(RUNTIME_STATUS_END.to_string());
    lines.join("\n")
}

fn merge_runtime_status_section(existing_body: &str, runtime_section: &str) -> String {
    let trimmed = existing_body.trim_end();
    if let (Some(start), Some(end)) = (
        trimmed.find(RUNTIME_STATUS_START),
        trimmed.find(RUNTIME_STATUS_END),
    ) {
        let end_index = end + RUNTIME_STATUS_END.len();
        let before = trimmed[..start].trim_end();
        let after = trimmed[end_index..].trim_start();
        if after.is_empty() {
            format!("{before}\n\n{runtime_section}\n")
        } else {
            format!("{before}\n\n{runtime_section}\n\n{after}\n")
        }
    } else {
        format!("{trimmed}\n\n{runtime_section}\n")
    }
}

async fn render_workpad_bootstrap(
    workspace: &std::path::Path,
    issue: &Issue,
    provider: &str,
) -> Result<String> {
    let hostname = runtime_hostname().await?;
    let sha = current_head_short_sha(workspace)
        .await?
        .unwrap_or_else(|| "unknown".to_string());
    Ok(
        render_workpad_bootstrap_sync(workspace, issue, provider, &sha).replacen(
            "unknown-host",
            &hostname,
            1,
        ),
    )
}

fn render_workpad_bootstrap_sync(
    workspace: &std::path::Path,
    issue: &Issue,
    provider: &str,
    sha: &str,
) -> String {
    let issue_url = issue.url.clone().unwrap_or_default();
    let header = workpad_header(provider);

    format!(
        "{header}\n\n```text\nunknown-host:{}@{sha}\n```\n\n### Plan\n\n- [ ] 1\\. Reconcile tracker and repository state\n- [ ] 2\\. Implement the requested issue scope\n- [ ] 3\\. Run required validation\n- [ ] 4\\. Open or update the pull request and link it to the issue\n\n### Acceptance Criteria\n\n- [ ] The requested issue scope is implemented for {}.\n- [ ] Required validation from the issue is complete.\n- [ ] A pull request is opened and linked before review handoff.\n- [ ] GitHub Actions and required PR checks are green before review handoff.\n\n### Validation\n\n- [ ] issue-provided validation steps executed\n\n### Notes\n\n- {AGENT_BOOTSTRAP_NOTE}\n- Issue: {}\n",
        workspace.display(),
        issue.identifier,
        issue_url
    )
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

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{
        merge_runtime_status_section, render_runtime_status_section, render_workpad_bootstrap_sync,
        workpad_has_progress, PullRequestChecksSummary, RUNTIME_STATUS_END, RUNTIME_STATUS_START,
    };
    use crate::github::PullRequestChecksState;
    use crate::model::Issue;
    use crate::providers::{workpad_header, AGENT_WORKPAD_HEADER};

    fn issue_with_workpad(body: Option<&str>) -> Issue {
        Issue {
            id: "1".to_string(),
            project_item_id: None,
            identifier: "dbachko/kairastra#1".to_string(),
            title: "Issue".to_string(),
            description: None,
            priority: None,
            state: "In Progress".to_string(),
            branch_name: None,
            url: Some("https://github.com/dbachko/kairastra/issues/1".to_string()),
            assignees: Vec::new(),
            labels: Vec::new(),
            blocked_by: Vec::new(),
            created_at: None,
            updated_at: None,
            workpad_comment_id: Some(1),
            workpad_comment_url: Some(
                "https://github.com/dbachko/kairastra/issues/1#issuecomment-1".to_string(),
            ),
            workpad_comment_body: body.map(ToString::to_string),
        }
    }

    #[test]
    fn bootstrap_workpad_does_not_count_as_progress() {
        let issue = issue_with_workpad(Some(
            "## Agent Workpad\n\n### Validation\n\n- [ ] issue-provided validation steps executed\n\n### Notes\n\n- Bootstrap created by Kairastra runtime before the first agent turn.\n",
        ));
        assert!(!workpad_has_progress(&issue));
    }

    #[test]
    fn checked_workpad_without_bootstrap_note_counts_as_progress() {
        let issue = issue_with_workpad(Some(
            "## Agent Workpad\n\n### Plan\n\n- [x] 1. Done\n\n### Notes\n\n- Updated by the agent.\n",
        ));
        assert!(workpad_has_progress(&issue));
    }

    #[test]
    fn provider_specific_workpad_counts_as_progress() {
        let issue = issue_with_workpad(Some(
            "## Codex Workpad\n\n### Plan\n\n- [x] 1. Done\n\n### Notes\n\n- Updated by the agent.\n",
        ));
        assert!(workpad_has_progress(&issue));
    }

    #[test]
    fn bootstrap_workpad_uses_provider_specific_header() {
        let issue = issue_with_workpad(None);
        let body =
            render_workpad_bootstrap_sync(Path::new("/tmp/workspace"), &issue, "codex", "abc123");

        assert!(body.starts_with(workpad_header("codex")));
        assert!(!body.starts_with(AGENT_WORKPAD_HEADER));
    }

    #[test]
    fn merge_runtime_status_appends_without_rewriting_plan() {
        let original = "## Agent Workpad\n\n### Plan\n\n- [x] 1. Real plan item\n\n### Validation\n\n- [ ] `cargo test`\n";
        let merged = merge_runtime_status_section(
            original,
            "<!-- kairastra-runtime-status:start -->\n### Runtime Status\n\n- Branch: demo\n<!-- kairastra-runtime-status:end -->",
        );

        assert!(merged.contains("- [x] 1. Real plan item"));
        assert!(merged.contains("- [ ] `cargo test`"));
        assert!(merged.contains("### Runtime Status"));
        assert_eq!(merged.matches("### Plan").count(), 1);
    }

    #[test]
    fn merge_runtime_status_replaces_existing_runtime_block_only() {
        let original = format!(
            "## Agent Workpad\n\n### Plan\n\n- [x] 1. Real plan item\n\n{RUNTIME_STATUS_START}\n### Runtime Status\n\n- Branch: old\n{RUNTIME_STATUS_END}\n"
        );
        let merged = merge_runtime_status_section(
            &original,
            "<!-- kairastra-runtime-status:start -->\n### Runtime Status\n\n- Branch: new\n<!-- kairastra-runtime-status:end -->",
        );

        assert!(merged.contains("- [x] 1. Real plan item"));
        assert!(merged.contains("- Branch: new"));
        assert!(!merged.contains("- Branch: old"));
        assert_eq!(merged.matches("### Runtime Status").count(), 1);
    }

    #[test]
    fn runtime_status_section_has_no_checkboxes() {
        let section = render_runtime_status_section(
            1,
            Some("demo"),
            "abc123",
            &[],
            None,
            Some(&PullRequestChecksSummary {
                state: PullRequestChecksState::Pending,
                failing: Vec::new(),
                pending: vec!["make-all".to_string()],
            }),
        );

        assert!(!section.contains("[x]"));
        assert!(!section.contains("[ ]"));
        assert!(section.contains("PR checks: pending"));
    }
}
