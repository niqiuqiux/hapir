use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::types::{AgentMessage, PlanItem};

/// Codex-compatible message format.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum CodexMessage {
    #[serde(rename = "message")]
    Message { message: String },
    #[serde(rename = "tool-call")]
    ToolCall {
        name: String,
        #[serde(rename = "callId")]
        call_id: String,
        input: Value,
    },
    #[serde(rename = "tool-call-result")]
    ToolCallResult {
        #[serde(rename = "callId")]
        call_id: String,
        output: Value,
    },
    #[serde(rename = "plan")]
    Plan { entries: Vec<PlanItem> },
    #[serde(rename = "error")]
    Error { message: String },
}

/// Convert an AgentMessage to a CodexMessage.
/// Returns None for turn_complete messages (they have no codex equivalent).
pub fn convert_agent_message(message: &AgentMessage) -> Option<CodexMessage> {
    match message {
        AgentMessage::Text { text } => Some(CodexMessage::Message {
            message: text.clone(),
        }),
        AgentMessage::TextDelta { .. } => None,
        AgentMessage::ToolCall {
            id,
            name,
            input,
            status: _,
        } => Some(CodexMessage::ToolCall {
            name: name.clone(),
            call_id: id.clone(),
            input: input.clone(),
        }),
        AgentMessage::ToolResult {
            id,
            output,
            status: _,
        } => Some(CodexMessage::ToolCallResult {
            call_id: id.clone(),
            output: output.clone(),
        }),
        AgentMessage::Plan { items } => Some(CodexMessage::Plan {
            entries: items.clone(),
        }),
        AgentMessage::Error { message } => Some(CodexMessage::Error {
            message: message.clone(),
        }),
        AgentMessage::TurnComplete { .. } => None,
    }
}
