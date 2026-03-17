use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;

use crate::auth::{AuthMode, AuthStatus};

pub const COMMAND_NAME: &str = "claude";
const AUTH_DIR_NAME: &str = ".claude";
const AUTH_MODE_ENV: &str = "CLAUDE_AUTH_MODE";
const API_KEY_ENV: &str = "ANTHROPIC_API_KEY";
const DOCKER_VOLUME_HINT: &str =
    "Docker mode should persist Claude auth inside the mounted /root/.claude directory in the container.";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeCliAuthStatus {
    logged_in: bool,
}

pub fn inspect_status() -> AuthStatus {
    let configured_mode = AuthMode::from_env_var(AUTH_MODE_ENV);
    let api_key_present = std::env::var(API_KEY_ENV)
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false);
    let auth_file_path = auth_file_path();
    let logged_in = read_logged_in_status().unwrap_or(false);
    let auth_file_present = logged_in;

    let inferred_mode = match configured_mode {
        AuthMode::ApiKey => AuthMode::ApiKey,
        AuthMode::Subscription => AuthMode::Subscription,
        AuthMode::Auto => {
            if api_key_present {
                AuthMode::ApiKey
            } else if logged_in {
                AuthMode::Subscription
            } else {
                AuthMode::Auto
            }
        }
    };

    AuthStatus {
        provider: "claude".to_string(),
        configured_mode,
        inferred_mode,
        provider_available: crate::auth::find_command(COMMAND_NAME).is_some(),
        auth_file_path,
        auth_file_present,
        api_key_present,
        credentials_present: api_key_present || auth_file_present,
        docker_volume_hint: DOCKER_VOLUME_HINT,
    }
}

pub fn run_login(mode: AuthMode) -> Result<()> {
    let command = crate::auth::find_command(COMMAND_NAME)
        .ok_or_else(|| anyhow!("{}_not_found_in_path", COMMAND_NAME))?;

    match mode {
        AuthMode::Subscription => {
            let status = Command::new(command)
                .args(["auth", "login"])
                .stdin(Stdio::inherit())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .status()
                .context("failed to launch `claude auth login`")?;
            if !status.success() {
                return Err(anyhow!("claude_login_failed"));
            }
        }
        AuthMode::ApiKey => {
            let key = std::env::var(API_KEY_ENV)
                .with_context(|| format!("{API_KEY_ENV} is required for api_key login mode"))?;
            if key.trim().is_empty() {
                return Err(anyhow!("claude_api_key_missing"));
            }
        }
        AuthMode::Auto => return Err(anyhow!("auth_login_requires_explicit_mode")),
    }

    Ok(())
}

fn read_logged_in_status() -> Result<bool> {
    let command = match crate::auth::find_command(COMMAND_NAME) {
        Some(command) => command,
        None => return Ok(false),
    };

    let output = Command::new(command)
        .args(["auth", "status", "--json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .context("failed to inspect `claude auth status --json`")?;
    if !output.status.success() {
        return Ok(false);
    }

    let status = serde_json::from_slice::<ClaudeCliAuthStatus>(&output.stdout)
        .context("failed to parse `claude auth status --json`")?;
    Ok(status.logged_in)
}

fn auth_file_path() -> PathBuf {
    if let Some(home) = dirs::home_dir() {
        return home.join(AUTH_DIR_NAME);
    }

    PathBuf::from(AUTH_DIR_NAME)
}
