//! Real-time sync events broadcast over WebSocket.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use super::message::DecryptedMessage;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[serde(tag = "type")]
#[ts(export)]
#[ts(tag = "type")]
pub enum SyncEvent {
    #[serde(rename = "session-added")]
    #[ts(rename = "session-added")]
    SessionAdded {
        #[serde(rename = "sessionId")]
        session_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        namespace: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        data: Option<Value>,
    },
    #[serde(rename = "session-updated")]
    #[ts(rename = "session-updated")]
    SessionUpdated {
        #[serde(rename = "sessionId")]
        session_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        namespace: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        data: Option<Value>,
    },
    #[serde(rename = "session-removed")]
    #[ts(rename = "session-removed")]
    SessionRemoved {
        #[serde(rename = "sessionId")]
        session_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        namespace: Option<String>,
    },
    #[serde(rename = "message-received")]
    #[ts(rename = "message-received")]
    MessageReceived {
        #[serde(rename = "sessionId")]
        session_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        namespace: Option<String>,
        message: DecryptedMessage,
    },
    #[serde(rename = "message-delta")]
    #[ts(rename = "message-delta")]
    MessageDelta {
        #[serde(rename = "sessionId")]
        session_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        namespace: Option<String>,
        delta: MessageDeltaData,
    },
    #[serde(rename = "machine-updated")]
    #[ts(rename = "machine-updated")]
    MachineUpdated {
        #[serde(rename = "machineId")]
        machine_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        namespace: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        data: Option<Value>,
    },
    #[serde(rename = "toast")]
    #[ts(rename = "toast")]
    Toast {
        #[serde(skip_serializing_if = "Option::is_none")]
        namespace: Option<String>,
        data: ToastData,
    },
    #[serde(rename = "connection-changed")]
    #[ts(rename = "connection-changed")]
    ConnectionChanged {
        #[serde(skip_serializing_if = "Option::is_none")]
        namespace: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        data: Option<ConnectionChangedData>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
#[ts(rename_all = "camelCase")]
pub struct ToastData {
    pub title: String,
    pub body: String,
    pub session_id: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
#[ts(rename_all = "camelCase")]
pub struct ConnectionChangedData {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subscription_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
#[ts(rename_all = "camelCase")]
pub struct MessageDeltaData {
    pub message_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_id: Option<String>,
    pub text: String,
    pub is_final: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seq: Option<f64>,
}
