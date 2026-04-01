use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, Context, Result};
use dialoguer::{theme::ColorfulTheme, Confirm, Input, MultiSelect, Password, Select};

use crate::config::{FieldSource, FieldSourceType, GitHubMode, TrackerSettings};
use crate::deploy::DeployMode;
use crate::doctor::{self, DoctorFormat, DoctorOptions};
use crate::github::{GitHubTracker, ProjectStatusOverview};
use crate::github_bootstrap::{
    apply_bootstrap_plan, derive_status_option_names, inspect_bootstrap_plan, BootstrapOptions,
    StatusFieldMode,
};
use crate::providers::{self, ProviderSetupConfig};
use crate::shared_skills;
use crate::workflow::{OPERATOR_CONFIG_DIRNAME, OPERATOR_ENV_FILENAME, REPO_WORKFLOW_FILENAME};

#[derive(Debug, Clone)]
pub struct SetupOptions {
    pub mode: Option<DeployMode>,
    pub workflow: Option<PathBuf>,
    pub env_file: Option<PathBuf>,
    pub service_unit: Option<PathBuf>,
    pub binary_path: Option<PathBuf>,
    pub bootstrap_github: bool,
    pub skip_labels: bool,
    pub skip_priority_field: bool,
    pub reconfigure: bool,
    pub non_interactive: bool,
}

#[derive(Debug, Clone)]
struct SetupValues {
    tracker_mode: GitHubMode,
    project_status: ProjectStatusConfig,
    normalize_project_statuses: bool,
    provider: String,
    provider_configs: Vec<ProviderSetupConfig>,
    github_owner: String,
    github_repo: String,
    github_project_owner: String,
    github_project_number: String,
    github_project_url: String,
    workspace_root: String,
    seed_repo: String,
    assignee_login: String,
    max_concurrent_agents: String,
    max_turns: String,
    github_token: String,
    anthropic_api_key: String,
    gemini_api_key: String,
    openai_api_key: String,
    rust_log: String,
    binary_path: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProjectOwnerKind {
    User,
    Organization,
}

#[derive(Debug, Clone)]
struct ProjectStatusConfig {
    active_states: Vec<String>,
    terminal_states: Vec<String>,
    claimable_states: Vec<String>,
    in_progress_state: Option<String>,
    human_review_state: Option<String>,
    done_state: Option<String>,
}

const GITHUB_TOKEN_SETTINGS_URL: &str = "https://github.com/settings/tokens";
const GITHUB_TOKEN_CLASSIC_URL: &str = "https://github.com/settings/tokens/new";
const CANONICAL_WORKFLOW_TEMPLATE: &str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../WORKFLOW.md"));
const CANONICAL_PR_TEMPLATE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../.github/pull_request_template.md"
));

pub async fn run(options: SetupOptions) -> Result<()> {
    let layout = detect_layout(&std::env::current_dir()?)?;
    let mode = choose_mode(options.mode, options.non_interactive)?;
    let workflow_path = resolve_workflow_path(&layout, options.workflow.as_ref());
    guard_default_workflow_target(&layout, options.workflow.as_ref(), &workflow_path)?;
    let env_file_path = resolve_env_file_path(&layout, mode, options.env_file.as_ref());
    let service_unit_path = resolve_service_path(&layout, options.service_unit.as_ref());

    let values = collect_values(
        &layout,
        mode,
        options.binary_path.as_ref(),
        options.non_interactive,
    )
    .await?;

    let scaffolded_support_assets = ensure_repo_support_dirs(&layout.repo_root, &values.provider)?;
    let added_ignore_rule = ensure_local_ignore_rule(&layout.repo_root, ".kairastra/")?;
    let shared_skills = ensure_shared_skills(&layout.repo_root, options.non_interactive)?;
    let workflow_content = render_workflow(mode, &values);
    let env_file_content = render_env_file(mode, &values, &workflow_path);

    write_text_file(
        &workflow_path,
        &workflow_content,
        options.non_interactive,
        options.reconfigure,
    )?;
    write_text_file(
        &env_file_path,
        &env_file_content,
        options.non_interactive,
        options.reconfigure,
    )?;

    if let Some(path) = service_unit_path.as_ref() {
        let unit = render_systemd_unit(&values, &workflow_path, &env_file_path);
        write_text_file(path, &unit, options.non_interactive, options.reconfigure)?;
    }

    let github_bootstrap = maybe_bootstrap_github(
        &layout,
        &values,
        options.bootstrap_github,
        options.skip_labels,
        options.skip_priority_field,
        options.non_interactive,
    )?;

    doctor::run(DoctorOptions {
        workflow: Some(workflow_path.clone()),
        env_file: Some(env_file_path.clone()),
        mode: Some(mode),
        format: DoctorFormat::Text,
    })
    .await
    .context("generated files failed doctor validation")?;

    println!();
    println!("Generated:");
    println!("- workflow: {}", workflow_path.display());
    println!("- env file: {}", env_file_path.display());
    for path in &scaffolded_support_assets {
        println!("- scaffolded support asset: {}", path.display());
    }
    if added_ignore_rule {
        println!("- .gitignore updated with .kairastra/");
    }
    print_shared_skills_result(&shared_skills);
    let provider_auth_ready = providers::inspect_auth_status(&values.provider)
        .map(|status| status.credentials_usable)
        .unwrap_or(false);

    if let Some(path) = service_unit_path.as_ref() {
        println!("- systemd unit: {}", path.display());
    }
    print_github_bootstrap_result(&github_bootstrap);
    println!("Next steps:");
    let mut step = 1;
    println!(
        "{}. Review {} and {}",
        step,
        workflow_path.display(),
        env_file_path.display()
    );
    step += 1;
    if !provider_auth_ready {
        println!(
            "{}. Initialize provider auth later if needed: {}",
            step,
            native_login_command(&values.provider)
        );
        step += 1;
    }
    if let Some(path) = service_unit_path.as_ref() {
        println!(
            "{step}. Install the unit: sudo cp {} /etc/systemd/system/kairastra.service",
            path.display()
        );
        println!("{}. Reload systemd: sudo systemctl daemon-reload", step + 1);
        println!(
            "{}. Start Kairastra: sudo systemctl enable --now kairastra.service",
            step + 2
        );
    } else if options.workflow.is_some() {
        println!(
            "{step}. Start Kairastra: kairastra run {}",
            workflow_path.display()
        );
    } else {
        println!("{step}. Start Kairastra: kairastra run");
    }

    Ok(())
}

fn choose_mode(explicit: Option<DeployMode>, non_interactive: bool) -> Result<DeployMode> {
    if let Some(mode) = explicit {
        return Ok(mode);
    }

    if non_interactive {
        return Ok(DeployMode::Native);
    }

    Ok(DeployMode::Native)
}

#[derive(Debug, Clone)]
struct SetupLayout {
    repo_root: PathBuf,
    #[allow(dead_code)]
    rust_dir: PathBuf,
}

fn detect_layout(cwd: &Path) -> Result<SetupLayout> {
    let repo_root = detect_git_repo_root(cwd)
        .ok_or_else(|| anyhow!("`kairastra setup` must be run inside a Git repository"))?;
    let rust_dir = if repo_root.join("rust/Cargo.toml").is_file() {
        repo_root.join("rust")
    } else {
        repo_root.clone()
    };

    Ok(SetupLayout {
        repo_root,
        rust_dir,
    })
}

fn resolve_workflow_path(layout: &SetupLayout, explicit: Option<&PathBuf>) -> PathBuf {
    if let Some(path) = explicit {
        return path.clone();
    }

    layout.repo_root.join(REPO_WORKFLOW_FILENAME)
}

fn resolve_env_file_path(
    layout: &SetupLayout,
    mode: DeployMode,
    explicit: Option<&PathBuf>,
) -> PathBuf {
    if let Some(path) = explicit {
        return path.clone();
    }

    let _ = mode;
    layout
        .repo_root
        .join(OPERATOR_CONFIG_DIRNAME)
        .join(OPERATOR_ENV_FILENAME)
}

fn resolve_service_path(layout: &SetupLayout, explicit: Option<&PathBuf>) -> Option<PathBuf> {
    if let Some(path) = explicit {
        return Some(path.clone());
    }

    if cfg!(target_os = "linux") {
        return Some(
            layout
                .repo_root
                .join(OPERATOR_CONFIG_DIRNAME)
                .join("kairastra.service"),
        );
    }

    None
}

fn ensure_local_ignore_rule(repo_root: &Path, entry: &str) -> Result<bool> {
    let gitignore_path = repo_root.join(".gitignore");
    if repo_ignore_contains(&gitignore_path, entry)? {
        return Ok(false);
    }

    let existing = fs::read_to_string(&gitignore_path).unwrap_or_default();
    let mut updated = existing;
    if !updated.is_empty() && !updated.ends_with('\n') {
        updated.push('\n');
    }
    updated.push_str(entry);
    updated.push('\n');
    fs::write(&gitignore_path, updated)
        .with_context(|| format!("failed to update {}", gitignore_path.display()))?;
    Ok(true)
}

fn repo_ignore_contains(path: &Path, entry: &str) -> Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let target = entry.trim_end_matches('/');
    Ok(content.lines().map(str::trim).any(|line| {
        let normalized = line.trim_end_matches('/');
        normalized == target || normalized == format!("/{target}")
    }))
}

fn guard_default_workflow_target(
    layout: &SetupLayout,
    explicit_workflow: Option<&PathBuf>,
    workflow_path: &Path,
) -> Result<()> {
    if explicit_workflow.is_some() {
        return Ok(());
    }

    let Some(source_root) = canonical_source_repo_root() else {
        return Ok(());
    };

    let repo_root =
        fs::canonicalize(&layout.repo_root).unwrap_or_else(|_| layout.repo_root.clone());
    let workflow_target =
        fs::canonicalize(workflow_path).unwrap_or_else(|_| workflow_path.to_path_buf());

    if repo_root == source_root && workflow_target == source_root.join(REPO_WORKFLOW_FILENAME) {
        return Err(anyhow!(
            "refusing to overwrite the Kairastra source repo's canonical WORKFLOW.md; pass --workflow with an explicit target path if you want to generate a workflow here"
        ));
    }

    Ok(())
}

fn canonical_source_repo_root() -> Option<PathBuf> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    fs::canonicalize(path).ok()
}

#[derive(Debug, Clone)]
struct GithubBootstrapResult {
    inspected: bool,
    applied: bool,
    changes: Vec<String>,
    already_satisfied: Vec<String>,
}

fn maybe_bootstrap_github(
    _layout: &SetupLayout,
    values: &SetupValues,
    bootstrap_github: bool,
    skip_labels: bool,
    skip_priority_field: bool,
    non_interactive: bool,
) -> Result<GithubBootstrapResult> {
    let bootstrap_status = effective_status_config(values);
    let options = BootstrapOptions {
        token: &values.github_token,
        owner: &values.github_owner,
        repo: &values.github_repo,
        project_owner: if values.github_project_owner.trim().is_empty() {
            None
        } else {
            Some(values.github_project_owner.as_str())
        },
        project_number: if values.github_project_number.trim().is_empty() {
            None
        } else {
            Some(values.github_project_number.as_str())
        },
        status_field_name: "Status",
        priority_field_name: "Priority",
        status_field_mode: if values.normalize_project_statuses {
            StatusFieldMode::Normalize
        } else {
            StatusFieldMode::Preserve
        },
        status_options: desired_status_options(&bootstrap_status),
        skip_labels,
        skip_priority_field,
    };

    let plan = inspect_bootstrap_plan(&options)?;
    if non_interactive {
        if bootstrap_github {
            let applied = apply_bootstrap_plan(&options)?;
            return Ok(GithubBootstrapResult {
                inspected: true,
                applied: true,
                changes: applied.changes,
                already_satisfied: applied.already_satisfied,
            });
        }
        return Ok(GithubBootstrapResult {
            inspected: true,
            applied: false,
            changes: plan.changes,
            already_satisfied: plan.already_satisfied,
        });
    }

    if plan.is_empty() {
        return Ok(GithubBootstrapResult {
            inspected: true,
            applied: false,
            changes: Vec::new(),
            already_satisfied: plan.already_satisfied,
        });
    }

    println!();
    println!("GitHub bootstrap plan:");
    for change in &plan.changes {
        println!("- {change}");
    }
    if Confirm::with_theme(&ColorfulTheme::default())
        .with_prompt("Apply these GitHub changes now?")
        .default(true)
        .interact()?
    {
        let applied = apply_bootstrap_plan(&options)?;
        return Ok(GithubBootstrapResult {
            inspected: true,
            applied: true,
            changes: applied.changes,
            already_satisfied: applied.already_satisfied,
        });
    }

    Ok(GithubBootstrapResult {
        inspected: true,
        applied: false,
        changes: plan.changes,
        already_satisfied: plan.already_satisfied,
    })
}

