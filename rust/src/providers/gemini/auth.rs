use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;

use crate::auth::{AuthMode, AuthStatus};

pub const COMMAND_NAME: &str = "gemini";
const AUTH_DIR_NAME: &str = ".gemini";
const AUTH_FILE_NAME: &str = "oauth_creds.json";
const SETTINGS_FILE_NAME: &str = "settings.json";
const AUTH_MODE_ENV: &str = "GEMINI_AUTH_MODE";
const DOCKER_VOLUME_HINT: &str =
    "Docker mode persists Gemini auth through the kairastra_home and kairastra_gemini volumes mounted for the non-root runtime user.";

#[derive(Debug, Deserialize)]
struct GeminiSettingsFile {
    security: Option<GeminiSecurityConfig>,
}

#[derive(Debug, Deserialize)]
struct GeminiSecurityConfig {
    auth: Option<GeminiAuthConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiAuthConfig {
    selected_type: Option<String>,
}

pub fn inspect_status() -> AuthStatus {
    let configured_mode = AuthMode::from_env_var(AUTH_MODE_ENV);
    let api_key_present = gemini_api_key_present();
    let auth_file_path = auth_file_path();
    let auth_file_present = auth_file_path.is_file();
    let selected_type = read_selected_auth_type();

    let inferred_mode = match configured_mode {
        AuthMode::ApiKey => AuthMode::ApiKey,
        AuthMode::Subscription => AuthMode::Subscription,
        AuthMode::Auto => {
            if api_key_present || selected_type.as_deref() == Some("gemini-api-key") {
                AuthMode::ApiKey
            } else if auth_file_present || selected_type.as_deref() == Some("oauth-personal") {
                AuthMode::Subscription
            } else {
                AuthMode::Auto
            }
        }
    };

    AuthStatus {
        provider: "gemini".to_string(),
        configured_mode,
        inferred_mode,
        provider_available: crate::auth::find_command(COMMAND_NAME).is_some(),
        auth_file_path,
        auth_file_present,
        api_key_present,
        credentials_present: auth_file_present || api_key_present,
        docker_volume_hint: DOCKER_VOLUME_HINT,
    }
}

pub fn run_login(mode: AuthMode) -> Result<()> {
    let command = crate::auth::find_command(COMMAND_NAME)
        .ok_or_else(|| anyhow!("{}_not_found_in_path", COMMAND_NAME))?;

    match mode {
        AuthMode::Subscription => {
            let status = Command::new(command)
                .args(["--prompt-interactive", "/auth"])
                .env_remove("GEMINI_API_KEY")
                .env_remove("GOOGLE_API_KEY")
                .stdin(Stdio::inherit())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .status()
                .context("failed to launch `gemini --prompt-interactive /auth`")?;
            if !status.success() {
                return Err(anyhow!("gemini_login_failed"));
            }
        }
        AuthMode::ApiKey => {
            if !gemini_api_key_present() {
                return Err(anyhow!("gemini_api_key_missing"));
            }
        }
        AuthMode::Auto => return Err(anyhow!("auth_login_requires_explicit_mode")),
    }

    Ok(())
}

fn auth_file_path() -> PathBuf {
    if let Some(home) = dirs::home_dir() {
        return home.join(AUTH_DIR_NAME).join(AUTH_FILE_NAME);
    }

    PathBuf::from(AUTH_DIR_NAME).join(AUTH_FILE_NAME)
}

fn settings_file_path() -> PathBuf {
    if let Some(home) = dirs::home_dir() {
        return home.join(AUTH_DIR_NAME).join(SETTINGS_FILE_NAME);
    }

    PathBuf::from(AUTH_DIR_NAME).join(SETTINGS_FILE_NAME)
}

fn read_selected_auth_type() -> Option<String> {
    let contents = std::fs::read_to_string(settings_file_path()).ok()?;
    let parsed = serde_json::from_str::<GeminiSettingsFile>(&contents).ok()?;
    parsed
        .security
        .and_then(|security| security.auth)
        .and_then(|auth| auth.selected_type)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn gemini_api_key_present() -> bool {
    ["GEMINI_API_KEY", "GOOGLE_API_KEY"].iter().any(|name| {
        std::env::var(name)
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false)
    })
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::fs;
    use std::sync::Mutex;

    use super::inspect_status;
    use crate::auth::AuthMode;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvVarGuard {
        key: &'static str,
        original: Option<OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: OsString) -> Self {
            let original = std::env::var_os(key);
            std::env::set_var(key, value);
            Self { key, original }
        }

        fn unset(key: &'static str) -> Self {
            let original = std::env::var_os(key);
            std::env::remove_var(key);
            Self { key, original }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(value) = self.original.take() {
                std::env::set_var(self.key, value);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    #[test]
    fn auto_mode_prefers_api_key_when_present() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _mode = EnvVarGuard::set("GEMINI_AUTH_MODE", OsString::from("auto"));
        let _key = EnvVarGuard::set("GEMINI_API_KEY", OsString::from("test-key"));

        let status = inspect_status();
        assert_eq!(status.inferred_mode, AuthMode::ApiKey);
        assert!(status.api_key_present);
        assert!(status.credentials_present);
    }

    #[test]
    fn oauth_file_counts_as_subscription_credentials() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let home_dir = dir.path().join("home");
        let gemini_dir = home_dir.join(".gemini");
        fs::create_dir_all(&gemini_dir).unwrap();
        fs::write(gemini_dir.join("oauth_creds.json"), "{}").unwrap();
        fs::write(
            gemini_dir.join("settings.json"),
            r#"{"security":{"auth":{"selectedType":"oauth-personal"}}}"#,
        )
        .unwrap();

        let _home = EnvVarGuard::set("HOME", home_dir.as_os_str().into());
        let _mode = EnvVarGuard::set("GEMINI_AUTH_MODE", OsString::from("auto"));
        let _api_key = EnvVarGuard::unset("GEMINI_API_KEY");
        let _google_api_key = EnvVarGuard::unset("GOOGLE_API_KEY");

        let status = inspect_status();
        assert_eq!(status.inferred_mode, AuthMode::Subscription);
        assert!(status.auth_file_present);
        assert!(status.credentials_present);
    }
}
