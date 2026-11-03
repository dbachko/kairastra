use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};
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
use crate::github::{is_rate_limited_error, GitHubTracker, RestIssueComment, Tracker};
use crate::model::{
    HumanInputAnswer, HumanInputRequest, Issue, PendingHumanInput, PendingHumanInputStatus,
};
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
    blocked_claims: HashMap<String, String>,
    retry_attempts: HashMap<String, RetryEntry>,
    last_idle_summary: Option<String>,
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
    stall_timeout_ms: u64,
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
    pending_human_input_request: Option<HumanInputRequest>,
    attempt: Option<u32>,
    handle: JoinHandle<()>,
}

#[derive(Clone)]
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
        let poll_result = self.poll_tick(&snapshot, &mut state, &worker_tx).await;
        self.drain_workers_until_idle(&snapshot, &mut state, &mut worker_rx)
            .await?;
        if let Err(error) = poll_result {
            if is_rate_limited_error(&error) {
                warn!(error = ?error, "tracker rate limited during run_once poll");
            } else {
                return Err(error);
            }
        }

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

        let mut snapshot = self.workflow_store.current()?;
        let mut runtime = Self::new(self.workflow_store.clone(), self.tracker.clone());
        let candidate = snapshot.clone();
        runtime.apply_snapshot(&mut snapshot, candidate)?;
        if let Err(error) = runtime.startup_cleanup(&snapshot).await {
            log_runtime_error("startup cleanup", &error);
        }
        if let Err(error) = runtime.poll_tick(&snapshot, &mut state, &worker_tx).await {
            log_runtime_error("initial poll", &error);
        }

        let mut poll_interval = Duration::from_millis(snapshot.settings.polling.interval_ms);
        let mut next_poll = tokio::time::Instant::now() + poll_interval;
        loop {
            let candidate = self.workflow_store.current()?;
            if let Err(error) = runtime.apply_snapshot(&mut snapshot, candidate) {
                warn!(error = ?error, "workflow reload rejected; keeping the previous runtime configuration");
            }
            let configured_interval = Duration::from_millis(snapshot.settings.polling.interval_ms);
            if configured_interval != poll_interval {
                poll_interval = configured_interval;
                next_poll = tokio::time::Instant::now() + poll_interval;
            }
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
                        if let Err(error) = runtime.handle_worker_message(&snapshot, &mut state, message).await {
                            log_runtime_error("worker message handling", &error);
                        }
                    }
                }
                _ = sleep(retry_wait) => {
                    if let Err(error) = runtime.dispatch_due_retries(&snapshot, &mut state, &worker_tx).await {
                        log_runtime_error("retry dispatch", &error);
                    }
                }
                _ = tokio::time::sleep_until(next_poll) => {
                    if let Err(error) = runtime.poll_tick(&snapshot, &mut state, &worker_tx).await {
                        log_runtime_error("poll tick", &error);
                    }
                    next_poll = tokio::time::Instant::now() + poll_interval;
                }
            }
        }
    }

    fn apply_snapshot(
        &mut self,
        current: &mut WorkflowSnapshot,
        candidate: WorkflowSnapshot,
    ) -> Result<()> {
        let previous = self.tracker.settings();
        let next = &candidate.settings.tracker;
        // Workers retain their tracker. A different repository/project would make
        // their issue IDs unsafe for the control loop to reconcile.
        if previous.mode != next.mode
            || previous.owner != next.owner
            || previous.repo != next.repo
            || previous.project_owner != next.project_owner
            || previous.project_v2_number != next.project_v2_number
            || previous.project_url != next.project_url
            || previous.graphql_endpoint != next.graphql_endpoint
            || previous.rest_endpoint != next.rest_endpoint
        {
            anyhow::bail!("tracker scope changes require a restart");
        }
        if previous != next {
            self.tracker = Arc::new(GitHubTracker::new(next.clone())?);
        }
        *current = candidate;
        Ok(())
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
        self.revalidate_blocked_claims(snapshot, state).await?;
        self.dispatch_due_retries(snapshot, state, worker_tx)
            .await?;

        let issues = self.tracker.fetch_candidate_issues().await?;
        let issues = self
            .reconcile_pending_human_inputs(snapshot, state, issues)
            .await?;
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
        if dispatchable.is_empty() {
            if let Some(summary) = summarize_idle_dispatch(snapshot, &issues, state) {
                if state.last_idle_summary.as_deref() != Some(summary.as_str()) {
                    let has_unknown_states = issues
                        .iter()
                        .any(|issue| !is_known_issue_state(&snapshot.settings, &issue.state));
                    if has_unknown_states {
                        warn!(available_slots, summary = %summary, "no dispatchable issues");
                    } else {
                        info!(available_slots, summary = %summary, "no dispatchable issues");
                    }
                    state.last_idle_summary = Some(summary);
                }
            }
        } else {
            state.last_idle_summary = None;
        }
        let mut dispatched = 0_usize;
        for issue in dispatchable {
            if dispatched >= available_slots {
                break;
            }

            let Some(issue) = self
                .revalidate_dispatch_issue(snapshot, state, &issue.id)
                .await?
            else {
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

    async fn revalidate_blocked_claims(
        &self,
        snapshot: &WorkflowSnapshot,
        state: &mut RuntimeState,
    ) -> Result<()> {
        if state.blocked_claims.is_empty() {
            return Ok(());
        }

        let blocked_ids = state.blocked_claims.keys().cloned().collect::<Vec<_>>();
        let refreshed = self.tracker.fetch_issue_states_by_ids(&blocked_ids).await?;
        let issues_by_id = refreshed
            .into_iter()
            .map(|issue| (issue.id.clone(), issue))
            .collect::<HashMap<_, _>>();

        for issue_id in blocked_ids {
            let Some(previous_state) = state.blocked_claims.get(&issue_id).cloned() else {
                continue;
            };

            let Some(issue) = issues_by_id.get(&issue_id) else {
                clear_blocked_claim(state, &issue_id);
                continue;
            };

            if snapshot.settings.terminal_state(&issue.state) {
                clear_blocked_claim(state, &issue_id);
                self.cleanup_terminal_issue(snapshot, issue.clone(), "blocked claim release")
                    .await;
                continue;
            }

            if should_release_blocked_claim(&snapshot.settings, &previous_state, issue) {
                clear_blocked_claim(state, &issue_id);
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
            let Some(retry) = state.retry_attempts.get(&issue_id).cloned() else {
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

            let dispatch = async {
                let refreshed = self
                    .tracker
                    .fetch_issue_states_by_ids(std::slice::from_ref(&issue_id))
                    .await?;
                let Some(issue) = refreshed.into_iter().next() else {
                    return Ok(false);
                };
                if snapshot.settings.terminal_state(&issue.state) {
                    self.cleanup_terminal_issue(snapshot, issue, "retry release")
                        .await;
                    return Ok(false);
                }
                if !issue_meets_dispatch_filters(snapshot, &issue, state, true) {
                    return Ok(false);
                }
                if !has_dispatch_capacity(snapshot, &issue, &running_state_counts(state)) {
                    return Ok(true);
                }
                if let Some(issue) = self.validate_dispatch_issue(snapshot, state, issue).await? {
                    self.spawn_worker(
                        snapshot.clone(),
                        state,
                        worker_tx,
                        issue,
                        Some(retry.attempt),
                    )
                    .await?;
                }
                Ok::<bool, anyhow::Error>(false)
            }
            .await;
            match dispatch {
                Ok(true) => {
                    // Keep the attempt and its reservation while capacity is unavailable.
                    state
                        .retry_attempts
                        .get_mut(&issue_id)
                        .expect("retry still queued")
                        .due_at = Instant::now() + Duration::from_secs(1);
                }
                Ok(false) => {
                    state.retry_attempts.remove(&issue_id);
                    if !state.blocked_claims.contains_key(&issue_id)
                        && !state.running.contains_key(&issue_id)
                    {
                        state.claimed.remove(&issue_id);
                    }
                }
                Err(error) => {
                    schedule_retry(
                        &snapshot.settings,
                        state,
                        issue_id.clone(),
                        issue_id,
                        retry.attempt,
                        false,
                    );
                    return Err(error);
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

            if running.stall_timeout_ms > 0
                && running.last_agent_timestamp.elapsed()
                    >= Duration::from_millis(running.stall_timeout_ms)
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
                    classify_provider_selection_block(&snapshot.settings, &issue, &warning)
                {
                    match self
                        .persist_dispatch_blocker(&snapshot, &issue, &provider, &failure, &blocked)
                        .await
                    {
                        Ok(issue) => {
                            if should_hold_blocked_claim(&snapshot.settings, &issue) {
                                hold_blocked_claim(state, &issue);
                            }
                        }
                        Err(error) => {
                            log_runtime_error("dispatch blocker annotation", &error);
                        }
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
        let stall_timeout_ms = providers::stall_timeout_ms_for_issue(&snapshot.settings, &issue)?;
        state.claimed.insert(issue.id.clone());

        let issue_id = issue.id.clone();
        let identifier = issue.identifier.clone();
        let tx = worker_tx.clone();
        let tracker = self.tracker.clone();
        let workspace_hint = workspace::workspace_path(&snapshot.settings, &issue.identifier).ok();
        let workspace_hint_for_task = workspace_hint.clone();
        let issue_for_task = issue.clone();

        let handle = tokio::spawn(async move {
            let completion = run_issue(
                snapshot,
                tracker,
                issue_for_task.clone(),
                attempt,
                tx.clone(),
            )
            .await;

            let _ = tx.send(WorkerMessage::Finished {
                issue_id: issue_for_task.id.clone(),
                identifier: issue_for_task.identifier.clone(),
                workspace_path: workspace_hint_for_task.unwrap_or_default(),
                attempt,
                result: completion.outcome,
                human_input_request: completion.human_input_request,
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
                stall_timeout_ms,
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
                pending_human_input_request: None,
                attempt,
                handle,
            },
        );

        Ok(true)
    }

    async fn revalidate_dispatch_issue(
        &self,
        snapshot: &WorkflowSnapshot,
        state: &mut RuntimeState,
        issue_id: &str,
    ) -> Result<Option<Issue>> {
        let refreshed = self
            .tracker
            .fetch_issue_states_by_ids(&[issue_id.to_string()])
            .await?;
        let Some(issue) = refreshed.into_iter().next() else {
            return Ok(None);
        };
        self.validate_dispatch_issue(snapshot, state, issue).await
    }

    async fn validate_dispatch_issue(
        &self,
        snapshot: &WorkflowSnapshot,
        state: &mut RuntimeState,
        mut issue: Issue,
    ) -> Result<Option<Issue>> {
        let counts = running_state_counts(state);
        if !issue_eligible_now(snapshot, &issue, state, &counts, true) {
            return Ok(None);
        }

        if let IssueProviderSelection::Blocked(warning) = issue_provider(snapshot, &issue) {
            if let Some((provider, failure, blocked)) =
                classify_provider_selection_block(&snapshot.settings, &issue, &warning)
            {
                match self
                    .persist_dispatch_blocker(snapshot, &issue, &provider, &failure, &blocked)
                    .await
                {
                    Ok(issue) => {
                        if should_hold_blocked_claim(&snapshot.settings, &issue) {
                            hold_blocked_claim(state, &issue);
                        }
                    }
                    Err(error) => {
                        log_runtime_error("dispatch blocker annotation", &error);
                    }
                }
            }
            warn!(
                issue_identifier = %issue.identifier,
                warning,
                "skipping issue because the requested agent provider is invalid or unavailable"
            );
            return Ok(None);
        }

        if snapshot.settings.claimable_state(&issue.state)
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

        if snapshot.settings.claimable_state(&issue.state) {
            issue = self
                .tracker
                .transition_issue_to_in_progress_on_claim(&issue)
                .await?;
        }

        if !issue_eligible_now(snapshot, &issue, state, &running_state_counts(state), true) {
            return Ok(None);
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
                    if let AgentEventKind::UserInputRequested = event.event {
                        running.pending_human_input_request =
                            serde_json::from_value(event.payload.clone()).ok();
                    }
                    rate_limits = extract_rate_limits(&event.payload);
                    log_provider_event(&running.provider, &running.identifier, &event);
                    if let AgentEventKind::SessionStarted = event.event {
                        running.turn_count = running.turn_count.max(1);
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
                human_input_request,
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
                let pending_human_input_request = resolve_human_input_request(
                    human_input_request,
                    running
                        .as_ref()
                        .and_then(|entry| entry.pending_human_input_request.clone()),
                );
                let persisted_workspace_path = finished_workspace_path(
                    running
                        .as_ref()
                        .and_then(|entry| entry.workspace_path.as_deref()),
                    workspace_path.as_path(),
                );
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
                        if error.contains("waiting_for_user_input") {
                            match pending_human_input_request {
                                Some(request) => {
                                    match self
                                        .persist_human_input_request(
                                            snapshot,
                                            &issue_id,
                                            persisted_workspace_path,
                                            request,
                                        )
                                        .await
                                    {
                                        Ok(issue) => hold_blocked_claim(state, &issue),
                                        Err(annotation_error) => {
                                            clear_blocked_claim(state, &issue_id);
                                            log_runtime_error(
                                                "human input request annotation",
                                                &annotation_error,
                                            );
                                        }
                                    }
                                }
                                None => {
                                    clear_blocked_claim(state, &issue_id);
                                    log_runtime_error(
                                        "human input request annotation",
                                        &anyhow::anyhow!(
                                            "waiting_for_user_input without request payload (provider={provider}, session={}, issue_identifier={identifier})",
                                            session_id.unwrap_or("unavailable"),
                                        ),
                                    );
                                }
                            }
                        } else if let Some(blocked) =
                            classify_blocked_worker_failure(&snapshot.settings, provider, &error)
                        {
                            match self
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
                                Ok(issue) => {
                                    if should_hold_blocked_claim(&snapshot.settings, &issue) {
                                        hold_blocked_claim(state, &issue);
                                    } else {
                                        clear_blocked_claim(state, &issue_id);
                                    }
                                }
                                Err(annotation_error) => {
                                    clear_blocked_claim(state, &issue_id);
                                    log_runtime_error(
                                        "blocked worker failure annotation",
                                        &annotation_error,
                                    );
                                }
                            }
                        } else {
                            clear_blocked_claim(state, &issue_id);
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
    ) -> Result<Issue> {
        let mut refreshed = self
            .tracker
            .fetch_issue_states_by_ids(&[issue_id.to_string()])
            .await?;
        let Some(mut issue) = refreshed.pop() else {
            return Err(anyhow::anyhow!(
                "blocked issue disappeared before annotation"
            ));
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
    ) -> Result<Issue> {
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

    async fn persist_human_input_request(
        &self,
        snapshot: &WorkflowSnapshot,
        issue_id: &str,
        workspace_path: &Path,
        request: HumanInputRequest,
    ) -> Result<Issue> {
        let mut pending = PendingHumanInput {
            request,
            status: PendingHumanInputStatus::AwaitingAnswer,
            resume_transition_applied: false,
            prompt_comment_id: None,
            prompt_comment_url: None,
            answered_comment_id: None,
            answered_by_login: None,
            answered_at: None,
            answers: Vec::new(),
        };
        workspace::write_pending_human_input(workspace_path, &pending)?;
        let mut refreshed = self
            .tracker
            .fetch_issue_states_by_ids(&[issue_id.to_string()])
            .await?;
        let Some(mut issue) = refreshed.pop() else {
            return Err(anyhow::anyhow!(
                "human input issue disappeared before annotation"
            ));
        };

        self.ensure_human_input_prompt(&issue, workspace_path, &mut pending)
            .await?;

        if issue.workpad_comment_id.is_some() {
            issue = self.tracker.refresh_workpad_comment(&issue).await?;
        }
        let base_body = issue.workpad_comment_body.clone().unwrap_or_else(|| {
            render_blocked_failure_workpad(
                pending.request.provider.as_str(),
                Some(workspace_path),
                &issue,
            )
        });
        let blocker_section = render_blocker_section(
            pending.request.provider.as_str(),
            None,
            pending.request.session_id.as_deref(),
            &format!("waiting_for_user_input: {}", pending.request.request_id),
            "Reply to the pending Kairastra GitHub comment with the requested answers, then move the issue back to an active state if it does not transition automatically.",
        );
        let merged_body = merge_blocker_section(&base_body, &blocker_section);
        issue = self
            .tracker
            .update_workpad_comment(&issue, &merged_body)
            .await?;

        if snapshot.settings.active_state(&issue.state)
            && !snapshot.settings.terminal_state(&issue.state)
        {
            issue = self
                .tracker
                .transition_issue_to_human_review(&issue)
                .await?;
        }

        warn!(
            issue_identifier = %issue.identifier,
            request_id = %pending.request.request_id,
            state = %issue.state,
            "issue waiting for human input; posted GitHub prompt comment"
        );
        Ok(issue)
    }

    async fn reconcile_pending_human_inputs(
        &self,
        snapshot: &WorkflowSnapshot,
        state: &mut RuntimeState,
        issues: Vec<Issue>,
    ) -> Result<Vec<Issue>> {
        let mut reconciled = Vec::with_capacity(issues.len());

        for issue in issues {
            match self
                .reconcile_pending_human_input(snapshot, state, issue.clone())
                .await
            {
                Ok(issue) => reconciled.push(issue),
                Err(error) => {
                    warn!(issue_identifier = %issue.identifier, error = ?error,
                        "pending input could not be reconciled; preserving state and blocking this issue");
                    hold_blocked_claim(state, &issue);
                    reconciled.push(issue);
                }
            }
        }
        Ok(reconciled)
    }

    async fn ensure_human_input_prompt(
        &self,
        issue: &Issue,
        workspace_path: &Path,
        pending: &mut PendingHumanInput,
    ) -> Result<()> {
        if pending.prompt_comment_id.is_some() {
            return Ok(());
        }
        let body = render_human_input_prompt_comment(&pending.request);
        let existing = self
            .tracker
            .list_issue_comments(issue)
            .await?
            .into_iter()
            .find(|comment| comment.body == body);
        let comment = match existing {
            Some(comment) => comment,
            None => self.tracker.create_issue_comment(issue, &body).await?,
        };
        pending.prompt_comment_id = Some(comment.id);
        pending.prompt_comment_url = comment.html_url;
        workspace::write_pending_human_input(workspace_path, pending)
    }

    async fn reconcile_pending_human_input(
        &self,
        snapshot: &WorkflowSnapshot,
        state: &mut RuntimeState,
        mut issue: Issue,
    ) -> Result<Issue> {
        let workspace_path = match workspace::workspace_path(&snapshot.settings, &issue.identifier)
        {
            Ok(path) => path,
            Err(_) => {
                return Ok(issue);
            }
        };

        let Some(mut pending) = workspace::load_pending_human_input(&workspace_path)? else {
            return Ok(issue);
        };

        match pending.status {
            PendingHumanInputStatus::AwaitingAnswer => {
                self.ensure_human_input_prompt(&issue, &workspace_path, &mut pending)
                    .await?;

                let comments = self.tracker.list_issue_comments(&issue).await?;
                if let Some((comment, answers)) =
                    find_human_input_answer(&pending.request, &comments)
                {
                    pending.status = PendingHumanInputStatus::Answered;
                    pending.answered_comment_id = Some(comment.id);
                    pending.answered_by_login =
                        comment.user.as_ref().and_then(|user| user.login.clone());
                    pending.answered_at = comment.updated_at.or(comment.created_at);
                    pending.answers = answers;
                    workspace::write_pending_human_input(&workspace_path, &pending)?;
                    let _ = self
                        .tracker
                        .create_issue_comment(&issue, &render_human_input_ack_comment(&pending))
                        .await;
                }
            }
            PendingHumanInputStatus::Answered => {}
        }

        if pending.status == PendingHumanInputStatus::Answered {
            clear_blocked_claim(state, &issue.id);
            if !pending.resume_transition_applied {
                if snapshot
                    .settings
                    .tracker
                    .human_review_state
                    .as_deref()
                    .is_some_and(|waiting| {
                        normalize_issue_state(waiting) == normalize_issue_state(&issue.state)
                    })
                {
                    issue = self.tracker.transition_issue_to_in_progress(&issue).await?;
                }
                pending.resume_transition_applied = true;
                workspace::write_pending_human_input(&workspace_path, &pending)?;
            }
        } else {
            hold_blocked_claim(state, &issue);
        }

        Ok(issue)
    }

    async fn persist_issue_blocker(
        &self,
        snapshot: &WorkflowSnapshot,
        issue: &mut Issue,
        context: BlockedIssueContext<'_>,
    ) -> Result<Issue> {
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
            *issue = self.tracker.transition_issue_to_human_review(issue).await?;
        }

        warn!(
            issue_identifier = %context.identifier,
            provider = context.provider,
            action = %context.blocked.operator_action,
            state = %issue.state,
            workpad_comment = issue.workpad_comment_url.as_deref().unwrap_or("unavailable"),
            "issue blocked; recorded blocker details on the issue"
        );
        Ok(issue.clone())
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

fn log_provider_event(provider: &str, issue_identifier: &str, event: &AgentEvent) {
    if provider.eq_ignore_ascii_case("codex") {
        log_codex_event(issue_identifier, event);
    } else {
        log_generic_agent_event(provider, issue_identifier, event);
    }
}

fn log_generic_agent_event(provider: &str, issue_identifier: &str, event: &AgentEvent) {
    match event.event {
        AgentEventKind::SessionStarted => {
            let session_id = event
                .payload
                .get("session_id")
                .and_then(JsonValue::as_str)
                .or(event.session_id.as_deref())
                .unwrap_or("unknown");
            info!(
                issue_identifier = %issue_identifier,
                provider,
                session_id,
                "agent session started"
            );
        }
        AgentEventKind::Notification => {
            log_generic_notification(provider, issue_identifier, &event.payload)
        }
        AgentEventKind::TurnCompleted => {
            if let Some(usage) = extract_token_usage(&event.payload) {
                info!(
                    issue_identifier = %issue_identifier,
                    provider,
                    input_tokens = usage.input_tokens,
                    output_tokens = usage.output_tokens,
                    total_tokens = usage.total_tokens,
                    "agent turn completed"
                );
            } else {
                info!(
                    issue_identifier = %issue_identifier,
                    provider,
                    "agent turn completed"
                );
            }
        }
        AgentEventKind::ApprovalRequired => {
            warn!(
                issue_identifier = %issue_identifier,
                provider,
                payload = %event.payload,
                "agent approval required"
            );
        }
        AgentEventKind::TurnInputRequired => {
            warn!(
                issue_identifier = %issue_identifier,
                provider,
                payload = %event.payload,
                "agent turn input required"
            );
        }
        AgentEventKind::TurnFailed
        | AgentEventKind::TurnCancelled
        | AgentEventKind::TurnEndedWithError => {
            warn!(
                issue_identifier = %issue_identifier,
                provider,
                payload = %event.payload,
                "agent turn ended with error"
            );
        }
        AgentEventKind::ToolCallCompleted => {
            let (tool_name, tool_id) = tool_log_fields(&event.payload);
            info!(
                issue_identifier = %issue_identifier,
                provider,
                tool_name = tool_name.unwrap_or("unknown"),
                tool_id = tool_id.unwrap_or("unknown"),
                "agent tool completed"
            );
        }
        AgentEventKind::ToolCallFailed => {
            let (tool_name, tool_id) = tool_log_fields(&event.payload);
            warn!(
                issue_identifier = %issue_identifier,
                provider,
                tool_name = tool_name.unwrap_or("unknown"),
                tool_id = tool_id.unwrap_or("unknown"),
                payload = %event.payload,
                "agent tool failed"
            );
        }
        AgentEventKind::UnsupportedToolCall => {
            warn!(
                issue_identifier = %issue_identifier,
                provider,
                payload = %event.payload,
                "agent requested unsupported tool"
            );
        }
        AgentEventKind::UserInputRequested => {
            info!(
                issue_identifier = %issue_identifier,
                provider,
                payload = %event.payload,
                "agent requested structured user input"
            );
        }
        AgentEventKind::ToolInputAutoAnswered => {}
        AgentEventKind::Malformed => {
            warn!(
                issue_identifier = %issue_identifier,
                provider,
                payload = %event.payload,
                "agent emitted malformed payload"
            );
        }
        AgentEventKind::ApprovalAutoApproved | AgentEventKind::OtherMessage => {}
    }
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
        AgentEventKind::UserInputRequested => {
            info!(
                issue_identifier = %issue_identifier,
                payload = %event.payload,
                "codex requested structured user input"
            );
        }
        AgentEventKind::ToolInputAutoAnswered => {}
        AgentEventKind::Notification => log_codex_notification(issue_identifier, &event.payload),
        _ => {}
    }
}

fn log_generic_notification(provider: &str, issue_identifier: &str, payload: &serde_json::Value) {
    let (tool_name, tool_id) = tool_log_fields(payload);
    let Some(tool_name) = tool_name else {
        return;
    };

    info!(
        issue_identifier = %issue_identifier,
        provider,
        tool_name,
        tool_id = tool_id.unwrap_or("unknown"),
        "agent tool started"
    );
}

fn render_human_input_prompt_comment(request: &HumanInputRequest) -> String {
    let mut body = String::new();
    body.push_str("Kairastra is waiting for structured input before it can continue.\n\n");
    body.push_str(&format!("- Request ID: `{}`\n", request.request_id));
    body.push_str("- Reply using this exact template:\n\n");
    body.push_str("```text\n");
    body.push_str(&format!("/kairastra answer {}\n", request.request_id));
    for question in &request.questions {
        let placeholder = if question.options.is_empty() {
            "<answer>"
        } else {
            "<option>"
        };
        body.push_str(&format!("{}: {placeholder}\n", question.id));
        if question.is_other || question.options.iter().any(|option| option.is_other) {
            body.push_str(&format!("{}_other: <free text>\n", question.id));
        }
    }
    body.push_str("```\n\n");
    for question in &request.questions {
        body.push_str(&format!("**{}**\n", question.question));
        if !question.options.is_empty() {
            for (index, option) in question.options.iter().enumerate() {
                let code = human_input_option_code(index);
                body.push_str(&format!("- {}. `{}`", code, option.label));
                if let Some(description) = option.description.as_deref() {
                    body.push_str(&format!(": {}", description));
                }
                if option.is_other {
                    body.push_str(" (allows free text)");
                }
                body.push('\n');
            }
            if question.is_other {
                body.push_str(&format!(
                    "- {}. `Other` (allows free text)\n",
                    human_input_option_code(question.options.len())
                ));
            }
        }
        body.push('\n');
    }
    body
}

fn render_human_input_ack_comment(pending: &PendingHumanInput) -> String {
    format!(
        "Captured answers for request `{}`. Kairastra will resume on the next run.",
        pending.request.request_id
    )
}

fn find_human_input_answer(
    request: &HumanInputRequest,
    comments: &[RestIssueComment],
) -> Option<(RestIssueComment, Vec<HumanInputAnswer>)> {
    comments
        .iter()
        .rev()
        .filter(|comment| human_input_comment_is_authorized(comment))
        .find_map(|comment| {
            parse_human_input_answer(request, comment).map(|answers| (comment.clone(), answers))
        })
}

fn human_input_comment_is_authorized(comment: &RestIssueComment) -> bool {
    matches!(
        comment.author_association.as_deref(),
        Some("OWNER") | Some("MEMBER") | Some("COLLABORATOR")
    )
}

fn finished_workspace_path<'a>(
    live_workspace_path: Option<&'a Path>,
    fallback_workspace_path: &'a Path,
) -> &'a Path {
    live_workspace_path.unwrap_or(fallback_workspace_path)
}

fn parse_human_input_answer(
    request: &HumanInputRequest,
    comment: &RestIssueComment,
) -> Option<Vec<HumanInputAnswer>> {
    let mut lines = comment
        .body
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("```"));

    let command = lines.next()?;
    if command != format!("/kairastra answer {}", request.request_id) {
        return None;
    }

    let fields = lines
        .filter_map(|line| {
            let (key, value) = line.split_once(':')?;
            Some((key.trim().to_string(), value.trim().to_string()))
        })
        .collect::<BTreeMap<_, _>>();

    let mut answers = Vec::with_capacity(request.questions.len());
    for question in &request.questions {
        let raw_answer = fields.get(&question.id)?.trim();
        let selected_option = if question.options.is_empty() {
            Some(raw_answer.to_string())
        } else {
            resolve_human_input_option(question, raw_answer)?
        };
        let mut other_text = fields
            .get(&format!("{}_other", question.id))
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        if question.is_other && selected_option.is_none() {
            if other_text.is_none()
                && !raw_answer.eq_ignore_ascii_case("Other")
                && !raw_answer
                    .eq_ignore_ascii_case(&human_input_option_code(question.options.len()))
            {
                other_text = Some(raw_answer.to_string()).filter(|text| !text.is_empty());
            }
            other_text.as_ref()?;
        }
        answers.push(HumanInputAnswer {
            question_id: question.id.clone(),
            selected_option,
            other_text,
        });
    }

    Some(answers)
}

fn resolve_human_input_option(
    question: &crate::model::HumanInputQuestion,
    raw_answer: &str,
) -> Option<Option<String>> {
    for (index, option) in question.options.iter().enumerate() {
        if raw_answer.eq_ignore_ascii_case(&human_input_option_code(index))
            || raw_answer == (index + 1).to_string()
            || raw_answer.eq_ignore_ascii_case(option.label.as_str())
        {
            return Some(Some(option.label.clone()));
        }
    }

    if question.is_other {
        Some(None)
    } else {
        None
    }
}

fn human_input_option_code(index: usize) -> String {
    let mut n = index + 1;
    let mut code = String::new();
    while n > 0 {
        let remainder = (n - 1) % 26;
        code.insert(0, (b'A' + remainder as u8) as char);
        n = (n - 1) / 26;
    }
    code
}

fn tool_log_fields(payload: &JsonValue) -> (Option<&str>, Option<&str>) {
    let tool_name = payload
        .get("tool_name")
        .and_then(JsonValue::as_str)
        .or_else(|| payload.get("name").and_then(JsonValue::as_str))
        .or_else(|| payload.get("tool").and_then(JsonValue::as_str));
    let tool_id = payload
        .get("tool_id")
        .and_then(JsonValue::as_str)
        .or_else(|| payload.get("tool_use_id").and_then(JsonValue::as_str))
        .or_else(|| payload.get("id").and_then(JsonValue::as_str));

    (tool_name, tool_id)
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
                    let command = extract_codex_command(item);
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
            let command = extract_codex_command(item);
            let tool = item
                .get("tool")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown");
            let server = item
                .get("server")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown");
            let exit_code = json_i64(
                item.get("exitCode")
                    .or_else(|| item.get("exit_code"))
                    .unwrap_or(&JsonValue::Null),
            )
            .map(|value| value.to_string())
            .unwrap_or_else(|| "unknown".to_string());
            let detail = codex_item_failure_excerpt(item);
            if codex_item_failure_is_expected_probe(&command, &exit_code, &detail) {
                debug!(
                    issue_identifier = %issue_identifier,
                    item_type,
                    status,
                    command,
                    tool,
                    server,
                    exit_code,
                    "codex probe completed without a match"
                );
            } else {
                warn!(
                    issue_identifier = %issue_identifier,
                    item_type,
                    status,
                    command,
                    tool,
                    server,
                    exit_code,
                    detail,
                    "codex item completed unsuccessfully"
                );
            }
        }
        "error" => {
            let error_type = payload
                .get("params")
                .and_then(|params| params.get("error"))
                .and_then(|error| error.get("type"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown");
            let detail = payload
                .get("params")
                .and_then(|params| params.get("error"))
                .and_then(json_value_excerpt)
                .unwrap_or_else(|| truncate_summary(&payload.to_string(), 240));
            warn!(
                issue_identifier = %issue_identifier,
                error_type,
                detail,
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
        AgentEventKind::UserInputRequested => "user_input_requested",
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
    truncate_utf8(value.trim(), limit)
}

fn truncate_utf8(value: &str, limit: usize) -> String {
    if value.len() <= limit {
        return value.to_string();
    }

    if limit <= 3 {
        return utf8_prefix(value, limit).to_string();
    }

    let mut truncated = utf8_prefix(value, limit.saturating_sub(3)).to_string();
    truncated.push_str("...");
    truncated
}

fn utf8_prefix(value: &str, max_bytes: usize) -> &str {
    if value.len() <= max_bytes {
        return value;
    }

    let mut end = 0;
    for (index, ch) in value.char_indices() {
        let next = index + ch.len_utf8();
        if next > max_bytes {
            break;
        }
        end = next;
    }

    &value[..end]
}

fn extract_codex_command(item: &JsonValue) -> String {
    if let Some(command) = item.get("command").and_then(serde_json::Value::as_str) {
        return truncate_summary(command, 240);
    }

    if let Some(command) = item
        .get("params")
        .and_then(|params| params.get("command"))
        .and_then(serde_json::Value::as_str)
    {
        return truncate_summary(command, 240);
    }

    if let Some(argv) = item.get("command").and_then(serde_json::Value::as_array) {
        let command = argv
            .iter()
            .filter_map(serde_json::Value::as_str)
            .collect::<Vec<_>>()
            .join(" ");
        return truncate_summary(&command, 240);
    }

    String::new()
}

fn codex_item_failure_excerpt(item: &JsonValue) -> String {
    item.get("aggregatedOutput")
        .or_else(|| item.get("aggregated_output"))
        .or_else(|| item.get("output"))
        .or_else(|| item.get("stderr"))
        .or_else(|| item.get("error"))
        .or_else(|| item.get("message"))
        .and_then(json_value_excerpt)
        .unwrap_or_default()
}

fn codex_item_failure_is_expected_probe(command: &str, exit_code: &str, detail: &str) -> bool {
    if exit_code != "1" || !detail.is_empty() {
        return false;
    }

    let lower = command.to_ascii_lowercase();
    lower.contains("test -e ")
        || lower.contains("test -d ")
        || lower.contains("test -f ")
        || lower.contains("rg ")
        || lower.contains("grep -q ")
}

fn json_value_excerpt(value: &JsonValue) -> Option<String> {
    match value {
        JsonValue::Null => None,
        JsonValue::String(text) => Some(truncate_summary(text, 240)),
        other => Some(truncate_summary(&other.to_string(), 240)),
    }
}

fn json_i64(value: &JsonValue) -> Option<i64> {
    match value {
        JsonValue::Number(number) => number.as_i64(),
        JsonValue::String(text) => text.trim().parse::<i64>().ok(),
        _ => None,
    }
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

fn classify_blocked_worker_failure(
    settings: &Settings,
    provider: &str,
    error: &str,
) -> Option<BlockedWorkerFailure> {
    let normalized = error.to_ascii_lowercase();
    let move_back_action = move_issue_back_instruction(settings);

    let operator_action = if normalized.contains("oauth token has expired")
        || normalized.contains("refresh your existing token")
        || normalized.contains("token_expired")
        || (normalized.contains("please obtain a new token")
            && normalized.contains("failed to authenticate"))
    {
        format!(
            "Refresh {} auth with `kairastra auth --provider {} login --mode subscription`, then {}.",
            provider_display_name(provider),
            provider,
            move_back_action
        )
    } else if normalized.contains("approval_required")
        || normalized.contains("requested permissions")
    {
        format!(
            "Grant the missing {} permissions or adjust the provider permission mode, then {}.",
            provider_display_name(provider),
            move_back_action
        )
    } else if normalized.contains("not logged in")
        || normalized.contains("authentication_failed")
        || normalized.contains("api key")
        || normalized.contains("credentials_present: false")
    {
        format!(
            "Configure {} auth in the runtime environment, then {}.",
            provider_display_name(provider),
            move_back_action
        )
    } else if normalized.contains("root/sudo privileges")
        || normalized.contains("dangerously-skip-permissions")
        || normalized.contains("bypasspermissions")
    {
        format!(
            "Run Claude in a non-root environment or change `providers.claude.permission_mode` / `approval_policy` so the runtime does not request bypass permissions, then {}.",
            move_back_action
        )
    } else if normalized.contains("failed to launch claude code")
        || normalized.contains("failed to launch codex app-server")
        || normalized.contains("failed to launch gemini cli")
        || normalized.contains("no such file or directory")
        || normalized.contains("command not found")
    {
        format!(
            "Install the {} runtime in the worker environment and verify it is available on `PATH`, then {}.",
            provider_display_name(provider),
            move_back_action
        )
    } else if normalized.contains("invalid_workflow_config")
        || normalized.contains("unsupported_agent_provider")
    {
        format!(
            "Fix the Kairastra workflow/provider configuration, then {}.",
            move_back_action
        )
    } else {
        return None;
    };

    Some(BlockedWorkerFailure { operator_action })
}

fn classify_provider_selection_block(
    settings: &Settings,
    issue: &Issue,
    warning: &str,
) -> Option<(String, String, BlockedWorkerFailure)> {
    let move_back_action = move_issue_back_instruction(settings);

    if warning == "invalid_issue_agent_label" {
        return Some((
            "unknown".to_string(),
            "Issue has an invalid `agent:` label.".to_string(),
            BlockedWorkerFailure {
                operator_action: format!(
                    "Replace the invalid `agent:` label with exactly one supported provider label, then {}.",
                    move_back_action
                ),
            },
        ));
    }

    if warning == "multiple_issue_agent_labels" {
        return Some((
            "unknown".to_string(),
            "Issue has multiple `agent:` labels.".to_string(),
            BlockedWorkerFailure {
                operator_action: format!(
                    "Leave exactly one `agent:` label on the issue, then {}.",
                    move_back_action
                ),
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
                    "Configure {display_name} in the active workflow/runtime or remove the `agent:{provider}` label, then {}.",
                    move_back_action
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
            operator_action: format!(
                "Fix the issue's provider selection or workflow configuration, then {}.",
                move_back_action
            ),
        },
    ))
}

fn move_issue_back_instruction(settings: &Settings) -> String {
    let mut states = Vec::new();
    let mut seen = HashSet::new();

    for state in &settings.tracker.claimable_states {
        let normalized = normalize_issue_state(state);
        if seen.insert(normalized) {
            states.push(state.as_str());
        }
    }

    if let Some(state) = settings.tracker.in_progress_state.as_deref() {
        let normalized = normalize_issue_state(state);
        if seen.insert(normalized) {
            states.push(state);
        }
    }

    if states.is_empty() {
        return "move the issue into a configured active state".to_string();
    }

    format!(
        "move the issue back to {}",
        format_inline_state_list(&states)
    )
}

fn format_inline_state_list(states: &[&str]) -> String {
    match states {
        [] => "a configured active state".to_string(),
        [state] => format!("`{state}`"),
        [first, second] => format!("`{first}` or `{second}`"),
        _ => {
            let mut rendered = states[..states.len() - 1]
                .iter()
                .map(|state| format!("`{state}`"))
                .collect::<Vec<_>>()
                .join(", ");
            rendered.push_str(", or ");
            rendered.push_str(&format!("`{}`", states[states.len() - 1]));
            rendered
        }
    }
}

fn provider_display_name(provider: &str) -> &'static str {
    match provider {
        "claude" => "Claude",
        "codex" => "Codex",
        "gemini" => "Gemini",
        _ => "provider",
    }
}

fn render_blocked_failure_workpad(
    provider: &str,
    _workspace_path: Option<&Path>,
    issue: &Issue,
) -> String {
    let header = providers::workpad_header(provider);
    let stamp = providers::workpad_environment_stamp("unknown-host", issue, "unknown");
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
    summary = truncate_utf8(&summary, 400);
    summary
}

fn render_worker_error_details(error: &str) -> String {
    let mut details = error.trim().to_string();
    if details.is_empty() {
        details = "No additional error details were captured.".to_string();
    }
    truncate_utf8(&details, 4_000)
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

fn summarize_idle_dispatch(
    snapshot: &WorkflowSnapshot,
    issues: &[Issue],
    state: &RuntimeState,
) -> Option<String> {
    if issues.is_empty() {
        return Some("no tracked issues matched the configured workflow".to_string());
    }

    let mut state_counts = BTreeMap::<String, usize>::new();
    let mut unknown_states = Vec::new();
    let mut blocked_claims = Vec::new();

    for issue in issues {
        *state_counts.entry(issue.state.clone()).or_default() += 1;

        if !is_known_issue_state(&snapshot.settings, &issue.state) {
            unknown_states.push(format!("{} ({})", issue.identifier, issue.state));
        }

        if state.blocked_claims.contains_key(&issue.id) {
            blocked_claims.push(issue.identifier.clone());
        }
    }

    let mut parts = vec![
        format!(
            "states={}",
            state_counts
                .into_iter()
                .map(|(state, count)| format!("{state}:{count}"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        format!(
            "claimable_states={}",
            snapshot.settings.tracker.claimable_states.join(", ")
        ),
    ];

    unknown_states.sort();
    if !unknown_states.is_empty() {
        parts.push(format!("unknown_states={}", unknown_states.join(", ")));
    }

    blocked_claims.sort();
    if !blocked_claims.is_empty() {
        parts.push(format!("blocked_claims={}", blocked_claims.join(", ")));
    }

    Some(parts.join(" | "))
}

fn running_state_counts(state: &RuntimeState) -> HashMap<String, usize> {
    let mut counts = HashMap::new();
    for running in state.running.values() {
        *counts
            .entry(normalize_issue_state(&running.issue.state))
            .or_default() += 1;
    }
    counts
}

fn dispatch_capacity_states(settings: &Settings, issue: &Issue) -> Vec<String> {
    let mut states = vec![normalize_issue_state(&issue.state)];
    if settings.claimable_state(&issue.state) {
        if let Some(target) = settings.tracker.in_progress_state.as_deref() {
            let target = normalize_issue_state(target);
            if !states.contains(&target) {
                states.push(target);
            }
        }
    }
    states
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
        let capacity_states = dispatch_capacity_states(&snapshot.settings, &issue);
        if capacity_states.iter().any(|state_name| {
            per_state_counts.get(state_name).copied().unwrap_or(0)
                >= snapshot
                    .settings
                    .max_concurrent_agents_for_state(state_name)
        }) {
            continue;
        }
        for state_name in capacity_states {
            *per_state_counts.entry(state_name).or_default() += 1;
        }
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
    issue_eligible_now(snapshot, issue, state, per_state_counts, false)
}

fn issue_eligible_now(
    snapshot: &WorkflowSnapshot,
    issue: &Issue,
    state: &RuntimeState,
    per_state_counts: &HashMap<String, usize>,
    allow_queued_retry: bool,
) -> bool {
    issue_meets_dispatch_filters(snapshot, issue, state, allow_queued_retry)
        && has_dispatch_capacity(snapshot, issue, per_state_counts)
}

fn issue_meets_dispatch_filters(
    snapshot: &WorkflowSnapshot,
    issue: &Issue,
    state: &RuntimeState,
    allow_queued_retry: bool,
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

    if state.claimed.contains(&issue.id)
        || state.running.contains_key(&issue.id)
        || (!allow_queued_retry && state.retry_attempts.contains_key(&issue.id))
    {
        trace!(issue_identifier = %issue.identifier, "skipping: already claimed, running or retrying");
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

    if snapshot.settings.claimable_state(&issue.state)
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

    true
}

fn has_dispatch_capacity(
    snapshot: &WorkflowSnapshot,
    issue: &Issue,
    per_state_counts: &HashMap<String, usize>,
) -> bool {
    dispatch_capacity_states(&snapshot.settings, issue)
        .iter()
        .all(|state_name| {
            per_state_counts.get(state_name).copied().unwrap_or(0)
                < snapshot
                    .settings
                    .max_concurrent_agents_for_state(state_name)
        })
}

fn should_hold_blocked_claim(settings: &Settings, issue: &Issue) -> bool {
    settings.active_state(&issue.state) && !settings.terminal_state(&issue.state)
}

fn is_known_issue_state(settings: &Settings, state: &str) -> bool {
    let normalized = normalize_issue_state(state);
    settings.active_state(state)
        || settings.terminal_state(state)
        || settings
            .tracker
            .human_review_state
            .as_deref()
            .map(normalize_issue_state)
            .is_some_and(|configured| configured == normalized)
        || settings
            .tracker
            .in_progress_state
            .as_deref()
            .map(normalize_issue_state)
            .is_some_and(|configured| configured == normalized)
        || settings
            .tracker
            .done_state
            .as_deref()
            .map(normalize_issue_state)
            .is_some_and(|configured| configured == normalized)
}

fn hold_blocked_claim(state: &mut RuntimeState, issue: &Issue) {
    state.claimed.insert(issue.id.clone());
    state
        .blocked_claims
        .insert(issue.id.clone(), normalize_issue_state(&issue.state));
}

fn clear_blocked_claim(state: &mut RuntimeState, issue_id: &str) {
    state.blocked_claims.remove(issue_id);
    state.claimed.remove(issue_id);
}

fn resolve_human_input_request(
    explicit_request: Option<HumanInputRequest>,
    cached_request: Option<HumanInputRequest>,
) -> Option<HumanInputRequest> {
    explicit_request.or(cached_request)
}

fn should_release_blocked_claim(settings: &Settings, previous_state: &str, issue: &Issue) -> bool {
    !settings.active_state(&issue.state) || normalize_issue_state(&issue.state) != previous_state
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

    use crate::agent::events::{AgentEvent, AgentEventKind};
    use crate::config::Settings;
    use crate::github::{RestIssueComment, RestIssueCommentUser};
    use crate::model::{
        BlockerRef, HumanInputOption, HumanInputQuestion, HumanInputRequest, Issue,
        WorkflowDefinition,
    };

    use super::{
        classify_blocked_worker_failure, classify_provider_selection_block,
        codex_item_failure_is_expected_probe, extract_codex_command, extract_rate_limits,
        extract_token_usage, find_human_input_answer, finished_workspace_path, hold_blocked_claim,
        issue_eligible, issue_provider, issue_sort_key, merge_blocker_section,
        parse_human_input_answer, render_blocked_failure_workpad,
        render_human_input_prompt_comment, resolve_human_input_request, select_dispatchable,
        should_release_blocked_claim, summarize_agent_event, summarize_idle_dispatch,
        summarize_worker_error, tool_log_fields, truncate_summary, usage_delta,
        IssueProviderSelection, RuntimeState, TokenUsage, BLOCKER_SECTION_END,
        BLOCKER_SECTION_START,
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

    fn snapshot_with_custom_status_mapping() -> WorkflowSnapshot {
        let workflow = WorkflowDefinition {
            config: serde_yaml::from_str(
                r#"
tracker:
  kind: github
  owner: openai
  project_v2_number: 7
  api_key: fake
  active_states: ["Ready", "Doing", "Needs Review"]
  terminal_states: ["Complete", "Closed"]
  claimable_states: ["Ready"]
  in_progress_state: Doing
  human_review_state: "Needs Review"
  done_state: Complete
agent:
  provider: codex
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

    fn human_input_request() -> HumanInputRequest {
        HumanInputRequest {
            request_id: "input-123".to_string(),
            provider: "codex".to_string(),
            issue_id: "1".to_string(),
            issue_identifier: "openai/repo#1".to_string(),
            session_id: Some("thread-1-turn-1".to_string()),
            requested_at: chrono::Utc::now(),
            questions: vec![HumanInputQuestion {
                id: "scope".to_string(),
                header: Some("Scope".to_string()),
                question: "What scope should we implement?".to_string(),
                is_other: false,
                options: vec![
                    HumanInputOption {
                        label: "Core only".to_string(),
                        description: Some("Ship the minimum path.".to_string()),
                        is_other: false,
                    },
                    HumanInputOption {
                        label: "Full flow".to_string(),
                        description: Some("Include all related plumbing.".to_string()),
                        is_other: true,
                    },
                ],
            }],
        }
    }

    fn answer_comment(body: &str, association: &str, id: u64) -> RestIssueComment {
        RestIssueComment {
            id,
            body: body.to_string(),
            html_url: Some(format!(
                "https://github.com/openai/repo/issues/1#issuecomment-{id}"
            )),
            user: Some(RestIssueCommentUser {
                login: Some("dbachko".to_string()),
            }),
            author_association: Some(association.to_string()),
            created_at: None,
            updated_at: None,
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
    fn blocks_custom_claimable_issues_with_active_blockers() {
        let workflow = WorkflowDefinition {
            config: serde_yaml::from_str(
                r#"
tracker:
  kind: github
  owner: openai
  project_v2_number: 7
  api_key: fake
  active_states: ["Ready", "Doing"]
  terminal_states: ["Done", "Closed"]
  claimable_states: ["Ready"]
agent:
  provider: codex
providers:
  codex: {}
"#,
            )
            .unwrap(),
            prompt_template: String::new(),
        };
        let snapshot = WorkflowSnapshot {
            settings: Settings::from_workflow(&workflow).unwrap(),
            definition: workflow,
        };
        let mut issue = issue("1", "Ready", Some(1));
        issue.blocked_by.push(BlockerRef {
            id: Some("b1".to_string()),
            identifier: Some("B-1".to_string()),
            state: Some("Doing".to_string()),
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
    fn claim_selection_reserves_destination_state_capacity() {
        let mut snapshot = snapshot();
        snapshot
            .settings
            .agent
            .max_concurrent_agents_by_state
            .clear();
        snapshot
            .settings
            .agent
            .max_concurrent_agents_by_state
            .insert("in progress".into(), 1);
        let tasks = (1..=4)
            .map(|id| issue(&id.to_string(), "Todo", None))
            .collect::<Vec<_>>();
        assert_eq!(
            select_dispatchable(&snapshot, &tasks, &RuntimeState::default()).len(),
            1
        );
    }

    fn orchestrator_for(snapshot: &WorkflowSnapshot) -> super::Orchestrator {
        super::Orchestrator::new(
            std::sync::Arc::new(crate::workflow::WorkflowStore::new("unused".into())),
            std::sync::Arc::new(
                crate::github::GitHubTracker::new(snapshot.settings.tracker.clone()).unwrap(),
            ),
        )
    }

    fn pending_input() -> crate::model::PendingHumanInput {
        crate::model::PendingHumanInput {
            request: human_input_request(),
            status: crate::model::PendingHumanInputStatus::AwaitingAnswer,
            resume_transition_applied: false,
            prompt_comment_id: None,
            prompt_comment_url: None,
            answered_comment_id: None,
            answered_by_login: None,
            answered_at: None,
            answers: Vec::new(),
        }
    }

    #[tokio::test]
    async fn corrupt_pending_input_blocks_only_its_own_issue() {
        let dir = tempfile::tempdir().unwrap();
        let mut snapshot = snapshot();
        snapshot.settings.workspace.root = dir.path().into();
        let corrupt = issue("1", "In Progress", None);
        let other = issue("2", "In Progress", None);
        let workspace =
            crate::workspace::workspace_path(&snapshot.settings, &corrupt.identifier).unwrap();
        std::fs::create_dir_all(workspace.join(".kairastra")).unwrap();
        let path = crate::workspace::pending_human_input_path(&workspace);
        std::fs::write(&path, "{truncated").unwrap();
        let mut state = RuntimeState::default();
        let issues = orchestrator_for(&snapshot)
            .reconcile_pending_human_inputs(&snapshot, &mut state, vec![corrupt, other])
            .await
            .unwrap();
        let selected = select_dispatchable(&snapshot, &issues, &state);
        assert_eq!(
            selected
                .iter()
                .map(|issue| issue.id.as_str())
                .collect::<Vec<_>>(),
            ["2"]
        );
        assert_eq!(std::fs::read_to_string(path).unwrap(), "{truncated");
    }

    #[tokio::test]
    async fn answered_input_does_not_reverse_backlog_pause() {
        let dir = tempfile::tempdir().unwrap();
        let mut snapshot = snapshot();
        snapshot.settings.workspace.root = dir.path().into();
        let task = issue("1", "Backlog", None);
        let workspace =
            crate::workspace::workspace_path(&snapshot.settings, &task.identifier).unwrap();
        let mut pending = pending_input();
        pending.status = crate::model::PendingHumanInputStatus::Answered;
        crate::workspace::write_pending_human_input(&workspace, &pending).unwrap();
        let mut state = RuntimeState::default();
        let result = orchestrator_for(&snapshot)
            .reconcile_pending_human_input(&snapshot, &mut state, task)
            .await
            .unwrap();
        assert_eq!(result.state, "Backlog");
        assert!(
            crate::workspace::load_pending_human_input(&workspace)
                .unwrap()
                .unwrap()
                .resume_transition_applied
        );
    }

    #[tokio::test]
    async fn human_input_is_saved_before_tracker_refresh() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/openai/kairastra/issues"))
            .respond_with(ResponseTemplate::new(503).set_body_json(json!({"message":"offline"})))
            .mount(&server)
            .await;
        let mut snapshot = snapshot();
        snapshot.settings.tracker.mode = crate::config::GitHubMode::IssuesOnly;
        snapshot.settings.tracker.repo = Some("kairastra".into());
        snapshot.settings.tracker.rest_endpoint = server.uri();
        let dir = tempfile::tempdir().unwrap();
        let request = human_input_request();
        assert!(orchestrator_for(&snapshot)
            .persist_human_input_request(&snapshot, "1", dir.path(), request.clone())
            .await
            .is_err());
        let saved = crate::workspace::load_pending_human_input(dir.path())
            .unwrap()
            .unwrap();
        assert_eq!(saved.request, request);
        assert!(saved.prompt_comment_id.is_none());
    }

    #[tokio::test]
    async fn human_input_prompt_recovery_reuses_an_accepted_post() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        let mut pending = pending_input();
        let body = render_human_input_prompt_comment(&pending.request);
        Mock::given(method("GET"))
            .and(path("/repos/openai/repo/issues/1/comments"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([{"id":42,"body":body}])))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;
        let mut snapshot = snapshot();
        snapshot.settings.tracker.rest_endpoint = server.uri();
        let dir = tempfile::tempdir().unwrap();
        let mut task = issue("1", "Human Review", None);
        task.identifier = "openai/repo#1".into();
        orchestrator_for(&snapshot)
            .ensure_human_input_prompt(&task, dir.path(), &mut pending)
            .await
            .unwrap();
        assert_eq!(pending.prompt_comment_id, Some(42));
    }

    #[test]
    fn question_level_other_allows_custom_answers() {
        let mut request = human_input_request();
        request.questions[0].is_other = true;
        request.questions[0]
            .options
            .iter_mut()
            .for_each(|option| option.is_other = false);
        assert!(render_human_input_prompt_comment(&request).contains("C. `Other`"));
        let comment = answer_comment(
            "/kairastra answer input-123\nscope: Other\nscope_other: custom scope",
            "OWNER",
            1,
        );
        let answers = parse_human_input_answer(&request, &comment).unwrap();
        assert_eq!(answers[0].other_text.as_deref(), Some("custom scope"));
        assert_eq!(answers[0].selected_option, None);
        let empty = answer_comment("/kairastra answer input-123\nscope: Other", "OWNER", 2);
        assert!(parse_human_input_answer(&request, &empty).is_none());
    }

    #[tokio::test]
    async fn reload_updates_tracker_auth_and_rejects_repository_switch() {
        use wiremock::matchers::{header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/probe"))
            .and(header("authorization", "Bearer rotated-test-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok":true})))
            .mount(&server)
            .await;
        let mut current = snapshot();
        current.settings.tracker.rest_endpoint = server.uri();
        let mut runtime = orchestrator_for(&current);
        let mut changed = current.clone();
        changed.settings.tracker.api_key = "rotated-test-token".into();
        changed.settings.tracker.human_review_state = Some("Needs Review".into());
        runtime.apply_snapshot(&mut current, changed).unwrap();
        assert_eq!(
            runtime.tracker.settings().human_review_state.as_deref(),
            Some("Needs Review")
        );
        runtime
            .tracker
            .rest_json(reqwest::Method::GET, "/probe", None)
            .await
            .unwrap();
        let previous = current.settings.tracker.clone();
        let mut changed = current.clone();
        changed.settings.tracker.repo = Some("different".into());
        assert!(runtime.apply_snapshot(&mut current, changed).is_err());
        assert_eq!(current.settings.tracker, previous);
        assert_eq!(runtime.tracker.settings(), &previous);
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
        let snapshot = snapshot();
        let blocked = classify_blocked_worker_failure(
            &snapshot.settings,
            "claude",
            "Not logged in · Please run /login; process_exited=exit status: 1",
        )
        .expect("Claude auth failures should block");

        assert!(blocked.operator_action.contains("Configure Claude auth"));
    }

    #[test]
    fn classifies_expired_claude_oauth_as_blocked() {
        let snapshot = snapshot();
        let blocked = classify_blocked_worker_failure(
            &snapshot.settings,
            "claude",
            r#"Failed to authenticate. API Error: 401 {"type":"error","error":{"type":"authentication_error","message":"OAuth token has expired. Please obtain a new token or refresh your existing token."}}"#,
        )
        .expect("expired Claude OAuth should block");

        assert!(blocked
            .operator_action
            .contains("kairastra auth --provider claude login"));
    }

    #[test]
    fn generic_claude_auth_failures_do_not_use_expired_oauth_guidance() {
        let snapshot = snapshot();
        let blocked = classify_blocked_worker_failure(
            &snapshot.settings,
            "claude",
            "Failed to authenticate: invalid api key",
        )
        .expect("generic Claude auth failures should block");

        assert!(blocked.operator_action.contains("Configure Claude auth"));
        assert!(!blocked
            .operator_action
            .contains("kairastra auth --provider claude login"));
    }

    #[test]
    fn truncate_summary_keeps_valid_utf8() {
        let summary = truncate_summary("hello café こんにちは", 11);
        assert!(summary.starts_with("hello "));
        assert!(summary.ends_with("..."));
        assert!(std::str::from_utf8(summary.as_bytes()).is_ok());
    }

    #[test]
    fn summarize_agent_event_handles_multibyte_payloads() {
        let event = AgentEvent {
            event: AgentEventKind::Notification,
            timestamp: chrono::Utc::now(),
            payload: json!({
                "method": "item/completed",
                "params": {
                    "item": {
                        "aggregatedOutput": "こんにちは".repeat(40)
                    }
                }
            }),
            session_id: None,
            agent_process_pid: None,
        };

        let summary = summarize_agent_event(&event);
        assert!(std::str::from_utf8(summary.as_bytes()).is_ok());
        assert!(summary.contains("notification (item/completed)"));
    }

    #[test]
    fn summarize_worker_error_handles_multibyte_text() {
        let error = "mañana ".repeat(200);
        let summary = summarize_worker_error(&error);
        assert!(std::str::from_utf8(summary.as_bytes()).is_ok());
        assert!(summary.ends_with("..."));
    }

    #[test]
    fn renders_human_input_prompt_comment_with_template() {
        let rendered = render_human_input_prompt_comment(&human_input_request());
        assert!(rendered.contains("/kairastra answer input-123"));
        assert!(rendered.contains("scope: <option>"));
        assert!(rendered.contains("scope_other: <free text>"));
        assert!(rendered.contains("A. `Core only`"));
        assert!(rendered.contains("B. `Full flow`"));
    }

    #[test]
    fn renders_human_input_prompt_with_answer_placeholder_for_free_text() {
        let mut request = human_input_request();
        request.questions[0].options.clear();

        let rendered = render_human_input_prompt_comment(&request);

        assert!(rendered.contains("scope: <answer>"));
        assert!(!rendered.contains("scope: <option>"));
    }

    #[test]
    fn parses_human_input_answer_comment() {
        let request = human_input_request();
        let comment = answer_comment(
            "/kairastra answer input-123\nscope: B\nscope_other: include migrations",
            "MEMBER",
            17,
        );

        let answers = parse_human_input_answer(&request, &comment).expect("answer should parse");
        assert_eq!(answers.len(), 1);
        assert_eq!(answers[0].question_id, "scope");
        assert_eq!(answers[0].selected_option.as_deref(), Some("Full flow"));
        assert_eq!(answers[0].other_text.as_deref(), Some("include migrations"));
    }

    #[test]
    fn parses_human_input_answer_inside_fenced_block() {
        let request = human_input_request();
        let comment = answer_comment(
            "```text\n/kairastra answer input-123\nscope: A\n```",
            "MEMBER",
            17,
        );

        let answers = parse_human_input_answer(&request, &comment).expect("answer should parse");
        assert_eq!(answers[0].selected_option.as_deref(), Some("Core only"));
    }

    #[test]
    fn human_input_options_support_more_than_three_choices() {
        let mut request = human_input_request();
        request.questions[0].options.extend([
            HumanInputOption {
                label: "Docs only".to_string(),
                description: None,
                is_other: false,
            },
            HumanInputOption {
                label: "Full product".to_string(),
                description: None,
                is_other: false,
            },
        ]);

        let rendered = render_human_input_prompt_comment(&request);
        assert!(rendered.contains("D. `Full product`"));

        let comment = answer_comment("/kairastra answer input-123\nscope: D", "MEMBER", 17);
        let answers = parse_human_input_answer(&request, &comment).expect("answer should parse");
        assert_eq!(answers[0].selected_option.as_deref(), Some("Full product"));

        let comment = answer_comment("/kairastra answer input-123\nscope: 4", "MEMBER", 18);
        let answers = parse_human_input_answer(&request, &comment).expect("answer should parse");
        assert_eq!(answers[0].selected_option.as_deref(), Some("Full product"));
    }

    #[test]
    fn finished_workspace_path_prefers_live_runtime_path() {
        let live = PathBuf::from("/tmp/live-workspace");
        let fallback = PathBuf::from("/tmp/spawn-hint");

        assert_eq!(
            finished_workspace_path(Some(&live), &fallback),
            live.as_path()
        );
        assert_eq!(finished_workspace_path(None, &fallback), fallback.as_path());
    }

    #[test]
    fn ignores_unauthorized_human_input_comments() {
        let request = human_input_request();
        let authorized = answer_comment("/kairastra answer input-123\nscope: A", "OWNER", 17);
        let unauthorized =
            answer_comment("/kairastra answer input-123\nscope: B", "CONTRIBUTOR", 18);

        let selected = find_human_input_answer(&request, &[authorized.clone(), unauthorized]);
        assert_eq!(selected.expect("authorized answer should win").0.id, 17);
    }

    #[test]
    fn explicit_human_input_request_wins_over_cached_request() {
        let explicit = human_input_request();
        let mut cached = human_input_request();
        cached.request_id = "input-stale".to_string();

        let resolved = resolve_human_input_request(Some(explicit.clone()), Some(cached))
            .expect("request should resolve");
        assert_eq!(resolved.request_id, explicit.request_id);
    }

    #[test]
    fn extracts_codex_command_from_string_or_array_payloads() {
        assert_eq!(
            extract_codex_command(&json!({"command": "/bin/bash -lc 'rg todo'"})),
            "/bin/bash -lc 'rg todo'"
        );
        assert_eq!(
            extract_codex_command(&json!({"command": ["rg", "--files"]})),
            "rg --files"
        );
        assert_eq!(
            extract_codex_command(&json!({"params": {"command": "gh pr view"}})),
            "gh pr view"
        );
    }

    #[test]
    fn downgrades_expected_probe_failures() {
        assert!(codex_item_failure_is_expected_probe(
            "/bin/bash -lc \"test -e packages/libs/database/dist && find packages/libs/database/dist -maxdepth 2 -type f | sed -n '1,40p'\"",
            "1",
            ""
        ));
        assert!(codex_item_failure_is_expected_probe(
            "/bin/bash -lc \"rg -n 'needle' packages/apps\"",
            "1",
            ""
        ));
        assert!(!codex_item_failure_is_expected_probe(
            "/bin/bash -lc 'ls packages/libs/database/dist'",
            "1",
            ""
        ));
        assert!(!codex_item_failure_is_expected_probe(
            "/bin/bash -lc \"rg -n 'needle' packages/apps\"",
            "2",
            ""
        ));
    }

    #[test]
    fn classifies_root_bypass_permission_failures_as_blocked() {
        let snapshot = snapshot();
        let blocked = classify_blocked_worker_failure(
            &snapshot.settings,
            "claude",
            "--dangerously-skip-permissions cannot be used with root/sudo privileges for security reasons",
        )
        .expect("root bypass permission failures should block");

        assert!(blocked.operator_action.contains("non-root"));
    }

    #[test]
    fn leaves_retryable_failures_unclassified() {
        let snapshot = snapshot();
        assert!(
            classify_blocked_worker_failure(&snapshot.settings, "claude", "turn_timeout").is_none()
        );
    }

    #[test]
    fn classifies_unconfigured_provider_labels_as_dispatch_blockers() {
        let snapshot = snapshot();
        let mut issue = issue("55", "Todo", Some(1));
        issue.labels = vec!["agent:claude".to_string()];

        let (provider, failure, blocked) = classify_provider_selection_block(
            &snapshot.settings,
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
        let snapshot = snapshot();
        let mut issue = issue("55", "Todo", Some(1));
        issue.labels = vec!["agent:claude".to_string(), "agent:codex".to_string()];

        let (provider, failure, blocked) = classify_provider_selection_block(
            &snapshot.settings,
            &issue,
            "multiple_issue_agent_labels",
        )
        .expect("multiple provider labels should be annotated");

        assert_eq!(provider, "unknown");
        assert!(failure.contains("multiple `agent:` labels"));
        assert!(blocked.operator_action.contains("exactly one"));
    }

    #[test]
    fn blocker_actions_reference_custom_requeue_states() {
        let snapshot = snapshot_with_custom_status_mapping();
        let blocked =
            classify_blocked_worker_failure(&snapshot.settings, "codex", "authentication_failed")
                .expect("auth failures should block");

        assert!(blocked.operator_action.contains("`Ready` or `Doing`"));
    }

    #[test]
    fn blocked_claims_release_after_state_change() {
        let snapshot = snapshot_with_custom_status_mapping();
        let original = issue("1", "Doing", Some(1));
        let changed = issue("1", "Ready", Some(1));

        assert!(should_release_blocked_claim(
            &snapshot.settings,
            "doing",
            &changed
        ));
        assert!(!should_release_blocked_claim(
            &snapshot.settings,
            "doing",
            &original
        ));
    }

    #[test]
    fn blocked_claims_make_issue_ineligible_until_released() {
        let snapshot = snapshot();
        let mut state = RuntimeState::default();
        let issue = issue("1", "Todo", Some(1));

        hold_blocked_claim(&mut state, &issue);

        assert!(!issue_eligible(&snapshot, &issue, &state, &HashMap::new()));
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
        assert!(body.contains("unknown-host:repo#55@unknown"));
        assert!(!body.contains("/workspaces/openai_repo_55"));
        assert!(body.contains("issues/55"));
    }

    #[test]
    fn idle_dispatch_summary_surfaces_unknown_issue_states() {
        let snapshot = snapshot();
        let issue = issue("3", "Open", Some(1));
        let state = RuntimeState::default();

        let summary = summarize_idle_dispatch(&snapshot, &[issue], &state).expect("summary");

        assert!(summary.contains("states=Open:1"));
        assert!(summary.contains("claimable_states=Todo"));
        assert!(summary.contains("unknown_states=openai/repo#3 (Open)"));
    }

    #[test]
    fn idle_dispatch_summary_does_not_flag_human_review_as_unknown() {
        let snapshot = snapshot();
        let issue = issue("3", "Human Review", Some(1));
        let state = RuntimeState::default();

        let summary = summarize_idle_dispatch(&snapshot, &[issue], &state).expect("summary");

        assert!(summary.contains("states=Human Review:1"));
        assert!(!summary.contains("unknown_states="));
    }

    #[test]
    fn tool_log_fields_support_gemini_payloads() {
        let payload = json!({
            "tool_name": "mcp_kairastra_github_github_rest",
            "tool_id": "tool-123"
        });

        let (tool_name, tool_id) = tool_log_fields(&payload);
        assert_eq!(tool_name, Some("mcp_kairastra_github_github_rest"));
        assert_eq!(tool_id, Some("tool-123"));
    }

    #[test]
    fn tool_log_fields_support_claude_payloads() {
        let payload = json!({
            "name": "github_rest",
            "tool_use_id": "toolu_abc"
        });

        let (tool_name, tool_id) = tool_log_fields(&payload);
        assert_eq!(tool_name, Some("github_rest"));
        assert_eq!(tool_id, Some("toolu_abc"));
    }

    #[test]
    fn review_retry_backoff_blocks_fresh_dispatch() {
        let snapshot = snapshot();
        let mut state = RuntimeState::default();
        let task = issue("1", "In Progress", Some(1));
        super::schedule_retry(
            &snapshot.settings,
            &mut state,
            task.id.clone(),
            task.identifier.clone(),
            6,
            false,
        );
        assert!(
            state.retry_attempts[&task.id].due_at
                > std::time::Instant::now() + std::time::Duration::from_secs(100)
        );
        assert!(
            select_dispatchable(&snapshot, &[task], &state).is_empty(),
            "a future retry must not be dispatched by a regular poll"
        );
    }

    #[tokio::test]
    async fn review_retry_survives_tracker_error() {
        use std::sync::Arc;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/openai/kairastra/issues"))
            .respond_with(
                ResponseTemplate::new(503)
                    .set_body_json(json!({"message":"temporarily unavailable"})),
            )
            .mount(&server)
            .await;
        let mut snapshot = snapshot();
        snapshot.settings.tracker.mode = crate::config::GitHubMode::IssuesOnly;
        snapshot.settings.tracker.repo = Some("kairastra".into());
        snapshot.settings.tracker.rest_endpoint = server.uri();
        let tracker =
            Arc::new(crate::github::GitHubTracker::new(snapshot.settings.tracker.clone()).unwrap());
        let store = Arc::new(crate::workflow::WorkflowStore::new(
            std::path::PathBuf::from("unused"),
        ));
        let orchestrator = super::Orchestrator::new(store, tracker);
        let mut state = RuntimeState::default();
        state.retry_attempts.insert(
            "1".into(),
            super::RetryEntry {
                attempt: 6,
                due_at: std::time::Instant::now(),
            },
        );
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        assert!(orchestrator
            .dispatch_due_retries(&snapshot, &mut state, &tx)
            .await
            .is_err());
        assert!(
            state.retry_attempts.contains_key("1"),
            "transient tracker error must retain scheduled retry and attempt count"
        );
        assert_eq!(state.retry_attempts["1"].attempt, 6);
        assert!(state.retry_attempts["1"].due_at > std::time::Instant::now());
    }

    #[tokio::test]
    async fn retries_wait_for_state_capacity_and_respect_dependencies() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        for blocked in [false, true] {
            let server = MockServer::start().await;
            let status = if blocked { "Todo" } else { "In Progress" };
            Mock::given(method("GET")).and(path("/repos/openai/kairastra/issues")).and(query_param("page", "1"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!([{"id":1,"node_id":"1","number":1,"title":"Test","state":"open","assignees":[],"labels":[{"name":status}]}]))).mount(&server).await;
            Mock::given(method("GET"))
                .and(path("/repos/openai/kairastra/issues"))
                .and(query_param("page", "2"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path(
                    "/repos/openai/kairastra/issues/1/dependencies/blocked_by",
                ))
                .respond_with(
                    ResponseTemplate::new(200).set_body_json(json!([{"id":9,"state":"open"}])),
                )
                .mount(&server)
                .await;
            let mut snapshot = snapshot();
            snapshot.settings.tracker.mode = crate::config::GitHubMode::IssuesOnly;
            snapshot.settings.tracker.repo = Some("kairastra".into());
            snapshot.settings.tracker.rest_endpoint = server.uri();
            snapshot.settings.tracker.status_source = Some(crate::config::FieldSource {
                source_type: crate::config::FieldSourceType::Label,
                name: None,
            });
            snapshot
                .settings
                .agent
                .max_concurrent_agents_by_state
                .insert("in progress".into(), 0);
            let mut state = RuntimeState::default();
            state.retry_attempts.insert(
                "1".into(),
                super::RetryEntry {
                    attempt: 4,
                    due_at: std::time::Instant::now(),
                },
            );
            let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
            orchestrator_for(&snapshot)
                .dispatch_due_retries(&snapshot, &mut state, &tx)
                .await
                .unwrap();
            assert!(state.running.is_empty());
            if blocked {
                assert!(!state.retry_attempts.contains_key("1"));
            } else {
                assert_eq!(state.retry_attempts["1"].attempt, 4);
                assert!(state.retry_attempts["1"].due_at > std::time::Instant::now());
            }
        }
    }

    #[tokio::test]
    async fn run_once_finishes_started_workers_after_later_dispatch_error() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        let all_reads = Arc::new(AtomicUsize::new(0));
        let reads = all_reads.clone();
        Mock::given(method("GET")).and(path("/repos/openai/kairastra/issues")).and(query_param("page", "1"))
            .respond_with(move |request: &wiremock::Request| {
                let all = request.url.query_pairs().any(|(name, value)| name == "state" && value == "all");
                if all && reads.fetch_add(1, Ordering::SeqCst) >= 2 {
                    return ResponseTemplate::new(503).set_body_json(json!({"message":"offline"}));
                }
                ResponseTemplate::new(200).set_body_json(json!([
                    {"id":1,"node_id":"1","number":1,"title":"A","state":"open","assignees":[],"labels":[{"name":"In Progress"}]},
                    {"id":2,"node_id":"2","number":2,"title":"B","state":"open","assignees":[],"labels":[{"name":"In Progress"}]}
                ]))
            }).mount(&server).await;
        Mock::given(method("GET"))
            .and(path("/repos/openai/kairastra/issues"))
            .and(query_param("page", "2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("finalized");
        let file = dir.path().join("WORKFLOW.md");
        std::fs::write(&file, format!("---\ntracker:\n  kind: github\n  owner: openai\n  repo: kairastra\n  mode: issues_only\n  status_source:\n    type: label\n  api_key: fake\n  rest_endpoint: {}\nworkspace:\n  root: {}\nhooks:\n  before_run: 'sleep 0.1; exit 1'\n  after_run: \"touch '{}'\"\nagent:\n  provider: codex\n  max_concurrent_agents: 2\nproviders:\n  codex: {{}}\n---\nPrompt\n", server.uri(), dir.path().join("workspaces").display(), marker.display())).unwrap();
        let store = Arc::new(crate::workflow::WorkflowStore::new(file));
        let snapshot = store.current().unwrap();
        let runtime = super::Orchestrator::new(
            store,
            Arc::new(crate::github::GitHubTracker::new(snapshot.settings.tracker).unwrap()),
        );
        assert!(runtime.run_once().await.is_err());
        assert!(all_reads.load(Ordering::SeqCst) >= 3);
        assert!(
            marker.exists(),
            "run_once returned before its previously launched worker finalized"
        );
    }

    #[tokio::test]
    async fn review_plan_mode_outer_watchdog_uses_plan_timeout() {
        use std::sync::Arc;
        let dir = tempfile::tempdir().unwrap();
        let mut snapshot = snapshot();
        snapshot.settings.workspace.root = dir.path().into();
        let mut task = issue("1", "In Progress", Some(1));
        task.labels.push("mode:plan".into());
        let tracker =
            Arc::new(crate::github::GitHubTracker::new(snapshot.settings.tracker.clone()).unwrap());
        let store = Arc::new(crate::workflow::WorkflowStore::new(
            std::path::PathBuf::from("unused"),
        ));
        let orchestrator = super::Orchestrator::new(store, tracker);
        let mut state = RuntimeState::default();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        assert!(orchestrator
            .spawn_worker(snapshot, &mut state, &tx, task, None)
            .await
            .unwrap());
        let running = &state.running["1"];
        running.handle.abort();
        assert!(running.stall_timeout_ms >= 900_000, "outer orchestrator watchdog must respect the provider's 15-minute planning timeout, got {}", running.stall_timeout_ms);
    }

    #[tokio::test]
    async fn review_retry_honors_changed_assignee_filter() {
        use std::sync::Arc;
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        let payload = json!([{"id":1,"node_id":"1","number":1,"title":"Issue", "state":"open", "assignees":[{"login":"someone-else"}], "labels":[{"name":"In Progress"}]}]);
        Mock::given(method("GET"))
            .and(path("/repos/openai/kairastra/issues"))
            .and(query_param("page", "1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(payload))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/repos/openai/kairastra/issues"))
            .and(query_param("page", "2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .mount(&server)
            .await;
        let mut snapshot = snapshot_with_assignee("codex-bot");
        let dir = tempfile::tempdir().unwrap();
        snapshot.settings.workspace.root = dir.path().into();
        snapshot.settings.tracker.mode = crate::config::GitHubMode::IssuesOnly;
        snapshot.settings.tracker.status_source = Some(crate::config::FieldSource {
            source_type: crate::config::FieldSourceType::Label,
            name: None,
        });
        snapshot.settings.tracker.repo = Some("kairastra".into());
        snapshot.settings.tracker.rest_endpoint = server.uri();
        let tracker =
            Arc::new(crate::github::GitHubTracker::new(snapshot.settings.tracker.clone()).unwrap());
        let fetched =
            crate::github::Tracker::fetch_issue_states_by_ids(tracker.as_ref(), &["1".to_string()])
                .await
                .unwrap();
        assert_eq!(fetched[0].state, "In Progress");
        let orchestrator = super::Orchestrator::new(
            Arc::new(crate::workflow::WorkflowStore::new("unused".into())),
            tracker,
        );
        let mut state = RuntimeState::default();
        state.retry_attempts.insert(
            "1".into(),
            super::RetryEntry {
                attempt: 2,
                due_at: std::time::Instant::now(),
            },
        );
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        orchestrator
            .dispatch_due_retries(&snapshot, &mut state, &tx)
            .await
            .unwrap();
        let started = !state.running.is_empty();
        for running in state.running.values() {
            running.handle.abort();
        }
        assert!(
            !started,
            "retry launched work after the issue was reassigned away from the configured bot"
        );
    }

    #[tokio::test]
    async fn review_polling_continues_during_worker_events() {
        use std::os::unix::fs::PermissionsExt;
        use std::sync::Arc;
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("codex-spam.py");
        let ready = dir.path().join("ready");
        let script_body = format!(
            r##"#!/usr/bin/env python3
import json, sys, time
for line in sys.stdin:
    message = json.loads(line)
    method = message.get('method')
    if method == 'initialize':
        print(json.dumps({{'id':1,'result':{{}}}}), flush=True)
    elif method == 'thread/start':
        print(json.dumps({{'id':2,'result':{{'thread':{{'id':'review-thread'}}}}}}), flush=True)
    elif method == 'turn/start':
        print(json.dumps({{'id':3,'result':{{'turn':{{'id':'review-turn'}}}}}}), flush=True)
        open({ready:?}, 'w').close()
        for i in range(150):
            print(json.dumps({{'method':'turn/plan/updated','params':{{'plan':[]}}}}), flush=True)
            time.sleep(0.004)
        print(json.dumps({{'method':'turn/completed','params':{{'turn':{{'id':'review-turn','status':'completed'}}}}}}), flush=True)
"##,
            ready = ready.to_str().unwrap()
        );
        std::fs::write(&script, script_body).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let issue_payload = json!([{"id":1,"node_id":"1","number":1,"title":"Issue", "state":"open", "assignees":[], "labels":[{"name":"In Progress"}]}]);
        Mock::given(method("GET"))
            .and(path("/repos/openai/kairastra/issues"))
            .and(query_param("page", "1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(issue_payload))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/repos/openai/kairastra/issues"))
            .and(query_param("page", "2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/repos/openai/kairastra/issues/1/comments"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/repos/openai/kairastra/issues/1/comments"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"id":9,"body":"## Codex Workpad\ncreated"})),
            )
            .mount(&server)
            .await;
        let yaml = format!("tracker:\n  kind: github\n  owner: openai\n  repo: kairastra\n  mode: issues_only\n  status_source:\n    type: label\n  api_key: fake\n  rest_endpoint: {}\npolling:\n  interval_ms: 30\nworkspace:\n  root: {}\nagent:\n  provider: codex\n  max_concurrent_agents: 1\nproviders:\n  codex:\n    command: {}\n", server.uri(), dir.path().join("workspaces").display(), script.display());
        let workflow_path = dir.path().join("WORKFLOW.md");
        std::fs::write(&workflow_path, format!("---\n{yaml}---\nPrompt\n")).unwrap();
        let store = Arc::new(crate::workflow::WorkflowStore::new(workflow_path));
        let snapshot = store.current().unwrap();
        let tracker =
            Arc::new(crate::github::GitHubTracker::new(snapshot.settings.tracker).unwrap());
        let orchestrator = super::Orchestrator::new(store, tracker);
        let handle = tokio::spawn(async move { orchestrator.run().await });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !ready.exists() && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert!(
            ready.exists(),
            "fake provider never reached its event stream"
        );
        let before = server.received_requests().await.unwrap().len();
        tokio::time::sleep(std::time::Duration::from_millis(180)).await;
        let after = server.received_requests().await.unwrap().len();
        handle.abort();
        let _ = handle.await;
        // The fake provider exits after its short stream; allow detached worker cleanup.
        tokio::time::sleep(std::time::Duration::from_millis(700)).await;
        assert!(after > before, "no polling/reconciliation requests occurred during 6 configured poll intervals while the worker streamed events ({before} -> {after})");
    }
}
