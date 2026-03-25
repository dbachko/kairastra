use std::path::Path;

use anyhow::{anyhow, Result};
use serde::Deserialize;
use serde_json::{json, Value as JsonValue};

use crate::config::{
    resolve_optional_bool, resolve_optional_string, resolve_optional_u64, resolve_u64,
    BoolOrString, IntOrString, Settings,
};

const DEFAULT_CODEX_COMMAND: &str = "codex app-server";
const DEFAULT_READ_TIMEOUT_MS: u64 = 5_000;
const DEFAULT_TURN_TIMEOUT_MS: u64 = 3_600_000;
const DEFAULT_STALL_TIMEOUT_MS: u64 = 300_000;

#[derive(Debug, Clone)]
pub struct CodexConfig {
    pub command: String,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
    pub fast: Option<bool>,
    pub approval_policy: JsonValue,
    pub thread_sandbox: String,
    pub turn_sandbox_policy: Option<JsonValue>,
    pub read_timeout_ms: u64,
    pub turn_timeout_ms: u64,
    pub stall_timeout_ms: u64,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
struct RawCodexConfig {
    command: Option<String>,
    model: Option<String>,
    reasoning_effort: Option<String>,
    fast: Option<BoolOrString>,
    approval_policy: Option<JsonValue>,
    thread_sandbox: Option<String>,
    turn_sandbox_policy: Option<JsonValue>,
    read_timeout_ms: Option<IntOrString>,
    turn_timeout_ms: Option<IntOrString>,
    stall_timeout_ms: Option<IntOrString>,
}

pub fn load(settings: &Settings) -> Result<CodexConfig> {
    let raw_value = settings
        .providers
        .get(&settings.agent.provider)
        .ok_or_else(|| {
            anyhow!(
                "invalid_workflow_config: providers.{} is required",
                settings.agent.provider.as_str()
            )
        })?;
    let raw = serde_yaml::from_value::<RawCodexConfig>(raw_value.clone())
        .map_err(|error| anyhow!("invalid_workflow_config: {error}"))?;

    let reasoning_effort = resolve_optional_string(raw.reasoning_effort);
    if let Some(value) = reasoning_effort.as_deref() {
        validate_reasoning_effort(value)?;
    }

    let config = CodexConfig {
        command: raw
            .command
            .unwrap_or_else(|| DEFAULT_CODEX_COMMAND.to_string()),
        model: resolve_optional_string(raw.model),
        reasoning_effort,
        fast: resolve_optional_bool(raw.fast, "providers.codex.fast")?,
        approval_policy: raw.approval_policy.unwrap_or_else(default_approval_policy),
        thread_sandbox: raw
            .thread_sandbox
            .unwrap_or_else(|| "workspace-write".to_string()),
        turn_sandbox_policy: raw.turn_sandbox_policy,
        read_timeout_ms: resolve_u64(
            raw.read_timeout_ms,
            DEFAULT_READ_TIMEOUT_MS,
            "providers.codex.read_timeout_ms",
        )?,
        turn_timeout_ms: resolve_u64(
            raw.turn_timeout_ms,
            DEFAULT_TURN_TIMEOUT_MS,
            "providers.codex.turn_timeout_ms",
        )?,
        stall_timeout_ms: resolve_optional_u64(
            raw.stall_timeout_ms,
            "providers.codex.stall_timeout_ms",
        )?
        .unwrap_or(DEFAULT_STALL_TIMEOUT_MS),
    };

    if config.command.trim().is_empty() {
        return Err(anyhow!(
            "invalid_workflow_config: providers.codex.command must not be empty"
        ));
    }

    Ok(config)
}

impl CodexConfig {
    pub fn turn_sandbox_policy(&self, workspace: &Path) -> JsonValue {
        let workspace_root = workspace.to_string_lossy().to_string();

        match self.turn_sandbox_policy.clone() {
            Some(mut policy) => {
                if let Some(object) = policy.as_object_mut() {
                    let is_workspace_write = object
                        .get("type")
                        .and_then(JsonValue::as_str)
                        .map(|value| value == "workspaceWrite")
                        .unwrap_or(false);
                    let missing_writable_roots = object
                        .get("writableRoots")
                        .map(|value| value.is_null())
                        .unwrap_or(true);

                    if is_workspace_write && missing_writable_roots {
                        object.insert("writableRoots".to_string(), json!([workspace_root]));
                    }
                }

                policy
            }
            None => json!({
                "type": "workspaceWrite",
                "writableRoots": [workspace_root]
            }),
        }
    }

