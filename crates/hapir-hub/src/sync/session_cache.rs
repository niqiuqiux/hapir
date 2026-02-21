use std::collections::{HashMap, HashSet};
use std::time::{SystemTime, UNIX_EPOCH};

use hapir_shared::modes::{ModelMode, PermissionMode};
use hapir_shared::schemas::{AgentState, Metadata, Session, SyncEvent, TodoItem};
use serde_json::Value;

use super::alive_time::clamp_alive_time;
use super::event_publisher::EventPublisher;
use super::todos::extract_todos_from_message_content;
use crate::store::Store;

const SESSION_TIMEOUT_MS: i64 = 30_000;

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

pub struct SessionCache {
    sessions: HashMap<String, Session>,
    last_broadcast_at: HashMap<String, i64>,
    todo_backfill_attempted: HashSet<String>,
}

impl SessionCache {
    pub fn new() -> Self {
        Self {
            sessions: HashMap::new(),
            last_broadcast_at: HashMap::new(),
            todo_backfill_attempted: HashSet::new(),
        }
    }

    pub fn get_sessions(&self) -> Vec<Session> {
        self.sessions.values().cloned().collect()
    }

    pub fn get_sessions_by_namespace(&self, namespace: &str) -> Vec<Session> {
        self.sessions
            .values()
            .filter(|s| s.namespace == namespace)
            .cloned()
            .collect()
    }

    pub fn get_session(&self, session_id: &str) -> Option<&Session> {
        self.sessions.get(session_id)
    }

    pub fn get_session_by_namespace(&self, session_id: &str, namespace: &str) -> Option<&Session> {
        self.sessions
            .get(session_id)
            .filter(|s| s.namespace == namespace)
    }

    pub fn resolve_session_access(
        &mut self,
        session_id: &str,
        namespace: &str,
        store: &Store,
        publisher: &EventPublisher,
    ) -> Result<(String, Session), &'static str> {
        let session = self
            .sessions
            .get(session_id)
            .cloned()
            .or_else(|| self.refresh_session(session_id, store, publisher));

