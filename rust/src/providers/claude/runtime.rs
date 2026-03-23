use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use chrono::Utc;
use serde_json::{json, Value as JsonValue};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{ChildStderr, ChildStdout, Command};
use tokio::sync::mpsc::UnboundedSender;
use tokio::time::{timeout, Duration};
use tracing::warn;

use crate::agent::{AgentBackend, AgentEvent, AgentEventKind, AgentSession, TurnResult};
use crate::config::Settings;
use crate::github::GitHubTracker;
use crate::model::Issue;

use super::config::ClaudeConfig;

#[derive(Debug, Clone)]
pub struct ClaudeBackend;

#[async_trait]
impl AgentBackend for ClaudeBackend {
    async fn start_session(
        &self,
        settings: &Settings,
        tracker: Arc<GitHubTracker>,
        workspace: &Path,
    ) -> Result<Box<dyn AgentSession>> {
        Ok(Box::new(ClaudeSession::start(
            settings,
            tracker.settings().api_key.as_str(),
            workspace,
        )?))
    }
}

pub struct ClaudeSession {
    config: ClaudeConfig,
    github_token: String,
    workspace: PathBuf,
    current_session_id: Option<String>,
    last_process_id: Option<u32>,
    turn_counter: u64,
}

#[async_trait]
impl AgentSession for ClaudeSession {
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

impl ClaudeSession {
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
        self.turn_counter += 1;
        let turn_id = format!("turn-{}", self.turn_counter);

