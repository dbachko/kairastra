use std::process::ExitStatus;

use crate::workspace::ProviderLease;
use anyhow::{anyhow, Context, Result};
use tokio::process::{Child, Command};
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

pub(crate) struct ProcessExit {
    pub(crate) status: ExitStatus,
    pub(crate) stdin_error: Option<String>,
}

// The lease survives cancellation and bounded foreground cleanup. The reaper
// releases it only after the child has exited and inherited stderr reaches EOF.
pub(crate) struct ProviderProcess {
    child: Option<Child>,
    lease: Option<ProviderLease>,
    stdin_writer: Option<JoinHandle<Result<()>>>,
    stderr_logger: Option<JoinHandle<Result<()>>>,
}

impl ProviderProcess {
    pub(crate) fn spawn(command: &mut Command, workspace: &std::path::Path) -> Result<Self> {
        let lease = ProviderLease::acquire(workspace)?;
        match command.spawn() {
            Ok(child) => Ok(Self {
                child: Some(child),
                lease: Some(lease),
                stdin_writer: None,
                stderr_logger: None,
            }),
            Err(error) => {
                lease.release()?;
                Err(error.into())
            }
        }
    }

    pub(crate) fn child_mut(&mut self) -> &mut Child {
        self.child.as_mut().expect("provider process is active")
    }

    pub(crate) fn track_stdin(&mut self, task: JoinHandle<Result<()>>) {
        self.stdin_writer = Some(task);
    }

    pub(crate) fn track_stderr(&mut self, task: JoinHandle<Result<()>>) {
        self.stderr_logger = Some(task);
    }

    pub(crate) async fn finish(mut self, grace: Duration) -> Result<ProcessExit> {
        let status = wait_for_exit(self.child_mut(), grace).await;
        let task = self.reap(false);
        let drained = match timeout(grace, task).await {
            Ok(result) => result.context("provider reaper failed")?,
            Err(_) => Err(anyhow!("provider_background_task_timeout: workspace_quarantined until provider cleanup finishes")),
        };
        match (status, drained) {
            (Ok(status), Ok(stdin_error)) => Ok(ProcessExit {
                status,
                stdin_error,
            }),
            (Err(error), Ok(_)) | (Ok(_), Err(error)) => Err(error),
            (Err(error), Err(cleanup)) => Err(anyhow!("{error:#}; cleanup={cleanup:#}")),
        }
    }

    fn reap(&mut self, kill: bool) -> JoinHandle<Result<Option<String>>> {
        let mut child = self.child.take().expect("provider process is active");
        let lease = self.lease.take().expect("provider lease is active");
        let stdin = self.stdin_writer.take();
        let stderr = self.stderr_logger.take();
        if kill {
            let _ = child.start_kill();
        }
        tokio::spawn(async move {
            // These waits run outside the bounded worker path. On cancellation or
            // daemon shutdown the durable lease stays in place for manual recovery.
            child
                .wait()
                .await
                .context("provider exit could not be verified; workspace_quarantined")?;
            let stdin_error = match stdin {
                Some(task) => match task.await {
                    Ok(Ok(())) => None,
                    Ok(Err(error)) => Some(error.to_string()),
                    Err(error) => Some(format!("stdin task failed: {error}")),
                },
                None => None,
            };
            if let Some(task) = stderr {
                task.await
                    .context("stderr task failed; workspace_quarantined")?
                    .context("stderr EOF could not be verified; workspace_quarantined")?;
            }
            lease.release()?;
            Ok(stdin_error)
        })
    }
}

