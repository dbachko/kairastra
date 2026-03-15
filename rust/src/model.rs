use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BlockerRef {
    pub id: Option<String>,
    pub identifier: Option<String>,
    pub state: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Issue {
    pub id: String,
    pub project_item_id: Option<String>,
    pub identifier: String,
    pub title: String,
    pub description: Option<String>,
    pub priority: Option<i64>,
    pub state: String,
    pub branch_name: Option<String>,
    pub url: Option<String>,
    pub assignees: Vec<String>,
    pub labels: Vec<String>,
    pub blocked_by: Vec<BlockerRef>,
    pub created_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
    pub workpad_comment_id: Option<u64>,
    pub workpad_comment_url: Option<String>,
    pub workpad_comment_body: Option<String>,
}

impl Issue {
    pub fn normalized_state(&self) -> String {
        self.state.trim().to_lowercase()
    }
}

#[derive(Debug, Clone)]
pub struct WorkflowDefinition {
    pub config: serde_yaml::Value,
    pub prompt_template: String,
}
