pub mod claude;
pub mod codex;
pub mod gemini;

use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, Result};

use crate::agent::{AgentBackend, AgentSession};
use crate::auth::{AuthMode, AuthStatus};
use crate::config::Settings;
use crate::deploy::DeployMode;
use crate::github::GitHubTracker;
use crate::model::Issue;

pub const AGENT_WORKPAD_HEADER: &str = "## Agent Workpad";
pub const CODEX_WORKPAD_HEADER: &str = "## Codex Workpad";
pub const CLAUDE_WORKPAD_HEADER: &str = "## Claude Workpad";
pub const GEMINI_WORKPAD_HEADER: &str = "## Gemini Workpad";
pub const AGENT_BOOTSTRAP_NOTE: &str =
    "Bootstrap created by Kairastra runtime before the first agent turn.";

pub fn workpad_header(provider: &str) -> &'static str {
    match provider {
        "codex" => CODEX_WORKPAD_HEADER,
        "claude" => CLAUDE_WORKPAD_HEADER,
        "gemini" => GEMINI_WORKPAD_HEADER,
        _ => AGENT_WORKPAD_HEADER,
    }
}

pub fn is_workpad_comment(body: &str) -> bool {
    let Some(first_non_empty_line) = body.lines().find(|line| !line.trim().is_empty()) else {
        return false;
    };

    matches!(
        first_non_empty_line.trim(),
        AGENT_WORKPAD_HEADER | CODEX_WORKPAD_HEADER | CLAUDE_WORKPAD_HEADER | GEMINI_WORKPAD_HEADER
    )
}

pub fn is_bootstrap_workpad(body: &str) -> bool {
    body.contains(AGENT_BOOTSTRAP_NOTE)
}

pub fn workpad_host_alias(hostname: &str) -> String {
    let first_label = hostname.trim().split('.').next().unwrap_or_default();
    let mut alias = String::with_capacity(first_label.len());
    let mut last_was_dash = false;

    for ch in first_label.chars().flat_map(|ch| ch.to_lowercase()) {
        let normalized = if ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-' {
            ch
        } else {
            '-'
        };
        if normalized == '-' {
            if !last_was_dash {
                alias.push('-');
            }
            last_was_dash = true;
        } else {
            alias.push(normalized);
            last_was_dash = false;
        }
    }

    let trimmed = alias.trim_matches('-');
    if trimmed.is_empty() {
        "unknown-host".to_string()
    } else {
        trimmed.to_string()
    }
}

pub fn workpad_environment_stamp(hostname: &str, issue: &Issue, sha: &str) -> String {
    let host = workpad_host_alias(hostname);
    let issue_ref = compact_issue_ref(issue);
    let short_sha = if sha.trim().is_empty() {
        "unknown"
    } else {
        sha.trim()
    };
    format!("{host}:{issue_ref}@{short_sha}")
}

fn compact_issue_ref(issue: &Issue) -> String {
    let identifier = issue.identifier.trim();
    if let Some((repo_path, issue_number)) = identifier.split_once('#') {
        let repo_name = repo_path
            .rsplit('/')
            .next()
            .map(sanitize_issue_component)
            .unwrap_or_default();
        let issue_number = sanitize_issue_component(issue_number);
        if !repo_name.is_empty() && !issue_number.is_empty() {
            return format!("{repo_name}#{issue_number}");
        }
    }

    let issue_id = sanitize_issue_component(issue.id.trim());
    if issue_id.is_empty() {
        "unknown-issue".to_string()
    } else {
        format!("issue-{issue_id}")
    }
}

fn sanitize_issue_component(value: &str) -> String {
    let mut sanitized = String::with_capacity(value.len());
    let mut last_was_dash = false;

    for ch in value.chars() {
        let normalized = if ch.is_ascii_alphanumeric() || ch == '.' || ch == '_' || ch == '-' {
            ch
        } else {
            '-'
        };
        if normalized == '-' {
            if !last_was_dash {
                sanitized.push('-');
            }
            last_was_dash = true;
        } else {
            sanitized.push(normalized);
            last_was_dash = false;
        }
    }

    sanitized.trim_matches('-').to_string()
}

