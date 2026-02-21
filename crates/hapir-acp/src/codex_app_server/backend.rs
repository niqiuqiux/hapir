use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde_json::Value;
use tokio::sync::{Mutex, oneshot};
use tracing::debug;

use crate::transport::AcpStdioTransport;
use crate::types::{
    AgentBackend, AgentSessionConfig, OnPermissionRequestFn, OnUpdateFn, PermissionRequest,
    PermissionResponse, PromptContent,
};

use super::message_handler::CodexMessageHandler;

struct PendingPermission {
    tx: oneshot::Sender<Value>,
}

/// Codex App Server backend using `codex app-server` over JSON-RPC 2.0 stdio.
///
/// Method mapping vs ACP SDK:
/// - `initialize` + `initialized` notification (two-step handshake)
/// - `thread/start` instead of `session/new`
/// - `turn/start` instead of `session/prompt`
/// - `turn/interrupt` instead of `session/cancel`
/// - Permission requests via `item/commandExecution/requestApproval`
///   and `item/fileChange/requestApproval`
pub struct CodexAppServerBackend {
    command: String,
    args: Vec<String>,
    env: Option<HashMap<String, String>>,
    transport: Mutex<Option<Arc<AcpStdioTransport>>>,
    permission_handler: Mutex<Option<OnPermissionRequestFn>>,
    pending_permissions: Arc<Mutex<HashMap<String, PendingPermission>>>,
    active_thread_id: Mutex<Option<String>>,
    message_handler: Arc<Mutex<Option<CodexMessageHandler>>>,
}

impl CodexAppServerBackend {
    pub fn new(command: String, args: Vec<String>, env: Option<HashMap<String, String>>) -> Self {
        Self {
            command,
            args,
            env,
            transport: Mutex::new(None),
            permission_handler: Mutex::new(None),
            pending_permissions: Arc::new(Mutex::new(HashMap::new())),
            active_thread_id: Mutex::new(None),
            message_handler: Arc::new(Mutex::new(None)),
        }
    }
}

