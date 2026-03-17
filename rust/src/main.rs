use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use clap::{Args, Parser, Subcommand};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

use symphony_rust::auth::{inspect_status, run_login, AuthMode};
use symphony_rust::deploy::DeployMode;
use symphony_rust::doctor::{run as run_doctor, DoctorFormat, DoctorOptions};
use symphony_rust::github::GitHubTracker;
use symphony_rust::orchestrator::Orchestrator;
use symphony_rust::setup::{run as run_setup, SetupOptions};
use symphony_rust::workflow::{default_workflow_path, WorkflowStore};

#[derive(Debug, Parser)]
#[command(name = "symphony-rust")]
#[command(about = "Symphony GitHub orchestrator in Rust")]
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
    #[arg(long, default_value = "codex")]
    provider: String,

    #[command(subcommand)]
    command: AuthCommand,
}

#[derive(Debug, Subcommand)]
enum AuthCommand {
    Status,
    Login(AuthLoginArgs),
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
            let status = inspect_status(&provider)?;
            println!("{}", serde_json::to_string_pretty(&status)?);
            Ok(())
        }
        Command::Auth(AuthArgs {
            provider,
            command: AuthCommand::Login(args),
        }) => run_login(&provider, args.mode.into()),
    }
}

async fn run_orchestrator(workflow: Option<PathBuf>, once: bool) -> Result<()> {
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
        info!(dashboard_url = %dashboard_url, "using GitHub dashboard for Symphony");
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
