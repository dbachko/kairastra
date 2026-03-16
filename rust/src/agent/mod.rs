pub mod backend;
pub mod codex;
pub mod events;

use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, Result};

use crate::config::{AgentProvider, Settings};
use crate::github::GitHubTracker;

pub use backend::{AgentBackend, AgentSession, TurnResult};
pub use events::{AgentEvent, AgentEventKind};

pub async fn start_session(
    settings: &Settings,
    tracker: Arc<GitHubTracker>,
    workspace: &Path,
) -> Result<Box<dyn AgentSession>> {
    match settings.agent.provider {
        AgentProvider::Codex => {
            codex::CodexBackend
                .start_session(settings, tracker, workspace)
                .await
        }
        other => Err(anyhow!("unsupported_agent_provider: {}", other.as_str())),
    }
}
