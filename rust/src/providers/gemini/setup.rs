use anyhow::Result;
use dialoguer::{theme::ColorfulTheme, Select};

use crate::auth::AuthMode;
use crate::deploy::DeployMode;

#[derive(Debug, Clone)]
pub struct GeminiSetupConfig {
    pub auth_mode: AuthMode,
    pub model: String,
    pub approval_mode: String,
}

pub fn collect(non_interactive: bool) -> Result<GeminiSetupConfig> {
    let theme = ColorfulTheme::default();
    let auth_mode = ask_auth_mode(&theme, non_interactive)?;
    let model = std::env::var("KAIRASTRA_GEMINI_MODEL").unwrap_or_default();
    let approval_mode =
        std::env::var("KAIRASTRA_GEMINI_APPROVAL_MODE").unwrap_or_else(|_| "yolo".to_string());

    Ok(GeminiSetupConfig {
        auth_mode,
        model,
        approval_mode,
    })
}

pub fn render_workflow_section(config: &GeminiSetupConfig) -> String {
    let _ = config;
    r#"  gemini:
    command: gemini
    model: $KAIRASTRA_GEMINI_MODEL
    approval_mode: $KAIRASTRA_GEMINI_APPROVAL_MODE"#
        .to_string()
}

pub fn render_env_section(_mode: DeployMode, config: &GeminiSetupConfig) -> String {
    [
        format!("GEMINI_AUTH_MODE={}", config.auth_mode),
        format!("KAIRASTRA_GEMINI_MODEL={}", config.model),
        format!("KAIRASTRA_GEMINI_APPROVAL_MODE={}", config.approval_mode),
    ]
    .join("\n")
}

fn print_auth_mode_help() {
    println!();
    println!("Gemini auth options");
    println!("- Google login uses the Gemini CLI/browser login flow.");
    println!(
        "- API key mode expects GEMINI_API_KEY to already be set before you run `kairastra doctor`."
    );
    println!();
}

fn ask_auth_mode(theme: &ColorfulTheme, non_interactive: bool) -> Result<AuthMode> {
    if non_interactive {
        return Ok(AuthMode::from_env_var("GEMINI_AUTH_MODE"));
    }

    print_auth_mode_help();

    let items = ["Google login", "Gemini API key from env"];
    let default = match AuthMode::from_env_var("GEMINI_AUTH_MODE") {
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
