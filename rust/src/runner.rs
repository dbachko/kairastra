use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use chrono::Utc;
use tokio::process::Command;
use tokio::sync::mpsc::UnboundedSender;
use tracing::{info, warn};

use crate::agent::AgentEvent;
use crate::github::{GitHubTracker, OpenPullRequest, PullRequestChecksSummary, Tracker};
use crate::model::{HumanInputRequest, Issue, PendingHumanInput, PendingHumanInputStatus};
use crate::prompt::{build_prompt, continuation_prompt};
use crate::providers::{
    self, is_bootstrap_workpad, is_workpad_comment, workpad_environment_stamp, workpad_header,
    AGENT_BOOTSTRAP_NOTE,
};
use crate::workflow::WorkflowSnapshot;
use crate::workspace;

#[derive(Debug, Clone)]
pub enum WorkerOutcome {
    Completed,
    NeedsContinuation,
}

#[derive(Debug, Clone)]
pub struct RunIssueResult {
    pub outcome: Result<WorkerOutcome, String>,
    pub human_input_request: Option<HumanInputRequest>,
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
        human_input_request: Option<HumanInputRequest>,
    },
}

pub async fn run_issue(
    snapshot: WorkflowSnapshot,
    tracker: Arc<GitHubTracker>,
    issue: Issue,
    attempt: Option<u32>,
    event_tx: UnboundedSender<WorkerMessage>,
) -> RunIssueResult {
    info!(issue_identifier = %issue.identifier, "ensuring workspace");
    let workspace = match workspace::ensure_workspace(&snapshot.settings, &issue)
        .await
        .with_context(|| format!("failed to prepare workspace for {}", issue.identifier))
    {
        Ok(workspace) => workspace,
        Err(error) => {
            return RunIssueResult {
                outcome: Err(format!("{error:#}")),
                human_input_request: None,
            };
        }
    };
    info!(issue_identifier = %issue.identifier, workspace = %workspace.path.display(), "workspace ready");
    let loaded_pending_human_input = match workspace::load_pending_human_input(&workspace.path)
        .with_context(|| {
            format!(
                "failed to load pending human input for {}",
                issue.identifier
            )
        }) {
        Ok(pending) => pending,
        Err(error) => {
            return RunIssueResult {
                outcome: Err(format!("{error:#}")),
                human_input_request: None,
            };
        }
    };
    let resolved_human_input = loaded_pending_human_input
        .as_ref()
        .filter(|pending| pending.status == PendingHumanInputStatus::Answered)
        .map(render_resolved_human_input_context);

    let _ = event_tx.send(WorkerMessage::RuntimeInfo {
        issue_id: issue.id.clone(),
        identifier: issue.identifier.clone(),
        workspace_path: workspace.path.clone(),
    });

    info!(issue_identifier = %issue.identifier, "running before_run hook");
    if let Err(error) =
        workspace::run_before_run_hook(&snapshot.settings, &workspace.path, &issue).await
    {
        return RunIssueResult {
            outcome: Err(format!("{error:#}")),
            human_input_request: None,
        };
    }
    info!(issue_identifier = %issue.identifier, "before_run hook complete");

    let mut latest_human_input_request = None;
    let result: Result<WorkerOutcome> = async {
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
                let mut prompt = build_prompt(&snapshot, &current_issue, attempt)?;
                if let Some(resolved) = resolved_human_input.as_deref() {
                    prompt.push_str("\n\n");
                    prompt.push_str(resolved);
                }
                prompt
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
            let pending_human_input_request = Arc::new(Mutex::new(None::<HumanInputRequest>));
            let pending_human_input_request_for_forwarder =
                Arc::clone(&pending_human_input_request);

            let forwarder = tokio::spawn(async move {
                while let Some(event) = forward_rx.recv().await {
                    if matches!(
                        event.event,
                        crate::agent::AgentEventKind::UserInputRequested
                    ) {
                        if let Ok(request) = serde_json::from_value(event.payload.clone()) {
                            let mut slot = pending_human_input_request_for_forwarder
                                .lock()
                                .expect("human input request mutex poisoned");
                            *slot = Some(request);
                        }
                    }
                    let _ = turn_tx.send(WorkerMessage::AppEvent {
                        issue_id: issue_id.clone(),
                        event,
                    });
                }
            });

            let turn_result = session.run_turn(&current_issue, &prompt, &forward_tx).await;
            drop(forward_tx);
            let _ = forwarder.await;
            let captured_human_input_request = pending_human_input_request
                .lock()
                .expect("human input request mutex poisoned")
                .clone();
            if captured_human_input_request.is_some() {
                latest_human_input_request = captured_human_input_request;
            }
            let should_clear_resolved_input = turn_number == 1 && resolved_human_input.is_some();
            turn_result?;
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
                        clear_resolved_human_input_if_needed(
                            should_clear_resolved_input,
                            &workspace.path,
                            &current_issue.identifier,
                        )?;
                        session.stop().await.ok();
                        return Ok(WorkerOutcome::Completed);
                    }

                    current_issue = issue;
                    clear_resolved_human_input_if_needed(
                        should_clear_resolved_input,
                        &workspace.path,
                        &current_issue.identifier,
                    )?;
                    if turn_number == snapshot.settings.agent.max_turns {
                        session.stop().await.ok();
                        return Ok(WorkerOutcome::NeedsContinuation);
                    }
                }
                _ => {
                    clear_resolved_human_input_if_needed(
                        should_clear_resolved_input,
                        &workspace.path,
                        &current_issue.identifier,
                    )?;
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
            return RunIssueResult {
                outcome: Err(format!("{error:#}")),
                human_input_request: latest_human_input_request,
            };
        }
    }
    RunIssueResult {
        outcome: result.map_err(|error| format!("{error:#}")),
        human_input_request: latest_human_input_request,
    }
}

