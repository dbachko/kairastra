use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

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
        let required_writable_roots = sandbox_writable_roots(workspace);

        match self.turn_sandbox_policy.clone() {
            Some(mut policy) => {
                if let Some(object) = policy.as_object_mut() {
                    let is_workspace_write = object
                        .get("type")
                        .and_then(JsonValue::as_str)
                        .map(|value| value == "workspaceWrite")
                        .unwrap_or(false);
                    if is_workspace_write {
                        let writable_roots = merge_writable_roots(
                            object.get("writableRoots"),
                            &required_writable_roots,
                        );
                        object.insert("writableRoots".to_string(), json!(writable_roots));
                    }
                }

                policy
            }
            None => json!({
                "type": "workspaceWrite",
                "writableRoots": required_writable_roots
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

fn sandbox_writable_roots(workspace: &Path) -> Vec<String> {
    let mut roots = BTreeSet::new();
    roots.insert(workspace.to_string_lossy().to_string());

    for path in workspace_git_admin_roots(workspace) {
        roots.insert(path.to_string_lossy().to_string());
    }

    roots.into_iter().collect()
}

fn merge_writable_roots(existing: Option<&JsonValue>, required_roots: &[String]) -> Vec<String> {
    let mut roots = BTreeSet::new();

    if let Some(values) = existing.and_then(JsonValue::as_array) {
        for value in values {
            if let Some(path) = value.as_str() {
                roots.insert(path.to_string());
            }
        }
    }

    for path in required_roots {
        roots.insert(path.clone());
    }

    roots.into_iter().collect()
}

fn workspace_git_admin_roots(workspace: &Path) -> Vec<PathBuf> {
    let git_path = workspace.join(".git");
    if !git_path.is_file() {
        return Vec::new();
    }

    let Some(gitdir) = resolve_worktree_gitdir(&git_path) else {
        return Vec::new();
    };

    let mut roots = BTreeSet::new();
    roots.insert(gitdir.clone());

    if let Some(common_dir) = resolve_common_dir(&gitdir) {
        roots.insert(common_dir);
    }

    roots.into_iter().collect()
}

fn resolve_worktree_gitdir(git_file: &Path) -> Option<PathBuf> {
    let contents = fs::read_to_string(git_file).ok()?;
    let raw_path = contents.strip_prefix("gitdir:")?.trim();
    let gitdir = if Path::new(raw_path).is_absolute() {
        PathBuf::from(raw_path)
    } else {
        git_file.parent()?.join(raw_path)
    };
    gitdir.canonicalize().ok().or(Some(gitdir))
}

fn resolve_common_dir(gitdir: &Path) -> Option<PathBuf> {
    let commondir_file = gitdir.join("commondir");
    let contents = fs::read_to_string(commondir_file).ok()?;
    let raw_path = contents.trim();
    if raw_path.is_empty() {
        return None;
    }
    let common_dir = if Path::new(raw_path).is_absolute() {
        PathBuf::from(raw_path)
    } else {
        gitdir.join(raw_path)
    };
    common_dir.canonicalize().ok().or(Some(common_dir))
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
    use std::fs;
    use std::path::Path;
    use std::sync::Mutex;

    use tempfile::tempdir;

    use crate::config::Settings;
    use crate::model::WorkflowDefinition;

    use super::load;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

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
        let _guard = ENV_LOCK.lock().unwrap();
        env::set_var("KAIRASTRA_CODEX_MODEL", "gpt-5.4");
        let settings = settings("    model: $KAIRASTRA_CODEX_MODEL");
        let config = load(&settings).unwrap();
        assert_eq!(config.model.as_deref(), Some("gpt-5.4"));
        env::remove_var("KAIRASTRA_CODEX_MODEL");
    }

    #[test]
    fn resolves_env_backed_reasoning_effort() {
        let _guard = ENV_LOCK.lock().unwrap();
        env::set_var("KAIRASTRA_CODEX_REASONING_EFFORT", "high");
        let settings = settings("    reasoning_effort: $KAIRASTRA_CODEX_REASONING_EFFORT");
        let config = load(&settings).unwrap();
        assert_eq!(config.reasoning_effort.as_deref(), Some("high"));
        env::remove_var("KAIRASTRA_CODEX_REASONING_EFFORT");
    }

    #[test]
    fn resolves_env_backed_fast_flag() {
        let _guard = ENV_LOCK.lock().unwrap();
        env::set_var("KAIRASTRA_CODEX_FAST", "true");
        let settings = settings("    fast: $KAIRASTRA_CODEX_FAST");
        let config = load(&settings).unwrap();
        assert_eq!(config.fast, Some(true));
        env::remove_var("KAIRASTRA_CODEX_FAST");
    }

    #[test]
    fn blank_env_backed_fast_flag_is_omitted() {
        let _guard = ENV_LOCK.lock().unwrap();
        env::set_var("KAIRASTRA_CODEX_FAST", "");
        let settings = settings("    fast: $KAIRASTRA_CODEX_FAST");
        let config = load(&settings).unwrap();
        assert_eq!(config.fast, None);
        assert_eq!(config.service_tier(), None);
        env::remove_var("KAIRASTRA_CODEX_FAST");
    }

    #[test]
    fn explicit_false_fast_flag_maps_to_flex() {
        let settings = settings("    fast: false");
        let config = load(&settings).unwrap();
        assert_eq!(config.fast, Some(false));
        assert_eq!(config.service_tier(), Some("flex"));
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
            serde_json::json!(["/tmp/workspace", "relative/path"])
        );
        assert_eq!(policy["networkAccess"], serde_json::json!(true));
    }

    #[test]
    fn worktree_git_admin_dirs_are_added_to_writable_roots() {
        let dir = tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        let gitdir = dir.path().join("seed/.git/worktrees/issue-1");
        let common_dir = dir.path().join("seed/.git");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&gitdir).unwrap();
        fs::create_dir_all(&common_dir).unwrap();
        fs::write(
            workspace.join(".git"),
            format!("gitdir: {}\n", gitdir.display()),
        )
        .unwrap();
        fs::write(gitdir.join("commondir"), "../../\n").unwrap();

        let settings = settings("");
        let policy = load(&settings).unwrap().turn_sandbox_policy(&workspace);
        let expected_common_dir = common_dir.canonicalize().unwrap();
        let expected_gitdir = gitdir.canonicalize().unwrap();

        assert_eq!(policy["type"], "workspaceWrite");
        assert_eq!(
            policy["writableRoots"],
            serde_json::json!([
                expected_common_dir.display().to_string(),
                expected_gitdir.display().to_string(),
                workspace.display().to_string()
            ])
        );
    }
}
