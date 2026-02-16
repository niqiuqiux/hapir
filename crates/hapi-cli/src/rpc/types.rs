use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcRequest {
    pub method: String,
    pub params: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Options for spawning a session via RPC
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpawnSessionOptions {
    pub directory: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree_name: Option<String>,
}

/// Options for stopping a session via RPC
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StopSessionOptions {
    pub session_id: String,
}

/// Path existence check request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathExistsRequest {
    pub paths: Vec<String>,
}

/// Path existence check response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathExistsResponse {
    pub results: Vec<bool>,
}
