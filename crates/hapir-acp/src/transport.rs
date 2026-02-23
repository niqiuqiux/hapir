use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio::task::JoinHandle;
use tracing::debug;

/// Handler for notification messages from the child process.
pub type NotificationHandler = Box<dyn Fn(String, Value) + Send + Sync>;

/// Handler for stderr output lines from the child process.
/// Receives the raw text line; interpretation is up to the backend.
pub type StderrHandler = Box<dyn Fn(String) + Send + Sync>;

/// Handler for incoming JSON-RPC requests from the child process.
/// Receives `(params, request_id)` and returns the result value.
pub type RequestHandler =
    Arc<dyn Fn(Value, Value) -> Pin<Box<dyn Future<Output = Value> + Send>> + Send + Sync>;

/// Default request timeout (120 seconds).
const DEFAULT_TIMEOUT_MS: u64 = 120_000;

struct PendingRequest {
    resolve: oneshot::Sender<Result<Value, String>>,
}

enum WriteCmd {
    Send(String),
    Close,
}

/// JSON-RPC 2.0 transport over stdin/stdout of a child process.
///
/// Spawns a child process, reads newline-delimited JSON from its stdout,
/// writes newline-delimited JSON to its stdin. Supports request/response
/// correlation, notifications, and incoming request handlers.
pub struct AcpStdioTransport {
    next_id: AtomicU64,
    pending: Arc<Mutex<HashMap<u64, PendingRequest>>>,
    request_handlers: Arc<Mutex<HashMap<String, RequestHandler>>>,
    notification_handler: Arc<Mutex<Option<NotificationHandler>>>,
    stderr_handler: Arc<Mutex<Option<StderrHandler>>>,
    write_tx: Mutex<Option<mpsc::UnboundedSender<WriteCmd>>>,
    child: Mutex<Option<Child>>,
    protocol_error: Arc<Mutex<Option<String>>>,
    _tasks: Mutex<Vec<JoinHandle<()>>>,
}

impl AcpStdioTransport {
    /// Spawn a child process and wire up the JSON-RPC transport.
    pub fn new(
        command: &str,
        args: &[String],
        env: Option<HashMap<String, String>>,
    ) -> Result<Arc<Self>, String> {
        let mut cmd = Command::new(command);
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        cmd.env("NO_COLOR", "1");
        if let Some(env_map) = env {
            cmd.envs(env_map);
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| format!("failed to spawn '{}': {}", command, e))?;

        let child_stdout = child.stdout.take().expect("child stdout");
        let child_stdin = child.stdin.take().expect("child stdin");
        let child_stderr = child.stderr.take().expect("child stderr");

        let (write_tx, write_rx) = mpsc::unbounded_channel::<WriteCmd>();

        let transport = Arc::new(Self {
            next_id: AtomicU64::new(1),
            pending: Arc::new(Mutex::new(HashMap::new())),
            request_handlers: Arc::new(Mutex::new(HashMap::new())),
            notification_handler: Arc::new(Mutex::new(None)),
            stderr_handler: Arc::new(Mutex::new(None)),
            write_tx: Mutex::new(Some(write_tx)),
            child: Mutex::new(Some(child)),
            protocol_error: Arc::new(Mutex::new(None)),
            _tasks: Mutex::new(Vec::new()),
        });

        let writer_handle = {
            let mut stdin = child_stdin;
            let mut rx = write_rx;
            tokio::spawn(async move {
                while let Some(cmd) = rx.recv().await {
                    match cmd {
                        WriteCmd::Send(payload) => {
                            if stdin.write_all(payload.as_bytes()).await.is_err() {
                                break;
                            }
                            let _ = stdin.flush().await;
                        }
                        WriteCmd::Close => break,
                    }
                }
                let _ = stdin.shutdown().await;
            })
        };

        let reader_handle = {
            let t = transport.clone();
            tokio::spawn(async move {
                let reader = BufReader::new(child_stdout);
                let mut lines = reader.lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let trimmed = line.trim().to_string();
                    if !trimmed.is_empty() {
                        debug!("[transport][stdout] {}", trimmed);
                        t.handle_line(&trimmed).await;
                    }
                }
                debug!("[transport] Reader ended, child process likely exited");
                t.reject_all_pending("child process exited").await;
            })
        };

