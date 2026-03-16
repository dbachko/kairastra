use std::fmt;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{anyhow, Context, Result};
use serde::Serialize;

use crate::config::AgentProvider;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuthMode {
    Auto,
    ApiKey,
    Chatgpt,
}

impl AuthMode {
    pub fn from_env() -> Self {
        match std::env::var("CODEX_AUTH_MODE")
            .unwrap_or_else(|_| "auto".to_string())
            .trim()
            .to_lowercase()
            .as_str()
        {
            "api_key" => Self::ApiKey,
            "chatgpt" => Self::Chatgpt,
            _ => Self::Auto,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::ApiKey => "api_key",
            Self::Chatgpt => "chatgpt",
        }
    }
}

impl fmt::Display for AuthMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuthProvider {
    Codex,
}

impl AuthProvider {
    pub fn from_agent_provider(provider: AgentProvider) -> Result<Self> {
        match provider {
            AgentProvider::Codex => Ok(Self::Codex),
            other => Err(anyhow!("unsupported_auth_provider: {}", other.as_str())),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Codex => "codex",
        }
    }

    pub fn command_name(self) -> &'static str {
        match self {
            Self::Codex => "codex",
        }
    }

    fn auth_dir_name(self) -> &'static str {
        match self {
            Self::Codex => ".codex",
        }
    }

    pub fn auth_file_path(self) -> PathBuf {
        auth_file_path(self.auth_dir_name())
    }

    pub fn docker_volume_hint(self) -> &'static str {
        match self {
            Self::Codex => {
                "Docker mode persists Codex auth inside the symphony_rust_codex volume mounted at /root/.codex in the container."
            }
        }
    }
}

impl fmt::Display for AuthProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct AuthStatus {
    pub provider: AuthProvider,
    pub configured_mode: AuthMode,
    pub inferred_mode: AuthMode,
    pub provider_available: bool,
    pub auth_file_path: PathBuf,
    pub auth_file_present: bool,
    pub openai_api_key_present: bool,
    pub docker_volume_hint: &'static str,
}

pub fn inspect_status(provider: AuthProvider) -> AuthStatus {
    let configured_mode = AuthMode::from_env();
    let openai_api_key_present = std::env::var("OPENAI_API_KEY")
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false);
    let auth_file_path = provider.auth_file_path();
    let auth_file_present = auth_file_path.is_file();

    let inferred_mode = match configured_mode {
        AuthMode::ApiKey => AuthMode::ApiKey,
        AuthMode::Chatgpt => AuthMode::Chatgpt,
        AuthMode::Auto => {
            if openai_api_key_present {
                AuthMode::ApiKey
            } else {
                AuthMode::Chatgpt
            }
        }
    };

    AuthStatus {
        provider,
        configured_mode,
        inferred_mode,
        provider_available: find_command(provider.command_name()).is_some(),
        auth_file_path,
        auth_file_present,
        openai_api_key_present,
        docker_volume_hint: provider.docker_volume_hint(),
    }
}

pub fn run_login(provider: AuthProvider, mode: AuthMode) -> Result<()> {
    let command = find_command(provider.command_name())
        .ok_or_else(|| anyhow!("{}_not_found_in_path", provider.as_str()))?;

    match provider {
        AuthProvider::Codex => match mode {
            AuthMode::Chatgpt => {
                let status = Command::new(command)
                    .arg("login")
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
        },
    }

    Ok(())
}

pub fn find_command(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;

    for entry in std::env::split_paths(&path_var) {
        let candidate = entry.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    None
}

fn auth_file_path(dir_name: &str) -> PathBuf {
    if let Some(home) = dirs::home_dir() {
        return home.join(dir_name).join("auth.json");
    }

    PathBuf::from(dir_name).join("auth.json")
}

#[cfg(test)]
mod tests {
    use super::{inspect_status, AuthMode, AuthProvider};

    #[test]
    fn auto_mode_prefers_api_key_when_present() {
        std::env::set_var("CODEX_AUTH_MODE", "auto");
        std::env::set_var("OPENAI_API_KEY", "test-key");
        let status = inspect_status(AuthProvider::Codex);
        assert_eq!(status.inferred_mode, AuthMode::ApiKey);
        std::env::remove_var("OPENAI_API_KEY");
    }
}
