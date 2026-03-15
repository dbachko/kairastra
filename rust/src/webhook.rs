use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use hmac::{Hmac, Mac};
use serde_json::json;
use sha2::Sha256;
use tokio::net::TcpListener;
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tracing::info;

use crate::config::WebhookSettings;

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone)]
struct WebhookState {
    secret: Arc<[u8]>,
    wake_signal: Arc<Notify>,
}

pub async fn spawn(
    settings: &WebhookSettings,
    wake_signal: Arc<Notify>,
) -> Result<Option<JoinHandle<Result<()>>>> {
    let Some(listen) = settings.listen.as_ref() else {
        return Ok(None);
    };
    let secret = settings
        .secret
        .as_ref()
        .ok_or_else(|| anyhow!("missing_webhook_secret"))?;

    let listener = TcpListener::bind(listen)
        .await
        .with_context(|| format!("failed to bind webhook listener on {listen}"))?;
    let local_addr = listener
        .local_addr()
        .context("failed to inspect webhook listen address")?;
    let path = normalized_path(&settings.path);

    let app = Router::new()
        .route(&path, post(handle_github_webhook))
        .route("/healthz", get(healthz))
        .with_state(WebhookState {
            secret: Arc::<[u8]>::from(secret.as_bytes()),
            wake_signal,
        });

    info!(%local_addr, path, "GitHub webhook listener enabled");

    Ok(Some(tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .map_err(|error| anyhow!("webhook_server_error: {error}"))
    })))
}

fn normalized_path(path: &str) -> String {
    if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    }
}

async fn healthz() -> impl IntoResponse {
    Json(json!({ "ok": true }))
}

async fn handle_github_webhook(
    State(state): State<WebhookState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    match process_github_webhook(&state, &headers, &body) {
        Ok(WebhookDecision::Accepted { wake }) => {
            if wake {
                state.wake_signal.notify_one();
            }
            (
                StatusCode::ACCEPTED,
                Json(json!({ "accepted": true, "wake": wake })),
            )
                .into_response()
        }
        Ok(WebhookDecision::Ignored { reason }) => (
            StatusCode::ACCEPTED,
            Json(json!({ "accepted": false, "reason": reason })),
        )
            .into_response(),
        Err(error) => (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "accepted": false, "error": error.to_string() })),
        )
            .into_response(),
    }
}

enum WebhookDecision {
    Accepted { wake: bool },
    Ignored { reason: &'static str },
}

fn process_github_webhook(
    state: &WebhookState,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<WebhookDecision> {
    verify_signature(
        headers
            .get("x-hub-signature-256")
            .and_then(|value| value.to_str().ok()),
        &state.secret,
        body,
    )?;

    let Some(event) = headers
        .get("x-github-event")
        .and_then(|value| value.to_str().ok())
    else {
        return Err(anyhow!("missing_x_github_event"));
    };

    if event == "ping" {
        return Ok(WebhookDecision::Ignored { reason: "ping" });
    }

    if should_wake_for_event(event) {
        Ok(WebhookDecision::Accepted { wake: true })
    } else {
        Ok(WebhookDecision::Ignored {
            reason: "unsupported_event",
        })
    }
}

fn should_wake_for_event(event: &str) -> bool {
    matches!(
        event,
        "issues"
            | "issue_comment"
            | "pull_request"
            | "pull_request_review"
            | "pull_request_review_comment"
            | "check_run"
            | "check_suite"
            | "projects_v2"
            | "projects_v2_item"
    )
}

fn verify_signature(signature: Option<&str>, secret: &[u8], body: &[u8]) -> Result<()> {
    let signature = signature.ok_or_else(|| anyhow!("missing_x_hub_signature_256"))?;
    let digest = signature
        .strip_prefix("sha256=")
        .ok_or_else(|| anyhow!("invalid_x_hub_signature_256"))?;
    let expected = hex::decode(digest).context("invalid_signature_hex")?;

    let mut mac = HmacSha256::new_from_slice(secret).context("invalid_webhook_secret")?;
    mac.update(body);
    mac.verify_slice(&expected)
        .map_err(|_| anyhow!("webhook_signature_mismatch"))
}

#[cfg(test)]
mod tests {
    use super::{should_wake_for_event, verify_signature, HmacSha256};
    use hmac::Mac;

    #[test]
    fn accepts_known_wakeup_events() {
        assert!(should_wake_for_event("issues"));
        assert!(should_wake_for_event("check_suite"));
        assert!(should_wake_for_event("projects_v2_item"));
        assert!(!should_wake_for_event("fork"));
    }

    #[test]
    fn verifies_valid_github_signature() {
        let secret = b"webhook-secret";
        let body = br#"{"action":"opened"}"#;
        let mut mac = HmacSha256::new_from_slice(secret).unwrap();
        mac.update(body);
        let signature = format!("sha256={}", hex::encode(mac.finalize().into_bytes()));

        verify_signature(Some(&signature), secret, body).unwrap();
    }

    #[test]
    fn rejects_invalid_github_signature() {
        let error = verify_signature(Some("sha256=deadbeef"), b"secret", br#"{}"#).unwrap_err();
        assert!(error.to_string().contains("webhook_signature_mismatch"));
    }
}
