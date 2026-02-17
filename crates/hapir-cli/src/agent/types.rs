use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};
use serde_json::Value;

// --- MCP types ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpEnvVar {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerStdio {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: Vec<McpEnvVar>,
}

// --- Session config ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSessionConfig {
    pub cwd: String,
    pub mcp_servers: Vec<McpServerStdio>,
}

// --- Prompt content ---

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum PromptContent {
    #[serde(rename = "text")]
    Text { text: String },
}

// --- Plan item ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanItem {
    pub content: String,
    pub priority: PlanItemPriority,
    pub status: PlanItemStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanItemPriority {
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanItemStatus {
    Pending,
    InProgress,
    Completed,
}

// --- Tool call status ---

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolResultStatus {
    Completed,
    Failed,
}

// --- Agent message ---

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AgentMessage {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_call")]
    ToolCall {
        id: String,
        name: String,
        input: Value,
        status: ToolCallStatus,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        id: String,
        output: Value,
        status: ToolResultStatus,
    },
    #[serde(rename = "plan")]
    Plan { items: Vec<PlanItem> },
    #[serde(rename = "turn_complete")]
    TurnComplete {
        #[serde(rename = "stopReason")]
        stop_reason: String,
    },
    #[serde(rename = "error")]
    Error { message: String },
}

// --- Permission types ---

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionOption {
    pub option_id: String,
    pub name: String,
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionRequest {
    pub id: String,
    pub session_id: String,
    pub tool_call_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_input: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_output: Option<Value>,
    pub options: Vec<PermissionOption>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "outcome")]
pub enum PermissionResponse {
    #[serde(rename = "selected")]
    Selected {
        #[serde(rename = "optionId")]
        option_id: String,
    },
    #[serde(rename = "cancelled")]
    Cancelled,
}

// --- Agent backend trait ---

pub type OnUpdateFn = Box<dyn Fn(AgentMessage) + Send + Sync>;
pub type OnPermissionRequestFn = Box<dyn Fn(PermissionRequest) + Send + Sync>;

pub trait AgentBackend: Send + Sync {
    fn initialize(&self) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + '_>>;

    fn new_session(
        &self,
        config: AgentSessionConfig,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<String>> + Send + '_>>;

    fn prompt(
        &self,
        session_id: &str,
        content: Vec<PromptContent>,
        on_update: OnUpdateFn,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + '_>>;

    fn cancel_prompt(
        &self,
        session_id: &str,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + '_>>;

    fn respond_to_permission(
        &self,
        session_id: &str,
        request: &PermissionRequest,
        response: PermissionResponse,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + '_>>;

    fn on_permission_request(&self, handler: OnPermissionRequestFn);

    fn disconnect(&self) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + '_>>;
}

// --- Factory type ---

pub type AgentBackendFactory = Box<dyn Fn() -> Box<dyn AgentBackend> + Send + Sync>;