fn desired_status_options(config: &ProjectStatusConfig) -> Vec<String> {
    derive_status_option_names(
        &config.active_states,
        &config.terminal_states,
        &config.claimable_states,
        config.in_progress_state.as_deref(),
        config.human_review_state.as_deref(),
        config.done_state.as_deref(),
    )
}

fn issues_only_status_config() -> ProjectStatusConfig {
    canonical_project_status_config()
}

fn effective_status_config(values: &SetupValues) -> ProjectStatusConfig {
    match values.tracker_mode {
        GitHubMode::ProjectsV2 => values.project_status.clone(),
        GitHubMode::IssuesOnly => issues_only_status_config(),
    }
}

fn print_github_bootstrap_result(result: &GithubBootstrapResult) {
    if !result.inspected {
        return;
    }
    if result.changes.is_empty() {
        println!("- GitHub metadata: already satisfied");
        return;
    }
    if result.applied {
        println!("- GitHub changes applied:");
    } else {
        println!("- GitHub changes skipped:");
    }
    for change in &result.changes {
        println!("  - {change}");
    }
    if !result.already_satisfied.is_empty() {
        println!("  already satisfied:");
        for item in &result.already_satisfied {
            println!("  - {item}");
        }
    }
}

fn ensure_repo_support_dirs(repo_root: &Path, provider: &str) -> Result<Vec<PathBuf>> {
    let mut scaffolded = Vec::new();
    for support_dir in providers::repo_support_dirs(provider)? {
        let support_path = repo_root.join(support_dir);
        if support_path.is_file() {
            return Err(anyhow!(
                "required repository support path exists as a file: {}",
                support_path.display()
            ));
        }
        if support_path.exists() {
            continue;
        }

        fs::create_dir_all(&support_path)?;
        let placeholder = support_path.join(".gitkeep");
        fs::write(&placeholder, b"")?;
        scaffolded.push(support_path);
    }

    let pr_template_path = repo_root.join(".github/pull_request_template.md");
    if !pr_template_path.exists() {
        if let Some(parent) = pr_template_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&pr_template_path, CANONICAL_PR_TEMPLATE).with_context(|| {
            format!(
                "failed to write canonical PR template to {}",
                pr_template_path.display()
            )
        })?;
        scaffolded.push(pr_template_path);
    }

    Ok(scaffolded)
}

#[derive(Debug, Clone)]
struct SharedSkillsResult {
    applied: bool,
    synced_dirs: Vec<String>,
    already_current: Vec<String>,
}

fn ensure_shared_skills(repo_root: &Path, non_interactive: bool) -> Result<SharedSkillsResult> {
    let plan = shared_skills::inspect_shared_skill_plan(repo_root)?;
    if plan.is_empty() {
        return Ok(SharedSkillsResult {
            applied: false,
            synced_dirs: Vec::new(),
            already_current: shared_skills::SHARED_SKILL_DIRS
                .iter()
                .map(|dir| dir.display_name.to_string())
                .collect(),
        });
    }

    if non_interactive {
        return Err(anyhow!(
            "required Kairastra workflow skills are missing or outdated under .agents/skills; rerun `krstr setup` interactively and confirm copying them into the repo"
        ));
    }

    println!();
    println!("Kairastra shared skills plan:");
    for dir in &plan.missing_dirs {
        println!("- copy {}", dir.relative_path);
    }
    for dir in &plan.outdated_dirs {
        println!("- update {}", dir.relative_path);
    }

    let confirmed = Confirm::with_theme(&ColorfulTheme::default())
        .with_prompt("Copy or update the required Kairastra workflow skills in this repo now?")
        .default(true)
        .interact()?;

    if !confirmed {
        return Err(anyhow!(
            "setup requires the Kairastra workflow skills under .agents/skills; rerun `krstr setup` and confirm copying them into the repo"
        ));
    }

    shared_skills::install_shared_skills(repo_root)?;

    Ok(SharedSkillsResult {
        applied: true,
        synced_dirs: plan
            .missing_or_outdated_dirs()
            .iter()
            .map(|dir| dir.display_name.to_string())
            .collect(),
        already_current: shared_skills::SHARED_SKILL_DIRS
            .iter()
            .filter(|dir| {
                !plan
                    .missing_or_outdated_dirs()
                    .iter()
                    .any(|changed| changed.relative_path == dir.relative_path)
            })
            .map(|dir| dir.display_name.to_string())
            .collect(),
    })
}

fn print_shared_skills_result(result: &SharedSkillsResult) {
    if result.applied {
        println!("- shared skills copied or updated:");
        for dir in &result.synced_dirs {
            println!("  - {dir}");
        }
        return;
    }

    println!("- shared skills already present:");
    for dir in &result.already_current {
        println!("  - {dir}");
    }
}

fn native_login_command(provider: &str) -> String {
    format!("kairastra auth --provider {provider} login --mode subscription")
}

async fn collect_values(
    layout: &SetupLayout,
    mode: DeployMode,
    explicit_binary_path: Option<&PathBuf>,
    non_interactive: bool,
) -> Result<SetupValues> {
    let cwd = layout.repo_root.clone();
    let theme = ColorfulTheme::default();
    let env_github_owner = std::env::var("KAIRASTRA_GITHUB_OWNER").unwrap_or_default();
    let env_github_repo = std::env::var("KAIRASTRA_GITHUB_REPO").unwrap_or_default();
    let env_project_owner = std::env::var("KAIRASTRA_GITHUB_PROJECT_OWNER").unwrap_or_default();
    let env_project_number = std::env::var("KAIRASTRA_GITHUB_PROJECT_NUMBER").unwrap_or_default();
    let env_project_url = std::env::var("KAIRASTRA_GITHUB_PROJECT_URL").unwrap_or_default();
    let inferred_origin_url = git_origin_url(&layout.repo_root).unwrap_or_default();
    let repo_input_default = default_repo_input(
        &layout.repo_root,
        &env_github_owner,
        &env_github_repo,
        &inferred_origin_url,
    );
    let repo_input = ask_string(
        &theme,
        "GitHub repo to manage (name or GitHub URL)",
        repo_input_default,
        non_interactive,
        false,
    )?;
    let parsed_repo = parse_repo_input(&repo_input);
    let tracker_mode = choose_tracker_mode(
        &theme,
        non_interactive,
        !env_project_number.trim().is_empty() || !env_project_url.trim().is_empty(),
    )?;
    let github_project_url = if tracker_mode == GitHubMode::ProjectsV2 {
        ask_string(
            &theme,
            "GitHub Project URL (optional, auto-fills project owner and number)",
            env_project_url,
            non_interactive,
            true,
        )?
    } else {
        String::new()
    };
    let parsed_project = parse_project_url(&github_project_url);
    let github_owner = if !env_github_owner.trim().is_empty() {
        env_github_owner
    } else if let Some(parsed) = parsed_repo.as_ref().and_then(|parsed| parsed.owner.clone()) {
        parsed
    } else if let Some(parsed) = parsed_project.as_ref() {
        parsed.owner.clone()
    } else {
        ask_string(
            &theme,
            "GitHub owner",
            String::new(),
            non_interactive,
            false,
        )?
    };
    let github_repo = parsed_repo
        .as_ref()
        .map(|parsed| parsed.repo.clone())
        .unwrap_or(repo_input);
    if github_owner.trim().is_empty() {
        return Err(anyhow!(
            "could not determine GitHub owner from the current repo. Set KAIRASTRA_GITHUB_OWNER or configure a GitHub origin remote."
        ));
    }
    if github_repo.trim().is_empty() {
        return Err(anyhow!(
            "could not determine GitHub repo name from the current repo. Set KAIRASTRA_GITHUB_REPO or configure a GitHub origin remote."
        ));
    }
    let github_project_owner = if tracker_mode == GitHubMode::ProjectsV2 {
        if !env_project_owner.trim().is_empty() {
            env_project_owner
        } else if let Some(parsed) = parsed_project.as_ref() {
            parsed.owner.clone()
        } else {
            ask_string(
                &theme,
                "GitHub Project owner (leave blank to use GitHub owner)",
                github_owner.clone(),
                non_interactive,
                false,
            )?
        }
    } else {
        String::new()
    };
    let github_project_number = if tracker_mode == GitHubMode::ProjectsV2 {
        if !env_project_number.trim().is_empty() {
            env_project_number
        } else if let Some(parsed) = parsed_project.as_ref() {
            parsed.project_number.clone()
        } else {
            ask_string(
                &theme,
                "GitHub Project v2 number",
                String::new(),
                non_interactive,
                false,
            )?
        }
    } else {
        String::new()
    };
    let github_token = resolve_github_token(
        &theme,
        non_interactive,
        github_token_help_context(tracker_mode, parsed_project.as_ref()),
    )?;
    let project_status_overview = if tracker_mode == GitHubMode::ProjectsV2 {
        inspect_project_status_overview(
            &github_token,
            &github_owner,
            &github_repo,
            &github_project_owner,
            &github_project_number,
            &github_project_url,
        )
        .await
        .ok()
    } else {
        None
    };
    let (project_status, normalize_project_statuses) = if tracker_mode == GitHubMode::ProjectsV2 {
        collect_project_status_config(
            &theme,
            non_interactive,
            project_status_overview.as_ref(),
            &github_owner,
            &github_project_owner,
            &github_project_number,
        )?
    } else {
        (issues_only_status_config(), false)
    };
    let _ = mode;
    let workspace_root = std::env::var("KAIRASTRA_WORKSPACE_ROOT").unwrap_or_else(|_| {
        cwd.join(OPERATOR_CONFIG_DIRNAME)
            .join("workspaces")
            .display()
            .to_string()
    });
    let seed_repo = ask_string(
        &theme,
        "Seed repo path (local git checkout)",
        std::env::var("KAIRASTRA_SEED_REPO")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| cwd.display().to_string()),
        non_interactive,
        false,
    )?;
    let assignee_login = std::env::var("KAIRASTRA_AGENT_ASSIGNEE").unwrap_or_default();
    let max_concurrent_agents = "4".to_string();
    let max_turns = "20".to_string();
    let provider = choose_provider(&theme, non_interactive)?;
    let provider_configs = collect_provider_configs(&provider, non_interactive)?;
    let anthropic_api_key = std::env::var("ANTHROPIC_API_KEY").unwrap_or_default();
    let gemini_api_key = std::env::var("GEMINI_API_KEY")
        .or_else(|_| std::env::var("GOOGLE_API_KEY"))
        .unwrap_or_default();
    let openai_api_key = std::env::var("OPENAI_API_KEY").unwrap_or_default();
    let rust_log = std::env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string());
    let binary_path = detect_binary_path(explicit_binary_path);

    Ok(SetupValues {
        tracker_mode,
        project_status,
        normalize_project_statuses,
        provider,
        provider_configs,
        github_owner,
        github_repo,
        github_project_owner,
        github_project_number,
        github_project_url,
        workspace_root,
        seed_repo,
        assignee_login,
        max_concurrent_agents,
        max_turns,
        github_token,
        anthropic_api_key,
        gemini_api_key,
        openai_api_key,
        rust_log,
        binary_path,
    })
}

fn choose_tracker_mode(
    theme: &ColorfulTheme,
    non_interactive: bool,
    prefer_projects_v2: bool,
) -> Result<GitHubMode> {
    if non_interactive {
        return Ok(if prefer_projects_v2 {
            GitHubMode::ProjectsV2
        } else {
            GitHubMode::IssuesOnly
        });
    }

    let options = [GitHubMode::IssuesOnly, GitHubMode::ProjectsV2];
    let labels = [
        "Repository issues only (recommended)",
        "GitHub Project v2 queue",
    ];
    let default = if prefer_projects_v2 { 1 } else { 0 };
    let selection = Select::with_theme(theme)
        .with_prompt("Queue source")
        .items(&labels)
        .default(default)
        .interact()?;
    Ok(options[selection])
}

fn collect_provider_configs(
    selected_provider: &str,
    non_interactive: bool,
) -> Result<Vec<ProviderSetupConfig>> {
    let mut configs = Vec::new();
    for (provider, _) in providers::setup_provider_choices() {
        let provider_non_interactive = non_interactive || *provider != selected_provider;
        configs.push(providers::collect_setup_config(
            provider,
            provider_non_interactive,
        )?);
    }
    Ok(configs)
}

