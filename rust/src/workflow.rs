use std::fs;
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::time::SystemTime;

use anyhow::{anyhow, Context, Result};
use tracing::warn;

use crate::config::Settings;
use crate::model::WorkflowDefinition;

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
    Ok(std::env::current_dir()?.join("WORKFLOW.md"))
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

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::{load_definition, WorkflowStore};

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
}
