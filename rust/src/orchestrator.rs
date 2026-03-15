use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use tokio::sync::mpsc::unbounded_channel;
use tokio::task::JoinHandle;
use tokio::time::{sleep, Duration};
use tracing::{error, info, warn};

use crate::app_server::AppServerEventKind;
use crate::config::{normalize_issue_state, Settings};
use crate::github::{GitHubTracker, Tracker};
use crate::model::Issue;
use crate::runner::{run_issue, WorkerMessage, WorkerOutcome};
use crate::workflow::{WorkflowSnapshot, WorkflowStore};
use crate::workspace;

const CONTINUATION_RETRY_DELAY_MS: u64 = 1_000;
const FAILURE_RETRY_BASE_MS: u64 = 10_000;

pub struct Orchestrator {
    workflow_store: Arc<WorkflowStore>,
    tracker: Arc<GitHubTracker>,
}

struct RuntimeState {
    running: HashMap<String, RunningEntry>,
    claimed: HashSet<String>,
    retry_attempts: HashMap<String, RetryEntry>,
}

struct RunningEntry {
    identifier: String,
    issue: Issue,
    workspace_path: Option<PathBuf>,
    last_codex_timestamp: Instant,
    session_id: Option<String>,
    attempt: Option<u32>,
    handle: JoinHandle<()>,
}

struct RetryEntry {
    attempt: u32,
    due_at: Instant,
}

impl Orchestrator {
    pub fn new(workflow_store: Arc<WorkflowStore>, tracker: Arc<GitHubTracker>) -> Self {
        Self {
            workflow_store,
            tracker,
        }
    }

    pub async fn run_once(&self) -> Result<()> {
        let snapshot = self.workflow_store.current()?;
        let mut state = RuntimeState {
            running: HashMap::new(),
            claimed: HashSet::new(),
            retry_attempts: HashMap::new(),
        };

        self.startup_cleanup(&snapshot).await?;
        self.poll_tick(&snapshot, &mut state, &unbounded_channel().0)
            .await?;
        Ok(())
    }

