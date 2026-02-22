use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::transport::AcpStdioTransport;
use crate::types::{
    AgentBackend, AgentSessionConfig, OnPermissionRequestFn, OnUpdateFn, PermissionOption,
    PermissionRequest, PermissionResponse, PromptContent,
};
use arc_swap::ArcSwap;
use serde_json::Value;
use tokio::sync::{Mutex, mpsc, oneshot};
use tracing::debug;

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
    permission_handler: Arc<ArcSwap<Option<OnPermissionRequestFn>>>,
    pending_permissions: Arc<Mutex<HashMap<String, PendingPermission>>>,
    active_thread_id: Mutex<Option<String>>,
    active_turn_id: Mutex<Option<String>>,
    notification_tx: Arc<ArcSwap<Option<mpsc::UnboundedSender<(String, Value)>>>>,
}

impl CodexAppServerBackend {
    pub fn new(command: String, args: Vec<String>, env: Option<HashMap<String, String>>) -> Self {
        Self {
            command,
            args,
            env,
            transport: Mutex::new(None),
            permission_handler: Arc::new(ArcSwap::from_pointee(None)),
            pending_permissions: Arc::new(Mutex::new(HashMap::new())),
            active_thread_id: Mutex::new(None),
            active_turn_id: Mutex::new(None),
            notification_tx: Arc::new(ArcSwap::from_pointee(None)),
        }
    }
}