fn render_resolved_human_input_context(pending: &PendingHumanInput) -> String {
    let mut context = String::from("Operator answers for the pending planning/input request:\n");
    context.push_str(&format!("- Request ID: `{}`\n", pending.request.request_id));
    if let Some(comment_id) = pending.answered_comment_id {
        context.push_str(&format!("- GitHub comment ID: `{comment_id}`\n"));
    }
    if let Some(login) = pending.answered_by_login.as_deref() {
        context.push_str(&format!("- Answered by: `{login}`\n"));
    }
    for answer in &pending.answers {
        context.push_str(&format!("- `{}`:", answer.question_id));
        if let Some(selected) = answer.selected_option.as_deref() {
            context.push_str(&format!(" selected `{selected}`"));
        }
        if let Some(other) = answer.other_text.as_deref() {
            context.push_str(&format!("; notes `{other}`"));
        }
        context.push('\n');
    }
    context.push_str("- Treat these answers as the latest user intent and continue from them.\n");
    context
}

fn clear_resolved_human_input_if_needed(
    should_clear: bool,
    workspace_path: &std::path::Path,
    issue_identifier: &str,
) -> Result<()> {
    if !should_clear {
        return Ok(());
    }

    workspace::clear_pending_human_input(workspace_path).with_context(|| {
        format!("failed to clear consumed human input state for {issue_identifier}")
    })
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

    is_workpad_comment(body)
        && !is_bootstrap_workpad(body)
        && workpad_section_completed(body, "Acceptance Criteria")
        && workpad_section_completed(body, "Validation")
}

fn workpad_section_completed(body: &str, title: &str) -> bool {
    let mut in_section = false;
    let mut checkbox_count = 0usize;
    let mut unchecked_count = 0usize;

    for line in body.lines() {
        if let Some(heading) = workpad_level_three_heading(line) {
            if in_section {
                break;
            }
            in_section = heading.eq_ignore_ascii_case(title);
            continue;
        }

        if !in_section {
            continue;
        }

        let normalized = line.to_ascii_lowercase();
        if normalized.contains("[ ]") {
            checkbox_count += 1;
            unchecked_count += 1;
        } else if normalized.contains("[x]") {
            checkbox_count += 1;
        }
    }

    checkbox_count > 0 && unchecked_count == 0
}

fn workpad_level_three_heading(line: &str) -> Option<&str> {
    line.trim()
        .strip_prefix("### ")
        .map(str::trim)
        .filter(|heading| !heading.is_empty())
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
        .unwrap_or_else(|| render_workpad_bootstrap_sync(issue, provider, "unknown-host", &sha));
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
    Ok(render_workpad_bootstrap_sync(
        issue, provider, &hostname, &sha,
    ))
}