    pub async fn run(&self) -> Result<()> {
        let (worker_tx, mut worker_rx) = unbounded_channel::<WorkerMessage>();
        let mut state = RuntimeState {
            running: HashMap::new(),
            claimed: HashSet::new(),
            retry_attempts: HashMap::new(),
        };

        let initial_snapshot = self.workflow_store.current()?;
        self.startup_cleanup(&initial_snapshot).await?;
        self.poll_tick(&initial_snapshot, &mut state, &worker_tx)
            .await?;

        loop {
            let snapshot = self.workflow_store.current()?;
            let poll_interval = Duration::from_millis(snapshot.settings.polling.interval_ms);
            let retry_wait = next_retry_wait(&state);

            tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    info!("received interrupt; stopping running workers");
                    for running in state.running.values() {
                        running.handle.abort();
                    }
                    return Ok(());
                }
                maybe_message = worker_rx.recv() => {
                    if let Some(message) = maybe_message {
                        self.handle_worker_message(&snapshot, &mut state, message).await?;
                    }
                }
                _ = sleep(retry_wait) => {
                    self.dispatch_due_retries(&snapshot, &mut state, &worker_tx).await?;
                }
                _ = sleep(poll_interval) => {
                    self.poll_tick(&snapshot, &mut state, &worker_tx).await?;
                }
            }
        }
    }

    async fn startup_cleanup(&self, snapshot: &WorkflowSnapshot) -> Result<()> {
        let issues = self
            .tracker
            .fetch_issues_by_states(&snapshot.settings.tracker.terminal_states)
            .await
            .context("failed to fetch terminal issues during startup cleanup")?;

        for issue in issues {
            self.cleanup_terminal_issue(snapshot, issue, "startup")
                .await;
        }

        Ok(())
    }

    async fn poll_tick(
        &self,
        snapshot: &WorkflowSnapshot,
        state: &mut RuntimeState,
        worker_tx: &tokio::sync::mpsc::UnboundedSender<WorkerMessage>,
    ) -> Result<()> {
        self.reconcile_running(snapshot, state).await?;
        self.dispatch_due_retries(snapshot, state, worker_tx)
            .await?;

        let issues = self.tracker.fetch_candidate_issues().await?;
        let available_slots = snapshot
            .settings
            .agent
            .max_concurrent_agents
            .saturating_sub(state.running.len());
        if available_slots == 0 {
            return Ok(());
        }

        let dispatchable = select_dispatchable(snapshot, &issues, state);
        let mut dispatched = 0_usize;
        for issue in dispatchable {
            if dispatched >= available_slots {
                break;
            }

            let Some(issue) = self.revalidate_dispatch_issue(snapshot, issue).await? else {
                continue;
            };

            self.spawn_worker(snapshot.clone(), state, worker_tx, issue, None)
                .await?;
            dispatched += 1;
        }

        Ok(())
    }

    async fn dispatch_due_retries(
        &self,
        snapshot: &WorkflowSnapshot,
        state: &mut RuntimeState,
        worker_tx: &tokio::sync::mpsc::UnboundedSender<WorkerMessage>,
    ) -> Result<()> {
        let now = Instant::now();
        let due_ids: Vec<String> = state
            .retry_attempts
            .iter()
            .filter(|(_, entry)| entry.due_at <= now)
            .map(|(issue_id, _)| issue_id.clone())
            .collect();

        for issue_id in due_ids {
            let Some(retry) = state.retry_attempts.remove(&issue_id) else {
                continue;
            };

            if state.running.len() >= snapshot.settings.agent.max_concurrent_agents {
                state.retry_attempts.insert(
                    issue_id.clone(),
                    RetryEntry {
                        due_at: Instant::now() + Duration::from_secs(1),
                        ..retry
                    },
                );
                continue;
            }

            let refreshed = self
                .tracker
                .fetch_issue_states_by_ids(&[issue_id.clone()])
                .await?;
            match refreshed.into_iter().next() {
                Some(issue) if snapshot.settings.active_state(&issue.state) => {
                    self.spawn_worker(
                        snapshot.clone(),
                        state,
                        worker_tx,
                        issue,
                        Some(retry.attempt),
                    )
                    .await?;
                }
                Some(issue) if snapshot.settings.terminal_state(&issue.state) => {
                    state.claimed.remove(&issue_id);
                    self.cleanup_terminal_issue(snapshot, issue, "retry").await;
                }
                _ => {
                    state.claimed.remove(&issue_id);
                }
            }
        }

        Ok(())
    }

    async fn reconcile_running(
        &self,
        snapshot: &WorkflowSnapshot,
        state: &mut RuntimeState,
    ) -> Result<()> {
        if state.running.is_empty() {
            return Ok(());
        }

        let ids: Vec<String> = state.running.keys().cloned().collect();
        let refreshed = self.tracker.fetch_issue_states_by_ids(&ids).await?;
        let refreshed_by_id: HashMap<String, Issue> = refreshed
            .into_iter()
            .map(|issue| (issue.id.clone(), issue))
            .collect();

        let mut remove_ids = Vec::new();
        let mut retries = Vec::new();
        let running_ids: Vec<String> = state.running.keys().cloned().collect();
        for issue_id in running_ids {
            let Some(running) = state.running.get_mut(&issue_id) else {
                continue;
            };

            if snapshot.settings.codex.stall_timeout_ms > 0
                && running.last_codex_timestamp.elapsed()
                    >= Duration::from_millis(snapshot.settings.codex.stall_timeout_ms)
            {
                running.handle.abort();
                retries.push((
                    issue_id.clone(),
                    running.identifier.clone(),
                    running.attempt.unwrap_or(0) + 1,
                ));
                remove_ids.push(issue_id.clone());
                continue;
            }

            match refreshed_by_id.get(&issue_id) {
                Some(issue) if snapshot.settings.terminal_state(&issue.state) => {
                    running.handle.abort();
                    state.claimed.remove(&issue_id);
                    self.cleanup_terminal_issue(snapshot, issue.clone(), "terminal reconciliation")
                        .await;
                    remove_ids.push(issue_id.clone());
                }
                Some(issue) if !snapshot.settings.active_state(&issue.state) => {
                    running.handle.abort();
                    state.claimed.remove(&issue_id);
                    remove_ids.push(issue_id.clone());
                }
                Some(issue) => {
                    running.issue = issue.clone();
                }
                None => {
                    running.handle.abort();
                    state.claimed.remove(&issue_id);
                    remove_ids.push(issue_id.clone());
                }
            }
        }

        for issue_id in remove_ids {
            state.running.remove(&issue_id);
        }
        for (issue_id, identifier, attempt) in retries {
            schedule_retry(
                &snapshot.settings,
                state,
                issue_id,
                identifier,
                attempt,
                false,
            );
        }

        Ok(())
    }

    async fn cleanup_terminal_issue(
        &self,
        snapshot: &WorkflowSnapshot,
        issue: Issue,
        context: &str,
    ) {
        let issue = match self.tracker.transition_closed_issue_to_done(&issue).await {
            Ok(updated) => updated,
            Err(error) => {
                warn!(
                    issue_identifier = %issue.identifier,
                    error = ?error,
                    cleanup_context = context,
                    "failed to normalize closed issue to done before cleanup"
                );
                issue
            }
        };

        if let Err(error) =
            workspace::remove_issue_workspace(&snapshot.settings, &issue.identifier).await
        {
            warn!(
                issue_identifier = %issue.identifier,
                error = ?error,
                cleanup_context = context,
                "workspace cleanup failed for terminal issue"
            );
        }
    }

    async fn spawn_worker(
        &self,
        snapshot: WorkflowSnapshot,
        state: &mut RuntimeState,
        worker_tx: &tokio::sync::mpsc::UnboundedSender<WorkerMessage>,
        issue: Issue,
        attempt: Option<u32>,
    ) -> Result<()> {
        if state.claimed.contains(&issue.id) || state.running.contains_key(&issue.id) {
            return Ok(());
        }

        state.claimed.insert(issue.id.clone());

        let issue_id = issue.id.clone();
        let identifier = issue.identifier.clone();
        let tx = worker_tx.clone();
        let tracker = self.tracker.clone();
        let workspace_hint = workspace::workspace_path(&snapshot.settings, &issue.identifier).ok();
        let workspace_hint_for_task = workspace_hint.clone();
        let issue_for_task = issue.clone();

        let handle = tokio::spawn(async move {
            let result = run_issue(
                snapshot,
                tracker,
                issue_for_task.clone(),
                attempt,
                tx.clone(),
            )
            .await
            .map_err(|error| error.to_string());

            let _ = tx.send(WorkerMessage::Finished {
                issue_id: issue_for_task.id.clone(),
                identifier: issue_for_task.identifier.clone(),
                workspace_path: workspace_hint_for_task.unwrap_or_default(),
                attempt,
                result,
            });
        });

        state.running.insert(
            issue_id.clone(),
            RunningEntry {
                identifier,
                issue,
                workspace_path: workspace_hint,
                last_codex_timestamp: Instant::now(),
                session_id: None,
                attempt,
                handle,
            },
        );

        Ok(())
    }

    async fn revalidate_dispatch_issue(
        &self,
        snapshot: &WorkflowSnapshot,
        issue: Issue,
    ) -> Result<Option<Issue>> {
        let refreshed = self
            .tracker
            .fetch_issue_states_by_ids(&[issue.id.clone()])
            .await?;
        let Some(mut issue) = refreshed.into_iter().next() else {
            return Ok(None);
        };

        if !snapshot.settings.active_state(&issue.state)
            || snapshot.settings.terminal_state(&issue.state)
        {
            return Ok(None);
        }

        if issue.state.trim().eq_ignore_ascii_case("todo")
            && issue.blocked_by.iter().any(|blocker| {
                blocker
                    .state
                    .as_deref()
                    .map(|state| !snapshot.settings.terminal_state(state))
                    .unwrap_or(true)
            })
        {
            return Ok(None);
        }

        if issue.state.trim().eq_ignore_ascii_case("todo") {
            issue = self
                .tracker
                .transition_issue_to_in_progress_on_claim(&issue)
                .await?;
        }

        Ok(Some(issue))
    }

    async fn handle_worker_message(
        &self,
        snapshot: &WorkflowSnapshot,
        state: &mut RuntimeState,
        message: WorkerMessage,
    ) -> Result<()> {
        match message {
            WorkerMessage::RuntimeInfo {
                issue_id,
                workspace_path,
                ..
            } => {
                if let Some(running) = state.running.get_mut(&issue_id) {
                    running.workspace_path = Some(workspace_path);
                }
            }
            WorkerMessage::AppEvent { issue_id, event } => {
                if let Some(running) = state.running.get_mut(&issue_id) {
                    running.last_codex_timestamp = Instant::now();
                    match event.event {
                        AppServerEventKind::SessionStarted => {
                            running.session_id = event
                                .payload
                                .get("session_id")
                                .and_then(serde_json::Value::as_str)
                                .map(ToString::to_string);
                        }
                        AppServerEventKind::TurnFailed
                        | AppServerEventKind::TurnCancelled
                        | AppServerEventKind::TurnInputRequired
                        | AppServerEventKind::ApprovalRequired
                        | AppServerEventKind::TurnEndedWithError => {
                            warn!(
                                issue_identifier = %running.identifier,
                                payload = %event.payload,
                                "worker emitted terminal app-server event"
                            );
                        }
                        _ => {}
                    }
                }
            }
            WorkerMessage::Finished {
                issue_id,
                identifier,
                attempt,
                result,
                ..
            } => {
                let _ = state.running.remove(&issue_id);
                match result {
                    Ok(WorkerOutcome::Completed) => {
                        if let Some(issue) = self
                            .tracker
                            .fetch_issue_states_by_ids(&[issue_id.clone()])
                            .await?
                            .into_iter()
                            .next()
                        {
                            if snapshot.settings.terminal_state(&issue.state) {
                                self.cleanup_terminal_issue(snapshot, issue, "worker completion")
                                    .await;
                            }
                        }
                        state.claimed.remove(&issue_id);
                        info!(issue_identifier = %identifier, "worker completed");
                    }
                    Ok(WorkerOutcome::NeedsContinuation) => {
                        schedule_retry(&snapshot.settings, state, issue_id, identifier, 1, true);
                    }
                    Err(error) => {
                        error!(issue_identifier = %identifier, error, "worker failed");
                        schedule_retry(
                            &snapshot.settings,
                            state,
                            issue_id,
                            identifier,
                            attempt.unwrap_or(0) + 1,
                            false,
                        );
                    }
                }
            }
        }

        Ok(())
    }
}

