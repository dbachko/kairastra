use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use chrono::Utc;
use serde_json::Value as JsonValue;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};
use tokio::task::JoinHandle;
use tokio::time::{sleep, Duration};
use tracing::{debug, error, info, trace, warn};

use crate::agent::{AgentEvent, AgentEventKind};
use crate::config::{normalize_issue_state, ProviderId, Settings};
use crate::github::{is_rate_limited_error, GitHubTracker, Tracker};
use crate::model::Issue;
use crate::providers;
use crate::runner::{run_issue, WorkerMessage, WorkerOutcome};
use crate::workflow::{WorkflowSnapshot, WorkflowStore};
use crate::workspace;

const CONTINUATION_RETRY_DELAY_MS: u64 = 1_000;
const FAILURE_RETRY_BASE_MS: u64 = 10_000;
const ISSUE_AGENT_LABEL_PREFIX: &str = "agent:";

enum IssueProviderSelection {
    Selected(ProviderId),
    Blocked(String),
}

pub struct Orchestrator {
    workflow_store: Arc<WorkflowStore>,
    tracker: Arc<GitHubTracker>,
}

#[derive(Default)]
struct RuntimeState {
    running: HashMap<String, RunningEntry>,
    claimed: HashSet<String>,
    retry_attempts: HashMap<String, RetryEntry>,
    agent_totals: AgentTotals,
    agent_rate_limits: Option<JsonValue>,
}

#[derive(Debug, Clone, Default)]
struct AgentTotals {
    input_tokens: u64,
    output_tokens: u64,
    total_tokens: u64,
    seconds_running: f64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct TokenUsage {
    input_tokens: u64,
    output_tokens: u64,
    total_tokens: u64,
}

struct RunningEntry {
    identifier: String,
    issue: Issue,
    provider: String,
    workspace_path: Option<PathBuf>,
    started_at: Instant,
    last_agent_timestamp: Instant,
    session_id: Option<String>,
    agent_process_pid: Option<String>,
    last_agent_event: Option<String>,
    last_agent_message: Option<String>,
    input_tokens: u64,
    output_tokens: u64,
    total_tokens: u64,
    last_reported_input_tokens: u64,
    last_reported_output_tokens: u64,
    last_reported_total_tokens: u64,
    turn_count: usize,
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
        let (worker_tx, mut worker_rx) = unbounded_channel::<WorkerMessage>();
        let mut state = RuntimeState::default();

        if let Err(error) = self.startup_cleanup(&snapshot).await {
            if is_rate_limited_error(&error) {
                warn!(error = ?error, "tracker rate limited during startup cleanup");
                return Ok(());
            }
            return Err(error);
        }
        if let Err(error) = self.poll_tick(&snapshot, &mut state, &worker_tx).await {
            if is_rate_limited_error(&error) {
                warn!(error = ?error, "tracker rate limited during run_once poll");
                return Ok(());
            }
            return Err(error);
        }

        self.drain_workers_until_idle(&snapshot, &mut state, &mut worker_rx)
            .await?;

        if !state.retry_attempts.is_empty() {
            info!(
                deferred_retry_count = state.retry_attempts.len(),
                "run_once finished; deferred retries will run on the next invocation"
            );
        }

        Ok(())
    }

