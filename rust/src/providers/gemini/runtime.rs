use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use chrono::Utc;
use serde_json::{json, Value as JsonValue};
use tokio::io::{AsyncBufReadExt, BufReader, Lines};
use tokio::process::{ChildStderr, ChildStdout, Command};
use tokio::sync::mpsc::UnboundedSender;
use tokio::time::{timeout, Duration};
use tracing::{debug, warn};

use crate::agent::{AgentBackend, AgentEvent, AgentEventKind, AgentSession, TurnResult};
use crate::config::Settings;
use crate::github::GitHubTracker;
use crate::model::Issue;

use super::config::GeminiConfig;

#[derive(Debug, Clone)]
pub struct GeminiBackend;

#[async_trait]
impl AgentBackend for GeminiBackend {
    async fn start_session(
        &self,
        settings: &Settings,
        tracker: Arc<GitHubTracker>,
        workspace: &Path,
    ) -> Result<Box<dyn AgentSession>> {
        Ok(Box::new(GeminiSession::start(
            settings,
            tracker.settings().api_key.as_str(),
            workspace,
        )?))
    }
}

pub struct GeminiSession {
    config: GeminiConfig,
    github_token: String,
    workspace: PathBuf,
    current_session_id: Option<String>,
    last_process_id: Option<u32>,
    turn_counter: u64,
}

#[async_trait]
impl AgentSession for GeminiSession {
    async fn run_turn(
        &mut self,
        issue: &Issue,
        prompt: &str,
        on_event: &UnboundedSender<AgentEvent>,
    ) -> Result<TurnResult> {
        self.run_turn_internal(issue, prompt, on_event).await
    }

    async fn stop(&mut self) -> Result<()> {
        Ok(())
    }

    fn process_id(&self) -> Option<u32> {
        self.last_process_id
    }
}

impl GeminiSession {
    pub fn start(settings: &Settings, github_token: &str, workspace: &Path) -> Result<Self> {
        validate_workspace_cwd(&settings.workspace.root, workspace)?;

        Ok(Self {
            config: super::config::load(settings)?,
            github_token: github_token.to_string(),
            workspace: workspace.to_path_buf(),
            current_session_id: None,
            last_process_id: None,
            turn_counter: 0,
        })
    }

    async fn run_turn_internal(
        &mut self,
        issue: &Issue,
        prompt: &str,
        on_event: &UnboundedSender<AgentEvent>,
    ) -> Result<TurnResult> {
        super::mcp::ensure_github_mcp_server()
            .context("failed to configure Gemini GitHub MCP server")?;

        self.turn_counter += 1;
        let turn_id = format!("turn-{}", self.turn_counter);

        let argv = self.cli_argv(prompt);
        let mut command = Command::new(&argv[0]);
        command
            .args(&argv[1..])
            .current_dir(&self.workspace)
            .env("GITHUB_TOKEN", self.github_token.as_str())
            .env("GH_TOKEN", self.github_token.as_str())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        debug!(issue_identifier = %issue.identifier, argv = ?argv, "launching Gemini");
        let mut child = command
            .spawn()
            .with_context(|| format!("failed to launch Gemini CLI for {}", issue.identifier))?;
        self.last_process_id = child.id();
        debug!(issue_identifier = %issue.identifier, pid = ?child.id(), "Gemini process spawned");

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("missing_gemini_stdout"))?;
        let stderr_lines = Arc::new(Mutex::new(Vec::new()));
        let stderr_logger = child.stderr.take().map(|stderr| {
            let stderr_lines = Arc::clone(&stderr_lines);
            tokio::spawn(log_stderr(stderr, stderr_lines))
        });

        let result = self
            .await_turn_completion(BufReader::new(stdout).lines(), &turn_id, on_event)
            .await;
        let timeout_reason = result.as_ref().err().and_then(|error| {
            let error_text = error.to_string();
            match error_text.as_str() {
                "turn_timeout" => Some("turn_timeout"),
                "turn_stalled" => Some("turn_stalled"),
                _ => None,
            }
        });
        if let Some(reason) = timeout_reason {
            child.kill().await.ok();
            emit_event(
                on_event,
                AgentEventKind::TurnEndedWithError,
                json!({
                    "reason": reason,
                    "turn_id": turn_id,
                }),
                self.current_session_id.clone(),
                self.last_process_id,
            );
        }

