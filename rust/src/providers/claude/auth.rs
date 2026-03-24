use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{anyhow, Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use dialoguer::{theme::ColorfulTheme, Input};
use getrandom::fill as getrandom_fill;
use reqwest::{blocking::Client, Url};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::auth::{AuthMode, AuthStatus};

pub const COMMAND_NAME: &str = "claude";
const AUTH_DIR_NAME: &str = ".claude";
const AUTH_FILE_NAME: &str = ".credentials.json";
const OAUTH_TOKEN_FILE_NAME: &str = "oauth-token";
const AUTH_MODE_ENV: &str = "CLAUDE_AUTH_MODE";
const API_KEY_ENV: &str = "ANTHROPIC_API_KEY";
pub const OAUTH_TOKEN_ENV: &str = "CLAUDE_CODE_OAUTH_TOKEN";
const OAUTH_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const OAUTH_AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
const OAUTH_TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
const OAUTH_REDIRECT_URI: &str = "https://platform.claude.com/oauth/code/callback";
const OAUTH_SCOPE: &str = "user:inference";
const DOCKER_VOLUME_HINT: &str =
    "Docker mode persists Claude auth through the symphony_rust_home and symphony_rust_claude volumes mounted for the non-root runtime user.";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeCliAuthStatus {
    logged_in: bool,
}

#[derive(Debug, Deserialize)]
struct OAuthExchangeResponse {
    access_token: String,
}

const SUBSCRIPTION_TOKEN_EXPIRES_IN_SECONDS: u64 = 31_536_000;

#[derive(Debug, Serialize)]
struct OAuthCodeExchangeRequest<'a> {
    grant_type: &'static str,
    code: &'a str,
    redirect_uri: &'static str,
    client_id: &'static str,
    code_verifier: &'a str,
    state: &'a str,
    expires_in: u64,
}

pub fn inspect_status() -> AuthStatus {
    let configured_mode = AuthMode::from_env_var(AUTH_MODE_ENV);
    let api_key_present = std::env::var(API_KEY_ENV)
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false);
    let oauth_token_env_present = read_non_empty_env(OAUTH_TOKEN_ENV).is_some();
    let oauth_token_file_present = oauth_token_file_path().is_file();
    let oauth_token_present = oauth_token_env_present || oauth_token_file_present;
    let effective_auth_path =
        effective_auth_path(oauth_token_env_present, oauth_token_file_present);
    let logged_in = read_logged_in_status().unwrap_or(false);
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
        docker_volume_hint: DOCKER_VOLUME_HINT,
    }
}

pub fn oauth_token() -> Option<String> {
    read_non_empty_env(OAUTH_TOKEN_ENV).or_else(read_oauth_token_from_file)
}

