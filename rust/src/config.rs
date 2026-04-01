use std::collections::HashMap;
use std::env;
use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use serde_yaml::Value as YamlValue;

use crate::model::WorkflowDefinition;

const DEFAULT_POLL_INTERVAL_MS: u64 = 30_000;
const DEFAULT_WORKSPACE_ROOT: &str = "kairastra_workspaces";
const DEFAULT_HOOK_TIMEOUT_MS: u64 = 60_000;
const DEFAULT_MAX_CONCURRENT_AGENTS: usize = 10;
const DEFAULT_MAX_TURNS: usize = 20;
const DEFAULT_MAX_RETRY_BACKOFF_MS: u64 = 300_000;

#[derive(Debug, Clone)]
pub struct Settings {
    pub tracker: TrackerSettings,
    pub polling: PollingSettings,
    pub workspace: WorkspaceSettings,
    pub hooks: HookSettings,
    pub agent: AgentSettings,
    pub providers: ProviderSettings,
}

#[derive(Debug, Clone)]
pub struct TrackerSettings {
    pub kind: String,
    pub mode: GitHubMode,
    pub api_key: String,
    pub owner: String,
    pub repo: Option<String>,
    pub project_owner: Option<String>,
    pub project_v2_number: Option<u32>,
    pub project_url: Option<String>,
    pub active_states: Vec<String>,
    pub terminal_states: Vec<String>,
    pub claimable_states: Vec<String>,
    pub in_progress_state: Option<String>,
    pub human_review_state: Option<String>,
    pub done_state: Option<String>,
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
    pub provider: ProviderId,
    pub max_concurrent_agents: usize,
    pub max_turns: usize,
    pub max_retry_backoff_ms: u64,
    pub assignee_login: Option<String>,
    pub max_concurrent_agents_by_state: HashMap<String, usize>,
}

#[derive(Debug, Clone)]
pub struct ProviderSettings {
    raw: HashMap<String, YamlValue>,
}

impl ProviderSettings {
    pub fn get(&self, provider: &ProviderId) -> Option<&YamlValue> {
        self.raw.get(provider.as_str())
    }

