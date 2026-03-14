use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use clap::Parser;
use tracing_subscriber::EnvFilter;

use symphony_rust::github::GitHubTracker;
use symphony_rust::orchestrator::Orchestrator;
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
    let tracker = Arc::new(GitHubTracker::new(snapshot.settings.tracker.clone())?);
    let orchestrator = Orchestrator::new(workflow_store, tracker);

    if cli.once {
        orchestrator.run_once().await?;
    } else {
        orchestrator.run().await?;
    }

    Ok(())
}
