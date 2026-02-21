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
    /// Whether the directory was created by the runner for this session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub directory_created: Option<bool>,
    /// User-facing message (e.g. "directory was created").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Last captured stderr output (up to 4000 chars) for debugging.
    #[serde(skip)]
    pub stderr_tail: Option<String>,
    /// Whether the session-started webhook has been received.
    /// Matches TS behavior: sessions are only listed after webhook populates happySessionId.
    #[serde(skip)]
    pub webhook_received: bool,
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
    /// Agent: "claude" (default), "codex", "gemini", "opencode"
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(alias = "flavor")]
    pub agent: Option<String>,
    /// If true, create the directory if it doesn't exist.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approved_new_directory_creation: Option<bool>,
    /// Model to pass to the agent (e.g. "claude-3-opus").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// If true, pass --yolo to the agent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub yolo: Option<bool>,
    /// Auth token to inject into the spawned process environment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    /// Session ID to resume.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resume_session_id: Option<String>,
}

/// Response from spawning a session — matches TS SpawnSessionResult shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpawnSessionResponse {
    /// "success", "requestToApproveDirectoryCreation", or "error"
    pub r#type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
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

/// List sessions response — HTTP endpoint format matching TS `{ children: [...] }`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListSessionsResponse {
    pub children: Vec<ListSessionEntry>,
}

/// Entry in the list sessions response
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListSessionEntry {
    pub started_by: String,
    pub happy_session_id: String,
    pub pid: u32,
}

/// Shutdown source
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownSource {
    HapiApp,
    HapiCli,
    OsSignal,
    Exception,
}

impl ShutdownSource {
    /// Returns the string representation matching the TypeScript implementation.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::HapiApp => "hapi-app",
            Self::HapiCli => "hapi-cli",
            Self::OsSignal => "os-signal",
            Self::Exception => "exception",
        }
    }
}
