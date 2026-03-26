use anyhow::{anyhow, Result};
use reqwest::Method;
use serde_json::{json, Value as JsonValue};

use crate::github::GitHubTracker;

pub const GITHUB_GRAPHQL_TOOL_NAME: &str = "github_graphql";
pub const GITHUB_REST_TOOL_NAME: &str = "github_rest";

pub fn tool_schemas() -> Vec<JsonValue> {
    vec![graphql_tool_schema(), rest_tool_schema()]
}

pub fn graphql_tool_schema() -> JsonValue {
    json!({
        "name": GITHUB_GRAPHQL_TOOL_NAME,
        "description": "Execute a raw GraphQL query or mutation against GitHub using Kairastra's configured auth.",
        "inputSchema": {
            "type": "object",
            "additionalProperties": false,
            "required": ["query"],
            "properties": {
                "query": {
                    "type": "string",
                    "description": "GraphQL query or mutation document."
                },
                "variables": {
                    "type": ["object", "null"],
                    "additionalProperties": true
                }
            }
        }
    })
}

pub fn rest_tool_schema() -> JsonValue {
    json!({
        "name": GITHUB_REST_TOOL_NAME,
        "description": "Execute a small allow-listed set of GitHub REST endpoints.",
        "inputSchema": {
            "type": "object",
            "additionalProperties": false,
            "required": ["method", "path"],
            "properties": {
                "method": { "type": "string" },
                "path": { "type": "string" },
                "body": { "type": ["object", "null"], "additionalProperties": true }
            }
        }
    })
}

pub async fn execute_github_graphql(
    tracker: &GitHubTracker,
    arguments: JsonValue,
) -> Result<JsonValue> {
    let query = arguments
        .get("query")
        .and_then(JsonValue::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let Some(query) = query else {
        return Err(anyhow!(
            "`github_graphql` requires a non-empty `query` string."
        ));
    };

    let variables = arguments
        .get("variables")
        .cloned()
        .unwrap_or_else(|| json!({}));

    tracker.graphql_raw(query, variables).await
}

pub async fn execute_github_rest(
    tracker: &GitHubTracker,
    arguments: JsonValue,
) -> Result<JsonValue> {
    let method = arguments
        .get("method")
        .and_then(JsonValue::as_str)
        .map(|value| value.to_uppercase());
    let path = arguments.get("path").and_then(JsonValue::as_str);

    let (Some(method), Some(path)) = (method, path) else {
        return Err(anyhow!("`github_rest` expects `method` and `path`."));
    };

    if !rest_path_allowed(path) {
        return Err(anyhow!("REST path not allow-listed: {path}"));
    }

    let method = match method.as_str() {
        "GET" => Method::GET,
        "POST" => Method::POST,
        "PATCH" => Method::PATCH,
        other => return Err(anyhow!("Unsupported github_rest method: {other}")),
    };

    let body = arguments.get("body").cloned();
    tracker.rest_json(method, path, body).await
}

pub fn rest_path_allowed(path: &str) -> bool {
    path.contains("/issues/") || path.contains("/pulls/")
}
