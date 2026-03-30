use anyhow::Result;

use crate::auth::AuthMode;
use crate::deploy::DeployMode;

#[derive(Debug, Clone)]
pub struct GeminiSetupConfig {
    pub auth_mode: AuthMode,
    pub model: String,
    pub approval_mode: String,
}

pub fn collect(_non_interactive: bool) -> Result<GeminiSetupConfig> {
    let auth_mode = AuthMode::from_env_var("GEMINI_AUTH_MODE");
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
