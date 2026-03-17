use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use chrono::Utc;
use reqwest::Method;
use serde_json::{json, Value as JsonValue};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::sync::mpsc::UnboundedSender;
use tokio::time::{timeout, Duration};
use tracing::{debug, warn};

use crate::agent::{AgentBackend, AgentEvent, AgentEventKind, AgentSession, TurnResult};
use crate::config::Settings;
use crate::github::GitHubTracker;
use crate::model::Issue;

use super::config::CodexConfig;

const INITIALIZE_ID: u64 = 1;
const THREAD_START_ID: u64 = 2;
const TURN_START_ID: u64 = 3;
const NON_INTERACTIVE_TOOL_INPUT_ANSWER: &str =
    "This is a non-interactive session. Operator input is unavailable.";

#[derive(Debug, Clone)]
pub struct CodexBackend;

#[async_trait]
impl AgentBackend for CodexBackend {
    async fn start_session(
        &self,
        settings: &Settings,
        tracker: Arc<GitHubTracker>,
        workspace: &Path,
    ) -> Result<Box<dyn AgentSession>> {
        Ok(Box::new(
            CodexSession::start(settings, tracker, workspace).await?,
        ))
    }
}

pub struct CodexSession {
    config: CodexConfig,
    child: Child,
    stdin: ChildStdin,
    stdout: Lines<BufReader<ChildStdout>>,
    thread_id: String,
    workspace: PathBuf,
    auto_approve_requests: bool,
    approval_policy: JsonValue,
    turn_sandbox_policy: JsonValue,
    tracker: Arc<GitHubTracker>,
}

#[async_trait]
impl AgentSession for CodexSession {
    async fn run_turn(
        &mut self,
        issue: &Issue,
        prompt: &str,
        on_event: &UnboundedSender<AgentEvent>,
    ) -> Result<TurnResult> {
        self.run_turn_internal(issue, prompt, on_event).await
    }

    async fn stop(&mut self) -> Result<()> {
        self.stop_internal().await
    }

    fn process_id(&self) -> Option<u32> {
        self.process_id_internal()
    }
}

impl CodexSession {
    pub async fn start(
        settings: &Settings,
        tracker: Arc<GitHubTracker>,
        workspace: &Path,
    ) -> Result<Self> {
        validate_workspace_cwd(&settings.workspace.root, workspace)?;
        let config = super::config::load(settings)?;

        let cargo_home = workspace.join(".cargo-home");
        tokio::fs::create_dir_all(&cargo_home)
            .await
            .context("failed to create workspace cargo home")?;

        let mut command = Command::new("bash");
        command.arg("-lc").arg(&config.command);
        command.current_dir(workspace);
        command.env("CARGO_HOME", &cargo_home);
        command.env("GITHUB_TOKEN", tracker.settings().api_key.as_str());
        command.env("GH_TOKEN", tracker.settings().api_key.as_str());
        command.stdin(Stdio::piped());
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());
        command.kill_on_drop(true);

        let mut child = command
            .spawn()
            .context("failed to launch codex app-server")?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("missing app-server stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("missing app-server stdout"))?;
        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(log_stderr(stderr));
        }

        let workspace_path = workspace.to_path_buf();
        let approval_policy = config.approval_policy.clone();
        let turn_sandbox_policy = config.turn_sandbox_policy(workspace);
        let auto_approve_requests =
            matches!(&approval_policy, JsonValue::String(value) if value == "never");

        let mut session = Self {
            config,
            child,
            stdin,
            stdout: BufReader::new(stdout).lines(),
            thread_id: String::new(),
            workspace: workspace_path,
            auto_approve_requests,
            approval_policy,
            turn_sandbox_policy,
            tracker,
        };