fn choose_provider(theme: &ColorfulTheme, non_interactive: bool) -> Result<String> {
    let env_provider = std::env::var("KAIRASTRA_AGENT_PROVIDER")
        .ok()
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty());
    if non_interactive {
        return Ok(env_provider.unwrap_or_else(|| providers::default_setup_provider().to_string()));
    }

    let choices = providers::setup_provider_choices();
    let default_provider =
        env_provider.unwrap_or_else(|| providers::default_setup_provider().to_string());
    let default_index = choices
        .iter()
        .position(|(provider, _)| *provider == default_provider)
        .unwrap_or(0);
    let labels = choices.iter().map(|(_, label)| *label).collect::<Vec<_>>();
    let selection = Select::with_theme(theme)
        .with_prompt("Agent provider")
        .items(&labels)
        .default(default_index)
        .interact()?;
    Ok(choices[selection].0.to_string())
}

fn detect_binary_path(explicit: Option<&PathBuf>) -> String {
    if let Some(path) = explicit {
        return path.display().to_string();
    }

    if let Ok(path) = std::env::var("KAIRASTRA_BINARY_PATH") {
        let trimmed = path.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }

    if let Ok(path) = std::env::current_exe() {
        let rendered = path.display().to_string();
        if !rendered.contains("/target/debug/") && !rendered.contains("/target/release/") {
            return rendered;
        }
    }

    "/usr/local/bin/kairastra".to_string()
}

fn default_repo_input(
    repo_root: &Path,
    env_github_owner: &str,
    env_github_repo: &str,
    origin_url: &str,
) -> String {
    if !env_github_owner.trim().is_empty() && !env_github_repo.trim().is_empty() {
        return format!("{}/{}", env_github_owner.trim(), env_github_repo.trim());
    }

    if !env_github_repo.trim().is_empty() {
        return env_github_repo.trim().to_string();
    }

    if let Some(parsed) = parse_repo_input(origin_url) {
        return match parsed.owner {
            Some(owner) => format!("{owner}/{}", parsed.repo),
            None => parsed.repo,
        };
    }

    repo_root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_string()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedProjectUrl {
    owner: String,
    project_number: String,
    owner_kind: ProjectOwnerKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedRepoInput {
    owner: Option<String>,
    repo: String,
}

fn parse_project_url(url: &str) -> Option<ParsedProjectUrl> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return None;
    }

    let without_scheme = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
        .unwrap_or(trimmed);
    let without_host = without_scheme
        .strip_prefix("github.com/")
        .or_else(|| without_scheme.strip_prefix("www.github.com/"))?;
    let path = without_host
        .split(['?', '#'])
        .next()
        .unwrap_or(without_host);
    let segments: Vec<&str> = path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();
    if segments.len() < 4 {
        return None;
    }
    if segments[0] != "users" && segments[0] != "orgs" {
        return None;
    }
    if segments[2] != "projects" {
        return None;
    }

    let project_number = segments[3];
    if project_number.parse::<u32>().is_err() {
        return None;
    }

    let owner_kind = match segments[0] {
        "users" => ProjectOwnerKind::User,
        "orgs" => ProjectOwnerKind::Organization,
        _ => return None,
    };

    Some(ParsedProjectUrl {
        owner: segments[1].to_string(),
        project_number: project_number.to_string(),
        owner_kind,
    })
}

fn parse_repo_input(value: &str) -> Option<ParsedRepoInput> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }

    if !trimmed.contains("://") && !trimmed.starts_with("git@") {
        let segments = trimmed
            .split('/')
            .filter(|segment| !segment.is_empty())
            .collect::<Vec<_>>();
        return match segments.as_slice() {
            [repo] => Some(ParsedRepoInput {
                owner: None,
                repo: (*repo).to_string(),
            }),
            [owner, repo] => Some(ParsedRepoInput {
                owner: Some((*owner).to_string()),
                repo: repo.trim_end_matches(".git").to_string(),
            }),
            _ => None,
        };
    }

    let without_host = if let Some(path) = trimmed.strip_prefix("git@github.com:") {
        path
    } else if let Some(path) = trimmed.strip_prefix("ssh://git@github.com/") {
        path
    } else {
        let without_scheme = trimmed
            .strip_prefix("https://")
            .or_else(|| trimmed.strip_prefix("http://"))
            .unwrap_or(trimmed);
        without_scheme
            .strip_prefix("github.com/")
            .or_else(|| without_scheme.strip_prefix("www.github.com/"))?
    };
    let path = without_host
        .split(['?', '#'])
        .next()
        .unwrap_or(without_host);
    let segments: Vec<&str> = path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();
    if segments.len() < 2 {
        return None;
    }

    let repo = segments[1].trim_end_matches(".git");
    if repo.is_empty() {
        return None;
    }

    Some(ParsedRepoInput {
        owner: Some(segments[0].to_string()),
        repo: repo.to_string(),
    })
}

fn detect_git_repo_root(cwd: &Path) -> Option<PathBuf> {
    let output = Command::new("git")
        .arg("rev-parse")
        .arg("--show-toplevel")
        .current_dir(cwd)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let root = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if root.is_empty() {
        None
    } else {
        Some(PathBuf::from(root))
    }
}

fn git_origin_url(repo_root: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["config", "--get", "remote.origin.url"])
        .current_dir(repo_root)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let origin = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if origin.is_empty() {
        None
    } else {
        Some(origin)
    }
}

fn ask_string(
    theme: &ColorfulTheme,
    prompt: &str,
    default: String,
    non_interactive: bool,
    allow_empty: bool,
) -> Result<String> {
    if non_interactive {
        return Ok(default);
    }

    let mut input = Input::<String>::with_theme(theme);
    input = input.with_prompt(prompt).default(default.clone());
    let value = input.interact_text()?;
    if allow_empty || !value.trim().is_empty() {
        Ok(value.trim().to_string())
    } else {
        Ok(default)
    }
}

fn ask_string_list(
    theme: &ColorfulTheme,
    prompt: &str,
    default: &[String],
    non_interactive: bool,
) -> Result<Vec<String>> {
    let rendered_default = default.join(", ");
    let value = ask_string(theme, prompt, rendered_default, non_interactive, true)?;
    Ok(parse_string_list(&value))
}

fn ask_optional_string(
    theme: &ColorfulTheme,
    prompt: &str,
    default: Option<&str>,
    non_interactive: bool,
) -> Result<Option<String>> {
    let value = ask_string(
        theme,
        prompt,
        default.unwrap_or_default().to_string(),
        non_interactive,
        true,
    )?;
    if value.trim().is_empty() {
        Ok(None)
    } else {
        Ok(Some(value))
    }
}

fn parse_string_list(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .collect()
}

fn parse_list_env(name: &str) -> Option<Vec<String>> {
    std::env::var(name)
        .ok()
        .map(|value| parse_string_list(&value))
        .filter(|values| !values.is_empty())
}

fn parse_optional_env(name: &str) -> Option<Option<String>> {
    let value = std::env::var(name).ok()?;
    let trimmed = value.trim();
    if trimmed.is_empty()
        || trimmed.eq_ignore_ascii_case("none")
        || trimmed.eq_ignore_ascii_case("null")
    {
        return Some(None);
    }
    Some(Some(trimmed.to_string()))
}

fn resolve_env_secret(names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| {
        std::env::var(name)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    })
}

fn print_setup_help_block<T: AsRef<str>>(title: &str, lines: &[T]) {
    println!();
    println!("{title}");
    for line in lines {
        println!("{}", line.as_ref());
    }
    println!();
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProjectStatusHelpTopic {
    Handling,
    ActiveStates,
    TerminalStates,
    ClaimableStates,
    InProgressState,
    HumanReviewState,
    DoneState,
}

fn project_status_help_title(topic: ProjectStatusHelpTopic) -> &'static str {
    match topic {
        ProjectStatusHelpTopic::Handling => "Project status handling",
        ProjectStatusHelpTopic::ActiveStates => "Dispatchable active states",
        ProjectStatusHelpTopic::TerminalStates => "Terminal states",
        ProjectStatusHelpTopic::ClaimableStates => "States treated as ready to claim",
        ProjectStatusHelpTopic::InProgressState => "Status to set when a claim starts",
        ProjectStatusHelpTopic::HumanReviewState => "Status to set for review or blocked handoff",
        ProjectStatusHelpTopic::DoneState => "Status to set when a closed issue is reconciled",
    }
}

fn project_status_help_lines(topic: ProjectStatusHelpTopic) -> Vec<&'static str> {
    match topic {
        ProjectStatusHelpTopic::Handling => vec![
            "- Choose whether Kairastra should map onto your existing Project statuses or rewrite the Project to Kairastra's default status set.",
            "- Keeping existing statuses is the safe default and does not mutate GitHub.",
        ],
        ProjectStatusHelpTopic::ActiveStates => vec![
            "- Pick the Project statuses Kairastra should treat as still in the working queue.",
            "- Only items in these active states are polled and dispatched until they move to a terminal state.",
        ],
        ProjectStatusHelpTopic::TerminalStates => vec![
            "- Pick the statuses Kairastra should treat as final or no longer dispatchable.",
            "- Closed issues and completed Project items should end up in one of these states.",
        ],
        ProjectStatusHelpTopic::ClaimableStates => vec![
            "- Optionally choose the subset of active states that mean an issue is ready to claim right now.",
            "- Leave this empty if any active state should be considered claimable.",
        ],
        ProjectStatusHelpTopic::InProgressState => vec![
            "- Choose the Project status Kairastra should set automatically when work begins on a claimed issue.",
            "- Select `Do not change project status` to leave the status untouched at claim time.",
        ],
        ProjectStatusHelpTopic::HumanReviewState => vec![
            "- Choose the Project status Kairastra should set when work needs human follow-up, review, or a blocked handoff.",
            "- Select `Do not change project status` to disable that automatic transition.",
        ],
        ProjectStatusHelpTopic::DoneState => vec![
            "- Choose the Project status Kairastra should set after it sees the issue is closed and reconciles the Project item.",
            "- Select `Do not change project status` to keep the Project status unchanged when the issue closes.",
        ],
    }
}

fn print_project_status_help(topic: ProjectStatusHelpTopic) {
    print_setup_help_block(
        project_status_help_title(topic),
        &project_status_help_lines(topic),
    );
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GitHubTokenHelpContext {
    tracker_mode: GitHubMode,
    project_owner_kind: Option<ProjectOwnerKind>,
}

fn github_token_help_context(
    tracker_mode: GitHubMode,
    parsed_project: Option<&ParsedProjectUrl>,
) -> GitHubTokenHelpContext {
    GitHubTokenHelpContext {
        tracker_mode,
        project_owner_kind: parsed_project.map(|project| project.owner_kind),
    }
}

fn github_token_tracker_guidance(context: GitHubTokenHelpContext) -> &'static str {
    match context {
        GitHubTokenHelpContext {
            tracker_mode: GitHubMode::ProjectsV2,
            project_owner_kind: Some(ProjectOwnerKind::User),
        } => "A GitHub token is required. User-owned Projects v2 require a classic PAT.",
        GitHubTokenHelpContext {
            tracker_mode: GitHubMode::ProjectsV2,
            project_owner_kind: Some(ProjectOwnerKind::Organization),
        } => "A GitHub token is required. Org-owned Projects v2 can use a classic PAT, and may support a fine-grained PAT when the org exposes the Projects permission.",
        GitHubTokenHelpContext {
            tracker_mode: GitHubMode::ProjectsV2,
            project_owner_kind: None,
        } => {
            "A GitHub token is required. User-owned Projects v2 require a classic PAT; org-owned Projects v2 can use a classic PAT and may support a fine-grained PAT when the org exposes the Projects permission."
        }
        GitHubTokenHelpContext {
            tracker_mode: GitHubMode::IssuesOnly,
            ..
        } => {
            "A GitHub token is required so Kairastra can read issues and clone/push against the target repo."
        }
    }
}