        let status = child
            .wait()
            .await
            .context("failed to wait for Gemini turn")?;
        debug!(issue_identifier = %issue.identifier, exit_status = %status, "Gemini process exited");
        if let Some(task) = stderr_logger {
            let _ = task.await;
        }
        let stderr_summary = summarize_stderr(&stderr_lines);

        match result {
            Ok(result) => {
                if !status.success() {
                    if let Some(stderr) = stderr_summary {
                        return Err(anyhow!(
                            "gemini_turn_failed: process_exited={status}; stderr={stderr}"
                        ));
                    }
                    return Err(anyhow!("gemini_turn_failed: process_exited={status}"));
                }
                self.current_session_id = Some(result.session_id.clone());
                Ok(result)
            }
            Err(error) => {
                if !status.success() {
                    if let Some(stderr) = stderr_summary {
                        return Err(anyhow!("{error}; process_exited={status}; stderr={stderr}"));
                    }
                    return Err(anyhow!("{error}; process_exited={status}"));
                }
                Err(error)
            }
        }
    }

    fn cli_argv(&self, prompt: &str) -> Vec<String> {
        let mut argv: Vec<String> = self
            .config
            .command
            .split_whitespace()
            .map(str::to_string)
            .collect();

        argv.extend([
            "--prompt".to_string(),
            prompt.to_string(),
            "--output-format".to_string(),
            "stream-json".to_string(),
            "--approval-mode".to_string(),
            self.config.approval_mode.clone(),
        ]);
        if let Some(model) = self.config.model.as_ref() {
            argv.push("--model".to_string());
            argv.push(model.clone());
        }
        if self.config.sandbox == Some(true) {
            argv.push("--sandbox".to_string());
        }
        if let Some(session_id) = self.current_session_id.as_ref() {
            argv.push("--resume".to_string());
            argv.push(session_id.clone());
        }

        argv
    }

    async fn await_turn_completion(
        &self,
        mut stdout: Lines<BufReader<ChildStdout>>,
        turn_id: &str,
        on_event: &UnboundedSender<AgentEvent>,
    ) -> Result<TurnResult> {
        let turn_started = Instant::now();
        let mut current_session_id = self.current_session_id.clone();
        let mut saw_approval_required = false;
        let mut emitted_approval_required = false;
        let mut last_event_at = Instant::now();

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

            let line = timeout(wait_for, stdout.next_line())
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
                .context("failed to read Gemini output line")?;

            let Some(line) = line else {
                warn!("Gemini stdout closed without a result message");
                return Err(anyhow!("gemini_stream_ended_without_result"));
            };
            last_event_at = Instant::now();
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            debug!(stdout_line = trimmed, "Gemini stdout");
            if !looks_like_json_line(trimmed) {
                emit_event(
                    on_event,
                    AgentEventKind::Notification,
                    json!({ "message": trimmed }),
                    current_session_id.clone(),
                    self.last_process_id,
                );
                continue;
            }

            let message = match serde_json::from_str::<JsonValue>(trimmed) {
                Ok(message) => message,
                Err(error) => {
                    emit_event(
                        on_event,
                        AgentEventKind::Malformed,
                        json!({
                            "error": error.to_string(),
                            "line": trimmed,
                        }),
                        current_session_id.clone(),
                        self.last_process_id,
                    );
                    continue;
                }
            };

            if let Some(session_id) = message.get("session_id").and_then(JsonValue::as_str) {
                current_session_id = Some(session_id.to_string());
            }

            match message.get("type").and_then(JsonValue::as_str) {
                Some("init") => {
                    if let Some(session_id) = current_session_id.as_ref() {
                        emit_event(
                            on_event,
                            AgentEventKind::SessionStarted,
                            json!({
                                "session_id": session_id,
                                "thread_id": session_id,
                                "turn_id": turn_id,
                            }),
                            Some(session_id.clone()),
                            self.last_process_id,
                        );
                    }
                }
                Some("message") => {
                    handle_message_event(
                        on_event,
                        &message,
                        current_session_id.clone(),
                        self.last_process_id,
                    );
                }
                Some("tool_use") => emit_event(
                    on_event,
                    AgentEventKind::Notification,
                    json!({
                        "tool_name": message.get("tool_name"),
                        "tool_id": message.get("tool_id"),
                        "parameters": message.get("parameters"),
                    }),
                    current_session_id.clone(),
                    self.last_process_id,
                ),
                Some("tool_result") => {
                    let approval_required = tool_result_requires_approval(&message);
                    let success = is_success_status(&message);
                    let event_kind = if approval_required {
                        saw_approval_required = true;
                        emitted_approval_required = true;
                        AgentEventKind::ApprovalRequired
                    } else if success {
                        AgentEventKind::ToolCallCompleted
                    } else {
                        AgentEventKind::ToolCallFailed
                    };
                    emit_event(
                        on_event,
                        event_kind,
                        message,
                        current_session_id.clone(),
                        self.last_process_id,
                    );
                }
                Some("error") => {
                    let approval_required = message_requires_approval(&message);
                    if approval_required {
                        emit_event(
                            on_event,
                            AgentEventKind::ApprovalRequired,
                            message.clone(),
                            current_session_id.clone(),
                            self.last_process_id,
                        );
                        return Err(anyhow!("approval_required"));
                    }

                    emit_event(
                        on_event,
                        AgentEventKind::TurnFailed,
                        message.clone(),
                        current_session_id.clone(),
                        self.last_process_id,
                    );
                    return Err(anyhow!(result_message(&message, "gemini_turn_failed")));
                }
                Some("result") => {
                    let success = is_success_status(&message);
                    if !success {
                        let approval_required = message_requires_approval(&message);
                        if approval_required {
                            if !emitted_approval_required {
                                emit_event(
                                    on_event,
                                    AgentEventKind::ApprovalRequired,
                                    message.clone(),
                                    current_session_id.clone(),
                                    self.last_process_id,
                                );
                            }
                            return Err(anyhow!("approval_required"));
                        }

                        emit_event(
                            on_event,
                            AgentEventKind::TurnFailed,
                            message.clone(),
                            current_session_id.clone(),
                            self.last_process_id,
                        );
                        return Err(anyhow!(result_message(&message, "gemini_turn_failed")));
                    }

                    if saw_approval_required {
                        if !emitted_approval_required {
                            emit_event(
                                on_event,
                                AgentEventKind::ApprovalRequired,
                                message.clone(),
                                current_session_id.clone(),
                                self.last_process_id,
                            );
                        }
                        return Err(anyhow!("approval_required"));
                    }

                    emit_event(
                        on_event,
                        AgentEventKind::TurnCompleted,
                        message.clone(),
                        current_session_id.clone(),
                        self.last_process_id,
                    );

                    let session_id = current_session_id
                        .clone()
                        .unwrap_or_else(|| turn_id.to_string());
                    return Ok(TurnResult {
                        session_id: session_id.clone(),
                        thread_id: session_id,
                        turn_id: turn_id.to_string(),
                    });
                }
                Some(_) => {
                    emit_event(
                        on_event,
                        AgentEventKind::OtherMessage,
                        message,
                        current_session_id.clone(),
                        self.last_process_id,
                    );
                }
                None => {
                    emit_event(
                        on_event,
                        AgentEventKind::Malformed,
                        json!({
                            "error": "missing_type",
                            "message": message,
                        }),
                        current_session_id.clone(),
                        self.last_process_id,
                    );
                }
            }
        }
    }
}

