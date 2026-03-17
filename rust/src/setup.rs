use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use dialoguer::{theme::ColorfulTheme, Confirm, Input, Select};

use crate::deploy::DeployMode;
use crate::doctor::{self, DoctorFormat, DoctorOptions};
use crate::providers::{self, ProviderSetupConfig};

#[derive(Debug, Clone)]
pub struct SetupOptions {
    pub mode: Option<DeployMode>,
    pub workflow: Option<PathBuf>,
    pub env_file: Option<PathBuf>,
    pub service_unit: Option<PathBuf>,
    pub binary_path: Option<PathBuf>,
    pub non_interactive: bool,
}

#[derive(Debug, Clone)]
struct SetupValues {
    provider: String,
    github_owner: String,
    github_repo: String,
    github_project_number: String,
    github_project_url: String,
    workspace_root: String,
    seed_repo: String,
    git_clone_url: String,
    assignee_login: String,
    max_concurrent_agents: String,
    max_turns: String,
    provider_config: ProviderSetupConfig,
    github_token: String,
    openai_api_key: String,
    rust_log: String,
    binary_path: String,
}

pub async fn run(options: SetupOptions) -> Result<()> {
    let layout = detect_layout(&std::env::current_dir()?);
    let mode = choose_mode(options.mode, options.non_interactive)?;
    let workflow_path = resolve_workflow_path(&layout, options.workflow.as_ref());
    let env_file_path = resolve_env_file_path(&layout, mode, options.env_file.as_ref());
    let service_unit_path = if mode == DeployMode::Native {
        Some(resolve_service_path(&layout, options.service_unit.as_ref()))
    } else {
        None
    };

    let values = collect_values(mode, options.binary_path.as_ref(), options.non_interactive)?;

    write_text_file(
        &workflow_path,
        &render_workflow(&values),
        options.non_interactive,
    )?;
    write_text_file(
        &env_file_path,
        &render_env_file(mode, &values, &workflow_path),
        options.non_interactive,
    )?;

    if let Some(path) = service_unit_path.as_ref() {
        let unit = render_systemd_unit(&values, &workflow_path, &env_file_path);
        write_text_file(path, &unit, options.non_interactive)?;
    }

    let _ = doctor::run(DoctorOptions {
        workflow: Some(workflow_path.clone()),
        env_file: Some(env_file_path.clone()),
        mode: Some(mode),
        format: DoctorFormat::Text,
    })
    .await;

    println!();
    println!("Generated:");
    println!("- workflow: {}", workflow_path.display());
    println!("- env file: {}", env_file_path.display());
    if let Some(path) = service_unit_path.as_ref() {
        println!("- systemd unit: {}", path.display());
        println!("Next steps:");
        println!(
            "1. Install the unit: sudo cp {} /etc/systemd/system/symphony.service",
            path.display()
        );
        println!("2. Reload systemd: sudo systemctl daemon-reload");
        println!("3. Start Symphony: sudo systemctl enable --now symphony.service");
    } else {
        println!("Next steps:");
        println!(
            "1. Review {} and {}",
            workflow_path.display(),
            env_file_path.display()
        );
        println!(
            "2. Start Docker mode: {}",
            docker_make_command(&layout, "docker-up")
        );
        if providers::setup_auth_mode(&values.provider_config) == crate::auth::AuthMode::Chatgpt {
            println!(
                "3. {}: {}",
                providers::docker_login_message(&values.provider)
                    .unwrap_or("Initialize provider auth in the container"),
                docker_make_command(&layout, "docker-login")
            );
        }
    }

    Ok(())
}

fn choose_mode(explicit: Option<DeployMode>, non_interactive: bool) -> Result<DeployMode> {
    if let Some(mode) = explicit {
        return Ok(mode);
    }

    if non_interactive {
        return Ok(if Path::new("rust/compose.yml").is_file() {
            DeployMode::Docker
        } else {
            DeployMode::Native
        });
    }

    let options = [DeployMode::Native, DeployMode::Docker];
    let labels = ["Native VPS (systemd)", "Docker Compose"];
    let selection = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("Deployment mode")
        .items(&labels)
        .default(0)
        .interact()?;
    Ok(options[selection])
}

