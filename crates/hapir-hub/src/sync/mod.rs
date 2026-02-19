pub mod alive_time;
pub mod event_publisher;
pub mod machine_cache;
pub mod message_service;
pub mod rpc_gateway;
pub mod session_cache;
pub mod sse_manager;
pub mod todos;
pub mod visibility_tracker;

use std::sync::Arc;

use hapir_shared::modes::{ModelMode, PermissionMode};
use hapir_shared::schemas::{AttachmentMetadata, DecryptedMessage, Session, SyncEvent};
use serde_json::Value;
use tokio::sync::{broadcast, mpsc, RwLock};

use crate::store::Store;
use event_publisher::EventPublisher;
use machine_cache::{Machine, MachineCache};
use message_service::{MessageService, MessagesPageResult};
use rpc_gateway::{RpcGateway, RpcTransport};
use session_cache::SessionCache;
use sse_manager::{SseManager, SseMessage, SseSubscription};
use visibility_tracker::VisibilityState;

/// The central sync engine that coordinates sessions, machines, messages, and RPC.
///
/// All fields use internal locking so the engine can be shared as `Arc<SyncEngine>`.
/// Lock ordering: session_cache → machine_cache. Never hold a cache lock across RPC calls.
pub struct SyncEngine {
    store: Arc<Store>,
    publisher: EventPublisher,
    session_cache: RwLock<SessionCache>,
    machine_cache: RwLock<MachineCache>,
    rpc_gateway: Arc<RpcGateway>,
}