    pub fn service_tier(&self) -> Option<&'static str> {
        match self.fast {
            Some(true) => Some("fast"),
            Some(false) => Some("flex"),
            None => None,
        }
    }
}

fn validate_reasoning_effort(value: &str) -> Result<()> {
    match value {
        "none" | "minimal" | "low" | "medium" | "high" | "xhigh" => Ok(()),
        _ => Err(anyhow!(
            "invalid_workflow_config: providers.codex.reasoning_effort must be one of none, minimal, low, medium, high, xhigh"
        )),
    }
}

fn default_approval_policy() -> JsonValue {
    json!({
        "reject": {
            "sandbox_approval": true,
            "rules": true,
            "mcp_elicitations": true
        }
    })
}

#[cfg(test)]
mod tests {
    use std::env;
    use std::path::Path;

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
  provider: codex
providers:
  codex:
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
        env::set_var("KAIRASTRA_CODEX_MODEL", "gpt-5.4");
        let settings = settings("    model: $KAIRASTRA_CODEX_MODEL");
        let config = load(&settings).unwrap();
        assert_eq!(config.model.as_deref(), Some("gpt-5.4"));
    }

    #[test]
    fn resolves_env_backed_reasoning_effort() {
        env::set_var("KAIRASTRA_CODEX_REASONING_EFFORT", "high");
        let settings = settings("    reasoning_effort: $KAIRASTRA_CODEX_REASONING_EFFORT");
        let config = load(&settings).unwrap();
        assert_eq!(config.reasoning_effort.as_deref(), Some("high"));
    }

    #[test]
    fn resolves_env_backed_fast_flag() {
        env::set_var("KAIRASTRA_CODEX_FAST", "true");
        let settings = settings("    fast: $KAIRASTRA_CODEX_FAST");
        let config = load(&settings).unwrap();
        assert_eq!(config.fast, Some(true));
    }

    #[test]
    fn default_turn_sandbox_policy_uses_workspace_root() {
        let settings = settings("");
        let policy = load(&settings)
            .unwrap()
            .turn_sandbox_policy(Path::new("/tmp/workspace"));

        assert_eq!(policy["type"], "workspaceWrite");
        assert_eq!(
            policy["writableRoots"],
            serde_json::json!(["/tmp/workspace"])
        );
    }

    #[test]
    fn explicit_workspace_write_policy_injects_workspace_root_when_missing() {
        let settings = settings(
            r#"    turn_sandbox_policy:
      type: workspaceWrite
      networkAccess: true"#,
        );
        let policy = load(&settings)
            .unwrap()
            .turn_sandbox_policy(Path::new("/tmp/workspace"));

        assert_eq!(policy["type"], "workspaceWrite");
        assert_eq!(policy["networkAccess"], serde_json::json!(true));
        assert_eq!(
            policy["writableRoots"],
            serde_json::json!(["/tmp/workspace"])
        );
    }

    #[test]
    fn explicit_writable_roots_are_preserved() {
        let settings = settings(
            r#"    turn_sandbox_policy:
      type: workspaceWrite
      writableRoots:
        - relative/path
      networkAccess: true"#,
        );
        let policy = load(&settings)
            .unwrap()
            .turn_sandbox_policy(Path::new("/tmp/workspace"));

        assert_eq!(
            policy["writableRoots"],
            serde_json::json!(["relative/path"])
        );
        assert_eq!(policy["networkAccess"], serde_json::json!(true));
    }
}
