use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use clap::ValueEnum;
use serde::Serialize;

use crate::auth::find_command;
use crate::config::{normalize_issue_state, FieldSourceType, Settings};
use crate::deploy::DeployMode;
use crate::envfile::{apply_env, load_env_file};
use crate::github::GitHubTracker;
use crate::providers;
use crate::workflow::{default_workflow_path, load_definition};

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum DoctorFormat {
    Text,
    Json,
}

#[derive(Debug, Clone)]
pub struct DoctorOptions {
    pub workflow: Option<PathBuf>,
    pub env_file: Option<PathBuf>,
    pub mode: Option<DeployMode>,
    pub format: DoctorFormat,
}

#[derive(Debug, Clone, Serialize)]
pub struct DoctorReport {
    pub mode: DeployMode,
    pub workflow_path: Option<PathBuf>,
    pub checks: Vec<DoctorCheck>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DoctorCheck {
    pub name: &'static str,
    pub status: DoctorStatus,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DoctorStatus {
    Pass,
    Warn,
    Fail,
}

impl DoctorReport {
    pub fn has_failures(&self) -> bool {
        self.checks
            .iter()
            .any(|check| check.status == DoctorStatus::Fail)
    }
}

pub async fn run(options: DoctorOptions) -> Result<DoctorReport> {
    let env_values = if let Some(path) = options.env_file.as_ref() {
        let env_values = load_env_file(path)?;
        apply_env(&env_values);
        Some(env_values)
    } else {
        None
    };

    let workflow_path = resolve_workflow_path(options.workflow.as_ref());
    let mut checks = Vec::new();
    let mode = options.mode.unwrap_or_else(infer_mode);

    if mode == DeployMode::Docker {
        if let Some(values) = env_values.as_ref() {
            let removed_keys = ["WORKFLOW_FILE", "SEED_REPO_PATH"]
                .into_iter()
                .filter(|key| values.contains_key(*key))
                .collect::<Vec<_>>();
            if !removed_keys.is_empty() {
                return Err(anyhow!(
                    "removed_docker_env_keys: {} are no longer supported in Docker mode; re-run setup or import config into Docker volumes",
                    removed_keys.join(", ")
                ));
            }
        }
    }

    checks.push(check_command("gh"));
    checks.push(match mode {
        DeployMode::Docker => check_command("docker"),
        DeployMode::Native => check_command("systemctl"),
    });

    let mut tracker_check = DoctorCheck {
        name: "github_tracker",
        status: DoctorStatus::Warn,
        detail: "workflow not loaded".to_string(),
    };
    let mut project_status_check = DoctorCheck {
        name: "project_status_mapping",
        status: DoctorStatus::Warn,
        detail: "workflow not loaded".to_string(),
    };

    let mut workspace_check = DoctorCheck {
        name: "workspace_root",
        status: DoctorStatus::Warn,
        detail: "workflow not loaded".to_string(),
    };
    let mut provider_command_check = DoctorCheck {
        name: "agent_provider_command",
        status: DoctorStatus::Warn,
        detail: "workflow not loaded".to_string(),
    };
    let mut provider_auth_check = DoctorCheck {
        name: "agent_provider_auth",
        status: DoctorStatus::Warn,
        detail: "workflow not loaded".to_string(),
    };

    if let Some(path) = workflow_path.as_ref() {
        if path.is_file() {
            match load_definition(path).and_then(|definition| Settings::from_workflow(&definition))
            {
                Ok(settings) => {
                    checks.push(DoctorCheck {
                        name: "workflow_config",
                        status: DoctorStatus::Pass,
                        detail: format!("loaded {}", path.display()),
                    });
                    provider_command_check = check_named_command(
                        "agent_provider_command",
                        provider_command_name(&settings)?,
                    );
                    provider_auth_check = check_auth_status(&settings);
                    tracker_check = check_github_tracker(&settings).await;
                    project_status_check = check_project_status_mapping(&settings).await;
                    workspace_check = check_workspace_root(&settings.workspace.root);
                }
                Err(error) => {
                    checks.push(DoctorCheck {
                        name: "workflow_config",
                        status: DoctorStatus::Fail,
                        detail: format!("{}: {}", path.display(), error),
                    });
                }
            }
        } else {
            checks.push(DoctorCheck {
                name: "workflow_config",
                status: DoctorStatus::Warn,
                detail: format!("workflow file not found at {}", path.display()),
            });
        }
    } else {
        checks.push(DoctorCheck {
            name: "workflow_config",
            status: DoctorStatus::Warn,
            detail: "no workflow path provided and no default WORKFLOW.md found".to_string(),
        });
    }

    checks.push(provider_command_check);
    checks.push(provider_auth_check);
    checks.push(tracker_check);
    checks.push(project_status_check);
    checks.push(workspace_check);

    let report = DoctorReport {
        mode,
        workflow_path,
        checks,
    };

    match options.format {
        DoctorFormat::Text => {
            print_text_report(&report);
        }
        DoctorFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
    }

    if report.has_failures() {
        return Err(anyhow!("doctor_failed"));
    }

    Ok(report)
}

fn resolve_workflow_path(explicit: Option<&PathBuf>) -> Option<PathBuf> {
    if let Some(path) = explicit {
        return Some(path.clone());
    }

    match default_workflow_path() {
        Ok(path) if path.is_file() => Some(path),
        _ => None,
    }
}

fn infer_mode() -> DeployMode {
    match std::env::var("KAIRASTRA_DEPLOY_MODE")
        .unwrap_or_default()
        .trim()
        .to_lowercase()
        .as_str()
    {
        "docker" => return DeployMode::Docker,
        "native" => return DeployMode::Native,
        _ => {}
    }

    if Path::new("rust/compose.yml").is_file() || Path::new("compose.yml").is_file() {
        return DeployMode::Docker;
    }
    DeployMode::Native
}

fn check_command(name: &'static str) -> DoctorCheck {
    check_named_command(name, name)
}

fn check_named_command(check_name: &'static str, command_name: &'static str) -> DoctorCheck {
    match find_command(command_name) {
        Some(path) => DoctorCheck {
            name: check_name,
            status: DoctorStatus::Pass,
            detail: format!("command={} found at {}", command_name, path.display()),
        },
        None => DoctorCheck {
            name: check_name,
            status: DoctorStatus::Fail,
            detail: format!("command={} not found in PATH", command_name),
        },
    }
}

fn provider_command_name(settings: &Settings) -> Result<&'static str> {
    providers::command_name(settings.agent.provider.as_str())
}

fn check_auth_status(settings: &Settings) -> DoctorCheck {
    let auth_status = match providers::inspect_auth_status(settings.agent.provider.as_str()) {
        Ok(status) => status,
        Err(error) => {
            return DoctorCheck {
                name: "agent_provider_auth",
                status: DoctorStatus::Fail,
                detail: error.to_string(),
            };
        }
    };
    DoctorCheck {
        name: "agent_provider_auth",
        status: if auth_status.credentials_present {
            DoctorStatus::Pass
        } else {
            DoctorStatus::Warn
        },
        detail: format!(
            "provider={} configured={} inferred={} auth_file={} api_key_present={} credentials_present={} local_auth_path={} docker_hint={}",
            auth_status.provider,
            auth_status.configured_mode,
            auth_status.inferred_mode,
            auth_status.auth_file_present,
            auth_status.api_key_present,
            auth_status.credentials_present,
            auth_status.auth_file_path.display(),
            auth_status.docker_volume_hint
        ),
    }
}

async fn check_github_tracker(settings: &Settings) -> DoctorCheck {
    let tracker = match GitHubTracker::new(settings.tracker.clone()) {
        Ok(tracker) => tracker,
        Err(error) => {
            return DoctorCheck {
                name: "github_tracker",
                status: DoctorStatus::Fail,
                detail: error.to_string(),
            }
        }
    };

    let response = tracker
        .graphql_raw("query Viewer { viewer { login } }", serde_json::json!({}))
        .await;

    match response {
        Ok(body) => {
            let login = body
                .get("data")
                .and_then(|value| value.get("viewer"))
                .and_then(|value| value.get("login"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("<unknown>");
            DoctorCheck {
                name: "github_tracker",
                status: DoctorStatus::Pass,
                detail: format!("authenticated as {}", login),
            }
        }
        Err(error) => DoctorCheck {
            name: "github_tracker",
            status: DoctorStatus::Fail,
            detail: error.to_string(),
        },
    }
}

async fn check_project_status_mapping(settings: &Settings) -> DoctorCheck {
    if settings.tracker.mode != crate::config::GitHubMode::ProjectsV2 {
        return DoctorCheck {
            name: "project_status_mapping",
            status: DoctorStatus::Pass,
            detail: "not applicable for issues_only".to_string(),
        };
    }

    if !matches!(
        settings
            .tracker
            .status_source
            .as_ref()
            .map(|source| source.source_type),
        Some(FieldSourceType::ProjectField)
    ) {
        return DoctorCheck {
            name: "project_status_mapping",
            status: DoctorStatus::Pass,
            detail: "not applicable for non-project status sources".to_string(),
        };
    }

    let tracker = match GitHubTracker::new(settings.tracker.clone()) {
        Ok(tracker) => tracker,
        Err(error) => {
            return DoctorCheck {
                name: "project_status_mapping",
                status: DoctorStatus::Fail,
                detail: error.to_string(),
            }
        }
    };

    let overview = match tracker.inspect_project_status_overview().await {
        Ok(overview) => overview,
        Err(error) => {
            return DoctorCheck {
                name: "project_status_mapping",
                status: DoctorStatus::Fail,
                detail: error.to_string(),
            }
        }
    };

    let allowed_issue_states = ["closed"];
    let option_names = overview
        .options
        .iter()
        .map(|value| normalize_issue_state(value))
        .collect::<Vec<_>>();
    let mut missing = Vec::new();

    for state in &settings.tracker.active_states {
        let normalized = normalize_issue_state(state);
        if !option_names
            .iter()
            .any(|candidate| candidate == &normalized)
        {
            missing.push(format!("active_states:{state}"));
        }
    }

    for state in &settings.tracker.terminal_states {
        let normalized = normalize_issue_state(state);
        if allowed_issue_states.contains(&normalized.as_str()) {
            continue;
        }
        if !option_names
            .iter()
            .any(|candidate| candidate == &normalized)
        {
            missing.push(format!("terminal_states:{state}"));
        }
    }

    for state in &settings.tracker.claimable_states {
        let normalized = normalize_issue_state(state);
        if !option_names
            .iter()
            .any(|candidate| candidate == &normalized)
        {
            missing.push(format!("claimable_states:{state}"));
        }
    }

    for (label, value) in [
        (
            "in_progress_state",
            settings.tracker.in_progress_state.as_ref(),
        ),
        (
            "human_review_state",
            settings.tracker.human_review_state.as_ref(),
        ),
        ("done_state", settings.tracker.done_state.as_ref()),
    ] {
        let Some(value) = value else {
            continue;
        };
        let normalized = normalize_issue_state(value);
        if !option_names
            .iter()
            .any(|candidate| candidate == &normalized)
        {
            missing.push(format!("{label}:{value}"));
        }
    }

    if !missing.is_empty() {
        return DoctorCheck {
            name: "project_status_mapping",
            status: DoctorStatus::Fail,
            detail: format!(
                "configured states missing from project field {}: {}",
                overview.field_name,
                missing.join(", ")
            ),
        };
    }

    DoctorCheck {
        name: "project_status_mapping",
        status: DoctorStatus::Pass,
        detail: format!(
            "field={} options={} items={}",
            overview.field_name,
            overview.options.join(", "),
            overview.total_items
        ),
    }
}

fn check_workspace_root(path: &Path) -> DoctorCheck {
    if path.exists() {
        return DoctorCheck {
            name: "workspace_root",
            status: DoctorStatus::Pass,
            detail: format!("exists at {}", path.display()),
        };
    }

    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    if parent.exists() {
        DoctorCheck {
            name: "workspace_root",
            status: DoctorStatus::Warn,
            detail: format!(
                "workspace root does not exist yet, but parent {} exists",
                parent.display()
            ),
        }
    } else {
        DoctorCheck {
            name: "workspace_root",
            status: DoctorStatus::Fail,
            detail: format!("parent directory {} does not exist", parent.display()),
        }
    }
}

fn print_text_report(report: &DoctorReport) {
    println!("Kairastra doctor report");
    println!("mode: {}", report.mode.as_str());
    if let Some(path) = report.workflow_path.as_ref() {
        println!("workflow: {}", path.display());
    }
    for check in &report.checks {
        println!(
            "[{}] {}: {}",
            match check.status {
                DoctorStatus::Pass => "PASS",
                DoctorStatus::Warn => "WARN",
                DoctorStatus::Fail => "FAIL",
            },
            check.name,
            check.detail
        );
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::infer_mode;
    use crate::deploy::DeployMode;
    use tempfile::tempdir;

    #[test]
    fn infer_mode_prefers_explicit_env() {
        std::env::set_var("KAIRASTRA_DEPLOY_MODE", "docker");
        assert_eq!(infer_mode(), DeployMode::Docker);
        std::env::set_var("KAIRASTRA_DEPLOY_MODE", "native");
        assert_eq!(infer_mode(), DeployMode::Native);
        std::env::remove_var("KAIRASTRA_DEPLOY_MODE");
    }

    #[tokio::test]
    async fn docker_doctor_rejects_removed_host_bind_keys() {
        let dir = tempdir().unwrap();
        let env_path = dir.path().join("docker.env");
        fs::write(
            &env_path,
            "KAIRASTRA_DEPLOY_MODE=docker\nWORKFLOW_FILE=../WORKFLOW.md\n",
        )
        .unwrap();

        let error = super::run(super::DoctorOptions {
            workflow: None,
            env_file: Some(env_path),
            mode: Some(DeployMode::Docker),
            format: super::DoctorFormat::Text,
        })
        .await
        .unwrap_err()
        .to_string();

        assert!(error.contains("removed_docker_env_keys"));
    }
}
