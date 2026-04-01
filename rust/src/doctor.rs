use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

use anyhow::{anyhow, Result};
use clap::ValueEnum;
use serde::Serialize;

use crate::auth::find_command;
use crate::config::{normalize_issue_state, FieldSourceType, Settings};
use crate::deploy::DeployMode;
use crate::envfile::{apply_env, load_env_file};
use crate::github::GitHubTracker;
use crate::github_bootstrap::{
    default_label_specs, derive_status_option_names, inspect_project_field_readiness,
    inspect_repo_label_readiness,
};
use crate::providers;
use crate::shared_skills;
use crate::workflow::{default_env_file_path, default_workflow_path, load_definition};

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
    let env_file_path = resolve_env_file_path(options.env_file.as_ref());
    let _env_values = if let Some(path) = env_file_path.as_ref() {
        let env_values = load_env_file(path)?;
        apply_env(&env_values);
        Some(env_values)
    } else {
        None
    };

    let workflow_path = resolve_workflow_path(options.workflow.as_ref());
    let mut checks = Vec::new();
    let mode = options.mode.unwrap_or_else(infer_mode);

    checks.push(check_command("gh"));
    if cfg!(target_os = "linux") {
        checks.push(check_command("systemctl"));
    }

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
    let mut provider_runtime_check = DoctorCheck {
        name: "agent_provider_runtime",
        status: DoctorStatus::Warn,
        detail: "workflow not loaded".to_string(),
    };
    let mut label_check = DoctorCheck {
        name: "github_repo_labels",
        status: DoctorStatus::Warn,
        detail: "workflow not loaded".to_string(),
    };
    let mut shared_skills_check = DoctorCheck {
        name: "seed_repo_skills",
        status: DoctorStatus::Warn,
        detail: "workflow not loaded".to_string(),
    };
    let mut seed_repo_git_check = DoctorCheck {
        name: "seed_repo_git",
        status: DoctorStatus::Warn,
        detail: "workflow not loaded".to_string(),
    };
    let mut project_field_check = DoctorCheck {
        name: "github_project_fields",
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
                    provider_runtime_check = check_provider_runtime(&settings).await;
                    seed_repo_git_check = check_seed_repo_git();
                    checks.push(check_seed_repo_support_dirs(&settings));
                    shared_skills_check = check_seed_repo_skills();
                    label_check = check_repo_labels(&settings);
                    project_field_check = check_project_fields(&settings);
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
    checks.push(provider_runtime_check);
    checks.push(seed_repo_git_check);
    checks.push(shared_skills_check);
    checks.push(label_check);
    checks.push(project_field_check);
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

fn resolve_env_file_path(explicit: Option<&PathBuf>) -> Option<PathBuf> {
    if let Some(path) = explicit {
        return Some(path.clone());
    }

    default_env_file_path().ok().flatten()
}

fn infer_mode() -> DeployMode {
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
            "provider={} configured={} inferred={} auth_file={} api_key_present={} credentials_present={} local_auth_path={}",
            auth_status.provider,
            auth_status.configured_mode,
            auth_status.inferred_mode,
            auth_status.auth_file_present,
            auth_status.api_key_present,
            auth_status.credentials_present,
            auth_status.auth_file_path.display(),
        ),
    }
}

async fn check_provider_runtime(settings: &Settings) -> DoctorCheck {
    match settings.agent.provider.as_str() {
        "codex" => check_codex_runtime(settings).await,
        other => DoctorCheck {
            name: "agent_provider_runtime",
            status: DoctorStatus::Pass,
            detail: format!("not applicable for provider={other}"),
        },
    }
}

