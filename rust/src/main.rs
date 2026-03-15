use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use clap::Parser;
use tokio::sync::Notify;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

use symphony_rust::github::GitHubTracker;
use symphony_rust::orchestrator::Orchestrator;
use symphony_rust::webhook;
use symphony_rust::workflow::{default_workflow_path, WorkflowStore};

#[derive(Debug, Parser)]
#[command(name = "symphony-rust")]
#[command(about = "Symphony GitHub orchestrator in Rust")]
struct Cli {
    #[arg(value_name = "WORKFLOW")]
    workflow: Option<PathBuf>,

    #[arg(long)]
    once: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    let workflow_path = match cli.workflow {
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
    let wake_signal = Arc::new(Notify::new());
    let tracker = Arc::new(GitHubTracker::new(snapshot.settings.tracker.clone())?);
    let webhook_server = webhook::spawn(&snapshot.settings.webhooks, wake_signal.clone()).await?;
    let orchestrator = Orchestrator::new(workflow_store, tracker, wake_signal);

    let orchestration = if cli.once {
        orchestrator.run_once().await
    } else {
        orchestrator.run().await
    };

    if let Some(server) = webhook_server {
        server.abort();
    }

    orchestration
}
