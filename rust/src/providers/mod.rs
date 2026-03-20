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
pub const AGENT_BOOTSTRAP_NOTE: &str =
    "Bootstrap created by Symphony runtime before the first agent turn.";

pub fn is_workpad_comment(body: &str) -> bool {
    body.contains(AGENT_WORKPAD_HEADER)
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
        "codex" => Some("Initialize provider auth in the container"),
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
