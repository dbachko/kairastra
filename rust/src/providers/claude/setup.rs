use anyhow::Result;

use crate::auth::AuthMode;
use crate::deploy::DeployMode;

#[derive(Debug, Clone)]
pub struct ClaudeSetupConfig {
    pub auth_mode: AuthMode,
    pub model: String,
    pub reasoning_effort: String,
}

pub fn collect(_non_interactive: bool) -> Result<ClaudeSetupConfig> {
    let auth_mode = AuthMode::from_env_var("CLAUDE_AUTH_MODE");
    let model = std::env::var("KAIRASTRA_CLAUDE_MODEL").unwrap_or_default();
    let reasoning_effort = std::env::var("KAIRASTRA_CLAUDE_REASONING_EFFORT").unwrap_or_default();

    Ok(ClaudeSetupConfig {
        auth_mode,
        model,
        reasoning_effort,
    })
}

pub fn render_workflow_section(config: &ClaudeSetupConfig) -> String {
    let _ = config;
    r#"  claude:
    command: claude
    model: $KAIRASTRA_CLAUDE_MODEL
    reasoning_effort: $KAIRASTRA_CLAUDE_REASONING_EFFORT
    approval_policy: never"#
        .to_string()
}

pub fn render_env_section(_mode: DeployMode, config: &ClaudeSetupConfig) -> String {
    [
        format!("CLAUDE_AUTH_MODE={}", config.auth_mode),
        format!("KAIRASTRA_CLAUDE_MODEL={}", config.model),
        format!(
            "KAIRASTRA_CLAUDE_REASONING_EFFORT={}",
            config.reasoning_effort
        ),
    ]
    .join("\n")
}
