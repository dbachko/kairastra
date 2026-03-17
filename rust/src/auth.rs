use std::fmt;
use std::path::PathBuf;

use anyhow::Result;
use serde::Serialize;

use crate::providers;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuthMode {
    Auto,
    ApiKey,
    Chatgpt,
}

impl AuthMode {
    pub fn from_env_var(name: &str) -> Self {
        match std::env::var(name)
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

#[derive(Debug, Clone, Serialize)]
pub struct AuthStatus {
    pub provider: String,
    pub configured_mode: AuthMode,
    pub inferred_mode: AuthMode,
    pub provider_available: bool,
    pub auth_file_path: PathBuf,
    pub auth_file_present: bool,
    pub openai_api_key_present: bool,
    pub docker_volume_hint: &'static str,
}

pub fn inspect_status(provider: &str) -> Result<AuthStatus> {
    providers::inspect_auth_status(provider)
}

pub fn run_login(provider: &str, mode: AuthMode) -> Result<()> {
    providers::run_login(provider, mode)
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

#[cfg(test)]
mod tests {
    use super::{inspect_status, AuthMode};

    #[test]
    fn auto_mode_prefers_api_key_when_present() {
        std::env::set_var("CODEX_AUTH_MODE", "auto");
        std::env::set_var("OPENAI_API_KEY", "test-key");
        let status = inspect_status("codex").unwrap();
        assert_eq!(status.inferred_mode, AuthMode::ApiKey);
        std::env::remove_var("OPENAI_API_KEY");
    }
}