fn github_token_help_lines(context: GitHubTokenHelpContext) -> Vec<String> {
    let mut lines = vec![
        format!("- Token settings: {GITHUB_TOKEN_SETTINGS_URL}"),
        format!("- Classic token creation: {GITHUB_TOKEN_CLASSIC_URL}"),
        "- Existing GITHUB_TOKEN or GH_TOKEN env vars will skip this prompt.".to_string(),
    ];

    match context {
        GitHubTokenHelpContext {
            tracker_mode: GitHubMode::ProjectsV2,
            project_owner_kind: Some(ProjectOwnerKind::User),
        } => {
            lines.extend([
                "- For user-owned Projects v2, use a classic PAT, not a fine-grained PAT."
                    .to_string(),
                "- Recommended scopes: `project`, `repo` for private repos, and `workflow` when pushes may edit `.github/workflows/*`."
                    .to_string(),
                "- For read-only diagnostics, `read:project` can replace `project`.".to_string(),
            ]);
        }
        GitHubTokenHelpContext {
            tracker_mode: GitHubMode::ProjectsV2,
            project_owner_kind: Some(ProjectOwnerKind::Organization),
        } => {
            lines.extend([
                "- For org-owned Projects v2, a fine-grained PAT may work when the org exposes the `Projects` permission; a classic PAT also works."
                    .to_string(),
                "- If you use a fine-grained PAT, look for the org-level `Projects` permission. If it is missing, create a classic PAT instead."
                    .to_string(),
                "- Recommended classic PAT scopes: `project`, `repo` for private repos, and `workflow` when pushes may edit `.github/workflows/*`."
                    .to_string(),
                "- For read-only diagnostics with a classic PAT, `read:project` can replace `project`.".to_string(),
            ]);
        }
        GitHubTokenHelpContext {
            tracker_mode: GitHubMode::ProjectsV2,
            project_owner_kind: None,
        } => {
            lines.extend([
                "- User-owned Projects v2 require a classic PAT; org-owned Projects v2 may work with a fine-grained PAT when the org exposes the `Projects` permission."
                    .to_string(),
                "- If you do not see a `Projects` permission while creating a fine-grained PAT, create a classic PAT instead."
                    .to_string(),
                "- Recommended classic PAT scopes: `project`, `repo` for private repos, and `workflow` when pushes may edit `.github/workflows/*`."
                    .to_string(),
                "- For read-only diagnostics with a classic PAT, `read:project` can replace `project`.".to_string(),
            ]);
        }
        GitHubTokenHelpContext {
            tracker_mode: GitHubMode::IssuesOnly,
            ..
        } => {
            lines.extend([
                "- Recommended scopes: `repo` for private repos and `workflow` only when pushes may edit `.github/workflows/*`."
                    .to_string(),
                "- Public repos can often work without `repo`, but private repos need it."
                    .to_string(),
            ]);
        }
    }

    lines.extend([
        "- If the repo or project belongs to an org with SSO, authorize the token for SSO after creating it.".to_string(),
        "- More details: rust/README.md and docs/troubleshooting.md".to_string(),
    ]);
    lines
}

fn github_token_error_guidance(context: GitHubTokenHelpContext, non_interactive: bool) -> String {
    let action = if non_interactive {
        "Set GITHUB_TOKEN or GH_TOKEN before running setup."
    } else {
        "Provide a non-empty token."
    };
    format!(
        "{} {} Create or review tokens at {} or {}. See rust/README.md and docs/troubleshooting.md for scope details.",
        github_token_tracker_guidance(context),
        action,
        GITHUB_TOKEN_SETTINGS_URL,
        GITHUB_TOKEN_CLASSIC_URL
    )
}

fn validate_non_empty_token_input(input: &str) -> std::result::Result<(), &'static str> {
    if input.trim().is_empty() {
        Err("GitHub token cannot be empty.")
    } else {
        Ok(())
    }
}

fn resolve_github_token(
    theme: &ColorfulTheme,
    non_interactive: bool,
    help_context: GitHubTokenHelpContext,
) -> Result<String> {
    if let Some(token) = resolve_env_secret(&["GITHUB_TOKEN", "GH_TOKEN"]) {
        return Ok(token);
    }

    if let Some(token) = resolve_gh_cli_token() {
        return Ok(token);
    }

    if non_interactive {
        return Err(anyhow!(github_token_error_guidance(help_context, true)));
    }

    let help_lines = github_token_help_lines(help_context);
    print_setup_help_block("GitHub token setup", &help_lines);

    let token = Password::with_theme(theme)
        .with_prompt("GitHub token")
        .allow_empty_password(true)
        .validate_with(|input: &String| validate_non_empty_token_input(input))
        .interact()?;
    Ok(token.trim().to_string())
}

fn resolve_gh_cli_token() -> Option<String> {
    let output = Command::new("gh").args(["auth", "token"]).output().ok()?;
    if !output.status.success() {
        return None;
    }

    let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if token.is_empty() {
        None
    } else {
        Some(token)
    }
}

fn canonical_project_status_options() -> Vec<String> {
    vec![
        "Backlog".to_string(),
        "Todo".to_string(),
        "In Progress".to_string(),
        "Human Review".to_string(),
        "Merging".to_string(),
        "Rework".to_string(),
        "Done".to_string(),
        "Cancelled".to_string(),
        "Duplicate".to_string(),
    ]
}

fn canonical_project_status_config() -> ProjectStatusConfig {
    ProjectStatusConfig {
        active_states: vec![
            "Todo".to_string(),
            "In Progress".to_string(),
            "Merging".to_string(),
            "Rework".to_string(),
        ],
        terminal_states: vec![
            "Closed".to_string(),
            "Cancelled".to_string(),
            "Duplicate".to_string(),
            "Done".to_string(),
        ],
        claimable_states: vec!["Todo".to_string()],
        in_progress_state: Some("In Progress".to_string()),
        human_review_state: Some("Human Review".to_string()),
        done_state: Some("Done".to_string()),
    }
}

fn status_normalization_confirmation_token(project_owner: &str, project_number: &str) -> String {
    format!("normalize {project_owner}#{project_number}")
}

fn project_status_overview_block_reason(overview: &ProjectStatusOverview) -> Option<String> {
    if overview.total_items == 0 {
        return None;
    }

    let canonical = canonical_project_status_options()
        .into_iter()
        .map(|value| value.to_ascii_lowercase())
        .collect::<Vec<_>>();
    let incompatible = overview
        .item_counts
        .keys()
        .filter(|status| {
            !canonical
                .iter()
                .any(|candidate| candidate == &status.to_ascii_lowercase())
        })
        .cloned()
        .collect::<Vec<_>>();
    if incompatible.is_empty() {
        None
    } else {
        Some(format!(
            "normalization is blocked because this Project already has items in statuses that would be changed or removed: {}",
            incompatible.join(", ")
        ))
    }
}

fn build_project_status_tracker_settings(
    github_token: &str,
    github_owner: &str,
    github_repo: &str,
    github_project_owner: &str,
    github_project_number: &str,
    github_project_url: &str,
) -> Option<TrackerSettings> {
    if github_token.trim().is_empty() || github_project_number.trim().is_empty() {
        return None;
    }
    let project_number = github_project_number.trim().parse::<u32>().ok()?;
    Some(TrackerSettings {
        kind: "github".to_string(),
        mode: GitHubMode::ProjectsV2,
        api_key: github_token.to_string(),
        owner: github_owner.to_string(),
        repo: Some(github_repo.to_string()),
        project_owner: Some(github_project_owner.to_string())
            .filter(|value| !value.trim().is_empty()),
        project_v2_number: Some(project_number),
        project_url: Some(github_project_url.to_string()).filter(|value| !value.trim().is_empty()),
        active_states: Vec::new(),
        terminal_states: Vec::new(),
        claimable_states: Vec::new(),
        in_progress_state: None,
        human_review_state: None,
        done_state: None,
        status_source: Some(FieldSource {
            source_type: FieldSourceType::ProjectField,
            name: Some("Status".to_string()),
        }),
        priority_source: None,
        graphql_endpoint: "https://api.github.com/graphql".to_string(),
        rest_endpoint: "https://api.github.com".to_string(),
    })
}

async fn inspect_project_status_overview(
    github_token: &str,
    github_owner: &str,
    github_repo: &str,
    github_project_owner: &str,
    github_project_number: &str,
    github_project_url: &str,
) -> Result<ProjectStatusOverview> {
    let settings = build_project_status_tracker_settings(
        github_token,
        github_owner,
        github_repo,
        github_project_owner,
        github_project_number,
        github_project_url,
    )
    .ok_or_else(|| {
        anyhow!("project status inspection requires GITHUB_TOKEN and a valid project number")
    })?;
    let tracker = GitHubTracker::new(settings)?;
    tracker.inspect_project_status_overview().await
}

fn prompt_multi_select_states(
    theme: &ColorfulTheme,
    prompt: &str,
    options: &[String],
    defaults: &[String],
    counts: Option<&std::collections::HashMap<String, usize>>,
    allow_empty: bool,
) -> Result<Vec<String>> {
    let labels = options
        .iter()
        .map(|option| {
            let count = counts
                .and_then(|counts| counts.get(option))
                .copied()
                .unwrap_or(0);
            if count > 0 {
                format!("{option} ({count} items)")
            } else {
                option.clone()
            }
        })
        .collect::<Vec<_>>();
    let default_checks = options
        .iter()
        .map(|option| {
            defaults
                .iter()
                .any(|candidate| candidate.eq_ignore_ascii_case(option))
        })
        .collect::<Vec<_>>();
    let selections = MultiSelect::with_theme(theme)
        .with_prompt(prompt)
        .items(&labels)
        .defaults(&default_checks)
        .interact()?;
    let chosen = selections
        .into_iter()
        .filter_map(|index| options.get(index).cloned())
        .collect::<Vec<_>>();
    if !allow_empty && chosen.is_empty() {
        return Err(anyhow!("{prompt} requires at least one selection"));
    }
    Ok(chosen)
}

fn prompt_optional_state(
    theme: &ColorfulTheme,
    prompt: &str,
    options: &[String],
    default: Option<&str>,
    counts: Option<&std::collections::HashMap<String, usize>>,
) -> Result<Option<String>> {
    let mut labels = vec!["Do not change project status".to_string()];
    labels.extend(options.iter().map(|option| {
        let count = counts
            .and_then(|counts| counts.get(option))
            .copied()
            .unwrap_or(0);
        if count > 0 {
            format!("{option} ({count} items)")
        } else {
            option.clone()
        }
    }));
    let default_index = default
        .and_then(|target| {
            options
                .iter()
                .position(|option| option.eq_ignore_ascii_case(target))
                .map(|index| index + 1)
        })
        .unwrap_or(0);
    let selection = Select::with_theme(theme)
        .with_prompt(prompt)
        .items(&labels)
        .default(default_index)
        .interact()?;
    if selection == 0 {
        Ok(None)
    } else {
        Ok(options.get(selection - 1).cloned())
    }
}

fn collect_existing_project_status_config(
    theme: &ColorfulTheme,
    overview: &ProjectStatusOverview,
) -> Result<ProjectStatusConfig> {
    let defaults = canonical_project_status_config();
    print_project_status_help(ProjectStatusHelpTopic::ActiveStates);
    let active_states = prompt_multi_select_states(
        theme,
        "Dispatchable active states",
        &overview.options,
        &defaults.active_states,
        Some(&overview.item_counts),
        false,
    )?;
    let mut terminal_options = vec!["Closed".to_string()];
    terminal_options.extend(overview.options.clone());
    print_project_status_help(ProjectStatusHelpTopic::TerminalStates);
    let terminal_states = prompt_multi_select_states(
        theme,
        "Terminal states",
        &terminal_options,
        &defaults.terminal_states,
        Some(&overview.item_counts),
        false,
    )?;
    print_project_status_help(ProjectStatusHelpTopic::ClaimableStates);
    let claimable_states = prompt_multi_select_states(
        theme,
        "States treated as ready to claim (optional)",
        &active_states,
        &defaults.claimable_states,
        Some(&overview.item_counts),
        true,
    )?;
    print_project_status_help(ProjectStatusHelpTopic::InProgressState);
    let in_progress_state = prompt_optional_state(
        theme,
        "Status to set when a claim starts",
        &overview.options,
        defaults.in_progress_state.as_deref(),
        Some(&overview.item_counts),
    )?;
    print_project_status_help(ProjectStatusHelpTopic::HumanReviewState);
    let human_review_state = prompt_optional_state(
        theme,
        "Status to set for review or blocked handoff",
        &overview.options,
        defaults.human_review_state.as_deref(),
        Some(&overview.item_counts),
    )?;
    print_project_status_help(ProjectStatusHelpTopic::DoneState);
    let done_state = prompt_optional_state(
        theme,
        "Status to set when a closed issue is reconciled",
        &overview.options,
        defaults.done_state.as_deref(),
        Some(&overview.item_counts),
    )?;

    Ok(ProjectStatusConfig {
        active_states,
        terminal_states,
        claimable_states,
        in_progress_state,
        human_review_state,
        done_state,
    })
}

