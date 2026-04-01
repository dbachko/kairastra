use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

use crate::auth::{AuthMode, AuthStatus};

pub const COMMAND_NAME: &str = "claude";
const AUTH_DIR_NAME: &str = ".claude";
const AUTH_FILE_NAME: &str = ".credentials.json";
const OAUTH_TOKEN_FILE_NAME: &str = "oauth-token";
const AUTH_MODE_ENV: &str = "CLAUDE_AUTH_MODE";
const API_KEY_ENV: &str = "ANTHROPIC_API_KEY";
pub const OAUTH_TOKEN_ENV: &str = "CLAUDE_CODE_OAUTH_TOKEN";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeCliAuthStatus {
    logged_in: bool,
}

/// Matches the `claudeAiOauth` section Claude Code writes/reads in `~/.claude/.credentials.json`.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeOAuthCredential {
    access_token: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    refresh_token: Option<String>,
    expires_at: u64, // Unix timestamp in **milliseconds**
    scopes: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeCredentialsFile {
    claude_ai_oauth: ClaudeOAuthCredential,
}

pub fn inspect_status() -> AuthStatus {
    let configured_mode = AuthMode::from_env_var(AUTH_MODE_ENV);
    let api_key_present = std::env::var(API_KEY_ENV)
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false);
    let oauth_token_env_present = read_non_empty_env(OAUTH_TOKEN_ENV).is_some();
    let oauth_token_file_present = oauth_token_file_path().is_file();
    let credentials_file_valid = read_credentials_file()
        .map(|c| !is_expired(c.claude_ai_oauth.expires_at))
        .unwrap_or(false);
    let oauth_token_present =
        oauth_token_env_present || oauth_token_file_present || credentials_file_valid;
    let effective_auth_path =
        effective_auth_path(oauth_token_env_present, oauth_token_file_present);
    let logged_in = read_logged_in_status().unwrap_or(false) || credentials_file_valid;
    let auth_file_present = auth_file_path().is_file() || oauth_token_present || logged_in;

    let inferred_mode = match configured_mode {
        AuthMode::ApiKey => AuthMode::ApiKey,
        AuthMode::Subscription => AuthMode::Subscription,
        AuthMode::Auto => {
            if api_key_present {
                AuthMode::ApiKey
            } else if oauth_token_present || logged_in {
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
        auth_file_path: effective_auth_path,
        auth_file_present,
        api_key_present,
        credentials_present: api_key_present || auth_file_present || logged_in,
    }
}

pub fn oauth_token() -> Option<String> {
    // Prefer the credentials file (what Claude Code reads natively) when it has a valid token.
    if let Some(creds) = read_credentials_file() {
        if !is_expired(creds.claude_ai_oauth.expires_at) {
            return Some(creds.claude_ai_oauth.access_token);
        }
    }
    read_non_empty_env(OAUTH_TOKEN_ENV).or_else(read_oauth_token_from_file)
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
        return home.join(AUTH_DIR_NAME).join(AUTH_FILE_NAME);
    }

    PathBuf::from(AUTH_DIR_NAME).join(AUTH_FILE_NAME)
}

fn oauth_token_file_path() -> PathBuf {
    if let Some(home) = dirs::home_dir() {
        return home.join(AUTH_DIR_NAME).join(OAUTH_TOKEN_FILE_NAME);
    }

    PathBuf::from(AUTH_DIR_NAME).join(OAUTH_TOKEN_FILE_NAME)
}

fn effective_auth_path(oauth_token_env_present: bool, oauth_token_file_present: bool) -> PathBuf {
    if oauth_token_file_present {
        oauth_token_file_path()
    } else if oauth_token_env_present {
        PathBuf::from(format!("${OAUTH_TOKEN_ENV}"))
    } else {
        auth_file_path()
    }
}

fn read_non_empty_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn read_oauth_token_from_file() -> Option<String> {
    std::fs::read_to_string(oauth_token_file_path())
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn is_expired(expires_at_ms: u64) -> bool {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    expires_at_ms < now.saturating_add(5 * 60 * 1_000)
}

fn credentials_file_path() -> PathBuf {
    auth_file_path() // ~/.claude/.credentials.json — same path
}

fn read_credentials_file() -> Option<ClaudeCredentialsFile> {
    let path = credentials_file_path();
    let data = std::fs::read_to_string(path).ok()?;
    serde_json::from_str::<ClaudeCredentialsFile>(&data).ok()
}
