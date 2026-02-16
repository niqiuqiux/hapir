use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::modes::{ModelMode, PermissionMode};

// --- Metadata ---

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MetadataSummary {
    pub text: String,
    pub updated_at: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeMetadata {
    pub base_path: String,
    pub branch: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Metadata {
    pub path: String,
    pub host: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub os: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<MetadataSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub machine_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claude_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codex_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gemini_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub opencode_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slash_commands: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub home_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub happy_home_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub happy_lib_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub happy_tools_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_from_runner: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_pid: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_by: Option<StartedBy>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lifecycle_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lifecycle_state_since: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archived_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archive_reason: Option<String>,
    /// Can be null or absent
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flavor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree: Option<WorktreeMetadata>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StartedBy {
    Runner,
    Terminal,
}

// --- Agent State ---

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AgentStateRequest {
    pub tool: String,
    pub arguments: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CompletedRequestStatus {
    Canceled,
    Denied,
    Approved,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionDecision {
    Approved,
    ApprovedForSession,
    Denied,
    Abort,
}

// PLACEHOLDER_SCHEMAS_PART2

/// Answers can be flat (Record<string, string[]>) or nested (Record<string, {answers: string[]}>)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum AnswersFormat {
    Flat(HashMap<String, Vec<String>>),
    Nested(HashMap<String, NestedAnswers>),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NestedAnswers {
    pub answers: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AgentStateCompletedRequest {
    pub tool: String,
    pub arguments: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<f64>,
    pub status: CompletedRequestStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision: Option<PermissionDecision>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow_tools: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answers: Option<AnswersFormat>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AgentState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub controlled_by_user: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requests: Option<HashMap<String, AgentStateRequest>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_requests: Option<HashMap<String, AgentStateCompletedRequest>>,
}

// --- Todo ---

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

// PLACEHOLDER_SCHEMAS_PART3

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TodoPriority {
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TodoItem {
    pub content: String,
    pub status: TodoStatus,
    pub priority: TodoPriority,
    pub id: String,
}

// --- Attachment ---

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentMetadata {
    pub id: String,
    pub filename: String,
    pub mime_type: String,
    pub size: f64,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview_url: Option<String>,
}

// --- Message ---

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DecryptedMessage {
    pub id: String,
    pub seq: Option<f64>,
    pub local_id: Option<String>,
    pub content: Value,
    pub created_at: f64,
}

// --- Session ---

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub id: String,
    pub namespace: String,
    pub seq: f64,
    pub created_at: f64,
    pub updated_at: f64,
    pub active: bool,
    pub active_at: f64,
    pub metadata: Option<Metadata>,
    pub metadata_version: f64,
    pub agent_state: Option<AgentState>,
    pub agent_state_version: f64,
    pub thinking: bool,
    pub thinking_at: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub todos: Option<Vec<TodoItem>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<PermissionMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_mode: Option<ModelMode>,
}

// PLACEHOLDER_SCHEMAS_PART4

// --- Sync Events ---

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum SyncEvent {
    #[serde(rename = "session-added")]
    SessionAdded {
        #[serde(rename = "sessionId")]
        session_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        namespace: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        data: Option<Value>,
    },
    #[serde(rename = "session-updated")]
    SessionUpdated {
        #[serde(rename = "sessionId")]
        session_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        namespace: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        data: Option<Value>,
    },
    #[serde(rename = "session-removed")]
    SessionRemoved {
        #[serde(rename = "sessionId")]
        session_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        namespace: Option<String>,
    },
    #[serde(rename = "message-received")]
    MessageReceived {
        #[serde(rename = "sessionId")]
        session_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        namespace: Option<String>,
        message: DecryptedMessage,
    },
    #[serde(rename = "machine-updated")]
    MachineUpdated {
        #[serde(rename = "machineId")]
        machine_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        namespace: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        data: Option<Value>,
    },
    #[serde(rename = "toast")]
    Toast {
        #[serde(skip_serializing_if = "Option::is_none")]
        namespace: Option<String>,
        data: ToastData,
    },
    #[serde(rename = "connection-changed")]
    ConnectionChanged {
        #[serde(skip_serializing_if = "Option::is_none")]
        namespace: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        data: Option<ConnectionChangedData>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ToastData {
    pub title: String,
    pub body: String,
    pub session_id: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionChangedData {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subscription_id: Option<String>,
}

// --- Tests ---

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_serde_roundtrip() {
        let meta = Metadata {
            path: "/home/user/project".into(),
            host: "myhost".into(),
            version: Some("1.0.0".into()),
            name: Some("test-session".into()),
            os: Some("linux".into()),
            summary: None,
            machine_id: Some("m1".into()),
            claude_session_id: None,
            codex_session_id: None,
            gemini_session_id: None,
            opencode_session_id: None,
            tools: Some(vec!["bash".into(), "files".into()]),
            slash_commands: None,
            home_dir: None,
            happy_home_dir: None,
            happy_lib_dir: None,
            happy_tools_dir: None,
            started_from_runner: Some(true),
            host_pid: Some(1234.0),
            started_by: Some(StartedBy::Runner),
            lifecycle_state: None,
            lifecycle_state_since: None,
            archived_by: None,
            archive_reason: None,
            flavor: Some("claude".into()),
            worktree: None,
        };
        let json = serde_json::to_string(&meta).unwrap();
        let back: Metadata = serde_json::from_str(&json).unwrap();
        assert_eq!(meta, back);
    }

    #[test]
    fn session_serde_roundtrip() {
        let session = Session {
            id: "s1".into(),
            namespace: "ns1".into(),
            seq: 5.0,
            created_at: 1000.0,
            updated_at: 2000.0,
            active: true,
            active_at: 1500.0,
            metadata: None,
            metadata_version: 1.0,
            agent_state: None,
            agent_state_version: 0.0,
            thinking: false,
            thinking_at: 0.0,
            todos: None,
            permission_mode: Some(PermissionMode::Default),
            model_mode: Some(ModelMode::Sonnet),
        };
        let json = serde_json::to_string(&session).unwrap();
        let back: Session = serde_json::from_str(&json).unwrap();
        assert_eq!(session, back);
    }

    #[test]
    fn agent_state_serde_roundtrip() {
        let state = AgentState {
            controlled_by_user: Some(true),
            requests: Some(HashMap::from([(
                "req1".into(),
                AgentStateRequest {
                    tool: "bash".into(),
                    arguments: serde_json::json!({"command": "ls"}),
                    created_at: Some(1000.0),
                },
            )])),
            completed_requests: None,
        };
        let json = serde_json::to_string(&state).unwrap();
        let back: AgentState = serde_json::from_str(&json).unwrap();
        assert_eq!(state, back);
    }

    #[test]
    fn todo_item_serde_roundtrip() {
        let todo = TodoItem {
            content: "Fix bug".into(),
            status: TodoStatus::InProgress,
            priority: TodoPriority::High,
            id: "t1".into(),
        };
        let json = serde_json::to_string(&todo).unwrap();
        let back: TodoItem = serde_json::from_str(&json).unwrap();
        assert_eq!(todo, back);
    }

    #[test]
    fn sync_event_serde_roundtrip() {
        let event = SyncEvent::SessionAdded {
            session_id: "s1".into(),
            namespace: Some("ns1".into()),
            data: None,
        };
        let json = serde_json::to_string(&event).unwrap();
        let back: SyncEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(event, back);

        let toast = SyncEvent::Toast {
            namespace: None,
            data: ToastData {
                title: "Permission".into(),
                body: "Approve?".into(),
                session_id: "s1".into(),
                url: "/sessions/s1".into(),
            },
        };
        let json = serde_json::to_string(&toast).unwrap();
        let back: SyncEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(toast, back);
    }

    #[test]
    fn decrypted_message_serde_roundtrip() {
        let msg = DecryptedMessage {
            id: "m1".into(),
            seq: Some(1.0),
            local_id: None,
            content: serde_json::json!({"role": "user", "content": "hello"}),
            created_at: 1000.0,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: DecryptedMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, back);
    }

    #[test]
    fn completed_request_with_answers() {
        let cr = AgentStateCompletedRequest {
            tool: "bash".into(),
            arguments: serde_json::json!({}),
            created_at: None,
            completed_at: None,
            status: CompletedRequestStatus::Approved,
            reason: None,
            mode: None,
            decision: Some(PermissionDecision::Approved),
            allow_tools: None,
            answers: Some(AnswersFormat::Flat(HashMap::from([(
                "q1".into(),
                vec!["a1".into()],
            )]))),
        };
        let json = serde_json::to_string(&cr).unwrap();
        let back: AgentStateCompletedRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(cr, back);
    }
}