impl Drop for ProviderProcess {
    fn drop(&mut self) {
        if self.child.is_some() && tokio::runtime::Handle::try_current().is_ok() {
            // Reaping owns the lease even when the turn future is aborted.
            std::mem::drop(self.reap(true));
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use tokio::io::AsyncReadExt;

    pub(crate) struct ReleaseOnDrop(pub PathBuf);

    impl Drop for ReleaseOnDrop {
        fn drop(&mut self) {
            let _ = std::fs::write(&self.0, "release");
        }
    }

    async fn wait_for_file(path: &Path) {
        timeout(Duration::from_secs(5), async {
            while !path.exists() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("fixture did not become ready");
    }

    async fn descendant(workspace: &Path, parent_exits: bool) -> (ProviderProcess, ReleaseOnDrop) {
        let release = ReleaseOnDrop(workspace.join("release"));
        let ending = if parent_exits { "exit 0" } else { "wait" };
        let mut command = Command::new("sh");
        command.args(["-c", &format!("(while [ ! -f release ] && [ -d \"$PWD\" ]; do sleep 0.02; done; printf late > descendant-finished) & printf ready > ready; {ending}")])
            .current_dir(workspace).stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::piped()).kill_on_drop(true);
        let mut process = ProviderProcess::spawn(&mut command, workspace).unwrap();
        let mut stderr = process.child_mut().stderr.take().unwrap();
        process.track_stderr(tokio::spawn(async move {
            stderr.read_to_end(&mut Vec::new()).await?;
            Ok(())
        }));
        wait_for_file(&workspace.join("ready")).await;
        if parent_exits {
            timeout(Duration::from_secs(5), process.child_mut().wait())
                .await
                .unwrap()
                .unwrap();
        }
        (process, release)
    }

    async fn wait_for_release(workspace: &Path) {
        timeout(Duration::from_secs(5), async {
            while crate::workspace::ensure_workspace_idle(workspace).is_err() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("workspace lease did not release after stderr EOF");
    }

    #[tokio::test]
    async fn inherited_stderr_quarantines_workspace_until_descendant_finishes() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("WORK");
        std::fs::create_dir(&workspace).unwrap();
        assert!(std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&workspace)
            .status()
            .unwrap()
            .success());
        let (process, release) = descendant(&workspace, true).await;
        // A normal repository clean must not erase daemon-owned lease state.
        assert!(std::process::Command::new("git")
            .args(["clean", "-fdx"])
            .current_dir(&workspace)
            .status()
            .unwrap()
            .success());
        assert!(crate::workspace::ensure_workspace_idle(&workspace).is_err());
        let error = timeout(
            Duration::from_secs(2),
            process.finish(Duration::from_millis(50)),
        )
        .await
        .expect("foreground cleanup must be bounded")
        .err()
        .unwrap();
        assert!(error
            .to_string()
            .contains("provider_background_task_timeout"));
        assert!(crate::workspace::ensure_workspace_idle(&workspace).is_err());
        let mut settings = crate::config::Settings::from_workflow(&crate::model::WorkflowDefinition {
            config: serde_yaml::from_str(&format!("tracker:\n  kind: github\n  owner: test\n  project_v2_number: 1\n  api_key: fake\nworkspace:\n  root: {}\nagent:\n  provider: claude\nproviders:\n  claude: {{}}", dir.path().display())).unwrap(),
            prompt_template: String::new(),
        }).unwrap();
        settings.hooks.after_run = Some("touch after-run-attempted".into());
        settings.hooks.before_remove = Some("touch before-remove-attempted".into());
        let issue = serde_json::from_value(serde_json::json!({"id":"1","identifier":"WORK","title":"Test","state":"In Progress","assignees":[],"labels":[],"blocked_by":[]})).unwrap();
        assert!(crate::workspace::ensure_workspace(&settings, &issue)
            .await
            .unwrap_err()
            .to_string()
            .contains("workspace_quarantined"));
        assert!(crate::workspace::remove_issue_workspace(&settings, "WORK")
            .await
            .unwrap_err()
            .to_string()
            .contains("workspace_quarantined"));
        assert!(
            crate::workspace::run_after_run_hook(&settings, &workspace, &issue)
                .await
                .is_err()
        );
        assert!(!workspace.join("after-run-attempted").exists());
        assert!(!workspace.join("before-remove-attempted").exists());
        drop(release);
        wait_for_release(&workspace).await;
        assert!(workspace.join("descendant-finished").exists());
        assert!(
            !crate::workspace::ensure_workspace(&settings, &issue)
                .await
                .unwrap()
                .created_now
        );
    }

    #[tokio::test]
    async fn cancelled_process_keeps_lease_until_inherited_stderr_closes() {
        let dir = tempfile::tempdir().unwrap();
        let (process, release) = descendant(dir.path(), false).await;
        drop(process);
        assert!(crate::workspace::ensure_workspace_idle(dir.path()).is_err());
        drop(release);
        wait_for_release(dir.path()).await;
        assert!(dir.path().join("descendant-finished").exists());
    }

    #[tokio::test]
    async fn failed_spawn_releases_unused_lease() {
        let dir = tempfile::tempdir().unwrap();
        assert!(ProviderProcess::spawn(
            &mut Command::new(dir.path().join("does-not-exist")),
            dir.path()
        )
        .is_err());
        crate::workspace::ensure_workspace_idle(dir.path()).unwrap();
    }
}
