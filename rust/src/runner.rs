use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
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
        for turn_number in 1..=snapshot.settings.agent.max_turns {
            let prompt = if turn_number == 1 {
                build_prompt(&snapshot, &current_issue, attempt)?
            } else {
                continuation_prompt(turn_number, snapshot.settings.agent.max_turns)
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
                Some(issue) if snapshot.settings.active_state(&issue.state) => {
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
