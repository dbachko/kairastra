use std::collections::HashMap;
use std::env;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value as JsonValue};

use crate::model::WorkflowDefinition;

const DEFAULT_POLL_INTERVAL_MS: u64 = 30_000;
const DEFAULT_WORKSPACE_ROOT: &str = "symphony_workspaces";
const DEFAULT_HOOK_TIMEOUT_MS: u64 = 60_000;
const DEFAULT_MAX_CONCURRENT_AGENTS: usize = 10;
const DEFAULT_MAX_TURNS: usize = 20;
const DEFAULT_MAX_RETRY_BACKOFF_MS: u64 = 300_000;
const DEFAULT_CODEX_COMMAND: &str = "codex app-server";
const DEFAULT_READ_TIMEOUT_MS: u64 = 5_000;
const DEFAULT_TURN_TIMEOUT_MS: u64 = 3_600_000;
const DEFAULT_STALL_TIMEOUT_MS: u64 = 300_000;
const DEFAULT_WEBHOOK_PATH: &str = "/github/webhook";

#[derive(Debug, Clone)]
pub struct Settings {
    pub tracker: TrackerSettings,
    pub polling: PollingSettings,
    pub webhooks: WebhookSettings,
    pub workspace: WorkspaceSettings,
    pub hooks: HookSettings,
    pub agent: AgentSettings,
    pub codex: CodexSettings,
}

#[derive(Debug, Clone)]
pub struct TrackerSettings {
    pub kind: String,
    pub mode: GitHubMode,
    pub api_key: String,
    pub owner: String,
    pub repo: Option<String>,
    pub project_v2_number: Option<u32>,
    pub project_url: Option<String>,
    pub active_states: Vec<String>,
    pub terminal_states: Vec<String>,
    pub status_source: Option<FieldSource>,
    pub priority_source: Option<FieldSource>,
    pub graphql_endpoint: String,
    pub rest_endpoint: String,
}

#[derive(Debug, Clone)]
pub struct PollingSettings {
    pub interval_ms: u64,
}

#[derive(Debug, Clone)]
pub struct WebhookSettings {
    pub listen: Option<String>,
    pub path: String,
    pub secret: Option<String>,
}

#[derive(Debug, Clone)]
pub struct WorkspaceSettings {
    pub root: PathBuf,
}

#[derive(Debug, Clone)]
pub struct HookSettings {
    pub after_create: Option<String>,
    pub before_run: Option<String>,
    pub after_run: Option<String>,
    pub before_remove: Option<String>,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone)]
pub struct AgentSettings {
    pub max_concurrent_agents: usize,
    pub max_turns: usize,
    pub max_retry_backoff_ms: u64,
    pub assignee_login: Option<String>,
    pub max_concurrent_agents_by_state: HashMap<String, usize>,
}

