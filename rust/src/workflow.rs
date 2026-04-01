use std::fs;
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::time::SystemTime;

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use tracing::warn;

use crate::config::Settings;
use crate::model::WorkflowDefinition;

pub const REPO_WORKFLOW_FILENAME: &str = "WORKFLOW.md";
pub const OPERATOR_CONFIG_DIRNAME: &str = ".kairastra";
pub const OPERATOR_ENV_FILENAME: &str = "kairastra.env";

#[derive(Debug, Clone)]
pub struct WorkflowSnapshot {
    pub definition: WorkflowDefinition,
    pub settings: Settings,
}

#[derive(Debug)]
pub struct WorkflowStore {
    path: PathBuf,
    state: RwLock<Option<CachedWorkflow>>,
}

#[derive(Debug, Clone)]
struct CachedWorkflow {
    modified_at: SystemTime,
    snapshot: WorkflowSnapshot,
}

impl WorkflowStore {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            state: RwLock::new(None),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn current(&self) -> Result<WorkflowSnapshot> {
        let modified_at = workflow_modified_time(&self.path)?;

        if let Some(cached) = self.state.read().expect("workflow lock poisoned").clone() {
            if cached.modified_at == modified_at {
                return Ok(cached.snapshot);
            }
        }

        match load_definition(&self.path).and_then(|definition| {
            let settings = Settings::from_workflow(&definition)?;
            Ok(WorkflowSnapshot {
                definition,
                settings,
            })
        }) {
            Ok(snapshot) => {
                self.state
                    .write()
                    .expect("workflow lock poisoned")
                    .replace(CachedWorkflow {
                        modified_at,
                        snapshot: snapshot.clone(),
                    });
                Ok(snapshot)
            }
            Err(error) => {
                if let Some(cached) = self.state.read().expect("workflow lock poisoned").clone() {
                    warn!(
                        workflow_path = %self.path.display(),
                        error = ?error,
                        "failed to reload WORKFLOW.md; keeping last known good workflow"
                    );
                    Ok(cached.snapshot)
                } else {
                    Err(error)
                }
            }
        }
    }
}

pub fn default_workflow_path() -> Result<PathBuf> {
    let cwd = std::env::current_dir()?;
    let root = cwd.join(REPO_WORKFLOW_FILENAME);
    Ok(root)
}

pub fn default_env_file_path() -> Result<Option<PathBuf>> {
    let cwd = std::env::current_dir()?;
    let repo_local = cwd
        .join(OPERATOR_CONFIG_DIRNAME)
        .join(OPERATOR_ENV_FILENAME);
    if repo_local.is_file() {
        return Ok(Some(repo_local));
    }

    let legacy = cwd.join(OPERATOR_ENV_FILENAME);
    if legacy.is_file() {
        return Ok(Some(legacy));
    }

    Ok(None)
}

