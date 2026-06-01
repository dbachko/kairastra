use anyhow::Result;
use serde_json::{Map as JsonMap, Value as JsonValue};

use crate::auth::AuthMode;
use crate::deploy::DeployMode;

const DEFAULT_GEMINI_MODEL: &str = "gemini-2.5-pro";
const DEFAULT_GEMINI_APPROVAL_MODE: &str = "yolo";
const DEFAULT_GEMINI_SANDBOX: bool = true;

#[derive(Debug, Clone)]
pub struct GeminiSetupConfig {
    pub auth_mode: AuthMode,
    pub model: String,
    pub approval_mode: String,
    pub sandbox: bool,
}

pub fn collect(_non_interactive: bool) -> Result<GeminiSetupConfig> {
    let auth_mode = AuthMode::from_env_var("GEMINI_AUTH_MODE");
    let model = std::env::var("KAIRASTRA_GEMINI_MODEL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_GEMINI_MODEL.to_string());
    let approval_mode = std::env::var("KAIRASTRA_GEMINI_APPROVAL_MODE")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_GEMINI_APPROVAL_MODE.to_string());
    let sandbox = env_bool("KAIRASTRA_GEMINI_SANDBOX").unwrap_or(DEFAULT_GEMINI_SANDBOX);

    Ok(GeminiSetupConfig {
        auth_mode,
        model,
        approval_mode,
        sandbox,
    })
}

pub fn render_workflow_section(config: &GeminiSetupConfig) -> String {
    let _ = config;
    r#"  gemini:
    command: gemini"#
        .to_string()
}

pub fn render_env_section(_mode: DeployMode, config: &GeminiSetupConfig) -> String {
    [format!("GEMINI_AUTH_MODE={}", config.auth_mode)].join("\n")
}

pub fn render_settings_json(config: &GeminiSetupConfig) -> JsonValue {
    let mut object = JsonMap::new();
    object.insert("model".to_string(), JsonValue::String(config.model.clone()));
    object.insert(
        "approval_mode".to_string(),
        JsonValue::String(config.approval_mode.clone()),
    );
    object.insert("sandbox".to_string(), JsonValue::Bool(config.sandbox));
    JsonValue::Object(object)
}

fn env_bool(name: &str) -> Option<bool> {
    let value = std::env::var(name).ok()?;
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Some(true),
        "false" | "0" | "no" | "off" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Value as JsonValue;

    use super::{render_env_section, render_settings_json, GeminiSetupConfig};
    use crate::auth::AuthMode;
    use crate::deploy::DeployMode;

    fn config() -> GeminiSetupConfig {
        GeminiSetupConfig {
            auth_mode: AuthMode::Auto,
            model: "gemini-2.5-pro".to_string(),
            approval_mode: "yolo".to_string(),
            sandbox: true,
        }
    }

    #[test]
    fn native_env_only_keeps_auth_mode() {
        let rendered = render_env_section(DeployMode::Native, &config());
        assert_eq!(rendered, "GEMINI_AUTH_MODE=auto");
    }

    #[test]
    fn settings_json_contains_gemini_defaults() {
        let rendered = render_settings_json(&config());
        assert_eq!(
            rendered.get("model").and_then(JsonValue::as_str),
            Some("gemini-2.5-pro")
        );
        assert_eq!(
            rendered.get("approval_mode").and_then(JsonValue::as_str),
            Some("yolo")
        );
        assert_eq!(
            rendered.get("sandbox").and_then(JsonValue::as_bool),
            Some(true)
        );
    }
}
