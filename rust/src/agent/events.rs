use chrono::Utc;
use serde_json::Value as JsonValue;

#[derive(Debug, Clone)]
pub struct AgentEvent {
    pub event: AgentEventKind,
    pub timestamp: chrono::DateTime<Utc>,
    pub payload: JsonValue,
    pub session_id: Option<String>,
    pub agent_process_pid: Option<String>,
}

#[derive(Debug, Clone)]
pub enum AgentEventKind {
    SessionStarted,
    Notification,
    TurnCompleted,
    TurnFailed,
    TurnCancelled,
    TurnInputRequired,
    ApprovalAutoApproved,
    ApprovalRequired,
    ToolCallCompleted,
    ToolCallFailed,
    UnsupportedToolCall,
    ToolInputAutoAnswered,
    Malformed,
    OtherMessage,
    TurnEndedWithError,
}
