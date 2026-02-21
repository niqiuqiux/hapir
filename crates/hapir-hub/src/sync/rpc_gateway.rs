use crate::ws::connection_manager::RpcCallError;
use anyhow::bail;
use hapir_shared::modes::{ModelMode, PermissionMode};
use hapir_shared::schemas::{AnswersFormat, AttachmentMetadata};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::oneshot;
use tokio::time::Instant;
use tracing::{debug, warn};

/// Trait for the RPC transport layer. The WebSocket server implements this.
pub trait RpcTransport: Send + Sync {
    /// Send an RPC request and get a response.
    fn rpc_call(
        &self,
        method: &str,
        params: Value,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<oneshot::Receiver<Result<Value, String>>, RpcCallError>>
                + Send
                + '_,
        >,
    >;

    /// Check whether a handler is registered for the given method.
    fn has_rpc_handler(&self, method: &str) -> Pin<Box<dyn Future<Output = bool> + Send + '_>>;

    /// Check whether any WebSocket connection exists for the given scope
    /// (session or machine room). Used to fast-fail RPC calls when no CLI
    /// is connected — polling is pointless if no connection can register a handler.
    fn has_scope_connection(&self, scope: &str) -> Pin<Box<dyn Future<Output = bool> + Send + '_>>;
}

// --- Response types ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcCommandResponse {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcReadFileResponse {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcUploadFileResponse {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcDeleteUploadResponse {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcDirectoryEntry {
    pub name: String,
    #[serde(rename = "type")]
    pub entry_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcListDirectoryResponse {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entries: Option<Vec<RpcDirectoryEntry>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpawnSessionResult {
    #[serde(rename = "type")]
    pub result_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// RPC gateway for calling methods on CLI/machine connections.
pub struct RpcGateway {
    transport: Arc<dyn RpcTransport>,
}

impl RpcGateway {
    pub fn new(transport: Arc<dyn RpcTransport>) -> Self {
        Self { transport }
    }

    async fn session_rpc(
        &self,
        session_id: &str,
        method: &str,
        params: Value,
    ) -> anyhow::Result<Value> {
        self.rpc_call(&format!("{session_id}:{method}"), params)
            .await
    }

    async fn machine_rpc(
        &self,
        machine_id: &str,
        method: &str,
        params: Value,
    ) -> anyhow::Result<Value> {
        self.rpc_call(&format!("{machine_id}:{method}"), params)
            .await
    }

    async fn rpc_call(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        debug!(method, "RPC call initiating");

        // When the handler is not yet registered, poll briefly before giving up.
        // This covers the race where the frontend queries a session right after
        // creation, before the CLI process has finished sending rpc-register.
        let rx = {
            const MAX_WAIT: Duration = Duration::from_secs(5);
            const POLL_INTERVAL: Duration = Duration::from_millis(100);

            let deadline = Instant::now() + MAX_WAIT;

            loop {
                match self.transport.rpc_call(method, params.clone()).await {
                    Ok(rx) => break rx,
                    Err(RpcCallError::SendFailed) => {
                        bail!("RPC send failed (connection lost): {method}");
                    }
                    Err(RpcCallError::NotRegistered) => {
                        if Instant::now() >= deadline {
                            warn!(method, "RPC handler not registered after waiting");
                            bail!("RPC handler not registered: {method}");
                        }
                        // Fast-fail: if no WebSocket connection exists for this
                        // scope (session/machine), no handler can ever register,
                        // so polling would just waste heap memory.
                        if let Some((scope, _)) = method.split_once(':') {
                            if !self.transport.has_scope_connection(scope).await {
                                debug!(
                                    method,
                                    scope, "no active connection for scope, skipping poll"
                                );
                                bail!("RPC handler not registered (no connection): {method}");
                            }
                        }
                        debug!(method, "RPC handler not yet registered, waiting…");
                        tokio::time::sleep(POLL_INTERVAL).await;
                    }
                }
            }
        };

        debug!(method, "RPC call dispatched, waiting for response");
        let result = tokio::time::timeout(Duration::from_secs(30), rx)
            .await
            .map_err(|_| anyhow::anyhow!("RPC call timed out: {method}"))?
            .map_err(|_| anyhow::anyhow!("RPC call cancelled"))?
            .map_err(|e| anyhow::anyhow!("RPC error: {e}"))?;

        Ok(result)
    }

    // --- Permission ---

    pub async fn approve_permission(
        &self,
        session_id: &str,
        request_id: &str,
        mode: Option<PermissionMode>,
        allow_tools: Option<Vec<String>>,
        decision: Option<&str>,
        answers: Option<AnswersFormat>,
    ) -> anyhow::Result<()> {
        self.session_rpc(
            session_id,
            "permission",
            serde_json::json!({
                "id": request_id,
                "approved": true,
                "mode": mode,
                "allowTools": allow_tools,
                "decision": decision,
                "answers": answers,
            }),
        )
        .await?;
        Ok(())
    }

    pub async fn deny_permission(
        &self,
        session_id: &str,
        request_id: &str,
        decision: Option<&str>,
    ) -> anyhow::Result<()> {
        self.session_rpc(
            session_id,
            "permission",
            serde_json::json!({
                "id": request_id,
                "approved": false,
                "decision": decision,
            }),
        )
        .await?;
        Ok(())
    }

    // --- Session control ---

    pub async fn abort_session(&self, session_id: &str) -> anyhow::Result<()> {
        self.session_rpc(
            session_id,
            "abort",
            serde_json::json!({
                "reason": "User aborted via Telegram Bot"
            }),
        )
        .await?;
        Ok(())
    }

    pub async fn switch_session(&self, session_id: &str, to: &str) -> anyhow::Result<()> {
        self.session_rpc(session_id, "switch", serde_json::json!({"to": to}))
            .await?;
        Ok(())
    }

    pub async fn request_session_config(
        &self,
        session_id: &str,
        permission_mode: Option<PermissionMode>,
        model_mode: Option<ModelMode>,
    ) -> anyhow::Result<Value> {
        self.session_rpc(
            session_id,
            "set-session-config",
            serde_json::json!({
                "permissionMode": permission_mode,
                "modelMode": model_mode,
            }),
        )
        .await
    }

    pub async fn send_user_message(
        &self,
        session_id: &str,
        message: &str,
        attachments: Option<&[AttachmentMetadata]>,
    ) -> anyhow::Result<Value> {
        self.session_rpc(
            session_id,
            "on-user-message",
            serde_json::json!({
                "message": message,
                "attachments": attachments,
            }),
        )
        .await
    }

    pub async fn kill_session(&self, session_id: &str) -> anyhow::Result<()> {
        self.session_rpc(session_id, "killSession", serde_json::json!({}))
            .await?;
        Ok(())
    }

    // --- Spawn ---

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
    ) -> Result<SpawnSessionResult, String> {
        let result = self
            .machine_rpc(
                machine_id,
                "spawn-happy-session",
                serde_json::json!({
                    "type": "spawn-in-directory",
                    "directory": directory,
                    "agent": agent,
                    "model": model,
                    "yolo": yolo,
                    "sessionType": session_type,
                    "worktreeName": worktree_name,
                    "resumeSessionId": resume_session_id,
                }),
            )
            .await;

        match result {
            Ok(val) => {
                if let Some(obj) = val.as_object() {
                    let t = obj.get("type").and_then(|v| v.as_str()).unwrap_or("error");
                    if t == "success"
                        && let Some(sid) = obj.get("sessionId").and_then(|v| v.as_str())
                    {
                        return Ok(SpawnSessionResult {
                            result_type: "success".into(),
                            session_id: Some(sid.to_string()),
                            message: None,
                        });
                    }
                    let msg = obj
                        .get("error")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Unexpected spawn result");
                    return Ok(SpawnSessionResult {
                        result_type: "error".into(),
                        session_id: None,
                        message: Some(msg.to_string()),
                    });
                }
                Ok(SpawnSessionResult {
                    result_type: "error".into(),
                    session_id: None,
                    message: Some("Unexpected spawn result".into()),
                })
            }
            Err(e) => {
                let msg = e.to_string();
                Ok(SpawnSessionResult {
                    result_type: "error".into(),
                    session_id: None,
                    message: Some(msg),
                })
            }
        }
    }

    // --- Git ---

    pub async fn get_git_status(
        &self,
        session_id: &str,
        cwd: Option<&str>,
    ) -> anyhow::Result<RpcCommandResponse> {
        let val = self
            .session_rpc(session_id, "git-status", serde_json::json!({"cwd": cwd}))
            .await?;
        Ok(serde_json::from_value(val)?)
    }

    pub async fn get_git_diff_numstat(
        &self,
        session_id: &str,
        cwd: Option<&str>,
        staged: Option<bool>,
    ) -> anyhow::Result<RpcCommandResponse> {
        let val = self
            .session_rpc(
                session_id,
                "git-diff-numstat",
                serde_json::json!({"cwd": cwd, "staged": staged}),
            )
            .await?;
        Ok(serde_json::from_value(val)?)
    }

    pub async fn get_git_diff_file(
        &self,
        session_id: &str,
        cwd: Option<&str>,
        file_path: &str,
        staged: Option<bool>,
    ) -> anyhow::Result<RpcCommandResponse> {
        let val = self
            .session_rpc(
                session_id,
                "git-diff-file",
                serde_json::json!({"cwd": cwd, "filePath": file_path, "staged": staged}),
            )
            .await?;
        Ok(serde_json::from_value(val)?)
    }

    // --- File operations ---

    pub async fn read_session_file(
        &self,
        session_id: &str,
        path: &str,
    ) -> anyhow::Result<RpcReadFileResponse> {
        let val = self
            .session_rpc(session_id, "readFile", serde_json::json!({"path": path}))
            .await?;
        Ok(serde_json::from_value(val)?)
    }

    pub async fn list_directory(
        &self,
        session_id: &str,
        path: &str,
    ) -> anyhow::Result<RpcListDirectoryResponse> {
        let val = self
            .session_rpc(
                session_id,
                "listDirectory",
                serde_json::json!({"path": path}),
            )
            .await?;
        Ok(serde_json::from_value(val)?)
    }

    pub async fn upload_file(
        &self,
        session_id: &str,
        filename: &str,
        content: &str,
        mime_type: &str,
    ) -> anyhow::Result<RpcUploadFileResponse> {
        let val = self.session_rpc(session_id, "uploadFile", serde_json::json!({
            "sessionId": session_id, "filename": filename, "content": content, "mimeType": mime_type
        })).await?;
        Ok(serde_json::from_value(val)?)
    }

    pub async fn delete_upload_file(
        &self,
        session_id: &str,
        path: &str,
    ) -> anyhow::Result<RpcDeleteUploadResponse> {
        let val = self
            .session_rpc(
                session_id,
                "deleteUpload",
                serde_json::json!({"sessionId": session_id, "path": path}),
            )
            .await?;
        Ok(serde_json::from_value(val)?)
    }

    pub async fn run_ripgrep(
        &self,
        session_id: &str,
        args: &[String],
        cwd: Option<&str>,
    ) -> anyhow::Result<RpcCommandResponse> {
        let val = self
            .session_rpc(
                session_id,
                "ripgrep",
                serde_json::json!({"args": args, "cwd": cwd}),
            )
            .await?;
        Ok(serde_json::from_value(val)?)
    }

    pub async fn check_paths_exist(
        &self,
        machine_id: &str,
        paths: &[String],
    ) -> anyhow::Result<HashMap<String, bool>> {
        let val = self
            .machine_rpc(
                machine_id,
                "path-exists",
                serde_json::json!({"paths": paths}),
            )
            .await?;
        let exists = val
            .get("exists")
            .ok_or_else(|| anyhow::anyhow!("unexpected path-exists result"))?;
        let map: HashMap<String, bool> = serde_json::from_value(exists.clone())?;
        Ok(map)
    }

    pub async fn list_slash_commands(
        &self,
        session_id: &str,
        agent: &str,
    ) -> anyhow::Result<Value> {
        self.session_rpc(
            session_id,
            "listSlashCommands",
            serde_json::json!({"agent": agent}),
        )
        .await
    }

    pub async fn list_skills(&self, session_id: &str) -> anyhow::Result<Value> {
        self.session_rpc(session_id, "listSkills", serde_json::json!({}))
            .await
    }
}
