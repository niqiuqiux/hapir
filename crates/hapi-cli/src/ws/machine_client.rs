use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::sync::Mutex;

use super::client::{WsClient, WsClientConfig};

/// Machine-scoped WebSocket client.
pub struct WsMachineClient {
    ws: Arc<WsClient>,
    machine_id: String,
    metadata: Arc<Mutex<Option<Value>>>,
    metadata_version: Arc<Mutex<i64>>,
    runner_state: Arc<Mutex<Option<Value>>>,
    runner_state_version: Arc<Mutex<i64>>,
    keep_alive_handle: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
}

impl WsMachineClient {
    pub fn new(api_url: &str, token: &str, machine_id: &str) -> Self {
        let ws = Arc::new(WsClient::new(WsClientConfig {
            url: api_url.to_string(),
            auth_token: token.to_string(),
            client_type: "machine-scoped".to_string(),
            scope_id: machine_id.to_string(),
            max_reconnect_attempts: None,
        }));

        Self {
            ws,
            machine_id: machine_id.to_string(),
            metadata: Arc::new(Mutex::new(None)),
            metadata_version: Arc::new(Mutex::new(0)),
            runner_state: Arc::new(Mutex::new(None)),
            runner_state_version: Arc::new(Mutex::new(0)),
            keep_alive_handle: Arc::new(Mutex::new(None)),
        }
    }

    pub async fn connect(&self) {
        let md = self.metadata.clone();
        let md_ver = self.metadata_version.clone();
        let rs = self.runner_state.clone();
        let rs_ver = self.runner_state_version.clone();

        self.ws.on("update", move |data| {
            let md = md.clone();
            let md_ver = md_ver.clone();
            let rs = rs.clone();
            let rs_ver = rs_ver.clone();

            tokio::spawn(async move {
                if let Some(update_type) = data.get("type").and_then(|v| v.as_str()) {
                    if update_type == "update-machine" {
                        if let Some(new_ver) = data.get("metadataVersion").and_then(|v| v.as_i64()) {
                            if new_ver > *md_ver.lock().await {
                                *md.lock().await = data.get("metadata").cloned();
                                *md_ver.lock().await = new_ver;
                            }
                        }
                        if let Some(new_ver) = data.get("runnerStateVersion").and_then(|v| v.as_i64()) {
                            if new_ver > *rs_ver.lock().await {
                                *rs.lock().await = data.get("runnerState").cloned();
                                *rs_ver.lock().await = new_ver;
                            }
                        }
                    }
                }
            });
        }).await;

        self.ws.connect().await;

        // Start keep-alive (every 20 seconds)
        let ws = self.ws.clone();
        let mid = self.machine_id.clone();
        let handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(20));
            loop {
                interval.tick().await;
                ws.emit("machine-alive", json!({ "mid": mid })).await;
            }
        });
        *self.keep_alive_handle.lock().await = Some(handle);
    }

    pub async fn update_metadata<F>(&self, handler: F) -> anyhow::Result<()>
    where
        F: FnOnce(Value) -> Value,
    {
        let current = self.metadata.lock().await.clone().unwrap_or(json!({}));
        let updated = handler(current);
        let version = *self.metadata_version.lock().await;

        let ack = self.ws.emit_with_ack("update-state", json!({
            "mid": self.machine_id,
            "expectedVersion": version,
            "metadata": updated,
        })).await?;

        if let Some(ver) = ack.get("version").and_then(|v| v.as_i64()) {
            *self.metadata_version.lock().await = ver;
        }
        if let Some(val) = ack.get("metadata") {
            *self.metadata.lock().await = Some(val.clone());
        }
        Ok(())
    }

    pub async fn update_runner_state<F>(&self, handler: F) -> anyhow::Result<()>
    where
        F: FnOnce(Value) -> Value,
    {
        let current = self.runner_state.lock().await.clone().unwrap_or(json!({}));
        let updated = handler(current);
        let version = *self.runner_state_version.lock().await;

        let ack = self.ws.emit_with_ack("update-state", json!({
            "mid": self.machine_id,
            "expectedVersion": version,
            "runnerState": updated,
        })).await?;

        if let Some(ver) = ack.get("version").and_then(|v| v.as_i64()) {
            *self.runner_state_version.lock().await = ver;
        }
        if let Some(val) = ack.get("runnerState") {
            *self.runner_state.lock().await = Some(val.clone());
        }
        Ok(())
    }

    /// Register an RPC handler scoped to this machine.
    pub async fn register_rpc(
        &self,
        method: &str,
        handler: impl Fn(Value) -> std::pin::Pin<Box<dyn std::future::Future<Output = Value> + Send>> + Send + Sync + 'static,
    ) {
        let scoped_method = format!("{}:{}", self.machine_id, method);
        self.ws.register_rpc(&scoped_method, handler).await;
        self.ws.emit("rpc-register", json!({
            "method": format!("{}:{}", self.machine_id, method),
        })).await;
    }

    pub async fn shutdown(&self) {
        if let Some(handle) = self.keep_alive_handle.lock().await.take() {
            handle.abort();
        }
        self.ws.close().await;
    }

    #[allow(dead_code)]
    pub fn machine_id(&self) -> &str {
        &self.machine_id
    }
}