        session.send_initialize().await?;
        session.thread_id = session.start_thread().await?;
        Ok(session)
    }

    pub fn process_id(&self) -> Option<u32> {
        self.process_id_internal()
    }

    pub async fn run_turn(
        &mut self,
        issue: &Issue,
        prompt: &str,
        on_event: &UnboundedSender<AgentEvent>,
    ) -> Result<TurnResult> {
        self.run_turn_internal(issue, prompt, on_event).await
    }

    pub async fn stop(&mut self) -> Result<()> {
        self.stop_internal().await
    }

    fn process_id_internal(&self) -> Option<u32> {
        self.child.id()
    }

    async fn run_turn_internal(
        &mut self,
        issue: &Issue,
        prompt: &str,
        on_event: &UnboundedSender<AgentEvent>,
    ) -> Result<TurnResult> {
        let turn_id = self.start_turn(issue, prompt).await?;
        emit_session_started(
            on_event,
            self.process_id_internal(),
            &self.thread_id,
            &turn_id,
        );

        let turn_id = self.await_turn_completion(Some(turn_id), on_event).await?;
        let session_id = format!("{}-{turn_id}", self.thread_id);

        Ok(TurnResult {
            session_id,
            thread_id: self.thread_id.clone(),
            turn_id,
        })
    }

    async fn stop_internal(&mut self) -> Result<()> {
        if let Some(status) = self.child.try_wait()? {
            debug!(?status, "app-server already exited");
            return Ok(());
        }

        self.child
            .kill()
            .await
            .context("failed to stop app-server")?;
        Ok(())
    }

    async fn send_initialize(&mut self) -> Result<()> {
        self.write_message(json!({
            "method": "initialize",
            "id": INITIALIZE_ID,
            "params": {
                "capabilities": {
                    "experimentalApi": true
                },
                "clientInfo": {
                    "name": "symphony-rust",
                    "title": "Symphony Rust",
                    "version": "0.1.0"
                }
            }
        }))
        .await?;

        let _ = self
            .await_response(INITIALIZE_ID, self.config.read_timeout_ms)
            .await?;
        self.write_message(json!({
            "method": "initialized",
            "params": {}
        }))
        .await?;
        Ok(())
    }

    async fn start_thread(&mut self) -> Result<String> {
        self.write_message(json!({
            "method": "thread/start",
            "id": THREAD_START_ID,
            "params": {
                "approvalPolicy": self.approval_policy,
                "sandbox": self.config.thread_sandbox,
                "cwd": self.workspace.to_string_lossy(),
                "dynamicTools": dynamic_tool_specs(),
                "model": self.config.model,
                "serviceTier": self.config.service_tier(),
            }
        }))
        .await?;

        let payload = self
            .await_response(THREAD_START_ID, self.config.read_timeout_ms)
            .await?;
        payload
            .get("thread")
            .and_then(|thread| thread.get("id"))
            .and_then(JsonValue::as_str)
            .map(ToString::to_string)
            .ok_or_else(|| anyhow!("invalid_thread_payload"))
    }

    async fn start_turn(&mut self, issue: &Issue, prompt: &str) -> Result<String> {
        self.write_message(json!({
            "method": "turn/start",
            "id": TURN_START_ID,
            "params": {
                "threadId": self.thread_id,
                "input": [
                    {
                        "type": "text",
                        "text": prompt
                    }
                ],
                "cwd": self.workspace.to_string_lossy(),
                "title": format!("{}: {}", issue.identifier, issue.title),
                "approvalPolicy": self.approval_policy,
                "sandboxPolicy": self.turn_sandbox_policy,
                "model": self.config.model,
                "effort": self.config.reasoning_effort,
                "serviceTier": self.config.service_tier(),
            }
        }))
        .await?;

        let payload = self.await_response(TURN_START_ID, 10_000).await?;
        payload
            .get("turn")
            .and_then(|turn| turn.get("id"))
            .and_then(JsonValue::as_str)
            .map(ToString::to_string)
            .ok_or_else(|| anyhow!("invalid_turn_payload"))
    }

    async fn await_response(&mut self, expected_id: u64, timeout_ms: u64) -> Result<JsonValue> {
        loop {
            let line = timeout(Duration::from_millis(timeout_ms), self.stdout.next_line())
                .await
                .map_err(|_| anyhow!("response_timeout"))?
                .context("failed to read app-server response")?;

            let Some(line) = line else {
                return Err(anyhow!("app_server_exited_during_response"));
            };

            let payload: JsonValue = match serde_json::from_str(&line) {
                Ok(payload) => payload,
                Err(_) => {
                    debug!(
                        line,
                        "ignoring non-json response while awaiting app-server response"
                    );
                    continue;
                }
            };

            if payload.get("id").and_then(JsonValue::as_u64) == Some(expected_id) {
                if let Some(result) = payload.get("result") {
                    return Ok(result.clone());
                }
                if let Some(error) = payload.get("error") {
                    return Err(anyhow!("app_server_error_response: {error}"));
                }
                return Ok(payload);
            }
        }
    }

    async fn await_turn_completion(
        &mut self,
        initial_turn_id: Option<String>,
        on_event: &UnboundedSender<AgentEvent>,
    ) -> Result<String> {
        let turn_started = Instant::now();
        let mut last_event_at = Instant::now();
        let turn_id = initial_turn_id;

        loop {
            if self.config.turn_timeout_ms > 0
                && turn_started.elapsed() >= Duration::from_millis(self.config.turn_timeout_ms)
            {
                return Err(anyhow!("turn_timeout"));
            }

            if self.config.stall_timeout_ms > 0
                && last_event_at.elapsed() >= Duration::from_millis(self.config.stall_timeout_ms)
            {
                return Err(anyhow!("turn_stalled"));
            }

            let remaining_turn = if self.config.turn_timeout_ms == 0 {
                Duration::from_secs(3600)
            } else {
                Duration::from_millis(self.config.turn_timeout_ms)
                    .saturating_sub(turn_started.elapsed())
            };
            let remaining_stall = if self.config.stall_timeout_ms == 0 {
                Duration::from_secs(3600)
            } else {
                Duration::from_millis(self.config.stall_timeout_ms)
                    .saturating_sub(last_event_at.elapsed())
            };
            let wait_for = remaining_turn.min(remaining_stall);

            let line = timeout(wait_for, self.stdout.next_line())
                .await
                .map_err(|_| {
                    if self.config.stall_timeout_ms > 0
                        && last_event_at.elapsed()
                            >= Duration::from_millis(self.config.stall_timeout_ms)
                    {
                        anyhow!("turn_stalled")
                    } else {
                        anyhow!("turn_timeout")
                    }
                })?
                .context("failed while reading turn stream")?;

            let Some(line) = line else {
                let status = self.child.wait().await.ok();
                return Err(anyhow!("app_server_exited: {:?}", status));
            };

            last_event_at = Instant::now();
            let now = Utc::now();

            let payload: JsonValue = match serde_json::from_str(&line) {
                Ok(payload) => payload,
                Err(_) => {
                    let _ = on_event.send(AgentEvent {
                        event: AgentEventKind::Malformed,
                        timestamp: now,
                        payload: json!({ "raw": line }),
                        session_id: current_session_id(&self.thread_id, turn_id.as_deref()),
                        agent_process_pid: self.process_id().map(|value| value.to_string()),
                    });
                    continue;
                }
            };

            let Some(method) = payload.get("method").and_then(JsonValue::as_str) else {
                let _ = on_event.send(AgentEvent {
                    event: AgentEventKind::OtherMessage,
                    timestamp: now,
                    payload,
                    session_id: current_session_id(&self.thread_id, turn_id.as_deref()),
                    agent_process_pid: self.process_id().map(|value| value.to_string()),
                });
                continue;
            };

            match method {
                "turn/completed" => {
                    let resolved_turn_id = turn_id
                        .clone()
                        .or_else(|| {
                            payload
                                .get("params")
                                .and_then(|params| params.get("turn"))
                                .and_then(|turn| turn.get("id"))
                                .and_then(JsonValue::as_str)
                                .map(ToString::to_string)
                        })
                        .unwrap_or_else(|| "turn".to_string());
                    let session_id = format!("{}-{resolved_turn_id}", self.thread_id);
                    let _ = on_event.send(AgentEvent {
                        event: AgentEventKind::TurnCompleted,
                        timestamp: now,
                        payload,
                        session_id: Some(session_id),
                        agent_process_pid: self.process_id().map(|value| value.to_string()),
                    });
                    return Ok(resolved_turn_id);
                }
                "turn/failed" => {
                    let resolved_turn_id = turn_id.clone().unwrap_or_else(|| "turn".to_string());
                    let session_id = format!("{}-{resolved_turn_id}", self.thread_id);
                    let _ = on_event.send(AgentEvent {
                        event: AgentEventKind::TurnFailed,
                        timestamp: now,
                        payload: payload.clone(),
                        session_id: Some(session_id),
                        agent_process_pid: self.process_id().map(|value| value.to_string()),
                    });
                    return Err(anyhow!("turn_failed: {}", payload));
                }
                "turn/cancelled" => {
                    let resolved_turn_id = turn_id.clone().unwrap_or_else(|| "turn".to_string());
                    let session_id = format!("{}-{resolved_turn_id}", self.thread_id);
                    let _ = on_event.send(AgentEvent {
                        event: AgentEventKind::TurnCancelled,
                        timestamp: now,
                        payload: payload.clone(),
                        session_id: Some(session_id),
                        agent_process_pid: self.process_id().map(|value| value.to_string()),
                    });
                    return Err(anyhow!("turn_cancelled: {}", payload));
                }
                "turn/input_required" => {
                    let resolved_turn_id = turn_id.clone().unwrap_or_else(|| "turn".to_string());
                    let session_id = format!("{}-{resolved_turn_id}", self.thread_id);
                    let _ = on_event.send(AgentEvent {
                        event: AgentEventKind::TurnInputRequired,
                        timestamp: now,
                        payload: payload.clone(),
                        session_id: Some(session_id),
                        agent_process_pid: self.process_id().map(|value| value.to_string()),
                    });
                    return Err(anyhow!("turn_input_required: {}", payload));
                }
                "item/commandExecution/requestApproval"
                | "execCommandApproval"
                | "applyPatchApproval"
                | "item/fileChange/requestApproval" => {
                    if self.auto_approve_requests {
                        self.auto_approve(&payload, method).await?;
                        let session_id = turn_id
                            .as_ref()
                            .map(|value| format!("{}-{value}", self.thread_id));
                        let _ = on_event.send(AgentEvent {
                            event: AgentEventKind::ApprovalAutoApproved,
                            timestamp: now,
                            payload,
                            session_id,
                            agent_process_pid: self.process_id().map(|value| value.to_string()),
                        });
                    } else {
                        let session_id = turn_id
                            .as_ref()
                            .map(|value| format!("{}-{value}", self.thread_id));
                        let _ = on_event.send(AgentEvent {
                            event: AgentEventKind::ApprovalRequired,
                            timestamp: now,
                            payload: payload.clone(),
                            session_id,
                            agent_process_pid: self.process_id().map(|value| value.to_string()),
                        });
                        return Err(anyhow!("approval_required: {}", payload));
                    }
                }
                "item/tool/call" => {
                    let success = self.handle_tool_call(&payload).await?;
                    let session_id = turn_id
                        .as_ref()
                        .map(|value| format!("{}-{value}", self.thread_id));
                    let _ = on_event.send(AgentEvent {
                        event: if success {
                            AgentEventKind::ToolCallCompleted
                        } else if payload
                            .get("params")
                            .and_then(|params| params.get("tool").or_else(|| params.get("name")))
                            .is_none()
                        {
                            AgentEventKind::UnsupportedToolCall
                        } else {
                            AgentEventKind::ToolCallFailed
                        },
                        timestamp: now,
                        payload,
                        session_id,
                        agent_process_pid: self.process_id().map(|value| value.to_string()),
                    });
                }
                "item/tool/requestUserInput" => {
                    self.handle_tool_request_user_input(&payload).await?;
                    let session_id = turn_id
                        .as_ref()
                        .map(|value| format!("{}-{value}", self.thread_id));
                    let _ = on_event.send(AgentEvent {
                        event: AgentEventKind::ToolInputAutoAnswered,
                        timestamp: now,
                        payload,
                        session_id,
                        agent_process_pid: self.process_id().map(|value| value.to_string()),
                    });
                }
                other => {
                    if needs_input(other, &payload) {
                        let session_id = turn_id
                            .as_ref()
                            .map(|value| format!("{}-{value}", self.thread_id));
                        let _ = on_event.send(AgentEvent {
                            event: AgentEventKind::TurnInputRequired,
                            timestamp: now,
                            payload: payload.clone(),
                            session_id,
                            agent_process_pid: self.process_id().map(|value| value.to_string()),
                        });
                        return Err(anyhow!("turn_input_required: {}", payload));
                    }

                    let session_id = turn_id
                        .as_ref()
                        .map(|value| format!("{}-{value}", self.thread_id));
                    let _ = on_event.send(AgentEvent {
                        event: AgentEventKind::Notification,
                        timestamp: now,
                        payload,
                        session_id,
                        agent_process_pid: self.process_id().map(|value| value.to_string()),
                    });
                }
            }
        }
    }

    async fn auto_approve(&mut self, payload: &JsonValue, method: &str) -> Result<()> {
        let id = payload
            .get("id")
            .and_then(JsonValue::as_u64)
            .ok_or_else(|| anyhow!("approval payload missing id"))?;
        let decision = match method {
            "execCommandApproval" | "applyPatchApproval" => "approved_for_session",
            _ => "acceptForSession",
        };
        self.write_message(json!({
            "id": id,
            "result": {
                "decision": decision
            }
        }))
        .await
    }

    async fn handle_tool_call(&mut self, payload: &JsonValue) -> Result<bool> {
        let id = payload
            .get("id")
            .and_then(JsonValue::as_u64)
            .ok_or_else(|| anyhow!("tool call payload missing id"))?;
        let params = payload
            .get("params")
            .and_then(JsonValue::as_object)
            .ok_or_else(|| anyhow!("tool call payload missing params"))?;
        let tool_name = params
            .get("tool")
            .or_else(|| params.get("name"))
            .and_then(JsonValue::as_str);
        let arguments = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));

        let result = match tool_name {
            Some("github_graphql") => execute_github_graphql(&self.tracker, arguments).await,
            Some("github_rest") => execute_github_rest(&self.tracker, arguments).await,
            Some(other) => dynamic_tool_failure(json!({
                "error": {
                    "message": format!("Unsupported dynamic tool: {other}."),
                    "supportedTools": ["github_graphql", "github_rest"]
                }
            })),
            None => dynamic_tool_failure(json!({
                "error": {
                    "message": "Tool call payload did not include a tool name."
                }
            })),
        };

        let success = result
            .get("success")
            .and_then(JsonValue::as_bool)
            .unwrap_or(false);

        self.write_message(json!({
            "id": id,
            "result": result
        }))
        .await?;

        Ok(success)
    }

    async fn handle_tool_request_user_input(&mut self, payload: &JsonValue) -> Result<()> {
        let id = payload
            .get("id")
            .and_then(JsonValue::as_u64)
            .ok_or_else(|| anyhow!("tool input payload missing id"))?;
        let questions = payload
            .get("params")
            .and_then(|params| params.get("questions"))
            .and_then(JsonValue::as_array)
            .cloned()
            .unwrap_or_default();

        let mut answers = serde_json::Map::new();
        for question in questions {
            let Some(question_id) = question.get("id").and_then(JsonValue::as_str) else {
                continue;
            };
            let answer = if self.auto_approve_requests {
                question
                    .get("options")
                    .and_then(JsonValue::as_array)
                    .and_then(|options| {
                        options.iter().find_map(|option| {
                            let label = option.get("label").and_then(JsonValue::as_str)?;
                            if label.to_lowercase().contains("approve") {
                                Some(label.to_string())
                            } else {
                                None
                            }
                        })
                    })
                    .or_else(|| {
                        question
                            .get("options")
                            .and_then(JsonValue::as_array)
                            .and_then(|options| {
                                options
                                    .first()
                                    .and_then(|option| option.get("label"))
                                    .and_then(JsonValue::as_str)
                                    .map(ToString::to_string)
                            })
                    })
                    .unwrap_or_else(|| NON_INTERACTIVE_TOOL_INPUT_ANSWER.to_string())
            } else {
                NON_INTERACTIVE_TOOL_INPUT_ANSWER.to_string()
            };

            answers.insert(question_id.to_string(), json!({ "answers": [answer] }));
        }

        self.write_message(json!({
            "id": id,
            "result": {
                "answers": answers
            }
        }))
        .await
    }

    async fn write_message(&mut self, payload: JsonValue) -> Result<()> {
        let encoded = serde_json::to_vec(&payload).context("failed to encode JSON-RPC payload")?;
        self.stdin.write_all(&encoded).await?;
        self.stdin.write_all(b"\n").await?;
        self.stdin.flush().await?;
        Ok(())
    }
}

