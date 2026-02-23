pub mod agent_state;
pub mod message;
pub mod metadata;
pub mod session;
pub mod sync_event;
pub mod todo;

pub use agent_state::*;
pub use message::*;
pub use metadata::*;
pub use session::*;
pub use sync_event::*;
pub use todo::*;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn metadata_serde_roundtrip() {
        let meta = Metadata {
            path: "/home/user/project".into(),
            host: "myhost".into(),
            version: Some("1.0.0".into()),
            name: Some("test-session".into()),
            os: Some("linux".into()),
            summary: None,
            machine_id: Some("m1".into()),
            claude_session_id: None,
            codex_session_id: None,
            gemini_session_id: None,
            opencode_session_id: None,
            tools: Some(vec!["bash".into(), "files".into()]),
            slash_commands: None,
            home_dir: None,
            happy_home_dir: None,
            happy_lib_dir: None,
            happy_tools_dir: None,
            started_from_runner: Some(true),
            host_pid: Some(1234.0),
            started_by: Some(StartedBy::Runner),
            lifecycle_state: None,
            lifecycle_state_since: None,
            archived_by: None,
            archive_reason: None,
            flavor: Some("claude".into()),
            worktree: None,
        };
        let json = serde_json::to_string(&meta).unwrap();
        let back: Metadata = serde_json::from_str(&json).unwrap();
        assert_eq!(meta, back);
    }

    #[test]
    fn session_serde_roundtrip() {
        let session = Session {
            id: "s1".into(),
            namespace: "ns1".into(),
            seq: 5.0,
            created_at: 1000.0,
            updated_at: 2000.0,
            active: true,
            active_at: 1500.0,
            metadata: None,
            metadata_version: 1.0,
            agent_state: None,
            agent_state_version: 0.0,
            thinking: false,
            thinking_at: 0.0,
            thinking_status: None,
            todos: None,
            permission_mode: Some(crate::modes::PermissionMode::Default),
            model_mode: Some(crate::modes::ModelMode::Sonnet),
        };
        let json = serde_json::to_string(&session).unwrap();
        let back: Session = serde_json::from_str(&json).unwrap();
        assert_eq!(session, back);
    }

    #[test]
    fn agent_state_serde_roundtrip() {
        let state = AgentState {
            controlled_by_user: Some(true),
            requests: Some(HashMap::from([(
                "req1".into(),
                AgentStateRequest {
                    tool: "bash".into(),
                    arguments: serde_json::json!({"command": "ls"}),
                    created_at: Some(1000.0),
                },
            )])),
            completed_requests: None,
        };
        let json = serde_json::to_string(&state).unwrap();
        let back: AgentState = serde_json::from_str(&json).unwrap();
        assert_eq!(state, back);
    }

    #[test]
    fn todo_item_serde_roundtrip() {
        let todo = TodoItem {
            content: "Fix bug".into(),
            status: TodoStatus::InProgress,
            priority: TodoPriority::High,
            id: "t1".into(),
        };
        let json = serde_json::to_string(&todo).unwrap();
        let back: TodoItem = serde_json::from_str(&json).unwrap();
        assert_eq!(todo, back);
    }

    #[test]
    fn sync_event_serde_roundtrip() {
        let event = SyncEvent::SessionAdded {
            session_id: "s1".into(),
            namespace: Some("ns1".into()),
            data: None,
        };
        let json = serde_json::to_string(&event).unwrap();
        let back: SyncEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(event, back);

        let toast = SyncEvent::Toast {
            namespace: None,
            data: ToastData {
                title: "Permission".into(),
                body: "Approve?".into(),
                session_id: "s1".into(),
                url: "/sessions/s1".into(),
            },
        };
        let json = serde_json::to_string(&toast).unwrap();
        let back: SyncEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(toast, back);
    }

    #[test]
    fn decrypted_message_serde_roundtrip() {
        let msg = DecryptedMessage {
            id: "m1".into(),
            seq: Some(1.0),
            local_id: None,
            content: serde_json::json!({"role": "user", "content": "hello"}),
            created_at: 1000.0,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: DecryptedMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, back);
    }

    #[test]
    fn completed_request_with_answers() {
        let cr = AgentStateCompletedRequest {
            tool: "bash".into(),
            arguments: serde_json::json!({}),
            created_at: None,
            completed_at: None,
            status: CompletedRequestStatus::Approved,
            reason: None,
            mode: None,
            decision: Some(PermissionDecision::Approved),
            allow_tools: None,
            answers: Some(AnswersFormat::Flat(HashMap::from([(
                "q1".into(),
                vec!["a1".into()],
            )]))),
        };
        let json = serde_json::to_string(&cr).unwrap();
        let back: AgentStateCompletedRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(cr, back);
    }

    #[test]
    fn answers_format_nested_roundtrip() {
        let nested = AnswersFormat::Nested(HashMap::from([(
            "q1".into(),
            NestedAnswers {
                answers: vec!["a1".into(), "a2".into()],
            },
        )]));
        let json = serde_json::to_string(&nested).unwrap();
        let back: AnswersFormat = serde_json::from_str(&json).unwrap();
        assert_eq!(nested, back);
    }

    #[test]
    fn sync_event_message_delta_roundtrip() {
        let event = SyncEvent::MessageDelta {
            session_id: "s1".into(),
            namespace: Some("ns1".into()),
            delta: MessageDeltaData {
                message_id: "m1".into(),
                local_id: None,
                text: "hello world".into(),
                is_final: false,
                seq: Some(3.0),
            },
        };
        let json = serde_json::to_string(&event).unwrap();
        let back: SyncEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(event, back);
    }

    #[test]
    fn sync_event_connection_changed_roundtrip() {
        let event = SyncEvent::ConnectionChanged {
            namespace: None,
            data: Some(ConnectionChangedData {
                status: "connected".into(),
                subscription_id: Some("sub-1".into()),
            }),
        };
        let json = serde_json::to_string(&event).unwrap();
        let back: SyncEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(event, back);
    }
}