    pub async fn run(&self) -> Result<()> {
        let (worker_tx, mut worker_rx) = unbounded_channel::<WorkerMessage>();
        let mut state = RuntimeState::default();

        let initial_snapshot = self.workflow_store.current()?;
        if let Err(error) = self.startup_cleanup(&initial_snapshot).await {
            log_runtime_error("startup cleanup", &error);
        }
        if let Err(error) = self
            .poll_tick(&initial_snapshot, &mut state, &worker_tx)
            .await
        {
            log_runtime_error("initial poll", &error);
        }

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
                        if let Err(error) = self.handle_worker_message(&snapshot, &mut state, message).await {
                            log_runtime_error("worker message handling", &error);
                        }
                    }
                }
                _ = sleep(retry_wait) => {
                    if let Err(error) = self.dispatch_due_retries(&snapshot, &mut state, &worker_tx).await {
                        log_runtime_error("retry dispatch", &error);
                    }
                }
                _ = sleep(poll_interval) => {
                    if let Err(error) = self.poll_tick(&snapshot, &mut state, &worker_tx).await {
                        log_runtime_error("poll tick", &error);
                    }
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

    async fn drain_workers_until_idle(
        &self,
        snapshot: &WorkflowSnapshot,
        state: &mut RuntimeState,
        worker_rx: &mut UnboundedReceiver<WorkerMessage>,
    ) -> Result<()> {
        while !state.running.is_empty() {
            let Some(message) = worker_rx.recv().await else {
                break;
            };
            self.handle_worker_message(snapshot, state, message).await?;
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
        debug!(
            fetched = issues.len(),
            running = state.running.len(),
            "poll tick"
        );
        let available_slots = snapshot
            .settings
            .agent
            .max_concurrent_agents
            .saturating_sub(state.running.len());
        if available_slots == 0 {
            debug!("no available slots; skipping dispatch");
            return Ok(());
        }

        let dispatchable = select_dispatchable(snapshot, &issues, state);
        debug!(
            fetched = issues.len(),
            dispatchable = dispatchable.len(),
            available_slots,
            "dispatch candidates"
        );
        let mut dispatched = 0_usize;
        for issue in dispatchable {
            if dispatched >= available_slots {
                break;
            }

            let Some(issue) = self.revalidate_dispatch_issue(snapshot, issue).await? else {
                continue;
            };

            if self
                .spawn_worker(snapshot.clone(), state, worker_tx, issue, None)
                .await?
            {
                dispatched += 1;
            }
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
                .fetch_issue_states_by_ids(std::slice::from_ref(&issue_id))
                .await?;
            match refreshed.into_iter().next() {
                Some(issue) if snapshot.settings.active_state(&issue.state) => {
                    let _ = self
                        .spawn_worker(
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
        let stall_timeout_ms = providers::stall_timeout_ms(&snapshot.settings)?;

        let mut remove_ids = Vec::new();
        let mut retries = Vec::new();
        let running_ids: Vec<String> = state.running.keys().cloned().collect();
        for issue_id in running_ids {
            let Some(running) = state.running.get_mut(&issue_id) else {
                continue;
            };

            if stall_timeout_ms > 0
                && running.last_agent_timestamp.elapsed() >= Duration::from_millis(stall_timeout_ms)
            {
                running.handle.abort();
                state.claimed.remove(&issue_id);
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
            if let Some(running) = state.running.remove(&issue_id) {
                state.agent_totals.seconds_running += running.started_at.elapsed().as_secs_f64();
            }
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
        mut snapshot: WorkflowSnapshot,
        state: &mut RuntimeState,
        worker_tx: &tokio::sync::mpsc::UnboundedSender<WorkerMessage>,
        issue: Issue,
        attempt: Option<u32>,
    ) -> Result<bool> {
        if state.claimed.contains(&issue.id) || state.running.contains_key(&issue.id) {
            return Ok(false);
        }

        let provider = match issue_provider(&snapshot, &issue) {
            IssueProviderSelection::Selected(provider) => provider,
            IssueProviderSelection::Blocked(warning) => {
                if let Some((provider, failure, blocked)) =
                    classify_provider_selection_block(&issue, &warning)
                {
                    if let Err(error) = self
                        .persist_dispatch_blocker(&snapshot, &issue, &provider, &failure, &blocked)
                        .await
                    {
                        log_runtime_error("dispatch blocker annotation", &error);
                    }
                }
                warn!(
                    issue_identifier = %issue.identifier,
                    warning,
                    "skipping issue because the requested agent provider is invalid or unavailable"
                );
                return Ok(false);
            }
        };
        let provider_name = provider.as_str().to_string();
        snapshot.settings.agent.provider = provider;
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
            .map_err(|error| format!("{error:#}"));

            let _ = tx.send(WorkerMessage::Finished {
                issue_id: issue_for_task.id.clone(),
                identifier: issue_for_task.identifier.clone(),
                workspace_path: workspace_hint_for_task.unwrap_or_default(),
                attempt,
                result,
            });
        });

        let started_at = Instant::now();
        info!(issue_identifier = %identifier, provider = %provider_name, "worker started");
        state.running.insert(
            issue_id.clone(),
            RunningEntry {
                identifier,
                issue,
                provider: provider_name,
                workspace_path: workspace_hint,
                started_at,
                last_agent_timestamp: started_at,
                session_id: None,
                agent_process_pid: None,
                last_agent_event: None,
                last_agent_message: None,
                input_tokens: 0,
                output_tokens: 0,
                total_tokens: 0,
                last_reported_input_tokens: 0,
                last_reported_output_tokens: 0,
                last_reported_total_tokens: 0,
                turn_count: 0,
                attempt,
                handle,
            },
        );

        Ok(true)
    }

    async fn revalidate_dispatch_issue(
        &self,
        snapshot: &WorkflowSnapshot,
        issue: Issue,
    ) -> Result<Option<Issue>> {
        let refreshed = self
            .tracker
            .fetch_issue_states_by_ids(std::slice::from_ref(&issue.id))
            .await?;
        let Some(mut issue) = refreshed.into_iter().next() else {
            return Ok(None);
        };

        if !snapshot.settings.active_state(&issue.state)
            || snapshot.settings.terminal_state(&issue.state)
        {
            return Ok(None);
        }

        if let IssueProviderSelection::Blocked(warning) = issue_provider(snapshot, &issue) {
            if let Some((provider, failure, blocked)) =
                classify_provider_selection_block(&issue, &warning)
            {
                if let Err(error) = self
                    .persist_dispatch_blocker(snapshot, &issue, &provider, &failure, &blocked)
                    .await
                {
                    log_runtime_error("dispatch blocker annotation", &error);
                }
            }
            warn!(
                issue_identifier = %issue.identifier,
                warning,
                "skipping issue because the requested agent provider is invalid or unavailable"
            );
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
            WorkerMessage::TurnStarted {
                issue_id,
                turn_number,
            } => {
                if let Some(running) = state.running.get_mut(&issue_id) {
                    running.last_agent_timestamp = Instant::now();
                    running.turn_count = running.turn_count.max(turn_number);
                    running.last_agent_event = Some("turn_started".to_string());
                    running.last_agent_message = Some(format!("turn {turn_number} started"));
                }
            }
            WorkerMessage::AppEvent { issue_id, event } => {
                let mut usage_delta = None;
                let mut rate_limits = None;
                if let Some(running) = state.running.get_mut(&issue_id) {
                    running.last_agent_timestamp = Instant::now();
                    if let Some(session_id) = event.session_id.clone().or_else(|| {
                        event
                            .payload
                            .get("session_id")
                            .and_then(JsonValue::as_str)
                            .map(ToString::to_string)
                    }) {
                        running.session_id = Some(session_id);
                    }
                    if let Some(process_id) = event.agent_process_pid.clone() {
                        running.agent_process_pid = Some(process_id);
                    }
                    running.last_agent_event = Some(agent_event_name(&event.event).to_string());
                    running.last_agent_message = Some(summarize_agent_event(&event));
                    if let Some(usage) = extract_token_usage(&event.payload) {
                        usage_delta = Some(apply_token_usage(running, usage));
                    }
                    rate_limits = extract_rate_limits(&event.payload);
                    let is_codex = running.provider.eq_ignore_ascii_case("codex");
                    if is_codex {
                        log_codex_event(&running.identifier, &event);
                    }
                    match event.event {
                        AgentEventKind::SessionStarted => {
                            running.turn_count = running.turn_count.max(1);
                        }
                        AgentEventKind::TurnFailed
                        | AgentEventKind::TurnCancelled
                        | AgentEventKind::TurnInputRequired
                        | AgentEventKind::ApprovalRequired
                        | AgentEventKind::TurnEndedWithError => {
                            if !is_codex {
                                warn!(
                                    issue_identifier = %running.identifier,
                                    payload = %event.payload,
                                    "worker emitted terminal app-server event"
                                );
                            }
                        }
                        _ => {}
                    }
                }
                if let Some(delta) = usage_delta {
                    state.agent_totals.input_tokens = state
                        .agent_totals
                        .input_tokens
                        .saturating_add(delta.input_tokens);
                    state.agent_totals.output_tokens = state
                        .agent_totals
                        .output_tokens
                        .saturating_add(delta.output_tokens);
                    state.agent_totals.total_tokens = state
                        .agent_totals
                        .total_tokens
                        .saturating_add(delta.total_tokens);
                }
                if let Some(latest_rate_limits) = rate_limits {
                    state.agent_rate_limits = Some(latest_rate_limits);
                }
            }
            WorkerMessage::Finished {
                issue_id,
                identifier,
                workspace_path,
                attempt,
                result,
                ..
            } => {
                let running = state.running.remove(&issue_id);
                if let Some(entry) = running.as_ref() {
                    state.agent_totals.seconds_running += entry.started_at.elapsed().as_secs_f64();
                }
                let provider = running
                    .as_ref()
                    .map(|entry| entry.provider.as_str())
                    .unwrap_or(snapshot.settings.agent.provider.as_str());
                let session_id = running
                    .as_ref()
                    .and_then(|entry| entry.session_id.as_deref());
                match result {
                    Ok(WorkerOutcome::Completed) => {
                        match self
                            .tracker
                            .fetch_issue_states_by_ids(std::slice::from_ref(&issue_id))
                            .await
                        {
                            Ok(refreshed) => {
                                if let Some(issue) = refreshed.into_iter().next() {
                                    if snapshot.settings.terminal_state(&issue.state) {
                                        self.cleanup_terminal_issue(
                                            snapshot,
                                            issue,
                                            "worker completion",
                                        )
                                        .await;
                                    }
                                }
                            }
                            Err(error) => {
                                log_runtime_error("worker completion refresh", &error);
                            }
                        }
                        state.claimed.remove(&issue_id);
                        info!(issue_identifier = %identifier, "worker completed");
                    }
                    Ok(WorkerOutcome::NeedsContinuation) => {
                        state.claimed.remove(&issue_id);
                        schedule_retry(&snapshot.settings, state, issue_id, identifier, 1, true);
                    }
                    Err(error) => {
                        error!(issue_identifier = %identifier, error, "worker failed");
                        state.claimed.remove(&issue_id);
                        if let Some(blocked) = classify_blocked_worker_failure(provider, &error) {
                            if let Err(annotation_error) = self
                                .persist_blocked_worker_failure(
                                    snapshot,
                                    &issue_id,
                                    BlockedIssueContext {
                                        identifier: &identifier,
                                        provider,
                                        workspace_path: Some(workspace_path.as_path()),
                                        attempt: Some(attempt.unwrap_or(0) + 1),
                                        session_id,
                                        error: &error,
                                        blocked: &blocked,
                                    },
                                )
                                .await
                            {
                                log_runtime_error(
                                    "blocked worker failure annotation",
                                    &annotation_error,
                                );
                            }
                        } else {
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
        }

        Ok(())
    }

    async fn persist_blocked_worker_failure(
        &self,
        snapshot: &WorkflowSnapshot,
        issue_id: &str,
        context: BlockedIssueContext<'_>,
    ) -> Result<()> {
        let mut refreshed = self
            .tracker
            .fetch_issue_states_by_ids(&[issue_id.to_string()])
            .await?;
        let Some(mut issue) = refreshed.pop() else {
            return Ok(());
        };

        self.persist_issue_blocker(snapshot, &mut issue, context)
            .await
    }

    async fn persist_dispatch_blocker(
        &self,
        snapshot: &WorkflowSnapshot,
        issue: &Issue,
        provider: &str,
        error: &str,
        blocked: &BlockedWorkerFailure,
    ) -> Result<()> {
        let mut issue = issue.clone();
        let identifier = issue.identifier.clone();
        let workspace_path = workspace::workspace_path(&snapshot.settings, &issue.identifier).ok();

        self.persist_issue_blocker(
            snapshot,
            &mut issue,
            BlockedIssueContext {
                identifier: &identifier,
                provider,
                workspace_path: workspace_path.as_deref(),
                attempt: None,
                session_id: None,
                error,
                blocked,
            },
        )
        .await
    }

    async fn persist_issue_blocker(
        &self,
        snapshot: &WorkflowSnapshot,
        issue: &mut Issue,
        context: BlockedIssueContext<'_>,
    ) -> Result<()> {
        if issue.workpad_comment_id.is_some() {
            *issue = self.tracker.refresh_workpad_comment(issue).await?;
        }

        let base_body = issue.workpad_comment_body.clone().unwrap_or_else(|| {
            render_blocked_failure_workpad(context.provider, context.workspace_path, issue)
        });
        let blocker_section = render_blocker_section(
            context.provider,
            context.attempt,
            context.session_id,
            context.error,
            context.blocked.operator_action.as_str(),
        );
        let merged_body = merge_blocker_section(&base_body, &blocker_section);
        *issue = self
            .tracker
            .update_workpad_comment(issue, &merged_body)
            .await?;

        if snapshot.settings.active_state(&issue.state)
            && !snapshot.settings.terminal_state(&issue.state)
        {
            *issue = self
                .tracker
                .transition_issue_project_status(issue, "Human Review")
                .await?;
        }

        warn!(
            issue_identifier = %context.identifier,
            provider = context.provider,
            action = %context.blocked.operator_action,
            state = %issue.state,
            "issue blocked; recorded blocker details on the issue"
        );
        Ok(())
    }
}

const BLOCKER_SECTION_START: &str = "<!-- kairastra-runtime-blocker:start -->";
const BLOCKER_SECTION_END: &str = "<!-- kairastra-runtime-blocker:end -->";

#[derive(Debug, Clone, PartialEq, Eq)]
struct BlockedWorkerFailure {
    operator_action: String,
}

#[derive(Clone, Copy)]
struct BlockedIssueContext<'a> {
    identifier: &'a str,
    provider: &'a str,
    workspace_path: Option<&'a Path>,
    attempt: Option<u32>,
    session_id: Option<&'a str>,
    error: &'a str,
    blocked: &'a BlockedWorkerFailure,
}

fn log_codex_event(issue_identifier: &str, event: &AgentEvent) {
    match event.event {
        AgentEventKind::SessionStarted => {
            let session_id = event
                .payload
                .get("session_id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown");
            info!(issue_identifier = %issue_identifier, session_id, "codex session started");
        }
        AgentEventKind::ApprovalAutoApproved => {
            info!(
                issue_identifier = %issue_identifier,
                payload = %event.payload,
                "codex approval auto-approved"
            );
        }
        AgentEventKind::ApprovalRequired => {
            warn!(
                issue_identifier = %issue_identifier,
                payload = %event.payload,
                "codex approval required"
            );
        }
        AgentEventKind::TurnInputRequired => {
            warn!(
                issue_identifier = %issue_identifier,
                payload = %event.payload,
                "codex turn input required"
            );
        }
        AgentEventKind::TurnFailed
        | AgentEventKind::TurnCancelled
        | AgentEventKind::TurnEndedWithError => {
            warn!(
                issue_identifier = %issue_identifier,
                payload = %event.payload,
                "codex turn ended with error"
            );
        }
        AgentEventKind::ToolCallFailed => {
            warn!(
                issue_identifier = %issue_identifier,
                payload = %event.payload,
                "codex dynamic tool call failed"
            );
        }
        AgentEventKind::UnsupportedToolCall => {
            warn!(
                issue_identifier = %issue_identifier,
                payload = %event.payload,
                "codex requested unsupported dynamic tool"
            );
        }
        AgentEventKind::Malformed => {
            warn!(
                issue_identifier = %issue_identifier,
                payload = %event.payload,
                "codex emitted malformed stdout payload"
            );
        }
        AgentEventKind::ToolInputAutoAnswered => {
            info!(
                issue_identifier = %issue_identifier,
                payload = %event.payload,
                "codex tool request user input auto-answered"
            );
        }
        AgentEventKind::Notification => log_codex_notification(issue_identifier, &event.payload),
        _ => {}
    }
}

fn log_codex_notification(issue_identifier: &str, payload: &serde_json::Value) {
    let Some(method) = payload.get("method").and_then(serde_json::Value::as_str) else {
        return;
    };

    match method {
        "turn/started" => {
            let turn_id = payload
                .get("params")
                .and_then(|params| params.get("turn"))
                .and_then(|turn| turn.get("id"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown");
            info!(issue_identifier = %issue_identifier, turn_id, "codex turn started");
        }
        "turn/plan/updated" => {
            let plan = payload
                .get("params")
                .and_then(|params| params.get("plan"))
                .and_then(serde_json::Value::as_array);
            let current_step = plan.and_then(|steps| {
                steps.iter().find_map(|step| {
                    let status = step.get("status").and_then(serde_json::Value::as_str)?;
                    if status == "inProgress" {
                        step.get("step").and_then(serde_json::Value::as_str)
                    } else {
                        None
                    }
                })
            });
            let step_count = plan.map(|steps| steps.len()).unwrap_or(0);
            if let Some(current_step) = current_step {
                info!(
                    issue_identifier = %issue_identifier,
                    step_count,
                    current_step,
                    "codex plan updated"
                );
            } else {
                info!(
                    issue_identifier = %issue_identifier,
                    step_count,
                    "codex plan updated"
                );
            }
        }
        "item/started" => {
            let Some(item) = payload.get("params").and_then(|params| params.get("item")) else {
                return;
            };
            let Some(item_type) = item.get("type").and_then(serde_json::Value::as_str) else {
                return;
            };

            match item_type {
                "commandExecution" => {
                    let command = item
                        .get("command")
                        .and_then(serde_json::Value::as_array)
                        .map(|argv| {
                            argv.iter()
                                .filter_map(serde_json::Value::as_str)
                                .collect::<Vec<_>>()
                                .join(" ")
                        })
                        .unwrap_or_default();
                    info!(
                        issue_identifier = %issue_identifier,
                        command,
                        "codex command started"
                    );
                }
                "fileChange" => {
                    let change_count = item
                        .get("changes")
                        .and_then(serde_json::Value::as_array)
                        .map(|changes| changes.len())
                        .unwrap_or(0);
                    info!(
                        issue_identifier = %issue_identifier,
                        change_count,
                        "codex file change started"
                    );
                }
                "dynamicToolCall" => {
                    let tool = item
                        .get("tool")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("unknown");
                    info!(issue_identifier = %issue_identifier, tool, "codex dynamic tool started");
                }
                "mcpToolCall" => {
                    let server = item
                        .get("server")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("unknown");
                    let tool = item
                        .get("tool")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("unknown");
                    info!(
                        issue_identifier = %issue_identifier,
                        server,
                        tool,
                        "codex MCP tool started"
                    );
                }
                _ => {}
            }
        }
        "item/completed" => {
            let Some(item) = payload.get("params").and_then(|params| params.get("item")) else {
                return;
            };
            let status = item
                .get("status")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("completed");
            if !matches!(status, "failed" | "declined") {
                return;
            }

            let item_type = item
                .get("type")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown");
            warn!(
                issue_identifier = %issue_identifier,
                item_type,
                status,
                payload = %payload,
                "codex item completed unsuccessfully"
            );
        }
        "error" => {
            warn!(
                issue_identifier = %issue_identifier,
                payload = %payload,
                "codex error notification"
            );
        }
        _ => {}
    }
}

fn agent_event_name(event: &AgentEventKind) -> &'static str {
    match event {
        AgentEventKind::SessionStarted => "session_started",
        AgentEventKind::Notification => "notification",
        AgentEventKind::TurnCompleted => "turn_completed",
        AgentEventKind::TurnFailed => "turn_failed",
        AgentEventKind::TurnCancelled => "turn_cancelled",
        AgentEventKind::TurnInputRequired => "turn_input_required",
        AgentEventKind::ApprovalAutoApproved => "approval_auto_approved",
        AgentEventKind::ApprovalRequired => "approval_required",
        AgentEventKind::ToolCallCompleted => "tool_call_completed",
        AgentEventKind::ToolCallFailed => "tool_call_failed",
        AgentEventKind::UnsupportedToolCall => "unsupported_tool_call",
        AgentEventKind::ToolInputAutoAnswered => "tool_input_auto_answered",
        AgentEventKind::Malformed => "malformed",
        AgentEventKind::OtherMessage => "other_message",
        AgentEventKind::TurnEndedWithError => "turn_ended_with_error",
    }
}

fn summarize_agent_event(event: &AgentEvent) -> String {
    let detail = event
        .payload
        .get("method")
        .and_then(JsonValue::as_str)
        .or_else(|| event.payload.get("type").and_then(JsonValue::as_str))
        .or_else(|| event.payload.get("subtype").and_then(JsonValue::as_str))
        .or_else(|| event.payload.get("tool_name").and_then(JsonValue::as_str));

    let base = match detail {
        Some(detail) => format!("{} ({detail})", agent_event_name(&event.event)),
        None => agent_event_name(&event.event).to_string(),
    };

    if base.len() >= 160 {
        return truncate_summary(&base, 160);
    }

    if event.payload.is_null() {
        return base;
    }

    truncate_summary(&format!("{base}: {}", event.payload), 240)
}

fn truncate_summary(value: &str, limit: usize) -> String {
    let mut truncated = value.trim().to_string();
    if truncated.len() > limit {
        truncated.truncate(limit.saturating_sub(3));
        truncated.push_str("...");
    }
    truncated
}

fn apply_token_usage(running: &mut RunningEntry, current: TokenUsage) -> TokenUsage {
    let delta = TokenUsage {
        input_tokens: usage_delta(running.last_reported_input_tokens, current.input_tokens),
        output_tokens: usage_delta(running.last_reported_output_tokens, current.output_tokens),
        total_tokens: usage_delta(running.last_reported_total_tokens, current.total_tokens),
    };

    running.input_tokens = current.input_tokens;
    running.output_tokens = current.output_tokens;
    running.total_tokens = current.total_tokens;
    running.last_reported_input_tokens = current.input_tokens;
    running.last_reported_output_tokens = current.output_tokens;
    running.last_reported_total_tokens = current.total_tokens;

    delta
}

fn usage_delta(previous: u64, current: u64) -> u64 {
    if current >= previous {
        current - previous
    } else {
        current
    }
}

fn extract_token_usage(payload: &JsonValue) -> Option<TokenUsage> {
    match payload {
        JsonValue::Object(object) => {
            if let Some(usage) = object.get("usage").and_then(extract_token_usage) {
                return Some(usage);
            }
            if let Some(usage) = token_usage_from_object(object) {
                return Some(usage);
            }
            object.values().find_map(extract_token_usage)
        }
        JsonValue::Array(values) => values.iter().find_map(extract_token_usage),
        _ => None,
    }
}

fn token_usage_from_object(object: &serde_json::Map<String, JsonValue>) -> Option<TokenUsage> {
    let input_tokens = object
        .get("input_tokens")
        .or_else(|| object.get("inputTokens"))
        .and_then(json_u64);
    let output_tokens = object
        .get("output_tokens")
        .or_else(|| object.get("outputTokens"))
        .and_then(json_u64);
    let total_tokens = object
        .get("total_tokens")
        .or_else(|| object.get("totalTokens"))
        .and_then(json_u64);

    if input_tokens.is_none() && output_tokens.is_none() && total_tokens.is_none() {
        return None;
    }

    let input_tokens = input_tokens.unwrap_or(0);
    let output_tokens = output_tokens.unwrap_or(0);
    let total_tokens = total_tokens.unwrap_or_else(|| input_tokens.saturating_add(output_tokens));

    Some(TokenUsage {
        input_tokens,
        output_tokens,
        total_tokens,
    })
}

fn extract_rate_limits(payload: &JsonValue) -> Option<JsonValue> {
    match payload {
        JsonValue::Object(object) => {
            if let Some(rate_limits) = object
                .get("rate_limits")
                .or_else(|| object.get("rateLimits"))
                .cloned()
            {
                return Some(rate_limits);
            }
            object.values().find_map(extract_rate_limits)
        }
        JsonValue::Array(values) => values.iter().find_map(extract_rate_limits),
        _ => None,
    }
}

fn json_u64(value: &JsonValue) -> Option<u64> {
    match value {
        JsonValue::Number(number) => number.as_u64(),
        JsonValue::String(text) => text.trim().parse::<u64>().ok(),
        _ => None,
    }
}

fn log_runtime_error(phase: &str, error: &anyhow::Error) {
    if is_rate_limited_error(error) {
        warn!(phase, error = ?error, "tracker rate limited; continuing");
    } else {
        error!(phase, error = ?error, "runtime operation failed; continuing");
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

fn classify_blocked_worker_failure(provider: &str, error: &str) -> Option<BlockedWorkerFailure> {
    let normalized = error.to_ascii_lowercase();

    let operator_action = if normalized.contains("approval_required")
        || normalized.contains("requested permissions")
    {
        format!(
            "Grant the missing {} permissions or adjust the provider permission mode, then move the issue back to `Todo` or `In Progress`.",
            provider_display_name(provider)
        )
    } else if normalized.contains("not logged in")
        || normalized.contains("authentication_failed")
        || normalized.contains("api key")
        || normalized.contains("credentials_present: false")
    {
        format!(
            "Configure {} auth in the runtime environment, then move the issue back to `Todo` or `In Progress`.",
            provider_display_name(provider)
        )
    } else if normalized.contains("root/sudo privileges")
        || normalized.contains("dangerously-skip-permissions")
        || normalized.contains("bypasspermissions")
    {
        "Run Claude in a non-root environment or change `providers.claude.permission_mode` / `approval_policy` so Docker does not request bypass permissions, then move the issue back to `Todo` or `In Progress`.".to_string()
    } else if normalized.contains("failed to launch claude code")
        || normalized.contains("failed to launch codex app-server")
        || normalized.contains("no such file or directory")
        || normalized.contains("command not found")
    {
        format!(
            "Install the {} runtime in the worker environment and verify it is available on `PATH`, then move the issue back to `Todo` or `In Progress`.",
            provider_display_name(provider)
        )
    } else if normalized.contains("invalid_workflow_config")
        || normalized.contains("unsupported_agent_provider")
    {
        "Fix the Kairastra workflow/provider configuration, then move the issue back to `Todo` or `In Progress`.".to_string()
    } else {
        return None;
    };

    Some(BlockedWorkerFailure { operator_action })
}

fn classify_provider_selection_block(
    issue: &Issue,
    warning: &str,
) -> Option<(String, String, BlockedWorkerFailure)> {
    if warning == "invalid_issue_agent_label" {
        return Some((
            "unknown".to_string(),
            "Issue has an invalid `agent:` label.".to_string(),
            BlockedWorkerFailure {
                operator_action: "Replace the invalid `agent:` label with exactly one supported provider label, then move the issue back to `Todo` or `In Progress`.".to_string(),
            },
        ));
    }

    if warning == "multiple_issue_agent_labels" {
        return Some((
            "unknown".to_string(),
            "Issue has multiple `agent:` labels.".to_string(),
            BlockedWorkerFailure {
                operator_action: "Leave exactly one `agent:` label on the issue, then move it back to `Todo` or `In Progress`.".to_string(),
            },
        ));
    }

    if let Some(provider) = warning.strip_prefix("issue_requested_provider_not_configured:") {
        let provider = provider.trim().to_ascii_lowercase();
        let display_name = provider_display_name(&provider);
        return Some((
            provider.clone(),
            format!(
                "Issue requested `agent:{provider}`, but {display_name} is not configured in the active workflow/runtime."
            ),
            BlockedWorkerFailure {
                operator_action: format!(
                    "Configure {display_name} in the active workflow/runtime or remove the `agent:{provider}` label, then move the issue back to `Todo` or `In Progress`."
                ),
            },
        ));
    }

    let requested = issue
        .labels
        .iter()
        .find_map(|label| label.strip_prefix(ISSUE_AGENT_LABEL_PREFIX))
        .map(|label| label.trim().to_ascii_lowercase())
        .filter(|label| !label.is_empty())
        .unwrap_or_else(|| "unknown".to_string());

    Some((
        requested,
        format!("Issue cannot be dispatched because provider selection failed: {warning}"),
        BlockedWorkerFailure {
            operator_action:
                "Fix the issue's provider selection or workflow configuration, then move the issue back to `Todo` or `In Progress`."
                    .to_string(),
        },
    ))
}

fn provider_display_name(provider: &str) -> &'static str {
    match provider {
        "claude" => "Claude",
        "codex" => "Codex",
        _ => "provider",
    }
}

fn render_blocked_failure_workpad(
    provider: &str,
    workspace_path: Option<&Path>,
    issue: &Issue,
) -> String {
    let header = providers::workpad_header(provider);
    let stamp = if let Some(workspace_path) = workspace_path {
        format!("unknown-host:{}@unknown", workspace_path.display())
    } else {
        "unknown-host:unknown-workspace@unknown".to_string()
    };
    let issue_line = issue.url.clone().unwrap_or_default();

    format!("{header}\n\n```text\n{stamp}\n```\n\n### Notes\n\n- Issue: {issue_line}\n")
}

fn render_blocker_section(
    provider: &str,
    attempt: Option<u32>,
    session_id: Option<&str>,
    error: &str,
    operator_action: &str,
) -> String {
    format!(
        "{BLOCKER_SECTION_START}\n### Blocker\n\n- Recorded at: {} UTC\n- Provider: {}\n- Attempt: {}\n- Session: {}\n- Failure: {}\n- Required action: {}\n\n```text\n{}\n```\n{BLOCKER_SECTION_END}",
        Utc::now().format("%Y-%m-%d %H:%M"),
        provider_display_name(provider),
        attempt
            .map(|value| value.to_string())
            .unwrap_or_else(|| "not-started".to_string()),
        session_id.unwrap_or("unavailable"),
        summarize_worker_error(error),
        operator_action,
        render_worker_error_details(error)
    )
}

fn summarize_worker_error(error: &str) -> String {
    let mut summary = error.split_whitespace().collect::<Vec<_>>().join(" ");
    if summary.len() > 400 {
        summary.truncate(397);
        summary.push_str("...");
    }
    summary
}

fn render_worker_error_details(error: &str) -> String {
    let mut details = error.trim().to_string();
    if details.is_empty() {
        details = "No additional error details were captured.".to_string();
    }
    if details.len() > 4_000 {
        details.truncate(3_997);
        details.push_str("...");
    }
    details
}

fn merge_blocker_section(existing_body: &str, blocker_section: &str) -> String {
    let trimmed = existing_body.trim_end();
    if let (Some(start), Some(end)) = (
        trimmed.find(BLOCKER_SECTION_START),
        trimmed.find(BLOCKER_SECTION_END),
    ) {
        let end_index = end + BLOCKER_SECTION_END.len();
        let before = trimmed[..start].trim_end();
        let after = trimmed[end_index..].trim_start();
        if after.is_empty() {
            format!("{before}\n\n{blocker_section}\n")
        } else {
            format!("{before}\n\n{blocker_section}\n\n{after}\n")
        }
    } else {
        format!("{trimmed}\n\n{blocker_section}\n")
    }
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
        trace!(issue_identifier = %issue.identifier, "skipping: missing required fields");
        return false;
    }

    if !snapshot.settings.active_state(&issue.state)
        || snapshot.settings.terminal_state(&issue.state)
    {
        trace!(
            issue_identifier = %issue.identifier,
            state = %issue.state,
            "skipping: state not active"
        );
        return false;
    }

    if state.claimed.contains(&issue.id) || state.running.contains_key(&issue.id) {
        trace!(issue_identifier = %issue.identifier, "skipping: already claimed or running");
        return false;
    }

    if let Some(assignee_login) = snapshot.settings.agent.assignee_login.as_deref() {
        if !issue
            .assignees
            .iter()
            .any(|assignee| assignee.eq_ignore_ascii_case(assignee_login))
        {
            trace!(
                issue_identifier = %issue.identifier,
                required_assignee = %assignee_login,
                "skipping: assignee filter not matched"
            );
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
        trace!(issue_identifier = %issue.identifier, "skipping: blocked by open dependency");
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
    if used >= allowed {
        trace!(
            issue_identifier = %issue.identifier,
            used,
            allowed,
            "skipping: concurrency limit reached for state"
        );
    }
    used < allowed
}

fn issue_sort_key(left: &Issue, right: &Issue) -> Ordering {
    left.priority
        .unwrap_or(i64::MAX)
        .cmp(&right.priority.unwrap_or(i64::MAX))
        .then_with(|| left.created_at.cmp(&right.created_at))
        .then_with(|| left.identifier.cmp(&right.identifier))
}

fn issue_provider(snapshot: &WorkflowSnapshot, issue: &Issue) -> IssueProviderSelection {
    let default_provider = snapshot.settings.agent.provider.clone();

    let requested = issue
        .labels
        .iter()
        .filter_map(|label| label.strip_prefix(ISSUE_AGENT_LABEL_PREFIX))
        .map(str::trim)
        .filter(|label| !label.is_empty())
        .collect::<Vec<_>>();

    if requested.is_empty() {
        return IssueProviderSelection::Selected(default_provider);
    }

    let mut parsed = Vec::new();
    for label in requested {
        match ProviderId::parse(label.to_string()) {
            Ok(provider) => parsed.push(provider),
            Err(_) => {
                return IssueProviderSelection::Blocked("invalid_issue_agent_label".to_string());
            }
        }
    }

    parsed.sort_by(|left, right| left.as_str().cmp(right.as_str()));
    parsed.dedup_by(|left, right| left.as_str() == right.as_str());

    match parsed.len() {
        0 => IssueProviderSelection::Selected(default_provider),
        1 => {
            let provider = parsed.into_iter().next().expect("length checked");
            if snapshot.settings.providers.get(&provider).is_none() {
                return IssueProviderSelection::Blocked(format!(
                    "issue_requested_provider_not_configured: {}",
                    provider.as_str()
                ));
            }
            IssueProviderSelection::Selected(provider)
        }
        _ => IssueProviderSelection::Blocked("multiple_issue_agent_labels".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;

    use serde_json::json;

    use crate::config::Settings;
    use crate::model::{BlockerRef, Issue, WorkflowDefinition};

    use super::{
        classify_blocked_worker_failure, classify_provider_selection_block, extract_rate_limits,
        extract_token_usage, issue_eligible, issue_provider, issue_sort_key, merge_blocker_section,
        render_blocked_failure_workpad, select_dispatchable, usage_delta, IssueProviderSelection,
        RuntimeState, TokenUsage, BLOCKER_SECTION_END, BLOCKER_SECTION_START,
    };
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
  provider: codex
  max_concurrent_agents: 10
  max_concurrent_agents_by_state:
    todo: 1
providers:
  codex: {}
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
  provider: codex
  max_concurrent_agents: 10
  assignee_login: {assignee_login}
providers:
  codex: {{}}
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

    fn snapshot_with_provider_overrides() -> WorkflowSnapshot {
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
  provider: codex
  max_concurrent_agents: 10
providers:
  codex: {}
  claude: {}
  gemini: {}
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

        let state = RuntimeState::default();

        assert!(!issue_eligible(&snapshot, &issue, &state, &HashMap::new()));
    }

    #[test]
    fn sorts_by_priority_then_identifier() {
        let mut issues = [
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
        let state = RuntimeState::default();

        let issues = vec![issue("1", "Todo", Some(1)), issue("2", "Todo", Some(2))];
        let selected = select_dispatchable(&snapshot, &issues, &state);
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].id, "1");
    }

    #[test]
    fn human_review_is_not_dispatchable_when_not_active() {
        let snapshot = snapshot();
        let state = RuntimeState::default();

        let issue = issue("1", "Human Review", Some(1));
        assert!(!issue_eligible(&snapshot, &issue, &state, &HashMap::new()));
    }

    #[test]
    fn merging_and_rework_are_dispatchable_when_active() {
        let snapshot = snapshot();
        let state = RuntimeState::default();

        let merging = issue("1", "Merging", Some(1));
        let rework = issue("2", "Rework", Some(1));

        assert!(issue_eligible(&snapshot, &merging, &state, &HashMap::new()));
        assert!(issue_eligible(&snapshot, &rework, &state, &HashMap::new()));
    }

    #[test]
    fn configured_assignee_filter_requires_matching_login() {
        let snapshot = snapshot_with_assignee("codex-bot");
        let state = RuntimeState::default();

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

    #[test]
    fn issue_agent_label_overrides_default_provider() {
        let snapshot = snapshot_with_provider_overrides();
        let mut issue = issue("1", "Todo", Some(1));
        issue.labels = vec!["agent:claude".to_string()];

        match issue_provider(&snapshot, &issue) {
            IssueProviderSelection::Selected(provider) => {
                assert_eq!(provider.as_str(), "claude");
            }
            IssueProviderSelection::Blocked(warning) => {
                panic!("expected provider selection, got warning: {warning}");
            }
        }
    }

    #[test]
    fn issue_agent_label_blocks_dispatch_for_multiple_distinct_providers() {
        let snapshot = snapshot_with_provider_overrides();
        let mut issue = issue("1", "Todo", Some(1));
        issue.labels = vec!["agent:claude".to_string(), "agent:gemini".to_string()];

        match issue_provider(&snapshot, &issue) {
            IssueProviderSelection::Selected(provider) => {
                panic!("expected provider warning, got {}", provider.as_str());
            }
            IssueProviderSelection::Blocked(warning) => {
                assert_eq!(warning, "multiple_issue_agent_labels");
            }
        }
    }

    #[test]
    fn issue_agent_label_blocks_dispatch_when_provider_is_unconfigured() {
        let snapshot = snapshot();
        let mut issue = issue("1", "Todo", Some(1));
        issue.labels = vec!["agent:claude".to_string()];

        match issue_provider(&snapshot, &issue) {
            IssueProviderSelection::Selected(provider) => {
                panic!("expected provider warning, got {}", provider.as_str());
            }
            IssueProviderSelection::Blocked(warning) => {
                assert_eq!(warning, "issue_requested_provider_not_configured: claude");
            }
        }
    }

    #[test]
    fn classifies_claude_auth_failures_as_blocked() {
        let blocked = classify_blocked_worker_failure(
            "claude",
            "Not logged in · Please run /login; process_exited=exit status: 1",
        )
        .expect("Claude auth failures should block");

        assert!(blocked.operator_action.contains("Configure Claude auth"));
    }

    #[test]
    fn classifies_root_bypass_permission_failures_as_blocked() {
        let blocked = classify_blocked_worker_failure(
            "claude",
            "--dangerously-skip-permissions cannot be used with root/sudo privileges for security reasons",
        )
        .expect("root bypass permission failures should block");

        assert!(blocked.operator_action.contains("non-root"));
    }

    #[test]
    fn leaves_retryable_failures_unclassified() {
        assert!(classify_blocked_worker_failure("claude", "turn_timeout").is_none());
    }

    #[test]
    fn classifies_unconfigured_provider_labels_as_dispatch_blockers() {
        let mut issue = issue("55", "Todo", Some(1));
        issue.labels = vec!["agent:claude".to_string()];

        let (provider, failure, blocked) = classify_provider_selection_block(
            &issue,
            "issue_requested_provider_not_configured: claude",
        )
        .expect("unconfigured provider labels should be annotated");

        assert_eq!(provider, "claude");
        assert!(failure.contains("agent:claude"));
        assert!(blocked.operator_action.contains("Configure Claude"));
    }

    #[test]
    fn classifies_multiple_provider_labels_as_dispatch_blockers() {
        let mut issue = issue("55", "Todo", Some(1));
        issue.labels = vec!["agent:claude".to_string(), "agent:codex".to_string()];

        let (provider, failure, blocked) =
            classify_provider_selection_block(&issue, "multiple_issue_agent_labels")
                .expect("multiple provider labels should be annotated");

        assert_eq!(provider, "unknown");
        assert!(failure.contains("multiple `agent:` labels"));
        assert!(blocked.operator_action.contains("exactly one"));
    }

    #[test]
    fn merge_blocker_section_replaces_existing_blocker() {
        let initial = "## Agent Workpad\n\nbody\n";
        let first = merge_blocker_section(
            initial,
            &format!("{BLOCKER_SECTION_START}\nfirst\n{BLOCKER_SECTION_END}"),
        );
        let second = merge_blocker_section(
            &first,
            &format!("{BLOCKER_SECTION_START}\nsecond\n{BLOCKER_SECTION_END}"),
        );

        assert!(second.contains("second"));
        assert!(!second.contains("first"));
    }

    #[test]
    fn extracts_nested_token_usage_and_rate_limits() {
        let payload = json!({
            "params": {
                "usage": {
                    "inputTokens": 12,
                    "outputTokens": "8"
                },
                "rateLimits": {
                    "primaryRemaining": 99
                }
            }
        });

        assert_eq!(
            extract_token_usage(&payload),
            Some(TokenUsage {
                input_tokens: 12,
                output_tokens: 8,
                total_tokens: 20,
            })
        );
        assert_eq!(
            extract_rate_limits(&payload),
            Some(json!({
                "primaryRemaining": 99
            }))
        );
    }

    #[test]
    fn usage_delta_treats_counter_reset_as_new_usage() {
        assert_eq!(usage_delta(10, 15), 5);
        assert_eq!(usage_delta(10, 4), 4);
    }

    #[test]
    fn blocked_failure_workpad_uses_provider_header() {
        let mut issue = issue("55", "In Progress", Some(1));
        issue.url = Some("https://github.com/openai/repo/issues/55".to_string());
        let workspace = PathBuf::from("/workspaces/openai_repo_55");
        let body = render_blocked_failure_workpad("claude", Some(workspace.as_path()), &issue);

        assert!(body.starts_with("## Claude Workpad"));
        assert!(body.contains("/workspaces/openai_repo_55"));
        assert!(body.contains("issues/55"));
    }
}
