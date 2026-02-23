//! Core session aggregate type.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::modes::{ModelMode, PermissionMode};

use super::agent_state::AgentState;
use super::metadata::Metadata;
use super::todo::TodoItem;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
#[ts(rename_all = "camelCase")]
pub struct Session {
    pub id: String,
    pub namespace: String,
    pub seq: f64,
    pub created_at: f64,
    pub updated_at: f64,
    pub active: bool,
    pub active_at: f64,
    pub metadata: Option<Metadata>,
    pub metadata_version: f64,
    pub agent_state: Option<AgentState>,
    pub agent_state_version: f64,
    pub thinking: bool,
    pub thinking_at: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub todos: Option<Vec<TodoItem>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<PermissionMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_mode: Option<ModelMode>,
}
