use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, USER_AGENT};
use reqwest::Client;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value as JsonValue};
use tracing::debug;

use crate::config::{FieldSourceType, GitHubMode, TrackerSettings};
use crate::model::{BlockerRef, Issue};

#[async_trait]
pub trait Tracker: Send + Sync {
    async fn fetch_candidate_issues(&self) -> Result<Vec<Issue>>;
    async fn fetch_issues_by_states(&self, states: &[String]) -> Result<Vec<Issue>>;
    async fn fetch_issue_states_by_ids(&self, issue_ids: &[String]) -> Result<Vec<Issue>>;
}

#[derive(Clone)]
pub struct GitHubTracker {
    settings: TrackerSettings,
    client: Client,
}

impl GitHubTracker {
    pub fn new(settings: TrackerSettings) -> Result<Self> {
        let mut headers = HeaderMap::new();
        headers.insert(USER_AGENT, HeaderValue::from_static("symphony-rust/0.1.0"));
        headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/vnd.github+json"),
        );
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", settings.api_key))
                .context("invalid GitHub token header")?,
        );

        let client = Client::builder()
            .default_headers(headers)
            .build()
            .context("failed to build GitHub HTTP client")?;

        Ok(Self { settings, client })
    }

    pub fn settings(&self) -> &TrackerSettings {
        &self.settings
    }

    pub async fn graphql_raw(&self, query: &str, variables: JsonValue) -> Result<JsonValue> {
        let payload = json!({
            "query": query,
            "variables": variables,
        });

        let response = self
            .client
            .post(&self.settings.graphql_endpoint)
            .json(&payload)
            .send()
            .await
            .context("failed to send GitHub GraphQL request")?;

        let status = response.status();
        let body = response
            .json::<JsonValue>()
            .await
            .context("failed to decode GitHub GraphQL response")?;

        if !status.is_success() {
            return Err(anyhow!("github_graphql_status: {} body={body}", status));
        }

        Ok(body)
    }

    pub async fn rest_json(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<JsonValue>,
    ) -> Result<JsonValue> {
        let url = format!(
            "{}{}",
            self.settings.rest_endpoint.trim_end_matches('/'),
            path
        );
        let request = self.client.request(method, url);
        let request = if let Some(body) = body {
            request.json(&body)
        } else {
            request
        };

        let response = request
            .send()
            .await
            .with_context(|| format!("failed to send GitHub REST request for {path}"))?;

        let status = response.status();
        let body = response
            .json::<JsonValue>()
            .await
            .context("failed to decode GitHub REST response")?;

        if !status.is_success() {
            return Err(anyhow!(
                "github_rest_status: {} path={} body={body}",
                status,
                path
            ));
        }

        Ok(body)
    }

    async fn graphql<T: DeserializeOwned>(&self, query: &str, variables: JsonValue) -> Result<T> {
        let body = self.graphql_raw(query, variables).await?;
        let envelope: GraphqlEnvelope<T> =
            serde_json::from_value(body).context("failed to decode GitHub GraphQL envelope")?;

        match (envelope.data, envelope.errors) {
            (Some(data), _) => Ok(data),
            (None, Some(errors)) => Err(anyhow!("github_graphql_errors: {:?}", errors)),
            (None, None) => Err(anyhow!("github_graphql_missing_data")),
        }
    }

    async fn list_project_items(&self) -> Result<Vec<ProjectItemNode>> {
        let project_number = self
            .settings
            .project_v2_number
            .ok_or_else(|| anyhow!("missing_github_project_v2_number"))?;
        let status_field = self
            .settings
            .status_source
            .as_ref()
            .and_then(|source| source.name.clone())
            .unwrap_or_else(|| "Status".to_string());
        let priority_field = self
            .settings
            .priority_source
            .as_ref()
            .and_then(|source| source.name.clone())
            .unwrap_or_else(|| "Priority".to_string());

        let query = r#"
query SymphonyProjectItems(
  $owner: String!,
  $projectNumber: Int!,
  $after: String,
  $statusField: String!,
  $priorityField: String!
) {
  organization(login: $owner) {
    projectV2(number: $projectNumber) {
      items(first: 100, after: $after) {
        pageInfo {
          hasNextPage
          endCursor
        }
        nodes {
          id
          status: fieldValueByName(name: $statusField) {
            __typename
            ... on ProjectV2ItemFieldSingleSelectValue { name }
            ... on ProjectV2ItemFieldTextValue { text }
            ... on ProjectV2ItemFieldNumberValue { number }
          }
          priority: fieldValueByName(name: $priorityField) {
            __typename
            ... on ProjectV2ItemFieldSingleSelectValue { name }
            ... on ProjectV2ItemFieldTextValue { text }
            ... on ProjectV2ItemFieldNumberValue { number }
          }
          content {
            __typename
            ... on Issue {
              id
              number
              title
              body
              url
              state
              createdAt
              updatedAt
              assignees(first: 20) {
                nodes {
                  login
                }
              }
              labels(first: 50) {
                nodes {
                  name
                }
              }
              repository {
                name
                owner {
                  login
                }
              }
            }
          }
        }
      }
    }
  }
  user(login: $owner) {
    projectV2(number: $projectNumber) {
      items(first: 100, after: $after) {
        pageInfo {
          hasNextPage
          endCursor
        }
        nodes {
          id
          status: fieldValueByName(name: $statusField) {
            __typename
            ... on ProjectV2ItemFieldSingleSelectValue { name }
            ... on ProjectV2ItemFieldTextValue { text }
            ... on ProjectV2ItemFieldNumberValue { number }
          }
          priority: fieldValueByName(name: $priorityField) {
            __typename
            ... on ProjectV2ItemFieldSingleSelectValue { name }
            ... on ProjectV2ItemFieldTextValue { text }
            ... on ProjectV2ItemFieldNumberValue { number }
          }
          content {
            __typename
            ... on Issue {
              id
              number
              title
              body
              url
              state
              createdAt
              updatedAt
              assignees(first: 20) {
                nodes {
                  login
                }
              }
              labels(first: 50) {
                nodes {
                  name
                }
              }
              repository {
                name
                owner {
                  login
                }
              }
            }
          }
        }
      }
    }
  }
}"#;

        let mut after: Option<String> = None;
        let mut items = Vec::new();

        loop {
            let data: ProjectItemsResponse = self
                .graphql(
                    query,
                    json!({
                        "owner": self.settings.owner,
                        "projectNumber": project_number,
                        "after": after,
                        "statusField": status_field,
                        "priorityField": priority_field,
                    }),
                )
                .await?;

            let page = data
                .organization
                .or(data.user)
                .and_then(|owner| owner.project_v2)
                .ok_or_else(|| anyhow!("github_project_not_found"))?
                .items;

            items.extend(page.nodes);
            if !page.page_info.has_next_page {
                break;
            }
            after = page.page_info.end_cursor;
        }

        Ok(items)
    }

    async fn load_project_status_field_metadata(&self) -> Result<ProjectStatusFieldMetadata> {
        let project_number = self
            .settings
            .project_v2_number
            .ok_or_else(|| anyhow!("missing_github_project_v2_number"))?;
        let status_field = self
            .settings
            .status_source
            .as_ref()
            .and_then(|source| source.name.clone())
            .unwrap_or_else(|| "Status".to_string());

        let query = r#"
query SymphonyProjectStatusField($owner: String!, $projectNumber: Int!) {
  organization(login: $owner) {
    projectV2(number: $projectNumber) {
      id
      fields(first: 50) {
        nodes {
          __typename
          ... on ProjectV2SingleSelectField {
            id
            name
            options {
              id
              name
            }
          }
        }
      }
    }
  }
  user(login: $owner) {
    projectV2(number: $projectNumber) {
      id
      fields(first: 50) {
        nodes {
          __typename
          ... on ProjectV2SingleSelectField {
            id
            name
            options {
              id
              name
            }
          }
        }
      }
    }
  }
}"#;

        let data: ProjectFieldMetadataResponse = self
            .graphql(
                query,
                json!({
                    "owner": self.settings.owner,
                    "projectNumber": project_number,
                }),
            )
            .await?;

        let project = data
            .organization
            .or(data.user)
            .and_then(|owner| owner.project_v2)
            .ok_or_else(|| anyhow!("github_project_not_found"))?;

        let field = project
            .fields
            .nodes
            .into_iter()
            .find_map(|field| match field {
                ProjectFieldNode::ProjectV2SingleSelectField(field)
                    if field.name.eq_ignore_ascii_case(&status_field) =>
                {
                    Some(field)
                }
                ProjectFieldNode::ProjectV2SingleSelectField(_) => None,
                ProjectFieldNode::Other => None,
            })
            .ok_or_else(|| anyhow!("github_project_status_field_not_found"))?;

        Ok(ProjectStatusFieldMetadata {
            project_id: project.id,
            field_id: field.id,
            options: field.options,
        })
    }

    pub async fn transition_issue_project_status(
        &self,
        issue: &Issue,
        target_status: &str,
    ) -> Result<Issue> {
        if self.settings.mode != GitHubMode::ProjectsV2
            || issue.state.trim().eq_ignore_ascii_case(target_status)
        {
            return Ok(issue.clone());
        }

        let Some(project_item_id) = issue.project_item_id.as_ref() else {
            return Ok(issue.clone());
        };

        let metadata = self.load_project_status_field_metadata().await?;
        let option_id = metadata
            .options
            .iter()
            .find(|option| option.name.eq_ignore_ascii_case(target_status))
            .map(|option| option.id.clone())
            .ok_or_else(|| anyhow!("github_project_status_option_not_found: {target_status}"))?;

        self.graphql_raw(
            r#"
mutation SymphonyUpdateProjectItemStatus(
  $projectId: ID!,
  $itemId: ID!,
  $fieldId: ID!,
  $optionId: String!
) {
  updateProjectV2ItemFieldValue(
    input: {
      projectId: $projectId
      itemId: $itemId
      fieldId: $fieldId
      value: { singleSelectOptionId: $optionId }
    }
  ) {
    projectV2Item {
      id
    }
  }
}"#,
            json!({
                "projectId": metadata.project_id,
                "itemId": project_item_id,
                "fieldId": metadata.field_id,
                "optionId": option_id,
            }),
        )
        .await?;

        let refreshed = self.fetch_issue_states_by_ids(&[issue.id.clone()]).await?;
        Ok(refreshed.into_iter().next().unwrap_or_else(|| {
            let mut updated = issue.clone();
            updated.state = target_status.to_string();
            updated
        }))
    }

    pub async fn transition_issue_to_in_progress_on_claim(&self, issue: &Issue) -> Result<Issue> {
        if !issue.state.trim().eq_ignore_ascii_case("todo") {
            return Ok(issue.clone());
        }
        self.transition_issue_project_status(issue, "In Progress")
            .await
    }

    pub async fn transition_closed_issue_to_done(&self, issue: &Issue) -> Result<Issue> {
        if !issue.state.trim().eq_ignore_ascii_case("closed") {
            return Ok(issue.clone());
        }
        self.transition_issue_project_status(issue, "Done").await
    }

    pub async fn has_open_pull_request_for_branch(
        &self,
        owner: &str,
        repo: &str,
        branch: &str,
    ) -> Result<bool> {
        let path =
            format!("/repos/{owner}/{repo}/pulls?state=open&head={owner}:{branch}&per_page=1");
        let response = self.rest_json(reqwest::Method::GET, &path, None).await?;
        let pulls = response
            .as_array()
            .ok_or_else(|| anyhow!("github_pull_list_payload_invalid"))?;
        Ok(!pulls.is_empty())
    }

    pub async fn ensure_workpad_comment(&self, issue: &Issue, body: &str) -> Result<Issue> {
        let (owner, repo, issue_number) = issue_locator(issue)?;
        let path = format!("/repos/{owner}/{repo}/issues/{issue_number}/comments?per_page=100");
        let response = self.rest_json(reqwest::Method::GET, &path, None).await?;
        let comments: Vec<RestIssueComment> =
            serde_json::from_value(response).context("invalid GitHub issue comments payload")?;

        if let Some(comment) = comments
            .into_iter()
            .rev()
            .find(|comment| comment.body.contains("## Codex Workpad"))
        {
            let mut updated = issue.clone();
            updated.workpad_comment_id = Some(comment.id);
            updated.workpad_comment_url = comment.html_url;
            updated.workpad_comment_body = Some(comment.body);
            return Ok(updated);
        }

        let path = format!("/repos/{owner}/{repo}/issues/{issue_number}/comments");
        let response = self
            .rest_json(reqwest::Method::POST, &path, Some(json!({ "body": body })))
            .await?;
        let comment: RestIssueComment = serde_json::from_value(response)
            .context("invalid GitHub created issue comment payload")?;

        let mut updated = issue.clone();
        updated.workpad_comment_id = Some(comment.id);
        updated.workpad_comment_url = comment.html_url;
        updated.workpad_comment_body = Some(comment.body);
        Ok(updated)
    }

    pub async fn refresh_workpad_comment(&self, issue: &Issue) -> Result<Issue> {
        let (owner, repo, issue_number) = issue_locator(issue)?;
        let path = format!("/repos/{owner}/{repo}/issues/{issue_number}/comments?per_page=100");
        let response = self.rest_json(reqwest::Method::GET, &path, None).await?;
        let comments: Vec<RestIssueComment> =
            serde_json::from_value(response).context("invalid GitHub issue comments payload")?;

        let mut updated = issue.clone();
        if let Some(comment) = comments
            .into_iter()
            .rev()
            .find(|comment| comment.body.contains("## Codex Workpad"))
        {
            updated.workpad_comment_id = Some(comment.id);
            updated.workpad_comment_url = comment.html_url;
            updated.workpad_comment_body = Some(comment.body);
        }

        Ok(updated)
    }

    async fn list_repo_issues(&self, state: &str) -> Result<Vec<RestIssue>> {
        let repo = self
            .settings
            .repo
            .as_ref()
            .ok_or_else(|| anyhow!("missing_github_repo"))?;
        let mut page = 1_u32;
        let mut issues = Vec::new();

        loop {
            let path = format!(
                "/repos/{}/{}/issues?state={}&per_page=100&page={}",
                self.settings.owner, repo, state, page
            );
            let response = self
                .rest_json(reqwest::Method::GET, &path, None)
                .await
                .with_context(|| format!("failed to list issues for page {page}"))?;
            let page_items: Vec<RestIssue> =
                serde_json::from_value(response).context("invalid GitHub issues payload")?;
            if page_items.is_empty() {
                break;
            }
            issues.extend(page_items);
            page += 1;
        }

        Ok(issues)
    }

    async fn fetch_blocked_by(
        &self,
        owner: &str,
        repo: &str,
        issue_number: u64,
    ) -> Result<Vec<BlockerRef>> {
        let path = format!("/repos/{owner}/{repo}/issues/{issue_number}/dependencies/blocked_by");

        let response = match self.rest_json(reqwest::Method::GET, &path, None).await {
            Ok(response) => response,
            Err(error) => {
                debug!(issue_number, error = ?error, "blocked_by lookup failed; treating as empty");
                return Ok(Vec::new());
            }
        };

        let entries = match response {
            JsonValue::Array(entries) => entries,
            JsonValue::Object(mut object) => object
                .remove("blocked_by")
                .or_else(|| object.remove("dependencies"))
                .and_then(|value| value.as_array().cloned())
                .unwrap_or_default(),
            _ => Vec::new(),
        };

        Ok(entries
            .into_iter()
            .map(|entry| BlockerRef {
                id: entry
                    .get("node_id")
                    .and_then(JsonValue::as_str)
                    .map(ToString::to_string)
                    .or_else(|| entry.get("id").and_then(json_stringish)),
                identifier: issue_identifier_from_json(&entry),
                state: entry
                    .get("state")
                    .and_then(JsonValue::as_str)
                    .map(title_case_state),
            })
            .collect())
    }

    async fn fetch_issue_field_values(
        &self,
        owner: &str,
        repo: &str,
        issue_number: u64,
    ) -> Result<HashMap<String, JsonValue>> {
        let path = format!("/repos/{owner}/{repo}/issues/{issue_number}/issue-field-values");
        let response = self.rest_json(reqwest::Method::GET, &path, None).await?;

        let entries = match response {
            JsonValue::Array(entries) => entries,
            JsonValue::Object(mut object) => object
                .remove("issue_field_values")
                .and_then(|value| value.as_array().cloned())
                .unwrap_or_default(),
            _ => Vec::new(),
        };

        let mut values = HashMap::new();
        for entry in entries {
            if let Some(name) = entry
                .get("field")
                .and_then(|field| field.get("name"))
                .and_then(JsonValue::as_str)
                .map(ToString::to_string)
                .or_else(|| {
                    entry
                        .get("name")
                        .and_then(JsonValue::as_str)
                        .map(ToString::to_string)
                })
            {
                if let Some(value) = entry
                    .get("value")
                    .cloned()
                    .or_else(|| entry.get("text").cloned())
                    .or_else(|| entry.get("number").cloned())
                {
                    values.insert(name, value);
                }
            }
        }

        Ok(values)
    }

    async fn normalize_project_issue(&self, item: ProjectItemNode) -> Result<Option<Issue>> {
        let ProjectItemNode {
            id: project_item_id,
            status,
            priority,
            content,
        } = item;

        let content = match content {
            Some(ProjectItemContent::Issue(issue)) => issue,
            _ => return Ok(None),
        };

        let owner = content.repository.owner.login.clone();
        let repo = content.repository.name.clone();
        let issue_state = title_case_state(content.state.as_deref().unwrap_or("OPEN"));
        let project_status = status.as_ref().and_then(field_value_string);
        let state = if issue_state.eq_ignore_ascii_case("closed") {
            project_status
                .filter(|status| {
                    self.settings
                        .terminal_states
                        .iter()
                        .any(|candidate| candidate.eq_ignore_ascii_case(status))
                })
                .unwrap_or(issue_state)
        } else {
            project_status.unwrap_or(issue_state)
        };

        let blocked_by = if state.trim().eq_ignore_ascii_case("todo") {
            self.fetch_blocked_by(&owner, &repo, content.number).await?
        } else {
            Vec::new()
        };

        Ok(Some(Issue {
            id: content.id.clone(),
            project_item_id: Some(project_item_id),
            identifier: format!("{owner}/{repo}#{}", content.number),
            title: content.title.clone(),
            description: content.body.clone().filter(|value| !value.is_empty()),
            priority: priority.as_ref().and_then(field_value_priority),
            state,
            branch_name: None,
            url: Some(content.url.clone()),
            assignees: content
                .assignees
                .nodes
                .iter()
                .filter_map(|assignee| assignee.login.as_ref().map(|value| value.to_lowercase()))
                .collect(),
            labels: content
                .labels
                .nodes
                .iter()
                .filter_map(|label| label.name.as_ref().map(|value| value.to_lowercase()))
                .collect(),
            blocked_by,
            created_at: content.created_at,
            updated_at: content.updated_at,
            workpad_comment_id: None,
            workpad_comment_url: None,
            workpad_comment_body: None,
        }))
    }

    async fn normalize_rest_issue(&self, issue: RestIssue) -> Result<Issue> {
        let owner = issue
            .repository
            .as_ref()
            .map(|repo| repo.owner.login.clone())
            .unwrap_or_else(|| self.settings.owner.clone());
        let repo = issue
            .repository
            .as_ref()
            .map(|repo| repo.name.clone())
            .or_else(|| self.settings.repo.clone())
            .ok_or_else(|| anyhow!("missing_github_repo"))?;

        let field_values = match self
            .settings
            .status_source
            .as_ref()
            .map(|source| source.source_type)
        {
            Some(FieldSourceType::IssueField) => self
                .fetch_issue_field_values(&owner, &repo, issue.number)
                .await
                .unwrap_or_default(),
            _ => HashMap::new(),
        };

        let state = resolve_issue_state(&self.settings, &issue, &field_values);
        let blocked_by = if state.trim().eq_ignore_ascii_case("todo") {
            self.fetch_blocked_by(&owner, &repo, issue.number).await?
        } else {
            Vec::new()
        };

        Ok(Issue {
            id: issue
                .node_id
                .clone()
                .unwrap_or_else(|| issue.id.to_string()),
            project_item_id: None,
            identifier: format!("{owner}/{repo}#{}", issue.number),
            title: issue.title.clone(),
            description: issue.body.clone().filter(|value| !value.is_empty()),
            priority: resolve_priority(&self.settings, &issue, &field_values),
            state,
            branch_name: None,
            url: issue.html_url.clone(),
            assignees: issue
                .assignees
                .iter()
                .filter_map(|assignee| assignee.login.as_ref().map(|value| value.to_lowercase()))
                .collect(),
            labels: issue
                .labels
                .iter()
                .filter_map(|label| label.name.as_ref().map(|value| value.to_lowercase()))
                .collect(),
            blocked_by,
            created_at: issue.created_at,
            updated_at: issue.updated_at,
            workpad_comment_id: None,
            workpad_comment_url: None,
            workpad_comment_body: None,
        })
    }
}