fn collect_manual_project_status_config(
    theme: &ColorfulTheme,
    non_interactive: bool,
) -> Result<ProjectStatusConfig> {
    let defaults = canonical_project_status_config();
    print_project_status_help(ProjectStatusHelpTopic::ActiveStates);
    let active_states = ask_string_list(
        theme,
        "Dispatchable active states (comma-separated)",
        &defaults.active_states,
        non_interactive,
    )?;
    print_project_status_help(ProjectStatusHelpTopic::TerminalStates);
    let terminal_states = ask_string_list(
        theme,
        "Terminal states (comma-separated)",
        &defaults.terminal_states,
        non_interactive,
    )?;
    print_project_status_help(ProjectStatusHelpTopic::ClaimableStates);
    let claimable_states = ask_string_list(
        theme,
        "Claimable states (comma-separated, optional)",
        &defaults.claimable_states,
        non_interactive,
    )?;
    print_project_status_help(ProjectStatusHelpTopic::InProgressState);
    let in_progress_state = ask_optional_string(
        theme,
        "Status to set when a claim starts (optional)",
        defaults.in_progress_state.as_deref(),
        non_interactive,
    )?;
    print_project_status_help(ProjectStatusHelpTopic::HumanReviewState);
    let human_review_state = ask_optional_string(
        theme,
        "Status to set for review or blocked handoff (optional)",
        defaults.human_review_state.as_deref(),
        non_interactive,
    )?;
    print_project_status_help(ProjectStatusHelpTopic::DoneState);
    let done_state = ask_optional_string(
        theme,
        "Status to set when a closed issue is reconciled (optional)",
        defaults.done_state.as_deref(),
        non_interactive,
    )?;
    Ok(ProjectStatusConfig {
        active_states,
        terminal_states,
        claimable_states,
        in_progress_state,
        human_review_state,
        done_state,
    })
}

fn collect_non_interactive_project_status_config() -> ProjectStatusConfig {
    let defaults = canonical_project_status_config();
    ProjectStatusConfig {
        active_states: parse_list_env("KAIRASTRA_ACTIVE_STATES")
            .unwrap_or_else(|| defaults.active_states.clone()),
        terminal_states: parse_list_env("KAIRASTRA_TERMINAL_STATES")
            .unwrap_or_else(|| defaults.terminal_states.clone()),
        claimable_states: parse_list_env("KAIRASTRA_CLAIMABLE_STATES")
            .unwrap_or_else(|| defaults.claimable_states.clone()),
        in_progress_state: parse_optional_env("KAIRASTRA_IN_PROGRESS_STATE")
            .unwrap_or_else(|| defaults.in_progress_state.clone()),
        human_review_state: parse_optional_env("KAIRASTRA_HUMAN_REVIEW_STATE")
            .unwrap_or_else(|| defaults.human_review_state.clone()),
        done_state: parse_optional_env("KAIRASTRA_DONE_STATE")
            .unwrap_or_else(|| defaults.done_state.clone()),
    }
}

fn confirm_project_normalization(
    theme: &ColorfulTheme,
    project_owner: &str,
    project_number: &str,
) -> Result<()> {
    let token = status_normalization_confirmation_token(project_owner, project_number);
    println!();
    println!("Normalize GitHub Project Status field?");
    println!(
        "This will update the Status field on GitHub Project {}#{} to Kairastra's default options.",
        project_owner, project_number
    );
    println!(
        "Status options that are not in the target set will be removed from the field definition."
    );
    println!("Kairastra cannot undo this change.");
    let confirmation = ask_string(
        theme,
        &format!("To continue, type: {token}"),
        String::new(),
        false,
        true,
    )?;
    if confirmation != token {
        return Err(anyhow!(
            "project status normalization confirmation did not match"
        ));
    }
    Ok(())
}

fn collect_project_status_config(
    theme: &ColorfulTheme,
    non_interactive: bool,
    overview: Option<&ProjectStatusOverview>,
    github_owner: &str,
    github_project_owner: &str,
    github_project_number: &str,
) -> Result<(ProjectStatusConfig, bool)> {
    if non_interactive {
        return Ok((collect_non_interactive_project_status_config(), false));
    }

    if let Some(overview) = overview {
        let block_reason = project_status_overview_block_reason(overview);
        let normalize_label = if let Some(reason) = block_reason.as_ref() {
            format!("Normalize Project to Kairastra statuses (unavailable: {reason})")
        } else {
            "Normalize Project to Kairastra statuses".to_string()
        };
        print_project_status_help(ProjectStatusHelpTopic::Handling);
        let choice = Select::with_theme(theme)
            .with_prompt("Project status handling")
            .items(&[
                "Keep existing Project statuses (recommended)".to_string(),
                normalize_label,
            ])
            .default(0)
            .interact()?;
        if choice == 1 {
            if let Some(reason) = block_reason {
                return Err(anyhow!(
                    "{reason}. Kairastra will not rewrite a live Project without an explicit migration feature."
                ));
            }
            let project_owner = if github_project_owner.trim().is_empty() {
                github_owner
            } else {
                github_project_owner
            };
            confirm_project_normalization(theme, project_owner, github_project_number)?;
            return Ok((canonical_project_status_config(), true));
        }
        return Ok((
            collect_existing_project_status_config(theme, overview)?,
            false,
        ));
    }

    Ok((collect_manual_project_status_config(theme, false)?, false))
}

fn write_text_file(
    path: &Path,
    content: &str,
    non_interactive: bool,
    reconfigure: bool,
) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
    }

    let should_write = if reconfigure {
        true
    } else if path.exists() && !non_interactive {
        if is_replaceable_bootstrap_file(path)? {
            true
        } else {
            Confirm::with_theme(&ColorfulTheme::default())
                .with_prompt(format!("Overwrite {}?", path.display()))
                .default(false)
                .interact()?
        }
    } else {
        true
    };

    if should_write {
        fs::write(path, content).with_context(|| format!("failed to write {}", path.display()))?;
    }

    Ok(())
}

fn is_replaceable_bootstrap_file(path: &Path) -> Result<bool> {
    let existing = fs::read_to_string(path)
        .with_context(|| format!("failed to read existing {}", path.display()))?;

    let _ = existing;
    Ok(false)
}

fn render_yaml_list(values: &[String], indent: usize) -> String {
    if values.is_empty() {
        return format!("{}[]\n", " ".repeat(indent));
    }
    let prefix = " ".repeat(indent);
    values
        .iter()
        .map(|value| format!("{prefix}- {}\n", render_yaml_scalar(value)))
        .collect::<String>()
}

fn render_optional_yaml_value(value: Option<&str>) -> String {
    match value {
        Some(value) => render_yaml_scalar(value),
        None => "null".to_string(),
    }
}

fn render_yaml_scalar(value: &str) -> String {
    serde_json::to_string(value).expect("YAML scalar serialization should succeed")
}