pub async fn start_session(
    settings: &Settings,
    tracker: Arc<GitHubTracker>,
    workspace: &Path,
) -> Result<Box<dyn AgentSession>> {
    match settings.agent.provider.as_str() {
        "claude" => {
            claude::runtime::ClaudeBackend
                .start_session(settings, tracker, workspace)
                .await
        }
        "codex" => {
            codex::runtime::CodexBackend
                .start_session(settings, tracker, workspace)
                .await
        }
        "gemini" => {
            gemini::runtime::GeminiBackend
                .start_session(settings, tracker, workspace)
                .await
        }
        other => Err(anyhow!("unsupported_agent_provider: {other}")),
    }
}

pub fn stall_timeout_ms(settings: &Settings) -> Result<u64> {
    match settings.agent.provider.as_str() {
        "claude" => Ok(claude::config::load(settings)?.stall_timeout_ms),
        "codex" => Ok(codex::config::load(settings)?.stall_timeout_ms),
        "gemini" => Ok(gemini::config::load(settings)?.stall_timeout_ms),
        other => Err(anyhow!("unsupported_agent_provider: {other}")),
    }
}

pub fn command_name(provider: &str) -> Result<&'static str> {
    match provider {
        "claude" => Ok(claude::auth::COMMAND_NAME),
        "codex" => Ok(codex::auth::COMMAND_NAME),
        "gemini" => Ok(gemini::auth::COMMAND_NAME),
        other => Err(anyhow!("unsupported_agent_provider: {other}")),
    }
}

pub fn inspect_auth_status(provider: &str) -> Result<AuthStatus> {
    match provider {
        "claude" => Ok(claude::auth::inspect_status()),
        "codex" => Ok(codex::auth::inspect_status()),
        "gemini" => Ok(gemini::auth::inspect_status()),
        other => Err(anyhow!("unsupported_agent_provider: {other}")),
    }
}

pub fn run_login(provider: &str, mode: AuthMode) -> Result<()> {
    match provider {
        "claude" => claude::auth::run_login(mode),
        "codex" => codex::auth::run_login(mode),
        "gemini" => gemini::auth::run_login(mode),
        other => Err(anyhow!("unsupported_agent_provider: {other}")),
    }
}

pub fn default_setup_provider() -> &'static str {
    "codex"
}

pub fn setup_provider_choices() -> &'static [(&'static str, &'static str)] {
    &[
        ("codex", "Codex"),
        ("claude", "Claude Code"),
        ("gemini", "Gemini CLI"),
    ]
}

pub fn setup_provider_id(config: &ProviderSetupConfig) -> &'static str {
    match config {
        ProviderSetupConfig::Claude(_) => "claude",
        ProviderSetupConfig::Codex(_) => "codex",
        ProviderSetupConfig::Gemini(_) => "gemini",
    }
}

pub fn collect_setup_config(provider: &str, non_interactive: bool) -> Result<ProviderSetupConfig> {
    match provider {
        "claude" => Ok(ProviderSetupConfig::Claude(claude::setup::collect(
            non_interactive,
        )?)),
        "codex" => Ok(ProviderSetupConfig::Codex(codex::setup::collect(
            non_interactive,
        )?)),
        "gemini" => Ok(ProviderSetupConfig::Gemini(gemini::setup::collect(
            non_interactive,
        )?)),
        other => Err(anyhow!("unsupported_agent_provider: {other}")),
    }
}

pub fn setup_auth_mode(config: &ProviderSetupConfig) -> AuthMode {
    match config {
        ProviderSetupConfig::Claude(config) => config.auth_mode,
        ProviderSetupConfig::Codex(config) => config.auth_mode,
        ProviderSetupConfig::Gemini(config) => config.auth_mode,
    }
}

pub fn render_workflow_provider_section(config: &ProviderSetupConfig) -> String {
    match config {
        ProviderSetupConfig::Claude(config) => claude::setup::render_workflow_section(config),
        ProviderSetupConfig::Codex(config) => codex::setup::render_workflow_section(config),
        ProviderSetupConfig::Gemini(config) => gemini::setup::render_workflow_section(config),
    }
}

pub fn render_env_provider_section(mode: DeployMode, config: &ProviderSetupConfig) -> String {
    match config {
        ProviderSetupConfig::Claude(config) => claude::setup::render_env_section(mode, config),
        ProviderSetupConfig::Codex(config) => codex::setup::render_env_section(mode, config),
        ProviderSetupConfig::Gemini(config) => gemini::setup::render_env_section(mode, config),
    }
}