#[async_trait]
impl Tracker for GitHubTracker {
    async fn fetch_candidate_issues(&self) -> Result<Vec<Issue>> {
        match self.settings.mode {
            GitHubMode::ProjectsV2 => {
                let mut issues = Vec::new();
                for item in self.list_project_items().await? {
                    if let Some(issue) = self.normalize_project_issue(item).await? {
                        issues.push(issue);
                    }
                }
                Ok(issues)
            }
            GitHubMode::IssuesOnly => {
                let mut issues = Vec::new();
                for issue in self.list_repo_issues("open").await? {
                    if issue.pull_request.is_some() {
                        continue;
                    }
                    issues.push(self.normalize_rest_issue(issue).await?);
                }
                Ok(issues)
            }
        }
    }

    async fn fetch_issues_by_states(&self, states: &[String]) -> Result<Vec<Issue>> {
        let wanted: Vec<String> = states
            .iter()
            .map(|state| state.trim().to_lowercase())
            .collect();
        let issues = match self.settings.mode {
            GitHubMode::ProjectsV2 => {
                let mut issues = Vec::new();
                for item in self.list_project_items().await? {
                    if let Some(issue) = self.normalize_project_issue(item).await? {
                        issues.push(issue);
                    }
                }
                issues
            }
            GitHubMode::IssuesOnly => {
                let mut issues = Vec::new();
                for issue in self.list_repo_issues("all").await? {
                    if issue.pull_request.is_some() {
                        continue;
                    }
                    issues.push(self.normalize_rest_issue(issue).await?);
                }
                issues
            }
        };

        Ok(issues
            .into_iter()
            .filter(|issue| wanted.contains(&issue.state.trim().to_lowercase()))
            .collect())
    }