        let mut command = Command::new("bash");
        command
            .arg("-lc")
            .arg(self.cli_command())
            .current_dir(&self.workspace)
            .env("GITHUB_TOKEN", self.github_token.as_str())
            .env("GH_TOKEN", self.github_token.as_str())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let mut child = command
            .spawn()
            .with_context(|| format!("failed to launch Claude Code for {}", issue.identifier))?;
        self.last_process_id = child.id();

        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("missing_claude_stdin"))?;
        stdin
            .write_all(prompt.as_bytes())
            .await
            .context("failed to write Claude prompt")?;
        stdin
            .shutdown()
            .await
            .context("failed to close Claude stdin")?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("missing_claude_stdout"))?;
        let stderr_lines = Arc::new(Mutex::new(Vec::new()));
        let stderr_logger = child.stderr.take().map(|stderr| {
            let stderr_lines = Arc::clone(&stderr_lines);
            tokio::spawn(log_stderr(stderr, stderr_lines))
        });

        let parse_result = timeout(
            Duration::from_millis(self.config.turn_timeout_ms),
            self.await_turn_completion(BufReader::new(stdout).lines(), &turn_id, on_event),
        )
        .await;

        let result = match parse_result {
            Ok(result) => result,
            Err(_) => {
                child.kill().await.ok();
                emit_event(
                    on_event,
                    AgentEventKind::TurnEndedWithError,
                    json!({
                        "reason": "turn_timeout",
                        "turn_id": turn_id,
                    }),
                    self.current_session_id.clone(),
                    self.last_process_id,
                );
                return Err(anyhow!("turn_timeout"));
            }
        };

        let status = child
            .wait()
            .await
            .context("failed to wait for Claude turn")?;
        if let Some(task) = stderr_logger {
            let _ = task.await;
        }
        let stderr_summary = summarize_stderr(&stderr_lines);
        match result {
            Ok(result) => {
                if !status.success() {
                    if let Some(stderr) = stderr_summary {
                        return Err(anyhow!(
                            "claude_turn_failed: process_exited={status}; stderr={stderr}"
                        ));
                    }
                    return Err(anyhow!("claude_turn_failed: process_exited={status}"));
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

    fn cli_command(&self) -> String {
        let mut args = vec![
            "--print".to_string(),
            "--verbose".to_string(),
            "--output-format".to_string(),
            "stream-json".to_string(),
            "--permission-mode".to_string(),
            self.config.permission_mode.clone(),
        ];
        if let Some(model) = self.config.model.as_ref() {
            args.push("--model".to_string());
            args.push(model.clone());
        }
        if let Some(effort) = self.config.reasoning_effort.as_ref() {
            args.push("--effort".to_string());
            args.push(effort.clone());
        }
        if let Some(session_id) = self.current_session_id.as_ref() {
            args.push("--resume".to_string());
            args.push(session_id.clone());
        }

        let mut command = self.config.command.clone();
        for arg in args {
            command.push(' ');
            command.push_str(&shell_escape(&arg));
        }
        command
    }

    async fn await_turn_completion(
        &self,
        mut stdout: Lines<BufReader<ChildStdout>>,
        turn_id: &str,
        on_event: &UnboundedSender<AgentEvent>,
    ) -> Result<TurnResult> {
        let mut current_session_id = self.current_session_id.clone();
        let mut saw_permission_denial = false;
        let mut emitted_approval_required = false;

        while let Some(line) = stdout
            .next_line()
            .await
            .context("failed to read Claude output line")?
        {
            if line.trim().is_empty() {
                continue;
            }

            let message = match serde_json::from_str::<JsonValue>(&line) {
                Ok(message) => message,
                Err(error) => {
                    emit_event(
                        on_event,
                        AgentEventKind::Malformed,
                        json!({
                            "error": error.to_string(),
                            "line": line,
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
                Some("system")
                    if message.get("subtype").and_then(JsonValue::as_str) == Some("init") =>
                {
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
                Some("assistant") => {
                    handle_assistant_message(
                        on_event,
                        &message,
                        current_session_id.clone(),
                        self.last_process_id,
                    );
                }
                Some("user") => {
                    let permission_denied = handle_user_message(
                        on_event,
                        &message,
                        current_session_id.clone(),
                        self.last_process_id,
                    );
                    if permission_denied {
                        saw_permission_denial = true;
                        emitted_approval_required = true;
                    }
                }
                Some("result") => {
                    let denied = permission_denials(&message);
                    if denied {
                        saw_permission_denial = true;
                    }

                    if message
                        .get("is_error")
                        .and_then(JsonValue::as_bool)
                        .unwrap_or(false)
                    {
                        emit_event(
                            on_event,
                            AgentEventKind::TurnFailed,
                            message.clone(),
                            current_session_id.clone(),
                            self.last_process_id,
                        );
                        return Err(anyhow!(result_message(&message)));
                    }

                    if saw_permission_denial {
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

        Err(anyhow!("claude_stream_ended_without_result"))
    }
}

fn handle_assistant_message(
    on_event: &UnboundedSender<AgentEvent>,
    message: &JsonValue,
    session_id: Option<String>,
    process_id: Option<u32>,
) {
    let Some(content) = message
        .get("message")
        .and_then(|value| value.get("content"))
        .and_then(JsonValue::as_array)
    else {
        emit_event(
            on_event,
            AgentEventKind::Notification,
            message.clone(),
            session_id,
            process_id,
        );
        return;
    };

    for block in content {
        match block.get("type").and_then(JsonValue::as_str) {
            Some("tool_use") => emit_event(
                on_event,
                AgentEventKind::Notification,
                json!({
                    "tool_name": block.get("name"),
                    "tool_use_id": block.get("id"),
                    "tool_input": block.get("input"),
                }),
                session_id.clone(),
                process_id,
            ),
            Some("text") | Some("thinking") => emit_event(
                on_event,
                AgentEventKind::Notification,
                block.clone(),
                session_id.clone(),
                process_id,
            ),
            _ => emit_event(
                on_event,
                AgentEventKind::Notification,
                block.clone(),
                session_id.clone(),
                process_id,
            ),
        }
    }
}

fn handle_user_message(
    on_event: &UnboundedSender<AgentEvent>,
    message: &JsonValue,
    session_id: Option<String>,
    process_id: Option<u32>,
) -> bool {
    let mut permission_denied = false;
    let Some(content) = message
        .get("message")
        .and_then(|value| value.get("content"))
        .and_then(JsonValue::as_array)
    else {
        return false;
    };

    for block in content {
        if block.get("type").and_then(JsonValue::as_str) != Some("tool_result") {
            continue;
        }

        let is_error = block
            .get("is_error")
            .and_then(JsonValue::as_bool)
            .unwrap_or(false);
        let content_text = tool_result_text(block);
        let permission_error = is_error && content_text.contains("requested permissions");
        permission_denied |= permission_error;

        let event_kind = if permission_error {
            AgentEventKind::ApprovalRequired
        } else if is_error {
            AgentEventKind::ToolCallFailed
        } else {
            AgentEventKind::ToolCallCompleted
        };

        emit_event(
            on_event,
            event_kind,
            block.clone(),
            session_id.clone(),
            process_id,
        );
    }

    permission_denied
}

fn tool_result_text(block: &JsonValue) -> String {
    match block.get("content") {
        Some(JsonValue::String(value)) => value.clone(),
        Some(value) => value.to_string(),
        None => String::new(),
    }
}

fn permission_denials(message: &JsonValue) -> bool {
    message
        .get("permission_denials")
        .and_then(JsonValue::as_array)
        .map(|entries| !entries.is_empty())
        .unwrap_or(false)
}

fn result_message(message: &JsonValue) -> String {
    message
        .get("result")
        .and_then(JsonValue::as_str)
        .or_else(|| message.get("subtype").and_then(JsonValue::as_str))
        .unwrap_or("claude_turn_failed")
        .to_string()
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

fn shell_escape(value: &str) -> String {
    format!("'{}'", value.replace('\'', r#"'"'"'"#))
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
        warn!(provider = "claude", stderr = trimmed, "Claude stderr");
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

    use super::{AgentEventKind, ClaudeSession};

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
  provider: claude
providers:
  claude:
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

        let error = ClaudeSession::start(&settings, "fake", dir.path())
            .err()
            .expect("workspace root should be rejected")
            .to_string();
        assert!(error.contains("invalid_workspace_cwd"));
    }

    #[tokio::test]
    async fn surfaces_permission_denials_as_approval_required() {
        let dir = tempdir().unwrap();
        let workspace_root = dir.path().join("workspaces");
        let workspace = workspace_root.join("ISSUE-1");
        fs::create_dir_all(&workspace).unwrap();

        let script = dir.path().join("fake-claude.sh");
        fs::write(
            &script,
            r#"#!/bin/sh
printf '%s\n' '{"type":"system","subtype":"init","session_id":"claude-session-1"}'
printf '%s\n' '{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_1","name":"Write","input":{"file_path":"hello.txt","content":"hi"}}]}}'
printf '%s\n' '{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_1","is_error":true,"content":"Claude requested permissions to write to hello.txt, but you have not granted it yet."}]}}'
printf '%s\n' '{"type":"result","subtype":"success","is_error":false,"session_id":"claude-session-1","permission_denials":[{"tool_name":"Write","tool_use_id":"toolu_1"}],"result":"done"}'
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
        let mut session = ClaudeSession::start(&settings, "fake", &workspace).unwrap();
        let (tx, mut rx) = unbounded_channel();

        let error = session
            .run_turn(&issue("ISSUE-1"), "prompt", &tx)
            .await
            .unwrap_err()
            .to_string();

        assert!(error.contains("approval_required"));
        let mut saw_approval_required = false;
        while let Ok(event) = rx.try_recv() {
            if matches!(event.event, AgentEventKind::ApprovalRequired) {
                saw_approval_required = true;
            }
        }
        assert!(saw_approval_required);
    }

    #[tokio::test]
    async fn surfaces_stderr_when_claude_exits_before_result() {
        let dir = tempdir().unwrap();
        let workspace_root = dir.path().join("workspaces");
        let workspace = workspace_root.join("ISSUE-ERR");
        fs::create_dir_all(&workspace).unwrap();

        let script = dir.path().join("stderr-claude.sh");
        fs::write(
            &script,
            r#"#!/bin/sh
printf '%s\n' 'Not logged in · Please run /login' >&2
exit 1
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
        let mut session = ClaudeSession::start(&settings, "fake", &workspace).unwrap();
        let (tx, _rx) = unbounded_channel();

        let error = session
            .run_turn(&issue("ISSUE-ERR"), "prompt", &tx)
            .await
            .unwrap_err()
            .to_string();

        assert!(error.contains("process_exited="));
        assert!(error.contains("Not logged in"));
    }

    #[tokio::test]
    async fn resumes_previous_session_for_follow_up_turns() {
        let dir = tempdir().unwrap();
        let workspace_root = dir.path().join("workspaces");
        let workspace = workspace_root.join("ISSUE-2");
        fs::create_dir_all(&workspace).unwrap();
        let trace_file = dir.path().join("claude.trace");

        let script = dir.path().join("trace-claude.sh");
        fs::write(
            &script,
            format!(
                r#"#!/bin/sh
trace_file='{}'
printf '%s\n' "$*" >> "$trace_file"
if printf '%s' "$*" | grep -q -- '--resume'; then
  session_id='claude-session-2'
else
  session_id='claude-session-1'
fi
printf '%s\n' "{{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"${{session_id}}\"}}"
printf '%s\n' "{{\"type\":\"assistant\",\"message\":{{\"content\":[{{\"type\":\"text\",\"text\":\"ok\"}}]}}}}"
printf '%s\n' "{{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"session_id\":\"${{session_id}}\",\"result\":\"ok\"}}"
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
        let mut session = ClaudeSession::start(&settings, "fake", &workspace).unwrap();
        let (tx, _rx) = unbounded_channel();

        let first = session
            .run_turn(&issue("ISSUE-2"), "prompt 1", &tx)
            .await
            .unwrap();
        assert_eq!(first.session_id, "claude-session-1");

        let second = session
            .run_turn(&issue("ISSUE-2"), "prompt 2", &tx)
            .await
            .unwrap();
        assert_eq!(second.session_id, "claude-session-2");

        let trace = fs::read_to_string(trace_file).unwrap();
        assert!(trace.contains("--resume claude-session-1"));
    }
}
