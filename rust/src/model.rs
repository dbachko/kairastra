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

#[derive(Debug, Clone, Default)]
pub struct WorkflowDefinition {
    pub config: serde_yaml::Value,
    pub prompt_template: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HumanInputOption {
    pub label: String,
    pub description: Option<String>,
    #[serde(default)]
    pub is_other: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HumanInputQuestion {
    pub id: String,
    pub header: Option<String>,
    pub question: String,
    pub options: Vec<HumanInputOption>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HumanInputAnswer {
    pub question_id: String,
    pub selected_option: Option<String>,
    pub other_text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PendingHumanInputStatus {
    AwaitingAnswer,
    Answered,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HumanInputRequest {
    pub request_id: String,
    pub provider: String,
    pub issue_id: String,
    pub issue_identifier: String,
    pub session_id: Option<String>,
    pub requested_at: DateTime<Utc>,
    pub questions: Vec<HumanInputQuestion>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PendingHumanInput {
    pub request: HumanInputRequest,
    pub status: PendingHumanInputStatus,
    pub prompt_comment_id: Option<u64>,
    pub prompt_comment_url: Option<String>,
    pub answered_comment_id: Option<u64>,
    pub answered_by_login: Option<String>,
    pub answered_at: Option<DateTime<Utc>>,
    pub answers: Vec<HumanInputAnswer>,
}
