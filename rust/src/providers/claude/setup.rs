use anyhow::Result;
use serde_json::{Map as JsonMap, Value as JsonValue};

use crate::auth::AuthMode;
use crate::deploy::DeployMode;

const DEFAULT_CLAUDE_MODEL: &str = "sonnet";
const DEFAULT_CLAUDE_REASONING_EFFORT: &str = "medium";
const DEFAULT_CLAUDE_PERMISSION_MODE: &str = "bypassPermissions";

#[derive(Debug, Clone)]
pub struct ClaudeSetupConfig {
    pub auth_mode: AuthMode,
    pub model: String,
    pub reasoning_effort: String,
    pub permission_mode: String,
}

pub fn collect(_non_interactive: bool) -> Result<ClaudeSetupConfig> {
    let auth_mode = AuthMode::from_env_var("CLAUDE_AUTH_MODE");
    let model = std::env::var("KAIRASTRA_CLAUDE_MODEL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_CLAUDE_MODEL.to_string());
    let reasoning_effort = std::env::var("KAIRASTRA_CLAUDE_REASONING_EFFORT")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_CLAUDE_REASONING_EFFORT.to_string());
    let permission_mode = std::env::var("KAIRASTRA_CLAUDE_PERMISSION_MODE")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_CLAUDE_PERMISSION_MODE.to_string());

    Ok(ClaudeSetupConfig {
        auth_mode,
        model,
        reasoning_effort,
        permission_mode,
    })
}

pub fn render_workflow_section(config: &ClaudeSetupConfig) -> String {
    let _ = config;
    r#"  claude:
    command: claude"#
        .to_string()
}

pub fn render_env_section(_mode: DeployMode, config: &ClaudeSetupConfig) -> String {
    [format!("CLAUDE_AUTH_MODE={}", config.auth_mode)].join("\n")
}

pub fn render_settings_json(config: &ClaudeSetupConfig) -> JsonValue {
    let mut object = JsonMap::new();
    object.insert("model".to_string(), JsonValue::String(config.model.clone()));
    object.insert(
        "reasoning_effort".to_string(),
        JsonValue::String(config.reasoning_effort.clone()),
    );
    object.insert(
        "permission_mode".to_string(),
        JsonValue::String(config.permission_mode.clone()),
    );
    JsonValue::Object(object)
}

#[cfg(test)]
mod tests {
    use serde_json::Value as JsonValue;

    use super::{render_env_section, render_settings_json, ClaudeSetupConfig};
    use crate::auth::AuthMode;
    use crate::deploy::DeployMode;

    fn config() -> ClaudeSetupConfig {
        ClaudeSetupConfig {
            auth_mode: AuthMode::Auto,
            model: "sonnet".to_string(),
            reasoning_effort: "medium".to_string(),
            permission_mode: "bypassPermissions".to_string(),
        }
    }

    #[test]
    fn native_env_only_keeps_auth_mode() {
        let rendered = render_env_section(DeployMode::Native, &config());
        assert_eq!(rendered, "CLAUDE_AUTH_MODE=auto");
    }

    #[test]
    fn settings_json_contains_claude_defaults() {
        let rendered = render_settings_json(&config());
        assert_eq!(
            rendered.get("model").and_then(JsonValue::as_str),
            Some("sonnet")
        );
        assert_eq!(
            rendered.get("reasoning_effort").and_then(JsonValue::as_str),
            Some("medium")
        );
        assert_eq!(
            rendered.get("permission_mode").and_then(JsonValue::as_str),
            Some("bypassPermissions")
        );
    }
}
