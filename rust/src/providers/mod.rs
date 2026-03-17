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

pub async fn start_session(
    settings: &Settings,
    tracker: Arc<GitHubTracker>,
    workspace: &Path,
) -> Result<Box<dyn AgentSession>> {
    match settings.agent.provider.as_str() {
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
        "codex" => Ok(codex::config::load(settings)?.stall_timeout_ms),
        other => Err(anyhow!("unsupported_agent_provider: {other}")),
    }
}

pub fn command_name(provider: &str) -> Result<&'static str> {
    match provider {
        "codex" => Ok(codex::auth::COMMAND_NAME),
        other => Err(anyhow!("unsupported_agent_provider: {other}")),
    }
}

pub fn inspect_auth_status(provider: &str) -> Result<AuthStatus> {
    match provider {
        "codex" => Ok(codex::auth::inspect_status()),
        other => Err(anyhow!("unsupported_agent_provider: {other}")),
    }
}

pub fn run_login(provider: &str, mode: AuthMode) -> Result<()> {
    match provider {
        "codex" => codex::auth::run_login(mode),
        other => Err(anyhow!("unsupported_agent_provider: {other}")),
    }
}

pub fn default_setup_provider() -> &'static str {
    "codex"
}

pub fn collect_setup_config(provider: &str, non_interactive: bool) -> Result<ProviderSetupConfig> {
    match provider {
        "codex" => Ok(ProviderSetupConfig::Codex(codex::setup::collect(
            non_interactive,
        )?)),
        other => Err(anyhow!("unsupported_agent_provider: {other}")),
    }
}

pub fn docker_login_message(provider: &str) -> Option<&'static str> {
    match provider {
        "codex" => Some("Initialize provider auth in the container"),
        _ => None,
    }
}

pub fn setup_auth_mode(config: &ProviderSetupConfig) -> AuthMode {
    match config {
        ProviderSetupConfig::Codex(config) => config.auth_mode,
    }
}

pub fn render_workflow_provider_section(config: &ProviderSetupConfig) -> String {
    match config {
        ProviderSetupConfig::Codex(config) => codex::setup::render_workflow_section(config),
    }
}

pub fn render_env_provider_section(mode: DeployMode, config: &ProviderSetupConfig) -> String {
    match config {
        ProviderSetupConfig::Codex(config) => codex::setup::render_env_section(mode, config),
    }
}

#[derive(Debug, Clone)]
pub enum ProviderSetupConfig {
    Codex(codex::setup::CodexSetupConfig),
}
