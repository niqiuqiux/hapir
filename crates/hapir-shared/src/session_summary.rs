use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::modes::ModelMode;
use crate::schemas::{Session, TodoStatus, WorktreeMetadata};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
#[ts(rename_all = "camelCase")]
pub struct SessionSummaryMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub machine_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<SummaryText>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flavor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree: Option<WorktreeMetadata>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct SummaryText {
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
#[ts(rename_all = "camelCase")]
pub struct TodoProgress {
    pub completed: usize,
    pub total: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
#[ts(rename_all = "camelCase")]
pub struct SessionSummary {
    pub id: String,
    pub active: bool,
    pub thinking: bool,
    pub active_at: f64,
    pub updated_at: f64,
    pub metadata: Option<SessionSummaryMetadata>,
    pub todo_progress: Option<TodoProgress>,
    pub pending_requests_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_mode: Option<ModelMode>,
}

pub fn to_session_summary(session: &Session) -> SessionSummary {
    let pending_requests_count = session
        .agent_state
        .as_ref()
        .and_then(|s| s.requests.as_ref())
        .map(|r| r.len())
        .unwrap_or(0);

    let metadata = session.metadata.as_ref().map(|m| SessionSummaryMetadata {
        name: m.name.clone(),
        path: m.path.clone(),
        machine_id: m.machine_id.clone(),
        summary: m.summary.as_ref().map(|s| SummaryText {
            text: s.text.clone(),
        }),
        flavor: m.flavor.clone(),
        worktree: m.worktree.clone(),
    });

    let todo_progress = session.todos.as_ref().and_then(|todos| {
        if todos.is_empty() {
            None
        } else {
            Some(TodoProgress {
                completed: todos
                    .iter()
                    .filter(|t| t.status == TodoStatus::Completed)
                    .count(),
                total: todos.len(),
            })
        }
    });

    SessionSummary {
        id: session.id.clone(),
        active: session.active,
        thinking: session.thinking,
        active_at: session.active_at,
        updated_at: session.updated_at,
        metadata,
        todo_progress,
        pending_requests_count,
        model_mode: session.model_mode,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schemas::{TodoItem, TodoPriority};

    #[test]
    fn session_summary_from_session() {
        let session = Session {
            id: "s1".into(),
            namespace: "ns1".into(),
            seq: 0.0,
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
            todos: Some(vec![
                TodoItem {
                    content: "task1".into(),
                    status: TodoStatus::Completed,
                    priority: TodoPriority::High,
                    id: "t1".into(),
                },
                TodoItem {
                    content: "task2".into(),
                    status: TodoStatus::Pending,
                    priority: TodoPriority::Low,
                    id: "t2".into(),
                },
            ]),
            permission_mode: None,
            model_mode: None,
        };

        let summary = to_session_summary(&session);
        assert_eq!(summary.id, "s1");
        assert!(summary.active);
        assert_eq!(summary.pending_requests_count, 0);
        let progress = summary.todo_progress.unwrap();
        assert_eq!(progress.completed, 1);
        assert_eq!(progress.total, 2);
    }

    #[test]
    fn session_summary_serde_roundtrip() {
        let summary = SessionSummary {
            id: "s1".into(),
            active: true,
            thinking: false,
            active_at: 1000.0,
            updated_at: 2000.0,
            metadata: None,
            todo_progress: None,
            pending_requests_count: 0,
            model_mode: None,
        };
        let json = serde_json::to_string(&summary).unwrap();
        let back: SessionSummary = serde_json::from_str(&json).unwrap();
        assert_eq!(summary, back);
    }
}