fn render_workflow(mode: DeployMode, values: &SetupValues) -> String {
    let _ = mode;
    let canonical_body = canonical_workflow_body();
    let assignee_line = if values.assignee_login.trim().is_empty() {
        String::new()
    } else {
        "  assignee_login: $KAIRASTRA_AGENT_ASSIGNEE\n".to_string()
    };
    let provider_sections = values
        .provider_configs
        .iter()
        .map(providers::render_workflow_provider_section)
        .collect::<Vec<_>>()
        .join("\n");
    let mut support_dirs = Vec::new();
    for config in &values.provider_configs {
        for dir in providers::repo_support_dirs(providers::setup_provider_id(config))
            .unwrap_or(&[".github"])
        {
            if !support_dirs.iter().any(|existing| existing == dir) {
                support_dirs.push(*dir);
            }
        }
    }
    let support_dirs = support_dirs.join(" ");
    let tracker_block = match values.tracker_mode {
        GitHubMode::ProjectsV2 => {
            format!(
                r#"tracker:
  kind: github
  mode: projects_v2
  api_key: $GITHUB_TOKEN
  owner: $KAIRASTRA_GITHUB_OWNER
  repo: $KAIRASTRA_GITHUB_REPO
  project_owner: $KAIRASTRA_GITHUB_PROJECT_OWNER
  project_v2_number: $KAIRASTRA_GITHUB_PROJECT_NUMBER
  project_url: $KAIRASTRA_GITHUB_PROJECT_URL
  status_source:
    type: project_field
    name: Status
  priority_source:
    type: project_field
    name: Priority
  active_states:
{active_states}  terminal_states:
{terminal_states}  claimable_states:
{claimable_states}  in_progress_state: {in_progress_state}
  human_review_state: {human_review_state}
  done_state: {done_state}"#,
                active_states = render_yaml_list(&values.project_status.active_states, 4),
                terminal_states = render_yaml_list(&values.project_status.terminal_states, 4),
                claimable_states = render_yaml_list(&values.project_status.claimable_states, 4),
                in_progress_state =
                    render_optional_yaml_value(values.project_status.in_progress_state.as_deref()),
                human_review_state =
                    render_optional_yaml_value(values.project_status.human_review_state.as_deref()),
                done_state =
                    render_optional_yaml_value(values.project_status.done_state.as_deref()),
            )
        }
        GitHubMode::IssuesOnly => r#"tracker:
  kind: github
  mode: issues_only
  api_key: $GITHUB_TOKEN
  owner: $KAIRASTRA_GITHUB_OWNER
  repo: $KAIRASTRA_GITHUB_REPO
  status_source:
    type: label
  active_states:
    - Todo
    - In Progress
    - Merging
    - Rework
  terminal_states:
    - Closed
    - Cancelled
    - Canceled
    - Duplicate
    - Done
  claimable_states:
    - Todo
  in_progress_state: In Progress
  human_review_state: Human Review
  done_state: Done"#
            .to_string(),
    };

    format!(
        r#"---
{tracker_block}
workspace:
  root: $KAIRASTRA_WORKSPACE_ROOT
hooks:
  after_create: |
    set -euo pipefail

    sanitize_issue_identifier() {{
      printf '%s' "${{ISSUE_IDENTIFIER:-issue}}" | tr -c 'A-Za-z0-9._-' '_'
    }}

    require_seed_repo() {{
      if [ -z "${{KAIRASTRA_SEED_REPO:-}}" ] || [ ! -d "$KAIRASTRA_SEED_REPO/.git" ]; then
        echo "KAIRASTRA_SEED_REPO must point at a git checkout before running Kairastra." >&2
        exit 1
      fi
      if ! git -C "$KAIRASTRA_SEED_REPO" rev-parse --verify HEAD >/dev/null 2>&1; then
        echo "KAIRASTRA_SEED_REPO must have at least one commit before running Kairastra." >&2
        exit 1
      fi
    }}

    ensure_workspace_checkout() {{
      branch_name="${{KAIRASTRA_WORKTREE_BRANCH:-kairastra/$(sanitize_issue_identifier)}}"
      git -C "$KAIRASTRA_SEED_REPO" worktree prune >/dev/null 2>&1 || true
      if git -C "$KAIRASTRA_SEED_REPO" show-ref --verify --quiet "refs/heads/$branch_name"; then
        git -C "$KAIRASTRA_SEED_REPO" worktree add --force "$PWD" "$branch_name"
      else
        git -C "$KAIRASTRA_SEED_REPO" worktree add --force -b "$branch_name" "$PWD" HEAD
      fi
    }}

    github_https_url() {{
      remote_url="$1"
      case "$remote_url" in
        git@github.com:*)
          printf 'https://github.com/%s\n' "${{remote_url#git@github.com:}}"
          ;;
        ssh://git@github.com/*)
          printf 'https://github.com/%s\n' "${{remote_url#ssh://git@github.com/}}"
          ;;
        *)
          printf '%s\n' "$remote_url"
          ;;
      esac
    }}

    configure_github_auth() {{
      if [ -z "${{GITHUB_TOKEN:-}}" ]; then
        return 0
      fi

      origin_url="$(git config --get remote.origin.url || true)"
      normalized_origin_url="$(github_https_url "$origin_url")"
      if [ -n "$normalized_origin_url" ] && [ "$normalized_origin_url" != "$origin_url" ]; then
        git remote set-url origin "$normalized_origin_url"
      fi

      push_url="$(git config --get remote.origin.pushurl || true)"
      normalized_push_url="$(github_https_url "$push_url")"
      if [ -n "$normalized_push_url" ] && [ "$normalized_push_url" != "$push_url" ]; then
        git remote set-url --push origin "$normalized_push_url"
      fi

      auth_header="$(printf 'x-access-token:%s' "$GITHUB_TOKEN" | base64 | tr -d '\n')"
      git config http.https://github.com/.extraheader "Authorization: Basic ${{auth_header}}"
    }}

    configure_origin_from_seed() {{
      source_remote="$(git -C "$KAIRASTRA_SEED_REPO" config --get remote.origin.url || true)"
      if [ -z "$source_remote" ]; then
        echo "KAIRASTRA_SEED_REPO must define remote.origin.url before running Kairastra." >&2
        exit 1
      fi
      git remote set-url origin "$source_remote"

      source_push="$(git -C "$KAIRASTRA_SEED_REPO" config --get remote.origin.pushurl || true)"
      if [ -n "${{KAIRASTRA_GIT_PUSH_URL:-}}" ]; then
        git remote set-url --push origin "$KAIRASTRA_GIT_PUSH_URL"
      elif [ -n "$source_push" ]; then
        git remote set-url --push origin "$source_push"
      fi
    }}

    restore_support_dir_from_seed() {{
      support_dir="$1"
      if [ -e "$support_dir" ]; then
        return 0
      fi
      if [ -n "${{KAIRASTRA_SEED_REPO:-}}" ] && [ -e "${{KAIRASTRA_SEED_REPO}}/$support_dir" ]; then
        cp -R "${{KAIRASTRA_SEED_REPO}}/$support_dir" "$support_dir"
      fi
    }}

    exclude_workspace_support_dir() {{
      support_dir="$1"
      exclude_path="$(git rev-parse --git-path info/exclude 2>/dev/null || true)"
      if [ -z "$exclude_path" ]; then
        return 0
      fi
      mkdir -p "$(dirname "$exclude_path")"
      touch "$exclude_path"
      entry="$support_dir/"
      if ! grep -Fqx "$entry" "$exclude_path" 2>/dev/null; then
        printf '%s\n' "$entry" >> "$exclude_path"
      fi
    }}

    remove_legacy_codex_workspace_support() {{
      if [ ! -e ".codex" ]; then
        return 0
      fi
      if git ls-files -- .codex 2>/dev/null | grep -q .; then
        return 0
      fi
      rm -rf .codex
    }}

    require_workspace_support_dirs() {{
      for support_dir in {support_dirs}; do
        restore_support_dir_from_seed "$support_dir"
        if [ ! -e "$support_dir" ]; then
          echo "Workspace bootstrap missing required repository support directory: $support_dir" >&2
          exit 1
        fi
        exclude_workspace_support_dir "$support_dir"
      done
    }}

    resolve_default_branch() {{
      if [ -n "${{KAIRASTRA_GIT_DEFAULT_BRANCH:-}}" ]; then
        printf '%s\n' "${{KAIRASTRA_GIT_DEFAULT_BRANCH}}"
        return 0
      fi

      remote_head="$(git symbolic-ref --quiet --short refs/remotes/origin/HEAD 2>/dev/null || true)"
      if [ -n "$remote_head" ]; then
        printf '%s\n' "${{remote_head#origin/}}"
        return 0
      fi

      remote_head="$(git remote show origin 2>/dev/null | sed -n 's/.*HEAD branch: //p' | head -n 1)"
      if [ -n "$remote_head" ]; then
        printf '%s\n' "$remote_head"
        return 0
      fi

      seed_branch="$(git -C "$KAIRASTRA_SEED_REPO" branch --show-current 2>/dev/null || true)"
      if [ -n "$seed_branch" ]; then
        printf '%s\n' "$seed_branch"
        return 0
      fi

      printf 'HEAD\n'
    }}

    fetch_origin_branch() {{
      branch_name="$1"
      if [ -z "$branch_name" ] || [ "$branch_name" = "HEAD" ]; then
        return 0
      fi
      git fetch --quiet origin "refs/heads/$branch_name:refs/remotes/origin/$branch_name" || true
    }}

    ensure_default_branch_baseline() {{
      current_branch="$(git rev-parse --abbrev-ref HEAD 2>/dev/null || true)"
      default_branch="$(resolve_default_branch)"
      if [ -z "$default_branch" ]; then
        return 0
      fi

      fetch_origin_branch "$default_branch"
      if [ -n "$current_branch" ] && [ "$current_branch" != "$default_branch" ]; then
        fetch_origin_branch "$current_branch"
      fi

      is_shallow="$(git rev-parse --is-shallow-repository 2>/dev/null || printf 'false\n')"
      if [ "$is_shallow" = "true" ]; then
        if [ -n "$current_branch" ] && [ "$current_branch" != "$default_branch" ] && [ "$current_branch" != "HEAD" ]; then
          git fetch --quiet --unshallow origin \
            "refs/heads/$default_branch:refs/remotes/origin/$default_branch" \
            "refs/heads/$current_branch:refs/remotes/origin/$current_branch" \
            || true
        else
          git fetch --quiet --unshallow origin \
            "refs/heads/$default_branch:refs/remotes/origin/$default_branch" \
            || true
        fi
      fi

      if git merge-base "origin/$default_branch" HEAD >/dev/null 2>&1; then
        return 0
      fi

      if [ -n "$current_branch" ] && [ "$current_branch" != "HEAD" ]; then
        git fetch --quiet origin \
          "refs/heads/$current_branch:refs/remotes/origin/$current_branch" \
          "refs/heads/$default_branch:refs/remotes/origin/$default_branch" \
          || true
      else
        git fetch --quiet origin "refs/heads/$default_branch:refs/remotes/origin/$default_branch" || true
      fi
    }}

    require_seed_repo
    ensure_workspace_checkout
    remove_legacy_codex_workspace_support
    require_workspace_support_dirs
    configure_origin_from_seed
    configure_github_auth
    ensure_default_branch_baseline

    git config user.name "${{KAIRASTRA_GIT_AUTHOR_NAME:-Kairastra}}"
    git config user.email "${{KAIRASTRA_GIT_AUTHOR_EMAIL:-kairastra@users.noreply.github.com}}"
  before_run: |
    set -euo pipefail

    git config --global --add safe.directory "$(pwd)"

    require_seed_repo() {{
      if [ -z "${{KAIRASTRA_SEED_REPO:-}}" ] || [ ! -d "$KAIRASTRA_SEED_REPO/.git" ]; then
        echo "KAIRASTRA_SEED_REPO must point at a git checkout before running Kairastra." >&2
        exit 1
      fi
      if ! git -C "$KAIRASTRA_SEED_REPO" rev-parse --verify HEAD >/dev/null 2>&1; then
        echo "KAIRASTRA_SEED_REPO must have at least one commit before running Kairastra." >&2
        exit 1
      fi
    }}

    restore_support_dir_from_seed() {{
      support_dir="$1"
      if [ -e "$support_dir" ]; then
        return 0
      fi
      if [ -n "${{KAIRASTRA_SEED_REPO:-}}" ] && [ -e "${{KAIRASTRA_SEED_REPO}}/$support_dir" ]; then
        cp -R "${{KAIRASTRA_SEED_REPO}}/$support_dir" "$support_dir"
      fi
    }}

    exclude_workspace_support_dir() {{
      support_dir="$1"
      exclude_path="$(git rev-parse --git-path info/exclude 2>/dev/null || true)"
      if [ -z "$exclude_path" ]; then
        return 0
      fi
      mkdir -p "$(dirname "$exclude_path")"
      touch "$exclude_path"
      entry="$support_dir/"
      if ! grep -Fqx "$entry" "$exclude_path" 2>/dev/null; then
        printf '%s\n' "$entry" >> "$exclude_path"
      fi
    }}

    remove_legacy_codex_workspace_support() {{
      if [ ! -e ".codex" ]; then
        return 0
      fi
      if git ls-files -- .codex 2>/dev/null | grep -q .; then
        return 0
      fi
      rm -rf .codex
    }}

    require_workspace_support_dirs() {{
      for support_dir in {support_dirs}; do
        restore_support_dir_from_seed "$support_dir"
        if [ ! -e "$support_dir" ]; then
          echo "Workspace bootstrap missing required repository support directory: $support_dir" >&2
          exit 1
        fi
        exclude_workspace_support_dir "$support_dir"
      done
    }}

    github_https_url() {{
      remote_url="$1"
      case "$remote_url" in
        git@github.com:*)
          printf 'https://github.com/%s\n' "${{remote_url#git@github.com:}}"
          ;;
        ssh://git@github.com/*)
          printf 'https://github.com/%s\n' "${{remote_url#ssh://git@github.com/}}"
          ;;
        *)
          printf '%s\n' "$remote_url"
          ;;
      esac
    }}

    configure_github_auth() {{
      if [ -z "${{GITHUB_TOKEN:-}}" ]; then
        return 0
      fi

      origin_url="$(git config --get remote.origin.url || true)"
      normalized_origin_url="$(github_https_url "$origin_url")"
      if [ -n "$normalized_origin_url" ] && [ "$normalized_origin_url" != "$origin_url" ]; then
        git remote set-url origin "$normalized_origin_url"
      fi

      push_url="$(git config --get remote.origin.pushurl || true)"
      normalized_push_url="$(github_https_url "$push_url")"
      if [ -n "$normalized_push_url" ] && [ "$normalized_push_url" != "$push_url" ]; then
        git remote set-url --push origin "$normalized_push_url"
      fi

      auth_header="$(printf 'x-access-token:%s' "$GITHUB_TOKEN" | base64 | tr -d '\n')"
      git config http.https://github.com/.extraheader "Authorization: Basic ${{auth_header}}"
    }}

    configure_origin_from_seed() {{
      source_remote="$(git -C "$KAIRASTRA_SEED_REPO" config --get remote.origin.url || true)"
      if [ -z "$source_remote" ]; then
        echo "KAIRASTRA_SEED_REPO must define remote.origin.url before running Kairastra." >&2
        exit 1
      fi
      git remote set-url origin "$source_remote"

      source_push="$(git -C "$KAIRASTRA_SEED_REPO" config --get remote.origin.pushurl || true)"
      if [ -n "${{KAIRASTRA_GIT_PUSH_URL:-}}" ]; then
        git remote set-url --push origin "$KAIRASTRA_GIT_PUSH_URL"
      elif [ -n "$source_push" ]; then
        git remote set-url --push origin "$source_push"
      fi
    }}

    resolve_default_branch() {{
      if [ -n "${{KAIRASTRA_GIT_DEFAULT_BRANCH:-}}" ]; then
        printf '%s\n' "${{KAIRASTRA_GIT_DEFAULT_BRANCH}}"
        return 0
      fi

      remote_head="$(git symbolic-ref --quiet --short refs/remotes/origin/HEAD 2>/dev/null || true)"
      if [ -n "$remote_head" ]; then
        printf '%s\n' "${{remote_head#origin/}}"
        return 0
      fi

      remote_head="$(git remote show origin 2>/dev/null | sed -n 's/.*HEAD branch: //p' | head -n 1)"
      if [ -n "$remote_head" ]; then
        printf '%s\n' "$remote_head"
        return 0
      fi

      seed_branch="$(git -C "$KAIRASTRA_SEED_REPO" branch --show-current 2>/dev/null || true)"
      if [ -n "$seed_branch" ]; then
        printf '%s\n' "$seed_branch"
        return 0
      fi

      printf 'HEAD\n'
    }}

    fetch_origin_branch() {{
      branch_name="$1"
      if [ -z "$branch_name" ] || [ "$branch_name" = "HEAD" ]; then
        return 0
      fi
      git fetch --quiet origin "refs/heads/$branch_name:refs/remotes/origin/$branch_name" || true
    }}

    ensure_default_branch_baseline() {{
      current_branch="$(git rev-parse --abbrev-ref HEAD 2>/dev/null || true)"
      default_branch="$(resolve_default_branch)"
      if [ -z "$default_branch" ]; then
        return 0
      fi

      fetch_origin_branch "$default_branch"
      if [ -n "$current_branch" ] && [ "$current_branch" != "$default_branch" ]; then
        fetch_origin_branch "$current_branch"
      fi

      is_shallow="$(git rev-parse --is-shallow-repository 2>/dev/null || printf 'false\n')"
      if [ "$is_shallow" = "true" ]; then
        if [ -n "$current_branch" ] && [ "$current_branch" != "$default_branch" ] && [ "$current_branch" != "HEAD" ]; then
          git fetch --quiet --unshallow origin \
            "refs/heads/$default_branch:refs/remotes/origin/$default_branch" \
            "refs/heads/$current_branch:refs/remotes/origin/$current_branch" \
            || true
        else
          git fetch --quiet --unshallow origin \
            "refs/heads/$default_branch:refs/remotes/origin/$default_branch" \
            || true
        fi
      fi

      if git merge-base "origin/$default_branch" HEAD >/dev/null 2>&1; then
        return 0
      fi

      if [ -n "$current_branch" ] && [ "$current_branch" != "HEAD" ]; then
        git fetch --quiet origin \
          "refs/heads/$current_branch:refs/remotes/origin/$current_branch" \
          "refs/heads/$default_branch:refs/remotes/origin/$default_branch" \
          || true
      else
        git fetch --quiet origin "refs/heads/$default_branch:refs/remotes/origin/$default_branch" || true
      fi
    }}

    require_seed_repo
    remove_legacy_codex_workspace_support
    require_workspace_support_dirs
    configure_origin_from_seed
    configure_github_auth
    ensure_default_branch_baseline

    git config user.name "${{KAIRASTRA_GIT_AUTHOR_NAME:-Kairastra}}"
    git config user.email "${{KAIRASTRA_GIT_AUTHOR_EMAIL:-kairastra@users.noreply.github.com}}"