fn looks_like_json_line(line: &str) -> bool {
    line.starts_with('{') || line.starts_with('[')
}

fn handle_message_event(
    on_event: &UnboundedSender<AgentEvent>,
    message: &JsonValue,
    session_id: Option<String>,
    process_id: Option<u32>,
) {
    if message.get("role").and_then(JsonValue::as_str) != Some("assistant") {
        return;
    }

    emit_event(
        on_event,
        AgentEventKind::Notification,
        message.clone(),
        session_id,
        process_id,
    );
}

fn is_success_status(message: &JsonValue) -> bool {
    message
        .get("status")
        .and_then(JsonValue::as_str)
        .unwrap_or("error")
        == "success"
}

fn tool_result_requires_approval(message: &JsonValue) -> bool {
    if is_success_status(message) {
        return false;
    }

    let output = message
        .get("output")
        .map(render_json_field)
        .unwrap_or_default()
        .to_ascii_lowercase();
    output.contains("approval")
        || output.contains("permission")
        || output.contains("confirm")
        || output.contains("requires consent")
}

fn message_requires_approval(message: &JsonValue) -> bool {
    let text = result_message(message, "").to_ascii_lowercase();
    text.contains("approval")
        || text.contains("permission")
        || text.contains("confirm")
        || text.contains("requires consent")
}