fn render_workpad_bootstrap_sync(
    issue: &Issue,
    provider: &str,
    hostname: &str,
    sha: &str,
) -> String {
    let issue_url = issue.url.clone().unwrap_or_default();
    let header = workpad_header(provider);
    let stamp = workpad_environment_stamp(hostname, issue, sha);

    format!(
        "{header}\n\n```text\n{stamp}\n```\n\n### Plan\n\n- [ ] 1\\. Reconcile tracker and repository state\n- [ ] 2\\. Implement the requested issue scope\n- [ ] 3\\. Run required validation\n- [ ] 4\\. Open or update the pull request and link it to the issue\n\n### Acceptance Criteria\n\n- [ ] The requested issue scope is implemented for {}.\n- [ ] Required validation from the issue is complete.\n- [ ] A pull request is opened and linked before review handoff.\n- [ ] GitHub Actions and required PR checks are green before review handoff.\n\n### Validation\n\n- [ ] issue-provided validation steps executed\n\n### Notes\n\n- {AGENT_BOOTSTRAP_NOTE}\n- Issue: {}\n",
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
    use std::fs;
    use std::sync::Arc;

    use tempfile::tempdir;
    use tokio::sync::mpsc::unbounded_channel;
    use wiremock::matchers::{body_string_contains, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::{
        merge_runtime_status_section, render_runtime_status_section, render_workpad_bootstrap_sync,
        run_issue, workpad_has_progress, PullRequestChecksSummary, RUNTIME_STATUS_END,
        RUNTIME_STATUS_START,
    };
    use crate::agent::AgentEventKind;
    use crate::config::Settings;
    use crate::github::GitHubTracker;
    use crate::github::PullRequestChecksState;
    use crate::model::{
        HumanInputAnswer, HumanInputQuestion, HumanInputRequest, Issue, PendingHumanInput,
        PendingHumanInputStatus, WorkflowDefinition,
    };
    use crate::providers::{workpad_header, AGENT_WORKPAD_HEADER};
    use crate::workspace::{
        load_pending_human_input, sanitize_workspace_key, write_pending_human_input,
    };

    fn settings_with_command(
        workspace_root: &std::path::Path,
        command: &str,
        endpoint: &str,
    ) -> Settings {
        let definition = WorkflowDefinition {
            config: serde_yaml::from_str(&format!(
                r#"---
tracker:
  kind: github
  owner: openai
  repo: kairastra
  api_key: fake
  endpoint: {endpoint}/graphql
  rest_endpoint: {endpoint}
  mode: issues_only
  status_source:
    type: label
workspace:
  root: {}
agent:
  provider: codex
providers:
  codex:
    command: "{}"
    read_timeout_ms: 15000
    turn_sandbox_policy:
      type: workspaceWrite
      networkAccess: true
"#,
                workspace_root.display(),
                command.replace('"', "\\\""),
            ))
            .unwrap(),
            prompt_template: String::new(),
        };

        Settings::from_workflow(&definition).unwrap()
    }

    fn issue_with_workpad(body: Option<&str>) -> Issue {
        Issue {
            id: "1".to_string(),
            project_item_id: None,
            identifier: "example-owner/example-repo#1".to_string(),
            title: "Issue".to_string(),
            description: None,
            priority: None,
            state: "In Progress".to_string(),
            branch_name: None,
            url: Some("https://github.com/example-owner/example-repo/issues/1".to_string()),
            assignees: Vec::new(),
            labels: Vec::new(),
            blocked_by: Vec::new(),
            created_at: None,
            updated_at: None,
            workpad_comment_id: Some(1),
            workpad_comment_url: Some(
                "https://github.com/example-owner/example-repo/issues/1#issuecomment-1".to_string(),
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
    fn plan_only_workpad_does_not_count_as_review_ready() {
        let issue = issue_with_workpad(Some(
            "## Agent Workpad\n\n### Plan\n\n- [x] 1. Done\n\n### Notes\n\n- Updated by the agent.\n",
        ));
        assert!(!workpad_has_progress(&issue));
    }

    #[test]
    fn provider_specific_workpad_counts_when_review_sections_are_complete() {
        let issue = issue_with_workpad(Some(
            "## Codex Workpad\n\n### Plan\n\n- [x] 1. Done\n\n### Acceptance Criteria\n\n- [x] Scope implemented\n- [x] Pull request opened\n\n### Validation\n\n- [x] cargo test\n\n### Notes\n\n- Updated by the agent.\n",
        ));
        assert!(workpad_has_progress(&issue));
    }

    #[test]
    fn unchecked_review_section_blocks_handoff_progress() {
        let issue = issue_with_workpad(Some(
            "## Agent Workpad\n\n### Acceptance Criteria\n\n- [x] Scope implemented\n\n### Validation\n\n- [ ] cargo test\n\n### Notes\n\n- Updated by the agent.\n",
        ));
        assert!(!workpad_has_progress(&issue));
    }

    #[test]
    fn bootstrap_workpad_uses_provider_specific_header() {
        let issue = issue_with_workpad(None);
        let body =
            render_workpad_bootstrap_sync(&issue, "codex", "MacBookPro.attlocal.net", "abc123");

        assert!(body.starts_with(workpad_header("codex")));
        assert!(!body.starts_with(AGENT_WORKPAD_HEADER));
        assert!(body.contains("macbookpro:example-repo#1@abc123"));
        assert!(!body.contains("/tmp/workspace"));
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
                pending: vec!["CI".to_string()],
            }),
        );

        assert!(!section.contains("[x]"));
        assert!(!section.contains("[ ]"));
        assert!(section.contains("PR checks: pending"));
    }

    #[tokio::test]
    async fn run_issue_carries_human_input_request_on_failed_turn() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/openai/kairastra/issues/1/comments"))
            .and(query_param("per_page", "100"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/repos/openai/kairastra/issues/1/comments"))
            .and(body_string_contains("## Codex Workpad"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": 11,
                "body": "## Codex Workpad\n\ncreated",
                "html_url": "https://github.com/openai/kairastra/issues/1#issuecomment-11",
                "user": { "login": "kairastra" },
                "author_association": "MEMBER",
                "created_at": null,
                "updated_at": null
            })))
            .mount(&server)
            .await;

        let dir = tempdir().unwrap();
        let workspace_root = dir.path().join("workspaces");
        let script = dir.path().join("request-user-input-codex.sh");
        fs::write(
            &script,
            r#"#!/bin/sh
count=0
while IFS= read -r _line; do
  count=$((count + 1))
  case "$count" in
    1)
      printf '%s\n' '{"id":1,"result":{}}'
      ;;
    2)
      printf '%s\n' '{"id":2,"result":{"thread":{"id":"thread-input"}}}'
      ;;
    3)
      printf '%s\n' '{"id":3,"result":{"turn":{"id":"turn-input"}}}'
      ;;
    4)
      printf '%s\n' '{"method":"item/tool/requestUserInput","id":9,"params":{"questions":[{"id":"scope","header":"Scope","question":"Pick a scope","options":[{"label":"Small","description":"Minimum change"},{"label":"Large","description":"Full flow","isOther":true}]}]}}'
      ;;
  esac
