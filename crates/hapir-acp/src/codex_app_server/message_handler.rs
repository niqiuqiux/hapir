use std::collections::HashMap;

use serde_json::Value;

use crate::types::{AgentMessage, ToolCallStatus, ToolResultStatus};
use crate::utils::derive_tool_name;

fn as_str(v: &Value) -> &str {
    v.as_str().unwrap_or("")
}

struct ToolCallInfo {
    name: String,
    input: Value,
}

/// Maps Codex App Server notifications to `AgentMessage` events.
///
/// Codex uses fine-grained notifications (`item/started`, `item/completed`,
/// `item/agentMessage/delta`, `turn/completed`) rather than the ACP-style
/// `session/update` envelope.
pub struct CodexMessageHandler {
    tool_calls: HashMap<String, ToolCallInfo>,
    streaming_message_id: Option<String>,
    on_message: Box<dyn Fn(AgentMessage) + Send + Sync>,
}

impl CodexMessageHandler {
    pub fn new<F>(on_message: F) -> Self
    where
        F: Fn(AgentMessage) + Send + Sync + 'static,
    {
        Self {
            tool_calls: HashMap::new(),
            streaming_message_id: None,
            on_message: Box::new(on_message),
        }
    }

    /// Dispatch a Codex App Server notification.
    pub fn handle_notification(&mut self, method: &str, params: &Value) {
        match method {
            "item/agentMessage/delta" => self.handle_text_delta(params),
            "item/started" => self.handle_item_started(params),
            "item/completed" => self.handle_item_completed(params),
            "turn/completed" => self.handle_turn_completed(params),
            _ => {}
        }
    }

    fn finalize_stream(&mut self) {
        if let Some(message_id) = self.streaming_message_id.take() {
            (self.on_message)(AgentMessage::TextDelta {
                message_id,
                text: String::new(),
                is_final: true,
            });
        }
    }

    fn handle_text_delta(&mut self, params: &Value) {
        let text = params.get("delta").and_then(|v| v.as_str()).unwrap_or("");
        if text.is_empty() {
            return;
        }

        if self.streaming_message_id.is_none() {
            let id = params
                .get("itemId")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            self.streaming_message_id = Some(if id.is_empty() {
                uuid::Uuid::new_v4().to_string()
            } else {
                id
            });
        }

        let message_id = self.streaming_message_id.as_ref().unwrap().clone();
        (self.on_message)(AgentMessage::TextDelta {
            message_id,
            text: text.to_string(),
            is_final: false,
        });
    }

    fn handle_item_started(&mut self, params: &Value) {
        let item_type = params.get("type").map(as_str).unwrap_or("");
        if item_type != "tool_call" && item_type != "function_call" {
            return;
        }

        self.finalize_stream();

        let id = params
            .get("itemId")
            .or_else(|| params.get("id"))
            .map(as_str)
            .unwrap_or("")
            .to_string();

        let name = derive_tool_name(
            params.get("name").and_then(|v| v.as_str()),
            params.get("kind").and_then(|v| v.as_str()),
            params.get("arguments").or_else(|| params.get("rawInput")),
        );

        let input = params
            .get("arguments")
            .or_else(|| params.get("rawInput"))
            .cloned()
            .unwrap_or(Value::Null);

        self.tool_calls.insert(
            id.clone(),
            ToolCallInfo {
                name: name.clone(),
                input: input.clone(),
            },
        );

        (self.on_message)(AgentMessage::ToolCall {
            id,
            name,
            input,
            status: ToolCallStatus::InProgress,
        });
    }

    fn handle_item_completed(&mut self, params: &Value) {
        let item_type = params.get("type").map(as_str).unwrap_or("");

        match item_type {
            "tool_call" | "function_call" => {
                let id = params
                    .get("itemId")
                    .or_else(|| params.get("id"))
                    .map(as_str)
                    .unwrap_or("")
                    .to_string();

                if let Some(info) = self.tool_calls.get(&id) {
                    (self.on_message)(AgentMessage::ToolCall {
                        id: id.clone(),
                        name: info.name.clone(),
                        input: info.input.clone(),
                        status: ToolCallStatus::Completed,
                    });
                }

                let output = params
                    .get("output")
                    .or_else(|| params.get("rawOutput"))
                    .cloned()
                    .unwrap_or(Value::Null);

                (self.on_message)(AgentMessage::ToolResult {
                    id,
                    output,
                    status: ToolResultStatus::Completed,
                });
            }
            "agent_message" | "message" => {
                self.finalize_stream();
            }
            _ => {}
        }
    }

    fn handle_turn_completed(&mut self, params: &Value) {
        self.finalize_stream();

        let stop_reason = params
            .get("stopReason")
            .or_else(|| params.get("reason"))
            .and_then(|v| v.as_str())
            .unwrap_or("end_turn")
            .to_string();

        (self.on_message)(AgentMessage::TurnComplete { stop_reason });
    }
}
