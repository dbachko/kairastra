use anyhow::Result;

use crate::auth::AuthMode;
use crate::deploy::DeployMode;

#[derive(Debug, Clone)]
pub struct CodexSetupConfig {
    pub auth_mode: AuthMode,
    pub model: String,
    pub reasoning_effort: String,
    pub fast: Option<bool>,
}

pub fn collect(_non_interactive: bool) -> Result<CodexSetupConfig> {
    let auth_mode = AuthMode::from_env_var("CODEX_AUTH_MODE");
    let model = std::env::var("KAIRASTRA_CODEX_MODEL").unwrap_or_default();
    let reasoning_effort = std::env::var("KAIRASTRA_CODEX_REASONING_EFFORT").unwrap_or_default();
    let fast = env_bool("KAIRASTRA_CODEX_FAST");

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
    let fast = config.fast.map(bool_to_env).unwrap_or_default();
    let mut lines = vec![
        format!("CODEX_AUTH_MODE={}", config.auth_mode),
        format!("KAIRASTRA_CODEX_MODEL={}", config.model),
        format!(
            "KAIRASTRA_CODEX_REASONING_EFFORT={}",
            config.reasoning_effort
        ),
        format!("KAIRASTRA_CODEX_FAST={fast}"),
    ];

    if mode == DeployMode::Docker {
        lines.insert(1, "CODEX_CLI_VERSION=0.114.0".to_string());
    }

    lines.join("\n")
}

fn bool_to_env(value: bool) -> &'static str {
    if value {
        "true"
    } else {
        "false"
    }
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
    use super::{bool_to_env, render_env_section, CodexSetupConfig};
    use crate::auth::AuthMode;
    use crate::deploy::DeployMode;

    fn config(fast: Option<bool>) -> CodexSetupConfig {
        CodexSetupConfig {
            auth_mode: AuthMode::Auto,
            model: "gpt-5.4".to_string(),
            reasoning_effort: "high".to_string(),
            fast,
        }
    }

    #[test]
    fn docker_env_leaves_fast_blank_when_not_set() {
        let rendered = render_env_section(DeployMode::Docker, &config(None));
        assert!(rendered.contains("KAIRASTRA_CODEX_FAST="));
        assert!(!rendered.contains("KAIRASTRA_CODEX_FAST=false"));
    }

    #[test]
    fn docker_env_renders_explicit_fast_override() {
        let rendered = render_env_section(DeployMode::Docker, &config(Some(true)));
        assert!(rendered.contains("KAIRASTRA_CODEX_FAST=true"));
        assert_eq!(bool_to_env(false), "false");
    }
}