done
"#,
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&script).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&script, perms).unwrap();
        }

        let settings = settings_with_command(
            &workspace_root,
            &script.display().to_string(),
            &server.uri(),
        );
        let snapshot = crate::workflow::WorkflowSnapshot {
            definition: WorkflowDefinition {
                config: serde_yaml::from_str(&format!(
                    r#"---
tracker:
  kind: github
  owner: openai
  repo: kairastra
  api_key: fake
  endpoint: {0}/graphql
  rest_endpoint: {0}
  mode: issues_only
  status_source:
    type: label
workspace:
  root: {1}
agent:
  provider: codex
providers:
  codex:
    command: "{2}"
    read_timeout_ms: 15000
    turn_sandbox_policy:
      type: workspaceWrite
      networkAccess: true
"#,
                    server.uri(),
                    workspace_root.display(),
                    script.display().to_string().replace('"', "\\\""),
                ))
                .unwrap(),
                prompt_template: String::new(),
            },
            settings: settings.clone(),
        };
        let tracker = Arc::new(GitHubTracker::new(settings.tracker.clone()).unwrap());
        let issue = Issue {
            id: "1".to_string(),
            project_item_id: None,
            identifier: "openai/kairastra#1".to_string(),
            title: "Need input".to_string(),
            description: Some("body".to_string()),
            priority: None,
            state: "Todo".to_string(),
            branch_name: None,
            url: Some("https://github.com/openai/kairastra/issues/1".to_string()),
            assignees: Vec::new(),
            labels: Vec::new(),
            blocked_by: Vec::new(),
            created_at: None,
            updated_at: None,
            workpad_comment_id: None,
            workpad_comment_url: None,
            workpad_comment_body: None,
        };
        let (tx, mut rx) = unbounded_channel();

        let result = run_issue(snapshot, tracker, issue, None, tx.clone()).await;
        drop(tx);

        assert!(
            matches!(result.outcome, Err(ref error) if error.contains("waiting_for_user_input"))
        );
        let request = result
            .human_input_request
            .expect("human input request should be preserved");
        assert_eq!(request.questions.len(), 1);
        assert_eq!(request.questions[0].id, "scope");

        let mut saw_user_input_event = false;
        while let Ok(message) = rx.try_recv() {
            if let super::WorkerMessage::AppEvent { event, .. } = message {
                if matches!(event.event, AgentEventKind::UserInputRequested) {
                    saw_user_input_event = true;
                }
            }
        }
        assert!(saw_user_input_event);
    }

    #[tokio::test]
    async fn run_issue_keeps_answered_input_when_post_turn_update_fails() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/openai/kairastra/issues/1/comments"))
            .and(query_param("per_page", "100"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/repos/openai/kairastra/issues/1/comments"))
            .and(body_string_contains("## Codex Workpad"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": 11,
                "body": "## Codex Workpad\n\ncreated",
                "html_url": "https://github.com/openai/kairastra/issues/1#issuecomment-11",
                "user": { "login": "kairastra" },
                "author_association": "MEMBER",
                "created_at": null,
                "updated_at": null
            })))
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .and(path("/repos/openai/kairastra/issues/comments/11"))
            .respond_with(ResponseTemplate::new(500).set_body_string("temporary failure"))
            .mount(&server)
            .await;

        let dir = tempdir().unwrap();
        let workspace_root = dir.path().join("workspaces");
        let script = dir.path().join("successful-codex.sh");
        fs::write(
            &script,
            r#"#!/bin/sh
count=0
while IFS= read -r _line; do
  count=$((count + 1))
  case "$count" in
    1)
      printf '%s\n' '{"id":1,"result":{}}'
      ;;
    2)
      printf '%s\n' '{"id":2,"result":{"thread":{"id":"thread-input"}}}'
      ;;
    3)
      printf '%s\n' '{"id":3,"result":{"turn":{"id":"turn-input"}}}'
      ;;
    4)
      printf '%s\n' '{"method":"turn/completed"}'
      ;;
  esac