        match session {
            Some(s) if s.namespace != namespace => Err("access-denied"),
            Some(s) => Ok((session_id.to_string(), s)),
            None => Err("not-found"),
        }
    }

    pub fn get_active_sessions(&self) -> Vec<Session> {
        self.sessions
            .values()
            .filter(|s| s.active)
            .cloned()
            .collect()
    }

    pub fn get_or_create_session(
        &mut self,
        tag: &str,
        metadata: &Value,
        agent_state: Option<&Value>,
        namespace: &str,
        store: &Store,
        publisher: &EventPublisher,
    ) -> anyhow::Result<Session> {
        use crate::store::sessions;
        let stored =
            sessions::get_or_create_session(&store.conn(), tag, metadata, agent_state, namespace)?;
        self.refresh_session(&stored.id, store, publisher)
            .ok_or_else(|| anyhow::anyhow!("failed to load session"))
    }

    pub fn refresh_session(
        &mut self,
        session_id: &str,
        store: &Store,
        publisher: &EventPublisher,
    ) -> Option<Session> {
        use crate::store::{messages, sessions};

        let mut stored = match sessions::get_session(&store.conn(), session_id) {
            Some(s) => s,
            None => {
                if let Some(removed) = self.sessions.remove(session_id) {
                    self.last_broadcast_at.remove(session_id);
                    self.todo_backfill_attempted.remove(session_id);
                    publisher.emit(SyncEvent::SessionRemoved {
                        session_id: session_id.to_string(),
                        namespace: Some(removed.namespace),
                    });
                }
                return None;
            }
        };

        let existing = self.sessions.get(session_id);

        // Todo backfill: scan messages for TodoWrite if todos are null
        if stored.todos.is_none() && !self.todo_backfill_attempted.contains(session_id) {
            self.todo_backfill_attempted.insert(session_id.to_string());
            let msgs = messages::get_messages(&store.conn(), session_id, 200, None);
            for msg in msgs.iter().rev() {
                if let Some(content) = &msg.content
                    && let Some(todos) = extract_todos_from_message_content(content)
                {
                    let todos_value = serde_json::to_value(&todos).ok();
                    if let Some(tv) = &todos_value
                        && sessions::set_session_todos(
                            &store.conn(),
                            session_id,
                            Some(tv),
                            msg.created_at,
                            &stored.namespace,
                        )
                    {
                        // Re-read stored session
                        if let Some(refreshed) = sessions::get_session(&store.conn(), session_id) {
                            stored = refreshed;
                        }
                    }
                    break;
                }
            }
        }

        // Parse metadata
        let metadata: Option<Metadata> = stored
            .metadata
            .as_ref()
            .and_then(|v| serde_json::from_value(v.clone()).ok());

        // Parse agent state
        let agent_state: Option<AgentState> = stored
            .agent_state
            .as_ref()
            .and_then(|v| serde_json::from_value(v.clone()).ok());

        // Parse todos
        let todos: Option<Vec<TodoItem>> = stored
            .todos
            .as_ref()
            .and_then(|v| serde_json::from_value(v.clone()).ok());

        let is_update = existing.is_some();

        let session = Session {
            id: stored.id.clone(),
            namespace: stored.namespace.clone(),
            seq: stored.seq as f64,
            created_at: stored.created_at as f64,
            updated_at: stored.updated_at as f64,
            active: existing.map(|e| e.active).unwrap_or(stored.active),
            active_at: existing
                .map(|e| e.active_at)
                .unwrap_or(stored.active_at.unwrap_or(stored.created_at) as f64),
            metadata,
            metadata_version: stored.metadata_version as f64,
            agent_state,
            agent_state_version: stored.agent_state_version as f64,
            thinking: existing.map(|e| e.thinking).unwrap_or(false),
            thinking_at: existing.map(|e| e.thinking_at).unwrap_or(0.0),
            todos,
            permission_mode: existing.and_then(|e| e.permission_mode),
            model_mode: existing.and_then(|e| e.model_mode),
        };

        self.sessions
            .insert(session_id.to_string(), session.clone());

        let event_data = serde_json::to_value(&session).ok();
        if is_update {
            publisher.emit(SyncEvent::SessionUpdated {
                session_id: session_id.to_string(),
                namespace: Some(session.namespace.clone()),
                data: event_data,
            });
        } else {
            publisher.emit(SyncEvent::SessionAdded {
                session_id: session_id.to_string(),
                namespace: Some(session.namespace.clone()),
                data: event_data,
            });
        }

        Some(session)
    }

    // PLACEHOLDER_SESSION_CACHE_PART2

    pub fn reload_all(&mut self, store: &Store, publisher: &EventPublisher) {
        use crate::store::sessions;
        let all = sessions::get_sessions(&store.conn());
        for s in all {
            self.refresh_session(&s.id, store, publisher);
        }
    }

    pub fn handle_session_alive(
        &mut self,
        sid: &str,
        time: i64,
        thinking: Option<bool>,
        permission_mode: Option<PermissionMode>,
        model_mode: Option<ModelMode>,
        store: &Store,
        publisher: &EventPublisher,
    ) {
        let t = match clamp_alive_time(time) {
            Some(t) => t,
            None => return,
        };

        if !self.sessions.contains_key(sid) {
            self.refresh_session(sid, store, publisher);
        }
        let session = match self.sessions.get_mut(sid) {
            Some(s) => s,
            None => return,
        };

        let was_active = session.active;
        let was_thinking = session.thinking;
        let prev_perm = session.permission_mode;
        let prev_model = session.model_mode;

        session.active = true;
        session.active_at = session.active_at.max(t as f64);
        session.thinking = thinking.unwrap_or(false);
        session.thinking_at = t as f64;
        if let Some(pm) = permission_mode {
            session.permission_mode = Some(pm);
        }
        if let Some(mm) = model_mode {
            session.model_mode = Some(mm);
        }

        let now = now_millis();
        let last = self.last_broadcast_at.get(sid).copied().unwrap_or(0);
        let mode_changed = prev_perm != session.permission_mode || prev_model != session.model_mode;
        let should_broadcast = (!was_active && session.active)
            || (was_thinking != session.thinking)
            || mode_changed
            || (now - last > 10_000);

        if should_broadcast {
            self.last_broadcast_at.insert(sid.to_string(), now);
            let data = serde_json::json!({
                "activeAt": session.active_at,
                "thinking": session.thinking,
                "permissionMode": session.permission_mode,
                "modelMode": session.model_mode,
            });
            publisher.emit(SyncEvent::SessionUpdated {
                session_id: sid.to_string(),
                namespace: Some(session.namespace.clone()),
                data: Some(data),
            });
        }
    }

    pub fn handle_session_end(
        &mut self,
        sid: &str,
        time: i64,
        store: &Store,
        publisher: &EventPublisher,
    ) {
        let t = clamp_alive_time(time).unwrap_or_else(now_millis);

        if !self.sessions.contains_key(sid) {
            self.refresh_session(sid, store, publisher);
        }
        let session = match self.sessions.get_mut(sid) {
            Some(s) => s,
            None => return,
        };

        if !session.active && !session.thinking {
            return;
        }

        session.active = false;
        session.thinking = false;
        session.thinking_at = t as f64;

        publisher.emit(SyncEvent::SessionUpdated {
            session_id: sid.to_string(),
            namespace: Some(session.namespace.clone()),
            data: Some(serde_json::json!({"active": false, "thinking": false})),
        });
    }

    pub fn expire_inactive(&mut self, publisher: &EventPublisher) {
        let now = now_millis();
        let expired: Vec<String> = self
            .sessions
            .iter()
            .filter(|(_, s)| s.active && (now - s.active_at as i64) > SESSION_TIMEOUT_MS)
            .map(|(id, _)| id.clone())
            .collect();

        if !expired.is_empty() {
            tracing::info!(count = expired.len(), "expiring inactive sessions");
        }
        for id in expired {
            if let Some(session) = self.sessions.get_mut(&id) {
                tracing::debug!(session_id = %id, "session expired due to inactivity");
                session.active = false;
                session.thinking = false;
                publisher.emit(SyncEvent::SessionUpdated {
                    session_id: id,
                    namespace: Some(session.namespace.clone()),
                    data: Some(serde_json::json!({"active": false})),
                });
            }
        }
    }

    pub fn apply_session_config(
        &mut self,
        session_id: &str,
        permission_mode: Option<PermissionMode>,
        model_mode: Option<ModelMode>,
        store: &Store,
        publisher: &EventPublisher,
    ) {
        if !self.sessions.contains_key(session_id) {
            self.refresh_session(session_id, store, publisher);
        }
        let session = match self.sessions.get_mut(session_id) {
            Some(s) => s,
            None => return,
        };

        if let Some(pm) = permission_mode {
            session.permission_mode = Some(pm);
        }
        if let Some(mm) = model_mode {
            session.model_mode = Some(mm);
        }

        let data = serde_json::to_value(&*session).ok();
        publisher.emit(SyncEvent::SessionUpdated {
            session_id: session_id.to_string(),
            namespace: Some(session.namespace.clone()),
            data,
        });
    }

    pub fn rename_session(
        &mut self,
        session_id: &str,
        name: &str,
        store: &Store,
        publisher: &EventPublisher,
    ) -> anyhow::Result<()> {
        use crate::store::sessions;

        let session = self
            .sessions
            .get(session_id)
            .ok_or_else(|| anyhow::anyhow!("session not found"))?;

        let current_meta = session.metadata.clone().unwrap_or_else(|| Metadata {
            path: String::new(),
            host: String::new(),
            version: None,
            name: None,
            os: None,
            summary: None,
            machine_id: None,
            claude_session_id: None,
            codex_session_id: None,
            gemini_session_id: None,
            opencode_session_id: None,
            tools: None,
            slash_commands: None,
            home_dir: None,
            happy_home_dir: None,
            happy_lib_dir: None,
            happy_tools_dir: None,
            started_from_runner: None,
            host_pid: None,
            started_by: None,
            lifecycle_state: None,
            lifecycle_state_since: None,
            archived_by: None,
            archive_reason: None,
            flavor: None,
            worktree: None,
        });

        let mut new_meta = current_meta;
        new_meta.name = Some(name.to_string());
        let new_meta_value = serde_json::to_value(&new_meta)?;

        let version = session.metadata_version as i64;
        let namespace = session.namespace.clone();

        let result = sessions::update_session_metadata(
            &store.conn(),
            session_id,
            &new_meta_value,
            version,
            &namespace,
            false,
        );

        use crate::store::types::VersionedUpdateResult;
        match result {
            VersionedUpdateResult::Success { .. } => {
                self.refresh_session(session_id, store, publisher);
                Ok(())
            }
            VersionedUpdateResult::VersionMismatch { .. } => {
                Err(anyhow::anyhow!("session was modified concurrently"))
            }
            VersionedUpdateResult::Error => {
                Err(anyhow::anyhow!("failed to update session metadata"))
            }
        }
    }

    // PLACEHOLDER_SESSION_CACHE_PART3

    pub fn delete_session(
        &mut self,
        session_id: &str,
        store: &Store,
        publisher: &EventPublisher,
    ) -> anyhow::Result<()> {
        use crate::store::sessions;

        let session = self
            .sessions
            .get(session_id)
            .ok_or_else(|| anyhow::anyhow!("session not found"))?;

        if session.active {
            return Err(anyhow::anyhow!("cannot delete active session"));
        }

        let namespace = session.namespace.clone();

        if !sessions::delete_session(&store.conn(), session_id, &namespace) {
            return Err(anyhow::anyhow!("failed to delete session"));
        }

        self.sessions.remove(session_id);
        self.last_broadcast_at.remove(session_id);
        self.todo_backfill_attempted.remove(session_id);

        publisher.emit(SyncEvent::SessionRemoved {
            session_id: session_id.to_string(),
            namespace: Some(namespace),
        });

        Ok(())
    }

    pub fn merge_sessions(
        &mut self,
        old_session_id: &str,
        new_session_id: &str,
        namespace: &str,
        store: &Store,
        publisher: &EventPublisher,
    ) -> anyhow::Result<()> {
        use crate::store::{messages, sessions};

        if old_session_id == new_session_id {
            return Ok(());
        }

        let old_stored =
            sessions::get_session_by_namespace(&store.conn(), old_session_id, namespace)
                .ok_or_else(|| anyhow::anyhow!("old session not found"))?;
        let new_stored =
            sessions::get_session_by_namespace(&store.conn(), new_session_id, namespace)
                .ok_or_else(|| anyhow::anyhow!("new session not found"))?;

        // Merge messages
        let merge_result =
            messages::merge_session_messages(&store.conn(), old_session_id, new_session_id)?;
        tracing::info!(
            old_session_id = old_session_id,
            new_session_id = new_session_id,
            moved = merge_result.0,
            old_max_seq = merge_result.1,
            new_max_seq = merge_result.2,
            "[mergeSessions] messages merged"
        );

        // Merge metadata
        if let (Some(old_meta), Some(new_meta)) = (&old_stored.metadata, &new_stored.metadata)
            && let Some(merged) = merge_metadata(old_meta, new_meta)
        {
            // Try up to 2 times for optimistic concurrency
            for _ in 0..2 {
                if let Some(latest) =
                    sessions::get_session_by_namespace(&store.conn(), new_session_id, namespace)
                {
                    let result = sessions::update_session_metadata(
                        &store.conn(),
                        new_session_id,
                        &merged,
                        latest.metadata_version,
                        namespace,
                        false,
                    );
                    use crate::store::types::VersionedUpdateResult;
                    match result {
                        VersionedUpdateResult::Success { .. } => break,
                        VersionedUpdateResult::Error => break,
                        _ => continue,
                    }
                }
            }
        }

        // Merge todos
        if old_stored.todos.is_some()
            && let Some(ts) = old_stored.todos_updated_at
        {
            sessions::set_session_todos(
                &store.conn(),
                new_session_id,
                old_stored.todos.as_ref(),
                ts,
                namespace,
            );
        }

        // Delete old session
        tracing::info!(
            old_session_id = old_session_id,
            new_session_id = new_session_id,
            "[mergeSessions] deleting old session"
        );
        if !sessions::delete_session(&store.conn(), old_session_id, namespace) {
            return Err(anyhow::anyhow!("failed to delete old session during merge"));
        }

        if self.sessions.remove(old_session_id).is_some() {
            publisher.emit(SyncEvent::SessionRemoved {
                session_id: old_session_id.to_string(),
                namespace: Some(namespace.to_string()),
            });
        }
        self.last_broadcast_at.remove(old_session_id);
        self.todo_backfill_attempted.remove(old_session_id);

        self.refresh_session(new_session_id, store, publisher);
        Ok(())
    }
}

