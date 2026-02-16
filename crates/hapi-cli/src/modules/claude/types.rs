use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Usage statistics for assistant messages.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_creation_input_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read_input_tokens: Option<u64>,
    pub output_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
}

/// Raw message envelope from JSONL session files.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawMessage {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

/// JSONL message types for Claude session files.
///
/// Discriminated on the `type` field. Keeps the schema lenient on
/// message content to handle SDK variations.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum RawJsonLines {
    #[serde(rename = "user")]
    User {
        uuid: String,
        message: RawMessage,
        #[serde(skip_serializing_if = "Option::is_none")]
        mode: Option<Value>,
        #[serde(rename = "toolUseResult", skip_serializing_if = "Option::is_none")]
        tool_use_result: Option<Value>,
    },
    #[serde(rename = "assistant")]
    Assistant {
        uuid: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        message: Option<RawMessage>,
        #[serde(rename = "requestId", skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
    },
    #[serde(rename = "summary")]
    Summary {
        summary: String,
        #[serde(rename = "leafUuid")]
        leaf_uuid: String,
    },
    #[serde(rename = "system")]
    System {
        uuid: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        subtype: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        tools: Option<Vec<String>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
    },
}

/// Known internal Claude Code event types that should be silently skipped.
pub const INTERNAL_CLAUDE_EVENT_TYPES: &[&str] =
    &["file-history-snapshot", "change", "queue-operation"];