done
"#,
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&script).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&script, perms).unwrap();
        }

        let settings = settings_with_command(
            &workspace_root,
            &script.display().to_string(),
            &server.uri(),
        );
        let snapshot = crate::workflow::WorkflowSnapshot {
            definition: WorkflowDefinition {
                config: serde_yaml::from_str(&format!(
                    r#"---
tracker:
  kind: github
  owner: openai
  repo: kairastra
  api_key: fake
  endpoint: {0}/graphql
  rest_endpoint: {0}
  mode: issues_only
  status_source:
    type: label
workspace:
  root: {1}
agent:
  provider: codex
providers:
  codex:
    command: "{2}"
    read_timeout_ms: 15000
    turn_sandbox_policy:
      type: workspaceWrite
      networkAccess: true
"#,
                    server.uri(),
                    workspace_root.display(),
                    script.display().to_string().replace('"', "\\\""),
                ))
                .unwrap(),
                prompt_template: String::new(),
            },
            settings: settings.clone(),
        };
        let tracker = Arc::new(GitHubTracker::new(settings.tracker.clone()).unwrap());
        let issue = Issue {
            id: "1".to_string(),
            project_item_id: None,
            identifier: "openai/kairastra#1".to_string(),
            title: "Need input".to_string(),
            description: Some("body".to_string()),
            priority: None,
            state: "Todo".to_string(),
            branch_name: None,
            url: Some("https://github.com/openai/kairastra/issues/1".to_string()),
            assignees: Vec::new(),
            labels: Vec::new(),
            blocked_by: Vec::new(),
            created_at: None,
            updated_at: None,
            workpad_comment_id: None,
            workpad_comment_url: None,
            workpad_comment_body: None,
        };
        let workspace_path = workspace_root.join(sanitize_workspace_key(&issue.identifier));
        fs::create_dir_all(&workspace_path).unwrap();
        write_pending_human_input(
            &workspace_path,
            &PendingHumanInput {
                request: HumanInputRequest {
                    request_id: "input-123".to_string(),
                    provider: "codex".to_string(),
                    issue_id: issue.id.clone(),
                    issue_identifier: issue.identifier.clone(),
                    session_id: Some("thread-input-turn-input".to_string()),
                    requested_at: chrono::Utc::now(),
                    questions: vec![HumanInputQuestion {
                        id: "scope".to_string(),
                        header: Some("Scope".to_string()),
                        question: "Pick a scope".to_string(),
                        options: Vec::new(),
                    }],
                },
                status: PendingHumanInputStatus::Answered,
                prompt_comment_id: Some(42),
                prompt_comment_url: Some("https://example.com/comment".to_string()),
                answered_comment_id: Some(43),
                answered_by_login: Some("dbachko".to_string()),
                answered_at: None,
                answers: vec![HumanInputAnswer {
                    question_id: "scope".to_string(),
                    selected_option: Some("Full flow".to_string()),
                    other_text: None,
                }],
            },
        )
        .unwrap();

        let (tx, _rx) = unbounded_channel();
        let result = run_issue(snapshot, tracker, issue, None, tx).await;

        assert!(result.outcome.is_err());
        assert!(load_pending_human_input(&workspace_path).unwrap().is_some());
    }
}