        let stderr_handle = {
            let t = transport.clone();
            tokio::spawn(async move {
                let reader = BufReader::new(child_stderr);
                let mut lines = reader.lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let text = line.trim().to_string();
                    if !text.is_empty() {
                        debug!("[transport][stderr] {}", text);
                        let handler = t.stderr_handler.lock().await;
                        if let Some(h) = handler.as_ref() {
                            h(text);
                        }
                    }
                }
            })
        };

        let t2 = transport.clone();
        tokio::spawn(async move {
            let mut tasks = t2._tasks.lock().await;
            tasks.push(writer_handle);
            tasks.push(reader_handle);
            tasks.push(stderr_handle);
        });

        Ok(transport)
    }

    pub async fn on_notification<F>(&self, handler: F)
    where
        F: Fn(String, Value) + Send + Sync + 'static,
    {
        *self.notification_handler.lock().await = Some(Box::new(handler));
    }

    pub async fn on_stderr<F>(&self, handler: F)
    where
        F: Fn(String) + Send + Sync + 'static,
    {
        *self.stderr_handler.lock().await = Some(Box::new(handler));
    }

    pub async fn register_request_handler(&self, method: &str, handler: RequestHandler) {
        self.request_handlers
            .lock()
            .await
            .insert(method.to_string(), handler);
    }

    pub async fn send_request(
        &self,
        method: &str,
        params: Value,
        timeout_ms: u64,
    ) -> Result<Value, String> {
        if let Some(err) = self.protocol_error.lock().await.as_ref() {
            return Err(err.clone());
        }

        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });

        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .await
            .insert(id, PendingRequest { resolve: tx });

        self.write_payload(&payload).await;

        if timeout_ms == u64::MAX {
            return rx.await.unwrap_or(Err("channel closed".to_string()));
        }

        let effective_timeout = if timeout_ms == 0 {
            DEFAULT_TIMEOUT_MS
        } else {
            timeout_ms
        };

        match tokio::time::timeout(std::time::Duration::from_millis(effective_timeout), rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err("channel closed".to_string()),
            Err(_) => {
                self.pending.lock().await.remove(&id);
                Err(format!(
                    "Request '{}' timed out after {}ms",
                    method, effective_timeout
                ))
            }
        }
    }

    pub async fn send_request_default(&self, method: &str, params: Value) -> Result<Value, String> {
        self.send_request(method, params, DEFAULT_TIMEOUT_MS).await
    }

    pub async fn send_notification(&self, method: &str, params: Value) {
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        self.write_payload(&payload).await;
    }

    pub async fn close(&self) {
        if let Some(tx) = self.write_tx.lock().await.take() {
            let _ = tx.send(WriteCmd::Close);
        }
        if let Some(mut child) = self.child.lock().await.take() {
            let _ = child.kill().await;
        }
        self.reject_all_pending("transport closed").await;
    }

    async fn write_payload(&self, payload: &Value) {
        let serialized = format!("{}\n", serde_json::to_string(payload).unwrap_or_default());
        if let Some(tx) = self.write_tx.lock().await.as_ref() {
            let _ = tx.send(WriteCmd::Send(serialized));
        }
    }

    async fn handle_line(&self, line: &str) {
        if self.protocol_error.lock().await.is_some() {
            return;
        }

        let parsed: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => {
                let err_msg = "Failed to parse JSON-RPC from agent".to_string();
                *self.protocol_error.lock().await = Some(err_msg.clone());
                debug!("[transport] Failed to parse JSON-RPC line: {}", line);
                self.reject_all_pending(&err_msg).await;
                if let Some(tx) = self.write_tx.lock().await.take() {
                    let _ = tx.send(WriteCmd::Close);
                }
                if let Some(mut child) = self.child.lock().await.take() {
                    let _ = child.kill().await;
                }
                return;
            }
        };

        if !parsed.is_object() {
            debug!("[transport] Ignoring non-object JSON from stdout");
            return;
        }

        if parsed.get("method").is_some()
            && let Some(id) = parsed.get("id")
            && !id.is_null()
        {
            self.handle_incoming_request(&parsed).await;
            return;
        }
        if parsed.get("method").is_some() {
            let method = parsed["method"].as_str().unwrap_or("").to_string();
            let params = parsed.get("params").cloned().unwrap_or(Value::Null);
            let handler = self.notification_handler.lock().await;
            if let Some(h) = handler.as_ref() {
                h(method, params);
            }
            return;
        }

        if parsed.get("id").is_some() {
            self.handle_response(&parsed).await;
        }
    }

    async fn handle_incoming_request(&self, request: &Value) {
        let method = request["method"].as_str().unwrap_or("").to_string();
        let params = request.get("params").cloned().unwrap_or(Value::Null);
        let id = request.get("id").cloned().unwrap_or(Value::Null);

        let handler = {
            let handlers = self.request_handlers.lock().await;
            handlers.get(&method).cloned()
        };

        match handler {
            Some(h) => {
                let result = h(params, id.clone()).await;
                let response = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": result,
                });
                self.write_payload(&response).await;
            }
            None => {
                let response = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {
                        "code": -32601,
                        "message": format!("Method not found: {}", method),
                    },
                });
                self.write_payload(&response).await;
            }
        }
    }

    async fn handle_response(&self, response: &Value) {
        let id = match response.get("id").and_then(|v| v.as_u64()) {
            Some(id) => id,
            None => {
                debug!("[transport] Received response without numeric id");
                return;
            }
        };

        let pending = self.pending.lock().await.remove(&id);
        match pending {
            Some(p) => {
                if let Some(err) = response.get("error") {
                    let msg = err
                        .get("message")
                        .and_then(|m| m.as_str())
                        .unwrap_or("Unknown error")
                        .to_string();
                    let _ = p.resolve.send(Err(msg));
                } else {
                    let result = response.get("result").cloned().unwrap_or(Value::Null);
                    let _ = p.resolve.send(Ok(result));
                }
            }
            None => {
                debug!(
                    "[transport] Received response with no pending request: {}",
                    id
                );
            }
        }
    }

    async fn reject_all_pending(&self, message: &str) {
        let mut pending = self.pending.lock().await;
        for (_, p) in pending.drain() {
            let _ = p.resolve.send(Err(message.to_string()));
        }
    }
}
