use std::collections::HashMap;

use serde_json::Value;

use crate::agent::types::{
    AgentMessage, PlanItem, PlanItemPriority, PlanItemStatus, ToolCallStatus, ToolResultStatus,
};
use crate::agent::utils::derive_tool_name;

use super::constants;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn as_str(v: &Value) -> Option<&str> {
    v.as_str()
}

fn normalize_status(status: Option<&str>) -> ToolCallStatus {
    match status {
        Some("in_progress") => ToolCallStatus::InProgress,
        Some("completed") => ToolCallStatus::Completed,
        Some("failed") => ToolCallStatus::Failed,
        _ => ToolCallStatus::Pending,
    }
}

fn derive_tool_name_from_update(update: &Value) -> String {
    let title = update.get("title").and_then(as_str);
    let kind = update.get("kind").and_then(as_str);
    let raw_input = update.get("rawInput");
    derive_tool_name(title, kind, raw_input)
}

fn extract_text_content(block: &Value) -> Option<&str> {
    let obj = block.as_object()?;
    if obj.get("type")?.as_str()? != "text" {
        return None;
    }
    obj.get("text")?.as_str()
}

fn normalize_plan_entries(entries: &Value) -> Vec<PlanItem> {
    let arr = match entries.as_array() {
        Some(a) => a,
        None => return Vec::new(),
    };

    let mut items = Vec::new();
    for entry in arr {
        let obj = match entry.as_object() {
            Some(o) => o,
            None => continue,
        };

        let content = match obj.get("content").and_then(|v| v.as_str()) {
            Some(c) if !c.is_empty() => c.to_string(),
            _ => continue,
        };

        let priority = match obj.get("priority").and_then(|v| v.as_str()) {
            Some("high") => PlanItemPriority::High,
            Some("medium") => PlanItemPriority::Medium,
            Some("low") => PlanItemPriority::Low,
            _ => continue,
        };

        let status = match obj.get("status").and_then(|v| v.as_str()) {
            Some("pending") => PlanItemStatus::Pending,
            Some("in_progress") => PlanItemStatus::InProgress,
            Some("completed") => PlanItemStatus::Completed,
            _ => continue,
        };

        items.push(PlanItem {
            content,
            priority,
            status,
        });
    }

    items
}

// ---------------------------------------------------------------------------
// AcpMessageHandler
// ---------------------------------------------------------------------------

/// Parses ACP session update notifications into `AgentMessage` events.
///
/// Buffers text chunks with deduplication logic and handles tool call
/// lifecycle events (creation, updates, completion/failure).
pub struct AcpMessageHandler {
    tool_calls: HashMap<String, ToolCallInfo>,
    buffered_text: String,
    on_message: Box<dyn Fn(AgentMessage) + Send + Sync>,
}

struct ToolCallInfo {
    name: String,
    input: Value,
}

impl AcpMessageHandler {
    pub fn new<F>(on_message: F) -> Self
    where
        F: Fn(AgentMessage) + Send + Sync + 'static,
    {
        Self {
            tool_calls: HashMap::new(),
            buffered_text: String::new(),
            on_message: Box::new(on_message),
        }
    }

    /// Emit any buffered text as an `AgentMessage::Text`.
    pub fn flush_text(&mut self) {
        if self.buffered_text.is_empty() {
            return;
        }
        let text = std::mem::take(&mut self.buffered_text);
        (self.on_message)(AgentMessage::Text { text });
    }

    /// Append a text chunk with deduplication.
    ///
    /// - If new text equals buffered: skip.
    /// - If new text starts with buffered: replace (it's a superset).
    /// - If buffered starts with new text: skip (buffered is already longer).
    /// - Otherwise: concatenate.
    fn append_text_chunk(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if self.buffered_text.is_empty() {
            self.buffered_text = text.to_string();
            return;
        }
        if text == self.buffered_text {
            return;
        }
        if text.starts_with(&self.buffered_text) {
            self.buffered_text = text.to_string();
            return;
        }
        if self.buffered_text.starts_with(text) {
            return;
        }
        self.buffered_text.push_str(text);
    }

    /// Process a single session update notification.
    pub fn handle_update(&mut self, update: &Value) {
        let obj = match update.as_object() {
            Some(o) => o,
            None => return,
        };

        let update_type = match obj.get("sessionUpdate").and_then(|v| v.as_str()) {
            Some(t) => t,
            None => return,
        };

        match update_type {
            constants::AGENT_MESSAGE_CHUNK => {
                if let Some(content) = obj.get("content") {
                    if let Some(text) = extract_text_content(content) {
                        self.append_text_chunk(text);
                    }
                }
            }
            constants::AGENT_THOUGHT_CHUNK => {
                // Silently ignored
            }
            constants::TOOL_CALL => {
                self.flush_text();
                self.handle_tool_call(update);
            }
            constants::TOOL_CALL_UPDATE => {
                self.flush_text();
                self.handle_tool_call_update(update);
            }
            constants::PLAN => {
                self.flush_text();
                if let Some(entries) = obj.get("entries") {
                    let items = normalize_plan_entries(entries);
                    if !items.is_empty() {
                        (self.on_message)(AgentMessage::Plan { items });
                    }
                }
            }
            _ => {}
        }
    }

    fn handle_tool_call(&mut self, update: &Value) {
        let tool_call_id = match update.get("toolCallId").and_then(as_str) {
            Some(id) => id.to_string(),
            None => return,
        };

        let name = derive_tool_name_from_update(update);
        let input = update.get("rawInput").cloned().unwrap_or(Value::Null);
        let status = normalize_status(update.get("status").and_then(as_str));

        self.tool_calls.insert(
            tool_call_id.clone(),
            ToolCallInfo {
                name: name.clone(),
                input: input.clone(),
            },
        );

        (self.on_message)(AgentMessage::ToolCall {
            id: tool_call_id,
            name,
            input,
            status,
        });
    }

    fn handle_tool_call_update(&mut self, update: &Value) {
        let tool_call_id = match update.get("toolCallId").and_then(as_str) {
            Some(id) => id.to_string(),
            None => return,
        };

        let status = normalize_status(update.get("status").and_then(as_str));

        if update.get("rawInput").is_some() {
            let name = derive_tool_name_from_update(update);
            let input = update.get("rawInput").cloned().unwrap_or(Value::Null);
            self.tool_calls.insert(
                tool_call_id.clone(),
                ToolCallInfo {
                    name: name.clone(),
                    input: input.clone(),
                },
            );
            (self.on_message)(AgentMessage::ToolCall {
                id: tool_call_id.clone(),
                name,
                input,
                status,
            });
        } else if let Some(existing) = self.tool_calls.get(&tool_call_id) {
            if status == ToolCallStatus::InProgress || status == ToolCallStatus::Pending {
                (self.on_message)(AgentMessage::ToolCall {
                    id: tool_call_id.clone(),
                    name: existing.name.clone(),
                    input: existing.input.clone(),
                    status,
                });
            }
        }

        if status == ToolCallStatus::Completed || status == ToolCallStatus::Failed {
            let output = update
                .get("rawOutput")
                .or_else(|| update.get("content"))
                .cloned()
                .unwrap_or_else(|| serde_json::json!({ "status": status }));

            let result_status = if status == ToolCallStatus::Failed {
                ToolResultStatus::Failed
            } else {
                ToolResultStatus::Completed
            };

            (self.on_message)(AgentMessage::ToolResult {
                id: tool_call_id,
                output,
                status: result_status,
            });
        }
    }
}