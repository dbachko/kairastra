use anyhow::Result;
use dialoguer::{theme::ColorfulTheme, Confirm, Input, Select};

use crate::auth::AuthMode;
use crate::deploy::DeployMode;

#[derive(Debug, Clone)]
pub struct CodexSetupConfig {
    pub auth_mode: AuthMode,
    pub model: String,
    pub reasoning_effort: String,
    pub fast: bool,
}

pub fn collect(non_interactive: bool) -> Result<CodexSetupConfig> {
    let theme = ColorfulTheme::default();
    let auth_mode = ask_auth_mode(&theme, non_interactive)?;
    let model = ask_string(
        &theme,
        "Provider model (optional)",
        std::env::var("SYMPHONY_CODEX_MODEL").unwrap_or_default(),
        non_interactive,
        true,
    )?;
    let reasoning_effort = ask_string(
        &theme,
        "Thinking effort (optional: none|minimal|low|medium|high|xhigh)",
        std::env::var("SYMPHONY_CODEX_REASONING_EFFORT").unwrap_or_default(),
        non_interactive,
        true,
    )?;
    let fast = ask_bool(
        &theme,
        "Enable provider fast mode",
        env_bool("SYMPHONY_CODEX_FAST").unwrap_or(false),
        non_interactive,
    )?;

    Ok(CodexSetupConfig {
        auth_mode,
        model,
        reasoning_effort,
        fast,
    })
}

pub fn render_workflow_section(config: &CodexSetupConfig) -> String {
    let _ = config;
    format!(
        r#"providers:
  codex:
    command: codex app-server
    model: $SYMPHONY_CODEX_MODEL
    reasoning_effort: $SYMPHONY_CODEX_REASONING_EFFORT
    fast: $SYMPHONY_CODEX_FAST
    approval_policy: never
    thread_sandbox: workspace-write
    turn_sandbox_policy:
      type: workspaceWrite
      networkAccess: true"#
    )
}

pub fn render_env_section(mode: DeployMode, config: &CodexSetupConfig) -> String {
    let mut lines = vec![
        format!("CODEX_AUTH_MODE={}", config.auth_mode),
        format!("SYMPHONY_CODEX_MODEL={}", config.model),
        format!(
            "SYMPHONY_CODEX_REASONING_EFFORT={}",
            config.reasoning_effort
        ),
        format!("SYMPHONY_CODEX_FAST={}", config.fast),
    ];

    if mode == DeployMode::Docker {
        lines.insert(1, "CODEX_CLI_VERSION=0.114.0".to_string());
    }

    lines.join("\n")
}

fn ask_auth_mode(theme: &ColorfulTheme, non_interactive: bool) -> Result<AuthMode> {
    if non_interactive {
        return Ok(AuthMode::from_env_var("CODEX_AUTH_MODE"));
    }

    let items = [
        "OpenAI subscription / device login",
        "OpenAI API key bootstrap",
    ];
    let default = match AuthMode::from_env_var("CODEX_AUTH_MODE") {
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

fn ask_bool(
    theme: &ColorfulTheme,
    prompt: &str,
    default: bool,
    non_interactive: bool,
) -> Result<bool> {
    if non_interactive {
        return Ok(default);
    }

    Confirm::with_theme(theme)
        .with_prompt(prompt)
        .default(default)
        .interact()
        .map_err(Into::into)
}

fn env_bool(name: &str) -> Option<bool> {
    let value = std::env::var(name).ok()?;
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Some(true),
        "false" | "0" | "no" | "off" => Some(false),
        _ => None,
    }
}
