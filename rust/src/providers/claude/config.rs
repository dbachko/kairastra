use anyhow::{anyhow, Result};
use serde::Deserialize;

use crate::config::{
    resolve_optional_string, resolve_optional_u64, resolve_u64, IntOrString, Settings,
};

const DEFAULT_CLAUDE_COMMAND: &str = "claude";
const DEFAULT_PERMISSION_MODE: &str = "default";
const DEFAULT_TURN_TIMEOUT_MS: u64 = 3_600_000;
const DEFAULT_STALL_TIMEOUT_MS: u64 = 300_000;

#[derive(Debug, Clone)]
pub struct ClaudeConfig {
    pub command: String,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
    pub permission_mode: String,
    pub turn_timeout_ms: u64,
    pub stall_timeout_ms: u64,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
struct RawClaudeConfig {
    command: Option<String>,
    model: Option<String>,
    reasoning_effort: Option<String>,
    permission_mode: Option<String>,
    approval_policy: Option<String>,
    turn_timeout_ms: Option<IntOrString>,
    stall_timeout_ms: Option<IntOrString>,
}

pub fn load(settings: &Settings) -> Result<ClaudeConfig> {
    let raw_value = settings
        .providers
        .get(&settings.agent.provider)
        .ok_or_else(|| {
            anyhow!(
                "invalid_workflow_config: providers.{} is required",
                settings.agent.provider.as_str()
            )
        })?;
    let raw = serde_yaml::from_value::<RawClaudeConfig>(raw_value.clone())
        .map_err(|error| anyhow!("invalid_workflow_config: {error}"))?;

    let reasoning_effort = resolve_optional_string(raw.reasoning_effort);
    if let Some(value) = reasoning_effort.as_deref() {
        validate_reasoning_effort(value)?;
    }

    let permission_mode = resolve_permission_mode(raw.permission_mode, raw.approval_policy)?;

    let config = ClaudeConfig {
        command: raw
            .command
            .unwrap_or_else(|| DEFAULT_CLAUDE_COMMAND.to_string()),
        model: resolve_optional_string(raw.model),
        reasoning_effort,
        permission_mode,
        turn_timeout_ms: resolve_u64(
            raw.turn_timeout_ms,
            DEFAULT_TURN_TIMEOUT_MS,
            "providers.claude.turn_timeout_ms",
        )?,
        stall_timeout_ms: resolve_optional_u64(
            raw.stall_timeout_ms,
            "providers.claude.stall_timeout_ms",
        )?
        .unwrap_or(DEFAULT_STALL_TIMEOUT_MS),
    };

    if config.command.trim().is_empty() {
        return Err(anyhow!(
            "invalid_workflow_config: providers.claude.command must not be empty"
        ));
    }

    Ok(config)
}

fn resolve_permission_mode(
    raw_permission_mode: Option<String>,
    raw_approval_policy: Option<String>,
) -> Result<String> {
    let explicit = resolve_optional_string(raw_permission_mode);
    let approval_policy = resolve_optional_string(raw_approval_policy);

    let selected = explicit
        .or_else(|| {
            approval_policy.as_deref().map(|value| {
                if value.eq_ignore_ascii_case("never") {
                    "bypassPermissions".to_string()
                } else {
                    DEFAULT_PERMISSION_MODE.to_string()
                }
            })
        })
        .unwrap_or_else(|| DEFAULT_PERMISSION_MODE.to_string());

    validate_permission_mode(&selected)?;
    Ok(selected)
}

fn validate_reasoning_effort(value: &str) -> Result<()> {
    match value {
        "low" | "medium" | "high" => Ok(()),
        _ => Err(anyhow!(
            "invalid_workflow_config: providers.claude.reasoning_effort must be one of low, medium, high"
        )),
    }
}

fn validate_permission_mode(value: &str) -> Result<()> {
    match value {
        "acceptEdits" | "bypassPermissions" | "default" | "dontAsk" | "plan" => Ok(()),
        _ => Err(anyhow!(
            "invalid_workflow_config: providers.claude.permission_mode must be one of acceptEdits, bypassPermissions, default, dontAsk, plan"
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
  provider: claude
providers:
  claude:
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
        env::set_var("KAIRASTRA_CLAUDE_MODEL", "sonnet");
        let settings = settings("    model: $KAIRASTRA_CLAUDE_MODEL");
        let config = load(&settings).unwrap();
        assert_eq!(config.model.as_deref(), Some("sonnet"));
    }

    #[test]
    fn maps_approval_policy_never_to_bypass_permissions() {
        let settings = settings("    approval_policy: never");
        let config = load(&settings).unwrap();
        assert_eq!(config.permission_mode, "bypassPermissions");
    }

    #[test]
    fn preserves_explicit_permission_mode() {
        let settings = settings("    permission_mode: acceptEdits");
        let config = load(&settings).unwrap();
        assert_eq!(config.permission_mode, "acceptEdits");
    }
}