#[derive(Debug, Clone)]
struct SetupLayout {
    repo_root: PathBuf,
    rust_dir: PathBuf,
}

fn detect_layout(cwd: &Path) -> SetupLayout {
    if cwd.file_name().and_then(|name| name.to_str()) == Some("rust")
        && cwd.join("Cargo.toml").is_file()
        && cwd.join("compose.yml").is_file()
    {
        return SetupLayout {
            repo_root: cwd.parent().unwrap_or(cwd).to_path_buf(),
            rust_dir: cwd.to_path_buf(),
        };
    }

    if cwd.join("rust/Cargo.toml").is_file() && cwd.join("rust/compose.yml").is_file() {
        return SetupLayout {
            repo_root: cwd.to_path_buf(),
            rust_dir: cwd.join("rust"),
        };
    }

    SetupLayout {
        repo_root: cwd.to_path_buf(),
        rust_dir: cwd.to_path_buf(),
    }
}

fn resolve_workflow_path(layout: &SetupLayout, explicit: Option<&PathBuf>) -> PathBuf {
    if let Some(path) = explicit {
        return path.clone();
    }

    layout.repo_root.join("WORKFLOW.md")
}

fn resolve_env_file_path(
    layout: &SetupLayout,
    mode: DeployMode,
    explicit: Option<&PathBuf>,
) -> PathBuf {
    if let Some(path) = explicit {
        return path.clone();
    }

    match mode {
        DeployMode::Native => layout.repo_root.join("symphony.env"),
        DeployMode::Docker => {
            let default = layout.rust_dir.join(".env");
            if default.exists() {
                layout.rust_dir.join(".env.generated")
            } else {
                default
            }
        }
    }
}

fn resolve_service_path(layout: &SetupLayout, explicit: Option<&PathBuf>) -> PathBuf {
    explicit
        .cloned()
        .unwrap_or_else(|| layout.repo_root.join("symphony.service"))
}

fn docker_make_command(layout: &SetupLayout, target: &str) -> String {
    if layout.repo_root == layout.rust_dir {
        format!("make {}", target)
    } else {
        format!("make -C {} {}", layout.rust_dir.display(), target)
    }
}