agent:
  provider: {provider}
  max_concurrent_agents: {max_concurrent_agents}
  max_turns: {max_turns}
{assignee_line}providers:
{provider_sections}
---
{canonical_body}"#,
        tracker_block = tracker_block,
        provider = values.provider,
        max_concurrent_agents = values.max_concurrent_agents,
        max_turns = values.max_turns,
        assignee_line = assignee_line,
        provider_sections = provider_sections,
        support_dirs = support_dirs,
        canonical_body = canonical_body
    )
}

fn canonical_workflow_body() -> &'static str {
    extract_workflow_body(CANONICAL_WORKFLOW_TEMPLATE).expect(
        "repo-root WORKFLOW.md must contain front matter followed by a canonical workflow body",
    )
}

fn extract_workflow_body(source: &str) -> Option<&str> {
    let rest = source.strip_prefix("---\n")?;
    let (_, body) = rest.split_once("\n---\n")?;
    Some(body)
}

fn render_project_env_lines(values: &SetupValues) -> String {
    if values.tracker_mode != GitHubMode::ProjectsV2 {
        return String::new();
    }

    format!(
        "KAIRASTRA_GITHUB_PROJECT_OWNER={}\nKAIRASTRA_GITHUB_PROJECT_NUMBER={}\nKAIRASTRA_GITHUB_PROJECT_URL={}\n",
        values.github_project_owner, values.github_project_number, values.github_project_url
    )
}

fn render_env_file(mode: DeployMode, values: &SetupValues, workflow_path: &Path) -> String {
    let _ = mode;
    let project_env_lines = render_project_env_lines(values);
    let provider_env = values
        .provider_configs
        .iter()
        .map(|config| providers::render_env_provider_section(mode, config))
        .filter(|section| !section.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        r#"# Generated by `kairastra setup`
KAIRASTRA_DEPLOY_MODE=native
GITHUB_TOKEN={github_token}
ANTHROPIC_API_KEY={anthropic_api_key}
GEMINI_API_KEY={gemini_api_key}
OPENAI_API_KEY={openai_api_key}
WORKFLOW_PATH={workflow_path}
KAIRASTRA_WORKSPACE_ROOT={workspace_root}
KAIRASTRA_GITHUB_OWNER={github_owner}
KAIRASTRA_GITHUB_REPO={github_repo}
{project_env_lines}KAIRASTRA_SEED_REPO={seed_repo}
KAIRASTRA_AGENT_ASSIGNEE={assignee_login}
{provider_env}
RUST_LOG={rust_log}
"#,
        github_token = values.github_token,
        anthropic_api_key = values.anthropic_api_key,
        gemini_api_key = values.gemini_api_key,
        openai_api_key = values.openai_api_key,
        workflow_path = absolute_display_path(workflow_path),
        workspace_root = values.workspace_root,
        github_owner = values.github_owner,
        github_repo = values.github_repo,
        project_env_lines = project_env_lines,
        seed_repo = values.seed_repo,
        assignee_login = values.assignee_login,
        provider_env = provider_env,
        rust_log = values.rust_log,
    )
}

fn render_systemd_unit(values: &SetupValues, workflow_path: &Path, env_file: &Path) -> String {
    format!(
        r#"[Unit]
Description=Kairastra Rust orchestrator
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
EnvironmentFile={}
WorkingDirectory={}
ExecStart={} run {}
Restart=on-failure
RestartSec=5

[Install]
WantedBy=multi-user.target
"#,
        absolute_display_path(env_file),
        std::env::current_dir()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|_| ".".to_string()),
        values.binary_path,
        absolute_display_path(workflow_path)
    )
}