#[derive(Debug, Clone)]
pub struct CodexSettings {
    pub command: String,
    pub approval_policy: JsonValue,
    pub thread_sandbox: String,
    pub turn_sandbox_policy: Option<JsonValue>,
    pub read_timeout_ms: u64,
    pub turn_timeout_ms: u64,
    pub stall_timeout_ms: u64,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GitHubMode {
    ProjectsV2,
    IssuesOnly,
}

impl Default for GitHubMode {
    fn default() -> Self {
        Self::ProjectsV2
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FieldSourceType {
    ProjectField,
    IssueField,
    GitHubState,
    Label,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FieldSource {
    #[serde(rename = "type")]
    pub source_type: FieldSourceType,
    pub name: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
struct RawSettings {
    tracker: RawTracker,
    polling: RawPolling,
    webhooks: RawWebhooks,
    workspace: RawWorkspace,
    hooks: RawHooks,
    agent: RawAgent,
    codex: RawCodex,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
struct RawTracker {
    kind: Option<String>,
    mode: GitHubMode,
    api_key: Option<String>,
    owner: Option<String>,
    repo: Option<String>,
    project_v2_number: Option<IntOrString>,
    project_url: Option<String>,
    active_states: Vec<String>,
    terminal_states: Vec<String>,
    status_source: Option<FieldSource>,
    priority_source: Option<FieldSource>,
    endpoint: Option<String>,
    rest_endpoint: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
struct RawPolling {
    interval_ms: Option<IntOrString>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
struct RawWebhooks {
    listen: Option<String>,
    path: Option<String>,
    secret: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
struct RawWorkspace {
    root: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
struct RawHooks {
    after_create: Option<String>,
    before_run: Option<String>,
    after_run: Option<String>,
    before_remove: Option<String>,
    timeout_ms: Option<IntOrString>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
struct RawAgent {
    max_concurrent_agents: Option<IntOrString>,
    max_turns: Option<IntOrString>,
    max_retry_backoff_ms: Option<IntOrString>,
    assignee_login: Option<String>,
    max_concurrent_agents_by_state: HashMap<String, IntOrString>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
struct RawCodex {
    command: Option<String>,
    approval_policy: Option<JsonValue>,
    thread_sandbox: Option<String>,
    turn_sandbox_policy: Option<JsonValue>,
    read_timeout_ms: Option<IntOrString>,
    turn_timeout_ms: Option<IntOrString>,
    stall_timeout_ms: Option<IntOrString>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum IntOrString {
    Int(u64),
    String(String),
}

impl Settings {
    pub fn from_workflow(workflow: &WorkflowDefinition) -> Result<Self> {
        let raw = serde_yaml::from_value::<RawSettings>(workflow.config.clone())
            .map_err(|error| anyhow!("invalid_workflow_config: {error}"))?;

        let tracker_kind = raw
            .tracker
            .kind
            .clone()
            .ok_or_else(|| anyhow!("missing_tracker_kind"))?;
        if tracker_kind != "github" {
            return Err(anyhow!("unsupported_tracker_kind: {tracker_kind}"));
        }

        let api_key = resolve_secret(raw.tracker.api_key, &["GITHUB_TOKEN", "GH_TOKEN"])
            .ok_or_else(|| anyhow!("missing_github_api_token"))?;
        let owner = resolve_required_string(raw.tracker.owner, "tracker.owner")?;
        let repo = resolve_optional_string(raw.tracker.repo);
        let project_v2_number = match raw.tracker.mode {
            GitHubMode::ProjectsV2 => Some(
                resolve_u32(raw.tracker.project_v2_number, "tracker.project_v2_number")?
                    .ok_or_else(|| anyhow!("missing_github_project_v2_number"))?,
            ),
            GitHubMode::IssuesOnly => None,
        };
        let project_url = resolve_optional_string(raw.tracker.project_url);

        if raw.tracker.mode == GitHubMode::IssuesOnly && repo.is_none() {
            return Err(anyhow!("missing_github_repo"));
        }

        let polling = PollingSettings {
            interval_ms: resolve_u64(
                raw.polling.interval_ms,
                DEFAULT_POLL_INTERVAL_MS,
                "polling.interval_ms",
            )?,
        };

        let webhook_listen = resolve_optional_string(raw.webhooks.listen);
        let webhook_secret = resolve_secret(raw.webhooks.secret, &["GITHUB_WEBHOOK_SECRET"]);
        if webhook_listen.is_some() && webhook_secret.is_none() {
            return Err(anyhow!(
                "invalid_workflow_config: webhooks.secret or GITHUB_WEBHOOK_SECRET is required when webhooks.listen is set"
            ));
        }
        let webhook_path = resolve_optional_string(raw.webhooks.path)
            .unwrap_or_else(|| DEFAULT_WEBHOOK_PATH.to_string());

        let workspace_root = match raw.workspace.root {
            Some(root) => expand_path(&root)?,
            None => env::temp_dir().join(DEFAULT_WORKSPACE_ROOT),
        };

        let hooks = HookSettings {
            after_create: raw.hooks.after_create,
            before_run: raw.hooks.before_run,
            after_run: raw.hooks.after_run,
            before_remove: raw.hooks.before_remove,
            timeout_ms: resolve_optional_u64(raw.hooks.timeout_ms, "hooks.timeout_ms")?
                .filter(|value| *value > 0)
                .unwrap_or(DEFAULT_HOOK_TIMEOUT_MS),
        };

        let max_concurrent_agents = resolve_usize(
            raw.agent.max_concurrent_agents,
            DEFAULT_MAX_CONCURRENT_AGENTS,
            "agent.max_concurrent_agents",
        )?;
        let max_turns = resolve_usize(raw.agent.max_turns, DEFAULT_MAX_TURNS, "agent.max_turns")?;
        let max_retry_backoff_ms = resolve_u64(
            raw.agent.max_retry_backoff_ms,
            DEFAULT_MAX_RETRY_BACKOFF_MS,
            "agent.max_retry_backoff_ms",
        )?;

        let mut state_limits = HashMap::new();
        for (state, limit) in raw.agent.max_concurrent_agents_by_state {
            let parsed = match limit {
                IntOrString::Int(value) => usize::try_from(value).ok(),
                IntOrString::String(value) => value.trim().parse::<usize>().ok(),
            };
            if let Some(value) = parsed.filter(|value| *value > 0) {
                state_limits.insert(normalize_issue_state(&state), value);
            }
        }

        let codex = CodexSettings {
            command: raw
                .codex
                .command
                .unwrap_or_else(|| DEFAULT_CODEX_COMMAND.to_string()),
            approval_policy: raw
                .codex
                .approval_policy
                .unwrap_or_else(default_approval_policy),
            thread_sandbox: raw
                .codex
                .thread_sandbox
                .unwrap_or_else(|| "workspace-write".to_string()),
            turn_sandbox_policy: raw.codex.turn_sandbox_policy,
            read_timeout_ms: resolve_u64(
                raw.codex.read_timeout_ms,
                DEFAULT_READ_TIMEOUT_MS,
                "codex.read_timeout_ms",
            )?,
            turn_timeout_ms: resolve_u64(
                raw.codex.turn_timeout_ms,
                DEFAULT_TURN_TIMEOUT_MS,
                "codex.turn_timeout_ms",
            )?,
            stall_timeout_ms: resolve_optional_u64(
                raw.codex.stall_timeout_ms,
                "codex.stall_timeout_ms",
            )?
            .unwrap_or(DEFAULT_STALL_TIMEOUT_MS),
        };

        if max_concurrent_agents == 0 {
            return Err(anyhow!(
                "invalid_workflow_config: agent.max_concurrent_agents must be > 0"
            ));
        }
        if max_turns == 0 {
            return Err(anyhow!(
                "invalid_workflow_config: agent.max_turns must be > 0"
            ));
        }
        if codex.command.is_empty() {
            return Err(anyhow!(
                "invalid_workflow_config: codex.command must not be empty"
            ));
        }

        Ok(Self {
            tracker: TrackerSettings {
                kind: tracker_kind,
                mode: raw.tracker.mode,
                api_key,
                owner,
                repo,
                project_v2_number,
                project_url,
                active_states: if raw.tracker.active_states.is_empty() {
                    vec!["Todo".to_string(), "In Progress".to_string()]
                } else {
                    raw.tracker.active_states
                },
                terminal_states: if raw.tracker.terminal_states.is_empty() {
                    vec![
                        "Closed".to_string(),
                        "Cancelled".to_string(),
                        "Canceled".to_string(),
                        "Duplicate".to_string(),
                        "Done".to_string(),
                    ]
                } else {
                    raw.tracker.terminal_states
                },
                status_source: raw.tracker.status_source,
                priority_source: raw.tracker.priority_source,
                graphql_endpoint: raw
                    .tracker
                    .endpoint
                    .unwrap_or_else(|| "https://api.github.com/graphql".to_string()),
                rest_endpoint: raw
                    .tracker
                    .rest_endpoint
                    .unwrap_or_else(|| "https://api.github.com".to_string()),
            },
            polling,
            webhooks: WebhookSettings {
                listen: webhook_listen,
                path: webhook_path,
                secret: webhook_secret,
            },
            workspace: WorkspaceSettings {
                root: workspace_root,
            },
            hooks,
            agent: AgentSettings {
                max_concurrent_agents,
                max_turns,
                max_retry_backoff_ms,
                assignee_login: resolve_optional_string(raw.agent.assignee_login)
                    .map(|value| value.to_lowercase()),
                max_concurrent_agents_by_state: state_limits,
            },
            codex,
        })
    }

    pub fn workflow_prompt(&self, workflow: &WorkflowDefinition) -> String {
        if workflow.prompt_template.trim().is_empty() {
            "You are working on a GitHub issue.\n\n{% if tracker.dashboard_url %}GitHub dashboard: {{ tracker.dashboard_url }}\n\n{% endif %}Identifier: {{ issue.identifier }}\nTitle: {{ issue.title }}\n\nBody:\n{% if issue.description %}\n{{ issue.description }}\n{% else %}\nNo description provided.\n{% endif %}\n".to_string()
        } else {
            workflow.prompt_template.clone()
        }
    }

    pub fn tracker_dashboard_url(&self) -> Option<String> {
        if let Some(project_url) = self
            .tracker
            .project_url
            .as_ref()
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
        {
            return Some(project_url.to_string());
        }

        match self.tracker.mode {
            GitHubMode::ProjectsV2 => self.tracker.project_v2_number.map(|project_number| {
                format!(
                    "https://github.com/users/{}/projects/{}",
                    self.tracker.owner, project_number
                )
            }),
            GitHubMode::IssuesOnly => self
                .tracker
                .repo
                .as_ref()
                .map(|repo| format!("https://github.com/{}/{repo}/issues", self.tracker.owner)),
        }
    }

    pub fn active_state(&self, state: &str) -> bool {
        let normalized = normalize_issue_state(state);
        self.tracker
            .active_states
            .iter()
            .any(|candidate| normalize_issue_state(candidate) == normalized)
    }

    pub fn terminal_state(&self, state: &str) -> bool {
        let normalized = normalize_issue_state(state);
        self.tracker
            .terminal_states
            .iter()
            .any(|candidate| normalize_issue_state(candidate) == normalized)
    }

    pub fn max_concurrent_agents_for_state(&self, state: &str) -> usize {
        self.agent
            .max_concurrent_agents_by_state
            .get(&normalize_issue_state(state))
            .copied()
            .unwrap_or(self.agent.max_concurrent_agents)
    }

    pub fn turn_sandbox_policy(&self, workspace: &Path) -> JsonValue {
        let workspace_root = workspace.to_string_lossy().to_string();

        match self.codex.turn_sandbox_policy.clone() {
            Some(mut policy) => {
                if let Some(object) = policy.as_object_mut() {
                    let is_workspace_write = object
                        .get("type")
                        .and_then(JsonValue::as_str)
                        .map(|value| value == "workspaceWrite")
                        .unwrap_or(false);
                    let missing_writable_roots = object
                        .get("writableRoots")
                        .map(|value| value.is_null())
                        .unwrap_or(true);

                    if is_workspace_write && missing_writable_roots {
                        object.insert("writableRoots".to_string(), json!([workspace_root]));
                    }
                }

                policy
            }
            None => json!({
                "type": "workspaceWrite",
                "writableRoots": [workspace_root]
            }),
        }
    }
}

pub fn normalize_issue_state(state: &str) -> String {
    state.trim().to_lowercase()
}

fn resolve_required_string(raw: Option<String>, field_name: &str) -> Result<String> {
    resolve_optional_string(raw)
        .ok_or_else(|| anyhow!("invalid_workflow_config: {field_name} is required"))
}

fn resolve_secret(raw: Option<String>, fallback_envs: &[&str]) -> Option<String> {
    match raw {
        Some(value) if value.starts_with('$') => env::var(value.trim_start_matches('$'))
            .ok()
            .filter(|value| !value.is_empty()),
        Some(value) if !value.trim().is_empty() => Some(value),
        _ => fallback_envs
            .iter()
            .find_map(|name| env::var(name).ok().filter(|value| !value.is_empty())),
    }
}

fn expand_path(raw: &str) -> Result<PathBuf> {
    let value = if raw.starts_with('$') && !raw.contains('/') {
        env::var(raw.trim_start_matches('$'))
            .with_context(|| format!("environment variable {raw} is not set"))?
    } else {
        raw.to_string()
    };

    let expanded = if value == "~" || value.starts_with("~/") {
        let home = dirs::home_dir().ok_or_else(|| anyhow!("home directory unavailable"))?;
        if value == "~" {
            home
        } else {
            home.join(value.trim_start_matches("~/"))
        }
    } else {
        PathBuf::from(value)
    };

    Ok(expanded)
}

fn resolve_u32(value: Option<IntOrString>, field_name: &str) -> Result<Option<u32>> {
    Ok(resolve_optional_u64(value, field_name)?
        .map(|value| u32::try_from(value))
        .transpose()
        .map_err(|_| anyhow!("invalid_workflow_config: {field_name} is out of range"))?)
}

fn resolve_usize(value: Option<IntOrString>, default: usize, field_name: &str) -> Result<usize> {
    match resolve_optional_u64(value, field_name)? {
        Some(value) => usize::try_from(value)
            .map_err(|_| anyhow!("invalid_workflow_config: {field_name} is out of range")),
        None => Ok(default),
    }
}

fn resolve_u64(value: Option<IntOrString>, default: u64, field_name: &str) -> Result<u64> {
    Ok(resolve_optional_u64(value, field_name)?.unwrap_or(default))
}

fn resolve_optional_u64(value: Option<IntOrString>, field_name: &str) -> Result<Option<u64>> {
    match value {
        Some(IntOrString::Int(value)) => Ok(Some(value)),
        Some(IntOrString::String(value)) => {
            let trimmed = value.trim();
            let resolved = if let Some(env_name) = trimmed.strip_prefix('$') {
                env::var(env_name)
                    .with_context(|| format!("environment variable ${env_name} is not set"))?
            } else {
                trimmed.to_string()
            };

            resolved
                .parse::<u64>()
                .map(Some)
                .map_err(|_| anyhow!("invalid_workflow_config: {field_name} must be an integer"))
        }
        None => Ok(None),
    }
}

fn resolve_optional_string(raw: Option<String>) -> Option<String> {
    match raw {
        Some(value) if value.starts_with('$') && !value.contains('/') => {
            env::var(value.trim_start_matches('$'))
                .ok()
                .filter(|resolved| !resolved.trim().is_empty())
        }
        Some(value) if !value.trim().is_empty() => Some(value),
        _ => None,
    }
}

fn default_approval_policy() -> JsonValue {
    json!({
        "reject": {
            "sandbox_approval": true,
            "rules": true,
            "mcp_elicitations": true
        }
    })
}

#[cfg(test)]
mod tests {
    use std::env;
    use std::path::Path;

    use crate::model::WorkflowDefinition;

    use super::{normalize_issue_state, Settings};

    #[test]
    fn resolves_env_backed_github_api_key() {
        env::set_var("GITHUB_TOKEN", "token-123");
        let definition = WorkflowDefinition {
            config: serde_yaml::from_str(
                r#"
tracker:
  kind: github
  owner: openai
  project_v2_number: 7
"#,
            )
            .unwrap(),
            prompt_template: String::new(),
        };

        let settings = Settings::from_workflow(&definition).unwrap();
        assert_eq!(settings.tracker.api_key, "token-123");
    }

    #[test]
    fn resolves_env_backed_owner_and_repo() {
        env::set_var("GITHUB_TOKEN", "token-123");
        env::set_var("SYMPHONY_GITHUB_OWNER", "openai");
        env::set_var("SYMPHONY_GITHUB_REPO", "symphony");

        let definition = WorkflowDefinition {
            config: serde_yaml::from_str(
                r#"
tracker:
  kind: github
  owner: $SYMPHONY_GITHUB_OWNER
  repo: $SYMPHONY_GITHUB_REPO
  project_v2_number: 7
"#,
            )
            .unwrap(),
            prompt_template: String::new(),
        };

        let settings = Settings::from_workflow(&definition).unwrap();
        assert_eq!(settings.tracker.owner, "openai");
        assert_eq!(settings.tracker.repo.as_deref(), Some("symphony"));
    }

    #[test]
    fn resolves_env_backed_agent_assignee_login() {
        env::set_var("GITHUB_TOKEN", "token-123");
        env::set_var("SYMPHONY_AGENT_ASSIGNEE", "Codex-Bot");
        let definition = WorkflowDefinition {
            config: serde_yaml::from_str(
                r#"
tracker:
  kind: github
  owner: openai
  project_v2_number: 7
agent:
  assignee_login: $SYMPHONY_AGENT_ASSIGNEE
"#,
            )
            .unwrap(),
            prompt_template: String::new(),
        };

        let settings = Settings::from_workflow(&definition).unwrap();
        assert_eq!(settings.agent.assignee_login.as_deref(), Some("codex-bot"));
    }

    #[test]
    fn normalizes_states_for_lookup() {
        assert_eq!(normalize_issue_state(" In Progress "), "in progress");
    }

    #[test]
    fn default_turn_sandbox_policy_uses_workspace_root() {
        env::set_var("GITHUB_TOKEN", "token-123");
        let definition = WorkflowDefinition {
            config: serde_yaml::from_str(
                r#"
tracker:
  kind: github
  owner: openai
  project_v2_number: 7
"#,
            )
            .unwrap(),
            prompt_template: String::new(),
        };

        let settings = Settings::from_workflow(&definition).unwrap();
        let policy = settings.turn_sandbox_policy(Path::new("/tmp/workspace"));

        assert_eq!(policy["type"], "workspaceWrite");
        assert_eq!(
            policy["writableRoots"],
            serde_json::json!(["/tmp/workspace"])
        );
    }

    #[test]
    fn explicit_workspace_write_policy_injects_workspace_root_when_missing() {
        env::set_var("GITHUB_TOKEN", "token-123");
        let definition = WorkflowDefinition {
            config: serde_yaml::from_str(
                r#"
tracker:
  kind: github
  owner: openai
  project_v2_number: 7
codex:
  turn_sandbox_policy:
    type: workspaceWrite
    networkAccess: true
"#,
            )
            .unwrap(),
            prompt_template: String::new(),
        };

        let settings = Settings::from_workflow(&definition).unwrap();
        let policy = settings.turn_sandbox_policy(Path::new("/tmp/workspace"));

        assert_eq!(policy["type"], "workspaceWrite");
        assert_eq!(policy["networkAccess"], serde_json::json!(true));
        assert_eq!(
            policy["writableRoots"],
            serde_json::json!(["/tmp/workspace"])
        );
    }

    #[test]
    fn explicit_writable_roots_are_preserved() {
        env::set_var("GITHUB_TOKEN", "token-123");
        let definition = WorkflowDefinition {
            config: serde_yaml::from_str(
                r#"
tracker:
  kind: github
  owner: openai
  project_v2_number: 7
codex:
  turn_sandbox_policy:
    type: workspaceWrite
    writableRoots:
      - relative/path
    networkAccess: true
"#,
            )
            .unwrap(),
            prompt_template: String::new(),
        };

        let settings = Settings::from_workflow(&definition).unwrap();
        let policy = settings.turn_sandbox_policy(Path::new("/tmp/workspace"));

        assert_eq!(
            policy["writableRoots"],
            serde_json::json!(["relative/path"])
        );
        assert_eq!(policy["networkAccess"], serde_json::json!(true));
    }

    #[test]
    fn resolves_env_backed_project_number_and_dashboard_url() {
        env::set_var("GITHUB_TOKEN", "token-123");
        env::set_var("SYMPHONY_PROJECT_NUMBER", "19");
        env::set_var(
            "SYMPHONY_PROJECT_URL",
            "https://github.com/users/dbachko/projects/19",
        );
        let definition = WorkflowDefinition {
            config: serde_yaml::from_str(
                r#"
tracker:
  kind: github
  owner: dbachko
  project_v2_number: $SYMPHONY_PROJECT_NUMBER
  project_url: $SYMPHONY_PROJECT_URL
"#,
            )
            .unwrap(),
            prompt_template: String::new(),
        };

        let settings = Settings::from_workflow(&definition).unwrap();
        assert_eq!(settings.tracker.project_v2_number, Some(19));
        assert_eq!(
            settings.tracker.project_url.as_deref(),
            Some("https://github.com/users/dbachko/projects/19")
        );
        assert_eq!(
            settings.tracker_dashboard_url().as_deref(),
            Some("https://github.com/users/dbachko/projects/19")
        );
    }

    #[test]
    fn resolves_env_backed_webhook_secret() {
        env::set_var("GITHUB_TOKEN", "token-123");
        env::set_var("GITHUB_WEBHOOK_SECRET", "webhook-secret");

        let definition = WorkflowDefinition {
            config: serde_yaml::from_str(
                r#"
tracker:
  kind: github
  owner: dbachko
  project_v2_number: 7
webhooks:
  listen: 127.0.0.1:8787
"#,
            )
            .unwrap(),
            prompt_template: String::new(),
        };

        let settings = Settings::from_workflow(&definition).unwrap();
        assert_eq!(settings.webhooks.listen.as_deref(), Some("127.0.0.1:8787"));
        assert_eq!(settings.webhooks.secret.as_deref(), Some("webhook-secret"));
        assert_eq!(settings.webhooks.path, "/github/webhook");
    }
}
