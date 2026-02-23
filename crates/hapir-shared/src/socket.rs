use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SocketErrorReason {
    NamespaceMissing,
    AccessDenied,
    NotFound,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TerminalOpenPayload {
    pub session_id: String,
    pub terminal_id: String,
    pub cols: u32,
    pub rows: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TerminalWritePayload {
    pub session_id: String,
    pub terminal_id: String,
    pub data: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TerminalResizePayload {
    pub session_id: String,
    pub terminal_id: String,
    pub cols: u32,
    pub rows: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TerminalClosePayload {
    pub session_id: String,
    pub terminal_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TerminalReadyPayload {
    pub session_id: String,
    pub terminal_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TerminalOutputPayload {
    pub session_id: String,
    pub terminal_id: String,
    pub data: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TerminalExitPayload {
    pub session_id: String,
    pub terminal_id: String,
    pub code: Option<i32>,
    pub signal: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TerminalErrorPayload {
    pub session_id: String,
    pub terminal_id: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UpdateMessage {
    pub id: String,
    pub seq: f64,
    pub created_at: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_id: Option<String>,
    pub content: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VersionedValue {
    pub version: f64,
    pub value: Value,
}

/// Discriminated union for update body types.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "t")]
pub enum UpdateBody {
    #[serde(rename = "new-message")]
    NewMessage { sid: String, message: UpdateMessage },
    #[serde(rename = "update-session")]
    UpdateSession {
        sid: String,
        metadata: Option<VersionedValue>,
        #[serde(rename = "agentState")]
        agent_state: Option<VersionedValue>,
    },
    #[serde(rename = "update-machine")]
    UpdateMachine {
        #[serde(rename = "machineId")]
        machine_id: String,
        metadata: Option<VersionedValue>,
        #[serde(rename = "runnerState")]
        runner_state: Option<VersionedValue>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Update {
    pub id: String,
    pub seq: f64,
    pub body: UpdateBody,
    pub created_at: f64,
}

/// Discriminated union for versioned update ack responses.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "result")]
pub enum VersionedUpdateResponse {
    #[serde(rename = "success")]
    Success {
        version: f64,
        #[serde(flatten)]
        fields: Value,
    },
    #[serde(rename = "version-mismatch")]
    VersionMismatch {
        version: f64,
        #[serde(flatten)]
        fields: Value,
    },
    #[serde(rename = "error")]
    Error {
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<SocketErrorReason>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SocketError {
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<SocketErrorReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<SocketErrorScope>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SocketErrorScope {
    Session,
    Machine,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_payload_serde_roundtrip() {
        let open = TerminalOpenPayload {
            session_id: "s1".into(),
            terminal_id: "t1".into(),
            cols: 80,
            rows: 24,
        };
        let json = serde_json::to_string(&open).unwrap();
        let back: TerminalOpenPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(open, back);
    }

    #[test]
    fn update_body_new_message_roundtrip() {
        let body = UpdateBody::NewMessage {
            sid: "s1".into(),
            message: UpdateMessage {
                id: "m1".into(),
                seq: 1.0,
                created_at: 1000.0,
                local_id: None,
                content: serde_json::json!({"role": "user"}),
            },
        };
        let json = serde_json::to_string(&body).unwrap();
        let back: UpdateBody = serde_json::from_str(&json).unwrap();
        assert_eq!(body, back);
    }

    #[test]
    fn update_body_session_roundtrip() {
        let body = UpdateBody::UpdateSession {
            sid: "s1".into(),
            metadata: Some(VersionedValue {
                version: 2.0,
                value: serde_json::json!({"path": "/tmp"}),
            }),
            agent_state: None,
        };
        let json = serde_json::to_string(&body).unwrap();
        let back: UpdateBody = serde_json::from_str(&json).unwrap();
        assert_eq!(body, back);
    }

    #[test]
    fn update_full_roundtrip() {
        let update = Update {
            id: "u1".into(),
            seq: 1.0,
            body: UpdateBody::UpdateMachine {
                machine_id: "m1".into(),
                metadata: None,
                runner_state: None,
            },
            created_at: 1000.0,
        };
        let json = serde_json::to_string(&update).unwrap();
        let back: Update = serde_json::from_str(&json).unwrap();
        assert_eq!(update, back);
    }

    #[test]
    fn terminal_exit_with_null_fields() {
        let exit = TerminalExitPayload {
            session_id: "s1".into(),
            terminal_id: "t1".into(),
            code: None,
            signal: None,
        };
        let json = serde_json::to_string(&exit).unwrap();
        assert!(json.contains("null"));
        let back: TerminalExitPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(exit, back);
    }

    #[test]
    fn versioned_update_response_success_roundtrip() {
        let resp = VersionedUpdateResponse::Success {
            version: 3.0,
            fields: serde_json::json!({"extra": "data"}),
        };
        let json = serde_json::to_string(&resp).unwrap();
        let back: VersionedUpdateResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(resp, back);
    }

    #[test]
    fn versioned_update_response_mismatch_roundtrip() {
        let resp = VersionedUpdateResponse::VersionMismatch {
            version: 2.0,
            fields: serde_json::json!({"current": true}),
        };
        let json = serde_json::to_string(&resp).unwrap();
        let back: VersionedUpdateResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(resp, back);
    }

    #[test]
    fn versioned_update_response_error_roundtrip() {
        let resp = VersionedUpdateResponse::Error {
            reason: Some(SocketErrorReason::AccessDenied),
        };
        let json = serde_json::to_string(&resp).unwrap();
        let back: VersionedUpdateResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(resp, back);

        let resp_none = VersionedUpdateResponse::Error { reason: None };
        let json = serde_json::to_string(&resp_none).unwrap();
        let back: VersionedUpdateResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(resp_none, back);
    }

    #[test]
    fn socket_error_serde_roundtrip() {
        let err = SocketError {
            message: "not found".into(),
            code: Some(SocketErrorReason::NotFound),
            scope: Some(SocketErrorScope::Session),
            id: Some("s1".into()),
        };
        let json = serde_json::to_string(&err).unwrap();
        let back: SocketError = serde_json::from_str(&json).unwrap();
        assert_eq!(err, back);
    }
}