fn next_retry_wait(state: &RuntimeState) -> Duration {
    let now = Instant::now();
    state
        .retry_attempts
        .values()
        .map(|entry| entry.due_at.saturating_duration_since(now))
        .min()
        .unwrap_or_else(|| Duration::from_secs(3600))
}

fn schedule_retry(
    settings: &Settings,
    state: &mut RuntimeState,
    issue_id: String,
    _identifier: String,
    attempt: u32,
    continuation: bool,
) {
    let delay = if continuation {
        CONTINUATION_RETRY_DELAY_MS
    } else {
        let multiplier = 2_u64.saturating_pow(attempt.saturating_sub(1));
        (FAILURE_RETRY_BASE_MS.saturating_mul(multiplier)).min(settings.agent.max_retry_backoff_ms)
    };

    state.retry_attempts.insert(
        issue_id.clone(),
        RetryEntry {
            attempt,
            due_at: Instant::now() + Duration::from_millis(delay),
        },
    );
}

fn select_dispatchable(
    snapshot: &WorkflowSnapshot,
    issues: &[Issue],
    state: &RuntimeState,
) -> Vec<Issue> {
    let mut per_state_counts: HashMap<String, usize> = HashMap::new();
    for running in state.running.values() {
        *per_state_counts
            .entry(normalize_issue_state(&running.issue.state))
            .or_default() += 1;
    }

    let mut selected: Vec<Issue> = issues
        .iter()
        .filter(|issue| issue_eligible(snapshot, issue, state, &per_state_counts))
        .cloned()
        .collect();

    selected.sort_by(issue_sort_key);

    let mut accepted = Vec::new();
    for issue in selected {
        let normalized_state = normalize_issue_state(&issue.state);
        let used = per_state_counts
            .get(&normalized_state)
            .copied()
            .unwrap_or(0);
        let allowed = snapshot
            .settings
            .max_concurrent_agents_for_state(&issue.state);
        if used >= allowed {
            continue;
        }
        per_state_counts.insert(normalized_state, used + 1);
        accepted.push(issue);
    }

    accepted
}