fn result_message(message: &JsonValue, fallback: &str) -> String {
    message
        .get("error")
        .map(render_json_field)
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            message
                .get("message")
                .map(render_json_field)
                .filter(|value| !value.trim().is_empty())
        })
        .or_else(|| {
            message
                .get("status")
                .and_then(JsonValue::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| fallback.to_string())
}

fn render_json_field(value: &JsonValue) -> String {
    match value {
        JsonValue::String(value) => value.clone(),
        _ => value.to_string(),
    }
}

fn emit_event(
    on_event: &UnboundedSender<AgentEvent>,
    event: AgentEventKind,
    payload: JsonValue,
    session_id: Option<String>,
    process_id: Option<u32>,
) {
    let _ = on_event.send(AgentEvent {
        event,
        timestamp: Utc::now(),
        payload,
        session_id,
        agent_process_pid: process_id.map(|value| value.to_string()),
    });
}

async fn log_stderr(stderr: ChildStderr, captured_lines: Arc<Mutex<Vec<String>>>) {
    let mut lines = BufReader::new(stderr).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(mut captured) = captured_lines.lock() {
            if captured.len() >= 5 {
                captured.remove(0);
            }
            captured.push(trimmed.to_string());
        }
        warn!(provider = "gemini", stderr = trimmed, "Gemini stderr");
    }
}

fn summarize_stderr(captured_lines: &Arc<Mutex<Vec<String>>>) -> Option<String> {
    let captured = captured_lines.lock().ok()?;
    if captured.is_empty() {
        None
    } else {
        Some(captured.join(" | "))
    }
}

