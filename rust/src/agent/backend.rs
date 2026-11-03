use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use tokio::sync::mpsc::UnboundedSender;

use crate::config::Settings;
use crate::github::GitHubTracker;
use crate::model::Issue;

use super::events::AgentEvent;

#[derive(Debug, Clone)]
pub struct TurnResult {
    pub session_id: String,
    pub thread_id: String,
    pub turn_id: String,
}

// async-trait 0.1.89 adds redundant must_use attributes to boxed futures on Rust 1.99.
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait AgentBackend: Send + Sync {
    async fn start_session(
        &self,
        settings: &Settings,
        tracker: Arc<GitHubTracker>,
        workspace: &Path,
    ) -> Result<Box<dyn AgentSession>>;
}

// async-trait 0.1.89 adds redundant must_use attributes to boxed futures on Rust 1.99.
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait AgentSession: Send {
    async fn run_turn(
        &mut self,
        issue: &Issue,
        prompt: &str,
        on_event: &UnboundedSender<AgentEvent>,
    ) -> Result<TurnResult>;

    async fn stop(&mut self) -> Result<()>;

    fn process_id(&self) -> Option<u32>;
}
