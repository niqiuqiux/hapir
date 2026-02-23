use std::collections::HashMap;

use serde_json::Value;

use crate::types::{AgentMessage, ToolCallStatus, ToolResultStatus};

fn as_str(v: &Value) -> &str {
    v.as_str().unwrap_or("")
}

/// Strip provider-specific noise (url, cf-ray, request id, headers) from error messages.
fn sanitize_error(raw: &str) -> String {
    // Cut at ", url:" — everything after is Cloudflare / provider metadata
    let trimmed = raw.find(", url:").map(|i| &raw[..i]).unwrap_or(raw);

    // Strip "unexpected status " prefix for cleaner display
    let cleaned = trimmed
        .strip_prefix("unexpected status ")
        .unwrap_or(trimmed);

    cleaned.to_string()
}

struct ToolCallInfo {
    name: String,
    input: Value,
}

/// Maps Codex App Server notifications to `AgentMessage` events.
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
    /// Returns true if this was a `turn/completed` (caller should stop processing).
    pub fn handle_notification(&mut self, method: &str, params: &Value) -> bool {
        match method {
            "item/agentMessage/delta" | "item/plan/delta" => self.handle_text_delta(params),
            "item/started" => self.handle_item_started(params),
            "item/completed" => self.handle_item_completed(params),
            "error" => self.handle_error(params),
            "turn/completed" => {
                self.handle_turn_completed(params);
                return true;
            }
            _ => {}
        }
        false
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
        let item = params.get("item").unwrap_or(params);
        let item_type = item.get("type").map(as_str).unwrap_or("");

        match item_type {
            "commandExecution" | "mcpToolCall" | "fileChange" => {
                self.finalize_stream();

                let id = item.get("id").map(as_str).unwrap_or("").to_string();

                let name = match item_type {
                    "commandExecution" => item
                        .get("command")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|v| v.as_str())
                                .collect::<Vec<_>>()
                                .join(" ")
                        })
                        .unwrap_or_else(|| "command".to_string()),
                    "mcpToolCall" => {
                        let tool = item.get("tool").map(as_str).unwrap_or("mcp_tool");
                        let server = item.get("server").map(as_str).unwrap_or("");
                        if server.is_empty() {
                            tool.to_string()
                        } else {
                            format!("{server}/{tool}")
                        }
                    }
                    "fileChange" => "file_change".to_string(),
                    _ => item_type.to_string(),
                };

                let input = match item_type {
                    "commandExecution" => serde_json::json!({
                        "command": item.get("command"),
                        "cwd": item.get("cwd"),
                    }),
                    "mcpToolCall" => item.get("arguments").cloned().unwrap_or(Value::Null),
                    "fileChange" => item.get("changes").cloned().unwrap_or(Value::Null),
                    _ => Value::Null,
                };

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
            "agentMessage" => {}
            _ => {}
        }
    }

    fn handle_item_completed(&mut self, params: &Value) {
        let item = params.get("item").unwrap_or(params);
        let item_type = item.get("type").map(as_str).unwrap_or("");

        match item_type {
            "commandExecution" | "mcpToolCall" | "fileChange" => {
                let id = item.get("id").map(as_str).unwrap_or("").to_string();

                if let Some(info) = self.tool_calls.get(&id) {
                    (self.on_message)(AgentMessage::ToolCall {
                        id: id.clone(),
                        name: info.name.clone(),
                        input: info.input.clone(),
                        status: ToolCallStatus::Completed,
                    });
                }

                let output = match item_type {
                    "commandExecution" => {
                        item.get("aggregatedOutput").cloned().unwrap_or(Value::Null)
                    }
                    "mcpToolCall" => item.get("result").cloned().unwrap_or(Value::Null),
                    "fileChange" => item.get("changes").cloned().unwrap_or(Value::Null),
                    _ => Value::Null,
                };

                let status = match item.get("status").map(as_str) {
                    Some("failed") | Some("declined") => ToolResultStatus::Failed,
                    _ => ToolResultStatus::Completed,
                };

                (self.on_message)(AgentMessage::ToolResult { id, output, status });
            }
            "agentMessage" => {
                self.finalize_stream();

                let text = item
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if !text.is_empty() {
                    (self.on_message)(AgentMessage::Text { text });
                }
            }
            _ => {}
        }
    }

    fn handle_error(&self, params: &Value) {
        let will_retry = params
            .get("willRetry")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let error = params.get("error").unwrap_or(params);
        let raw = error
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("Unknown error");

        if will_retry {
            (self.on_message)(AgentMessage::ThinkingStatus {
                status: Some(sanitize_error(raw)),
            });
        }
        // willRetry=false: don't emit Error here, turn/completed will handle it
    }

    fn handle_turn_completed(&mut self, params: &Value) {
        self.finalize_stream();

        // Clear thinking status on turn end
        (self.on_message)(AgentMessage::ThinkingStatus { status: None });

        let turn = params.get("turn").unwrap_or(params);
        let status = turn
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("completed");

        if status == "failed" {
            let raw = turn
                .get("error")
                .or_else(|| turn.get("lastError"))
                .and_then(|e| {
                    e.get("message")
                        .and_then(|m| m.as_str())
                        .or_else(|| e.as_str())
                })
                .unwrap_or("Turn failed with unknown error");

            let error_message = sanitize_error(raw);

            (self.on_message)(AgentMessage::Error {
                message: error_message,
            });
        }

        let stop_reason = match status {
            "interrupted" => "interrupted",
            "failed" => "error",
            _ => "end_turn",
        }
        .to_string();

        (self.on_message)(AgentMessage::TurnComplete { stop_reason });
    }
}