fn validate_workspace_cwd(root: &Path, workspace: &Path) -> Result<()> {
    let canonical_root = root
        .canonicalize()
        .with_context(|| format!("failed to canonicalize {}", root.display()))?;
    let canonical_workspace = workspace
        .canonicalize()
        .with_context(|| format!("failed to canonicalize {}", workspace.display()))?;

    if canonical_root == canonical_workspace {
        return Err(anyhow!("invalid_workspace_cwd: workspace_root"));
    }

    let root_prefix = format!("{}/", canonical_root.display());
    if !canonical_workspace
        .display()
        .to_string()
        .starts_with(&root_prefix)
    {
        return Err(anyhow!("invalid_workspace_cwd: outside_workspace_root"));
    }

    Ok(())
}

async fn log_stderr(stderr: ChildStderr) {
    let mut lines = BufReader::new(stderr).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let sanitized = strip_ansi_sequences(&line);
        let trimmed = sanitized.trim();
        if trimmed.is_empty() || is_noisy_codex_stderr(trimmed) {
            debug!(line = trimmed, "codex stderr");
            continue;
        }

        if looks_like_error(trimmed) {
            warn!(line = trimmed, "codex stderr");
        } else {
            debug!(line = trimmed, "codex stderr");
        }
    }
}

fn is_noisy_codex_stderr(line: &str) -> bool {
    line.contains("codex_otel.")
        || line.contains("event.name=")
        || line.contains("session_loop")
        || line.contains("submission_dispatch")
        || line.contains("turn.id=")
        || line.contains("conversation.id=")
        || line.contains("app.version=")
        || line.contains("tool_name=")
        || line.contains("codex_core::file_watcher")
        || line.contains("codex_core::analytics_client: events failed with status 403 Forbidden")
        || line.contains("codex_core::shell_snapshot: Failed to delete shell snapshot")
        || line.starts_with("- Treat a top-level `errors` array as a failed operation")
        || line.contains("If the failure is a non-fast-forward or sync problem, run the `pull`")
        || line.contains("If the failure is due to auth, permissions, or workflow restrictions on")
        || line == "the configured remote, stop and surface the exact error instead of"
        || line.starts_with("To see what failed, try: gh run view ")
        || line.contains("No watch was found")
        || line.contains("channel closed")
        || line.contains("processor task exited")
        || line.contains("outbound router task exited")
        || line.contains("stdout writer exited")
        || line.starts_with("Wall time:")
        || line.starts_with("Process exited with code")
        || line.starts_with("Original token count:")
        || line == "Output:"
}

