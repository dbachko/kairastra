use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value as JsonValue};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::config::{GitHubMode, TrackerSettings};
use crate::github::GitHubTracker;
use crate::github_tools::{
    execute_github_graphql, execute_github_rest, tool_schemas, GITHUB_GRAPHQL_TOOL_NAME,
    GITHUB_REST_TOOL_NAME,
};

const DEFAULT_GITHUB_GRAPHQL_ENDPOINT: &str = "https://api.github.com/graphql";
const DEFAULT_GITHUB_REST_ENDPOINT: &str = "https://api.github.com";
const DEFAULT_MCP_PROTOCOL_VERSION: &str = "2025-06-18";

pub async fn run() -> Result<()> {
    let tracker = GitHubTracker::new(tracker_settings_from_env()?)?;
    let stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let mut lines = BufReader::new(stdin).lines();

    while let Some(line) = lines
        .next_line()
        .await
        .context("failed to read MCP input")?
    {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let message: JsonValue = serde_json::from_str(trimmed)
            .with_context(|| format!("invalid MCP JSON: {trimmed}"))?;
        let Some(response) = handle_message(&tracker, message).await? else {
            continue;
        };

        let encoded =
            serde_json::to_string(&response).context("failed to encode MCP response JSON")?;
        stdout
            .write_all(encoded.as_bytes())
            .await
            .context("failed to write MCP response")?;
        stdout
            .write_all(b"\n")
            .await
            .context("failed to terminate MCP response")?;
        stdout
            .flush()
            .await
            .context("failed to flush MCP response")?;
    }

    Ok(())
}

async fn handle_message(tracker: &GitHubTracker, message: JsonValue) -> Result<Option<JsonValue>> {
    let method = message
        .get("method")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| anyhow!("missing MCP method"))?;
    let id = message.get("id").cloned();
    let params = message.get("params").cloned().unwrap_or_else(|| json!({}));

    let response = match method {
        "initialize" => {
            let protocol_version = params
                .get("protocolVersion")
                .and_then(JsonValue::as_str)
                .filter(|value| !value.trim().is_empty())
                .unwrap_or(DEFAULT_MCP_PROTOCOL_VERSION);
            success_response(
                id,
                json!({
                    "protocolVersion": protocol_version,
                    "capabilities": {
                        "tools": {
                            "listChanged": false
                        }
                    },
                    "serverInfo": {
                        "name": "kairastra-github",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                }),
            )
        }
        "notifications/initialized" => return Ok(None),
        "ping" => success_response(id, json!({})),
        "tools/list" => success_response(id, json!({ "tools": tool_schemas() })),
        "resources/list" => success_response(id, json!({ "resources": [] })),
        "prompts/list" => success_response(id, json!({ "prompts": [] })),
        "tools/call" => handle_tool_call(tracker, id, params).await,
        other => error_response(id, -32601, format!("Method not found: {other}")),
    };

    Ok(Some(response))
}

async fn handle_tool_call(
    tracker: &GitHubTracker,
    id: Option<JsonValue>,
    params: JsonValue,
) -> JsonValue {
    let name = params.get("name").and_then(JsonValue::as_str);
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));

    let result = match name {
        Some(GITHUB_GRAPHQL_TOOL_NAME) => execute_github_graphql(tracker, arguments).await,
        Some(GITHUB_REST_TOOL_NAME) => execute_github_rest(tracker, arguments).await,
        Some(other) => Err(anyhow!("Unknown tool: {other}")),
        None => Err(anyhow!("Tool call missing `name`.")),
    };

    match result {
        Ok(payload) => success_response(
            id,
            json!({
                "content": [json!({
                    "type": "text",
                    "text": pretty_json(&payload)
                })],
                "structuredContent": payload,
                "isError": false
            }),
        ),
        Err(error) => success_response(
            id,
            json!({
                "content": [json!({
                    "type": "text",
                    "text": error.to_string()
                })],
                "isError": true
            }),
        ),
    }
}

fn success_response(id: Option<JsonValue>, result: JsonValue) -> JsonValue {
    json!({
        "jsonrpc": "2.0",
        "id": id.unwrap_or(JsonValue::Null),
        "result": result
    })
}

fn error_response(id: Option<JsonValue>, code: i64, message: String) -> JsonValue {
    json!({
        "jsonrpc": "2.0",
        "id": id.unwrap_or(JsonValue::Null),
        "error": {
            "code": code,
            "message": message
        }
    })
}

fn pretty_json(payload: &JsonValue) -> String {
    serde_json::to_string_pretty(payload).unwrap_or_else(|_| payload.to_string())
}

fn tracker_settings_from_env() -> Result<TrackerSettings> {
    let api_key = std::env::var("GITHUB_TOKEN")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            std::env::var("GH_TOKEN")
                .ok()
                .filter(|value| !value.trim().is_empty())
        })
        .ok_or_else(|| anyhow!("missing_github_api_token"))?;

    let owner = std::env::var("KAIRASTRA_GITHUB_OWNER")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    let repo = std::env::var("KAIRASTRA_GITHUB_REPO")
        .ok()
        .filter(|value| !value.trim().is_empty());

    Ok(TrackerSettings {
        kind: "github".to_string(),
        mode: GitHubMode::IssuesOnly,
        api_key,
        owner,
        repo,
        project_owner: None,
        project_v2_number: None,
        project_url: None,
        active_states: Vec::new(),
        terminal_states: Vec::new(),
        status_source: None,
        priority_source: None,
        graphql_endpoint: DEFAULT_GITHUB_GRAPHQL_ENDPOINT.to_string(),
        rest_endpoint: DEFAULT_GITHUB_REST_ENDPOINT.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::handle_message;
    use crate::config::{GitHubMode, TrackerSettings};
    use crate::github::GitHubTracker;

    fn fake_tracker() -> GitHubTracker {
        GitHubTracker::new(TrackerSettings {
            kind: "github".to_string(),
            mode: GitHubMode::IssuesOnly,
            api_key: "test-token".to_string(),
            owner: "openai".to_string(),
            repo: Some("symphony".to_string()),
            project_owner: None,
            project_v2_number: None,
            project_url: None,
            active_states: Vec::new(),
            terminal_states: Vec::new(),
            status_source: None,
            priority_source: None,
            graphql_endpoint: "https://api.github.com/graphql".to_string(),
            rest_endpoint: "https://api.github.com".to_string(),
        })
        .unwrap()
    }

    #[tokio::test]
    async fn initialize_returns_server_info_and_tools_capability() {
        let tracker = fake_tracker();
        let response = handle_message(
            &tracker,
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-06-18"
                }
            }),
        )
        .await
        .unwrap()
        .unwrap();

        assert_eq!(response["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(response["result"]["serverInfo"]["name"], "kairastra-github");
        assert_eq!(
            response["result"]["capabilities"]["tools"]["listChanged"],
            false
        );
    }

    #[tokio::test]
    async fn tools_list_exposes_github_tools() {
        let tracker = fake_tracker();
        let response = handle_message(
            &tracker,
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/list"
            }),
        )
        .await
        .unwrap()
        .unwrap();

        let tools = response["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0]["name"], "github_graphql");
        assert_eq!(tools[1]["name"], "github_rest");
    }

    #[tokio::test]
    async fn unknown_tool_returns_tool_error_result() {
        let tracker = fake_tracker();
        let response = handle_message(
            &tracker,
            json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": {
                    "name": "unknown_tool",
                    "arguments": {}
                }
            }),
        )
        .await
        .unwrap()
        .unwrap();

        assert_eq!(response["result"]["isError"], true);
        assert!(response["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Unknown tool"));
    }
}