impl AgentBackend for CodexAppServerBackend {
    fn initialize(&self) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + '_>> {
        Box::pin(async move {
            if self.transport.lock().await.is_some() {
                return Ok(());
            }

            let transport = AcpStdioTransport::new(&self.command, &self.args, self.env.clone());

            let msg_handler = self.message_handler.clone();
            transport
                .on_notification(move |method, params| {
                    if let Ok(mut handler) = msg_handler.try_lock()
                        && let Some(h) = handler.as_mut()
                    {
                        h.handle_notification(&method, &params);
                    }
                })
                .await;

            *self.transport.lock().await = Some(transport.clone());

            let pending_cmd = self.pending_permissions.clone();
            transport
                .register_request_handler(
                    "item/commandExecution/requestApproval",
                    Arc::new(move |params: Value, _id: Value| {
                        let pending = pending_cmd.clone();
                        Box::pin(async move {
                            let id = params
                                .as_object()
                                .and_then(|o| o.get("id"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("tool-unknown")
                                .to_string();

                            let (tx, rx) = oneshot::channel();
                            pending.lock().await.insert(id, PendingPermission { tx });
                            rx.await
                                .unwrap_or_else(|_| serde_json::json!({"approved": false}))
                        })
                    }),
                )
                .await;

            let pending_file = self.pending_permissions.clone();
            transport
                .register_request_handler(
                    "item/fileChange/requestApproval",
                    Arc::new(move |params: Value, _id: Value| {
                        let pending = pending_file.clone();
                        Box::pin(async move {
                            let id = params
                                .as_object()
                                .and_then(|o| o.get("id"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("tool-unknown")
                                .to_string();

                            let (tx, rx) = oneshot::channel();
                            pending.lock().await.insert(id, PendingPermission { tx });
                            rx.await
                                .unwrap_or_else(|_| serde_json::json!({"approved": false}))
                        })
                    }),
                )
                .await;

            // Codex two-step handshake: send `initialize` request, then `initialized` notification
            let response = transport
                .send_request_default(
                    "initialize",
                    serde_json::json!({
                        "protocolVersion": "2025-03-26",
                        "clientInfo": {
                            "name": "hapir",
                            "version": env!("CARGO_PKG_VERSION")
                        },
                        "capabilities": {}
                    }),
                )
                .await
                .map_err(|e| anyhow::anyhow!(e))?;

            debug!("[CodexAppServer] Initialize response: {:?}", response);

            transport
                .send_notification("initialized", serde_json::json!({}))
                .await;

            debug!("[CodexAppServer] Initialized");
            Ok(())
        })
    }

    fn new_session(
        &self,
        config: AgentSessionConfig,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<String>> + Send + '_>> {
        Box::pin(async move {
            let transport = self
                .transport
                .lock()
                .await
                .clone()
                .ok_or_else(|| anyhow::anyhow!("Codex transport not initialized"))?;

            let response = transport
                .send_request_default(
                    "thread/start",
                    serde_json::json!({
                        "cwd": config.cwd,
                    }),
                )
                .await
                .map_err(|e| anyhow::anyhow!(e))?;

            let thread_id = response
                .as_object()
                .and_then(|o| o.get("threadId").or_else(|| o.get("id")))
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("Invalid thread/start response"))?
                .to_string();

            *self.active_thread_id.lock().await = Some(thread_id.clone());
            debug!("[CodexAppServer] Thread started: {}", thread_id);
            Ok(thread_id)
        })
    }

    fn prompt(
        &self,
        session_id: &str,
        content: Vec<PromptContent>,
        on_update: OnUpdateFn,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + '_>> {
        let session_id = session_id.to_string();
        let content = content.clone();
        Box::pin(async move {
            let transport = self
                .transport
                .lock()
                .await
                .clone()
                .ok_or_else(|| anyhow::anyhow!("Codex transport not initialized"))?;

            *self.message_handler.lock().await = Some(CodexMessageHandler::new(move |msg| {
                on_update(msg);
            }));

            let prompt_text = content
                .iter()
                .map(|c| match c {
                    PromptContent::Text { text } => text.as_str(),
                })
                .collect::<Vec<_>>()
                .join("\n");

            let result = transport
                .send_request(
                    "turn/start",
                    serde_json::json!({
                        "threadId": session_id,
                        "content": prompt_text,
                    }),
                    u64::MAX,
                )
                .await;

            *self.message_handler.lock().await = None;

            result.map(|_| ()).map_err(|e| anyhow::anyhow!(e))
        })
    }

    fn cancel_prompt(
        &self,
        session_id: &str,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + '_>> {
        let session_id = session_id.to_string();
        Box::pin(async move {
            if let Some(transport) = self.transport.lock().await.as_ref() {
                transport
                    .send_notification(
                        "turn/interrupt",
                        serde_json::json!({"threadId": session_id}),
                    )
                    .await;
            }
            Ok(())
        })
    }

    fn respond_to_permission(
        &self,
        _session_id: &str,
        request: &PermissionRequest,
        response: PermissionResponse,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + '_>> {
        let request_id = request.id.clone();
        Box::pin(async move {
            let pending = self.pending_permissions.lock().await.remove(&request_id);
            if let Some(p) = pending {
                let approved = matches!(response, PermissionResponse::Selected { ref option_id } if option_id != "deny");
                let _ = p.tx.send(serde_json::json!({"approved": approved}));
            } else {
                debug!(
                    "[CodexAppServer] No pending permission for id {}",
                    request_id
                );
            }
            Ok(())
        })
    }

    fn on_permission_request(&self, handler: OnPermissionRequestFn) {
        let mut h = self.permission_handler.blocking_lock();
        *h = Some(handler);
    }

    fn disconnect(&self) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + '_>> {
        Box::pin(async move {
            if let Some(transport) = self.transport.lock().await.take() {
                transport.close().await;
            }
            Ok(())
        })
    }
}
