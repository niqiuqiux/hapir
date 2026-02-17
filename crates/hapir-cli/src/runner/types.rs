use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A tracked session in the runner
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackedSession {
    pub session_id: String,
    pub pid: Option<u32>,
    pub directory: String,
    pub started_by: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
}

/// Request to spawn a new session
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpawnSessionRequest {
    pub directory: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree_name: Option<String>,
    /// Agent flavor: "claude" (default), "codex", "gemini", "opencode"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flavor: Option<String>,
}

/// Response from spawning a session
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpawnSessionResponse {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requires_user_approval: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action_required: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub directory: Option<String>,
}

/// Session started webhook payload
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStartedPayload {
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
}

/// Stop session request
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StopSessionRequest {
    pub session_id: String,
}

/// List sessions response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListSessionsResponse {
    pub sessions: Vec<TrackedSession>,
}

/// Shutdown source
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownSource {
    HapiApp,
    HapiCli,
    OsSignal,
    Exception,
}
