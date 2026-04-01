use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use clap::{Args, Parser, Subcommand};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

use kairastra::auth::{inspect_status, run_login, run_login_menu, AuthMode};
use kairastra::deploy::DeployMode;
use kairastra::doctor::{run as run_doctor, DoctorFormat, DoctorOptions};
use kairastra::envfile::{apply_env_if_missing, load_env_file};
use kairastra::github::GitHubTracker;
use kairastra::github_mcp;
use kairastra::orchestrator::Orchestrator;
use kairastra::setup::{run as run_setup, SetupOptions};
use kairastra::workflow::{default_env_file_path, default_workflow_path, WorkflowStore};

#[derive(Debug, Parser)]
#[command(name = "kairastra")]
#[command(about = "Kairastra GitHub orchestrator in Rust")]
struct ModernCli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Run(RunArgs),
    Setup(SetupArgs),
    Doctor(DoctorArgs),
    Auth(AuthArgs),
    GithubMcp,
}

#[derive(Debug, Args)]
struct RunArgs {
    #[arg(value_name = "WORKFLOW")]
    workflow: Option<PathBuf>,

    #[arg(long)]
    once: bool,
}

#[derive(Debug, Args)]
struct SetupArgs {
    #[arg(long)]
    mode: Option<DeployMode>,

    #[arg(long, value_name = "PATH")]
    workflow: Option<PathBuf>,

    #[arg(long = "env-file", value_name = "PATH")]
    env_file: Option<PathBuf>,

    #[arg(long = "service-unit", value_name = "PATH")]
    service_unit: Option<PathBuf>,

    #[arg(long = "binary-path", value_name = "PATH")]
    binary_path: Option<PathBuf>,

    #[arg(long)]
    bootstrap_github: bool,

    #[arg(long)]
    skip_labels: bool,

    #[arg(long)]
    skip_priority_field: bool,

    #[arg(long)]
    reconfigure: bool,

    #[arg(long)]
    non_interactive: bool,
}

#[derive(Debug, Args)]
struct DoctorArgs {
    #[arg(long, value_name = "PATH")]
    workflow: Option<PathBuf>,

    #[arg(long = "env-file", value_name = "PATH")]
    env_file: Option<PathBuf>,

    #[arg(long)]
    mode: Option<DeployMode>,

    #[arg(long, value_enum, default_value_t = DoctorFormat::Text)]
    format: DoctorFormat,
}

#[derive(Debug, Args)]
struct AuthArgs {
    #[arg(long)]
    provider: Option<String>,

    #[command(subcommand)]
    command: AuthCommand,
}

#[derive(Debug, Subcommand)]
enum AuthCommand {
    Status,
    Login(AuthLoginArgs),
    Menu,
}

#[derive(Debug, Args)]
struct AuthLoginArgs {
    #[arg(long, value_enum)]
    mode: AuthModeArg,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum AuthModeArg {
    Subscription,
    ApiKey,
}

impl From<AuthModeArg> for AuthMode {
    fn from(value: AuthModeArg) -> Self {
        match value {
            AuthModeArg::Subscription => AuthMode::Subscription,
            AuthModeArg::ApiKey => AuthMode::ApiKey,
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let cli = ModernCli::parse();
    match cli.command {
        Command::Run(args) => run_orchestrator(args.workflow, args.once).await,
        Command::Setup(args) => {
            run_setup(SetupOptions {
                mode: args.mode,
                workflow: args.workflow,
                env_file: args.env_file,
                service_unit: args.service_unit,
                binary_path: args.binary_path,
                bootstrap_github: args.bootstrap_github,
                skip_labels: args.skip_labels,
                skip_priority_field: args.skip_priority_field,
                reconfigure: args.reconfigure,
                non_interactive: args.non_interactive,
            })
            .await
        }
        Command::Doctor(args) => run_doctor(DoctorOptions {
            workflow: args.workflow,
            env_file: args.env_file,
            mode: args.mode,
            format: args.format,
        })
        .await
        .map(|_| ()),
        Command::Auth(AuthArgs {
            provider,
            command: AuthCommand::Status,
        }) => {
            let provider = provider.unwrap_or_else(|| "codex".to_string());
            let status = inspect_status(&provider)?;
            println!("{}", serde_json::to_string_pretty(&status)?);
            Ok(())
        }
        Command::Auth(AuthArgs {
            provider,
            command: AuthCommand::Login(args),
        }) => run_login(
            &provider.unwrap_or_else(|| "codex".to_string()),
            args.mode.into(),
        ),
        Command::Auth(AuthArgs {
            provider,
            command: AuthCommand::Menu,
        }) => run_login_menu(provider.as_deref()),
        Command::GithubMcp => github_mcp::run().await,
    }
}

async fn run_orchestrator(workflow: Option<PathBuf>, once: bool) -> Result<()> {
    if let Some(path) = default_env_file_path()? {
        let env_values = load_env_file(&path)?;
        apply_env_if_missing(&env_values);
    }

    let workflow_path = match workflow {
        Some(path) => path,
        None => match std::env::var_os("WORKFLOW_PATH") {
            Some(path) => PathBuf::from(path),
            None => default_workflow_path()?,
        },
    };

    let workflow_store = Arc::new(WorkflowStore::new(workflow_path));
    let snapshot = workflow_store.current()?;
    if let Some(dashboard_url) = snapshot.settings.tracker_dashboard_url() {
        info!(dashboard_url = %dashboard_url, "using GitHub dashboard for Kairastra");
    } else {
        warn!("no GitHub dashboard URL configured; falling back to tracker-only polling");
    }
    let tracker = Arc::new(GitHubTracker::new(snapshot.settings.tracker.clone())?);
    let orchestrator = Orchestrator::new(workflow_store, tracker);

    if once {
        orchestrator.run_once().await?;
    } else {
        orchestrator.run().await?;
    }

    Ok(())
}
