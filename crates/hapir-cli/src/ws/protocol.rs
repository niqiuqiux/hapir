use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// Outgoing message (client -> server)
#[derive(Debug, Clone, Serialize)]
pub struct WsRequest {
    /// Optional request ID for messages expecting an ack
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub event: String,
    pub data: Value,
}

/// Incoming message (server -> client)
#[derive(Debug, Clone, Deserialize)]
pub struct WsMessage {
    /// Present if this is an ack response
    #[serde(default)]
    pub id: Option<String>,
    pub event: String,
    #[serde(default)]
    pub data: Value,
}

impl WsRequest {
    /// Create a request that expects an ack response
    pub fn with_ack(event: impl Into<String>, data: Value) -> (Self, String) {
        let id = Uuid::new_v4().to_string();
        let req = Self {
            id: Some(id.clone()),
            event: event.into(),
            data,
        };
        (req, id)
    }

    /// Create a fire-and-forget request (no ack)
    pub fn fire(event: impl Into<String>, data: Value) -> Self {
        Self {
            id: None,
            event: event.into(),
            data,
        }
    }
}