fn looks_like_error(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    lower.contains(" error")
        || lower.starts_with("error")
        || lower.contains(" failed")
        || lower.contains(" failure")
        || lower.contains("panic")
        || lower.contains("fatal")
        || lower.contains("denied")
        || lower.contains("exception")
        || lower.contains("timed out")
}

fn strip_ansi_sequences(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index] == 0x1b {
            index += 1;
            if index < bytes.len() && bytes[index] == b'[' {
                index += 1;
                while index < bytes.len() {
                    let byte = bytes[index];
                    index += 1;
                    if (byte as char).is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }

        output.push(bytes[index] as char);
        index += 1;
    }

    output
}

fn emit_session_started(
    on_event: &UnboundedSender<AgentEvent>,
    process_id: Option<u32>,
    thread_id: &str,
    turn_id: &str,
) {
    let session_id = format!("{thread_id}-{turn_id}");
    let _ = on_event.send(AgentEvent {
        event: AgentEventKind::SessionStarted,
        timestamp: Utc::now(),
        payload: json!({
            "session_id": session_id,
            "thread_id": thread_id,
            "turn_id": turn_id,
        }),
        session_id: Some(session_id),
        agent_process_pid: process_id.map(|value| value.to_string()),
    });
}

fn current_session_id(thread_id: &str, turn_id: Option<&str>) -> Option<String> {
    turn_id.map(|turn_id| format!("{thread_id}-{turn_id}"))
}

