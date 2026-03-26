use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, SystemTime};

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;

use crate::auth::{AuthMode, AuthStatus};

pub const COMMAND_NAME: &str = "gemini";
const AUTH_DIR_NAME: &str = ".gemini";
const AUTH_FILE_NAME: &str = "oauth_creds.json";
const SETTINGS_FILE_NAME: &str = "settings.json";
const AUTH_MODE_ENV: &str = "GEMINI_AUTH_MODE";
const SUBSCRIPTION_AUTH_TYPES: &[&str] = &["oauth-personal", "google", "login_with_google"];
const API_KEY_AUTH_TYPE: &str = "gemini-api-key";
const GEMINI_LOGIN_POLL_INTERVAL: Duration = Duration::from_millis(250);
const GEMINI_LOGIN_SETTLE_DELAY: Duration = Duration::from_millis(750);
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct GeminiLoginState {
    auth_file_present: bool,
    auth_file_size: Option<u64>,
    auth_file_modified: Option<SystemTime>,
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
            if api_key_present || selected_type.as_deref() == Some(API_KEY_AUTH_TYPE) {
                AuthMode::ApiKey
            } else if auth_file_present || is_subscription_selected_type(selected_type.as_deref()) {
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
            let baseline = read_login_state();
            eprintln!(
                "Gemini opens its own interactive auth UI. Kairastra will close it once the login is saved."
            );
            eprintln!("If you are re-authenticating over an existing login, type /quit when you are done.");

            let mut child = Command::new(command)
                .args(["--prompt-interactive", "/auth"])
                .env_remove("GEMINI_API_KEY")
                .env_remove("GOOGLE_API_KEY")
                .stdin(Stdio::inherit())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .spawn()
                .context("failed to launch `gemini --prompt-interactive /auth`")?;

            loop {
                if login_completed_since(&baseline, &read_login_state()) {
                    eprintln!();
                    eprintln!("Gemini login saved. Closing Gemini CLI...");
                    thread::sleep(GEMINI_LOGIN_SETTLE_DELAY);
                    let _ = child.kill();
                    let _ = child.wait();
                    restore_terminal_after_forced_exit();
                    break;
                }

                if let Some(status) = child.try_wait().context("failed to poll Gemini login")? {
                    if !status.success() && !login_completed_since(&baseline, &read_login_state()) {
                        return Err(anyhow!("gemini_login_failed"));
                    }
                    break;
                }

                thread::sleep(GEMINI_LOGIN_POLL_INTERVAL);
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

fn read_login_state() -> GeminiLoginState {
    let auth_path = auth_file_path();
    let auth_metadata = fs::metadata(&auth_path)
        .ok()
        .filter(|metadata| metadata.is_file());

    GeminiLoginState {
        auth_file_present: auth_metadata.is_some(),
        auth_file_size: auth_metadata.as_ref().map(|metadata| metadata.len()),
        auth_file_modified: auth_metadata
            .as_ref()
            .and_then(|metadata| metadata.modified().ok()),
        selected_type: read_selected_auth_type(),
    }
}

fn login_completed_since(baseline: &GeminiLoginState, current: &GeminiLoginState) -> bool {
    current != baseline
        && current.auth_file_present
        && current.auth_file_size.unwrap_or(0) > 0
        && is_subscription_selected_type(current.selected_type.as_deref())
}

fn is_subscription_selected_type(selected_type: Option<&str>) -> bool {
    SUBSCRIPTION_AUTH_TYPES.contains(&selected_type.unwrap_or_default())
}

fn restore_terminal_after_forced_exit() {
    let _ = io::stderr().write_all(b"\x1b[?1049l\x1b[?25h\x1b[0m\r\n");
    let _ = io::stderr().flush();
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
    use std::time::{Duration, Instant};

    use super::{
        inspect_status, is_subscription_selected_type, login_completed_since, read_login_state,
        run_login,
    };
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

    #[test]
    fn supports_legacy_google_selected_type() {
        assert!(is_subscription_selected_type(Some("oauth-personal")));
        assert!(is_subscription_selected_type(Some("google")));
        assert!(is_subscription_selected_type(Some("login_with_google")));
        assert!(!is_subscription_selected_type(Some("gemini-api-key")));
    }

    #[test]
    fn login_completion_requires_changed_subscription_state() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let home_dir = dir.path().join("home");
        let gemini_dir = home_dir.join(".gemini");
        fs::create_dir_all(&gemini_dir).unwrap();

        let _home = EnvVarGuard::set("HOME", home_dir.as_os_str().into());
        let baseline = read_login_state();

        fs::write(gemini_dir.join("oauth_creds.json"), "{\"token\":\"abc\"}").unwrap();
        fs::write(
            gemini_dir.join("settings.json"),
            r#"{"security":{"auth":{"selectedType":"oauth-personal"}}}"#,
        )
        .unwrap();

        let current = read_login_state();
        assert!(login_completed_since(&baseline, &current));
    }

    #[test]
    #[cfg(unix)]
    fn login_returns_after_new_credentials_are_written() {
        use std::os::unix::fs::PermissionsExt;

        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let bin_dir = dir.path().join("bin");
        let home_dir = dir.path().join("home");
        let gemini_dir = home_dir.join(".gemini");
        fs::create_dir_all(&bin_dir).unwrap();
        fs::create_dir_all(&gemini_dir).unwrap();

        let script_path = bin_dir.join("gemini");
        fs::write(
            &script_path,
            r#"#!/bin/sh
set -eu
mkdir -p "$HOME/.gemini"
sleep 1
cat > "$HOME/.gemini/oauth_creds.json" <<'EOF'
{"token":"abc"}
EOF
cat > "$HOME/.gemini/settings.json" <<'EOF'
{"security":{"auth":{"selectedType":"oauth-personal"}}}
EOF
sleep 30
"#,
        )
        .unwrap();

        let mut perms = fs::metadata(&script_path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&script_path, perms).unwrap();

        let original_path = std::env::var_os("PATH").unwrap_or_default();
        let mut paths = vec![bin_dir.clone()];
        paths.extend(std::env::split_paths(&original_path));
        let composed_path = std::env::join_paths(paths).unwrap();

        let _home = EnvVarGuard::set("HOME", home_dir.as_os_str().into());
        let _path = EnvVarGuard::set("PATH", composed_path);
        let _api_key = EnvVarGuard::unset("GEMINI_API_KEY");
        let _google_api_key = EnvVarGuard::unset("GOOGLE_API_KEY");

        let start = Instant::now();
        run_login(AuthMode::Subscription).unwrap();
        assert!(start.elapsed() < Duration::from_secs(10));

        let status = inspect_status();
        assert_eq!(status.inferred_mode, AuthMode::Subscription);
        assert!(status.auth_file_present);
    }
}