fn validate_workspace_cwd(root: &Path, workspace: &Path) -> Result<()> {
    let root = std::fs::canonicalize(root)
        .with_context(|| format!("failed to resolve workspace root {}", root.display()))?;
    let workspace = std::fs::canonicalize(workspace)
        .with_context(|| format!("failed to resolve workspace {}", workspace.display()))?;

    if workspace == root {
        return Err(anyhow!("invalid_workspace_cwd: workspace_root"));
    }

    if !workspace.starts_with(&root) {
        return Err(anyhow!("invalid_workspace_cwd: outside_workspace_root"));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;
    use tokio::sync::mpsc::unbounded_channel;

    use crate::agent::AgentSession;
    use crate::config::Settings;
    use crate::model::{Issue, WorkflowDefinition};

    use super::{AgentEventKind, GeminiSession};

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
workspace:
  root: {}
agent:
  provider: gemini
providers:
  gemini:
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

    #[test]
    fn rejects_workspace_root_as_cwd() {
        let dir = tempdir().unwrap();
        let settings = settings_with_command(dir.path(), "printf ''", "");

        let error = GeminiSession::start(&settings, "fake", dir.path())
            .err()
            .expect("workspace root should be rejected")
            .to_string();
        assert!(error.contains("invalid_workspace_cwd"));
    }

    #[tokio::test]
    async fn completes_turn_from_stream_json_output() {
        let dir = tempdir().unwrap();
        let workspace_root = dir.path().join("workspaces");
        let workspace = workspace_root.join("ISSUE-1");
        fs::create_dir_all(&workspace).unwrap();

        let script = dir.path().join("fake-gemini.sh");
        fs::write(
            &script,
            r#"#!/bin/sh
printf '%s\n' 'Loaded cached credentials.'
printf '%s\n' '{"type":"init","session_id":"gemini-session-1","model":"auto-gemini-3"}'
printf '%s\n' '{"type":"message","role":"assistant","content":"Checking the repo","delta":true}'
printf '%s\n' '{"type":"tool_use","tool_name":"run_shell_command","tool_id":"tool-1","parameters":{"command":"pwd"}}'
printf '%s\n' '{"type":"tool_result","tool_id":"tool-1","status":"success","output":"/tmp/workspace"}'
printf '%s\n' '{"type":"result","status":"success","stats":{"total_tokens":7}}'
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

        let settings = settings_with_command(&workspace_root, &script.display().to_string(), "");
        let mut session = GeminiSession::start(&settings, "fake", &workspace).unwrap();
        let (tx, mut rx) = unbounded_channel();

        let result = session
            .run_turn(&issue("ISSUE-1"), "prompt", &tx)
            .await
            .unwrap();
        assert_eq!(result.session_id, "gemini-session-1");

        let mut saw_session_started = false;
        let mut saw_tool_complete = false;
        let mut saw_turn_complete = false;
        let mut saw_preamble_notification = false;
        while let Ok(event) = rx.try_recv() {
            match event.event {
                AgentEventKind::SessionStarted => saw_session_started = true,
                AgentEventKind::ToolCallCompleted => saw_tool_complete = true,
                AgentEventKind::TurnCompleted => saw_turn_complete = true,
                AgentEventKind::Notification => {
                    if event
                        .payload
                        .get("message")
                        .and_then(|value| value.as_str())
                        == Some("Loaded cached credentials.")
                    {
                        saw_preamble_notification = true;
                    }
                }
                _ => {}
            }
        }

        assert!(saw_session_started);
        assert!(saw_tool_complete);
        assert!(saw_turn_complete);
        assert!(saw_preamble_notification);
    }

    #[tokio::test]
    async fn resumes_previous_session_for_follow_up_turns() {
        let dir = tempdir().unwrap();
        let workspace_root = dir.path().join("workspaces");
        let workspace = workspace_root.join("ISSUE-2");
        fs::create_dir_all(&workspace).unwrap();
        let trace_file = dir.path().join("gemini.trace");

        let script = dir.path().join("trace-gemini.sh");
        fs::write(
            &script,
            format!(
                r#"#!/bin/sh
trace_file='{}'
printf '%s\n' "$*" >> "$trace_file"
if printf '%s' "$*" | grep -q -- '--resume'; then
  session_id='gemini-session-2'
else
  session_id='gemini-session-1'
fi
printf '%s\n' "{{\"type\":\"init\",\"session_id\":\"${{session_id}}\",\"model\":\"auto-gemini-3\"}}"
printf '%s\n' "{{\"type\":\"message\",\"role\":\"assistant\",\"content\":\"ok\",\"delta\":true}}"
printf '%s\n' "{{\"type\":\"result\",\"status\":\"success\",\"session_id\":\"${{session_id}}\"}}"
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

        let settings = settings_with_command(&workspace_root, &script.display().to_string(), "");
        let mut session = GeminiSession::start(&settings, "fake", &workspace).unwrap();
        let (tx, _rx) = unbounded_channel();

        let first = session
            .run_turn(&issue("ISSUE-2"), "prompt 1", &tx)
            .await
            .unwrap();
        assert_eq!(first.session_id, "gemini-session-1");

        let second = session
            .run_turn(&issue("ISSUE-2"), "prompt 2", &tx)
            .await
            .unwrap();
        assert_eq!(second.session_id, "gemini-session-2");

        let trace = fs::read_to_string(trace_file).unwrap();
        assert!(trace.contains("--resume gemini-session-1"));
    }
}