fn absolute_display_path(path: &Path) -> String {
    fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .display()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::{
        canonical_workflow_body, ensure_repo_support_dirs, render_env_file, render_systemd_unit,
        render_workflow, SetupValues,
    };
    use crate::config::GitHubMode;
    use crate::deploy::DeployMode;
    use crate::providers::claude::setup::ClaudeSetupConfig;
    use crate::providers::codex::setup::CodexSetupConfig;
    use crate::providers::gemini::setup::GeminiSetupConfig;
    use crate::providers::ProviderSetupConfig;
    use std::fs;
    use std::path::Path;
    use std::process::Command;
    use tempfile::tempdir;

    fn sample_values() -> SetupValues {
        SetupValues {
            tracker_mode: GitHubMode::ProjectsV2,
            project_status: super::canonical_project_status_config(),
            normalize_project_statuses: false,
            provider: "codex".to_string(),
            provider_configs: vec![
                ProviderSetupConfig::Codex(CodexSetupConfig {
                    auth_mode: crate::auth::AuthMode::Subscription,
                    model: "gpt-5.4".to_string(),
                    reasoning_effort: "high".to_string(),
                    fast: Some(true),
                }),
                ProviderSetupConfig::Claude(ClaudeSetupConfig {
                    auth_mode: crate::auth::AuthMode::ApiKey,
                    model: "sonnet".to_string(),
                    reasoning_effort: "high".to_string(),
                }),
                ProviderSetupConfig::Gemini(GeminiSetupConfig {
                    auth_mode: crate::auth::AuthMode::Subscription,
                    model: "gemini-2.5-pro".to_string(),
                    approval_mode: "yolo".to_string(),
                }),
            ],
            github_owner: "example-owner".to_string(),
            github_repo: "example-repo".to_string(),
            github_project_owner: "example-project-owner".to_string(),
            github_project_number: "7".to_string(),
            github_project_url: "https://github.com/users/example-project-owner/projects/7"
                .to_string(),
            workspace_root: "/var/lib/kairastra/workspaces".to_string(),
            seed_repo: "/tmp/kairastra-seed".to_string(),
            assignee_login: "codex-bot".to_string(),
            max_concurrent_agents: "4".to_string(),
            max_turns: "20".to_string(),
            github_token: String::new(),
            anthropic_api_key: String::new(),
            gemini_api_key: String::new(),
            openai_api_key: String::new(),
            rust_log: "info".to_string(),
            binary_path: "/usr/local/bin/kairastra".to_string(),
        }
    }

    fn init_git_repo(path: &Path) {
        let status = Command::new("git")
            .args(["init", "-q"])
            .current_dir(path)
            .status()
            .unwrap();
        assert!(status.success());
    }

    fn normalized_path(path: &Path) -> std::path::PathBuf {
        fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
    }

    #[test]
    fn workflow_template_uses_env_placeholders() {
        let rendered = render_workflow(DeployMode::Native, &sample_values());
        assert!(rendered.contains("owner: $KAIRASTRA_GITHUB_OWNER"));
        assert!(rendered.contains("project_owner: $KAIRASTRA_GITHUB_PROJECT_OWNER"));
        assert!(rendered.contains("provider: codex"));
        assert!(rendered.contains("assignee_login: $KAIRASTRA_AGENT_ASSIGNEE"));
        assert!(rendered.contains("providers:"));
        assert!(rendered.contains("  codex:"));
        assert!(rendered.contains("  claude:"));
        assert!(rendered.contains("  gemini:"));
        assert!(rendered.contains("model: $KAIRASTRA_CODEX_MODEL"));
        assert!(rendered.contains("model: $KAIRASTRA_CLAUDE_MODEL"));
        assert!(rendered.contains("model: $KAIRASTRA_GEMINI_MODEL"));
        assert!(rendered.contains("reasoning_effort: $KAIRASTRA_CODEX_REASONING_EFFORT"));
        assert!(rendered.contains("reasoning_effort: $KAIRASTRA_CLAUDE_REASONING_EFFORT"));
        assert!(rendered.contains("approval_mode: $KAIRASTRA_GEMINI_APPROVAL_MODE"));
        assert!(rendered.contains("fast: $KAIRASTRA_CODEX_FAST"));
        assert!(rendered.contains("for support_dir in .agents .github; do"));
        assert!(!rendered.contains("for support_dir in .codex .github; do"));
        assert!(rendered.contains("git rev-parse --git-path info/exclude"));
        assert!(rendered.contains("entry=\"$support_dir/\""));
        assert!(rendered.contains("remove_legacy_codex_workspace_support"));
        assert!(rendered.contains("git ls-files -- .codex"));
        assert!(
            rendered.contains("Workspace bootstrap missing required repository support directory")
        );
        assert!(rendered.contains("git -C \"$KAIRASTRA_SEED_REPO\" worktree add --force"));
        assert!(rendered.contains("kairastra/$(sanitize_issue_identifier)"));
        assert!(rendered.contains("git -C \"$KAIRASTRA_SEED_REPO\" config --get remote.origin.url"));
        assert!(rendered.contains("git remote set-url --push origin \"$KAIRASTRA_GIT_PUSH_URL\""));
        assert!(rendered.contains("git config --get remote.origin.pushurl || true"));
        assert!(rendered.contains("http.https://github.com/.extraheader"));
        assert!(rendered.contains("resolve_default_branch()"));
        assert!(rendered.contains("fetch_origin_branch()"));
        assert!(rendered.contains("fetch_origin_branch \"$default_branch\""));
        assert!(rendered.contains("fetch_origin_branch \"$current_branch\""));
        assert!(rendered.contains("ensure_default_branch_baseline()"));
        assert!(rendered.contains("git fetch --quiet --unshallow origin \\"));
        assert!(rendered.contains("before_run: |"));
        assert!(rendered.contains(r#"  in_progress_state: "In Progress""#));
        assert!(rendered.contains(r#"  human_review_state: "Human Review""#));
        assert!(rendered.contains(r#"  done_state: "Done""#));
        assert!(rendered.contains("## Default posture"));
        assert!(rendered.contains("## Status map"));
        assert!(rendered.contains("## Step 0: Determine current issue state and route"));
        assert!(rendered.contains("## Workpad template"));
        assert!(rendered.contains(".agents/skills/kairastra-push/SKILL.md"));
        assert!(rendered.contains(".agents/skills/kairastra-land/SKILL.md"));
    }

    #[test]
    fn workflow_uses_selected_provider_but_keeps_all_provider_blocks() {
        let mut values = sample_values();
        values.provider = "claude".to_string();

        let rendered = render_workflow(DeployMode::Native, &values);
        assert!(rendered.contains("provider: claude"));
        assert!(rendered.contains("for support_dir in .agents .github; do"));
        assert!(rendered.contains("  codex:"));
        assert!(rendered.contains("  claude:"));
        assert!(rendered.contains("  gemini:"));
        assert!(rendered.contains("model: $KAIRASTRA_CLAUDE_MODEL"));
        assert!(rendered.contains("reasoning_effort: $KAIRASTRA_CLAUDE_REASONING_EFFORT"));
    }

    #[test]
    fn native_env_contains_provider_and_project_settings() {
        let rendered = render_env_file(
            DeployMode::Native,
            &sample_values(),
            Path::new("WORKFLOW.md"),
        );
        assert!(rendered.contains("KAIRASTRA_GITHUB_PROJECT_OWNER=example-project-owner"));
        assert!(rendered.contains("KAIRASTRA_SEED_REPO=/tmp/kairastra-seed"));
        assert!(!rendered.contains("KAIRASTRA_GIT_CLONE_URL="));
        assert!(rendered.contains("CODEX_AUTH_MODE=subscription"));
        assert!(rendered.contains("KAIRASTRA_CODEX_MODEL=gpt-5.4"));
        assert!(rendered.contains("KAIRASTRA_CODEX_REASONING_EFFORT=high"));
        assert!(rendered.contains("KAIRASTRA_CODEX_FAST=true"));
        assert!(rendered.contains("CLAUDE_AUTH_MODE=api_key"));
        assert!(rendered.contains("KAIRASTRA_CLAUDE_MODEL=sonnet"));
        assert!(rendered.contains("KAIRASTRA_CLAUDE_REASONING_EFFORT=high"));
        assert!(rendered.contains("GEMINI_AUTH_MODE=subscription"));
        assert!(rendered.contains("KAIRASTRA_GEMINI_MODEL=gemini-2.5-pro"));
        assert!(rendered.contains("KAIRASTRA_GEMINI_APPROVAL_MODE=yolo"));
    }

    #[test]
    fn systemd_unit_runs_run_subcommand() {
        let rendered = render_systemd_unit(
            &sample_values(),
            Path::new("WORKFLOW.md"),
            Path::new("kairastra.env"),
        );
        assert!(rendered.contains("ExecStart=/usr/local/bin/kairastra run"));
    }

    #[test]
    fn parses_project_url_for_owner_and_number() {
        let parsed =
            super::parse_project_url("https://github.com/users/openai/projects/7").unwrap();
        assert_eq!(parsed.owner, "openai");
        assert_eq!(parsed.project_number, "7");
        assert_eq!(parsed.owner_kind, super::ProjectOwnerKind::User);
    }

    #[test]
    fn parses_org_project_url_with_extra_path() {
        let parsed =
            super::parse_project_url("https://github.com/orgs/acme/projects/12/views/1").unwrap();
        assert_eq!(parsed.owner, "acme");
        assert_eq!(parsed.project_number, "12");
        assert_eq!(parsed.owner_kind, super::ProjectOwnerKind::Organization);
    }

    #[test]
    fn rejects_unrecognized_project_url() {
        assert!(super::parse_project_url("https://github.com/openai/kairastra").is_none());
    }

    #[test]
    fn parses_repo_input_from_url() {
        let parsed = super::parse_repo_input("https://github.com/openai/kairastra").unwrap();
        assert_eq!(parsed.owner.as_deref(), Some("openai"));
        assert_eq!(parsed.repo, "kairastra");
    }

    #[test]
    fn parses_repo_input_from_owner_repo_shorthand() {
        let parsed = super::parse_repo_input("openai/kairastra").unwrap();
        assert_eq!(parsed.owner.as_deref(), Some("openai"));
        assert_eq!(parsed.repo, "kairastra");
    }

    #[test]
    fn parses_repo_input_from_repo_name_only() {
        let parsed = super::parse_repo_input("kairastra").unwrap();
        assert_eq!(parsed.owner, None);
        assert_eq!(parsed.repo, "kairastra");
    }

    #[test]
    fn detects_rust_subdir_layout() {
        let dir = tempdir().unwrap();
        init_git_repo(dir.path());
        let rust_dir = dir.path().join("rust");
        fs::create_dir_all(&rust_dir).unwrap();
        fs::write(rust_dir.join("Cargo.toml"), "").unwrap();

        let from_root = super::detect_layout(dir.path()).unwrap();
        assert_eq!(from_root.repo_root, normalized_path(dir.path()));
        assert_eq!(from_root.rust_dir, normalized_path(&rust_dir));

        let from_rust = super::detect_layout(&rust_dir).unwrap();
        assert_eq!(from_rust.repo_root, normalized_path(dir.path()));
        assert_eq!(from_rust.rust_dir, normalized_path(&rust_dir));
    }

    #[test]
    fn workflow_path_defaults_to_repo_root_workflow() {
        let dir = tempdir().unwrap();
        init_git_repo(dir.path());

        let layout = super::detect_layout(dir.path()).unwrap();
        let path = super::resolve_workflow_path(&layout, None);

        assert_eq!(path, normalized_path(dir.path()).join("WORKFLOW.md"));
    }

    #[test]
    fn env_file_path_defaults_to_dot_kairastra_env() {
        let dir = tempdir().unwrap();
        init_git_repo(dir.path());

        let layout = super::detect_layout(dir.path()).unwrap();
        let path = super::resolve_env_file_path(&layout, DeployMode::Native, None);

        assert_eq!(
            path,
            normalized_path(dir.path()).join(".kairastra/kairastra.env")
        );
    }

    #[test]
    fn detect_layout_requires_git_repo() {
        let dir = tempdir().unwrap();
        let error = super::detect_layout(dir.path()).unwrap_err().to_string();
        assert!(error.contains("must be run inside a Git repository"));
    }

    #[test]
    fn scaffolds_missing_repo_support_dirs() {
        let dir = tempdir().unwrap();
        init_git_repo(dir.path());

        let scaffolded = ensure_repo_support_dirs(dir.path(), "codex").unwrap();
        assert!(scaffolded.contains(&dir.path().join(".agents")));
        assert!(scaffolded.contains(&dir.path().join(".github")));
        assert!(scaffolded.contains(&dir.path().join(".github/pull_request_template.md")));
        assert!(dir.path().join(".agents").is_dir());
        assert!(dir.path().join(".agents/.gitkeep").is_file());
        assert!(dir.path().join(".github").is_dir());
        assert!(dir.path().join(".github/.gitkeep").is_file());
        assert!(dir
            .path()
            .join(".github/pull_request_template.md")
            .is_file());
    }

    #[test]
    fn preserves_existing_support_dirs_and_adds_missing_pr_template() {
        let dir = tempdir().unwrap();
        init_git_repo(dir.path());
        fs::create_dir_all(dir.path().join(".agents")).unwrap();
        fs::write(dir.path().join(".agents/existing.txt"), "agents\n").unwrap();
        fs::create_dir_all(dir.path().join(".github")).unwrap();
        fs::write(dir.path().join(".github/existing.yml"), "name: existing\n").unwrap();

        let scaffolded = ensure_repo_support_dirs(dir.path(), "codex").unwrap();
        assert_eq!(
            scaffolded,
            vec![dir.path().join(".github/pull_request_template.md")]
        );
        assert!(dir.path().join(".agents/existing.txt").is_file());
        assert!(dir.path().join(".github/existing.yml").is_file());
        assert!(dir
            .path()
            .join(".github/pull_request_template.md")
            .is_file());
        assert!(!dir.path().join(".agents/.gitkeep").exists());
        assert!(!dir.path().join(".github/.gitkeep").exists());
    }

    #[test]
    fn does_not_overwrite_existing_pr_template() {
        let dir = tempdir().unwrap();
        init_git_repo(dir.path());
        fs::create_dir_all(dir.path().join(".github")).unwrap();
        fs::write(
            dir.path().join(".github/pull_request_template.md"),
            "custom template\n",
        )
        .unwrap();

        let scaffolded = ensure_repo_support_dirs(dir.path(), "codex").unwrap();
        assert!(scaffolded.contains(&dir.path().join(".agents")));
        assert!(!scaffolded.contains(&dir.path().join(".github/pull_request_template.md")));
        assert_eq!(
            fs::read_to_string(dir.path().join(".github/pull_request_template.md")).unwrap(),
            "custom template\n"
        );
    }

    #[test]
    fn adds_dot_kairastra_to_gitignore() {
        let dir = tempdir().unwrap();
        init_git_repo(dir.path());

        let added = super::ensure_local_ignore_rule(dir.path(), ".kairastra/").unwrap();
        assert!(added);

        let gitignore = fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert!(gitignore.contains(".kairastra/"));
    }

    #[test]
    fn creates_gitignore_when_missing() {
        let dir = tempdir().unwrap();
        init_git_repo(dir.path());

        assert!(!dir.path().join(".gitignore").exists());
        let added = super::ensure_local_ignore_rule(dir.path(), ".kairastra/").unwrap();

        assert!(added);
        assert!(dir.path().join(".gitignore").is_file());
        let gitignore = fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert_eq!(gitignore, ".kairastra/\n");
    }

    #[test]
    fn skips_ignore_update_when_gitignore_already_has_entry() {
        let dir = tempdir().unwrap();
        init_git_repo(dir.path());
        fs::write(dir.path().join(".gitignore"), ".kairastra/\n").unwrap();

        let added = super::ensure_local_ignore_rule(dir.path(), ".kairastra/").unwrap();
        assert!(!added);
    }

    #[test]
    fn guard_refuses_default_workflow_in_source_repo() {
        let source_root = super::canonical_source_repo_root().unwrap();
        let layout = super::SetupLayout {
            repo_root: source_root.clone(),
            rust_dir: source_root.join("rust"),
        };

        let error =
            super::guard_default_workflow_target(&layout, None, &source_root.join("WORKFLOW.md"))
                .unwrap_err()
                .to_string();

        assert!(error.contains("refusing to overwrite"));
    }

    #[test]
    fn issues_only_workflow_uses_repo_first_defaults() {
        let mut values = sample_values();
        values.tracker_mode = GitHubMode::IssuesOnly;

        let rendered = render_workflow(DeployMode::Native, &values);
        assert!(rendered.contains("mode: issues_only"));
        assert!(rendered.contains("type: label"));
        assert!(rendered.contains("- Todo"));
        assert!(rendered.contains(r#"  human_review_state: Human Review"#));
        assert!(!rendered.contains("project_v2_number"));
        assert!(!rendered.contains("project_owner"));
        assert!(!rendered.contains("type: project_field"));
    }

    #[test]
    fn issues_only_bootstrap_statuses_match_issue_state_workflow() {
        let mut values = sample_values();
        values.tracker_mode = GitHubMode::IssuesOnly;

        let names = super::desired_status_options(&super::effective_status_config(&values));

        assert_eq!(
            names,
            vec![
                "Todo".to_string(),
                "In Progress".to_string(),
                "Merging".to_string(),
                "Rework".to_string(),
                "Closed".to_string(),
                "Cancelled".to_string(),
                "Duplicate".to_string(),
                "Done".to_string(),
                "Human Review".to_string(),
            ]
        );
    }

    #[test]
    fn projects_v2_bootstrap_statuses_use_selected_project_states() {
        let values = sample_values();

        let names = super::desired_status_options(&super::effective_status_config(&values));

        assert_eq!(
            names,
            vec![
                "Todo".to_string(),
                "In Progress".to_string(),
                "Merging".to_string(),
                "Rework".to_string(),
                "Closed".to_string(),
                "Cancelled".to_string(),
                "Duplicate".to_string(),
                "Done".to_string(),
                "Human Review".to_string(),
            ]
        );
    }

    #[test]
    fn issues_only_env_omits_project_specific_values() {
        let mut values = sample_values();
        values.tracker_mode = GitHubMode::IssuesOnly;

        let rendered = render_env_file(DeployMode::Native, &values, Path::new("WORKFLOW.md"));
        assert!(rendered.contains("KAIRASTRA_GITHUB_OWNER=example-owner"));
        assert!(rendered.contains("KAIRASTRA_GITHUB_REPO=example-repo"));
        assert!(!rendered.contains("KAIRASTRA_GITHUB_PROJECT_OWNER"));
        assert!(!rendered.contains("KAIRASTRA_GITHUB_PROJECT_NUMBER"));
        assert!(!rendered.contains("KAIRASTRA_GITHUB_PROJECT_URL"));
    }

    #[test]
    fn workflow_quotes_custom_status_names() {
        let mut values = sample_values();
        values.project_status.active_states = vec!["Ready: Waiting".to_string()];
        values.project_status.terminal_states = vec!["Done / Shipped".to_string()];
        values.project_status.claimable_states = vec!["Ready: Waiting".to_string()];
        values.project_status.in_progress_state = Some("Doing: Active".to_string());
        values.project_status.human_review_state = Some("Needs Review".to_string());
        values.project_status.done_state = None;

        let rendered = render_workflow(DeployMode::Native, &values);
        assert!(rendered.contains(r#"- "Ready: Waiting""#));
        assert!(rendered.contains(r#"- "Done / Shipped""#));
        assert!(rendered.contains(r#"  in_progress_state: "Doing: Active""#));
        assert!(rendered.contains(r#"  human_review_state: "Needs Review""#));
        assert!(rendered.contains("  done_state: null"));
    }

    #[test]
    fn canonical_workflow_body_contains_full_orchestration_sections() {
        let body = canonical_workflow_body();
        assert!(body.contains("## Default posture"));
        assert!(body.contains("## Status map"));
        assert!(body.contains("## Step 0: Determine current issue state and route"));
        assert!(body.contains("Never land or merge from `Human Review`; only land from `Merging`."));
        assert!(body.contains("## Workpad template"));
    }

    #[test]
    fn generated_workflow_body_matches_canonical_template_body() {
        let rendered = render_workflow(DeployMode::Native, &sample_values());
        let (_, body) = rendered.split_once("\n---\n").unwrap();
        assert_eq!(
            body.trim_end_matches('\n'),
            canonical_workflow_body().trim_end_matches('\n')
        );
    }
}