    async fn fetch_issue_states_by_ids(&self, issue_ids: &[String]) -> Result<Vec<Issue>> {
        if issue_ids.is_empty() {
            return Ok(Vec::new());
        }

        match self.settings.mode {
            GitHubMode::ProjectsV2 => {
                let ids: Arc<[String]> = issue_ids.to_vec().into();
                let mut issues = Vec::new();
                for item in self.list_project_items().await? {
                    if let Some(content) = &item.content {
                        let issue_id = match content {
                            ProjectItemContent::Issue(issue) => &issue.id,
                        };
                        if ids.iter().any(|candidate| candidate == issue_id) {
                            if let Some(issue) = self.normalize_project_issue(item).await? {
                                issues.push(issue);
                            }
                        }
                    }
                }
                Ok(issues)
            }
            GitHubMode::IssuesOnly => {
                let mut issues = Vec::new();
                for issue in self.list_repo_issues("all").await? {
                    if issue.pull_request.is_some() {
                        continue;
                    }
                    let id = issue
                        .node_id
                        .clone()
                        .unwrap_or_else(|| issue.id.to_string());
                    if issue_ids.iter().any(|candidate| candidate == &id) {
                        issues.push(self.normalize_rest_issue(issue).await?);
                    }
                }
                Ok(issues)
            }
        }
    }
}

