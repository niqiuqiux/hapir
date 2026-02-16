//! WebSocket message protocol (replaces Socket.IO).
//!
//! Request (with ack):  `{"id": "uuid", "event": "session-alive", "data": {...}}`
//! Response (ack):      `{"id": "uuid", "event": "session-alive:ack", "data": {...}}`
//! One-way event:       `{"event": "update", "data": {...}}`

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A WebSocket message envelope.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WsMessage {
    /// Present for request/response pairs (ack pattern).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Event name. Ack responses use `"{event}:ack"`.
    pub event: String,
    /// Event payload.
    pub data: Value,
}

impl WsMessage {
    /// Create a one-way event (no ack expected).
    pub fn event(event: impl Into<String>, data: Value) -> Self {
        Self {
            id: None,
            event: event.into(),
            data,
        }
    }

    /// Create a request that expects an ack response.
    pub fn request(id: impl Into<String>, event: impl Into<String>, data: Value) -> Self {
        Self {
            id: Some(id.into()),
            event: event.into(),
            data,
        }
    }

    /// Create an ack response for a given request.
    pub fn ack(id: impl Into<String>, event: &str, data: Value) -> Self {
        Self {
            id: Some(id.into()),
            event: format!("{event}:ack"),
            data,
        }
    }

    /// Check if this message is an ack response.
    pub fn is_ack(&self) -> bool {
        self.event.ends_with(":ack")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ws_message_event_roundtrip() {
        let msg = WsMessage::event("update", json!({"sid": "s1"}));
        let json_str = serde_json::to_string(&msg).unwrap();
        let back: WsMessage = serde_json::from_str(&json_str).unwrap();
        assert_eq!(msg, back);
        assert!(msg.id.is_none());
    }

    #[test]
    fn ws_message_request_ack_roundtrip() {
        let req = WsMessage::request("req-1", "session-alive", json!({"sid": "s1"}));
        assert!(!req.is_ack());

        let ack = WsMessage::ack("req-1", "session-alive", json!({"ok": true}));
        assert!(ack.is_ack());
        assert_eq!(ack.event, "session-alive:ack");

        let json_str = serde_json::to_string(&ack).unwrap();
        let back: WsMessage = serde_json::from_str(&json_str).unwrap();
        assert_eq!(ack, back);
    }
}
