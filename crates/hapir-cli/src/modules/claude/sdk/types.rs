use serde::{Deserialize, Serialize};
use serde_json::Value;

/// SDK message types received from the Claude process stdout.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum SdkMessage {
    #[serde(rename = "user")]
    User {
        #[serde(skip_serializing_if = "Option::is_none")]
        parent_tool_use_id: Option<String>,
        message: SdkUserMessageBody,
    },
    #[serde(rename = "assistant")]
    Assistant {
        #[serde(skip_serializing_if = "Option::is_none")]
        parent_tool_use_id: Option<String>,
        message: SdkAssistantMessageBody,
    },
    #[serde(rename = "system")]
    System {
        subtype: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        tools: Option<Vec<String>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        slash_commands: Option<Vec<String>>,
    },
    #[serde(rename = "result")]
    Result {
        subtype: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        result: Option<String>,
        num_turns: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        usage: Option<SdkUsage>,
        total_cost_usd: f64,
        duration_ms: u64,
        duration_api_ms: u64,
        is_error: bool,
        session_id: String,
    },
    #[serde(rename = "control_response")]
    ControlResponse { response: ControlResponseBody },
    #[serde(rename = "control_request")]
    ControlRequest {
        request_id: String,
        request: ControlRequestBody,
    },
    #[serde(rename = "control_cancel_request")]
    ControlCancelRequest { request_id: String },
    #[serde(rename = "log")]
    Log { log: SdkLogBody },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SdkUserMessageBody {
    pub role: String,
    pub content: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SdkAssistantMessageBody {
    pub role: String,
    pub content: Vec<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SdkUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read_input_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_creation_input_tokens: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlResponseBody {
    pub request_id: String,
    pub subtype: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlRequestBody {
    pub subtype: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SdkLogBody {
    pub level: String,
    pub message: String,
}

/// Permission result for tool calls.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "behavior")]
pub enum PermissionResult {
    #[serde(rename = "allow")]
    Allow {
        #[serde(rename = "updatedInput")]
        updated_input: Value,
    },
    #[serde(rename = "deny")]
    Deny { message: String },
}

/// Query options for spawning a Claude process.
#[derive(Debug, Clone, Default)]
pub struct QueryOptions {
    pub cwd: Option<String>,
    pub model: Option<String>,
    pub fallback_model: Option<String>,
    pub custom_system_prompt: Option<String>,
    pub append_system_prompt: Option<String>,
    pub max_turns: Option<u32>,
    pub permission_mode: Option<String>,
    pub allowed_tools: Vec<String>,
    pub disallowed_tools: Vec<String>,
    pub additional_directories: Vec<String>,
    pub mcp_servers: Option<Value>,
    pub path_to_executable: Option<String>,
    pub resume: Option<String>,
    pub continue_conversation: bool,
    pub settings_path: Option<String>,
    pub strict_mcp_config: bool,
}
