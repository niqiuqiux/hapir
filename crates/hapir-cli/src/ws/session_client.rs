use std::sync::Arc;

use serde_json::{json, Value};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use hapir_shared::schemas::{Metadata, Session};

use super::client::{WsClient, WsClientConfig};

/// Session-scoped WebSocket client.
pub struct WsSessionClient {
    ws: Arc<WsClient>,
    session_id: String,
    metadata: Arc<Mutex<Option<Metadata>>>,
    metadata_version: Arc<Mutex<i64>>,
    agent_state: Arc<Mutex<Option<Value>>>,
    agent_state_version: Arc<Mutex<i64>>,
    #[allow(dead_code)]
    last_seen_message_seq: Arc<Mutex<i64>>,
}

impl WsSessionClient {
    pub fn new(api_url: &str, token: &str, session: &Session) -> Self {
        let ws = Arc::new(WsClient::new(WsClientConfig {
            url: api_url.to_string(),
            auth_token: token.to_string(),
            client_type: "session-scoped".to_string(),
            scope_id: session.id.clone(),
            max_reconnect_attempts: None,
        }));

        let agent_state_value = session.agent_state.as_ref()
            .and_then(|s| serde_json::to_value(s).ok());

        Self {
            ws,
            session_id: session.id.clone(),
            metadata: Arc::new(Mutex::new(session.metadata.clone())),
            metadata_version: Arc::new(Mutex::new(session.metadata_version as i64)),
            agent_state: Arc::new(Mutex::new(agent_state_value)),
            agent_state_version: Arc::new(Mutex::new(session.agent_state_version as i64)),
            last_seen_message_seq: Arc::new(Mutex::new(0)),
        }
    }

    /// Start the connection and set up event handlers.
    pub async fn connect(&self) {
        let metadata = self.metadata.clone();
        let metadata_version = self.metadata_version.clone();
        let agent_state = self.agent_state.clone();
        let agent_state_version = self.agent_state_version.clone();
        let last_seq = self.last_seen_message_seq.clone();

        self.ws.on("update", move |data| {
            let md = metadata.clone();
            let md_ver = metadata_version.clone();
            let as_ = agent_state.clone();
            let as_ver = agent_state_version.clone();
            let seq = last_seq.clone();

            tokio::spawn(async move {
                if let Some(new_ver) = data.get("metadataVersion").and_then(|v| v.as_i64()) {
                    let current = *md_ver.lock().await;
                    if new_ver > current {
                        if let Some(new_md) = data.get("metadata") {
                            if let Ok(parsed) = serde_json::from_value::<Metadata>(new_md.clone()) {
                                *md.lock().await = Some(parsed);
                                *md_ver.lock().await = new_ver;
                            }
                        }
                    }
                }

                if let Some(new_ver) = data.get("agentStateVersion").and_then(|v| v.as_i64()) {
                    let current = *as_ver.lock().await;
                    if new_ver > current {
                        if let Some(new_state) = data.get("agentState") {
                            *as_.lock().await = Some(new_state.clone());
                            *as_ver.lock().await = new_ver;
                        }
                    }
                }

                if let Some(msg_seq) = data.get("messageSeq").and_then(|v| v.as_i64()) {
                    let mut current = seq.lock().await;
                    if msg_seq > *current {
                        *current = msg_seq;
                    }
                }
            });
        }).await;

        // Register initial session-alive as a connect message so it is sent
        // after all RPC handlers are re-registered on every (re)connect.
        let time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        self.ws.add_connect_message("session-alive", json!({
            "sid": self.session_id,
            "time": time,
            "thinking": false,
            "mode": "local",
            "runtime": "",
        })).await;

        self.ws.connect().await;
    }

    /// Send keep-alive.
    pub async fn keep_alive(&self, thinking: bool, mode: &str, runtime: &str) {
        let time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        self.ws.emit("session-alive", json!({
            "sid": self.session_id,
            "time": time,
            "thinking": thinking,
            "mode": mode,
            "runtime": runtime,
        })).await;
    }

