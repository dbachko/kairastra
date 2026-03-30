use anyhow::Result;
use dialoguer::{theme::ColorfulTheme, Select};

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
    let model = std::env::var("KAIRASTRA_CODEX_MODEL").unwrap_or_default();
    let reasoning_effort = std::env::var("KAIRASTRA_CODEX_REASONING_EFFORT").unwrap_or_default();
    let fast = env_bool("KAIRASTRA_CODEX_FAST").unwrap_or(false);

    Ok(CodexSetupConfig {
        auth_mode,
        model,
        reasoning_effort,
        fast,
    })
}

pub fn render_workflow_section(config: &CodexSetupConfig) -> String {
    let _ = config;
    r#"  codex:
    command: codex app-server
    model: $KAIRASTRA_CODEX_MODEL
    reasoning_effort: $KAIRASTRA_CODEX_REASONING_EFFORT
    fast: $KAIRASTRA_CODEX_FAST
    approval_policy: never
    thread_sandbox: workspace-write
    turn_sandbox_policy:
      type: workspaceWrite
      networkAccess: true"#
        .to_string()
}

pub fn render_env_section(mode: DeployMode, config: &CodexSetupConfig) -> String {
    let mut lines = vec![
        format!("CODEX_AUTH_MODE={}", config.auth_mode),
        format!("KAIRASTRA_CODEX_MODEL={}", config.model),
        format!(
            "KAIRASTRA_CODEX_REASONING_EFFORT={}",
            config.reasoning_effort
        ),
        format!("KAIRASTRA_CODEX_FAST={}", config.fast),
    ];

    if mode == DeployMode::Docker {
        lines.insert(1, "CODEX_CLI_VERSION=0.114.0".to_string());
    }

    lines.join("\n")
}

fn print_auth_mode_help() {
    println!();
    println!("Codex auth options");
    println!("- OpenAI subscription / device login uses the Codex CLI login flow.");
    println!(
        "- API key mode expects OPENAI_API_KEY to already be set before you run `kairastra doctor`."
    );
    println!();
}

fn ask_auth_mode(theme: &ColorfulTheme, non_interactive: bool) -> Result<AuthMode> {
    if non_interactive {
        return Ok(AuthMode::from_env_var("CODEX_AUTH_MODE"));
    }

    print_auth_mode_help();

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
fn env_bool(name: &str) -> Option<bool> {
    let value = std::env::var(name).ok()?;
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Some(true),
        "false" | "0" | "no" | "off" => Some(false),
        _ => None,
    }
}