impl AgentBackend for CodexAppServerBackend {
    fn initialize(&self) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + '_>> {
        Box::pin(async move {
            if self.transport.lock().await.is_some() {
                return Ok(());
            }

            let transport = AcpStdioTransport::new(&self.command, &self.args, self.env.clone())
                .map_err(|e| anyhow::anyhow!(e))?;

            let notif_tx = self.notification_tx.clone();
            transport
                .on_notification(move |method, params| {
                    if let Some(tx) = notif_tx.load().as_ref() {
                        let _ = tx.send((method, params));
                    }
                })
                .await;

            *self.transport.lock().await = Some(transport.clone());

            let pending_cmd = self.pending_permissions.clone();
            let handler_cmd = self.permission_handler.clone();
            transport
                .register_request_handler(
                    "item/commandExecution/requestApproval",
                    Arc::new(move |params: Value, _id: Value| {
                        let pending = pending_cmd.clone();
                        let handler = handler_cmd.clone();
                        Box::pin(async move {
                            let id = params
                                .as_object()
                                .and_then(|o| o.get("itemId").or_else(|| o.get("id")))
                                .and_then(|v| v.as_str())
                                .unwrap_or("tool-unknown")
                                .to_string();

                            let command = params
                                .as_object()
                                .and_then(|o| o.get("command"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();

                            let thread_id = params
                                .as_object()
                                .and_then(|o| o.get("threadId"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();

                            let (tx, rx) = oneshot::channel();
                            pending
                                .lock()
                                .await
                                .insert(id.clone(), PendingPermission { tx });

                            if let Some(h) = handler.load().as_ref() {
                                h(PermissionRequest {
                                    id: id.clone(),
                                    session_id: thread_id,
                                    tool_call_id: id.clone(),
                                    title: Some(command),
                                    kind: Some("commandExecution".to_string()),
                                    raw_input: Some(params),
                                    raw_output: None,
                                    options: vec![
                                        PermissionOption {
                                            option_id: "accept".to_string(),
                                            name: "Allow".to_string(),
                                            kind: "allow".to_string(),
                                        },
                                        PermissionOption {
                                            option_id: "accept_session".to_string(),
                                            name: "Allow for session".to_string(),
                                            kind: "allow".to_string(),
                                        },
                                        PermissionOption {
                                            option_id: "deny".to_string(),
                                            name: "Deny".to_string(),
                                            kind: "deny".to_string(),
                                        },
                                    ],
                                });
                            }

                            rx.await
                                .unwrap_or_else(|_| serde_json::json!({"decision": "decline"}))
                        })
                    }),
                )
                .await;

            let pending_file = self.pending_permissions.clone();
            let handler_file = self.permission_handler.clone();
            transport
                .register_request_handler(
                    "item/fileChange/requestApproval",
                    Arc::new(move |params: Value, _id: Value| {
                        let pending = pending_file.clone();
                        let handler = handler_file.clone();
                        Box::pin(async move {
                            let id = params
                                .as_object()
                                .and_then(|o| o.get("itemId").or_else(|| o.get("id")))
                                .and_then(|v| v.as_str())
                                .unwrap_or("tool-unknown")
                                .to_string();

                            let reason = params
                                .as_object()
                                .and_then(|o| o.get("reason"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();

                            let thread_id = params
                                .as_object()
                                .and_then(|o| o.get("threadId"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();

                            let (tx, rx) = oneshot::channel();
                            pending
                                .lock()
                                .await
                                .insert(id.clone(), PendingPermission { tx });

                            if let Some(h) = handler.load().as_ref() {
                                h(PermissionRequest {
                                    id: id.clone(),
                                    session_id: thread_id,
                                    tool_call_id: id.clone(),
                                    title: Some(reason),
                                    kind: Some("fileChange".to_string()),
                                    raw_input: Some(params),
                                    raw_output: None,
                                    options: vec![
                                        PermissionOption {
                                            option_id: "accept".to_string(),
                                            name: "Allow".to_string(),
                                            kind: "allow".to_string(),
                                        },
                                        PermissionOption {
                                            option_id: "accept_session".to_string(),
                                            name: "Allow for session".to_string(),
                                            kind: "allow".to_string(),
                                        },
                                        PermissionOption {
                                            option_id: "deny".to_string(),
                                            name: "Deny".to_string(),
                                            kind: "deny".to_string(),
                                        },
                                    ],
                                });
                            }

                            rx.await
                                .unwrap_or_else(|_| serde_json::json!({"decision": "decline"}))
                        })
                    }),
                )
                .await;

            let response = transport
                .send_request_default(
                    "initialize",
                    serde_json::json!({
                        "protocolVersion": "2025-03-26",
                        "clientInfo": {
                            "name": "hapir",
                            "title": "HAPIR",
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

            debug!("[CodexAppServer] thread/start response: {:?}", response);

            let thread_id = response
                .as_object()
                .and_then(|o| {
                    o.get("threadId")
                        .or_else(|| o.get("id"))
                        .or_else(|| o.get("thread").and_then(|t| t.get("id")))
                })
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("Invalid thread/start response: {response}"))?
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

            // Create per-prompt channels
            let (ntx, mut nrx) = mpsc::unbounded_channel::<(String, Value)>();
            self.notification_tx.store(Arc::new(Some(ntx)));

            let input: Vec<Value> = content
                .iter()
                .map(|c| match c {
                    PromptContent::Text { text } => serde_json::json!({
                        "type": "text",
                        "text": text,
                    }),
                })
                .collect();

            // Spawn notification processor: owns the message handler,
            // processes all notifications, breaks on turn/completed.
            let process_handle = tokio::spawn(async move {
                let mut handler = CodexMessageHandler::new(move |msg| {
                    on_update(msg);
                });
                loop {
                    match nrx.recv().await {
                        Some((method, params)) => {
                            if handler.handle_notification(&method, &params) {
                                break;
                            }
                        }
                        None => break,
                    }
                }
            });

            // Send turn/start and wait for the RPC ack (comes back immediately
            // with status "inProgress"). Use default timeout for the ack only.
            let rpc_result = transport
                .send_request_default(
                    "turn/start",
                    serde_json::json!({
                        "threadId": session_id,
                        "input": input,
                    }),
                )
                .await;

            match rpc_result {
                Ok(resp) => {
                    let turn_id = resp
                        .get("turn")
                        .and_then(|t| t.get("id"))
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    *self.active_turn_id.lock().await = turn_id;
                }
                Err(e) => {
                    process_handle.abort();
                    self.notification_tx.store(Arc::new(None));
                    return Err(anyhow::anyhow!(e));
                }
            }

            // Turn accepted, wait for turn/completed via notification processor
            let _ = process_handle.await;

            // Close channels
            self.notification_tx.store(Arc::new(None));

            Ok(())
        })
    }

    fn cancel_prompt(
        &self,
        session_id: &str,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + '_>> {
        let session_id = session_id.to_string();
        Box::pin(async move {
            if let Some(transport) = self.transport.lock().await.as_ref() {
                let mut params = serde_json::json!({"threadId": session_id});
                if let Some(turn_id) = self.active_turn_id.lock().await.as_ref() {
                    params["turnId"] = serde_json::json!(turn_id);
                }
                let _ = transport
                    .send_request_default("turn/interrupt", params)
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
                let result = match response {
                    PermissionResponse::Selected { ref option_id } if option_id == "deny" => {
                        serde_json::json!({"decision": "decline"})
                    }
                    PermissionResponse::Selected { ref option_id }
                        if option_id == "accept_session" =>
                    {
                        serde_json::json!({"decision": "accept", "acceptSettings": {"forSession": true}})
                    }
                    PermissionResponse::Selected { .. } => {
                        serde_json::json!({"decision": "accept"})
                    }
                    _ => serde_json::json!({"decision": "decline"}),
                };
                let _ = p.tx.send(result);
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
        self.permission_handler.store(Arc::new(Some(handler)));
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