    pub fn ids(&self) -> Vec<&str> {
        self.raw.keys().map(String::as_str).collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderId(String);

impl ProviderId {
    pub fn parse(raw: String) -> Result<Self> {
        let normalized = raw.trim().to_ascii_lowercase();
        if normalized.is_empty() {
            return Err(anyhow!(
                "invalid_workflow_config: agent.provider is required"
            ));
        }
        Ok(Self(normalized))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GitHubMode {
    #[default]
    ProjectsV2,
    IssuesOnly,
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
    workspace: RawWorkspace,
    hooks: RawHooks,
    agent: RawAgent,
    providers: HashMap<String, YamlValue>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
struct RawTracker {
    kind: Option<String>,
    mode: GitHubMode,
    api_key: Option<String>,
    owner: Option<String>,
    repo: Option<String>,
    project_owner: Option<String>,
    project_v2_number: Option<IntOrString>,
    project_url: Option<String>,
    active_states: Vec<String>,
    terminal_states: Vec<String>,
    claimable_states: Option<Vec<String>>,
    in_progress_state: NullableString,
    human_review_state: NullableString,
    done_state: NullableString,
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
    provider: Option<String>,
    max_concurrent_agents: Option<IntOrString>,
    max_turns: Option<IntOrString>,
    max_retry_backoff_ms: Option<IntOrString>,
    assignee_login: Option<String>,
    max_concurrent_agents_by_state: HashMap<String, IntOrString>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub(crate) enum IntOrString {
    Int(u64),
    String(String),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub(crate) enum BoolOrString {
    Bool(bool),
    String(String),
}

#[derive(Debug, Clone, Default)]
enum NullableString {
    #[default]
    Missing,
    String(String),
    Null,
}

impl<'de> Deserialize<'de> for NullableString {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = Option::<String>::deserialize(deserializer)?;
        Ok(match value {
            Some(value) => Self::String(value),
            None => Self::Null,
        })
    }
}

impl Settings {
    pub fn from_workflow(workflow: &WorkflowDefinition) -> Result<Self> {
        reject_removed_codex_block(&workflow.config)?;

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
        let project_owner = resolve_optional_string(raw.tracker.project_owner);
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
        let provider = resolve_provider_id(raw.agent.provider)?;

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
        let Some(selected_provider_config) = raw.providers.get(provider.as_str()) else {
            return Err(anyhow!(
                "invalid_workflow_config: providers.{} is required",
                provider.as_str()
            ));
        };
        if !matches!(selected_provider_config, YamlValue::Mapping(_)) {
            return Err(anyhow!(
                "invalid_workflow_config: providers.{} must be a mapping",
                provider.as_str()
            ));
        }

        Ok(Self {
            tracker: TrackerSettings {
                kind: tracker_kind,
                mode: raw.tracker.mode,
                api_key,
                owner,
                repo,
                project_owner,
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
                claimable_states: raw
                    .tracker
                    .claimable_states
                    .unwrap_or_else(|| vec!["Todo".to_string()]),
                in_progress_state: resolve_nullable_string_or_default(
                    raw.tracker.in_progress_state,
                    "In Progress",
                ),
                human_review_state: resolve_nullable_string_or_default(
                    raw.tracker.human_review_state,
                    "Human Review",
                ),
                done_state: resolve_nullable_string_or_default(raw.tracker.done_state, "Done"),
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
            workspace: WorkspaceSettings {
                root: workspace_root,
            },
            hooks,
            agent: AgentSettings {
                provider,
                max_concurrent_agents,
                max_turns,
                max_retry_backoff_ms,
                assignee_login: resolve_optional_string(raw.agent.assignee_login)
                    .map(|value| value.to_lowercase()),
                max_concurrent_agents_by_state: state_limits,
            },
            providers: ProviderSettings { raw: raw.providers },
        })
    }

    pub fn workflow_prompt(&self, workflow: &WorkflowDefinition) -> String {
        if workflow.prompt_template.trim().is_empty() {
            "You are working on a GitHub issue.\n\n{% if tracker.dashboard_url %}GitHub dashboard: {{ tracker.dashboard_url }}\n\n{% endif %}Identifier: {{ issue.identifier }}\nTitle: {{ issue.title }}\n\nRepository guidance:\n- Discover the repository layout before assuming directories or file paths.\n- Verify a file or directory exists before reading, editing, or passing it to shell commands.\n- Prefer repository discovery commands such as `rg --files .` or `test -e <path>` when a path is uncertain.\n- When using `rg`, `sed`, `cat`, `git diff`, or similar commands, only pass paths you have already confirmed exist.\n- Quote any confirmed path that contains shell metacharacters such as parentheses, spaces, brackets, `*`, or `?` before passing it to `bash`.\n- Prefer finding exact filenames first, then open those exact paths instead of mixing real and guessed directories in one command.\n- Before running a package script, inspect the relevant `package.json` and confirm the script exists for that package.\n- Do not treat skill names, labels, or issue text as filesystem paths.\n- If an expected path does not exist, inspect the workspace and adapt to the actual repo structure.\n\nBody:\n{% if issue.description %}\n{{ issue.description }}\n{% else %}\nNo description provided.\n{% endif %}\n".to_string()
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

    pub fn claimable_state(&self, state: &str) -> bool {
        let normalized = normalize_issue_state(state);
        self.tracker
            .claimable_states
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
}

pub fn normalize_issue_state(state: &str) -> String {
    state.trim().to_lowercase()
}

fn reject_removed_codex_block(config: &serde_yaml::Value) -> Result<()> {
    if config.get("codex").is_some() {
        return Err(anyhow!(
            "invalid_workflow_config: top-level `codex` is no longer supported; use agent.provider plus providers.codex"
        ));
    }

    Ok(())
}

fn resolve_provider_id(raw: Option<String>) -> Result<ProviderId> {
    ProviderId::parse(resolve_required_string(raw, "agent.provider")?)
}

pub(crate) fn resolve_required_string(raw: Option<String>, field_name: &str) -> Result<String> {
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

pub(crate) fn expand_path(raw: &str) -> Result<PathBuf> {
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

pub(crate) fn resolve_u32(value: Option<IntOrString>, field_name: &str) -> Result<Option<u32>> {
    resolve_optional_u64(value, field_name)?
        .map(u32::try_from)
        .transpose()
        .map_err(|_| anyhow!("invalid_workflow_config: {field_name} is out of range"))
}

pub(crate) fn resolve_usize(
    value: Option<IntOrString>,
    default: usize,
    field_name: &str,
) -> Result<usize> {
    match resolve_optional_u64(value, field_name)? {
        Some(value) => usize::try_from(value)
            .map_err(|_| anyhow!("invalid_workflow_config: {field_name} is out of range")),
        None => Ok(default),
    }
}

pub(crate) fn resolve_u64(
    value: Option<IntOrString>,
    default: u64,
    field_name: &str,
) -> Result<u64> {
    Ok(resolve_optional_u64(value, field_name)?.unwrap_or(default))
}

pub(crate) fn resolve_optional_u64(
    value: Option<IntOrString>,
    field_name: &str,
) -> Result<Option<u64>> {
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

pub(crate) fn resolve_optional_string(raw: Option<String>) -> Option<String> {
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

pub(crate) fn resolve_optional_bool(
    value: Option<BoolOrString>,
    field_name: &str,
) -> Result<Option<bool>> {
    match value {
        Some(BoolOrString::Bool(value)) => Ok(Some(value)),
        Some(BoolOrString::String(value)) => {
            let trimmed = value.trim();
            let resolved = if let Some(env_name) = trimmed.strip_prefix('$') {
                env::var(env_name)
                    .with_context(|| format!("environment variable ${env_name} is not set"))?
            } else {
                trimmed.to_string()
            };
            let normalized = resolved.trim().to_ascii_lowercase();
            match normalized.as_str() {
                "" => Ok(None),
                "true" | "1" | "yes" | "on" => Ok(Some(true)),
                "false" | "0" | "no" | "off" => Ok(Some(false)),
                _ => Err(anyhow!(
                    "invalid_workflow_config: {field_name} must be a boolean"
                )),
            }
        }
        None => Ok(None),
    }
}

fn resolve_nullable_string_or_default(raw: NullableString, default: &str) -> Option<String> {
    match raw {
        NullableString::String(value) => {
            resolve_optional_string(Some(value)).or_else(|| Some(default.to_string()))
        }
        NullableString::Null => None,
        NullableString::Missing => Some(default.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use std::env;

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
agent:
  provider: codex
providers:
  codex: {}
"#,
            )
            .unwrap(),
            prompt_template: String::new(),
        };

        let settings = Settings::from_workflow(&definition).unwrap();
        assert_eq!(settings.tracker.api_key, "token-123");
    }

    #[test]
    fn rejects_removed_codex_block() {
        env::set_var("GITHUB_TOKEN", "token-123");
        let definition = WorkflowDefinition {
            config: serde_yaml::from_str(
                r#"
tracker:
  kind: github
  owner: openai
  project_v2_number: 7
agent:
  provider: codex
providers:
  codex: {}
codex:
  command: codex app-server
"#,
            )
            .unwrap(),
            prompt_template: String::new(),
        };

        let error = Settings::from_workflow(&definition)
            .unwrap_err()
            .to_string();
        assert!(error.contains("top-level `codex` is no longer supported"));
    }

    #[test]
    fn accepts_unknown_provider_when_selected_block_exists() {
        env::set_var("GITHUB_TOKEN", "token-123");
        let definition = WorkflowDefinition {
            config: serde_yaml::from_str(
                r#"
tracker:
  kind: github
  owner: openai
  project_v2_number: 7
agent:
  provider: claude
providers:
  claude: {}
"#,
            )
            .unwrap(),
            prompt_template: String::new(),
        };

        let settings = Settings::from_workflow(&definition).unwrap();
        assert_eq!(settings.agent.provider.as_str(), "claude");
        assert!(settings.providers.get(&settings.agent.provider).is_some());
    }

    #[test]
    fn rejects_missing_selected_provider_block() {
        env::set_var("GITHUB_TOKEN", "token-123");
        let definition = WorkflowDefinition {
            config: serde_yaml::from_str(
                r#"
tracker:
  kind: github
  owner: openai
  project_v2_number: 7
agent:
  provider: claude
"#,
            )
            .unwrap(),
            prompt_template: String::new(),
        };

        let error = Settings::from_workflow(&definition)
            .unwrap_err()
            .to_string();
        assert!(error.contains("providers.claude is required"));
    }

    #[test]
    fn resolves_env_backed_owner_and_repo() {
        env::set_var("GITHUB_TOKEN", "token-123");
        env::set_var("KAIRASTRA_GITHUB_OWNER", "openai");
        env::set_var("KAIRASTRA_GITHUB_REPO", "kairastra");

        let definition = WorkflowDefinition {
            config: serde_yaml::from_str(
                r#"
tracker:
  kind: github
  owner: $KAIRASTRA_GITHUB_OWNER
  repo: $KAIRASTRA_GITHUB_REPO
  project_v2_number: 7
agent:
  provider: codex
providers:
  codex: {}
"#,
            )
            .unwrap(),
            prompt_template: String::new(),
        };

        let settings = Settings::from_workflow(&definition).unwrap();
        assert_eq!(settings.tracker.owner, "openai");
        assert_eq!(settings.tracker.repo.as_deref(), Some("kairastra"));
    }

    #[test]
    fn default_prompt_includes_repo_layout_guidance() {
        env::set_var("GITHUB_TOKEN", "token-123");
        let definition = WorkflowDefinition {
            config: serde_yaml::from_str(
                r#"
tracker:
  kind: github
  owner: openai
  project_v2_number: 7
agent:
  provider: codex
providers:
  codex: {}
"#,
            )
            .unwrap(),
            prompt_template: String::new(),
        };

        let settings = Settings::from_workflow(&definition).unwrap();
        let prompt = settings.workflow_prompt(&definition);

        assert!(prompt
            .contains("Discover the repository layout before assuming directories or file paths."));
        assert!(prompt.contains("rg --files ."));
        assert!(prompt.contains("only pass paths you have already confirmed exist"));
        assert!(prompt.contains("Quote any confirmed path that contains shell metacharacters"));
        assert!(
            prompt.contains("Before running a package script, inspect the relevant `package.json`")
        );
        assert!(
            prompt.contains("Do not treat skill names, labels, or issue text as filesystem paths.")
        );
    }

    #[test]
    fn resolves_env_backed_agent_assignee_login() {
        env::set_var("GITHUB_TOKEN", "token-123");
        env::set_var("KAIRASTRA_AGENT_ASSIGNEE", "Codex-Bot");
        let definition = WorkflowDefinition {
            config: serde_yaml::from_str(
                r#"
tracker:
  kind: github
  owner: openai
  project_v2_number: 7
agent:
  provider: codex
  assignee_login: $KAIRASTRA_AGENT_ASSIGNEE
providers:
  codex: {}
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
    fn resolves_env_backed_project_number_and_dashboard_url() {
        env::set_var("GITHUB_TOKEN", "token-123");
        env::set_var("KAIRASTRA_PROJECT_NUMBER", "19");
        env::set_var(
            "KAIRASTRA_PROJECT_URL",
            "https://github.com/users/example-owner/projects/19",
        );
        let definition = WorkflowDefinition {
            config: serde_yaml::from_str(
                r#"
tracker:
  kind: github
  owner: example-owner
  project_v2_number: $KAIRASTRA_PROJECT_NUMBER
  project_url: $KAIRASTRA_PROJECT_URL
agent:
  provider: codex
providers:
  codex: {}
"#,
            )
            .unwrap(),
            prompt_template: String::new(),
        };

        let settings = Settings::from_workflow(&definition).unwrap();
        assert_eq!(settings.tracker.project_v2_number, Some(19));
        assert_eq!(
            settings.tracker.project_url.as_deref(),
            Some("https://github.com/users/example-owner/projects/19")
        );
        assert_eq!(
            settings.tracker_dashboard_url().as_deref(),
            Some("https://github.com/users/example-owner/projects/19")
        );
    }

    #[test]
    fn tracker_status_defaults_preserve_existing_behavior() {
        env::set_var("GITHUB_TOKEN", "token-123");
        let definition = WorkflowDefinition {
            config: serde_yaml::from_str(
                r#"
tracker:
  kind: github
  owner: openai
  project_v2_number: 7
agent:
  provider: codex
providers:
  codex: {}
"#,
            )
            .unwrap(),
            prompt_template: String::new(),
        };

        let settings = Settings::from_workflow(&definition).unwrap();
        assert_eq!(settings.tracker.claimable_states, vec!["Todo".to_string()]);
        assert_eq!(
            settings.tracker.in_progress_state.as_deref(),
            Some("In Progress")
        );
        assert_eq!(
            settings.tracker.human_review_state.as_deref(),
            Some("Human Review")
        );
        assert_eq!(settings.tracker.done_state.as_deref(), Some("Done"));
    }

    #[test]
    fn tracker_status_mapping_accepts_custom_targets_and_nulls() {
        env::set_var("GITHUB_TOKEN", "token-123");
        let definition = WorkflowDefinition {
            config: serde_yaml::from_str(
                r#"
tracker:
  kind: github
  owner: openai
  project_v2_number: 7
  claimable_states: ["Ready"]
  in_progress_state: Doing
  human_review_state: ~
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

        let settings = Settings::from_workflow(&definition).unwrap();
        assert_eq!(settings.tracker.claimable_states, vec!["Ready".to_string()]);
        assert_eq!(settings.tracker.in_progress_state.as_deref(), Some("Doing"));
        assert_eq!(settings.tracker.human_review_state, None);
        assert_eq!(settings.tracker.done_state.as_deref(), Some("Complete"));
    }

    #[test]
    fn tracker_status_mapping_preserves_explicit_empty_claimable_states() {
        env::set_var("GITHUB_TOKEN", "token-123");
        let definition = WorkflowDefinition {
            config: serde_yaml::from_str(
                r#"
tracker:
  kind: github
  owner: openai
  project_v2_number: 7
  claimable_states: []
agent:
  provider: codex
providers:
  codex: {}
"#,
            )
            .unwrap(),
            prompt_template: String::new(),
        };

        let settings = Settings::from_workflow(&definition).unwrap();
        assert!(settings.tracker.claimable_states.is_empty());
    }
}