fn needs_input(method: &str, payload: &JsonValue) -> bool {
    method == "turn/input_required"
        || payload
            .get("input_required")
            .and_then(JsonValue::as_bool)
            .unwrap_or(false)
        || payload
            .get("type")
            .and_then(JsonValue::as_str)
            .map(|value| value == "input_required")
            .unwrap_or(false)
        || payload
            .get("params")
            .and_then(|params| params.get("requiresInput"))
            .and_then(JsonValue::as_bool)
            .unwrap_or(false)
}

fn dynamic_tool_specs() -> JsonValue {
    json!([
        {
            "name": "github_graphql",
            "description": "Execute a raw GraphQL query or mutation against GitHub using Symphony's configured auth.",
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "required": ["query"],
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "GraphQL query or mutation document."
                    },
                    "variables": {
                        "type": ["object", "null"],
                        "additionalProperties": true
                    }
                }
            }
        },
        {
            "name": "github_rest",
            "description": "Execute a small allow-listed set of GitHub REST endpoints.",
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "required": ["method", "path"],
                "properties": {
                    "method": { "type": "string" },
                    "path": { "type": "string" },
                    "body": { "type": ["object", "null"], "additionalProperties": true }
                }
            }
        }
    ])
}

async fn execute_github_graphql(tracker: &GitHubTracker, arguments: JsonValue) -> JsonValue {
    let query = arguments
        .get("query")
        .and_then(JsonValue::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let Some(query) = query else {
        return dynamic_tool_failure(json!({
            "error": { "message": "`github_graphql` requires a non-empty `query` string." }
        }));
    };

    let variables = arguments
        .get("variables")
        .cloned()
        .unwrap_or_else(|| json!({}));

    match tracker.graphql_raw(query, variables).await {
        Ok(response) => dynamic_tool_response(true, response),
        Err(error) => dynamic_tool_failure(json!({
            "error": { "message": error.to_string() }
        })),
    }
}

async fn execute_github_rest(tracker: &GitHubTracker, arguments: JsonValue) -> JsonValue {
    let method = arguments
        .get("method")
        .and_then(JsonValue::as_str)
        .map(|value| value.to_uppercase());
    let path = arguments.get("path").and_then(JsonValue::as_str);

    let (Some(method), Some(path)) = (method, path) else {
        return dynamic_tool_failure(json!({
            "error": {
                "message": "`github_rest` expects `method` and `path`."
            }
        }));
    };

    if !rest_path_allowed(path) {
        return dynamic_tool_failure(json!({
            "error": {
                "message": format!("REST path not allow-listed: {path}")
            }
        }));
    }

    let method = match method.as_str() {
        "GET" => Method::GET,
        "POST" => Method::POST,
        "PATCH" => Method::PATCH,
        other => {
            return dynamic_tool_failure(json!({
                "error": {
                    "message": format!("Unsupported github_rest method: {other}")
                }
            }))
        }
    };

    let body = arguments.get("body").cloned();

    match tracker.rest_json(method, path, body).await {
        Ok(response) => dynamic_tool_response(true, response),
        Err(error) => dynamic_tool_failure(json!({
            "error": { "message": error.to_string() }
        })),
    }
}

fn rest_path_allowed(path: &str) -> bool {
    path.contains("/issues/") || path.contains("/pulls/")
}

fn dynamic_tool_response(success: bool, payload: JsonValue) -> JsonValue {
    let output = serde_json::to_string_pretty(&payload).unwrap_or_else(|_| payload.to_string());
    json!({
        "success": success,
        "output": output,
        "contentItems": [
            {
                "type": "inputText",
                "text": output
            }
        ]
    })
}

fn dynamic_tool_failure(payload: JsonValue) -> JsonValue {
    dynamic_tool_response(false, payload)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::Arc;

    use tempfile::tempdir;
    use tokio::sync::mpsc::unbounded_channel;
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::config::Settings;
    use crate::github::GitHubTracker;
    use crate::model::{Issue, WorkflowDefinition};

    use super::{AgentEventKind, CodexSession};

    fn issue(identifier: &str) -> Issue {
        Issue {
            id: identifier.to_string(),
            project_item_id: None,
            identifier: identifier.to_string(),
            title: "Test issue".to_string(),
            description: Some("body".to_string()),
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

    fn settings_with_command(
        workspace_root: &std::path::Path,
        command: &str,
        tracker_extra: &str,
        provider_extra: &str,
    ) -> Settings {
        let definition = WorkflowDefinition {
            config: serde_yaml::from_str(&format!(
                r#"---
tracker:
  kind: github
  owner: openai
  project_v2_number: 7
  api_key: fake
{tracker_extra}
workspace:
  root: {}
agent:
  provider: codex
providers:
  codex:
    command: "{}"
{}
"#,
                workspace_root.display(),
                command.replace('"', "\\\""),
                indent_provider_extra(provider_extra),
            ))
            .unwrap(),
            prompt_template: "Prompt".to_string(),
        };
        Settings::from_workflow(&definition).unwrap()
    }

    fn indent_provider_extra(extra: &str) -> String {
        if extra.trim().is_empty() {
            String::new()
        } else {
            extra
                .lines()
                .map(|line| {
                    if line.trim().is_empty() {
                        String::new()
                    } else {
                        format!("    {}", line.trim_start())
                    }
                })
                .collect::<Vec<_>>()
                .join("\n")
        }
    }

    fn tracker(settings: &Settings) -> Arc<GitHubTracker> {
        Arc::new(GitHubTracker::new(settings.tracker.clone()).unwrap())
    }

    #[tokio::test]
    async fn rejects_workspace_root_as_cwd() {
        let dir = tempdir().unwrap();
        let settings = settings_with_command(dir.path(), "printf ''", "", "");
        let tracker = tracker(&settings);

        let error = CodexSession::start(&settings, tracker, dir.path())
            .await
            .err()
            .expect("workspace root should be rejected")
            .to_string();
        assert!(error.contains("invalid_workspace_cwd"));
    }

    #[tokio::test]
    async fn surfaces_turn_input_required_as_error() {
        let dir = tempdir().unwrap();
        let workspace_root = dir.path().join("workspaces");
        let workspace = workspace_root.join("ISSUE-1");
        fs::create_dir_all(&workspace).unwrap();

        let script = dir.path().join("fake-codex.sh");
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
      printf '%s\n' '{"id":2,"result":{"thread":{"id":"thread-1"}}}'
      ;;
    3)
      printf '%s\n' '{"id":3,"result":{"turn":{"id":"turn-1"}}}'
      ;;
    4)
      printf '%s\n' '{"method":"turn/input_required","params":{"requiresInput":true}}'
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
            "",
            r#"  turn_sandbox_policy:
    type: workspaceWrite
    networkAccess: true"#,
        );
        let tracker = tracker(&settings);
        let mut session = CodexSession::start(&settings, tracker, &workspace)
            .await
            .unwrap();
        let (tx, mut rx) = unbounded_channel();

        let error = session
            .run_turn(&issue("ISSUE-1"), "prompt", &tx)
            .await
            .unwrap_err()
            .to_string();

        assert!(error.contains("turn_input_required"));
        let mut saw_input_required = false;
        while let Ok(event) = rx.try_recv() {
            if matches!(event.event, AgentEventKind::TurnInputRequired) {
                saw_input_required = true;
            }
        }
        assert!(saw_input_required);
    }

    #[tokio::test]
    async fn approval_requests_fail_under_default_policy() {
        let dir = tempdir().unwrap();
        let workspace_root = dir.path().join("workspaces");
        let workspace = workspace_root.join("ISSUE-2");
        fs::create_dir_all(&workspace).unwrap();

        let script = dir.path().join("approval-codex.sh");
        fs::write(
            &script,
            r#"#!/bin/sh
count=0
while IFS= read -r _line; do
  count=$((count + 1))
  case "$count" in
    1) printf '%s\n' '{"id":1,"result":{}}' ;;
    2) printf '%s\n' '{"id":2,"result":{"thread":{"id":"thread-2"}}}' ;;
    3) printf '%s\n' '{"id":3,"result":{"turn":{"id":"turn-2"}}}' ;;
    4) printf '%s\n' '{"id":99,"method":"item/commandExecution/requestApproval","params":{"command":"gh pr view"}}' ;;
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

        let settings =
            settings_with_command(&workspace_root, &script.display().to_string(), "", "");
        let tracker = tracker(&settings);
        let mut session = CodexSession::start(&settings, tracker, &workspace)
            .await
            .unwrap();
        let (tx, _rx) = unbounded_channel();

        let error = session
            .run_turn(&issue("ISSUE-2"), "prompt", &tx)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("approval_required"));
    }

    #[tokio::test]
    async fn auto_approves_command_requests_when_policy_is_never() {
        let dir = tempdir().unwrap();
        let workspace_root = dir.path().join("workspaces");
        let workspace = workspace_root.join("ISSUE-3");
        fs::create_dir_all(&workspace).unwrap();
        let trace_file = dir.path().join("approval.trace");

        let script = dir.path().join("auto-approval-codex.sh");
        fs::write(
            &script,
            format!(
                r#"#!/bin/sh
trace_file='{}'
count=0
while IFS= read -r line; do
  count=$((count + 1))
  printf '%s\n' "$line" >> "$trace_file"
  case "$count" in
    1) printf '%s\n' '{{"id":1,"result":{{}}}}' ;;
    2) printf '%s\n' '{{"id":2,"result":{{"thread":{{"id":"thread-3"}}}}}}' ;;
    3) printf '%s\n' '{{"id":3,"result":{{"turn":{{"id":"turn-3"}}}}}}' ;;
    4) printf '%s\n' '{{"id":99,"method":"item/commandExecution/requestApproval","params":{{"command":"gh pr view"}}}}' ;;
    5) printf '%s\n' '{{"method":"turn/completed"}}' ;;
  esac
