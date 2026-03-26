use anyhow::{anyhow, Result};
use serde::Deserialize;

use crate::config::{
    resolve_optional_bool, resolve_optional_string, resolve_optional_u64, resolve_u64,
    BoolOrString, IntOrString, Settings,
};

const DEFAULT_GEMINI_COMMAND: &str = "gemini";
const DEFAULT_APPROVAL_MODE: &str = "yolo";
const DEFAULT_TURN_TIMEOUT_MS: u64 = 3_600_000;
const DEFAULT_STALL_TIMEOUT_MS: u64 = 300_000;

#[derive(Debug, Clone)]
pub struct GeminiConfig {
    pub command: String,
    pub model: Option<String>,
    pub approval_mode: String,
    pub sandbox: Option<bool>,
    pub turn_timeout_ms: u64,
    pub stall_timeout_ms: u64,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
struct RawGeminiConfig {
    command: Option<String>,
    model: Option<String>,
    approval_mode: Option<String>,
    approval_policy: Option<String>,
    sandbox: Option<BoolOrString>,
    turn_timeout_ms: Option<IntOrString>,
    stall_timeout_ms: Option<IntOrString>,
}

pub fn load(settings: &Settings) -> Result<GeminiConfig> {
    let raw_value = settings
        .providers
        .get(&settings.agent.provider)
        .ok_or_else(|| {
            anyhow!(
                "invalid_workflow_config: providers.{} is required",
                settings.agent.provider.as_str()
            )
        })?;
    let raw = serde_yaml::from_value::<RawGeminiConfig>(raw_value.clone())
        .map_err(|error| anyhow!("invalid_workflow_config: {error}"))?;

    let config = GeminiConfig {
        command: raw
            .command
            .unwrap_or_else(|| DEFAULT_GEMINI_COMMAND.to_string()),
        model: resolve_optional_string(raw.model),
        approval_mode: resolve_approval_mode(raw.approval_mode, raw.approval_policy)?,
        sandbox: resolve_optional_bool(raw.sandbox, "providers.gemini.sandbox")?,
        turn_timeout_ms: resolve_u64(
            raw.turn_timeout_ms,
            DEFAULT_TURN_TIMEOUT_MS,
            "providers.gemini.turn_timeout_ms",
        )?,
        stall_timeout_ms: resolve_optional_u64(
            raw.stall_timeout_ms,
            "providers.gemini.stall_timeout_ms",
        )?
        .unwrap_or(DEFAULT_STALL_TIMEOUT_MS),
    };

    if config.command.trim().is_empty() {
        return Err(anyhow!(
            "invalid_workflow_config: providers.gemini.command must not be empty"
        ));
    }

    Ok(config)
}

fn resolve_approval_mode(
    raw_approval_mode: Option<String>,
    raw_approval_policy: Option<String>,
) -> Result<String> {
    let explicit = resolve_optional_string(raw_approval_mode);
    let approval_policy = resolve_optional_string(raw_approval_policy);

    let selected = explicit
        .or_else(|| {
            approval_policy.as_deref().map(|value| {
                if value.eq_ignore_ascii_case("never") {
                    "yolo".to_string()
                } else {
                    DEFAULT_APPROVAL_MODE.to_string()
                }
            })
        })
        .unwrap_or_else(|| DEFAULT_APPROVAL_MODE.to_string());

    validate_approval_mode(&selected)?;
    Ok(selected)
}

fn validate_approval_mode(value: &str) -> Result<()> {
    match value {
        "default" | "auto_edit" | "yolo" | "plan" => Ok(()),
        _ => Err(anyhow!(
            "invalid_workflow_config: providers.gemini.approval_mode must be one of default, auto_edit, yolo, plan"
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::env;

    use crate::config::Settings;
    use crate::model::WorkflowDefinition;

    use super::load;

    fn settings(provider_extra: &str) -> Settings {
        env::set_var("GITHUB_TOKEN", "token-123");
        let provider_block = if provider_extra.trim().is_empty() {
            "    {}".to_string()
        } else {
            provider_extra.to_string()
        };
        let definition = WorkflowDefinition {
            config: serde_yaml::from_str(&format!(
                r#"
tracker:
  kind: github
  owner: openai
  project_v2_number: 7
agent:
  provider: gemini
providers:
  gemini:
{provider_block}
"#
            ))
            .unwrap(),
            prompt_template: String::new(),
        };
        Settings::from_workflow(&definition).unwrap()
    }

    #[test]
    fn resolves_env_backed_model() {
        env::set_var("KAIRASTRA_GEMINI_MODEL", "gemini-2.5-pro");
        let settings = settings("    model: $KAIRASTRA_GEMINI_MODEL");
        let config = load(&settings).unwrap();
        assert_eq!(config.model.as_deref(), Some("gemini-2.5-pro"));
    }

    #[test]
    fn maps_approval_policy_never_to_yolo() {
        let settings = settings("    approval_policy: never");
        let config = load(&settings).unwrap();
        assert_eq!(config.approval_mode, "yolo");
    }

    #[test]
    fn preserves_explicit_approval_mode() {
        let settings = settings("    approval_mode: auto_edit");
        let config = load(&settings).unwrap();
        assert_eq!(config.approval_mode, "auto_edit");
    }
}
