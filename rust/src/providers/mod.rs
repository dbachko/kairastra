pub mod claude;
pub mod codex;

use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, Result};

use crate::agent::{AgentBackend, AgentSession};
use crate::auth::{AuthMode, AuthStatus};
use crate::config::Settings;
use crate::deploy::DeployMode;
use crate::github::GitHubTracker;

pub const AGENT_WORKPAD_HEADER: &str = "## Agent Workpad";
pub const CODEX_WORKPAD_HEADER: &str = "## Codex Workpad";
pub const CLAUDE_WORKPAD_HEADER: &str = "## Claude Workpad";
pub const AGENT_BOOTSTRAP_NOTE: &str =
    "Bootstrap created by Kairastra runtime before the first agent turn.";

pub fn workpad_header(provider: &str) -> &'static str {
    match provider {
        "codex" => CODEX_WORKPAD_HEADER,
        "claude" => CLAUDE_WORKPAD_HEADER,
        _ => AGENT_WORKPAD_HEADER,
    }
}

pub fn is_workpad_comment(body: &str) -> bool {
    let Some(first_non_empty_line) = body.lines().find(|line| !line.trim().is_empty()) else {
        return false;
    };

    matches!(
        first_non_empty_line.trim(),
        AGENT_WORKPAD_HEADER | CODEX_WORKPAD_HEADER | CLAUDE_WORKPAD_HEADER
    )
}

pub fn is_bootstrap_workpad(body: &str) -> bool {
    body.contains(AGENT_BOOTSTRAP_NOTE)
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
        other => Err(anyhow!("unsupported_agent_provider: {other}")),
    }
}

pub fn stall_timeout_ms(settings: &Settings) -> Result<u64> {
    match settings.agent.provider.as_str() {
        "claude" => Ok(claude::config::load(settings)?.stall_timeout_ms),
        "codex" => Ok(codex::config::load(settings)?.stall_timeout_ms),
        other => Err(anyhow!("unsupported_agent_provider: {other}")),
    }
}

pub fn command_name(provider: &str) -> Result<&'static str> {
    match provider {
        "claude" => Ok(claude::auth::COMMAND_NAME),
        "codex" => Ok(codex::auth::COMMAND_NAME),
        other => Err(anyhow!("unsupported_agent_provider: {other}")),
    }
}

pub fn inspect_auth_status(provider: &str) -> Result<AuthStatus> {
    match provider {
        "claude" => Ok(claude::auth::inspect_status()),
        "codex" => Ok(codex::auth::inspect_status()),
        other => Err(anyhow!("unsupported_agent_provider: {other}")),
    }
}

pub fn run_login(provider: &str, mode: AuthMode) -> Result<()> {
    match provider {
        "claude" => claude::auth::run_login(mode),
        "codex" => codex::auth::run_login(mode),
        other => Err(anyhow!("unsupported_agent_provider: {other}")),
    }
}

pub fn default_setup_provider() -> &'static str {
    "codex"
}

pub fn setup_provider_choices() -> &'static [(&'static str, &'static str)] {
    &[("codex", "Codex"), ("claude", "Claude Code")]
}

pub fn setup_provider_id(config: &ProviderSetupConfig) -> &'static str {
    match config {
        ProviderSetupConfig::Claude(_) => "claude",
        ProviderSetupConfig::Codex(_) => "codex",
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
        other => Err(anyhow!("unsupported_agent_provider: {other}")),
    }
}

pub fn docker_login_message(provider: &str) -> Option<&'static str> {
    match provider {
        "claude" => Some("Initialize Claude auth in the container"),
        "codex" => Some("Initialize Codex auth in the container"),
        _ => None,
    }
}

pub fn setup_auth_mode(config: &ProviderSetupConfig) -> AuthMode {
    match config {
        ProviderSetupConfig::Claude(config) => config.auth_mode,
        ProviderSetupConfig::Codex(config) => config.auth_mode,
    }
}

pub fn render_workflow_provider_section(config: &ProviderSetupConfig) -> String {
    match config {
        ProviderSetupConfig::Claude(config) => claude::setup::render_workflow_section(config),
        ProviderSetupConfig::Codex(config) => codex::setup::render_workflow_section(config),
    }
}

pub fn render_env_provider_section(mode: DeployMode, config: &ProviderSetupConfig) -> String {
    match config {
        ProviderSetupConfig::Claude(config) => claude::setup::render_env_section(mode, config),
        ProviderSetupConfig::Codex(config) => codex::setup::render_env_section(mode, config),
    }
}

pub fn repo_support_dirs(provider: &str) -> Result<&'static [&'static str]> {
    match provider {
        "claude" => Ok(&[".github"]),
        "codex" => Ok(&[".codex", ".github"]),
        other => Err(anyhow!("unsupported_agent_provider: {other}")),
    }
}

#[derive(Debug, Clone)]
pub enum ProviderSetupConfig {
    Claude(claude::setup::ClaudeSetupConfig),
    Codex(codex::setup::CodexSetupConfig),
}

#[cfg(test)]
mod tests {
    use super::{
        is_workpad_comment, setup_provider_id, workpad_header, ProviderSetupConfig,
        AGENT_WORKPAD_HEADER, CLAUDE_WORKPAD_HEADER, CODEX_WORKPAD_HEADER,
    };

    #[test]
    fn recognizes_supported_workpad_headers() {
        assert!(is_workpad_comment("## Agent Workpad\n\nbody"));
        assert!(is_workpad_comment("## Codex Workpad\n\nbody"));
        assert!(is_workpad_comment("## Claude Workpad\n\nbody"));
        assert!(!is_workpad_comment("## Design Workpad\n\nbody"));
    }

    #[test]
    fn resolves_provider_specific_workpad_headers() {
        assert_eq!(workpad_header("codex"), CODEX_WORKPAD_HEADER);
        assert_eq!(workpad_header("claude"), CLAUDE_WORKPAD_HEADER);
        assert_eq!(workpad_header("unknown"), AGENT_WORKPAD_HEADER);
    }

    #[test]
    fn setup_provider_id_matches_config_variant() {
        let codex = ProviderSetupConfig::Codex(crate::providers::codex::setup::CodexSetupConfig {
            auth_mode: crate::auth::AuthMode::Auto,
            model: String::new(),
            reasoning_effort: String::new(),
            fast: false,
        });
        let claude =
            ProviderSetupConfig::Claude(crate::providers::claude::setup::ClaudeSetupConfig {
                auth_mode: crate::auth::AuthMode::Auto,
                model: String::new(),
                reasoning_effort: String::new(),
            });

        assert_eq!(setup_provider_id(&codex), "codex");
        assert_eq!(setup_provider_id(&claude), "claude");
    }
}
