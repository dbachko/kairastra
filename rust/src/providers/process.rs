use std::process::ExitStatus;

use anyhow::{anyhow, Context, Result};
use tokio::process::Child;
use tokio::task::JoinHandle;
use tokio::time::{timeout, Duration};

pub(crate) fn shutdown_grace(turn_timeout_ms: u64) -> Duration {
    Duration::from_millis(if turn_timeout_ms == 0 {
        2_000
    } else {
        turn_timeout_ms.min(2_000)
    })
}

pub(crate) async fn wait_for_exit(child: &mut Child, grace: Duration) -> Result<ExitStatus> {
    match timeout(grace, child.wait()).await {
        Ok(status) => status.context("failed to wait for provider process"),
        Err(_) => {
            child
                .start_kill()
                .context("failed to terminate stalled provider")?;
            let _ = timeout(Duration::from_secs(2), child.wait()).await;
            Err(anyhow!("provider_shutdown_timeout"))
        }
    }
}

pub(crate) async fn drain_task<T>(mut task: JoinHandle<T>, grace: Duration) -> Result<T> {
    match timeout(grace, &mut task).await {
        Ok(result) => result.context("provider background task failed"),
        Err(_) => {
            task.abort();
            Err(anyhow!("provider_background_task_timeout"))
        }
    }
}