fn issue_eligible(
    snapshot: &WorkflowSnapshot,
    issue: &Issue,
    state: &RuntimeState,
    per_state_counts: &HashMap<String, usize>,
) -> bool {
    if issue.id.trim().is_empty()
        || issue.identifier.trim().is_empty()
        || issue.title.trim().is_empty()
        || issue.state.trim().is_empty()
    {
        return false;
    }

    if !snapshot.settings.active_state(&issue.state)
        || snapshot.settings.terminal_state(&issue.state)
    {
        return false;
    }

    if state.claimed.contains(&issue.id) || state.running.contains_key(&issue.id) {
        return false;
    }

    if let Some(assignee_login) = snapshot.settings.agent.assignee_login.as_deref() {
        if !issue
            .assignees
            .iter()
            .any(|assignee| assignee.eq_ignore_ascii_case(assignee_login))
        {
            return false;
        }
    }

    if issue.state.trim().eq_ignore_ascii_case("todo")
        && issue.blocked_by.iter().any(|blocker| {
            blocker
                .state
                .as_deref()
                .map(|state| !snapshot.settings.terminal_state(state))
                .unwrap_or(true)
        })
    {
        return false;
    }

    let normalized_state = normalize_issue_state(&issue.state);
    let used = per_state_counts
        .get(&normalized_state)
        .copied()
        .unwrap_or(0);
    let allowed = snapshot
        .settings
        .max_concurrent_agents_for_state(&issue.state);
    used < allowed
}