    /// Send session end.
    pub async fn send_session_end(&self) {
        let time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        self.ws.emit("session-end", json!({
            "sid": self.session_id,
            "time": time,
        })).await;
    }

    /// Update metadata with optimistic concurrency.
    pub async fn update_metadata<F>(&self, handler: F) -> anyhow::Result<()>
    where
        F: FnOnce(Metadata) -> Metadata,
    {
        let current = match self.metadata.lock().await.clone() {
            Some(m) => m,
            None => anyhow::bail!("no metadata to update"),
        };
        let updated = handler(current);
        let version = *self.metadata_version.lock().await;

        let ack = self.ws.emit_with_ack("update-metadata", json!({
            "sid": self.session_id,
            "expectedVersion": version,
            "metadata": updated,
        })).await?;

        apply_versioned_ack(&ack, "metadata", &self.metadata, &self.metadata_version).await;
        Ok(())
    }

    /// Update agent state with optimistic concurrency.
    pub async fn update_agent_state<F>(&self, handler: F) -> anyhow::Result<()>
    where
        F: FnOnce(Value) -> Value,
    {
        let current = self.agent_state.lock().await.clone().unwrap_or(json!({}));
        let updated = handler(current);
        let version = *self.agent_state_version.lock().await;

        let ack = self.ws.emit_with_ack("update-state", json!({
            "sid": self.session_id,
            "expectedVersion": version,
            "agentState": updated,
        })).await?;

        apply_versioned_ack(&ack, "agentState", &self.agent_state, &self.agent_state_version).await;
        Ok(())
    }

    /// Send a message.
    pub async fn send_message(&self, body: Value) {
        self.ws.emit("message", json!({
            "sid": self.session_id,
            "message": body,
        })).await;
    }

    /// Send a message delta for streaming.
    pub async fn send_message_delta(&self, message_id: &str, text: &str, is_final: bool) {
        self.ws.emit("message-delta", json!({
            "sid": self.session_id,
            "delta": {
                "messageId": message_id,
                "text": text,
                "isFinal": is_final,
            }
        })).await;
    }

    /// Register an RPC handler scoped to this session.
    pub async fn register_rpc(
        &self,
        method: &str,
        handler: impl Fn(Value) -> std::pin::Pin<Box<dyn Future<Output = Value> + Send>> + Send + Sync + 'static,
    ) {
        let scoped_method = format!("{}:{}", self.session_id, method);
        info!(method = %scoped_method, "registering session-scoped RPC handler");
        self.ws.register_rpc(&scoped_method, handler).await;
        self.ws.emit("rpc-register", json!({
            "method": scoped_method,
        })).await;
    }

    /// Close the session connection.
    pub async fn close(&self) {
        self.ws.close().await;
    }

    #[allow(dead_code)]
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    #[allow(dead_code)]
    pub async fn metadata(&self) -> Option<Metadata> {
        self.metadata.lock().await.clone()
    }
}

/// Apply a versioned ack response, updating local state.
async fn apply_versioned_ack<T: serde::de::DeserializeOwned + Clone>(
    ack: &Value,
    value_key: &str,
    local_value: &Mutex<Option<T>>,
    local_version: &Mutex<i64>,
) {
    let result = ack.get("result").and_then(|v| v.as_str()).unwrap_or("");

    match result {
        "success" | "version-mismatch" => {
            if let Some(ver) = ack.get("version").and_then(|v| v.as_i64()) {
                *local_version.lock().await = ver;
            }
            if let Some(val) = ack.get(value_key) {
                if let Ok(parsed) = serde_json::from_value::<T>(val.clone()) {
                    *local_value.lock().await = Some(parsed);
                }
            }
            if result == "version-mismatch" {
                debug!("version mismatch, local state updated from server");
            }
        }
        "error" => {
            let reason = ack.get("reason").and_then(|v| v.as_str()).unwrap_or("unknown");
            warn!(reason = reason, "versioned update error");
        }
        _ => {
            warn!(result = result, "unknown ack result");
        }
    }
}
