use std::fmt;
use std::path::PathBuf;

use anyhow::{anyhow, Result};
use dialoguer::{theme::ColorfulTheme, Confirm, Select};
use serde::Serialize;

use crate::providers;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuthMode {
    Auto,
    ApiKey,
    Subscription,
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
            "subscription" => Self::Subscription,
            _ => Self::Auto,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::ApiKey => "api_key",
            Self::Subscription => "subscription",
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
    pub api_key_present: bool,
    pub credentials_present: bool,
    pub docker_volume_hint: &'static str,
}

pub fn inspect_status(provider: &str) -> Result<AuthStatus> {
    providers::inspect_auth_status(provider)
}

pub fn run_login(provider: &str, mode: AuthMode) -> Result<()> {
    providers::run_login(provider, mode)
}

pub fn run_login_menu(provider: Option<&str>) -> Result<()> {
    if let Some(provider) = provider.map(str::trim).filter(|value| !value.is_empty()) {
        return handle_login_selection(provider);
    }

    let entries = providers::setup_provider_choices()
        .iter()
        .map(|(provider, display_name)| {
            let status = inspect_status(provider)?;
            Ok(AuthMenuEntry {
                display_name,
                label: provider_menu_label(display_name, &status),
                status,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    let labels = entries
        .iter()
        .map(|entry| entry.label.as_str())
        .collect::<Vec<_>>();
    let default_index = entries
        .iter()
        .position(|entry| !entry.status.credentials_present)
        .unwrap_or(0);

    let selection = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("Select provider auth")
        .default(default_index)
        .items(&labels)
        .interact()?;

    handle_login_action(&entries[selection].status, entries[selection].display_name)
}

fn handle_login_selection(provider: &str) -> Result<()> {
    let display_name = provider_display_name(provider);
    let status = inspect_status(provider)?;
    handle_login_action(&status, display_name)
}

fn handle_login_action(status: &AuthStatus, display_name: &str) -> Result<()> {
    match login_action(status) {
        LoginAction::Subscription => run_login(&status.provider, AuthMode::Subscription),
        LoginAction::AlreadyLoggedIn => {
            let rerun = Confirm::with_theme(&ColorfulTheme::default())
                .with_prompt(format!(
                    "{display_name} already has a saved login. Run login again?"
                ))
                .default(false)
                .interact()?;
            if rerun {
                run_login(&status.provider, AuthMode::Subscription)
            } else {
                Ok(())
            }
        }
        LoginAction::ApiKeyReady => {
            if status.auth_file_present {
                println!(
                    "{display_name} has both a saved login and {}. Because the provider is in auto mode, the API key currently takes precedence.",
                    api_key_env_name(&status.provider)
                );
            } else {
                println!(
                    "{display_name} is already ready via {}. No interactive login is required.",
                    api_key_env_name(&status.provider)
                );
            }
            Ok(())
        }
        LoginAction::DockerApiKeyRecommended => {
            println!(
                "{display_name} in Docker should use API-key auth. Set CLAUDE_AUTH_MODE=api_key and {} in rust/.env, then rerun `make docker-up`.",
                api_key_env_name(&status.provider)
            );
            let try_browser_login = Confirm::with_theme(&ColorfulTheme::default())
                .with_prompt("Try the browser-based Claude login anyway?")
                .default(false)
                .interact()?;
            if try_browser_login {
                run_login(&status.provider, AuthMode::Subscription)
            } else {
                Ok(())
            }
        }
        LoginAction::NeedsApiKey => Err(anyhow!(
            "{} is configured for API-key auth. Set {} and rerun doctor.",
            display_name,
            api_key_env_name(&status.provider)
        )),
        LoginAction::ProviderUnavailable => Err(anyhow!(
            "{} CLI is not available on PATH in this environment.",
            display_name
        )),
    }
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

fn provider_menu_label(display_name: &str, status: &AuthStatus) -> String {
    match login_action(status) {
        LoginAction::AlreadyLoggedIn => format!("✓ {display_name} (logged in)"),
        LoginAction::ApiKeyReady => {
            if status.auth_file_present {
                format!(
                    "✓ {display_name} (ready via {}; saved login also present)",
                    api_key_env_name(&status.provider)
                )
            } else {
                format!(
                    "✓ {display_name} (ready via {})",
                    api_key_env_name(&status.provider)
                )
            }
        }
        LoginAction::DockerApiKeyRecommended => format!(
            "Use {} for {display_name} (recommended in Docker)",
            api_key_env_name(&status.provider)
        ),
        LoginAction::Subscription => format!("Log in to {display_name}"),
        LoginAction::NeedsApiKey => format!(
            "Set {} for {display_name}",
            api_key_env_name(&status.provider)
        ),
        LoginAction::ProviderUnavailable => format!("Install {display_name} CLI"),
    }
}

fn provider_display_name(provider: &str) -> &'static str {
    match provider {
        "claude" => "Claude Code",
        "codex" => "Codex",
        _ => "Provider",
    }
}

fn api_key_env_name(provider: &str) -> &'static str {
    match provider {
        "claude" => "ANTHROPIC_API_KEY",
        "codex" => "OPENAI_API_KEY",
        _ => "API_KEY",
    }
}

fn login_action(status: &AuthStatus) -> LoginAction {
    if !status.provider_available {
        return LoginAction::ProviderUnavailable;
    }

    if status.api_key_present && matches!(status.inferred_mode, AuthMode::ApiKey) {
        return LoginAction::ApiKeyReady;
    }

    if status.auth_file_present {
        return LoginAction::AlreadyLoggedIn;
    }

    if status.provider == "claude" && running_in_docker() {
        return LoginAction::DockerApiKeyRecommended;
    }

    match status.configured_mode {
        AuthMode::ApiKey => LoginAction::NeedsApiKey,
        AuthMode::Auto | AuthMode::Subscription => LoginAction::Subscription,
    }
}

fn running_in_docker() -> bool {
    matches!(
        std::env::var("SYMPHONY_DEPLOY_MODE"),
        Ok(value) if value.trim().eq_ignore_ascii_case("docker")
    )
}

struct AuthMenuEntry {
    display_name: &'static str,
    label: String,
    status: AuthStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LoginAction {
    Subscription,
    AlreadyLoggedIn,
    ApiKeyReady,
    DockerApiKeyRecommended,
    NeedsApiKey,
    ProviderUnavailable,
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Mutex;

    use super::{
        inspect_status, login_action, provider_menu_label, AuthMode, AuthStatus, LoginAction,
    };

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn auto_mode_prefers_api_key_when_present() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::set_var("CODEX_AUTH_MODE", "auto");
        std::env::set_var("OPENAI_API_KEY", "test-key");
        let status = inspect_status("codex").unwrap();
        assert_eq!(status.inferred_mode, AuthMode::ApiKey);
        assert!(status.api_key_present);
        assert!(status.credentials_present);
        std::env::remove_var("OPENAI_API_KEY");
    }

    fn status(provider: &str) -> AuthStatus {
        AuthStatus {
            provider: provider.to_string(),
            configured_mode: AuthMode::Auto,
            inferred_mode: AuthMode::Subscription,
            provider_available: true,
            auth_file_path: PathBuf::from(format!("/tmp/{provider}")),
            auth_file_present: false,
            api_key_present: false,
            credentials_present: false,
            docker_volume_hint: "hint",
        }
    }

    #[test]
    fn login_action_prefers_api_key_when_present() {
        let mut status = status("claude");
        status.inferred_mode = AuthMode::ApiKey;
        status.api_key_present = true;
        status.credentials_present = true;

        assert_eq!(login_action(&status), LoginAction::ApiKeyReady);
    }

    #[test]
    fn login_action_requires_api_key_in_api_key_mode() {
        let mut status = status("codex");
        status.configured_mode = AuthMode::ApiKey;

        assert_eq!(login_action(&status), LoginAction::NeedsApiKey);
    }

    #[test]
    fn provider_menu_label_marks_logged_in_status() {
        let mut status = status("claude");
        status.auth_file_present = true;
        status.credentials_present = true;

        assert_eq!(
            provider_menu_label("Claude Code", &status),
            "✓ Claude Code (logged in)"
        );
    }

    #[test]
    fn provider_menu_label_marks_missing_status_as_login() {
        let status = status("codex");

        assert_eq!(provider_menu_label("Codex", &status), "Log in to Codex");
    }

    #[test]
    fn provider_menu_label_mentions_saved_login_when_api_key_also_present() {
        let mut status = status("codex");
        status.inferred_mode = AuthMode::ApiKey;
        status.api_key_present = true;
        status.auth_file_present = true;
        status.credentials_present = true;

        assert_eq!(
            provider_menu_label("Codex", &status),
            "✓ Codex (ready via OPENAI_API_KEY; saved login also present)"
        );
    }

    #[test]
    fn login_action_recommends_api_key_for_claude_in_docker() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::set_var("SYMPHONY_DEPLOY_MODE", "docker");
        let status = status("claude");

        assert_eq!(login_action(&status), LoginAction::DockerApiKeyRecommended);
        std::env::remove_var("SYMPHONY_DEPLOY_MODE");
    }
}