async fn check_codex_runtime(settings: &Settings) -> DoctorCheck {
    let version = codex_version_detail();
    let command = settings
        .providers
        .get(&settings.agent.provider)
        .and_then(|value| value.get("command"))
        .and_then(serde_yaml::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("codex app-server");

    match crate::providers::codex::runtime::probe_startup(settings).await {
        Ok(probe) => DoctorCheck {
            name: "agent_provider_runtime",
            status: DoctorStatus::Pass,
            detail: match version {
                Some(version) => format!(
                    "provider=codex command=\"{}\" version=\"{}\" startup ok; thread/start accepted (thread={})",
                    command, version, probe.thread_id
                ),
                None => format!(
                    "provider=codex command=\"{}\" startup ok; thread/start accepted (thread={})",
                    command, probe.thread_id
                ),
            },
        },
        Err(error) => DoctorCheck {
            name: "agent_provider_runtime",
            status: DoctorStatus::Fail,
            detail: match version {
                Some(version) => format!(
                    "provider=codex command=\"{}\" version=\"{}\" is incompatible with Kairastra's app-server startup path: {}. Kairastra launches Codex in a sanitized environment and expects initialize + thread/start to succeed.",
                    command, version, error
                ),
                None => format!(
                    "provider=codex command=\"{}\" is incompatible with Kairastra's app-server startup path: {}. Kairastra launches Codex in a sanitized environment and expects initialize + thread/start to succeed.",
                    command, error
                ),
            },
        },
    }
}

fn codex_version_detail() -> Option<String> {
    let path = find_command("codex")?;
    let output = StdCommand::new(path).arg("--version").output().ok()?;
    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let version = stdout.trim();
    if version.is_empty() {
        None
    } else {
        Some(version.to_string())
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
        if parent
            .file_name()
            .and_then(|value| value.to_str())
            .map(|value| value == ".kairastra")
            .unwrap_or(false)
        {
            return DoctorCheck {
                name: "workspace_root",
                status: DoctorStatus::Pass,
                detail: format!(
                    "workspace root {} will be created on first run",
                    path.display()
                ),
            };
        }
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

fn check_repo_labels(settings: &Settings) -> DoctorCheck {
    let token = settings.tracker.api_key.as_str();
    let owner = settings.tracker.owner.as_str();
    let repo = settings.tracker.repo.as_deref().unwrap_or_default();
    if repo.is_empty() {
        return DoctorCheck {
            name: "github_repo_labels",
            status: DoctorStatus::Warn,
            detail: "tracker repo is not configured; skipping repo label validation".to_string(),
        };
    }

    let desired_statuses = derive_status_option_names(
        &settings.tracker.active_states,
        &settings.tracker.terminal_states,
        &settings.tracker.claimable_states,
        settings.tracker.in_progress_state.as_deref(),
        settings.tracker.human_review_state.as_deref(),
        settings.tracker.done_state.as_deref(),
    );
    let desired_specs = default_label_specs(&desired_statuses);

    match inspect_repo_label_readiness(token, owner, repo, &desired_specs) {
        Ok((missing, divergent)) if missing.is_empty() && divergent.is_empty() => DoctorCheck {
            name: "github_repo_labels",
            status: DoctorStatus::Pass,
            detail: format!(
                "expected label pack present: {}",
                desired_specs
                    .iter()
                    .map(|spec| spec.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        },
        Ok((missing, divergent)) => {
            let mut parts = Vec::new();
            if !missing.is_empty() {
                parts.push(format!("missing: {}", missing.join(", ")));
            }
            if !divergent.is_empty() {
                parts.push(format!("needs update: {}", divergent.join(", ")));
            }
            DoctorCheck {
                name: "github_repo_labels",
                status: DoctorStatus::Warn,
                detail: parts.join("; "),
            }
        }
        Err(error) => DoctorCheck {
            name: "github_repo_labels",
            status: DoctorStatus::Warn,
            detail: format!("could not inspect repo labels: {error}"),
        },
    }
}

fn check_project_fields(settings: &Settings) -> DoctorCheck {
    if settings.tracker.mode != crate::config::GitHubMode::ProjectsV2 {
        return DoctorCheck {
            name: "github_project_fields",
            status: DoctorStatus::Pass,
            detail: "not applicable for issues_only".to_string(),
        };
    }

    let Some(project_number) = settings.tracker.project_v2_number else {
        return DoctorCheck {
            name: "github_project_fields",
            status: DoctorStatus::Fail,
            detail: "project_v2_number is not configured".to_string(),
        };
    };
    let project_owner = settings
        .tracker
        .project_owner
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(settings.tracker.owner.as_str());
    let desired_statuses = derive_status_option_names(
        &settings.tracker.active_states,
        &settings.tracker.terminal_states,
        &settings.tracker.claimable_states,
        settings.tracker.in_progress_state.as_deref(),
        settings.tracker.human_review_state.as_deref(),
        settings.tracker.done_state.as_deref(),
    );

    match inspect_project_field_readiness(
        settings.tracker.api_key.as_str(),
        project_owner,
        &project_number.to_string(),
        "Status",
        "Priority",
        &desired_statuses,
    ) {
        Ok(readiness)
            if readiness.status_present
                && readiness.priority_present
                && readiness.missing_status_options.is_empty() =>
        {
            DoctorCheck {
            name: "github_project_fields",
            status: DoctorStatus::Pass,
            detail: format!(
                "required project fields present: Status, Priority; selected statuses available: {}",
                desired_statuses.join(", ")
            ),
        }
        }
        Ok(readiness) => {
            let mut missing = Vec::new();
            if !readiness.status_present {
                missing.push("Status");
            }
            if !readiness.priority_present {
                missing.push("Priority");
            }
            let mut parts = Vec::new();
            if !missing.is_empty() {
                parts.push(format!("missing required project fields: {}", missing.join(", ")));
            }
            if !readiness.missing_status_options.is_empty() {
                parts.push(format!(
                    "missing selected status options: {}",
                    readiness.missing_status_options.join(", ")
                ));
            }
            DoctorCheck {
                name: "github_project_fields",
                status: DoctorStatus::Fail,
                detail: parts.join("; "),
            }
        }
        Err(error) => DoctorCheck {
            name: "github_project_fields",
            status: DoctorStatus::Fail,
            detail: format!("could not inspect project fields: {error}"),
        },
    }
}

fn check_seed_repo_support_dirs(settings: &Settings) -> DoctorCheck {
    let Some(seed_repo) = std::env::var("KAIRASTRA_SEED_REPO")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    else {
        return DoctorCheck {
            name: "seed_repo_support_dirs",
            status: DoctorStatus::Warn,
            detail:
                "KAIRASTRA_SEED_REPO is not set; skipping seed repo support directory validation"
                    .to_string(),
        };
    };

    let seed_repo_path = Path::new(&seed_repo);
    if !seed_repo_path.exists() {
        return DoctorCheck {
            name: "seed_repo_support_dirs",
            status: DoctorStatus::Fail,
            detail: format!(
                "seed repo path does not exist: {}",
                seed_repo_path.display()
            ),
        };
    }

    let required_support_dirs = match providers::repo_support_dirs(settings.agent.provider.as_str())
    {
        Ok(dirs) => dirs,
        Err(error) => {
            return DoctorCheck {
                name: "seed_repo_support_dirs",
                status: DoctorStatus::Fail,
                detail: error.to_string(),
            };
        }
    };

    let missing = required_support_dirs
        .iter()
        .map(|dir| seed_repo_path.join(dir))
        .filter(|path| !path.exists())
        .collect::<Vec<_>>();

    if missing.is_empty() {
        return DoctorCheck {
            name: "seed_repo_support_dirs",
            status: DoctorStatus::Pass,
            detail: format!(
                "required support directories present in {}: {}",
                seed_repo_path.display(),
                required_support_dirs.join(", ")
            ),
        };
    }

    DoctorCheck {
        name: "seed_repo_support_dirs",
        status: DoctorStatus::Fail,
        detail: format!(
            "seed repo is missing required support directories: {}",
            missing
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn check_seed_repo_git() -> DoctorCheck {
    let Some(seed_repo) = std::env::var("KAIRASTRA_SEED_REPO")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    else {
        return DoctorCheck {
            name: "seed_repo_git",
            status: DoctorStatus::Fail,
            detail: "KAIRASTRA_SEED_REPO is not set".to_string(),
        };
    };

    let seed_repo_path = Path::new(&seed_repo);
    if !seed_repo_path.exists() {
        return DoctorCheck {
            name: "seed_repo_git",
            status: DoctorStatus::Fail,
            detail: format!(
                "seed repo path does not exist: {}",
                seed_repo_path.display()
            ),
        };
    }

    let head_check = StdCommand::new("git")
        .arg("-C")
        .arg(seed_repo_path)
        .args(["rev-parse", "--verify", "HEAD"])
        .output();
    match head_check {
        Ok(output) if output.status.success() => {}
        Ok(_) => {
            return DoctorCheck {
                name: "seed_repo_git",
                status: DoctorStatus::Fail,
                detail: format!(
                    "seed repo must have at least one commit: {}",
                    seed_repo_path.display()
                ),
            };
        }
        Err(error) => {
            return DoctorCheck {
                name: "seed_repo_git",
                status: DoctorStatus::Fail,
                detail: format!("could not inspect seed repo HEAD: {error}"),
            };
        }
    }

    let origin_output = StdCommand::new("git")
        .arg("-C")
        .arg(seed_repo_path)
        .args(["config", "--get", "remote.origin.url"])
        .output();
    let origin_url = match origin_output {
        Ok(output) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        }
        Ok(_) => String::new(),
        Err(error) => {
            return DoctorCheck {
                name: "seed_repo_git",
                status: DoctorStatus::Fail,
                detail: format!("could not inspect seed repo origin remote: {error}"),
            };
        }
    };

    if origin_url.is_empty() {
        return DoctorCheck {
            name: "seed_repo_git",
            status: DoctorStatus::Fail,
            detail: format!(
                "seed repo is missing remote.origin.url: {}",
                seed_repo_path.display()
            ),
        };
    }

    let branch = StdCommand::new("git")
        .arg("-C")
        .arg(seed_repo_path)
        .args(["branch", "--show-current"])
        .output()
        .ok()
        .and_then(|output| {
            if output.status.success() {
                let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if value.is_empty() {
                    None
                } else {
                    Some(value)
                }
            } else {
                None
            }
        })
        .unwrap_or_else(|| "<unknown>".to_string());

    DoctorCheck {
        name: "seed_repo_git",
        status: DoctorStatus::Pass,
        detail: format!(
            "seed repo git checkout ready at {} (branch={}, origin={})",
            seed_repo_path.display(),
            branch,
            origin_url
        ),
    }
}

fn check_seed_repo_skills() -> DoctorCheck {
    let Some(seed_repo) = std::env::var("KAIRASTRA_SEED_REPO")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    else {
        return DoctorCheck {
            name: "seed_repo_skills",
            status: DoctorStatus::Warn,
            detail: "KAIRASTRA_SEED_REPO is not set; skipping shared skill validation".to_string(),
        };
    };

    let seed_repo_path = Path::new(&seed_repo);
    if !seed_repo_path.exists() {
        return DoctorCheck {
            name: "seed_repo_skills",
            status: DoctorStatus::Fail,
            detail: format!(
                "seed repo path does not exist: {}",
                seed_repo_path.display()
            ),
        };
    }

    let missing = shared_skills::missing_skill_entrypoints(seed_repo_path);
    if missing.is_empty() {
        return DoctorCheck {
            name: "seed_repo_skills",
            status: DoctorStatus::Pass,
            detail: "required Kairastra shared skills are present in the seed repo".to_string(),
        };
    }

    DoctorCheck {
        name: "seed_repo_skills",
        status: DoctorStatus::Fail,
        detail: format!(
            "seed repo is missing required Kairastra shared skill files: {}",
            missing
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ),
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
    use super::{
        check_seed_repo_git, check_seed_repo_skills, check_seed_repo_support_dirs,
        check_workspace_root, infer_mode, DoctorStatus,
    };
    use crate::deploy::DeployMode;
    use crate::workflow::load_definition;
    use std::fs;
    use std::path::Path;
    use std::sync::{Mutex, OnceLock};
    use tempfile::tempdir;

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn write_minimal_workflow(dir: &Path) -> std::path::PathBuf {
        let workflow_path = dir.join("WORKFLOW.md");
        fs::write(
            &workflow_path,
            r#"---
tracker:
  kind: github
  mode: issues_only
  api_key: ghp_test
  owner: example-owner
  repo: example-repo
  status_source:
    type: git_hub_state
  active_states:
    - Open
  terminal_states:
    - Closed
workspace:
  root: /tmp/kairastra-workspaces
agent:
  provider: codex
providers:
  codex:
    command: codex app-server
    approval_policy: never
    thread_sandbox: workspace-write
    turn_sandbox_policy:
      type: workspaceWrite
      networkAccess: true
---
"#,
        )
        .unwrap();
        workflow_path
    }

    fn init_git_repo(path: &Path) {
        let status = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(path)
            .status()
            .unwrap();
        assert!(status.success());
        fs::write(path.join("README.md"), "seed\n").unwrap();
        let status = std::process::Command::new("git")
            .args(["add", "README.md"])
            .current_dir(path)
            .status()
            .unwrap();
        assert!(status.success());
        let status = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=Kairastra",
                "-c",
                "user.email=kairastra@example.com",
                "commit",
                "-q",
                "-m",
                "init",
            ])
            .current_dir(path)
            .status()
            .unwrap();
        assert!(status.success());
        let status = std::process::Command::new("git")
            .args([
                "remote",
                "add",
                "origin",
                "https://github.com/example/repo.git",
            ])
            .current_dir(path)
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[test]
    fn infer_mode_defaults_to_native() {
        std::env::set_var("KAIRASTRA_DEPLOY_MODE", "native");
        assert_eq!(infer_mode(), DeployMode::Native);
        std::env::remove_var("KAIRASTRA_DEPLOY_MODE");
    }

    #[test]
    fn seed_repo_support_dirs_fail_when_missing() {
        let _guard = env_lock().lock().unwrap();
        let dir = tempdir().unwrap();
        std::env::set_var("KAIRASTRA_SEED_REPO", dir.path());
        let workflow_path = write_minimal_workflow(dir.path());
        let definition = load_definition(&workflow_path).unwrap();
        let settings = crate::config::Settings::from_workflow(&definition).unwrap();

        let check = check_seed_repo_support_dirs(&settings);
        assert_eq!(check.status, DoctorStatus::Fail);
        assert!(check.detail.contains(".agents"));

        std::env::remove_var("KAIRASTRA_SEED_REPO");
    }

    #[test]
    fn seed_repo_git_fails_without_head() {
        let _guard = env_lock().lock().unwrap();
        let dir = tempdir().unwrap();
        std::env::set_var("KAIRASTRA_SEED_REPO", dir.path());

        let check = check_seed_repo_git();
        assert_eq!(check.status, DoctorStatus::Fail);
        assert!(check.detail.contains("at least one commit"));

        std::env::remove_var("KAIRASTRA_SEED_REPO");
    }

    #[test]
    fn seed_repo_git_passes_with_head_and_origin() {
        let _guard = env_lock().lock().unwrap();
        let dir = tempdir().unwrap();
        init_git_repo(dir.path());
        std::env::set_var("KAIRASTRA_SEED_REPO", dir.path());

        let check = check_seed_repo_git();
        assert_eq!(check.status, DoctorStatus::Pass);
        assert!(check
            .detail
            .contains("origin=https://github.com/example/repo.git"));

        std::env::remove_var("KAIRASTRA_SEED_REPO");
    }

    #[test]
    fn seed_repo_support_dirs_pass_when_present() {
        let _guard = env_lock().lock().unwrap();
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".agents")).unwrap();
        fs::create_dir_all(dir.path().join(".github")).unwrap();
        std::env::set_var("KAIRASTRA_SEED_REPO", dir.path());
        let workflow_path = write_minimal_workflow(dir.path());
        let definition = load_definition(&workflow_path).unwrap();
        let settings = crate::config::Settings::from_workflow(&definition).unwrap();

        let check = check_seed_repo_support_dirs(&settings);
        assert_eq!(check.status, DoctorStatus::Pass);

        std::env::remove_var("KAIRASTRA_SEED_REPO");
    }

    #[test]
    fn seed_repo_skills_fail_when_missing() {
        let _guard = env_lock().lock().unwrap();
        let dir = tempdir().unwrap();
        std::env::set_var("KAIRASTRA_SEED_REPO", dir.path());

        let check = check_seed_repo_skills();
        assert_eq!(check.status, DoctorStatus::Fail);
        assert!(check
            .detail
            .contains(".agents/skills/kairastra-commit/SKILL.md"));

        std::env::remove_var("KAIRASTRA_SEED_REPO");
    }

    #[test]
    fn seed_repo_skills_pass_when_present() {
        let _guard = env_lock().lock().unwrap();
        let dir = tempdir().unwrap();
        crate::shared_skills::install_shared_skills(dir.path()).unwrap();
        std::env::set_var("KAIRASTRA_SEED_REPO", dir.path());

        let check = check_seed_repo_skills();
        assert_eq!(check.status, DoctorStatus::Pass);

        std::env::remove_var("KAIRASTRA_SEED_REPO");
    }

    #[test]
    fn workspace_root_under_dot_kairastra_is_pass_when_missing() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".kairastra")).unwrap();

        let check = check_workspace_root(&dir.path().join(".kairastra/workspaces"));
        assert_eq!(check.status, DoctorStatus::Pass);
        assert!(check.detail.contains("will be created on first run"));
    }

    #[test]
    fn workspace_root_with_existing_parent_outside_dot_kairastra_stays_warn() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("custom-root")).unwrap();

        let check = check_workspace_root(&dir.path().join("custom-root/workspaces"));
        assert_eq!(check.status, DoctorStatus::Warn);
        assert!(check.detail.contains("does not exist yet"));
    }
}
