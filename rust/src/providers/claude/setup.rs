use anyhow::Result;
use dialoguer::{theme::ColorfulTheme, Select};

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

fn print_auth_mode_help() {
    println!();
    println!("Claude auth options");
    println!("- Claude account login uses the Claude CLI/browser login flow.");
    println!(
        "- API key mode expects ANTHROPIC_API_KEY to already be set before you run `kairastra doctor`."
    );
    println!();
}

fn ask_auth_mode(theme: &ColorfulTheme, non_interactive: bool) -> Result<AuthMode> {
    if non_interactive {
        return Ok(AuthMode::from_env_var("CLAUDE_AUTH_MODE"));
    }

    print_auth_mode_help();

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
