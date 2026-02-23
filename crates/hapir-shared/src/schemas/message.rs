//! Decrypted message and attachment types.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
#[ts(rename_all = "camelCase")]
pub struct AttachmentMetadata {
    pub id: String,
    pub filename: String,
    pub mime_type: String,
    pub size: f64,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
#[ts(rename_all = "camelCase")]
pub struct DecryptedMessage {
    pub id: String,
    pub seq: Option<f64>,
    pub local_id: Option<String>,
    pub content: Value,
    pub created_at: f64,
}