impl SyncEngine {
    pub fn new(store: Arc<Store>, rpc_transport: Arc<dyn RpcTransport>) -> Self {
        let sse_manager = SseManager::new(30_000);
        let publisher = EventPublisher::new(sse_manager);
        let mut session_cache = SessionCache::new();
        let mut machine_cache = MachineCache::new();

        // Load existing data from store
        session_cache.reload_all(&store, &publisher);
        machine_cache.reload_all(&store, &publisher);

        let rpc_gateway = Arc::new(RpcGateway::new(rpc_transport));

        Self {
            store,
            publisher,
            session_cache: RwLock::new(session_cache),
            machine_cache: RwLock::new(machine_cache),
            rpc_gateway,
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<SyncEvent> {
        self.publisher.subscribe()
    }

    // --- SSE delegate methods ---

    pub fn subscribe_sse(
        &self,
        id: String,
        namespace: String,
        all: bool,
        session_id: Option<String>,
        machine_id: Option<String>,
        visibility: VisibilityState,
    ) -> (mpsc::UnboundedReceiver<SseMessage>, SseSubscription) {
        self.publisher.subscribe_sse(id, namespace, all, session_id, machine_id, visibility)
    }

    pub fn unsubscribe_sse(&self, id: &str) {
        self.publisher.unsubscribe_sse(id);
    }

    pub fn sse_connection_count(&self) -> usize {
        self.publisher.sse_connection_count()
    }

    pub fn has_visible_sse_connection(&self, namespace: &str) -> bool {
        self.publisher.has_visible_sse_connection(namespace)
    }

    pub fn set_sse_visibility(
        &self,
        subscription_id: &str,
        namespace: &str,
        state: VisibilityState,
    ) -> bool {
        self.publisher.set_sse_visibility(subscription_id, namespace, state)
    }

    pub fn send_heartbeats(&self) {
        self.publisher.send_heartbeats();
    }

    pub fn send_sse_toast(&self, namespace: &str, event: &SyncEvent) -> usize {
        self.publisher.send_toast(namespace, event)
    }

    pub fn heartbeat_ms(&self) -> u64 {
        self.publisher.heartbeat_ms()
    }

    // --- Session accessors ---

    pub async fn get_sessions(&self) -> Vec<Session> {
        self.session_cache.read().await.get_sessions()
    }

    pub async fn get_sessions_by_namespace(&self, namespace: &str) -> Vec<Session> {
        self.session_cache.read().await.get_sessions_by_namespace(namespace)
    }

    pub async fn get_session(&self, session_id: &str) -> Option<Session> {
        if let Some(s) = self.session_cache.read().await.get_session(session_id).cloned() {
            return Some(s);
        }
        self.session_cache.write().await.refresh_session(session_id, &self.store, &self.publisher)
    }

    pub async fn get_session_by_namespace(&self, session_id: &str, namespace: &str) -> Option<Session> {
        {
            let cache = self.session_cache.read().await;
            if let Some(s) = cache.get_session_by_namespace(session_id, namespace).cloned() {
                return Some(s);
            }
        }
        let session = self.session_cache.write().await.refresh_session(session_id, &self.store, &self.publisher);
        session.filter(|s| s.namespace == namespace)
    }

    pub async fn resolve_session_access(
        &self,
        session_id: &str,
        namespace: &str,
    ) -> Result<(String, Session), &'static str> {
        self.session_cache.write().await.resolve_session_access(session_id, namespace, &self.store, &self.publisher)
    }

    pub async fn get_active_sessions(&self) -> Vec<Session> {
        self.session_cache.read().await.get_active_sessions()
    }

    pub async fn get_or_create_session(
        &self,
        tag: &str,
        metadata: &Value,
        agent_state: Option<&Value>,
        namespace: &str,
    ) -> anyhow::Result<Session> {
        self.session_cache.write().await.get_or_create_session(tag, metadata, agent_state, namespace, &self.store, &self.publisher)
    }

    // --- Machine accessors ---

    pub async fn get_machines(&self) -> Vec<Machine> {
        self.machine_cache.read().await.get_machines()
    }

    pub async fn get_machines_by_namespace(&self, namespace: &str) -> Vec<Machine> {
        self.machine_cache.read().await.get_machines_by_namespace(namespace)
    }

    pub async fn get_machine(&self, machine_id: &str) -> Option<Machine> {
        self.machine_cache.read().await.get_machine(machine_id).cloned()
    }

    pub async fn get_machine_by_namespace(&self, machine_id: &str, namespace: &str) -> Option<Machine> {
        self.machine_cache.read().await.get_machine_by_namespace(machine_id, namespace).cloned()
    }

    pub async fn get_online_machines(&self) -> Vec<Machine> {
        self.machine_cache.read().await.get_online_machines()
    }

    pub async fn get_online_machines_by_namespace(&self, namespace: &str) -> Vec<Machine> {
        self.machine_cache.read().await.get_online_machines_by_namespace(namespace)
    }

    pub async fn get_or_create_machine(
        &self,
        id: &str,
        metadata: &Value,
        runner_state: Option<&Value>,
        namespace: &str,
    ) -> anyhow::Result<Machine> {
        self.machine_cache.write().await.get_or_create_machine(id, metadata, runner_state, namespace, &self.store, &self.publisher)
    }

    // --- Messages ---

    pub fn get_messages_page(&self, session_id: &str, limit: i64, before_seq: Option<i64>) -> MessagesPageResult {
        MessageService::get_messages_page(&self.store, session_id, limit, before_seq)
    }

    pub fn get_messages_after(&self, session_id: &str, after_seq: i64, limit: i64) -> Vec<DecryptedMessage> {
        MessageService::get_messages_after(&self.store, session_id, after_seq, limit)
    }

    pub async fn send_message(
        &self,
        session_id: &str,
        namespace: &str,
        text: &str,
        local_id: Option<&str>,
        attachments: Option<&[AttachmentMetadata]>,
        sent_from: Option<&str>,
    ) -> anyhow::Result<()> {
        tracing::debug!(session_id, text_len = text.len(), "send_message: storing and dispatching");
        MessageService::send_message(&self.store, &self.publisher, session_id, namespace, text, local_id, attachments, sent_from)?;

        // Notify the session process via RPC so it can pick up the message
        match self.rpc_gateway.send_user_message(session_id, text, attachments).await {
            Ok(resp) => tracing::debug!(session_id, ?resp, "send_message: RPC delivered"),
            Err(e) => tracing::warn!(session_id, error = %e, "send_message: failed to deliver via RPC"),
        }

        Ok(())
    }

    // --- Realtime event handling ---

    pub async fn handle_realtime_event(&self, event: SyncEvent) {
        match &event {
            SyncEvent::SessionUpdated { session_id, .. } => {
                self.session_cache.write().await.refresh_session(session_id, &self.store, &self.publisher);
                return;
            }
            SyncEvent::MachineUpdated { machine_id, .. } => {
                self.machine_cache.write().await.refresh_machine(machine_id, &self.store, &self.publisher);
                return;
            }
            SyncEvent::MessageReceived { session_id, .. } => {
                let needs_refresh = self.session_cache.read().await.get_session(session_id).is_none();
                if needs_refresh {
                    self.session_cache.write().await.refresh_session(session_id, &self.store, &self.publisher);
                }
            }
            _ => {}
        }
        self.publisher.emit(event);
    }

    pub async fn handle_session_alive(
        &self,
        sid: &str,
        time: i64,
        thinking: Option<bool>,
        permission_mode: Option<PermissionMode>,
        model_mode: Option<ModelMode>,
    ) {
        self.session_cache.write().await.handle_session_alive(
            sid, time, thinking, permission_mode, model_mode, &self.store, &self.publisher,
        );
    }

    pub async fn handle_session_end(&self, sid: &str, time: i64) {
        self.session_cache.write().await.handle_session_end(sid, time, &self.store, &self.publisher);
    }

    pub async fn handle_machine_alive(&self, machine_id: &str, time: i64) {
        self.machine_cache.write().await.handle_machine_alive(machine_id, time, &self.store, &self.publisher);
    }

    /// Mark a machine as offline immediately (e.g. on WebSocket disconnect).
    pub async fn mark_machine_offline(&self, machine_id: &str) {
        self.machine_cache.write().await.mark_machine_offline(machine_id, &self.publisher);
    }

    /// Called periodically to expire inactive sessions and machines.
    pub async fn expire_inactive(&self) {
        self.session_cache.write().await.expire_inactive(&self.publisher);
        self.machine_cache.write().await.expire_inactive(&self.publisher);
    }

    // --- RPC delegations ---

    pub async fn approve_permission(
        &self,
        session_id: &str,
        request_id: &str,
        mode: Option<PermissionMode>,
        allow_tools: Option<Vec<String>>,
        decision: Option<&str>,
        answers: Option<hapir_shared::schemas::AnswersFormat>,
    ) -> anyhow::Result<()> {
        self.rpc_gateway.approve_permission(session_id, request_id, mode, allow_tools, decision, answers).await
    }

    pub async fn deny_permission(
        &self,
        session_id: &str,
        request_id: &str,
        decision: Option<&str>,
    ) -> anyhow::Result<()> {
        self.rpc_gateway.deny_permission(session_id, request_id, decision).await
    }

    pub async fn abort_session(&self, session_id: &str) -> anyhow::Result<()> {
        self.rpc_gateway.abort_session(session_id).await
    }

    /// Archive: best-effort RPC kill → then update cache.
    /// The kill RPC is fire-and-forget: the session is archived regardless of
    /// whether the CLI is still connected or has a handler registered.
    pub async fn archive_session(&self, session_id: &str) -> anyhow::Result<()> {
        if let Err(e) = self.rpc_gateway.kill_session(session_id).await {
            tracing::debug!(session_id, error = %e, "killSession RPC failed (proceeding with archive)");
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        self.handle_session_end(session_id, now).await;
        Ok(())
    }

    pub async fn switch_session(&self, session_id: &str, to: &str) -> anyhow::Result<()> {
        self.rpc_gateway.switch_session(session_id, to).await
    }

    pub async fn rename_session(&self, session_id: &str, name: &str) -> anyhow::Result<()> {
        self.session_cache.write().await.rename_session(session_id, name, &self.store, &self.publisher)
    }

    pub async fn delete_session(&self, session_id: &str) -> anyhow::Result<()> {
        self.session_cache.write().await.delete_session(session_id, &self.store, &self.publisher)
    }

    /// Apply session config: RPC (no lock) → then update cache
    pub async fn apply_session_config(
        &self,
        session_id: &str,
        permission_mode: Option<PermissionMode>,
        model_mode: Option<ModelMode>,
    ) -> anyhow::Result<()> {
        let result = self.rpc_gateway.request_session_config(session_id, permission_mode, model_mode).await?;
        let applied = result.get("applied")
            .ok_or_else(|| anyhow::anyhow!("missing applied session config"))?;
        let pm: Option<PermissionMode> = applied.get("permissionMode")
            .and_then(|v| serde_json::from_value(v.clone()).ok());
        let mm: Option<ModelMode> = applied.get("modelMode")
            .and_then(|v| serde_json::from_value(v.clone()).ok());
        self.session_cache.write().await.apply_session_config(session_id, pm, mm, &self.store, &self.publisher);
        Ok(())
    }

    pub async fn spawn_session(
        &self,
        machine_id: &str,
        directory: &str,
        agent: &str,
        model: Option<&str>,
        yolo: Option<bool>,
        session_type: Option<&str>,
        worktree_name: Option<&str>,
        resume_session_id: Option<&str>,
    ) -> Result<rpc_gateway::SpawnSessionResult, String> {
        self.rpc_gateway.spawn_session(machine_id, directory, agent, model, yolo, session_type, worktree_name, resume_session_id).await
    }

    pub async fn check_paths_exist(&self, machine_id: &str, paths: &[String]) -> anyhow::Result<std::collections::HashMap<String, bool>> {
        self.rpc_gateway.check_paths_exist(machine_id, paths).await
    }

    pub async fn get_git_status(&self, session_id: &str, cwd: Option<&str>) -> anyhow::Result<rpc_gateway::RpcCommandResponse> {
        self.rpc_gateway.get_git_status(session_id, cwd).await
    }

    pub async fn get_git_diff_numstat(&self, session_id: &str, cwd: Option<&str>, staged: Option<bool>) -> anyhow::Result<rpc_gateway::RpcCommandResponse> {
        self.rpc_gateway.get_git_diff_numstat(session_id, cwd, staged).await
    }

    pub async fn get_git_diff_file(&self, session_id: &str, cwd: Option<&str>, file_path: &str, staged: Option<bool>) -> anyhow::Result<rpc_gateway::RpcCommandResponse> {
        self.rpc_gateway.get_git_diff_file(session_id, cwd, file_path, staged).await
    }

    pub async fn read_session_file(&self, session_id: &str, path: &str) -> anyhow::Result<rpc_gateway::RpcReadFileResponse> {
        self.rpc_gateway.read_session_file(session_id, path).await
    }

    pub async fn list_directory(&self, session_id: &str, path: &str) -> anyhow::Result<rpc_gateway::RpcListDirectoryResponse> {
        self.rpc_gateway.list_directory(session_id, path).await
    }

    pub async fn upload_file(&self, session_id: &str, filename: &str, content: &str, mime_type: &str) -> anyhow::Result<rpc_gateway::RpcUploadFileResponse> {
        self.rpc_gateway.upload_file(session_id, filename, content, mime_type).await
    }

    pub async fn delete_upload_file(&self, session_id: &str, path: &str) -> anyhow::Result<rpc_gateway::RpcDeleteUploadResponse> {
        self.rpc_gateway.delete_upload_file(session_id, path).await
    }

    pub async fn run_ripgrep(&self, session_id: &str, args: &[String], cwd: Option<&str>) -> anyhow::Result<rpc_gateway::RpcCommandResponse> {
        self.rpc_gateway.run_ripgrep(session_id, args, cwd).await
    }

    pub async fn list_slash_commands(&self, session_id: &str, agent: &str) -> anyhow::Result<Value> {
        self.rpc_gateway.list_slash_commands(session_id, agent).await
    }

    pub async fn list_skills(&self, session_id: &str) -> anyhow::Result<Value> {
        self.rpc_gateway.list_skills(session_id).await
    }

    pub async fn merge_sessions(
        &self,
        old_session_id: &str,
        new_session_id: &str,
        namespace: &str,
    ) -> anyhow::Result<()> {
        self.session_cache.write().await.merge_sessions(old_session_id, new_session_id, namespace, &self.store, &self.publisher)
    }

    /// Resume an inactive session by finding a suitable machine and spawning.
    /// Follows lock discipline: read/write lock → release → RPC (no lock) → write lock
    pub async fn resume_session(
        &self,
        session_id: &str,
        namespace: &str,
    ) -> ResumeSessionResult {
        // Phase 1: validate session access (write lock for potential refresh)
        let (original_id, session) = {
            let access = self.session_cache.write().await.resolve_session_access(
                session_id, namespace, &self.store, &self.publisher,
            );
            match access {
                Ok(pair) => pair,
                Err("access-denied") => {
                    return ResumeSessionResult::Error {
                        message: "Session access denied".into(),
                        code: ResumeSessionErrorCode::AccessDenied,
                    }
                }
                Err(_) => {
                    return ResumeSessionResult::Error {
                        message: "Session not found".into(),
                        code: ResumeSessionErrorCode::SessionNotFound,
                    }
                }
            }
        };

        if session.active {
            return ResumeSessionResult::Success { session_id: original_id };
        }

        let metadata = match &session.metadata {
            Some(m) if !m.path.is_empty() => m.clone(),
            _ => {
                return ResumeSessionResult::Error {
                    message: "Session metadata missing path".into(),
                    code: ResumeSessionErrorCode::ResumeUnavailable,
                }
            }
        };

        let flavor = match metadata.flavor.as_deref() {
            Some("codex") => "codex",
            Some("gemini") => "gemini",
            Some("opencode") => "opencode",
            _ => "claude",
        };

        let resume_token = match flavor {
            "codex" => metadata.codex_session_id.as_deref(),
            "gemini" => metadata.gemini_session_id.as_deref(),
            "opencode" => metadata.opencode_session_id.as_deref(),
            _ => metadata.claude_session_id.as_deref(),
        };

        let resume_token = resume_token.map(|t| t.to_string());

        // Phase 2: find target machine (read lock, then release)
        let target = {
            let mc = self.machine_cache.read().await;
            let online_machines = mc.get_online_machines_by_namespace(namespace);
            if online_machines.is_empty() {
                return ResumeSessionResult::Error {
                    message: "No machine online".into(),
                    code: ResumeSessionErrorCode::NoMachineOnline,
                };
            }

            let found = online_machines.iter().find(|m| {
                metadata.machine_id.as_deref() == Some(&m.id)
            }).or_else(|| {
                let host = metadata.host.as_str();
                online_machines.iter().find(|m| {
                    m.metadata.as_ref().is_some_and(|meta| meta.host == host)
                })
            });

            match found {
                Some(m) => m.clone(),
                None => {
                    return ResumeSessionResult::Error {
                        message: "No machine online".into(),
                        code: ResumeSessionErrorCode::NoMachineOnline,
                    }
                }
            }
        };

        // Phase 3: RPC spawn (no lock held)
        // If no resume token (session created but agent never started), spawn fresh.
        let spawn_result = self.rpc_gateway.spawn_session(
            &target.id,
            &metadata.path,
            flavor,
            None,
            None,
            None,
            None,
            resume_token.as_deref(),
        ).await;

        let spawn_session_id = match spawn_result {
            Ok(r) if r.result_type == "success" => {
                match r.session_id {
                    Some(id) => id,
                    None => {
                        return ResumeSessionResult::Error {
                            message: "Spawn returned success but no session ID".into(),
                            code: ResumeSessionErrorCode::ResumeFailed,
                        }
                    }
                }
            }
            Ok(r) => {
                return ResumeSessionResult::Error {
                    message: r.message.unwrap_or_else(|| "Spawn failed".into()),
                    code: ResumeSessionErrorCode::ResumeFailed,
                }
            }
            Err(e) => {
                return ResumeSessionResult::Error {
                    message: e,
                    code: ResumeSessionErrorCode::ResumeFailed,
                }
            }
        };

        // Phase 4: poll for active (short read locks each iteration)
        if !self.wait_for_session_active(&spawn_session_id, 15_000).await {
            return ResumeSessionResult::Error {
                message: "Session failed to become active".into(),
                code: ResumeSessionErrorCode::ResumeFailed,
            };
        }

        // Phase 5: merge sessions (write lock)
        if spawn_session_id != original_id
            && let Err(e) = self.session_cache.write().await.merge_sessions(
                &original_id, &spawn_session_id, namespace, &self.store, &self.publisher,
            )
        {
            return ResumeSessionResult::Error {
                message: e.to_string(),
                code: ResumeSessionErrorCode::ResumeFailed,
            };
        }

        ResumeSessionResult::Success { session_id: spawn_session_id }
    }

    /// Poll until a session becomes active, up to timeout_ms.
    /// Each poll iteration takes a short read lock then releases it.
    pub async fn wait_for_session_active(&self, session_id: &str, timeout_ms: u64) -> bool {
        let start = std::time::Instant::now();
        let timeout = std::time::Duration::from_millis(timeout_ms);

        loop {
            if let Some(session) = self.get_session(session_id).await
                && session.active
            {
                return true;
            }
            if start.elapsed() >= timeout {
                return false;
            }
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
    }
}

#[derive(Debug, Clone)]
pub enum ResumeSessionResult {
    Success { session_id: String },
    Error { message: String, code: ResumeSessionErrorCode },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResumeSessionErrorCode {
    SessionNotFound,
    AccessDenied,
    NoMachineOnline,
    ResumeUnavailable,
    ResumeFailed,
}
