//! Agent permission requests, decisions, and state tracking.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
#[ts(rename_all = "camelCase")]
pub struct AgentStateRequest {
    pub tool: String,
    pub arguments: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
#[ts(rename_all = "lowercase")]
pub enum CompletedRequestStatus {
    Canceled,
    Denied,
    Approved,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
#[ts(rename_all = "snake_case")]
pub enum PermissionDecision {
    Approved,
    ApprovedForSession,
    Denied,
    Abort,
}

/// Answers can be flat (Record<string, string[]>) or nested (Record<string, {answers: string[]}>)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[serde(untagged)]
#[ts(export)]
#[ts(untagged)]
pub enum AnswersFormat {
    Flat(HashMap<String, Vec<String>>),
    Nested(HashMap<String, NestedAnswers>),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct NestedAnswers {
    pub answers: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
#[ts(rename_all = "camelCase")]
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
#[ts(rename_all = "camelCase")]
pub struct AgentState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub controlled_by_user: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requests: Option<HashMap<String, AgentStateRequest>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_requests: Option<HashMap<String, AgentStateCompletedRequest>>,
}
