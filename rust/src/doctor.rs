use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use clap::ValueEnum;
use serde::Serialize;

use crate::auth::{find_command, inspect_status, AuthProvider};
use crate::config::Settings;
use crate::deploy::DeployMode;
use crate::envfile::{apply_env, load_env_file};
use crate::github::GitHubTracker;
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
    if let Some(path) = options.env_file.as_ref() {
        let env_values = load_env_file(path)?;
        apply_env(&env_values);
    }

    let workflow_path = resolve_workflow_path(options.workflow.as_ref());
    let mut checks = Vec::new();
    let mode = options.mode.unwrap_or_else(|| infer_mode());

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
                    provider_command_check = check_command(provider_command_name(&settings));
                    provider_auth_check = check_auth_status(&settings);
                    tracker_check = check_github_tracker(&settings).await;
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
    match std::env::var("SYMPHONY_DEPLOY_MODE")
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
    match find_command(name) {
        Some(path) => DoctorCheck {
            name,
            status: DoctorStatus::Pass,
            detail: format!("found at {}", path.display()),
        },
        None => DoctorCheck {
            name,
            status: DoctorStatus::Fail,
            detail: "not found in PATH".to_string(),
        },
    }
}

fn provider_command_name(settings: &Settings) -> &'static str {
    match settings.agent.provider {
        crate::config::AgentProvider::Codex => "codex",
        crate::config::AgentProvider::Claude => "claude",
        crate::config::AgentProvider::Gemini => "gemini",
    }
}

fn check_auth_status(settings: &Settings) -> DoctorCheck {
    let provider = match AuthProvider::from_agent_provider(settings.agent.provider) {
        Ok(provider) => provider,
        Err(error) => {
            return DoctorCheck {
                name: "agent_provider_auth",
                status: DoctorStatus::Fail,
                detail: error.to_string(),
            };
        }
    };
    let auth_status = inspect_status(provider);
    DoctorCheck {
        name: "agent_provider_auth",
        status: if auth_status.openai_api_key_present || auth_status.auth_file_present {
            DoctorStatus::Pass
        } else {
            DoctorStatus::Warn
        },
        detail: format!(
            "provider={} configured={} inferred={} auth_file={} api_key_present={} local_auth_path={} docker_hint={}",
            auth_status.provider,
            auth_status.configured_mode,
            auth_status.inferred_mode,
            auth_status.auth_file_present,
            auth_status.openai_api_key_present,
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
    println!("Symphony doctor report");
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
    use super::infer_mode;
    use crate::deploy::DeployMode;

    #[test]
    fn infer_mode_prefers_explicit_env() {
        std::env::set_var("SYMPHONY_DEPLOY_MODE", "docker");
        assert_eq!(infer_mode(), DeployMode::Docker);
        std::env::set_var("SYMPHONY_DEPLOY_MODE", "native");
        assert_eq!(infer_mode(), DeployMode::Native);
        std::env::remove_var("SYMPHONY_DEPLOY_MODE");
    }
}