pub fn load_definition(path: &Path) -> Result<WorkflowDefinition> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("missing_workflow_file: could not read {}", path.display()))?;

    let (front_matter, prompt_lines) = split_front_matter(&content);
    let prompt_template = prompt_lines.join("\n").trim().to_string();

    let config = if front_matter.trim().is_empty() {
        serde_yaml::Value::Mapping(Default::default())
    } else {
        let value: serde_yaml::Value = serde_yaml::from_str(&front_matter)
            .map_err(|error| anyhow!("workflow_parse_error: {error}"))?;
        match value {
            serde_yaml::Value::Mapping(_) => value,
            _ => return Err(anyhow!("workflow_front_matter_not_a_map")),
        }
    };

    Ok(WorkflowDefinition {
        config,
        prompt_template,
    })
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepoWorkflowHooks {
    pub after_create: Option<String>,
    pub before_run: Option<String>,
    pub after_run: Option<String>,
    pub before_remove: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct RepoWorkflow {
    pub definition: WorkflowDefinition,
    pub hooks: RepoWorkflowHooks,
}

pub fn default_repo_workflow() -> RepoWorkflow {
    RepoWorkflow {
        definition: WorkflowDefinition {
            config: serde_yaml::Value::Mapping(Default::default()),
            prompt_template: String::new(),
        },
        hooks: RepoWorkflowHooks::default(),
    }
}

pub fn load_repo_workflow(path: &Path) -> Result<RepoWorkflow> {
    if !path.is_file() {
        return Ok(default_repo_workflow());
    }

    let definition = load_definition(path)?;
    let raw = serde_yaml::from_value::<RawRepoWorkflow>(definition.config.clone())
        .map_err(|error| anyhow!("invalid_repo_workflow_config: {error}"))?;

    Ok(RepoWorkflow {
        definition,
        hooks: RepoWorkflowHooks {
            after_create: raw.hooks.after_create,
            before_run: raw.hooks.before_run,
            after_run: raw.hooks.after_run,
            before_remove: raw.hooks.before_remove,
        },
    })
}

fn split_front_matter(content: &str) -> (String, Vec<String>) {
    let normalized = content.replace("\r\n", "\n");
    let lines: Vec<String> = normalized.split('\n').map(ToString::to_string).collect();

    if lines.first().map(String::as_str) != Some("---") {
        return ("".to_string(), lines);
    }

    let mut front = Vec::new();
    let mut prompt_start = None;

    for (index, line) in lines.iter().enumerate().skip(1) {
        if line == "---" {
            prompt_start = Some(index + 1);
            break;
        }
        front.push(line.clone());
    }

    match prompt_start {
        Some(start) => (front.join("\n"), lines[start..].to_vec()),
        None => (front.join("\n"), Vec::new()),
    }
}

fn workflow_modified_time(path: &Path) -> Result<SystemTime> {
    fs::metadata(path)
        .with_context(|| format!("missing_workflow_file: could not stat {}", path.display()))?
        .modified()
        .context("workflow metadata missing modified time")
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct RawRepoWorkflow {
    hooks: RawRepoWorkflowHooks,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct RawRepoWorkflowHooks {
    after_create: Option<String>,
    before_run: Option<String>,
    after_run: Option<String>,
    before_remove: Option<String>,
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::{Mutex, OnceLock};

    use tempfile::tempdir;

    use super::{
        default_repo_workflow, default_workflow_path, load_definition, load_repo_workflow,
        RepoWorkflowHooks, WorkflowStore,
    };

    fn cwd_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn supports_prompt_only_workflows() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("WORKFLOW.md");
        fs::write(&path, "Prompt only\n").unwrap();

        let workflow = load_definition(&path).unwrap();
        assert_eq!(workflow.prompt_template, "Prompt only");
        assert!(matches!(workflow.config, serde_yaml::Value::Mapping(_)));
    }

    #[test]
    fn supports_unterminated_front_matter() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("WORKFLOW.md");
        fs::write(&path, "---\ntracker:\n  kind: github\n").unwrap();

        let workflow = load_definition(&path).unwrap();
        assert_eq!(workflow.prompt_template, "");
        assert_eq!(
            workflow
                .config
                .get("tracker")
                .and_then(|value| value.get("kind"))
                .and_then(serde_yaml::Value::as_str),
            Some("github")
        );
    }

    #[test]
    fn rejects_non_map_front_matter() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("WORKFLOW.md");
        fs::write(&path, "---\n- nope\n---\nPrompt\n").unwrap();

        let error = load_definition(&path).unwrap_err().to_string();
        assert!(error.contains("workflow_front_matter_not_a_map"));
    }

    #[test]
    fn keeps_last_known_good_workflow_on_reload_failure() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("WORKFLOW.md");
        fs::write(
            &path,
            r#"---
tracker:
  kind: github
  owner: openai
  project_v2_number: 7
  api_key: fake
agent:
  provider: codex
providers:
  codex: {}
---
hello
"#,
        )
        .unwrap();

        let store = WorkflowStore::new(path.clone());
        let first = store.current().unwrap();
        assert_eq!(first.definition.prompt_template, "hello");

        std::thread::sleep(std::time::Duration::from_millis(5));
        fs::write(&path, "---\n- broken\n---\nnope\n").unwrap();

        let second = store.current().unwrap();
        assert_eq!(second.definition.prompt_template, "hello");
    }

    #[test]
    fn missing_repo_workflow_uses_default() {
        let dir = tempdir().unwrap();
        let workflow = load_repo_workflow(&dir.path().join("WORKFLOW.md")).unwrap();

        assert_eq!(workflow.definition.prompt_template, "");
        assert_eq!(workflow.hooks, default_repo_workflow().hooks);
    }

    #[test]
    fn repo_workflow_accepts_prompt_and_hooks_only() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("WORKFLOW.md");
        fs::write(
            &path,
            r#"---
hooks:
  after_create: echo ready
  before_run: echo run
  after_run: echo done
  before_remove: echo bye
---
Repo prompt
"#,
        )
        .unwrap();

        let workflow = load_repo_workflow(&path).unwrap();
        assert_eq!(workflow.definition.prompt_template, "Repo prompt");
        assert_eq!(
            workflow.hooks,
            RepoWorkflowHooks {
                after_create: Some("echo ready".to_string()),
                before_run: Some("echo run".to_string()),
                after_run: Some("echo done".to_string()),
                before_remove: Some("echo bye".to_string()),
            }
        );
    }

    #[test]
    fn repo_workflow_rejects_global_config_fields() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("WORKFLOW.md");
        fs::write(
            &path,
            r#"---
tracker:
  kind: github
---
Repo prompt
"#,
        )
        .unwrap();

        let error = load_repo_workflow(&path).unwrap_err().to_string();
        assert!(error.contains("invalid_repo_workflow_config"));
    }

    #[test]
    fn default_workflow_path_prefers_repo_root_workflow() {
        let _guard = cwd_lock().lock().unwrap();
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("WORKFLOW.md"), "root\n").unwrap();

        let original = std::env::current_dir().unwrap();
        std::env::set_current_dir(dir.path()).unwrap();
        let resolved = default_workflow_path().unwrap();
        std::env::set_current_dir(original).unwrap();

        assert_eq!(
            resolved,
            fs::canonicalize(dir.path()).unwrap().join("WORKFLOW.md")
        );
    }
}