fn issue_sort_key(left: &Issue, right: &Issue) -> Ordering {
    left.priority
        .unwrap_or(i64::MAX)
        .cmp(&right.priority.unwrap_or(i64::MAX))
        .then_with(|| left.created_at.cmp(&right.created_at))
        .then_with(|| left.identifier.cmp(&right.identifier))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::config::Settings;
    use crate::model::{BlockerRef, Issue, WorkflowDefinition};

    use super::{issue_eligible, issue_sort_key, select_dispatchable, RuntimeState};
    use crate::workflow::WorkflowSnapshot;

    fn snapshot() -> WorkflowSnapshot {
        let workflow = WorkflowDefinition {
            config: serde_yaml::from_str(
                r#"
tracker:
  kind: github
  owner: openai
  project_v2_number: 7
  api_key: fake
  active_states: ["Todo", "In Progress", "Merging", "Rework"]
  terminal_states: ["Done", "Closed"]
agent:
  max_concurrent_agents: 10
  max_concurrent_agents_by_state:
    todo: 1
"#,
            )
            .unwrap(),
            prompt_template: String::new(),
        };
        let settings = Settings::from_workflow(&workflow).unwrap();
        WorkflowSnapshot {
            definition: workflow,
            settings,
        }
    }

    fn snapshot_with_assignee(assignee_login: &str) -> WorkflowSnapshot {
        let workflow = WorkflowDefinition {
            config: serde_yaml::from_str(&format!(
                r#"
tracker:
  kind: github
  owner: openai
  project_v2_number: 7
  api_key: fake
  active_states: ["Todo", "In Progress", "Merging", "Rework"]
  terminal_states: ["Done", "Closed"]
agent:
  max_concurrent_agents: 10
  assignee_login: {assignee_login}
"#
            ))
            .unwrap(),
            prompt_template: String::new(),
        };
        let settings = Settings::from_workflow(&workflow).unwrap();
        WorkflowSnapshot {
            definition: workflow,
            settings,
        }
    }

    fn issue(id: &str, state: &str, priority: Option<i64>) -> Issue {
        Issue {
            id: id.to_string(),
            project_item_id: None,
            identifier: format!("openai/repo#{id}"),
            title: format!("Issue {id}"),
            description: None,
            priority,
            state: state.to_string(),
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

    #[test]
    fn blocks_todo_issues_with_active_blockers() {
        let snapshot = snapshot();
        let mut issue = issue("1", "Todo", Some(1));
        issue.blocked_by.push(BlockerRef {
            id: Some("b1".to_string()),
            identifier: Some("B-1".to_string()),
            state: Some("In Progress".to_string()),
        });

        let state = RuntimeState {
            running: Default::default(),
            claimed: Default::default(),
            retry_attempts: Default::default(),
        };

        assert!(!issue_eligible(&snapshot, &issue, &state, &HashMap::new()));
    }

    #[test]
    fn sorts_by_priority_then_identifier() {
        let mut issues = vec![
            issue("2", "Todo", Some(2)),
            issue("1", "Todo", Some(1)),
            issue("3", "Todo", None),
        ];
        issues.sort_by(issue_sort_key);
        assert_eq!(issues[0].id, "1");
        assert_eq!(issues[1].id, "2");
        assert_eq!(issues[2].id, "3");
    }

    #[test]
    fn enforces_per_state_capacity() {
        let snapshot = snapshot();
        let state = RuntimeState {
            running: Default::default(),
            claimed: Default::default(),
            retry_attempts: Default::default(),
        };

        let issues = vec![issue("1", "Todo", Some(1)), issue("2", "Todo", Some(2))];
        let selected = select_dispatchable(&snapshot, &issues, &state);
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].id, "1");
    }

    #[test]
    fn human_review_is_not_dispatchable_when_not_active() {
        let snapshot = snapshot();
        let state = RuntimeState {
            running: Default::default(),
            claimed: Default::default(),
            retry_attempts: Default::default(),
        };

        let issue = issue("1", "Human Review", Some(1));
        assert!(!issue_eligible(&snapshot, &issue, &state, &HashMap::new()));
    }

    #[test]
    fn merging_and_rework_are_dispatchable_when_active() {
        let snapshot = snapshot();
        let state = RuntimeState {
            running: Default::default(),
            claimed: Default::default(),
            retry_attempts: Default::default(),
        };

        let merging = issue("1", "Merging", Some(1));
        let rework = issue("2", "Rework", Some(1));

        assert!(issue_eligible(&snapshot, &merging, &state, &HashMap::new()));
        assert!(issue_eligible(&snapshot, &rework, &state, &HashMap::new()));
    }

    #[test]
    fn configured_assignee_filter_requires_matching_login() {
        let snapshot = snapshot_with_assignee("codex-bot");
        let state = RuntimeState {
            running: Default::default(),
            claimed: Default::default(),
            retry_attempts: Default::default(),
        };

        let mut assigned = issue("1", "Todo", Some(1));
        assigned.assignees = vec!["codex-bot".to_string()];

        let mut other_assignee = issue("2", "Todo", Some(1));
        other_assignee.assignees = vec!["someone-else".to_string()];

        let unassigned = issue("3", "Todo", Some(1));

        assert!(issue_eligible(
            &snapshot,
            &assigned,
            &state,
            &HashMap::new()
        ));
        assert!(!issue_eligible(
            &snapshot,
            &other_assignee,
            &state,
            &HashMap::new()
        ));
        assert!(!issue_eligible(
            &snapshot,
            &unassigned,
            &state,
            &HashMap::new()
        ));
    }
}
