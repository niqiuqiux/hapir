//! Shared request/response types for the CLI HTTP API (`/cli/*` routes).

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::message::DecryptedMessage;
use super::session::Session;

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateSessionRequest {
    pub tag: String,
    pub metadata: Value,
    pub agent_state: Option<Value>,
    pub namespace: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateSessionResponse {
    pub session: Session,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateMachineRequest {
    pub id: String,
    pub metadata: Value,
    pub runner_state: Option<Value>,
    pub namespace: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateMachineResponse {
    pub machine: Value,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListMessagesQuery {
    pub after_seq: i64,
    pub limit: Option<i64>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListMessagesResponse {
    pub messages: Vec<DecryptedMessage>,
}
