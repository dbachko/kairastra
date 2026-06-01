use anyhow::Result;
use serde_json::{Map as JsonMap, Value as JsonValue};

use crate::auth::AuthMode;
use crate::deploy::DeployMode;

const DEFAULT_CODEX_MODEL: &str = "gpt-5.4";
const DEFAULT_CODEX_REASONING_EFFORT: &str = "medium";

#[derive(Debug, Clone)]
pub struct CodexSetupConfig {
    pub auth_mode: AuthMode,
    pub model: String,
    pub reasoning_effort: String,
    pub collaboration_mode: String,
    pub fast: Option<bool>,
}

pub fn collect(_non_interactive: bool) -> Result<CodexSetupConfig> {
    let auth_mode = AuthMode::from_env_var("CODEX_AUTH_MODE");
    let model = std::env::var("KAIRASTRA_CODEX_MODEL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_CODEX_MODEL.to_string());
    let reasoning_effort = std::env::var("KAIRASTRA_CODEX_REASONING_EFFORT")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_CODEX_REASONING_EFFORT.to_string());
    let collaboration_mode = std::env::var("KAIRASTRA_CODEX_COLLABORATION_MODE")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "default".to_string());
    let fast = env_bool("KAIRASTRA_CODEX_FAST");

    Ok(CodexSetupConfig {
        auth_mode,
        model,
        reasoning_effort,
        collaboration_mode,
        fast,
    })
}

pub fn render_workflow_section(config: &CodexSetupConfig) -> String {
    let _ = config;
    r#"  codex:
    command: codex app-server
    approval_policy: never
    thread_sandbox: workspace-write
    turn_sandbox_policy:
      type: workspaceWrite
      networkAccess: true"#
        .to_string()
}

pub fn render_env_section(mode: DeployMode, config: &CodexSetupConfig) -> String {
    let _ = mode;
    let lines = [format!("CODEX_AUTH_MODE={}", config.auth_mode)];

    lines.join("\n")
}

pub fn render_settings_json(config: &CodexSetupConfig) -> JsonValue {
    let mut object = JsonMap::new();
    object.insert("model".to_string(), JsonValue::String(config.model.clone()));
    object.insert(
        "reasoning_effort".to_string(),
        JsonValue::String(config.reasoning_effort.clone()),
    );
    object.insert(
        "collaboration_mode".to_string(),
        JsonValue::String(config.collaboration_mode.clone()),
    );
    if let Some(fast) = config.fast {
        object.insert("fast".to_string(), JsonValue::Bool(fast));
    }
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

    use super::{render_env_section, render_settings_json, CodexSetupConfig};
    use crate::auth::AuthMode;
    use crate::deploy::DeployMode;

    fn config(fast: Option<bool>) -> CodexSetupConfig {
        CodexSetupConfig {
            auth_mode: AuthMode::Auto,
            model: "gpt-5.4".to_string(),
            reasoning_effort: "medium".to_string(),
            collaboration_mode: "default".to_string(),
            fast,
        }
    }

    #[test]
    fn native_env_only_keeps_auth_mode() {
        let rendered = render_env_section(DeployMode::Native, &config(None));
        assert_eq!(rendered, "CODEX_AUTH_MODE=auto");
    }

    #[test]
    fn settings_json_contains_codex_defaults() {
        let rendered = render_settings_json(&config(Some(true)));
        assert_eq!(
            rendered.get("model").and_then(JsonValue::as_str),
            Some("gpt-5.4")
        );
        assert_eq!(
            rendered.get("reasoning_effort").and_then(JsonValue::as_str),
            Some("medium")
        );
        assert_eq!(
            rendered
                .get("collaboration_mode")
                .and_then(JsonValue::as_str),
            Some("default")
        );
        assert_eq!(
            rendered.get("fast").and_then(JsonValue::as_bool),
            Some(true)
        );
    }
}
