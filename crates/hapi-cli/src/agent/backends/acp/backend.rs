use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde_json::Value;
use tokio::sync::{oneshot, Mutex};
use tracing::debug;

use crate::agent::types::{
    AgentBackend, AgentSessionConfig, OnPermissionRequestFn, OnUpdateFn,
    PermissionOption, PermissionRequest, PermissionResponse, PromptContent,
};

use super::message_handler::AcpMessageHandler;
use super::transport::{AcpStderrError, AcpStdioTransport};

// ---------------------------------------------------------------------------
// Retry helper
// ---------------------------------------------------------------------------

async fn with_retry<F, Fut>(max_attempts: u32, f: F) -> Result<Value, String>
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<Value, String>>,
{
    let mut last_err = String::new();
    for attempt in 0..max_attempts {
        match f().await {
            Ok(v) => return Ok(v),
            Err(e) => {
                last_err = e;
                if attempt + 1 < max_attempts {
                    let delay = std::cmp::min(1000 * (1 << attempt), 5000);
                    debug!(
                        "[ACP] Attempt {} failed, retrying in {}ms: {}",
                        attempt + 1,
                        delay,
                        last_err
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                }
            }
        }
    }
    Err(last_err)
}

// ---------------------------------------------------------------------------
// Pending permission
// ---------------------------------------------------------------------------

struct PendingPermission {
    tx: oneshot::Sender<Value>,
}

// ---------------------------------------------------------------------------
// AcpSdkBackend
// ---------------------------------------------------------------------------

/// ACP backend that communicates with an agent process via JSON-RPC over stdio.
pub struct AcpSdkBackend {
    command: String,
    args: Vec<String>,
    env: Option<HashMap<String, String>>,
    transport: Mutex<Option<Arc<AcpStdioTransport>>>,
    permission_handler: Mutex<Option<OnPermissionRequestFn>>,
    #[allow(dead_code)]
    stderr_error_handler: Mutex<Option<Box<dyn Fn(AcpStderrError) + Send + Sync>>>,
    pending_permissions: Arc<Mutex<HashMap<String, PendingPermission>>>,
    active_session_id: Mutex<Option<String>>,
    message_handler: Arc<Mutex<Option<AcpMessageHandler>>>,
    is_processing: Mutex<bool>,
    response_complete_txs: Mutex<Vec<oneshot::Sender<()>>>,
}

impl AcpSdkBackend {
    pub fn new(
        command: String,
        args: Vec<String>,
        env: Option<HashMap<String, String>>,
    ) -> Self {
        Self {
            command,
            args,
            env,
            transport: Mutex::new(None),
            permission_handler: Mutex::new(None),
            stderr_error_handler: Mutex::new(None),
            pending_permissions: Arc::new(Mutex::new(HashMap::new())),
            active_session_id: Mutex::new(None),
            message_handler: Arc::new(Mutex::new(None)),
            is_processing: Mutex::new(false),
            response_complete_txs: Mutex::new(Vec::new()),
        }
    }

    /// Whether a prompt is currently being processed.
    pub async fn processing_message(&self) -> bool {
        *self.is_processing.lock().await
    }

    /// Wait for any in-progress response to complete.
    pub async fn wait_for_response_complete(&self) {
        if !*self.is_processing.lock().await {
            return;
        }
        let (tx, rx) = oneshot::channel();
        self.response_complete_txs.lock().await.push(tx);
        let _ = rx.await;
    }

    fn notify_response_complete(&self) {
        let mut txs = self.response_complete_txs.blocking_lock();
        for tx in txs.drain(..) {
            let _ = tx.send(());
        }
    }

    #[allow(dead_code)]
    async fn handle_session_update(&self, params: &Value) {
        let obj = match params.as_object() {
            Some(o) => o,
            None => return,
        };

        let session_id = obj.get("sessionId").and_then(|v| v.as_str());
        let active = self.active_session_id.lock().await;
        if let (Some(active_id), Some(sid)) = (active.as_deref(), session_id) {
            if sid != active_id {
                return;
            }
        }
        drop(active);

        let update = match obj.get("update") {
            Some(u) => u.clone(),
            None => return,
        };

        let mut handler = self.message_handler.lock().await;
        if let Some(h) = handler.as_mut() {
            h.handle_update(&update);
        }
    }

    #[allow(dead_code)]
    async fn handle_permission_request(&self, params: Value) -> Value {
        let obj = match params.as_object() {
            Some(o) => o,
            None => return serde_json::json!({"outcome": {"outcome": "cancelled"}}),
        };

        let active_id = self.active_session_id.lock().await.clone();
        let session_id = obj
            .get("sessionId")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or(active_id)
            .unwrap_or_else(|| "unknown".to_string());

        let tool_call = obj
            .get("toolCall")
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();

        let tool_call_id = tool_call
            .get("toolCallId")
            .and_then(|v| v.as_str())
            .unwrap_or("tool-unknown")
            .to_string();

        let title = tool_call
            .get("title")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let kind = tool_call
            .get("kind")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let raw_input = tool_call.get("rawInput").cloned();
        let raw_output = tool_call.get("rawOutput").cloned();

        let options: Vec<PermissionOption> = obj
            .get("options")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .enumerate()
                    .filter_map(|(i, opt)| {
                        let o = opt.as_object()?;
                        Some(PermissionOption {
                            option_id: o
                                .get("optionId")
                                .and_then(|v| v.as_str())
                                .unwrap_or(&format!("option-{}", i + 1))
                                .to_string(),
                            name: o
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or(&format!("Option {}", i + 1))
                                .to_string(),
                            kind: o
                                .get("kind")
                                .and_then(|v| v.as_str())
                                .unwrap_or("allow_once")
                                .to_string(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        let request = PermissionRequest {
            id: tool_call_id.clone(),
            session_id,
            tool_call_id: tool_call_id.clone(),
            title,
            kind,
            raw_input,
            raw_output,
            options,
        };

        // Notify permission handler
        let handler = self.permission_handler.lock().await;
        if let Some(h) = handler.as_ref() {
            h(request);
        } else {
            debug!("[ACP] No permission handler registered; cancelling request");
            return serde_json::json!({"outcome": {"outcome": "cancelled"}});
        }
        drop(handler);

        // Wait for response via oneshot
        let (tx, rx) = oneshot::channel();
        self.pending_permissions
            .lock()
            .await
            .insert(tool_call_id, PendingPermission { tx });

        rx.await
            .unwrap_or_else(|_| serde_json::json!({"outcome": {"outcome": "cancelled"}}))
    }
}

impl AgentBackend for AcpSdkBackend {
    fn initialize(&self) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + '_>> {
        Box::pin(async move {
            if self.transport.lock().await.is_some() {
                return Ok(());
            }

            let transport = AcpStdioTransport::new(
                &self.command,
                &self.args,
                self.env.clone(),
            );

            // Set up notification handler
            let msg_for_notif = self.message_handler.clone();

            transport
                .on_notification(move |method, params| {
                    if method == "session/update" {
                        let obj = match params.as_object() {
                            Some(o) => o,
                            None => return,
                        };
                        let update = match obj.get("update") {
                            Some(u) => u.clone(),
                            None => return,
                        };
                        // We can't await in a sync closure, so use try_lock
                        if let Ok(mut handler) = msg_for_notif.try_lock() {
                            if let Some(h) = handler.as_mut() {
                                h.handle_update(&update);
                            }
                        }
                    }
                })
                .await;

            // Store transport before registering handlers
            *self.transport.lock().await = Some(transport.clone());

            // Register the permission request handler
            let backend_pending = self.pending_permissions.clone();

            transport
                .register_request_handler(
                    "session/request_permission",
                    Arc::new(move |params: Value, _request_id: Value| {
                        let pending = backend_pending.clone();
                        Box::pin(async move {
                            // Simplified: just create a pending entry and wait
                            // The full permission flow is handled by respond_to_permission
                            let obj = params.as_object();
                            let tool_call = obj
                                .and_then(|o| o.get("toolCall"))
                                .and_then(|v| v.as_object())
                                .cloned()
                                .unwrap_or_default();
                            let tool_call_id = tool_call
                                .get("toolCallId")
                                .and_then(|v| v.as_str())
                                .unwrap_or("tool-unknown")
                                .to_string();

                            let (tx, rx) = oneshot::channel();
                            pending
                                .lock()
                                .await
                                .insert(tool_call_id, PendingPermission { tx });

                            rx.await.unwrap_or_else(|_| {
                                serde_json::json!({"outcome": {"outcome": "cancelled"}})
                            })
                        })
                    }),
                )
                .await;

            // Send initialize request with retry
            let t = transport.clone();
            let response = with_retry(3, || {
                let t = t.clone();
                async move {
                    t.send_request_default(
                        "initialize",
                        serde_json::json!({
                            "protocolVersion": 1,
                            "clientCapabilities": {
                                "fs": {"readTextFile": false, "writeTextFile": false},
                                "terminal": false
                            },
                            "clientInfo": {
                                "name": "hapi",
                                "version": env!("CARGO_PKG_VERSION")
                            }
                        }),
                    )
                    .await
                }
            })
            .await
            .map_err(|e| anyhow::anyhow!(e))?;

            if !response.is_object()
                || response.get("protocolVersion").and_then(|v| v.as_u64()).is_none()
            {
                return Err(anyhow::anyhow!(
                    "Invalid initialize response from ACP agent"
                ));
            }

            debug!(
                "[ACP] Initialized with protocol version {}",
                response["protocolVersion"]
            );
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
                .ok_or_else(|| anyhow::anyhow!("ACP transport not initialized"))?;

            let mcp_servers: Vec<Value> = config
                .mcp_servers
                .iter()
                .map(|s| {
                    serde_json::json!({
                        "name": s.name,
                        "command": s.command,
                        "args": s.args,
                        "env": s.env.iter().map(|e| {
                            serde_json::json!({"name": e.name, "value": e.value})
                        }).collect::<Vec<_>>(),
                    })
                })
                .collect();

            let t = transport.clone();
            let cwd = config.cwd.clone();
            let response = with_retry(3, || {
                let t = t.clone();
                let cwd = cwd.clone();
                let servers = mcp_servers.clone();
                async move {
                    t.send_request_default(
                        "session/new",
                        serde_json::json!({
                            "cwd": cwd,
                            "mcpServers": servers,
                        }),
                    )
                    .await
                }
            })
            .await
            .map_err(|e| anyhow::anyhow!(e))?;

            let session_id = response
                .as_object()
                .and_then(|o| o.get("sessionId"))
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("Invalid session/new response from ACP agent"))?
                .to_string();

            *self.active_session_id.lock().await = Some(session_id.clone());
            Ok(session_id)
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
                .ok_or_else(|| anyhow::anyhow!("ACP transport not initialized"))?;

            *self.active_session_id.lock().await = Some(session_id.clone());
            *self.message_handler.lock().await = Some(AcpMessageHandler::new(move |msg| {
                on_update(msg);
            }));
            *self.is_processing.lock().await = true;

            let prompt_content: Vec<Value> = content
                .iter()
                .map(|c| match c {
                    PromptContent::Text { text } => {
                        serde_json::json!({"type": "text", "text": text})
                    }
                })
                .collect();

            let result = transport
                .send_request(
                    "session/prompt",
                    serde_json::json!({
                        "sessionId": session_id,
                        "prompt": prompt_content,
                    }),
                    u64::MAX, // No timeout for prompts
                )
                .await;

            // Flush and cleanup
            if let Ok(ref response) = result {
                let stop_reason = response
                    .as_object()
                    .and_then(|o| o.get("stopReason"))
                    .and_then(|v| v.as_str());

                if let Some(_reason) = stop_reason {
                    let mut handler = self.message_handler.lock().await;
                    if let Some(h) = handler.as_mut() {
                        h.flush_text();
                    }
                    // The on_update was moved into the handler, so we can't call it directly.
                    // The turn_complete message is emitted through the handler's on_message.
                    // We need a different approach - emit through the handler.
                    drop(handler);
                }
            }

            // Final flush
            {
                let mut handler = self.message_handler.lock().await;
                if let Some(h) = handler.as_mut() {
                    h.flush_text();
                }
            }
            *self.message_handler.lock().await = None;
            *self.is_processing.lock().await = false;
            self.notify_response_complete();

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
                        "session/cancel",
                        serde_json::json!({"sessionId": session_id}),
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
            match pending {
                Some(p) => {
                    let outcome = match response {
                        PermissionResponse::Cancelled => {
                            serde_json::json!({"outcome": {"outcome": "cancelled"}})
                        }
                        PermissionResponse::Selected { option_id } => {
                            serde_json::json!({"outcome": {"outcome": "selected", "optionId": option_id}})
                        }
                    };
                    let _ = p.tx.send(outcome);
                }
                None => {
                    debug!("[ACP] No pending permission request for id {}", request_id);
                }
            }
            Ok(())
        })
    }

    fn on_permission_request(&self, handler: OnPermissionRequestFn) {
        // Use blocking_lock since this is called from a sync context
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