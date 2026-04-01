use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{anyhow, Context, Result};

use crate::auth::{AuthMode, AuthStatus};

pub const COMMAND_NAME: &str = "codex";
const AUTH_DIR_NAME: &str = ".codex";
const AUTH_MODE_ENV: &str = "CODEX_AUTH_MODE";

pub fn inspect_status() -> AuthStatus {
    let configured_mode = AuthMode::from_env_var(AUTH_MODE_ENV);
    let api_key_present = std::env::var("OPENAI_API_KEY")
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false);
    let auth_file_path = auth_file_path();
    let auth_file_present = auth_file_path.is_file();

    let inferred_mode = match configured_mode {
        AuthMode::ApiKey => AuthMode::ApiKey,
        AuthMode::Subscription => AuthMode::Subscription,
        AuthMode::Auto => {
            if api_key_present {
                AuthMode::ApiKey
            } else {
                AuthMode::Subscription
            }
        }
    };

    AuthStatus {
        provider: "codex".to_string(),
        configured_mode,
        inferred_mode,
        provider_available: crate::auth::find_command(COMMAND_NAME).is_some(),
        auth_file_path,
        auth_file_present,
        api_key_present,
        credentials_present: auth_file_present || api_key_present,
    }
}

pub fn run_login(mode: AuthMode) -> Result<()> {
    let command = crate::auth::find_command(COMMAND_NAME)
        .ok_or_else(|| anyhow!("{}_not_found_in_path", COMMAND_NAME))?;

    match mode {
        AuthMode::Subscription => {
            let mut login = Command::new(command);
            login.arg("login");
            let status = login
                .stdin(Stdio::inherit())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .status()
                .context("failed to launch `codex login`")?;
            if !status.success() {
                return Err(anyhow!("codex_login_failed"));
            }
        }
        AuthMode::ApiKey => {
            let key = std::env::var("OPENAI_API_KEY")
                .context("OPENAI_API_KEY is required for api_key login mode")?;
            let mut child = Command::new(command)
                .arg("login")
                .arg("--with-api-key")
                .stdin(Stdio::piped())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .spawn()
                .context("failed to launch `codex login --with-api-key`")?;
            if let Some(stdin) = child.stdin.as_mut() {
                stdin.write_all(key.as_bytes())?;
            }
            let status = child.wait()?;
            if !status.success() {
                return Err(anyhow!("codex_api_key_login_failed"));
            }
        }
        AuthMode::Auto => return Err(anyhow!("auth_login_requires_explicit_mode")),
    }

    Ok(())
}

fn auth_file_path() -> PathBuf {
    if let Some(home) = dirs::home_dir() {
        return home.join(AUTH_DIR_NAME).join("auth.json");
    }

    PathBuf::from(AUTH_DIR_NAME).join("auth.json")
}