/// Merge old metadata into new, preserving name/summary/worktree/path/host from old if missing in new.
fn merge_metadata(old: &Value, new: &Value) -> Option<Value> {
    let old_obj = old.as_object()?;
    let new_obj = new.as_object()?;
    let mut merged = new_obj.clone();
    let mut changed = false;

    // Preserve name
    if old_obj.get("name").and_then(|v| v.as_str()).is_some()
        && new_obj.get("name").and_then(|v| v.as_str()).is_none()
    {
        merged.insert("name".into(), old_obj["name"].clone());
        changed = true;
    }

    // Preserve summary if newer
    let old_updated = old_obj
        .get("summary")
        .and_then(|s| s.get("updatedAt"))
        .and_then(|v| v.as_f64());
    let new_updated = new_obj
        .get("summary")
        .and_then(|s| s.get("updatedAt"))
        .and_then(|v| v.as_f64());
    if let Some(old_t) = old_updated
        && (new_updated.is_none() || old_t > new_updated.unwrap_or(0.0))
        && let Some(summary) = old_obj.get("summary")
    {
        merged.insert("summary".into(), summary.clone());
        changed = true;
    }

    // Preserve worktree
    if old_obj.contains_key("worktree") && !new_obj.contains_key("worktree") {
        merged.insert("worktree".into(), old_obj["worktree"].clone());
        changed = true;
    }

    // Preserve path/host
    for key in &["path", "host"] {
        if old_obj.get(*key).and_then(|v| v.as_str()).is_some()
            && new_obj.get(*key).and_then(|v| v.as_str()).is_none()
        {
            merged.insert((*key).to_string(), old_obj[*key].clone());
            changed = true;
        }
    }

    if changed {
        Some(Value::Object(merged))
    } else {
        None
    }
}