fn resolve_issue_state(
    settings: &TrackerSettings,
    issue: &RestIssue,
    field_values: &HashMap<String, JsonValue>,
) -> String {
    match settings
        .status_source
        .as_ref()
        .map(|source| source.source_type)
    {
        Some(FieldSourceType::IssueField) => settings
            .status_source
            .as_ref()
            .and_then(|source| source.name.as_ref())
            .and_then(|name| field_values.get(name))
            .and_then(json_stringish)
            .as_deref()
            .map(title_case_state)
            .unwrap_or_else(|| title_case_state(&issue.state)),
        Some(FieldSourceType::Label) => settings
            .status_source
            .as_ref()
            .and_then(|source| source.name.as_ref())
            .and_then(|prefix| {
                issue
                    .labels
                    .iter()
                    .filter_map(|label| label.name.as_ref())
                    .find_map(|label| label.strip_prefix(prefix).map(str::trim))
            })
            .map(ToString::to_string)
            .unwrap_or_else(|| title_case_state(&issue.state)),
        Some(FieldSourceType::GitHubState) | None | Some(FieldSourceType::ProjectField) => {
            title_case_state(&issue.state)
        }
    }
}

fn resolve_priority(
    settings: &TrackerSettings,
    issue: &RestIssue,
    field_values: &HashMap<String, JsonValue>,
) -> Option<i64> {
    match settings
        .priority_source
        .as_ref()
        .map(|source| source.source_type)
    {
        Some(FieldSourceType::IssueField) => settings
            .priority_source
            .as_ref()
            .and_then(|source| source.name.as_ref())
            .and_then(|name| field_values.get(name))
            .and_then(json_priority),
        _ => issue
            .labels
            .iter()
            .filter_map(|label| {
                label.name.as_ref().and_then(|name| {
                    let lowered = name.to_lowercase();
                    lowered.strip_prefix('p')?.parse::<i64>().ok()
                })
            })
            .min(),
    }
}

