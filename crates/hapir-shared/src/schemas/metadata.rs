//! Session metadata and host environment descriptors.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
#[ts(rename_all = "camelCase")]
pub struct MetadataSummary {
    pub text: String,
    pub updated_at: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
#[ts(rename_all = "camelCase")]
pub struct WorktreeMetadata {
    pub base_path: String,
    pub branch: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
#[ts(rename_all = "camelCase")]
pub struct Metadata {
    pub path: String,
    pub host: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub os: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<MetadataSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub machine_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claude_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codex_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gemini_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub opencode_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slash_commands: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub home_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub happy_home_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub happy_lib_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub happy_tools_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_from_runner: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_pid: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_by: Option<StartedBy>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lifecycle_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lifecycle_state_since: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archived_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archive_reason: Option<String>,
    /// Can be null or absent
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flavor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree: Option<WorktreeMetadata>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
#[ts(rename_all = "lowercase")]
pub enum StartedBy {
    Runner,
    Terminal,
}