done
"#,
                trace_file.display()
            ),
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
            "",
            r#"  approval_policy: never
  model: gpt-5.4
  reasoning_effort: high
  fast: true"#,
        );
        let tracker = tracker(&settings);
        let mut session = CodexSession::start(&settings, tracker, &workspace)
            .await
            .unwrap();
        let (tx, _rx) = unbounded_channel();

        let result = session
            .run_turn(&issue("ISSUE-3"), "prompt", &tx)
            .await
            .unwrap();
        assert_eq!(result.turn_id, "turn-3");

        let trace = fs::read_to_string(trace_file).unwrap();
        assert!(trace.contains(r#""decision":"acceptForSession""#));
        assert!(trace.contains(r#""method":"thread/start""#));
        assert!(trace.contains(r#""method":"turn/start""#));
        assert!(trace.contains(r#""model":"gpt-5.4""#));
        assert!(trace.contains(r#""effort":"high""#));
        assert!(trace.contains(r#""serviceTier":"fast""#));
    }

    #[test]
    fn strips_ansi_sequences_from_stderr_lines() {
        let line = "\u{1b}[2m2026-03-16T00:00:00Z\u{1b}[0m \u{1b}[31mERROR\u{1b}[0m boom";
        assert_eq!(
            super::strip_ansi_sequences(line),
            "2026-03-16T00:00:00Z ERROR boom"
        );
    }

    #[test]
    fn filters_codex_telemetry_noise() {
        assert!(super::is_noisy_codex_stderr(
            "INFO codex_otel.log_only: event.name=codex.tool_result app.version=0.114.0"
        ));
        assert!(super::is_noisy_codex_stderr("Wall time: 0.0000 seconds"));
        assert!(super::is_noisy_codex_stderr(
            "WARN codex_core::file_watcher: failed to unwatch /root/.codex/skills/.system: No watch was found."
        ));
        assert!(super::is_noisy_codex_stderr(
            "WARN codex_core::analytics_client: events failed with status 403 Forbidden: <!DOCTYPE html><html><title>Just a moment...</title>"
        ));
        assert!(super::is_noisy_codex_stderr(
            "WARN session_init:shell_snapshot{thread_id=abc}: codex_core::shell_snapshot: Failed to delete shell snapshot at \"/root/.codex/shell_snapshots/x\": Os { code: 2, kind: NotFound, message: \"No such file or directory\" }"
        ));
        assert!(super::is_noisy_codex_stderr(
            "- If the failure is a non-fast-forward or sync problem, run the `pull`"
        ));
        assert!(super::is_noisy_codex_stderr(
            "To see what failed, try: gh run view 23131072410 --log-failed"
        ));
    }

    #[test]
    fn detects_error_like_stderr_lines() {
        assert!(super::looks_like_error("Error: invalid_workflow_config"));
        assert!(super::looks_like_error("permission denied"));
        assert!(!super::looks_like_error("origin /seed-repo (fetch)"));
    }

    #[tokio::test]
    async fn unsupported_tool_calls_return_failure_without_stalling() {
        let dir = tempdir().unwrap();
        let workspace_root = dir.path().join("workspaces");
        let workspace = workspace_root.join("ISSUE-4");
        fs::create_dir_all(&workspace).unwrap();
        let trace_file = dir.path().join("tool.trace");

        let script = dir.path().join("tool-codex.sh");
        fs::write(
            &script,
            format!(
                r#"#!/bin/sh
trace_file='{}'
count=0
while IFS= read -r line; do
  count=$((count + 1))
  printf '%s\n' "$line" >> "$trace_file"
  case "$count" in
    1) printf '%s\n' '{{"id":1,"result":{{}}}}' ;;
    2) printf '%s\n' '{{"id":2,"result":{{"thread":{{"id":"thread-4"}}}}}}' ;;
    3) printf '%s\n' '{{"id":3,"result":{{"turn":{{"id":"turn-4"}}}}}}' ;;
    4) printf '%s\n' '{{"id":100,"method":"item/tool/call","params":{{"tool":"unknown_tool","arguments":{{}}}}}}' ;;
    5) printf '%s\n' '{{"method":"turn/completed"}}' ;;
  esac
done
"#,
                trace_file.display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&script).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&script, perms).unwrap();
        }

        let settings =
            settings_with_command(&workspace_root, &script.display().to_string(), "", "");
        let tracker = tracker(&settings);
        let mut session = CodexSession::start(&settings, tracker, &workspace)
            .await
            .unwrap();
        let (tx, _rx) = unbounded_channel();

        session
            .run_turn(&issue("ISSUE-4"), "prompt", &tx)
            .await
            .unwrap();

        let trace = fs::read_to_string(trace_file).unwrap();
        assert!(trace.contains("Unsupported dynamic tool"));
    }

    #[tokio::test]
    async fn supported_github_graphql_tool_calls_return_tool_result() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .and(body_string_contains("query Viewer"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": { "viewer": { "login": "octocat" } }
            })))
            .mount(&server)
            .await;

        let dir = tempdir().unwrap();
        let workspace_root = dir.path().join("workspaces");
        let workspace = workspace_root.join("ISSUE-5");
        fs::create_dir_all(&workspace).unwrap();
        let trace_file = dir.path().join("graphql-tool.trace");

        let script = dir.path().join("graphql-tool-codex.sh");
        fs::write(
            &script,
            format!(
                r#"#!/bin/sh
trace_file='{}'
count=0
while IFS= read -r line; do
  count=$((count + 1))
  printf '%s\n' "$line" >> "$trace_file"
  case "$count" in
    1) printf '%s\n' '{{"id":1,"result":{{}}}}' ;;
    2) printf '%s\n' '{{"id":2,"result":{{"thread":{{"id":"thread-5"}}}}}}' ;;
    3) printf '%s\n' '{{"id":3,"result":{{"turn":{{"id":"turn-5"}}}}}}' ;;
    4) printf '%s\n' '{{"id":101,"method":"item/tool/call","params":{{"tool":"github_graphql","arguments":{{"query":"query Viewer {{ viewer {{ login }} }}"}}}}}}' ;;
    5) printf '%s\n' '{{"method":"turn/completed"}}' ;;
  esac
done
"#,
                trace_file.display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&script).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&script, perms).unwrap();
        }

        let tracker_extra = format!(
            "  endpoint: {}/graphql\n  rest_endpoint: {}",
            server.uri(),
            server.uri()
        );
        let settings = settings_with_command(
            &workspace_root,
            &script.display().to_string(),
            &tracker_extra,
            "",
        );
        let tracker = tracker(&settings);
        let mut session = CodexSession::start(&settings, tracker, &workspace)
            .await
            .unwrap();
        let (tx, _rx) = unbounded_channel();

        session
            .run_turn(&issue("ISSUE-5"), "prompt", &tx)
            .await
            .unwrap();

        let trace = fs::read_to_string(trace_file).unwrap();
        assert!(trace.contains(r#""success":true"#));
        assert!(trace.contains("octocat"));
    }
}