fn collect_values(
    mode: DeployMode,
    explicit_binary_path: Option<&PathBuf>,
    non_interactive: bool,
) -> Result<SetupValues> {
    let cwd = std::env::current_dir()?;
    let theme = ColorfulTheme::default();
    let env_github_owner = std::env::var("SYMPHONY_GITHUB_OWNER").unwrap_or_default();
    let env_github_repo = std::env::var("SYMPHONY_GITHUB_REPO").unwrap_or_default();
    let env_project_number = std::env::var("SYMPHONY_GITHUB_PROJECT_NUMBER").unwrap_or_default();
    let env_project_url = std::env::var("SYMPHONY_GITHUB_PROJECT_URL").unwrap_or_default();

    let github_project_url = ask_string(
        &theme,
        "GitHub Project URL (optional, can auto-fill owner and number)",
        env_project_url,
        non_interactive,
        true,
    )?;
    let parsed_project = parse_project_url(&github_project_url);
    let repo_input = ask_string(
        &theme,
        "GitHub repo (name or GitHub URL)",
        env_github_repo,
        non_interactive,
        false,
    )?;
    let parsed_repo = parse_repo_input(&repo_input);
    let github_owner = if !env_github_owner.trim().is_empty() {
        env_github_owner
    } else if let Some(parsed) = parsed_project.as_ref() {
        parsed.owner.clone()
    } else if let Some(parsed) = parsed_repo.as_ref().and_then(|parsed| parsed.owner.clone()) {
        parsed
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
    let github_project_number = if !env_project_number.trim().is_empty() {
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
    };
    let workspace_root = ask_string(
        &theme,
        "Workspace root",
        match mode {
            DeployMode::Native => "/var/lib/symphony/workspaces".to_string(),
            DeployMode::Docker => "/workspaces".to_string(),
        },
        non_interactive,
        false,
    )?;
    let seed_repo = ask_string(
        &theme,
        "Seed repo path",
        std::env::var("SYMPHONY_SEED_REPO")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| cwd.display().to_string()),
        non_interactive,
        false,
    )?;
    let git_clone_url = ask_string(
        &theme,
        "Canonical clone URL (optional)",
        std::env::var("SYMPHONY_GIT_CLONE_URL").unwrap_or_default(),
        non_interactive,
        true,
    )?;
    let assignee_login = ask_string(
        &theme,
        "Dispatch only assigned issues for login (optional)",
        std::env::var("SYMPHONY_AGENT_ASSIGNEE").unwrap_or_default(),
        non_interactive,
        true,
    )?;
    let max_concurrent_agents = ask_string(
        &theme,
        "Max concurrent agents",
        "4".to_string(),
        non_interactive,
        false,
    )?;
    let max_turns = ask_string(
        &theme,
        "Max turns per issue",
        "20".to_string(),
        non_interactive,
        false,
    )?;
    let provider = providers::default_setup_provider().to_string();
    let provider_config = providers::collect_setup_config(&provider, non_interactive)?;
    let github_token = std::env::var("GITHUB_TOKEN").unwrap_or_default();
    let openai_api_key = std::env::var("OPENAI_API_KEY").unwrap_or_default();
    let rust_log = std::env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string());
    let binary_path = detect_binary_path(explicit_binary_path);

    Ok(SetupValues {
        provider,
        github_owner,
        github_repo,
        github_project_number,
        github_project_url,
        workspace_root,
        seed_repo,
        git_clone_url,
        assignee_login,
        max_concurrent_agents,
        max_turns,
        provider_config,
        github_token,
        openai_api_key,
        rust_log,
        binary_path,
    })
}