pub fn repo_support_dirs(provider: &str) -> Result<&'static [&'static str]> {
    match provider {
        "claude" => Ok(&[".agents", ".github"]),
        "codex" => Ok(&[".agents", ".github"]),
        "gemini" => Ok(&[".agents", ".github"]),
        other => Err(anyhow!("unsupported_agent_provider: {other}")),
    }
}

#[derive(Debug, Clone)]
pub enum ProviderSetupConfig {
    Claude(claude::setup::ClaudeSetupConfig),
    Codex(codex::setup::CodexSetupConfig),
    Gemini(gemini::setup::GeminiSetupConfig),
}

#[cfg(test)]
mod tests {
    use super::{
        is_workpad_comment, repo_support_dirs, setup_provider_id, workpad_environment_stamp,
        workpad_header, workpad_host_alias, ProviderSetupConfig, AGENT_WORKPAD_HEADER,
        CLAUDE_WORKPAD_HEADER, CODEX_WORKPAD_HEADER, GEMINI_WORKPAD_HEADER,
    };
    use crate::model::Issue;

    fn sample_issue() -> Issue {
        Issue {
            id: "17".to_string(),
            project_item_id: None,
            identifier: "example-owner/example-repo#17".to_string(),
            title: "Issue".to_string(),
            description: None,
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

    #[test]
    fn recognizes_supported_workpad_headers() {
        assert!(is_workpad_comment("## Agent Workpad\n\nbody"));
        assert!(is_workpad_comment("## Codex Workpad\n\nbody"));
        assert!(is_workpad_comment("## Claude Workpad\n\nbody"));
        assert!(is_workpad_comment("## Gemini Workpad\n\nbody"));
        assert!(!is_workpad_comment("## Design Workpad\n\nbody"));
    }

    #[test]
    fn resolves_provider_specific_workpad_headers() {
        assert_eq!(workpad_header("codex"), CODEX_WORKPAD_HEADER);
        assert_eq!(workpad_header("claude"), CLAUDE_WORKPAD_HEADER);
        assert_eq!(workpad_header("gemini"), GEMINI_WORKPAD_HEADER);
        assert_eq!(workpad_header("unknown"), AGENT_WORKPAD_HEADER);
    }

    #[test]
    fn setup_provider_id_matches_config_variant() {
        let codex = ProviderSetupConfig::Codex(crate::providers::codex::setup::CodexSetupConfig {
            auth_mode: crate::auth::AuthMode::Auto,
            model: String::new(),
            reasoning_effort: String::new(),
            fast: None,
        });
        let claude =
            ProviderSetupConfig::Claude(crate::providers::claude::setup::ClaudeSetupConfig {
                auth_mode: crate::auth::AuthMode::Auto,
                model: String::new(),
                reasoning_effort: String::new(),
            });
        let gemini =
            ProviderSetupConfig::Gemini(crate::providers::gemini::setup::GeminiSetupConfig {
                auth_mode: crate::auth::AuthMode::Auto,
                model: String::new(),
                approval_mode: "yolo".to_string(),
            });

        assert_eq!(setup_provider_id(&codex), "codex");
        assert_eq!(setup_provider_id(&claude), "claude");
        assert_eq!(setup_provider_id(&gemini), "gemini");
    }

    #[test]
    fn codex_repo_support_dirs_only_require_repo_owned_files() {
        assert_eq!(repo_support_dirs("codex").unwrap(), &[".agents", ".github"]);
    }

    #[test]
    fn workpad_host_alias_uses_first_hostname_label() {
        assert_eq!(workpad_host_alias("MacBookPro.attlocal.net"), "macbookpro");
        assert_eq!(workpad_host_alias("DEV-BOX_01.local"), "dev-box-01");
        assert_eq!(workpad_host_alias("___"), "unknown-host");
    }

    #[test]
    fn workpad_environment_stamp_uses_repo_and_issue_identifier() {
        let issue = sample_issue();
        assert_eq!(
            workpad_environment_stamp("MacBookPro.attlocal.net", &issue, "ace31c7"),
            "macbookpro:example-repo#17@ace31c7"
        );
    }

    #[test]
    fn workpad_environment_stamp_falls_back_to_issue_id_for_malformed_identifier() {
        let mut issue = sample_issue();
        issue.identifier = "malformed".to_string();
        assert_eq!(
            workpad_environment_stamp("host.local", &issue, ""),
            "host:issue-17@unknown"
        );
    }
}