fn field_value_string(value: &ProjectFieldValue) -> Option<String> {
    value
        .name
        .clone()
        .or_else(|| value.text.clone())
        .or_else(|| value.number.map(|number| format!("{number:.0}")))
}

fn field_value_priority(value: &ProjectFieldValue) -> Option<i64> {
    value
        .number
        .map(|number| number as i64)
        .or_else(|| {
            value
                .text
                .as_ref()
                .and_then(|text| text.trim().parse::<i64>().ok())
        })
        .or_else(|| {
            value
                .name
                .as_ref()
                .and_then(|text| text.trim().parse::<i64>().ok())
        })
}

fn json_stringish(value: &JsonValue) -> Option<String> {
    match value {
        JsonValue::String(value) => Some(value.clone()),
        JsonValue::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn json_priority(value: &JsonValue) -> Option<i64> {
    match value {
        JsonValue::Number(value) => value.as_i64(),
        JsonValue::String(value) => value.trim().parse::<i64>().ok(),
        _ => None,
    }
}

fn issue_identifier_from_json(value: &JsonValue) -> Option<String> {
    let owner = value
        .get("repository")
        .and_then(|repo| repo.get("owner"))
        .and_then(|owner| owner.get("login"))
        .and_then(JsonValue::as_str)
        .or_else(|| value.get("owner").and_then(JsonValue::as_str))?;
    let repo = value
        .get("repository")
        .and_then(|repo| repo.get("name"))
        .and_then(JsonValue::as_str)
        .or_else(|| value.get("repo").and_then(JsonValue::as_str))?;
    let number = value
        .get("number")
        .and_then(JsonValue::as_u64)
        .or_else(|| value.get("issue_number").and_then(JsonValue::as_u64))?;
    Some(format!("{owner}/{repo}#{number}"))
}

fn title_case_state(raw: &str) -> String {
    match raw.to_lowercase().as_str() {
        "open" => "Open".to_string(),
        "closed" => "Closed".to_string(),
        other => {
            let mut chars = other.chars();
            match chars.next() {
                Some(first) => format!("{}{}", first.to_uppercase(), chars.as_str()),
                None => String::new(),
            }
        }
    }
}

#[derive(Debug, Deserialize)]
struct GraphqlEnvelope<T> {
    data: Option<T>,
    errors: Option<Vec<JsonValue>>,
}

#[derive(Debug, Deserialize)]
struct ProjectItemsResponse {
    organization: Option<ProjectOrganization>,
    user: Option<ProjectOrganization>,
}

#[derive(Debug, Deserialize)]
struct ProjectFieldMetadataResponse {
    organization: Option<ProjectFieldOrganization>,
    user: Option<ProjectFieldOrganization>,
}

#[derive(Debug, Deserialize)]
struct ProjectOrganization {
    #[serde(rename = "projectV2")]
    project_v2: Option<ProjectV2>,
}

#[derive(Debug, Deserialize)]
struct ProjectFieldOrganization {
    #[serde(rename = "projectV2")]
    project_v2: Option<ProjectFieldsProjectV2>,
}

#[derive(Debug, Deserialize)]
struct ProjectV2 {
    items: ProjectItemsPage,
}

#[derive(Debug, Deserialize)]
struct ProjectFieldsProjectV2 {
    id: String,
    fields: ProjectFieldsPage,
}

#[derive(Debug, Deserialize)]
struct ProjectItemsPage {
    #[serde(rename = "pageInfo")]
    page_info: PageInfo,
    nodes: Vec<ProjectItemNode>,
}

#[derive(Debug, Deserialize)]
struct ProjectFieldsPage {
    nodes: Vec<ProjectFieldNode>,
}

#[derive(Debug, Deserialize)]
struct PageInfo {
    #[serde(rename = "hasNextPage")]
    has_next_page: bool,
    #[serde(rename = "endCursor")]
    end_cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ProjectItemNode {
    #[allow(dead_code)]
    id: String,
    status: Option<ProjectFieldValue>,
    priority: Option<ProjectFieldValue>,
    content: Option<ProjectItemContent>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "__typename")]
enum ProjectFieldNode {
    ProjectV2SingleSelectField(ProjectSingleSelectField),
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
struct ProjectSingleSelectField {
    id: String,
    name: String,
    options: Vec<ProjectSingleSelectFieldOption>,
}

#[derive(Debug, Clone, Deserialize)]
struct ProjectSingleSelectFieldOption {
    id: String,
    name: String,
}

struct ProjectStatusFieldMetadata {
    project_id: String,
    field_id: String,
    options: Vec<ProjectSingleSelectFieldOption>,
}

#[derive(Debug, Deserialize)]
struct ProjectFieldValue {
    name: Option<String>,
    text: Option<String>,
    number: Option<f64>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "__typename")]
enum ProjectItemContent {
    Issue(ProjectIssue),
}

#[derive(Debug, Deserialize)]
struct ProjectIssue {
    id: String,
    number: u64,
    title: String,
    body: Option<String>,
    url: String,
    state: Option<String>,
    #[serde(rename = "createdAt")]
    created_at: Option<DateTime<Utc>>,
    #[serde(rename = "updatedAt")]
    updated_at: Option<DateTime<Utc>>,
    assignees: ProjectAssignees,
    labels: ProjectLabels,
    repository: ProjectRepository,
}

#[derive(Debug, Deserialize)]
struct ProjectAssignees {
    nodes: Vec<ProjectAssignee>,
}

#[derive(Debug, Deserialize)]
struct ProjectAssignee {
    login: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ProjectLabels {
    nodes: Vec<ProjectLabel>,
}

#[derive(Debug, Deserialize)]
struct ProjectLabel {
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ProjectRepository {
    name: String,
    owner: ProjectOwner,
}

#[derive(Debug, Deserialize)]
struct ProjectOwner {
    login: String,
}

#[derive(Debug, Deserialize)]
struct RestIssue {
    id: u64,
    node_id: Option<String>,
    number: u64,
    title: String,
    body: Option<String>,
    state: String,
    html_url: Option<String>,
    assignees: Vec<RestAssignee>,
    labels: Vec<RestLabel>,
    created_at: Option<DateTime<Utc>>,
    updated_at: Option<DateTime<Utc>>,
    repository: Option<ProjectRepository>,
    pull_request: Option<JsonValue>,
}

#[derive(Debug, Deserialize)]
struct RestAssignee {
    login: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RestLabel {
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RestIssueComment {
    id: u64,
    body: String,
    html_url: Option<String>,
}

fn issue_locator(issue: &Issue) -> Result<(String, String, u64)> {
    let (repo_path, number) = issue
        .identifier
        .split_once('#')
        .ok_or_else(|| anyhow!("invalid_issue_identifier: {}", issue.identifier))?;
    let (owner, repo) = repo_path
        .split_once('/')
        .ok_or_else(|| anyhow!("invalid_issue_identifier: {}", issue.identifier))?;
    let issue_number = number
        .parse::<u64>()
        .with_context(|| format!("invalid_issue_number: {}", issue.identifier))?;
    Ok((owner.to_string(), repo.to_string(), issue_number))
}

#[cfg(test)]
mod tests {
    use wiremock::matchers::{body_string_contains, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::config::Settings;
    use crate::model::{Issue, WorkflowDefinition};

    use super::{GitHubTracker, Tracker};

    fn settings(yaml: &str) -> Settings {
        let definition = WorkflowDefinition {
            config: serde_yaml::from_str(yaml).unwrap(),
            prompt_template: String::new(),
        };
        Settings::from_workflow(&definition).unwrap()
    }

    #[tokio::test]
    async fn projects_v2_candidate_fetch_normalizes_issue_fields() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/graphql"))
            .and(body_string_contains("SymphonyProjectItems"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": {
                    "organization": {
                        "projectV2": {
                            "items": {
                                "pageInfo": { "hasNextPage": false, "endCursor": null },
                                "nodes": [{
                                    "id": "item-1",
                                    "status": { "__typename": "ProjectV2ItemFieldSingleSelectValue", "name": "Todo" },
                                    "priority": { "__typename": "ProjectV2ItemFieldNumberValue", "number": 1 },
                                    "content": {
                                        "__typename": "Issue",
                                        "id": "issue-node-1",
                                        "number": 42,
                                        "title": "Port tracker",
                                        "body": "body",
                                        "url": "https://github.com/openai/symphony/issues/42",
                                        "state": "OPEN",
                                        "createdAt": "2026-03-13T00:00:00Z",
                                        "updatedAt": "2026-03-13T01:00:00Z",
                                        "assignees": { "nodes": [{ "login": "codex-bot" }] },
                                        "labels": { "nodes": [{ "name": "Backend" }] },
                                        "repository": {
                                            "name": "symphony",
                                            "owner": { "login": "openai" }
                                        }
                                    }
                                }]
                            }
                        }
                    }
                }
            })))
            .mount(&server)
            .await;

        Mock::given(method("GET"))
            .and(path(
                "/repos/openai/symphony/issues/42/dependencies/blocked_by",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .mount(&server)
            .await;

        let tracker = GitHubTracker::new(
            settings(&format!(
                r#"tracker:
  kind: github
  owner: openai
  api_key: fake
  endpoint: {0}/graphql
  rest_endpoint: {0}
  project_v2_number: 7
  mode: projects_v2
  status_source:
    type: project_field
    name: Status
  priority_source:
    type: project_field
    name: Priority
"#,
                server.uri()
            ))
            .tracker,
        )
        .unwrap();

        let issues = tracker.fetch_candidate_issues().await.unwrap();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].identifier, "openai/symphony#42");
        assert_eq!(issues[0].state, "Todo");
        assert_eq!(issues[0].priority, Some(1));
        assert_eq!(issues[0].assignees, vec!["codex-bot"]);
        assert_eq!(issues[0].labels, vec!["backend"]);
    }

    #[tokio::test]
    async fn user_owned_projects_v2_fallback_is_supported() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/graphql"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": {
                    "organization": null,
                    "user": {
                        "projectV2": {
                            "items": {
                                "pageInfo": { "hasNextPage": false, "endCursor": null },
                                "nodes": []
                            }
                        }
                    }
                }
            })))
            .mount(&server)
            .await;

        let tracker = GitHubTracker::new(
            settings(&format!(
                r#"tracker:
  kind: github
  owner: dbachko
  api_key: fake
  endpoint: {0}/graphql
  rest_endpoint: {0}
  project_v2_number: 7
  mode: projects_v2
"#,
                server.uri()
            ))
            .tracker,
        )
        .unwrap();

        let issues = tracker.fetch_candidate_issues().await.unwrap();
        assert!(issues.is_empty());
    }

    #[tokio::test]
    async fn user_owned_projects_v2_fallback_ignores_organization_not_found_errors() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/graphql"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": {
                    "organization": null,
                    "user": {
                        "projectV2": {
                            "items": {
                                "pageInfo": { "hasNextPage": false, "endCursor": null },
                                "nodes": []
                            }
                        }
                    }
                },
                "errors": [{
                    "message": "Could not resolve to an Organization with the login of 'dbachko'.",
                    "type": "NOT_FOUND",
                    "path": ["organization"]
                }]
            })))
            .mount(&server)
            .await;

        let tracker = GitHubTracker::new(
            settings(&format!(
                r#"tracker:
  kind: github
  owner: dbachko
  api_key: fake
  endpoint: {0}/graphql
  rest_endpoint: {0}
  project_v2_number: 7
  mode: projects_v2
"#,
                server.uri()
            ))
            .tracker,
        )
        .unwrap();

        let issues = tracker.fetch_candidate_issues().await.unwrap();
        assert!(issues.is_empty());
    }

    #[tokio::test]
    async fn closed_issue_overrides_project_field_status() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/graphql"))
            .and(body_string_contains("SymphonyProjectItems"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": {
                    "organization": {
                        "projectV2": {
                            "items": {
                                "pageInfo": { "hasNextPage": false, "endCursor": null },
                                "nodes": [{
                                    "id": "item-1",
                                    "status": { "__typename": "ProjectV2ItemFieldSingleSelectValue", "name": "Todo" },
                                    "priority": null,
                                    "content": {
                                        "__typename": "Issue",
                                        "id": "issue-node-1",
                                        "number": 42,
                                        "title": "Port tracker",
                                        "body": "body",
                                        "url": "https://github.com/openai/symphony/issues/42",
                                        "state": "CLOSED",
                                        "createdAt": "2026-03-13T00:00:00Z",
                                        "updatedAt": "2026-03-13T01:00:00Z",
                                        "assignees": { "nodes": [] },
                                        "labels": { "nodes": [] },
                                        "repository": {
                                            "name": "symphony",
                                            "owner": { "login": "openai" }
                                        }
                                    }
                                }]
                            }
                        }
                    }
                }
            })))
            .mount(&server)
            .await;

        let tracker = GitHubTracker::new(
            settings(&format!(
                r#"tracker:
  kind: github
  owner: openai
  api_key: fake
  endpoint: {0}/graphql
  rest_endpoint: {0}
  project_v2_number: 7
  mode: projects_v2
  status_source:
    type: project_field
    name: Status
"#,
                server.uri()
            ))
            .tracker,
        )
        .unwrap();

        let issues = tracker.fetch_candidate_issues().await.unwrap();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].state, "Closed");
    }

    #[tokio::test]
    async fn issues_only_mode_uses_issue_fields_and_blockers() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/repos/openai/symphony/issues"))
            .and(query_param("state", "open"))
            .and(query_param("page", "1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "id": 123,
                    "node_id": "issue-node-123",
                    "number": 7,
                    "title": "Implement adapter",
                    "body": "details",
                    "state": "open",
                    "html_url": "https://github.com/openai/symphony/issues/7",
                    "assignees": [{ "login": "codex-bot" }],
                    "labels": [{ "name": "p2" }],
                    "created_at": "2026-03-13T00:00:00Z",
                    "updated_at": "2026-03-13T01:00:00Z"
                }
            ])))
            .mount(&server)
            .await;

        Mock::given(method("GET"))
            .and(path("/repos/openai/symphony/issues"))
            .and(query_param("state", "open"))
            .and(query_param("page", "2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .mount(&server)
            .await;

        Mock::given(method("GET"))
            .and(path("/repos/openai/symphony/issues/7/issue-field-values"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                { "field": { "name": "Status" }, "value": "Todo" },
                { "field": { "name": "Priority" }, "value": 2 }
            ])))
            .mount(&server)
            .await;

        Mock::given(method("GET"))
            .and(path(
                "/repos/openai/symphony/issues/7/dependencies/blocked_by",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "id": 99,
                    "node_id": "blocker-99",
                    "number": 3,
                    "state": "OPEN",
                    "repository": {
                        "name": "symphony",
                        "owner": { "login": "openai" }
                    }
                }
            ])))
            .mount(&server)
            .await;

        let tracker = GitHubTracker::new(
            settings(&format!(
                r#"tracker:
  kind: github
  owner: openai
  repo: symphony
  api_key: fake
  endpoint: {0}/graphql
  rest_endpoint: {0}
  mode: issues_only
  status_source:
    type: issue_field
    name: Status
  priority_source:
    type: issue_field
    name: Priority
"#,
                server.uri()
            ))
            .tracker,
        )
        .unwrap();

        let issues = tracker.fetch_candidate_issues().await.unwrap();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].identifier, "openai/symphony#7");
        assert_eq!(issues[0].state, "Todo");
        assert_eq!(issues[0].priority, Some(2));
        assert_eq!(issues[0].assignees, vec!["codex-bot"]);
        assert_eq!(issues[0].blocked_by.len(), 1);
        assert_eq!(
            issues[0].blocked_by[0].identifier.as_deref(),
            Some("openai/symphony#3")
        );
    }

    #[tokio::test]
    async fn claim_transition_updates_project_status_to_in_progress() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/graphql"))
            .and(body_string_contains("SymphonyProjectStatusField"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": {
                    "organization": {
                        "projectV2": {
                            "id": "project-1",
                            "fields": {
                                "nodes": [{
                                    "__typename": "ProjectV2SingleSelectField",
                                    "id": "field-status",
                                    "name": "Status",
                                    "options": [
                                        { "id": "opt-todo", "name": "Todo" },
                                        { "id": "opt-progress", "name": "In Progress" }
                                    ]
                                }]
                            }
                        }
                    }
                }
            })))
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/graphql"))
            .and(body_string_contains("SymphonyUpdateProjectItemStatus"))
            .and(body_string_contains("opt-progress"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": {
                    "updateProjectV2ItemFieldValue": {
                        "projectV2Item": { "id": "item-1" }
                    }
                }
            })))
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/graphql"))
            .and(body_string_contains("SymphonyProjectItems"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": {
                    "organization": {
                        "projectV2": {
                            "items": {
                                "pageInfo": { "hasNextPage": false, "endCursor": null },
                                "nodes": [{
                                    "id": "item-1",
                                    "status": { "__typename": "ProjectV2ItemFieldSingleSelectValue", "name": "In Progress" },
                                    "priority": null,
                                    "content": {
                                        "__typename": "Issue",
                                        "id": "issue-node-1",
                                        "number": 42,
                                        "title": "Port tracker",
                                        "body": "body",
                                        "url": "https://github.com/openai/symphony/issues/42",
                                        "state": "OPEN",
                                        "createdAt": "2026-03-13T00:00:00Z",
                                        "updatedAt": "2026-03-13T01:00:00Z",
                                        "assignees": { "nodes": [] },
                                        "labels": { "nodes": [] },
                                        "repository": {
                                            "name": "symphony",
                                            "owner": { "login": "openai" }
                                        }
                                    }
                                }]
                            }
                        }
                    }
                }
            })))
            .mount(&server)
            .await;

        let tracker = GitHubTracker::new(
            settings(&format!(
                r#"tracker:
  kind: github
  owner: openai
  api_key: fake
  endpoint: {0}/graphql
  rest_endpoint: {0}
  project_v2_number: 7
  mode: projects_v2
  status_source:
    type: project_field
    name: Status
"#,
                server.uri()
            ))
            .tracker,
        )
        .unwrap();

        let issue = crate::model::Issue {
            id: "issue-node-1".to_string(),
            project_item_id: Some("item-1".to_string()),
            identifier: "openai/symphony#42".to_string(),
            title: "Port tracker".to_string(),
            description: Some("body".to_string()),
            priority: None,
            state: "Todo".to_string(),
            branch_name: None,
            url: Some("https://github.com/openai/symphony/issues/42".to_string()),
            assignees: Vec::new(),
            labels: Vec::new(),
            blocked_by: Vec::new(),
            created_at: None,
            updated_at: None,
            workpad_comment_id: None,
            workpad_comment_url: None,
            workpad_comment_body: None,
        };

        let updated = tracker
            .transition_issue_to_in_progress_on_claim(&issue)
            .await
            .unwrap();
        assert_eq!(updated.state, "In Progress");
        assert_eq!(updated.project_item_id.as_deref(), Some("item-1"));
    }

    #[tokio::test]
    async fn closed_issue_transition_updates_project_status_to_done() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/graphql"))
            .and(body_string_contains("SymphonyProjectStatusField"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": {
                    "organization": {
                        "projectV2": {
                            "id": "project-1",
                            "fields": {
                                "nodes": [{
                                    "__typename": "ProjectV2SingleSelectField",
                                    "id": "field-status",
                                    "name": "Status",
                                    "options": [
                                        { "id": "opt-todo", "name": "Todo" },
                                        { "id": "opt-done", "name": "Done" }
                                    ]
                                }]
                            }
                        }
                    }
                }
            })))
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/graphql"))
            .and(body_string_contains("SymphonyUpdateProjectItemStatus"))
            .and(body_string_contains("opt-done"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": {
                    "updateProjectV2ItemFieldValue": {
                        "projectV2Item": { "id": "item-1" }
                    }
                }
            })))
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/graphql"))
            .and(body_string_contains("SymphonyProjectItems"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": {
                    "organization": {
                        "projectV2": {
                            "items": {
                                "pageInfo": { "hasNextPage": false, "endCursor": null },
                                "nodes": [{
                                    "id": "item-1",
                                    "status": { "__typename": "ProjectV2ItemFieldSingleSelectValue", "name": "Done" },
                                    "priority": null,
                                    "content": {
                                        "__typename": "Issue",
                                        "id": "issue-node-1",
                                        "number": 42,
                                        "title": "Port tracker",
                                        "body": "body",
                                        "url": "https://github.com/openai/symphony/issues/42",
                                        "state": "CLOSED",
                                        "createdAt": "2026-03-13T00:00:00Z",
                                        "updatedAt": "2026-03-13T01:00:00Z",
                                        "assignees": { "nodes": [] },
                                        "labels": { "nodes": [] },
                                        "repository": {
                                            "name": "symphony",
                                            "owner": { "login": "openai" }
                                        }
                                    }
                                }]
                            }
                        }
                    }
                }
            })))
            .mount(&server)
            .await;

        let tracker = GitHubTracker::new(
            settings(&format!(
                r#"tracker:
  kind: github
  owner: openai
  api_key: fake
  endpoint: {0}/graphql
  rest_endpoint: {0}
  project_v2_number: 7
  mode: projects_v2
  status_source:
    type: project_field
    name: Status
"#,
                server.uri()
            ))
            .tracker,
        )
        .unwrap();

        let issue = crate::model::Issue {
            id: "issue-node-1".to_string(),
            project_item_id: Some("item-1".to_string()),
            identifier: "openai/symphony#42".to_string(),
            title: "Port tracker".to_string(),
            description: Some("body".to_string()),
            priority: None,
            state: "Closed".to_string(),
            branch_name: None,
            url: Some("https://github.com/openai/symphony/issues/42".to_string()),
            assignees: Vec::new(),
            labels: Vec::new(),
            blocked_by: Vec::new(),
            created_at: None,
            updated_at: None,
            workpad_comment_id: None,
            workpad_comment_url: None,
            workpad_comment_body: None,
        };

        let updated = tracker
            .transition_closed_issue_to_done(&issue)
            .await
            .unwrap();
        assert_eq!(updated.state, "Done");
        assert_eq!(updated.project_item_id.as_deref(), Some("item-1"));
    }

    #[tokio::test]
    async fn ensure_workpad_comment_reuses_existing_comment() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/repos/openai/symphony/issues/42/comments"))
            .and(query_param("per_page", "100"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "id": 7,
                    "body": "noise",
                    "html_url": "https://github.com/openai/symphony/issues/42#issuecomment-7"
                },
                {
                    "id": 9,
                    "body": "## Codex Workpad\n\nexisting",
                    "html_url": "https://github.com/openai/symphony/issues/42#issuecomment-9"
                }
            ])))
            .mount(&server)
            .await;

        let tracker = GitHubTracker::new(
            settings(&format!(
                r#"tracker:
  kind: github
  owner: openai
  api_key: fake
  endpoint: {0}/graphql
  rest_endpoint: {0}
  repo: symphony
  mode: issues_only
"#,
                server.uri()
            ))
            .tracker,
        )
        .unwrap();

        let issue = Issue {
            id: "issue-node-42".to_string(),
            project_item_id: None,
            identifier: "openai/symphony#42".to_string(),
            title: "Issue".to_string(),
            description: None,
            priority: None,
            state: "Todo".to_string(),
            branch_name: None,
            url: Some("https://github.com/openai/symphony/issues/42".to_string()),
            assignees: Vec::new(),
            labels: Vec::new(),
            blocked_by: Vec::new(),
            created_at: None,
            updated_at: None,
            workpad_comment_id: None,
            workpad_comment_url: None,
            workpad_comment_body: None,
        };

        let updated = tracker
            .ensure_workpad_comment(&issue, "## Codex Workpad\n\nnew")
            .await
            .unwrap();

        assert_eq!(updated.workpad_comment_id, Some(9));
        assert_eq!(
            updated.workpad_comment_url.as_deref(),
            Some("https://github.com/openai/symphony/issues/42#issuecomment-9")
        );
    }

    #[tokio::test]
    async fn ensure_workpad_comment_creates_when_missing() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/repos/openai/symphony/issues/42/comments"))
            .and(query_param("per_page", "100"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/repos/openai/symphony/issues/42/comments"))
            .and(body_string_contains("## Codex Workpad"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": 11,
                "body": "## Codex Workpad\n\ncreated",
                "html_url": "https://github.com/openai/symphony/issues/42#issuecomment-11"
            })))
            .mount(&server)
            .await;

        let tracker = GitHubTracker::new(
            settings(&format!(
                r#"tracker:
  kind: github
  owner: openai
  api_key: fake
  endpoint: {0}/graphql
  rest_endpoint: {0}
  repo: symphony
  mode: issues_only
"#,
                server.uri()
            ))
            .tracker,
        )
        .unwrap();

        let issue = Issue {
            id: "issue-node-42".to_string(),
            project_item_id: None,
            identifier: "openai/symphony#42".to_string(),
            title: "Issue".to_string(),
            description: None,
            priority: None,
            state: "Todo".to_string(),
            branch_name: None,
            url: Some("https://github.com/openai/symphony/issues/42".to_string()),
            assignees: Vec::new(),
            labels: Vec::new(),
            blocked_by: Vec::new(),
            created_at: None,
            updated_at: None,
            workpad_comment_id: None,
            workpad_comment_url: None,
            workpad_comment_body: None,
        };

        let updated = tracker
            .ensure_workpad_comment(&issue, "## Codex Workpad\n\ncreated")
            .await
            .unwrap();

        assert_eq!(updated.workpad_comment_id, Some(11));
        assert_eq!(
            updated.workpad_comment_url.as_deref(),
            Some("https://github.com/openai/symphony/issues/42#issuecomment-11")
        );
    }
}