fn detect_binary_path(explicit: Option<&PathBuf>) -> String {
    if let Some(path) = explicit {
        return path.display().to_string();
    }

    if let Ok(path) = std::env::var("SYMPHONY_BINARY_PATH") {
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

    "/usr/local/bin/symphony-rust".to_string()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedProjectUrl {
    owner: String,
    project_number: String,
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

    Some(ParsedProjectUrl {
        owner: segments[1].to_string(),
        project_number: project_number.to_string(),
    })
}

fn parse_repo_input(value: &str) -> Option<ParsedRepoInput> {
    let trimmed = value.trim();
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

fn write_text_file(path: &Path, content: &str, non_interactive: bool) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
    }

    let should_write = if path.exists() && !non_interactive {
        Confirm::with_theme(&ColorfulTheme::default())
            .with_prompt(format!("Overwrite {}?", path.display()))
            .default(false)
            .interact()?
    } else {
        true
    };

    if should_write {
        fs::write(path, content).with_context(|| format!("failed to write {}", path.display()))?;
    }

    Ok(())
}

fn render_workflow(values: &SetupValues) -> String {
    let assignee_line = if values.assignee_login.trim().is_empty() {
        String::new()
    } else {
        "  assignee_login: $SYMPHONY_AGENT_ASSIGNEE\n".to_string()
    };
    let provider_section = providers::render_workflow_provider_section(&values.provider_config);

    format!(
        r#"---
tracker:
  kind: github
  mode: projects_v2
  api_key: $GITHUB_TOKEN
  owner: $SYMPHONY_GITHUB_OWNER
  repo: $SYMPHONY_GITHUB_REPO
  project_v2_number: $SYMPHONY_GITHUB_PROJECT_NUMBER
  project_url: $SYMPHONY_GITHUB_PROJECT_URL
  status_source:
    type: project_field
    name: Status
  priority_source:
    type: project_field
    name: Priority
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
workspace:
  root: $SYMPHONY_WORKSPACE_ROOT
hooks:
  after_create: |
    set -euo pipefail

    clone_with_auth() {{
      clone_url="$1"
      if [ -n "${{GITHUB_TOKEN:-}}" ] && printf '%s' "$clone_url" | grep -q '^https://github.com/'; then
        auth_header="$(printf 'x-access-token:%s' "$GITHUB_TOKEN" | base64 | tr -d '\n')"
        git -c http.extraheader="Authorization: Basic ${{auth_header}}" clone --depth 1 "$clone_url" .
        git config http.https://github.com/.extraheader "Authorization: Basic ${{auth_header}}"
      else
        git clone --depth 1 "$clone_url" .
      fi
    }}

    overlay_seed_repo() {{
      seed_repo="$1"
      if command -v rsync >/dev/null 2>&1; then
        rsync -a --delete --exclude '.git' "${{seed_repo}}/" ./
      else
        echo "rsync is required when overlaying SYMPHONY_SEED_REPO on top of a remote clone." >&2
        exit 1
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

    restore_support_dir_from_seed() {{
      support_dir="$1"
      if [ -e "$support_dir" ]; then
        return 0
      fi
      if [ -n "${{SYMPHONY_SEED_REPO:-}}" ] && [ -e "${{SYMPHONY_SEED_REPO}}/$support_dir" ]; then
        cp -R "${{SYMPHONY_SEED_REPO}}/$support_dir" "$support_dir"
      fi
    }}

    require_workspace_support_dirs() {{
      for support_dir in .codex .github; do
        restore_support_dir_from_seed "$support_dir"
        if [ ! -e "$support_dir" ]; then
          echo "Workspace bootstrap missing required repository support directory: $support_dir" >&2
          exit 1
        fi
      done
    }}

    adopt_seed_repo_origin() {{
      if [ -z "${{SYMPHONY_SEED_REPO:-}}" ] || [ ! -d "$SYMPHONY_SEED_REPO/.git" ]; then
        return 0
      fi
      source_remote="$(git -C "$SYMPHONY_SEED_REPO" config --get remote.origin.url || true)"
      current_remote="$(git config --get remote.origin.url || true)"
      if [ -n "$source_remote" ] && {{ [ "$current_remote" = "$SYMPHONY_SEED_REPO" ] || [ -z "$current_remote" ]; }}; then
        git remote set-url origin "$source_remote"
      fi
    }}

    if [ -n "${{SYMPHONY_GIT_CLONE_URL:-}}" ]; then
      clone_with_auth "$SYMPHONY_GIT_CLONE_URL"
      if [ -n "${{SYMPHONY_SEED_REPO:-}}" ] && [ -d "$SYMPHONY_SEED_REPO" ]; then
        overlay_seed_repo "$SYMPHONY_SEED_REPO"
      fi
    elif [ -n "${{SYMPHONY_SEED_REPO:-}}" ] && [ -d "$SYMPHONY_SEED_REPO/.git" ]; then
      git clone "$SYMPHONY_SEED_REPO" .
      adopt_seed_repo_origin
    else
      echo "Set SYMPHONY_GIT_CLONE_URL, or point SYMPHONY_SEED_REPO at a git checkout, before running Symphony." >&2
      exit 1
    fi

    if [ -n "${{SYMPHONY_GIT_PUSH_URL:-}}" ]; then
      git remote set-url --push origin "$SYMPHONY_GIT_PUSH_URL"
    fi

    require_workspace_support_dirs
    configure_github_auth

    git config user.name "${{SYMPHONY_GIT_AUTHOR_NAME:-Symphony}}"
    git config user.email "${{SYMPHONY_GIT_AUTHOR_EMAIL:-symphony@users.noreply.github.com}}"
  before_run: |
    set -euo pipefail

    restore_support_dir_from_seed() {{
      support_dir="$1"
      if [ -e "$support_dir" ]; then
        return 0
      fi
      if [ -n "${{SYMPHONY_SEED_REPO:-}}" ] && [ -e "${{SYMPHONY_SEED_REPO}}/$support_dir" ]; then
        cp -R "${{SYMPHONY_SEED_REPO}}/$support_dir" "$support_dir"
      fi
    }}

    require_workspace_support_dirs() {{
      for support_dir in .codex .github; do
        restore_support_dir_from_seed "$support_dir"
        if [ ! -e "$support_dir" ]; then
          echo "Workspace bootstrap missing required repository support directory: $support_dir" >&2
          exit 1
        fi
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

    adopt_seed_repo_origin() {{
      if [ -z "${{SYMPHONY_SEED_REPO:-}}" ] || [ ! -d "$SYMPHONY_SEED_REPO/.git" ]; then
        return 0
      fi
      source_remote="$(git -C "$SYMPHONY_SEED_REPO" config --get remote.origin.url || true)"
      current_remote="$(git config --get remote.origin.url || true)"
      if [ -n "$source_remote" ] && {{ [ "$current_remote" = "$SYMPHONY_SEED_REPO" ] || [ -z "$current_remote" ]; }}; then
        git remote set-url origin "$source_remote"
      fi
    }}

    require_workspace_support_dirs
    adopt_seed_repo_origin

    if [ -n "${{SYMPHONY_GIT_PUSH_URL:-}}" ]; then
      git remote set-url --push origin "$SYMPHONY_GIT_PUSH_URL"
    fi

    configure_github_auth

    git config user.name "${{SYMPHONY_GIT_AUTHOR_NAME:-Symphony}}"
    git config user.email "${{SYMPHONY_GIT_AUTHOR_EMAIL:-symphony@users.noreply.github.com}}"
agent:
  provider: {provider}
  max_concurrent_agents: {max_concurrent_agents}
  max_turns: {max_turns}
{assignee_line}{provider_section}
---

You are working on GitHub issue `{{{{ issue.identifier }}}}`.

{{% if tracker.dashboard_url %}}
Dashboard: {{{{ tracker.dashboard_url }}}}
{{% endif %}}
Title: {{{{ issue.title }}}}
URL: {{{{ issue.url }}}}

Description:
{{% if issue.description %}}
{{{{ issue.description }}}}
{{% else %}}
No description provided.
{{% endif %}}
"#,
        provider = values.provider,
        max_concurrent_agents = values.max_concurrent_agents,
        max_turns = values.max_turns,
        assignee_line = assignee_line,
        provider_section = provider_section
    )
}

fn render_env_file(mode: DeployMode, values: &SetupValues, workflow_path: &Path) -> String {
    let workflow_abs = absolute_display_path(workflow_path);
    let provider_env = providers::render_env_provider_section(mode, &values.provider_config);
    match mode {
        DeployMode::Native => format!(
            r#"# Generated by `symphony-rust setup`
SYMPHONY_DEPLOY_MODE=native
GITHUB_TOKEN={github_token}
OPENAI_API_KEY={openai_api_key}
WORKFLOW_PATH={workflow_path}
SYMPHONY_WORKSPACE_ROOT={workspace_root}
SYMPHONY_GITHUB_OWNER={github_owner}
SYMPHONY_GITHUB_REPO={github_repo}
SYMPHONY_GITHUB_PROJECT_NUMBER={github_project_number}
SYMPHONY_GITHUB_PROJECT_URL={github_project_url}
SYMPHONY_GIT_CLONE_URL={git_clone_url}
SYMPHONY_SEED_REPO={seed_repo}
SYMPHONY_AGENT_ASSIGNEE={assignee_login}
{provider_env}
RUST_LOG={rust_log}
"#,
            github_token = values.github_token,
            openai_api_key = values.openai_api_key,
            workflow_path = workflow_abs,
            workspace_root = values.workspace_root,
            github_owner = values.github_owner,
            github_repo = values.github_repo,
            github_project_number = values.github_project_number,
            github_project_url = values.github_project_url,
            git_clone_url = values.git_clone_url,
            seed_repo = values.seed_repo,
            assignee_login = values.assignee_login,
            provider_env = provider_env,
            rust_log = values.rust_log,
        ),
        DeployMode::Docker => format!(
            r#"# Generated by `symphony-rust setup`
SYMPHONY_DEPLOY_MODE=docker
GITHUB_TOKEN={github_token}
OPENAI_API_KEY={openai_api_key}
WORKFLOW_FILE={workflow_path}
SEED_REPO_PATH={seed_repo}
SYMPHONY_WORKSPACE_ROOT={workspace_root}
RUST_LOG={rust_log}
SYMPHONY_GITHUB_OWNER={github_owner}
SYMPHONY_GITHUB_REPO={github_repo}
SYMPHONY_GITHUB_PROJECT_NUMBER={github_project_number}
SYMPHONY_GITHUB_PROJECT_URL={github_project_url}
SYMPHONY_GIT_CLONE_URL={git_clone_url}
SYMPHONY_AGENT_ASSIGNEE={assignee_login}
{provider_env}
"#,
            github_token = values.github_token,
            openai_api_key = values.openai_api_key,
            workflow_path = workflow_abs,
            seed_repo = values.seed_repo,
            workspace_root = values.workspace_root,
            rust_log = values.rust_log,
            github_owner = values.github_owner,
            github_repo = values.github_repo,
            github_project_number = values.github_project_number,
            github_project_url = values.github_project_url,
            git_clone_url = values.git_clone_url,
            assignee_login = values.assignee_login,
            provider_env = provider_env,
        ),
    }
}

fn render_systemd_unit(values: &SetupValues, workflow_path: &Path, env_file: &Path) -> String {
    format!(
        r#"[Unit]
Description=Symphony Rust orchestrator
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
    use super::{render_env_file, render_systemd_unit, render_workflow, SetupValues};
    use crate::deploy::DeployMode;
    use crate::providers::codex::setup::CodexSetupConfig;
    use crate::providers::ProviderSetupConfig;
    use std::fs;
    use std::path::Path;
    use tempfile::tempdir;

    fn sample_values() -> SetupValues {
        SetupValues {
            provider: "codex".to_string(),
            github_owner: "openai".to_string(),
            github_repo: "symphony".to_string(),
            github_project_number: "7".to_string(),
            github_project_url: "https://github.com/users/openai/projects/7".to_string(),
            workspace_root: "/workspaces".to_string(),
            seed_repo: "/seed".to_string(),
            git_clone_url: "https://github.com/openai/symphony.git".to_string(),
            assignee_login: "codex-bot".to_string(),
            max_concurrent_agents: "4".to_string(),
            max_turns: "20".to_string(),
            provider_config: ProviderSetupConfig::Codex(CodexSetupConfig {
                auth_mode: crate::auth::AuthMode::Chatgpt,
                model: "gpt-5.4".to_string(),
                reasoning_effort: "high".to_string(),
                fast: true,
            }),
            github_token: String::new(),
            openai_api_key: String::new(),
            rust_log: "info".to_string(),
            binary_path: "/usr/local/bin/symphony-rust".to_string(),
        }
    }

    #[test]
    fn workflow_template_uses_env_placeholders() {
        let rendered = render_workflow(&sample_values());
        assert!(rendered.contains("owner: $SYMPHONY_GITHUB_OWNER"));
        assert!(rendered.contains("provider: codex"));
        assert!(rendered.contains("assignee_login: $SYMPHONY_AGENT_ASSIGNEE"));
        assert!(rendered.contains("providers:"));
        assert!(rendered.contains("model: $SYMPHONY_CODEX_MODEL"));
        assert!(rendered.contains("reasoning_effort: $SYMPHONY_CODEX_REASONING_EFFORT"));
        assert!(rendered.contains("fast: $SYMPHONY_CODEX_FAST"));
        assert!(rendered.contains("for support_dir in .codex .github; do"));
        assert!(
            rendered.contains("Workspace bootstrap missing required repository support directory")
        );
        assert!(rendered.contains("git -C \"$SYMPHONY_SEED_REPO\" config --get remote.origin.url"));
        assert!(rendered.contains("adopt_seed_repo_origin"));
        assert!(rendered.contains("git remote set-url --push origin \"$SYMPHONY_GIT_PUSH_URL\""));
        assert!(rendered.contains("git config --get remote.origin.pushurl || true"));
        assert!(rendered.contains("http.https://github.com/.extraheader"));
        assert!(rendered.contains("before_run: |"));
        assert!(
            rendered.contains("current_remote=\"$(git config --get remote.origin.url || true)\"")
        );
    }

    #[test]
    fn docker_env_uses_workflow_file_key() {
        let rendered = render_env_file(
            DeployMode::Docker,
            &sample_values(),
            Path::new("WORKFLOW.md"),
        );
        assert!(rendered.contains("WORKFLOW_FILE="));
        assert!(rendered.contains("CODEX_AUTH_MODE=chatgpt"));
        assert!(rendered.contains("SYMPHONY_CODEX_MODEL=gpt-5.4"));
        assert!(rendered.contains("SYMPHONY_CODEX_REASONING_EFFORT=high"));
        assert!(rendered.contains("SYMPHONY_CODEX_FAST=true"));
    }

    #[test]
    fn systemd_unit_runs_run_subcommand() {
        let rendered = render_systemd_unit(
            &sample_values(),
            Path::new("WORKFLOW.md"),
            Path::new("symphony.env"),
        );
        assert!(rendered.contains("ExecStart=/usr/local/bin/symphony-rust run"));
    }

    #[test]
    fn parses_project_url_for_owner_and_number() {
        let parsed =
            super::parse_project_url("https://github.com/users/openai/projects/7").unwrap();
        assert_eq!(parsed.owner, "openai");
        assert_eq!(parsed.project_number, "7");
    }

    #[test]
    fn parses_org_project_url_with_extra_path() {
        let parsed =
            super::parse_project_url("https://github.com/orgs/acme/projects/12/views/1").unwrap();
        assert_eq!(parsed.owner, "acme");
        assert_eq!(parsed.project_number, "12");
    }

    #[test]
    fn rejects_unrecognized_project_url() {
        assert!(super::parse_project_url("https://github.com/openai/symphony").is_none());
    }

    #[test]
    fn parses_repo_input_from_url() {
        let parsed = super::parse_repo_input("https://github.com/openai/symphony-gh").unwrap();
        assert_eq!(parsed.owner.as_deref(), Some("openai"));
        assert_eq!(parsed.repo, "symphony-gh");
    }

    #[test]
    fn detects_rust_subdir_layout() {
        let dir = tempdir().unwrap();
        let rust_dir = dir.path().join("rust");
        fs::create_dir_all(&rust_dir).unwrap();
        fs::write(rust_dir.join("Cargo.toml"), "").unwrap();
        fs::write(rust_dir.join("compose.yml"), "").unwrap();

        let from_root = super::detect_layout(dir.path());
        assert_eq!(from_root.repo_root, dir.path());
        assert_eq!(from_root.rust_dir, rust_dir);

        let from_rust = super::detect_layout(&rust_dir);
        assert_eq!(from_rust.repo_root, dir.path());
        assert_eq!(from_rust.rust_dir, rust_dir);
    }

    #[test]
    fn workflow_path_defaults_to_workflow_md_even_when_existing() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("WORKFLOW.md"), "existing").unwrap();

        let layout = super::detect_layout(dir.path());
        let path = super::resolve_workflow_path(&layout, None);

        assert_eq!(path, dir.path().join("WORKFLOW.md"));
    }
}