pub fn run_login(mode: AuthMode) -> Result<()> {
    let command = crate::auth::find_command(COMMAND_NAME)
        .ok_or_else(|| anyhow!("{}_not_found_in_path", COMMAND_NAME))?;

    match mode {
        AuthMode::Subscription => {
            if running_in_docker() {
                return run_docker_subscription_login(command);
            }

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

fn run_docker_subscription_login(_command: PathBuf) -> Result<()> {
    eprintln!("Generating a long-lived Claude subscription token for Docker...");
    let flow = DockerOAuthFlow::new()?;

    println!("Open this URL in your browser and complete Claude sign-in:\n");
    println!("{}", flow.authorize_url()?);
    println!();
    println!("After signing in, Claude will show an Authentication Code (a long string, ~40+ characters).");
    println!("Paste that code below. If Claude shows a full URL instead, paste the entire URL.");
    println!();

    let pasted_code: String = Input::with_theme(&ColorfulTheme::default())
        .with_prompt("Paste Authentication Code")
        .validate_with(|value: &String| {
            if value.trim().is_empty() {
                Err("Authentication code cannot be empty")
            } else {
                Ok(())
            }
        })
        .interact_text()?;

    let authorization_code = parse_authorization_code(&pasted_code, &flow.state)?;
    eprintln!("Exchanging Claude authentication code...");
    let token = exchange_auth_code(&flow, &authorization_code)?;
    persist_oauth_token(&token)?;
    eprintln!(
        "Saved Claude subscription token to {}.",
        oauth_token_file_path().display()
    );
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

fn running_in_docker() -> bool {
    matches!(
        std::env::var("SYMPHONY_DEPLOY_MODE"),
        Ok(value) if value.trim().eq_ignore_ascii_case("docker")
    )
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

fn persist_oauth_token(token: &str) -> Result<()> {
    let path = oauth_token_file_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    std::fs::write(&path, token).with_context(|| format!("failed to write {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let permissions = std::fs::Permissions::from_mode(0o600);
        std::fs::set_permissions(&path, permissions)
            .with_context(|| format!("failed to secure {}", path.display()))?;
    }

    Ok(())
}

fn parse_authorization_code(input: &str, expected_state: &str) -> Result<String> {
    let sanitized = input
        .replace("\u{1b}[200~", "")
        .replace("\u{1b}[201~", "")
        .chars()
        .filter(|ch| !ch.is_control() || *ch == '\n' || *ch == '\r' || *ch == '\t')
        .collect::<String>()
        .trim()
        .to_string();

    if sanitized.is_empty() {
        return Err(anyhow!("claude_auth_code_empty"));
    }

    if let Ok(url) = Url::parse(&sanitized) {
        if url.as_str().starts_with(OAUTH_AUTHORIZE_URL) {
            return Err(anyhow!(
                "claude_auth_code_is_authorize_url: paste the Authentication Code shown by Claude after browser sign-in, not the authorize URL"
            ));
        }

        if let Some(code) = url
            .query_pairs()
            .find_map(|(key, value)| (key == "code").then_some(value.into_owned()))
        {
            if code == "true" {
                return Err(anyhow!(
                    "claude_auth_code_is_authorize_url: paste the Authentication Code shown by Claude after browser sign-in, not the authorize URL"
                ));
            }
            if let Some(state) = url
                .query_pairs()
                .find_map(|(key, value)| (key == "state").then_some(value.into_owned()))
            {
                validate_oauth_state(&state, expected_state)?;
            }
            return Ok(code);
        }
    }

    let (code, state) = match sanitized.split_once('#') {
        Some((code, state)) => (code.trim(), Some(state.trim())),
        None => (sanitized.as_str(), None),
    };
    if let Some(state) = state {
        validate_oauth_state(state, expected_state)?;
    }
    if code.is_empty() {
        return Err(anyhow!("claude_auth_code_empty"));
    }

    Ok(code.to_string())
}

fn validate_oauth_state(actual: &str, expected: &str) -> Result<()> {
    if actual == expected {
        Ok(())
    } else {
        Err(anyhow!(
            "claude_auth_code_state_mismatch: expected {}, got {}",
            expected,
            actual
        ))
    }
}

fn exchange_auth_code(flow: &DockerOAuthFlow, authorization_code: &str) -> Result<String> {
    let request = oauth_code_exchange_request(flow, authorization_code);

    let response = oauth_client()?
        .post(OAUTH_TOKEN_URL)
        .json(&request)
        .send()
        .context("failed to exchange Claude OAuth code")?;

    let payload =
        parse_oauth_json_response::<OAuthExchangeResponse>(response, "claude_oauth_code_exchange")?;
    Ok(payload.access_token)
}

fn oauth_code_exchange_request<'a>(
    flow: &'a DockerOAuthFlow,
    authorization_code: &'a str,
) -> OAuthCodeExchangeRequest<'a> {
    OAuthCodeExchangeRequest {
        grant_type: "authorization_code",
        code: authorization_code,
        redirect_uri: OAUTH_REDIRECT_URI,
        client_id: OAUTH_CLIENT_ID,
        code_verifier: flow.code_verifier.as_str(),
        state: flow.state.as_str(),
        expires_in: SUBSCRIPTION_TOKEN_EXPIRES_IN_SECONDS,
    }
}

fn oauth_client() -> Result<Client> {
    Client::builder()
        .user_agent("symphony-rust/0.1")
        .build()
        .context("failed to build Claude OAuth client")
}

fn parse_oauth_json_response<T: for<'de> Deserialize<'de>>(
    response: reqwest::blocking::Response,
    context: &str,
) -> Result<T> {
    let status = response.status();
    let body = response
        .text()
        .with_context(|| format!("{context}: failed to read response body"))?;

    if !status.is_success() {
        return Err(anyhow!(
            "{context}_failed: http_status={status}; body={body}"
        ));
    }

    serde_json::from_str::<T>(&body)
        .with_context(|| format!("{context}: failed to parse response body"))
}

struct DockerOAuthFlow {
    state: String,
    code_verifier: String,
    code_challenge: String,
}

impl DockerOAuthFlow {
    fn new() -> Result<Self> {
        let state = random_urlsafe_token(32)?;
        let code_verifier = random_urlsafe_token(48)?;
        let code_challenge = pkce_challenge(&code_verifier);

        Ok(Self {
            state,
            code_verifier,
            code_challenge,
        })
    }

    fn authorize_url(&self) -> Result<Url> {
        let mut url = Url::parse(OAUTH_AUTHORIZE_URL)
            .context("failed to parse Claude OAuth authorize URL")?;
        url.query_pairs_mut()
            .append_pair("code", "true")
            .append_pair("client_id", OAUTH_CLIENT_ID)
            .append_pair("response_type", "code")
            .append_pair("redirect_uri", OAUTH_REDIRECT_URI)
            .append_pair("scope", OAUTH_SCOPE)
            .append_pair("code_challenge", &self.code_challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("state", &self.state);
        Ok(url)
    }
}

fn random_urlsafe_token(byte_len: usize) -> Result<String> {
    let mut bytes = vec![0_u8; byte_len];
    getrandom_fill(&mut bytes)
        .map_err(|error| anyhow!("failed to generate random OAuth token bytes: {error}"))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn pkce_challenge(code_verifier: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(code_verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        oauth_code_exchange_request, parse_authorization_code, validate_oauth_state,
        DockerOAuthFlow,
    };

    #[test]
    fn parses_code_and_state_from_pasted_fragment() {
        let code = parse_authorization_code(
            "\u{1b}[200~abc123#expected-state\u{1b}[201~",
            "expected-state",
        )
        .unwrap();

        assert_eq!(code, "abc123");
    }

    #[test]
    fn parses_code_and_state_from_callback_url() {
        let code = parse_authorization_code(
            "https://platform.claude.com/oauth/code/callback?code=abc123&state=expected-state",
            "expected-state",
        )
        .unwrap();

        assert_eq!(code, "abc123");
    }

    #[test]
    fn rejects_authorize_url_instead_of_authentication_code() {
        let error = parse_authorization_code(
            "https://claude.ai/oauth/authorize?code=true&client_id=client-id&state=expected-state",
            "expected-state",
        )
        .unwrap_err();

        assert_eq!(
            error.to_string(),
            "claude_auth_code_is_authorize_url: paste the Authentication Code shown by Claude after browser sign-in, not the authorize URL"
        );
    }

    #[test]
    fn rejects_mismatched_state() {
        let error = validate_oauth_state("wrong-state", "expected-state").unwrap_err();

        assert_eq!(
            error.to_string(),
            "claude_auth_code_state_mismatch: expected expected-state, got wrong-state"
        );
    }

    #[test]
    fn authorize_url_contains_pkce_and_state() {
        let flow = DockerOAuthFlow::new().unwrap();
        let url = flow.authorize_url().unwrap();
        let query = url.query_pairs().collect::<Vec<_>>();

        assert!(query
            .iter()
            .any(|(key, value)| key == "client_id" && !value.is_empty()));
        assert!(query
            .iter()
            .any(|(key, value)| key == "code_challenge" && !value.is_empty()));
        assert!(query
            .iter()
            .any(|(key, value)| key == "state" && value == flow.state.as_str()));
    }

    #[test]
    fn serializes_code_exchange_request_with_pkce_fields() {
        let flow = DockerOAuthFlow {
            state: "expected-state".to_string(),
            code_verifier: "code-verifier".to_string(),
            code_challenge: "code-challenge".to_string(),
        };
        let request = oauth_code_exchange_request(&flow, "abc123");

        assert_eq!(
            serde_json::to_value(request).unwrap(),
            json!({
                "grant_type": "authorization_code",
                "code": "abc123",
                "redirect_uri": "https://platform.claude.com/oauth/code/callback",
                "client_id": "9d1c250a-e61b-44d9-88ed-5944d1962f5e",
                "code_verifier": "code-verifier",
                "state": "expected-state",
                "expires_in": 31536000,
            })
        );
    }
}
