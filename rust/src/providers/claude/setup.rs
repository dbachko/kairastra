use anyhow::Result;
use dialoguer::{theme::ColorfulTheme, Input, Select};

use crate::auth::AuthMode;
use crate::deploy::DeployMode;

#[derive(Debug, Clone)]
pub struct ClaudeSetupConfig {
    pub auth_mode: AuthMode,
    pub model: String,
    pub reasoning_effort: String,
}

pub fn collect(non_interactive: bool) -> Result<ClaudeSetupConfig> {
    let theme = ColorfulTheme::default();
    let auth_mode = ask_auth_mode(&theme, non_interactive)?;
    let model = ask_string(
        &theme,
        "Provider model (optional)",
        std::env::var("SYMPHONY_CLAUDE_MODEL").unwrap_or_default(),
        non_interactive,
        true,
    )?;
    let reasoning_effort = ask_string(
        &theme,
        "Thinking effort (optional: low|medium|high)",
        std::env::var("SYMPHONY_CLAUDE_REASONING_EFFORT").unwrap_or_default(),
        non_interactive,
        true,
    )?;

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
    model: $SYMPHONY_CLAUDE_MODEL
    reasoning_effort: $SYMPHONY_CLAUDE_REASONING_EFFORT
    approval_policy: never"#
        .to_string()
}

pub fn render_env_section(_mode: DeployMode, config: &ClaudeSetupConfig) -> String {
    [
        format!("CLAUDE_AUTH_MODE={}", config.auth_mode),
        format!("SYMPHONY_CLAUDE_MODEL={}", config.model),
        format!(
            "SYMPHONY_CLAUDE_REASONING_EFFORT={}",
            config.reasoning_effort
        ),
    ]
    .join("\n")
}

fn ask_auth_mode(theme: &ColorfulTheme, non_interactive: bool) -> Result<AuthMode> {
    if non_interactive {
        return Ok(AuthMode::from_env_var("CLAUDE_AUTH_MODE"));
    }

    let items = ["Claude account login", "Anthropic API key from env"];
    let default = match AuthMode::from_env_var("CLAUDE_AUTH_MODE") {
        AuthMode::ApiKey => 1,
        _ => 0,
    };
    let selection = Select::with_theme(theme)
        .with_prompt("Provider auth flow")
        .items(&items)
        .default(default)
        .interact()?;
    Ok(match selection {
        1 => AuthMode::ApiKey,
        _ => AuthMode::Subscription,
    })
}

fn ask_string(
    theme: &ColorfulTheme,
    prompt: &str,
    default: String,
    non_interactive: bool,
    allow_empty: bool,
) -> Result<String> {
    if non_interactive {
        return Ok(default);
    }

    let mut input = Input::<String>::with_theme(theme);
    input = input.with_prompt(prompt).default(default.clone());
    let value = input.interact_text()?;
    if allow_empty || !value.trim().is_empty() {
        Ok(value.trim().to_string())
    } else {
        Ok(default)
    }
}
